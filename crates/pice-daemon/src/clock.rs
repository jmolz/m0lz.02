//! Time abstraction for the gate reconciler and gate-decide handler.
//!
//! Phase 6's reconciler (`orchestrator::gate_reconciler`) and the
//! `ReviewGate::Decide` handler both need to ask "has this gate
//! expired?". Production uses [`SystemClock`] (wraps `chrono::Utc::now`
//! and `tokio::time::sleep`). Tests use [`MockClock`] to advance time
//! deterministically under `tokio::test(start_paused = true)`.
//!
//! ## Why is this in `pice-daemon` and not `pice-core`?
//!
//! `pice-core` is dependency-free of async / network / database
//! (contract criterion #15 for Phase 6 and, earlier, criterion #1 of
//! the Phase-0 daemon-foundation plan). `SystemClock` needs
//! `tokio::time::sleep`; `MockClock` needs `tokio::sync::Notify` to
//! wake a sleeper when mock time advances. Both imports are forbidden
//! in `pice-core`. Keeping the trait + impls next to the async
//! consumers (the reconciler + the handler) avoids a partial definition
//! split across crates.
//!
//! The pure helpers that this module composes with
//! ([`pice_core::gate::resolve_timeout_action`],
//! [`pice_core::gate::apply_timeout_if_expired`]) take
//! `now: DateTime<Utc>` as a parameter and live in `pice-core` — so
//! the unit tests for the gate state machine itself run there without
//! pulling tokio into `pice-core`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Notify;

/// Source of current time + async sleep primitive.
///
/// Production: `Arc::new(SystemClock)`. Tests:
/// `Arc::new(MockClock::new(t0))` + `clock.advance(dur)` to trigger
/// sleepers deterministically.
///
/// `dyn Clock` is object-safe via `#[async_trait]` because we need to
/// store `Arc<dyn Clock>` on `DaemonContext` (threaded into the
/// reconciler task) and not parameterize every handler on a generic
/// `C: Clock`. The `Send + Sync` bounds let the clock cross task /
/// thread boundaries without `.lock()` contention.
#[async_trait]
pub trait Clock: Send + Sync {
    /// Current UTC time. Persisted into `requested_at` / `decided_at`
    /// RFC3339 strings in the manifest + SQLite audit rows.
    fn now(&self) -> DateTime<Utc>;

    /// Block until `at` is reached. Returns immediately if `at <= now()`.
    ///
    /// The implementation MUST handle mock-clock advance: in tests, a
    /// call to `MockClock::advance` that pushes `now` past `at` should
    /// wake the sleeper within one notification round-trip, NOT leave
    /// the task blocked on real wall-clock time.
    async fn sleep_until(&self, at: DateTime<Utc>);
}

/// Production clock: `chrono::Utc::now` + `tokio::time::sleep`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    async fn sleep_until(&self, at: DateTime<Utc>) {
        let now = Utc::now();
        let remaining = at - now;
        if remaining <= chrono::Duration::zero() {
            return;
        }
        // `to_std()` rejects negative durations; we've already clamped
        // on the zero check above. Any residual Err from an extreme
        // edge case (e.g. chrono duration overflow) degrades to a
        // single-tick wait so the reconciler loop doesn't stall.
        let dur = remaining
            .to_std()
            .unwrap_or(std::time::Duration::from_millis(1));
        tokio::time::sleep(dur).await;
    }
}

/// Test clock backed by a shared, mutable `now` cell + `Notify`
/// wake-up for any in-flight sleepers.
///
/// Use under `#[tokio::test(start_paused = true)]` so tokio's own
/// timer wheel doesn't race the mock. `advance(dur)` bumps `now` and
/// wakes every sleeper; sleepers re-check `now >= at` and return if
/// satisfied or continue waiting otherwise.
///
/// Gated to `#[cfg(test)]` so the `.expect()` panic sites (documented
/// as "fail the test loudly if an earlier assertion poisoned the
/// mutex") don't trip the strict `clippy::expect_used` deny on
/// production code. The gate widens to a `test-utils` feature if
/// Phase 7 adds the full background reconciler that needs a
/// cross-crate mock.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct MockClock {
    // `std::sync::Mutex` rather than `tokio::sync::Mutex` on purpose:
    // the critical section is a couple of DateTime assignments, never
    // held across `.await`. A `tokio::sync::Mutex` here would yield
    // mid-assignment and give Notify-waiters a chance to see a torn
    // state.
    inner: Arc<std::sync::Mutex<DateTime<Utc>>>,
    wake: Arc<Notify>,
}

#[cfg(test)]
impl MockClock {
    /// Construct a mock clock anchored at `start`.
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(start)),
            wake: Arc::new(Notify::new()),
        }
    }

    /// Advance `now` by `dur` and wake every in-flight sleeper. Panics
    /// if the mutex is poisoned — a poisoned mutex in a test clock
    /// means an earlier assertion panicked with the lock held, which
    /// should fail the test loudly, not silently.
    pub fn advance(&self, dur: chrono::Duration) {
        {
            let mut guard = self.inner.lock().expect("MockClock mutex poisoned");
            *guard += dur;
        }
        self.wake.notify_waiters();
    }

    /// Jump `now` to a specific instant. Mostly useful when a test
    /// needs to pin time relative to a fixture's pinned RFC3339
    /// deadline rather than reason about a duration.
    pub fn set(&self, at: DateTime<Utc>) {
        {
            let mut guard = self.inner.lock().expect("MockClock mutex poisoned");
            *guard = at;
        }
        self.wake.notify_waiters();
    }
}

#[cfg(test)]
#[async_trait]
impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.inner.lock().expect("MockClock mutex poisoned")
    }

    async fn sleep_until(&self, at: DateTime<Utc>) {
        // Re-check in a loop: `notify_waiters` only wakes permits that
        // existed at the time of the call. A sleeper that registers
        // after `advance` has already bumped `now` past `at` must still
        // return immediately, not park forever. The `notified()` future
        // + subsequent `now` read handles both orderings:
        //   1. now is already past at → return before any await
        //   2. advance happens during notified().await → wake, re-check
        loop {
            if self.now() >= at {
                return;
            }
            // Prepare the notified future BEFORE re-checking now to
            // close the advance-then-register race (per tokio Notify
            // docs).
            let waiter = self.wake.notified();
            if self.now() >= at {
                return;
            }
            waiter.await;
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fixed(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn system_clock_now_advances_monotonically() {
        let c = SystemClock;
        let a = c.now();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let b = c.now();
        assert!(b >= a);
    }

    #[tokio::test]
    async fn system_clock_sleep_until_past_returns_immediately() {
        let c = SystemClock;
        let start = std::time::Instant::now();
        c.sleep_until(fixed("2000-01-01T00:00:00Z")).await;
        // Past-dated sleep must not actually wait — allow a 50ms slack
        // for CI scheduler jitter.
        assert!(start.elapsed() < std::time::Duration::from_millis(50));
    }

    #[tokio::test]
    async fn mock_clock_advance_wakes_sleeper() {
        let clock = Arc::new(MockClock::new(fixed("2026-04-20T00:00:00Z")));
        let target = fixed("2026-04-20T00:00:10Z");
        let clock2 = Arc::clone(&clock);

        let sleeper = tokio::spawn(async move {
            clock2.sleep_until(target).await;
            clock2.now()
        });

        // Let the sleeper park, then advance past the target.
        tokio::task::yield_now().await;
        clock.advance(chrono::Duration::seconds(15));

        let woke_at = tokio::time::timeout(std::time::Duration::from_secs(1), sleeper)
            .await
            .expect("sleeper did not wake within 1s")
            .unwrap();
        assert!(woke_at >= target);
    }

    #[tokio::test]
    async fn mock_clock_advance_below_target_does_not_wake() {
        let clock = Arc::new(MockClock::new(fixed("2026-04-20T00:00:00Z")));
        let target = fixed("2026-04-20T00:00:30Z");
        let clock2 = Arc::clone(&clock);

        let sleeper = tokio::spawn(async move {
            clock2.sleep_until(target).await;
            true
        });

        // Advance 10s — still 20s short of the 30s target.
        tokio::task::yield_now().await;
        clock.advance(chrono::Duration::seconds(10));
        tokio::task::yield_now().await;

        // Sleeper must still be parked.
        assert!(!sleeper.is_finished(), "sleeper woke too early");

        // Bring it home.
        clock.advance(chrono::Duration::seconds(25));
        let done = tokio::time::timeout(std::time::Duration::from_secs(1), sleeper)
            .await
            .expect("sleeper did not wake on second advance")
            .unwrap();
        assert!(done);
    }

    #[tokio::test]
    async fn mock_clock_set_triggers_wake() {
        let clock = Arc::new(MockClock::new(fixed("2026-04-20T00:00:00Z")));
        let target = fixed("2027-01-01T00:00:00Z");
        let clock2 = Arc::clone(&clock);
        let sleeper = tokio::spawn(async move { clock2.sleep_until(target).await });
        tokio::task::yield_now().await;
        clock.set(fixed("2027-06-01T00:00:00Z"));
        tokio::time::timeout(std::time::Duration::from_secs(1), sleeper)
            .await
            .expect("sleeper did not wake after set past target")
            .unwrap();
    }

    #[tokio::test]
    async fn dyn_clock_is_object_safe() {
        // Construction through `Arc<dyn Clock>` is the production
        // shape — if this compiles the trait is object-safe.
        let _c: Arc<dyn Clock> = Arc::new(SystemClock);
        let _m: Arc<dyn Clock> = Arc::new(MockClock::new(fixed("2026-04-20T00:00:00Z")));
    }
}
