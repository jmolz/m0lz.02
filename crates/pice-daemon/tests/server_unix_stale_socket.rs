//! Integration test for Unix-socket stale-cleanup recovery.
//!
//! This test deliberately lives in its own integration test binary rather than
//! next to the `server::unix` unit tests because of a macOS fd-inheritance race
//! that surfaces under parallel unit-test execution:
//!
//! When the `pice_daemon` unit-test binary runs all 47 of its tests at once, a
//! sibling test (`prompt::builders::*`) spawns `git` subprocesses. On macOS —
//! and despite `SOCK_CLOEXEC` being set atomically by Rust's stdlib — there is
//! a window during which a freshly-bound stale-simulator socket fd can leak
//! into a concurrently-forked child. The child inherits a reference to the
//! socket, so even after the parent's `UnixListener::drop` closes *its* fd the
//! kernel considers the socket live (the child still holds a ref). A
//! subsequent `UnixStream::connect` from the parent succeeds, which makes our
//! stale-detection probe wrongly conclude "a daemon is listening", and the
//! test fails intermittently.
//!
//! Moving the test into a dedicated integration test binary puts it in its own
//! process, with no sibling threads forking anything. The race cannot happen
//! because there are no concurrent forks in this process. The test runs
//! deterministically on every platform.
//!
//! **Do NOT inline this into `src/server/unix.rs`.** The flake is intrinsic to
//! shared-process parallelism; the split is the fix, not a workaround.

#![cfg(unix)]

use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::PathBuf;

use pice_daemon::server::unix::UnixSocketListener;
use tempfile::tempdir;

/// Matches the helper in `src/server/unix.rs` tests — produces a temp socket
/// path whose `TempDir` handle must outlive the listener.
fn temp_socket_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("pice.sock");
    (dir, path)
}

#[tokio::test]
async fn stale_socket_is_cleaned_up_on_rebind() {
    let (_tmp, path) = temp_socket_path();

    // Simulate a SIGKILL'd daemon by binding a *std* (non-tokio) `UnixListener`
    // and dropping it. std's `Drop` synchronously calls `libc::close(fd)`, so
    // the kernel state becomes "socket file on disk, no listener" the moment
    // the inner scope exits. Because this integration test binary has no
    // sibling test threads forking subprocesses (see module docs), there is
    // no fd-leak path via child inheritance.
    {
        let _stale = StdUnixListener::bind(&path).expect("setup: bind stale std listener");
    }
    assert!(
        path.exists(),
        "setup invariant: stale socket file must remain on disk after drop"
    );

    // `bind_with_stale_recovery` must probe via `UnixStream::connect`, observe
    // `ECONNREFUSED`, remove the stale file, and rebind successfully.
    let listener = UnixSocketListener::bind(&path)
        .await
        .expect("rebind after stale cleanup");
    assert!(
        path.exists(),
        "new socket should exist after stale recovery"
    );
    drop(listener);
}
