//! Daemon authentication: token generation, persistence, and validation.
//!
//! Every `DaemonRequest` carries a top-level `auth` field containing a hex-
//! encoded bearer token. The daemon generates a fresh token on each startup,
//! writes it to a file with restricted permissions, and validates incoming
//! requests against the active token with a constant-time comparison.
//!
//! ## Token lifecycle
//!
//! 1. Daemon starts → [`generate_token`] produces 32 random bytes → 64 hex chars.
//! 2. [`write_token_file`] writes the token to disk with 0600 permissions (Unix)
//!    or inherited ACL (Windows). The file path is provided by the caller
//!    (typically `~/.pice/daemon.token`).
//! 3. The CLI adapter reads the token file via [`read_token_file`] and includes
//!    it in every `DaemonRequest.auth`.
//! 4. The daemon validates each request via [`validate_request`], which uses
//!    constant-time comparison to prevent timing-based token recovery.
//! 5. On daemon restart, a new token is generated. Old tokens are invalid
//!    immediately — there is no grace period or version negotiation.
//!
//! ## Security properties
//!
//! - **Entropy**: 32 bytes (256 bits) from the OS CSPRNG via `getrandom`.
//! - **Constant-time comparison**: `subtle::ConstantTimeEq` prevents timing
//!   side-channels. A length check runs first (leaks only length info, not
//!   content — and the length is always 64 for valid tokens).
//! - **File permissions**: 0600 (owner read/write only) on Unix. On Windows,
//!   the default ACL inherits from the parent directory — tightening to an
//!   explicit DACL is a T17+ hardening task if threat-modeling requires it.
//! - **Never logged**: the token is not included in any `tracing` output,
//!   error messages, or `Debug`/`Display` impls. Functions that handle it
//!   use opaque `&str` / `String`, never format it into diagnostics.

use std::fmt::Write as FmtWrite;
use std::io;
use std::path::Path;

use anyhow::{Context, Result};
use pice_core::protocol::{DaemonRequest, DaemonResponse};
use subtle::ConstantTimeEq;

/// JSON-RPC error code for authentication failure.
const AUTH_FAILED_CODE: i32 = -32002;

/// Generate a new bearer token: 32 random bytes, hex-encoded to 64 chars.
///
/// Uses the OS CSPRNG via `getrandom` — no seeding required, no blocking
/// on first call (unlike `/dev/random` on old Linux kernels).
pub fn generate_token() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| {
        // `getrandom::Error` doesn't implement `std::error::Error` in 0.2,
        // so wrap it manually.
        anyhow::anyhow!("failed to generate random token: {e}")
    })?;
    Ok(hex_encode(&buf))
}

/// Write `token` to a file at `path` with 0600 permissions (Unix).
///
/// Creates the file if it doesn't exist. Overwrites unconditionally if it
/// does — this is the "rotate on every daemon start" contract.
///
/// On Windows, the file is created with default permissions (inherited from
/// the parent directory's ACL). An explicit restrictive DACL is not set —
/// the parent directory (`~/.pice/`) should already be access-controlled.
pub fn write_token_file(path: &Path, token: &str) -> Result<()> {
    // Write the file first, then set permissions. Same pattern as unix.rs's
    // socket chmod — the file must exist before we can chmod it.
    std::fs::write(path, token.as_bytes())
        .with_context(|| format!("failed to write token file at {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("failed to chmod 0600 on token file at {}", path.display()))?;
    }

    Ok(())
}

/// Read the bearer token from a previously-written token file.
///
/// Trims trailing whitespace/newlines to tolerate editors that append `\n`.
/// Returns an error if the file doesn't exist or can't be read.
pub fn read_token_file(path: &Path) -> Result<String> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read token file at {}", path.display()))?;
    Ok(contents.trim().to_string())
}

/// Constant-time comparison of two token strings.
///
/// Returns `true` if `provided` matches `expected`. The length check
/// happens first and is NOT constant-time — it leaks only the length of
/// the provided token, not its content. Since valid tokens are always
/// 64 hex chars, this reveals nothing an attacker doesn't already know
/// from the protocol spec.
pub fn validate_token(provided: &str, expected: &str) -> bool {
    if provided.len() != expected.len() {
        return false;
    }
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Validate a `DaemonRequest`'s auth field against the active token.
///
/// Returns `Ok(())` if the request is authenticated. Returns
/// `Err(DaemonResponse)` with error code `-32002` if the token is
/// missing or invalid — the caller can send this response directly.
///
/// The error message is deliberately generic ("authentication failed") and
/// does NOT include the provided token, the expected token, or any hint
/// about which bytes matched. This is both a security measure (no oracle)
/// and a logging safety measure (the token must never appear in output).
#[allow(clippy::result_large_err)] // DaemonResponse is returned unboxed so the caller can send it directly.
pub fn validate_request(req: &DaemonRequest, active_token: &str) -> Result<(), DaemonResponse> {
    if validate_token(&req.auth, active_token) {
        Ok(())
    } else {
        Err(DaemonResponse::error(
            req.id,
            AUTH_FAILED_CODE,
            "authentication failed",
        ))
    }
}

/// Derive the default token file path from the daemon state directory.
///
/// Follows the same convention as
/// [`pice_core::transport::SocketPath::default_from_env`]: the token lives
/// adjacent to the socket file in `~/.pice/`.
///
/// This function does NOT check whether the directory exists — the caller
/// (daemon lifecycle, T21) is responsible for ensuring `~/.pice/` exists
/// before calling [`write_token_file`].
pub fn default_token_path() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join(".pice")
            .join("daemon.token")
    }

    #[cfg(windows)]
    {
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join(".pice")
            .join("daemon.token")
    }
}

/// Hex-encode a byte slice into a lowercase hexadecimal string.
///
/// Uses `fmt::Write` on a pre-allocated `String` — no heap allocations
/// beyond the output string itself.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Phase 4.1 Pass-6 C13: `fmt::Write` on `String` is provably
        // infallible (no allocation failure path; the impl is
        // `Result::Ok` unconditionally). Grandfathered under
        // `-D clippy::expect_used`.
        #[allow(clippy::expect_used)]
        {
            write!(s, "{b:02x}").expect("write to String is infallible");
        }
    }
    s
}

/// Check whether a file has 0600 permissions (owner read/write only).
///
/// Returns `Ok(true)` if permissions match, `Ok(false)` if they don't,
/// `Err` if the file can't be stat'd. Unix-only — returns `Ok(true)` on
/// non-Unix platforms (permissions are ACL-based, not mode-based).
pub fn check_token_file_permissions(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)?;
        Ok(meta.permissions().mode() & 0o777 == 0o600)
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::protocol::methods;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn generate_token_produces_64_hex_chars() {
        let token = generate_token().expect("generate");
        assert_eq!(token.len(), 64, "token should be 64 hex chars");
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "token should be valid hex, got: {token}"
        );
    }

    #[test]
    fn generate_token_is_not_constant() {
        let t1 = generate_token().expect("generate 1");
        let t2 = generate_token().expect("generate 2");
        assert_ne!(t1, t2, "two generated tokens should differ");
    }

    #[test]
    fn write_and_read_token_roundtrip() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("daemon.token");

        let token = generate_token().expect("generate");
        write_token_file(&path, &token).expect("write");

        let read_back = read_token_file(&path).expect("read");
        assert_eq!(read_back, token, "read-back should match written token");
    }

    #[cfg(unix)]
    #[test]
    fn write_token_file_sets_0600_permissions() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("daemon.token");

        let token = generate_token().expect("generate");
        write_token_file(&path, &token).expect("write");

        assert!(
            check_token_file_permissions(&path).expect("stat"),
            "token file should have 0600 permissions"
        );
    }

    #[test]
    fn validate_token_accepts_matching_tokens() {
        let token = generate_token().expect("generate");
        assert!(
            validate_token(&token, &token),
            "identical tokens should match"
        );
    }

    #[test]
    fn validate_token_rejects_mismatched_tokens() {
        let t1 = generate_token().expect("generate 1");
        let t2 = generate_token().expect("generate 2");
        assert!(
            !validate_token(&t1, &t2),
            "different tokens should not match"
        );
    }

    #[test]
    fn validate_token_rejects_different_lengths() {
        let token = generate_token().expect("generate");
        assert!(
            !validate_token("short", &token),
            "length-mismatched tokens should not match"
        );
    }

    #[test]
    fn validate_request_passes_valid_auth() {
        let token = generate_token().expect("generate");
        let req = DaemonRequest::new(1, methods::DAEMON_HEALTH, &token, json!({}));
        assert!(
            validate_request(&req, &token).is_ok(),
            "valid auth should pass"
        );
    }

    #[test]
    fn validate_request_rejects_invalid_auth() {
        let token = generate_token().expect("generate");
        let req = DaemonRequest::new(1, methods::DAEMON_HEALTH, "wrong-token", json!({}));
        let err = validate_request(&req, &token).expect_err("should reject");
        assert_eq!(err.id, 1);
        let error = err.error.expect("should have error payload");
        assert_eq!(error.code, AUTH_FAILED_CODE);
        assert!(
            error.message.contains("authentication failed"),
            "error should say auth failed, got: {}",
            error.message
        );
    }

    #[test]
    fn validate_request_rejects_empty_auth() {
        let token = generate_token().expect("generate");
        let req = DaemonRequest::new(1, methods::DAEMON_HEALTH, "", json!({}));
        assert!(
            validate_request(&req, &token).is_err(),
            "empty auth should be rejected"
        );
    }

    #[test]
    fn hex_encode_produces_correct_output() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0xab, 0x12]), "00ffab12");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn read_token_file_trims_trailing_newline() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("daemon.token");

        // Simulate an editor that appends a newline.
        std::fs::write(
            &path,
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789\n",
        )
        .expect("write");

        let token = read_token_file(&path).expect("read");
        assert_eq!(token.len(), 64, "trailing newline should be trimmed");
    }
}
