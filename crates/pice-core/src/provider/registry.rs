//! Provider registry — maps provider names to command/args for spawning.
//!
//! Moved from `pice-cli/src/provider/registry.rs` in T5 of the Phase 0 refactor.
//! Path-walking + config lookup is pure logic; the async `ProviderHost` that
//! actually spawns providers lives in `pice-daemon::provider::host`.

use crate::config::PiceConfig;
use std::path::{Path, PathBuf};

/// A resolved provider command and args.
pub struct ResolvedProvider {
    pub command: String,
    pub args: Vec<String>,
}

/// Find the workspace root by looking for `packages/` relative to the binary.
///
/// Falls back to the current working directory if the binary location
/// cannot be determined (e.g., running via `cargo run`).
fn find_provider_base() -> PathBuf {
    // Allow an explicit override (robust for installed/relocated binaries).
    if let Ok(base) = std::env::var("PICE_PROVIDER_BASE") {
        let p = PathBuf::from(base);
        if p.join("packages").is_dir() {
            return p;
        }
    }

    // Try relative to the binary itself (works for installed binaries)
    if let Ok(exe) = std::env::current_exe() {
        // Resolve symlinks first — e.g. ~/.local/bin/pice-daemon -> target/release/
        // pice-daemon — otherwise the walk-up starts in ~/.local/bin and never finds
        // `packages/`, silently falling back to CWD and spawning a non-existent path.
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        // exe is at target/debug/pice or target/release/pice or /usr/local/bin/pice
        // Walk up looking for packages/ directory
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..5 {
            if let Some(ref d) = dir {
                if d.join("packages").is_dir() {
                    return d.clone();
                }
                dir = d.parent().map(|p| p.to_path_buf());
            } else {
                break;
            }
        }
    }

    // Fall back to CWD (works during development with `cargo run`)
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn provider_path(base: &Path, pkg: &str) -> String {
    child_process_path_arg(&base.join("packages").join(pkg).join("dist").join("bin.js"))
}

#[cfg(windows)]
fn child_process_path_arg(path: &Path) -> String {
    strip_windows_verbatim_prefix(&path.to_string_lossy())
}

#[cfg(any(windows, test))]
fn strip_windows_verbatim_prefix(value: &str) -> String {
    // `std::fs::canonicalize` resolves Windows symlinks but returns verbatim
    // paths such as `\\?\D:\repo\...`. Those are valid Rust/Win32 paths,
    // but Node's CLI entrypoint resolver can mis-handle them and try to lstat
    // just the drive root (`D:`), causing provider startup EOFs in CI. Keep the
    // symlink-resolved path for discovery, but pass conventional Win32 paths to
    // child processes.
    if let Some(rest) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = value.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(not(windows))]
fn child_process_path_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// Resolve a provider name to its command/args for spawning.
///
/// Locates provider binaries relative to the pice binary's own location,
/// falling back to CWD for development. In the future, this could scan
/// node_modules, a plugin directory, or a provider registry.
pub fn resolve(name: &str, _config: &PiceConfig) -> Option<ResolvedProvider> {
    let base = find_provider_base();

    match name {
        "stub" => Some(ResolvedProvider {
            command: "node".to_string(),
            args: vec![provider_path(&base, "provider-stub")],
        }),
        "claude-code" => Some(ResolvedProvider {
            command: "node".to_string(),
            args: vec![provider_path(&base, "provider-claude-code")],
        }),
        "codex" => Some(ResolvedProvider {
            command: "node".to_string(),
            args: vec![provider_path(&base, "provider-codex")],
        }),
        "local" => Some(ResolvedProvider {
            command: "node".to_string(),
            args: vec![provider_path(&base, "provider-local")],
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stub_provider() {
        let config = PiceConfig::default();
        let resolved = resolve("stub", &config);
        assert!(resolved.is_some());
        let r = resolved.unwrap();
        assert_eq!(r.command, "node");
        let provider_arg = PathBuf::from(&r.args[0]);
        assert!(r.args[0].contains("provider-stub"));
        assert_eq!(
            provider_arg.file_name().and_then(|name| name.to_str()),
            Some("bin.js")
        );
        assert_eq!(
            provider_arg
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str()),
            Some("dist")
        );
    }

    #[test]
    fn resolve_unknown_provider_returns_none() {
        let config = PiceConfig::default();
        assert!(resolve("nonexistent", &config).is_none());
    }

    #[test]
    fn find_provider_base_returns_a_path() {
        let base = find_provider_base();
        // Should return something (either workspace root or CWD)
        assert!(base.is_absolute() || base.to_string_lossy() == ".");
    }

    #[test]
    fn strip_windows_verbatim_prefix_normalizes_disk_paths() {
        assert_eq!(
            strip_windows_verbatim_prefix(
                r"\\?\D:\a\m0lz.02\m0lz.02\packages\provider-stub\dist\bin.js"
            ),
            r"D:\a\m0lz.02\m0lz.02\packages\provider-stub\dist\bin.js"
        );
    }

    #[test]
    fn strip_windows_verbatim_prefix_normalizes_unc_paths() {
        assert_eq!(
            strip_windows_verbatim_prefix(
                r"\\?\UNC\server\share\m0lz.02\packages\provider-stub\dist\bin.js"
            ),
            r"\\server\share\m0lz.02\packages\provider-stub\dist\bin.js"
        );
    }

    #[test]
    fn strip_windows_verbatim_prefix_leaves_regular_paths_alone() {
        assert_eq!(
            strip_windows_verbatim_prefix(r"D:\a\m0lz.02\packages\provider-stub\dist\bin.js"),
            r"D:\a\m0lz.02\packages\provider-stub\dist\bin.js"
        );
    }

    #[test]
    #[cfg(windows)]
    fn provider_path_strips_windows_verbatim_disk_prefix_for_node() {
        let base = PathBuf::from(r"\\?\D:\a\m0lz.02\m0lz.02");
        assert_eq!(
            provider_path(&base, "provider-stub"),
            r"D:\a\m0lz.02\m0lz.02\packages\provider-stub\dist\bin.js"
        );
    }

    #[test]
    #[cfg(windows)]
    fn provider_path_strips_windows_verbatim_unc_prefix_for_node() {
        let base = PathBuf::from(r"\\?\UNC\server\share\m0lz.02");
        assert_eq!(
            provider_path(&base, "provider-stub"),
            r"\\server\share\m0lz.02\packages\provider-stub\dist\bin.js"
        );
    }
}
