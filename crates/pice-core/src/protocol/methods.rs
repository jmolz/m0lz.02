//! Daemon RPC method name constants.
//!
//! Phase 0 deliberately keeps the daemon RPC surface minimal — one polymorphic
//! `cli/dispatch` method plus lifecycle methods. Per-command methods
//! (`execute/create`, `evaluate/create`, etc.) will be added in v0.2 Phase 1+
//! when finer-grained layer-scoped control becomes necessary.
//!
//! See `.codex/rules/protocol.md` "Daemon RPC Methods (v0.2)" for the full
//! planned v0.2+ surface.

// ─── Lifecycle methods ──────────────────────────────────────────────────────

/// Liveness probe. Request params: `{}`. Response result: `{"version": "x.y.z", "uptime_seconds": N}`.
pub const DAEMON_HEALTH: &str = "daemon/health";

/// Request orderly daemon shutdown. Request params: `{}`. Response result: `{"shutting_down": true}`.
pub const DAEMON_SHUTDOWN: &str = "daemon/shutdown";

// ─── Dispatch method ────────────────────────────────────────────────────────

/// Execute a `CommandRequest` in the daemon. Request params: serialized
/// `CommandRequest`. The daemon streams chunks/events via notifications on
/// the same connection, then sends a final `cli/stream-done` notification
/// carrying the `CommandResponse` before responding to the original request.
pub const CLI_DISPATCH: &str = "cli/dispatch";

// ─── Streaming notifications (daemon → CLI) ─────────────────────────────────

/// A provider text chunk destined for the CLI's terminal stdout.
/// Params: `{"text": "..."}`.
pub const CLI_STREAM_CHUNK: &str = "cli/stream-chunk";

/// A structured event (evaluation result, progress, warning, etc.).
/// Params: event-specific payload.
pub const CLI_STREAM_EVENT: &str = "cli/stream-event";

/// Final dispatch result. Params: serialized `CommandResponse`.
/// Sent immediately before the final `DaemonResponse` on the same connection
/// so the CLI can render the response synchronously.
pub const CLI_STREAM_DONE: &str = "cli/stream-done";

// ─── Phase 7 subscribe RPCs (router-level, not `cli/dispatch`) ──────────────

/// Subscribe to `manifest/event` notifications. The RPC response body carries
/// the initial snapshot; subsequent `manifest/event` notifications stream on
/// the same connection until the client closes it. There is NO
/// `manifest/unsubscribe` RPC — connection close IS unsubscribe.
///
/// Request params: `SubscribeManifestRequest`. Response result:
/// `SubscribeManifestResponse`. After the response, notifications are sent
/// as `MANIFEST_EVENT`.
pub const MANIFEST_SUBSCRIBE: &str = "manifest/subscribe";

/// Subscribe to `logs/chunk` notifications (when `follow: true`) or request
/// a one-shot log history snapshot (when `follow: false`). The RPC response
/// body carries the history vector; if `follow: true`, subsequent
/// `logs/chunk` notifications stream on the same connection until a terminal
/// frame (`LogChunk { terminal: true }`) is broadcast or the client closes.
pub const LOGS_STREAM: &str = "logs/stream";

// ─── Phase 7 notifications (daemon → CLI, no `id` field) ────────────────────

/// A structured manifest state-transition event (layer started, pass
/// complete, gate requested, etc.). Params: `ManifestEventPayload`.
pub const MANIFEST_EVENT: &str = "manifest/event";

/// A captured provider session log chunk. Params: `LogChunk`. `terminal: true`
/// marks the end-of-stream frame; follow subscribers observe it and exit.
pub const LOGS_CHUNK: &str = "logs/chunk";

#[cfg(test)]
mod tests {
    use super::*;

    /// All method-name constants — used by both uniqueness + kebab-case tests.
    /// Extending this list also extends both parity tests automatically, so a
    /// typo in a new constant cannot silently pass CI.
    const ALL_METHODS: &[&str] = &[
        DAEMON_HEALTH,
        DAEMON_SHUTDOWN,
        CLI_DISPATCH,
        CLI_STREAM_CHUNK,
        CLI_STREAM_EVENT,
        CLI_STREAM_DONE,
        MANIFEST_SUBSCRIBE,
        LOGS_STREAM,
        MANIFEST_EVENT,
        LOGS_CHUNK,
    ];

    #[test]
    fn method_names_are_unique() {
        // Sanity check — catches typos where a new constant accidentally shares
        // a value with an existing one.
        let mut sorted = ALL_METHODS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ALL_METHODS.len(),
            "duplicate method name constant"
        );
    }

    #[test]
    fn method_names_use_kebab_case_segments() {
        // Enforce naming convention: `namespace/kebab-case-method`.
        // Rejects underscores and CamelCase at the method level.
        for name in ALL_METHODS {
            assert!(
                name.contains('/'),
                "method {name} missing namespace/ prefix"
            );
            assert!(
                !name.contains('_'),
                "method {name} should use kebab-case, not snake_case"
            );
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '/' || c == '-'),
                "method {name} has unexpected characters"
            );
        }
    }

    /// Phase 7 sanity: MANIFEST_SUBSCRIBE / LOGS_STREAM are router-level
    /// methods, NOT `cli/dispatch` variants. They live in the router
    /// second-level branch (see `handlers/subscribe.rs`). This test pins
    /// that the names are NOT `cli/*`-prefixed — if someone refactors them
    /// into the `cli/dispatch` funnel by accident, the test catches it.
    #[test]
    fn phase_7_subscribe_methods_are_router_level() {
        for name in &[MANIFEST_SUBSCRIBE, LOGS_STREAM, MANIFEST_EVENT, LOGS_CHUNK] {
            assert!(
                !name.starts_with("cli/"),
                "Phase 7 method {name} must NOT use the cli/ namespace — \
                 it is a router-level RPC, not a cli/dispatch variant"
            );
        }
    }
}
