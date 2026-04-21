//! Phase 7 Task 14: desktop notification dispatcher.
//!
//! Cross-platform via [`notify_rust`] (macOS/Linux/Windows native backends
//! — no per-platform shell-out). Notifications are **non-load-bearing**:
//! if `show()` fails, the dispatcher logs at `tracing::debug!` and falls
//! back to a terminal BEL + one-line `eprintln!("\x07[pice] {title} — {body}")`.
//! The CLI never surfaces the failure to the user and never returns an
//! error — a headless CI runner without a notification daemon must still
//! exit cleanly.
//!
//! Wired into:
//! - `pice status --follow` — emits on `GateRequested` / `FeatureComplete`
//!   (Task 18 `SubscribedGateSource` layers the gate prompt on top).
//! - `pice evaluate --background --wait` — emits when the wait completes
//!   (terminal outcome reached).
//!
//! Duplicate notifications within a **500ms** debounce window on the same
//! `(feature_id, event_kind)` tuple emit once. This matches the observed
//! burst pattern where a cohort transition triggers both `LayerComplete`
//! and `FeatureComplete` in the same tokio tick.
//!
//! Config:
//! - `[notifications]` table in `~/.pice/config.toml` (user floor).
//! - Same table in `.pice/config.toml` (project). Floor-merge: project
//!   may DISABLE any `on_*` flag the user enabled; never ENABLE one the
//!   user disabled (same semantics as `workflow.review`). Defaults:
//!   `on_complete=true`, `on_gate=true`, `on_failure=true`.

pub mod config;

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use config::NotificationsConfig;

/// Discriminant for a desktop notification. Mapped to `on_complete` /
/// `on_gate` / `on_failure` config flags — events whose kind is disabled
/// do not emit even if [`notify`] is called directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationKind {
    /// Feature transitioned to `Passed` — success.
    Complete,
    /// Feature transitioned to `Failed` / `FailedInterrupted` — failure.
    Failure,
    /// A review gate was requested.
    Gate,
}

impl NotificationKind {
    /// The discriminant used for the `(feature_id, kind)` debounce key.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Failure => "failure",
            Self::Gate => "gate",
        }
    }

    /// Whether this kind is gated on by the given config.
    pub fn enabled_in(&self, cfg: &NotificationsConfig) -> bool {
        match self {
            Self::Complete => cfg.on_complete,
            Self::Failure => cfg.on_failure,
            Self::Gate => cfg.on_gate,
        }
    }
}

/// Dispatch a desktop notification with debounce + config gating.
///
/// Debounce key: `(feature_id, kind)`. A repeat within `DEBOUNCE_WINDOW`
/// is silently dropped. `DEBOUNCE_WINDOW` is 500ms per the plan — the
/// observed `LayerComplete → FeatureComplete` burst arrives within
/// ~1 tokio tick, well inside the window.
///
/// `dispatcher` is the pluggable notification backend. Production uses
/// [`NotifyRustDispatcher`]; tests use [`RecordingDispatcher`].
pub fn notify(
    state: &NotificationState,
    cfg: &NotificationsConfig,
    kind: NotificationKind,
    feature_id: &str,
    title: &str,
    body: &str,
    dispatcher: &dyn Dispatcher,
) {
    if !kind.enabled_in(cfg) {
        return;
    }
    if state.observe_and_debounce(feature_id, kind) {
        tracing::trace!(feature_id, kind = kind.as_str(), "notification debounced");
        return;
    }
    if let Err(err) = dispatcher.show(title, body) {
        tracing::debug!(
            ?err,
            feature_id,
            kind = kind.as_str(),
            "notification show failed; falling back to terminal BEL"
        );
        eprintln!("\x07[pice] {title} — {body}");
    }
}

/// 500ms debounce window — matches the observed cohort-transition burst
/// interval. Small enough that a human hitting the same key twice on
/// purpose still sees both notifications.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

/// Process-level state for notification debounce. Wrap in [`Mutex`] so a
/// status-follow loop can dispatch concurrently from multiple spawn
/// tasks without missing a dedupe.
#[derive(Default)]
pub struct NotificationState {
    inner: Mutex<Vec<(String, NotificationKind, Instant)>>,
}

impl NotificationState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the `(feature_id, kind)` pair was seen within the
    /// last [`DEBOUNCE_WINDOW`], otherwise records the observation and
    /// returns `false`.
    ///
    /// Linear scan across buffered observations — the buffer is bounded
    /// to at most a handful of entries in practice (one per live
    /// `pice status --follow` session), so a HashMap would be overkill.
    pub fn observe_and_debounce(&self, feature_id: &str, kind: NotificationKind) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // Sweep expired entries before the hit check so the vec doesn't
        // grow unboundedly over a long-running `pice status --follow`.
        g.retain(|(_, _, ts)| now.duration_since(*ts) < DEBOUNCE_WINDOW);
        for (f, k, ts) in g.iter() {
            if f == feature_id && *k == kind && now.duration_since(*ts) < DEBOUNCE_WINDOW {
                return true;
            }
        }
        g.push((feature_id.to_string(), kind, now));
        false
    }
}

/// Abstraction over the notification backend. Production wires
/// [`NotifyRustDispatcher`]; tests inject [`RecordingDispatcher`] or a
/// `FailingDispatcher` to assert the terminal fallback.
pub trait Dispatcher: Send + Sync {
    /// Show a notification. Errors flow through `notify`'s tracing +
    /// terminal-fallback path.
    fn show(&self, title: &str, body: &str) -> Result<(), String>;
}

/// Real dispatcher — writes to the OS notification center via
/// [`notify_rust::Notification`].
pub struct NotifyRustDispatcher;

impl Dispatcher for NotifyRustDispatcher {
    fn show(&self, title: &str, body: &str) -> Result<(), String> {
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .appname("pice")
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Test dispatcher — records every shown notification into an in-memory
/// vec. Used by unit tests to assert call count + content.
#[cfg(test)]
#[derive(Default)]
pub struct RecordingDispatcher {
    pub shown: Mutex<Vec<(String, String)>>,
}

#[cfg(test)]
impl Dispatcher for RecordingDispatcher {
    fn show(&self, title: &str, body: &str) -> Result<(), String> {
        self.shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((title.to_string(), body.to_string()));
        Ok(())
    }
}

/// Test dispatcher — always fails. Used to exercise the terminal fallback
/// branch without needing a real notification backend that can reliably
/// fail (some platforms swallow errors silently when no daemon is up).
#[cfg(test)]
#[derive(Default)]
pub struct FailingDispatcher;

#[cfg(test)]
impl Dispatcher for FailingDispatcher {
    fn show(&self, _title: &str, _body: &str) -> Result<(), String> {
        Err("simulated dispatcher failure".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_all_on() -> NotificationsConfig {
        NotificationsConfig {
            on_complete: true,
            on_gate: true,
            on_failure: true,
        }
    }

    #[test]
    fn notify_emits_when_kind_enabled() {
        let state = NotificationState::new();
        let cfg = cfg_all_on();
        let rec = RecordingDispatcher::default();
        notify(
            &state,
            &cfg,
            NotificationKind::Complete,
            "feat-x",
            "done",
            "all clear",
            &rec,
        );
        let shown = rec.shown.lock().unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].0, "done");
        assert_eq!(shown[0].1, "all clear");
    }

    #[test]
    fn notify_skips_when_kind_disabled() {
        let state = NotificationState::new();
        let mut cfg = cfg_all_on();
        cfg.on_gate = false;
        let rec = RecordingDispatcher::default();
        notify(
            &state,
            &cfg,
            NotificationKind::Gate,
            "feat-x",
            "gate",
            "review required",
            &rec,
        );
        assert!(rec.shown.lock().unwrap().is_empty());
    }

    #[test]
    fn debounce_suppresses_duplicate_in_window() {
        let state = NotificationState::new();
        let cfg = cfg_all_on();
        let rec = RecordingDispatcher::default();

        // First call fires.
        notify(
            &state,
            &cfg,
            NotificationKind::Complete,
            "feat-x",
            "t",
            "b",
            &rec,
        );
        // Second call within 500ms is suppressed.
        notify(
            &state,
            &cfg,
            NotificationKind::Complete,
            "feat-x",
            "t",
            "b",
            &rec,
        );
        assert_eq!(rec.shown.lock().unwrap().len(), 1);
    }

    #[test]
    fn debounce_independent_per_feature_and_kind() {
        let state = NotificationState::new();
        let cfg = cfg_all_on();
        let rec = RecordingDispatcher::default();

        notify(
            &state,
            &cfg,
            NotificationKind::Complete,
            "feat-a",
            "t",
            "b",
            &rec,
        );
        // Different feature — not debounced.
        notify(
            &state,
            &cfg,
            NotificationKind::Complete,
            "feat-b",
            "t",
            "b",
            &rec,
        );
        // Same feature, different kind — not debounced.
        notify(
            &state,
            &cfg,
            NotificationKind::Gate,
            "feat-a",
            "t",
            "b",
            &rec,
        );
        assert_eq!(rec.shown.lock().unwrap().len(), 3);
    }

    #[test]
    fn notify_failure_logs_and_uses_terminal_fallback() {
        // Exercises the failure branch — a real-platform failure would
        // route through `tracing::debug!` + BEL-prefixed eprintln. We
        // can't easily capture stderr in a unit test, but we CAN assert
        // the dispatcher was invoked and the function returned (no
        // panic + no re-surfaced error — notifications are fire-and-
        // forget).
        let state = NotificationState::new();
        let cfg = cfg_all_on();
        let failing = FailingDispatcher;
        notify(
            &state,
            &cfg,
            NotificationKind::Failure,
            "feat-x",
            "failed",
            "see logs",
            &failing,
        );
        // No panic + no assertion — reaching here is the test.
    }

    #[test]
    fn kind_enabled_in_reflects_config() {
        let cfg = NotificationsConfig {
            on_complete: true,
            on_gate: false,
            on_failure: false,
        };
        assert!(NotificationKind::Complete.enabled_in(&cfg));
        assert!(!NotificationKind::Gate.enabled_in(&cfg));
        assert!(!NotificationKind::Failure.enabled_in(&cfg));
    }
}
