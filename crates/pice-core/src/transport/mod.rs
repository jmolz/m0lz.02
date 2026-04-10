//! Platform-abstracted transport descriptors for the daemon socket.
//!
//! This module is pure: it only defines the socket path type and the default
//! path resolution logic. The actual async `Read`/`Write`/`listen`/`connect`
//! implementations live in `pice-daemon::server::{unix,windows}` because they
//! depend on `tokio`.
//!
//! ## Platform conventions
//!
//! - **Unix (macOS/Linux):** `~/.pice/daemon.sock` — Unix domain socket file
//!   with 0600 permissions. Enforced by `pice-daemon::server::unix`.
//! - **Windows:** `\\.\pipe\pice-daemon` — named pipe with default owner-only
//!   ACL. Enforced by `pice-daemon::server::windows`.
//!
//! The `PICE_DAEMON_SOCKET` environment variable overrides both conventions
//! when set. Useful for integration tests (per-process sockets in a tempdir).

use std::path::PathBuf;

/// Platform-abstracted path to the daemon socket.
///
/// The two variants correspond to the two transport types. On any platform,
/// exactly one variant is meaningful — the `#[cfg]`-gated code in
/// `pice-daemon::server` picks the right one at compile time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketPath {
    /// Unix domain socket file path (macOS/Linux).
    Unix(PathBuf),
    /// Windows named pipe name, e.g. `\\.\pipe\pice-daemon`.
    Windows(String),
}

impl SocketPath {
    /// Resolve the default socket path from environment and platform convention.
    ///
    /// Priority order:
    /// 1. `PICE_DAEMON_SOCKET` environment variable (absolute path / pipe name)
    /// 2. Platform default: `~/.pice/daemon.sock` on Unix, `\\.\pipe\pice-daemon` on Windows
    ///
    /// On Unix, if `HOME` is unset the fallback is `./.pice/daemon.sock`
    /// (relative to the CWD). This keeps tests that run in a clean environment
    /// deterministic.
    pub fn default_from_env() -> Self {
        if let Ok(s) = std::env::var("PICE_DAEMON_SOCKET") {
            #[cfg(windows)]
            {
                return SocketPath::Windows(s);
            }
            #[cfg(unix)]
            {
                return SocketPath::Unix(PathBuf::from(s));
            }
        }

        #[cfg(unix)]
        {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            SocketPath::Unix(PathBuf::from(home).join(".pice").join("daemon.sock"))
        }

        #[cfg(windows)]
        {
            SocketPath::Windows(r"\\.\pipe\pice-daemon".to_string())
        }
    }

    /// Display form for logging and error messages. Uses the underlying path
    /// string on Unix and the pipe name on Windows.
    pub fn display(&self) -> String {
        match self {
            SocketPath::Unix(path) => path.display().to_string(),
            SocketPath::Windows(name) => name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn default_socket_path_respects_env_var_unix() {
        // Save/restore env to avoid polluting other tests.
        let saved = std::env::var("PICE_DAEMON_SOCKET").ok();

        // SAFETY: tests within a single binary run in the same process, and
        // `cargo test` serializes env-dependent tests by default for this crate
        // because we don't use `parallel` features. But to be safe in a
        // multi-threaded test runner, we set/restore the var within the test
        // body and do not rely on env state outside the assertion window.
        unsafe {
            std::env::set_var("PICE_DAEMON_SOCKET", "/tmp/pice-test.sock");
        }
        let sp = SocketPath::default_from_env();
        match sp {
            SocketPath::Unix(p) => assert_eq!(p, PathBuf::from("/tmp/pice-test.sock")),
            _ => panic!("expected Unix variant on unix"),
        }

        // Restore
        unsafe {
            match saved {
                Some(v) => std::env::set_var("PICE_DAEMON_SOCKET", v),
                None => std::env::remove_var("PICE_DAEMON_SOCKET"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn default_socket_path_platform_fallback_unix() {
        // Save and clear PICE_DAEMON_SOCKET so the fallback path is exercised.
        let saved = std::env::var("PICE_DAEMON_SOCKET").ok();
        unsafe {
            std::env::remove_var("PICE_DAEMON_SOCKET");
        }

        let sp = SocketPath::default_from_env();
        match sp {
            SocketPath::Unix(p) => {
                // Default path ends with .pice/daemon.sock regardless of HOME setting.
                let s = p.to_string_lossy();
                assert!(
                    s.ends_with(".pice/daemon.sock"),
                    "expected fallback path to end with .pice/daemon.sock, got {s}"
                );
            }
            _ => panic!("expected Unix variant on unix"),
        }

        // Restore
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("PICE_DAEMON_SOCKET", v);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn default_socket_path_platform_fallback_windows() {
        let saved = std::env::var("PICE_DAEMON_SOCKET").ok();
        unsafe {
            std::env::remove_var("PICE_DAEMON_SOCKET");
        }

        let sp = SocketPath::default_from_env();
        match sp {
            SocketPath::Windows(name) => {
                assert_eq!(name, r"\\.\pipe\pice-daemon");
            }
            _ => panic!("expected Windows variant on windows"),
        }

        unsafe {
            if let Some(v) = saved {
                std::env::set_var("PICE_DAEMON_SOCKET", v);
            }
        }
    }

    #[test]
    fn socket_path_display_unix() {
        let sp = SocketPath::Unix(PathBuf::from("/var/run/pice.sock"));
        assert_eq!(sp.display(), "/var/run/pice.sock");
    }

    #[test]
    fn socket_path_display_windows() {
        let sp = SocketPath::Windows(r"\\.\pipe\pice-daemon".to_string());
        assert_eq!(sp.display(), r"\\.\pipe\pice-daemon");
    }

    #[test]
    fn socket_path_equality() {
        let a = SocketPath::Unix(PathBuf::from("/tmp/a"));
        let b = SocketPath::Unix(PathBuf::from("/tmp/a"));
        let c = SocketPath::Unix(PathBuf::from("/tmp/c"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
