//! Daemon RPC method name constants.
//!
//! Phase 0 deliberately keeps the daemon RPC surface minimal — one polymorphic
//! `cli/dispatch` method plus lifecycle methods. Per-command methods
//! (`execute/create`, `evaluate/create`, etc.) will be added in v0.2 Phase 1+
//! when finer-grained layer-scoped control becomes necessary.
//!
//! See `.claude/rules/protocol.md` "Daemon RPC Methods (v0.2)" for the full
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_names_are_unique() {
        // Sanity check — catches typos where a new constant accidentally shares
        // a value with an existing one.
        let all = [
            DAEMON_HEALTH,
            DAEMON_SHUTDOWN,
            CLI_DISPATCH,
            CLI_STREAM_CHUNK,
            CLI_STREAM_EVENT,
            CLI_STREAM_DONE,
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "duplicate method name constant");
    }

    #[test]
    fn method_names_use_kebab_case_segments() {
        // Enforce naming convention: `namespace/kebab-case-method`.
        // Rejects underscores and CamelCase at the method level.
        for name in [
            DAEMON_HEALTH,
            DAEMON_SHUTDOWN,
            CLI_DISPATCH,
            CLI_STREAM_CHUNK,
            CLI_STREAM_EVENT,
            CLI_STREAM_DONE,
        ] {
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
}
