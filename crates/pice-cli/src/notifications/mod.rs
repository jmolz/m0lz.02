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

/// Pluggable fallback writer. Production uses [`StderrFallbackWriter`] which
/// calls `eprintln!`; tests inject [`CapturingFallbackWriter`] to assert the
/// exact bytes written without needing to redirect the process's stderr fd.
pub trait FallbackWriter: Send + Sync {
    fn write_line(&self, line: &str);
}

/// Production fallback writer — writes to process stderr via `eprintln!`.
pub struct StderrFallbackWriter;

impl FallbackWriter for StderrFallbackWriter {
    fn write_line(&self, line: &str) {
        eprintln!("{line}");
    }
}

/// Test fallback writer — appends to an `Arc<Mutex<String>>` so the test
/// can assert on the captured content without redirecting stderr.
#[cfg(test)]
#[derive(Default, Clone)]
pub struct CapturingFallbackWriter {
    pub captured: std::sync::Arc<std::sync::Mutex<String>>,
}

#[cfg(test)]
impl FallbackWriter for CapturingFallbackWriter {
    fn write_line(&self, line: &str) {
        let mut g = self.captured.lock().unwrap_or_else(|e| e.into_inner());
        g.push_str(line);
        g.push('\n');
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
    notify_with_fallback(
        state,
        cfg,
        kind,
        feature_id,
        title,
        body,
        dispatcher,
        &StderrFallbackWriter,
    );
}

/// Internal variant of [`notify`] with a pluggable fallback writer. Called
/// by the public `notify` with the production [`StderrFallbackWriter`]; tests
/// call this directly with a [`CapturingFallbackWriter`] to assert content.
#[allow(clippy::too_many_arguments)]
pub fn notify_with_fallback(
    state: &NotificationState,
    cfg: &NotificationsConfig,
    kind: NotificationKind,
    feature_id: &str,
    title: &str,
    body: &str,
    dispatcher: &dyn Dispatcher,
    fallback: &dyn FallbackWriter,
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
        fallback.write_line(&format!("\x07[pice] {title} — {body}"));
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
        // Exercises the failure branch end-to-end with two hard assertions:
        //
        // 1. `tracing::debug!` was emitted — verified via a test-local
        //    `tracing_subscriber` layer that records events to an
        //    `Arc<Mutex<Vec<String>>>`.
        //
        // 2. The fallback writer received the BEL + title + body line —
        //    verified via `CapturingFallbackWriter` (dependency-injected,
        //    avoids OS-level stderr redirection which is shared state across
        //    concurrent test threads).
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::{layer::SubscriberExt, Layer};

        // ── Tracing capture ──────────────────────────────────────────────
        // Each entry in `debug_events` is the formatted message string of a
        // `tracing::debug!` event captured during this test's scope. We use
        // a `Mutex<Vec<String>>` because `tracing_subscriber::Layer::on_event`
        // is called synchronously (no async), and we only need a single-
        // threaded assertion after the `notify_with_fallback` call.
        let debug_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&debug_events);

        // Build a thin recording layer. We record only DEBUG-level events
        // that contain "falling back" — the specific message from the
        // failure branch in `notify_with_fallback`.
        struct DebugRecorder {
            events: Arc<Mutex<Vec<String>>>,
        }
        impl<S: tracing::Subscriber> Layer<S> for DebugRecorder {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() != tracing::Level::DEBUG {
                    return;
                }
                // Collect the debug message into a string via a visitor.
                struct MsgVisitor(String);
                impl tracing::field::Visit for MsgVisitor {
                    fn record_debug(
                        &mut self,
                        _field: &tracing::field::Field,
                        value: &dyn std::fmt::Debug,
                    ) {
                        self.0.push_str(&format!("{value:?}"));
                    }
                    fn record_str(&mut self, _field: &tracing::field::Field, value: &str) {
                        self.0.push_str(value);
                    }
                }
                let mut visitor = MsgVisitor(String::new());
                event.record(&mut visitor);
                self.events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(visitor.0);
            }
        }

        let recorder = DebugRecorder {
            events: events_clone,
        };

        // Install a test-scoped subscriber. `try_init()` may fail when
        // another test already set a global default — guard with `let _ =`.
        // We then use `with_default` to scope the subscriber to this
        // thread so parallel tests don't interfere.
        let subscriber = tracing_subscriber::registry().with(recorder);
        let _guard = tracing::subscriber::set_default(subscriber);

        // ── Fallback writer ──────────────────────────────────────────────
        let writer = CapturingFallbackWriter::default();

        let state = NotificationState::new();
        let cfg = cfg_all_on();
        let failing = FailingDispatcher;

        notify_with_fallback(
            &state,
            &cfg,
            NotificationKind::Failure,
            "feat-x",
            "failed",
            "see logs",
            &failing,
            &writer,
        );

        // ── Assert 1: tracing::debug! was emitted ───────────────────────
        let events = debug_events.lock().unwrap_or_else(|e| e.into_inner());
        let found_debug = events
            .iter()
            .any(|e| e.contains("falling back") || e.contains("terminal BEL"));
        assert!(
            found_debug,
            "expected a tracing::debug! event containing 'falling back' or 'terminal BEL', \
             got events: {events:?}"
        );
        drop(events);

        // ── Assert 2: fallback writer received BEL + title + body ───────
        let captured = writer
            .captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert!(
            captured.contains('\x07'),
            "fallback line must start with BEL (\\x07), got: {captured:?}"
        );
        assert!(
            captured.contains("failed"),
            "fallback line must contain the notification title ('failed'), got: {captured:?}"
        );
        assert!(
            captured.contains("see logs"),
            "fallback line must contain the notification body ('see logs'), got: {captured:?}"
        );
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
