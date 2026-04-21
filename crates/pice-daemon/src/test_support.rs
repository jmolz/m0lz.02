//! Test support helpers shared across lib-level `#[cfg(test)]` inline
//! modules AND the integration-test binaries under `crates/pice-daemon/
//! tests/*.rs`.
//!
//! The Phase 6 review-gate tests serialize access to the process-
//! global `PICE_STATE_DIR` env var via a mutex. Integration-test
//! binaries and the lib-test binary compile into SEPARATE processes
//! under `cargo test`, so a static in this module produces a distinct
//! `Mutex<()>` instance per binary (which is exactly the intended
//! semantic — each binary's tests mutually exclude WITHIN the binary,
//! and no sharing is needed across binaries because each `cargo test`
//! test binary is its own process).
//!
//! The module is `pub` at crate root so integration tests can
//! `use pice_daemon::test_support::{StateDirGuard, state_dir_lock};`
//! rather than duplicating the struct definition. Definition drift
//! across duplicated copies (e.g., one binary forgets to restore the
//! prior env value on Drop while another remembers) is a real risk
//! the Phase 6 eval-pass-2 review flagged — centralizing the type
//! removes it.
//!
//! This module must NOT pull in heavy runtime dependencies or
//! introduce new `unwrap/expect` calls in production code paths; it
//! contains only test helpers and is guarded by the Phase 6 `#[cfg(
//! any(test, debug_assertions))]` gate in the rare case `cargo build
//! --release` ever compiles it (the current crate has no non-test
//! consumer — gate tightens to `#[cfg(test)]` if we add a
//! `test-utils` feature down the line).

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Per-binary serialization lock for tests that mutate
/// `PICE_STATE_DIR`. Each test binary (lib + each integration file)
/// produces its own static instance — which is the intended behavior,
/// since each binary runs in its own process.
pub fn state_dir_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII guard that:
/// 1. Acquires the per-binary `state_dir_lock`.
/// 2. Snapshots the existing `PICE_STATE_DIR` value (if any).
/// 3. Sets `PICE_STATE_DIR` to `dir`.
/// 4. On drop: restores the prior env value (set or unset it).
///
/// The lock prevents two `#[tokio::test]` tasks in the same binary
/// from racing on the process-global env mutation. The prior-value
/// snapshot keeps tests hermetic — a test that panics mid-flight
/// still restores the env to what the surrounding test harness
/// expects.
pub struct StateDirGuard<'a> {
    // Held for the lifetime of the guard. Poison is recovered
    // silently (`unwrap_or_else(|p| p.into_inner())`) because a
    // poisoned lock means an earlier test panicked while holding it
    // — the follow-up test's assertions will panic loud if the
    // recovered env state is wrong.
    _guard: MutexGuard<'a, ()>,
    prev: Option<String>,
}

impl<'a> StateDirGuard<'a> {
    /// Acquire the serialization lock, snapshot + override
    /// `PICE_STATE_DIR`. The returned guard MUST outlive every
    /// interaction with `PICE_STATE_DIR` in the test body.
    pub fn new(dir: &std::path::Path) -> Self {
        let guard = state_dir_lock().lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("PICE_STATE_DIR").ok();
        std::env::set_var("PICE_STATE_DIR", dir);
        Self {
            _guard: guard,
            prev,
        }
    }
}

impl Drop for StateDirGuard<'_> {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var("PICE_STATE_DIR", v),
            None => std::env::remove_var("PICE_STATE_DIR"),
        }
    }
}
