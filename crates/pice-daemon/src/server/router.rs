//! Daemon RPC router — method dispatch table and shared daemon context.
//!
//! The router sits between authentication ([`super::auth`]) and the per-command
//! handlers (`crate::handlers::*`). It receives an already-framed
//! [`DaemonRequest`], validates the bearer token, then dispatches to the
//! appropriate method handler.
//!
//! ## Phase 0 method surface
//!
//! | Method | Handler | Purpose |
//! |--------|---------|---------|
//! | `daemon/health` | [`handle_health`] | Liveness probe + version |
//! | `daemon/shutdown` | [`handle_shutdown`] | Orderly shutdown request |
//! | `cli/dispatch` | [`handle_dispatch`] | Execute a `CommandRequest` (T19 stub) |
//! | anything else | — | `-32601 method not found` |
//!
//! ## `DaemonContext`
//!
//! [`DaemonContext`] is the shared state struct threaded through every handler.
//! Phase 0 defines the minimal fields required by T17's auth + T18's router.
//! T19 (handlers), T20 (inline mode), and T21 (lifecycle) extend it with
//! orchestrator, metrics DB, config, and provider host references.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use pice_core::cli::CommandRequest;
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use serde_json::json;

use super::auth;
use crate::handlers;
use crate::orchestrator::NullSink;

/// JSON-RPC error code for "method not found" (standard JSON-RPC 2.0).
const METHOD_NOT_FOUND_CODE: i32 = -32601;

/// JSON-RPC error code for "invalid params" (standard JSON-RPC 2.0).
const INVALID_PARAMS_CODE: i32 = -32602;

/// JSON-RPC error code for "internal error" (standard JSON-RPC 2.0).
const INTERNAL_ERROR_CODE: i32 = -32603;

/// Shared daemon state threaded through every RPC handler.
///
/// Constructed once during daemon startup (T21) and shared via `&DaemonContext`
/// across all connection-handling tasks. All fields are either immutable after
/// construction or interior-mutable (`AtomicBool`) so `&self` suffices.
///
/// ## Extension plan
///
/// T19 adds: `orchestrator: ProviderOrchestrator`, provider registry.
/// T20 adds: `DaemonContext::inline()` constructor (no socket, no token).
/// T21 adds: config, metrics DB handle, socket path, log handle.
pub struct DaemonContext {
    /// The active bearer token for this daemon instance. Generated on startup,
    /// rotated on every restart. Compared with constant-time equality in
    /// [`auth::validate_request`].
    active_token: String,

    /// Crate version from `Cargo.toml`, baked in at compile time.
    version: &'static str,

    /// Monotonic timestamp of daemon startup, used to compute `uptime_seconds`
    /// in the `daemon/health` response.
    start_time: Instant,

    /// Set to `true` by [`handle_shutdown`]. The lifecycle event loop (T21)
    /// observes this flag to begin the graceful shutdown sequence.
    ///
    /// `Relaxed` ordering is sufficient: the shutdown flag is advisory (the
    /// event loop polls it periodically), not a synchronization fence.
    shutdown_requested: AtomicBool,
}

impl DaemonContext {
    /// Construct a new context. Called once during daemon startup.
    ///
    /// `token` is the hex-encoded bearer token from [`auth::generate_token`].
    pub fn new(token: String) -> Self {
        Self {
            active_token: token,
            version: env!("CARGO_PKG_VERSION"),
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
        }
    }

    /// Construct a minimal context for inline mode (no socket, no auth).
    ///
    /// Used by `PICE_DAEMON_INLINE=1` and integration tests. Skips: socket
    /// setup, auth token generation, stale-cleanup, watchdog. The token is
    /// set to an empty string since inline mode never validates auth.
    pub fn inline() -> Self {
        Self {
            active_token: String::new(),
            version: env!("CARGO_PKG_VERSION"),
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
        }
    }

    /// Check whether a shutdown has been requested.
    ///
    /// The lifecycle event loop (T21) calls this to decide when to begin the
    /// graceful shutdown sequence.
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Relaxed)
    }

    /// Test-only constructor with a custom version string.
    ///
    /// Uses a fixed version instead of `env!("CARGO_PKG_VERSION")` so tests
    /// can assert on a known value without depending on Cargo.toml.
    #[cfg(test)]
    pub(crate) fn new_for_test(token: &str) -> Self {
        Self {
            active_token: token.to_string(),
            version: "0.1.0-test",
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
        }
    }
}

/// Authenticate and dispatch a daemon RPC request.
///
/// This is the top-level entry point called by the connection handler (T21)
/// after framing a complete JSON line into a [`DaemonRequest`].
///
/// Returns a [`DaemonResponse`] in all cases — the caller writes it back on
/// the same connection. Auth failures and unknown methods produce error
/// responses, never panics.
pub async fn route(req: DaemonRequest, ctx: &DaemonContext) -> DaemonResponse {
    // Authenticate before dispatching. Auth failure returns an error response
    // directly — we never reveal which method was attempted.
    if let Err(auth_err) = auth::validate_request(&req, &ctx.active_token) {
        return auth_err;
    }

    match req.method.as_str() {
        methods::DAEMON_HEALTH => handle_health(req.id, ctx),
        methods::DAEMON_SHUTDOWN => handle_shutdown(req.id, ctx),
        methods::CLI_DISPATCH => handle_dispatch(req, ctx).await,
        _ => DaemonResponse::error(req.id, METHOD_NOT_FOUND_CODE, "method not found"),
    }
}

/// `daemon/health` — liveness probe.
///
/// Returns the daemon version and uptime in seconds. Designed to complete in
/// <5ms (per `.claude/rules/daemon.md` "Watchdog" section). No I/O, no locks,
/// no allocations beyond the JSON serialization.
fn handle_health(id: u64, ctx: &DaemonContext) -> DaemonResponse {
    let uptime = ctx.start_time.elapsed().as_secs();
    DaemonResponse::success(
        id,
        json!({
            "version": ctx.version,
            "uptime_seconds": uptime,
        }),
    )
}

/// `daemon/shutdown` — request orderly shutdown.
///
/// Sets the shutdown flag and returns immediately. The actual shutdown
/// sequence (drain in-flight RPCs, flush manifests, close providers, remove
/// socket) is driven by the lifecycle event loop in T21 — this handler only
/// signals intent.
fn handle_shutdown(id: u64, ctx: &DaemonContext) -> DaemonResponse {
    ctx.shutdown_requested.store(true, Ordering::Relaxed);
    DaemonResponse::success(id, json!({"shutting_down": true}))
}

/// `cli/dispatch` — execute a `CommandRequest` in the daemon.
///
/// Deserializes `CommandRequest` from `req.params`, dispatches to the
/// appropriate handler via [`handlers::dispatch`], and wraps the result
/// into a `DaemonResponse`.
///
/// Phase 0: handlers are stubs that return placeholder responses. The
/// streaming path (chunks/events via notifications on the connection) is
/// wired in T21 when the connection handler is built. For now, a `NullSink`
/// is used — streaming output is discarded.
///
/// T21+ will replace the `NullSink` with a socket-backed sink that relays
/// `cli/stream-chunk` and `cli/stream-event` notifications to the CLI.
async fn handle_dispatch(req: DaemonRequest, ctx: &DaemonContext) -> DaemonResponse {
    // Parse CommandRequest from the request params.
    let command: CommandRequest = match serde_json::from_value(req.params.clone()) {
        Ok(cmd) => cmd,
        Err(e) => {
            return DaemonResponse::error(
                req.id,
                INVALID_PARAMS_CODE,
                format!("failed to parse CommandRequest: {e}"),
            );
        }
    };

    // Dispatch to the handler. NullSink is temporary — T21 wires a real sink.
    match handlers::dispatch(command, ctx, &NullSink).await {
        Ok(response) => {
            DaemonResponse::success(req.id, serde_json::to_value(response).unwrap_or_default())
        }
        Err(e) => DaemonResponse::error(req.id, INTERNAL_ERROR_CODE, format!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a DaemonContext with a known token.
    fn test_ctx(token: &str) -> DaemonContext {
        DaemonContext::new_for_test(token)
    }

    /// Helper: create a DaemonRequest with the given method and token.
    fn test_req(id: u64, method: &str, token: &str) -> DaemonRequest {
        DaemonRequest::new(id, method, token, json!({}))
    }

    // ── daemon/health ──────────────────────────────────────────────────

    #[tokio::test]
    async fn health_returns_version_and_uptime() {
        let ctx = test_ctx("valid-token");
        let req = test_req(1, methods::DAEMON_HEALTH, "valid-token");

        let resp = route(req, &ctx).await;
        assert_eq!(resp.id, 1);
        assert!(resp.error.is_none(), "health should succeed");

        let result = resp.result.expect("should have result");
        assert_eq!(result["version"], "0.1.0-test");
        // Uptime should be a non-negative integer (we just started).
        assert!(
            result["uptime_seconds"].as_u64().is_some(),
            "uptime_seconds should be a number"
        );
    }

    // ── daemon/shutdown ────────────────────────────────────────────────

    #[tokio::test]
    async fn shutdown_sets_flag_and_returns_success() {
        let ctx = test_ctx("valid-token");
        assert!(!ctx.is_shutdown_requested(), "should start false");

        let req = test_req(2, methods::DAEMON_SHUTDOWN, "valid-token");
        let resp = route(req, &ctx).await;

        assert_eq!(resp.id, 2);
        assert!(resp.error.is_none(), "shutdown should succeed");

        let result = resp.result.expect("should have result");
        assert_eq!(result["shutting_down"], true);
        assert!(
            ctx.is_shutdown_requested(),
            "shutdown flag should be set after handler"
        );
    }

    // ── cli/dispatch ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_routes_valid_command_request() {
        let ctx = test_ctx("valid-token");
        // Send a valid Init command as params.
        let req = DaemonRequest::new(
            3,
            methods::CLI_DISPATCH,
            "valid-token",
            serde_json::json!({"command": "init", "force": false, "json": false}),
        );

        let resp = route(req, &ctx).await;
        assert_eq!(resp.id, 3);
        assert!(resp.error.is_none(), "valid dispatch should succeed");

        let result = resp.result.expect("should have result");
        // The stub handler returns a Text response.
        assert_eq!(result["type"], "text");
        assert!(result["content"].as_str().unwrap().contains("stub"));
    }

    #[tokio::test]
    async fn dispatch_rejects_malformed_params() {
        let ctx = test_ctx("valid-token");
        // Send invalid params — missing required fields.
        let req = DaemonRequest::new(
            3,
            methods::CLI_DISPATCH,
            "valid-token",
            serde_json::json!({"not_a_command": true}),
        );

        let resp = route(req, &ctx).await;
        assert_eq!(resp.id, 3);

        let err = resp.error.expect("bad params should return error");
        assert_eq!(err.code, INVALID_PARAMS_CODE);
        assert!(
            err.message.contains("failed to parse"),
            "should indicate parse failure, got: {}",
            err.message
        );
    }

    // ── Unknown method ─────────────────────────────────────────────────

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let ctx = test_ctx("valid-token");
        let req = test_req(4, "bogus/method", "valid-token");

        let resp = route(req, &ctx).await;
        assert_eq!(resp.id, 4);

        let err = resp.error.expect("unknown method should return error");
        assert_eq!(err.code, METHOD_NOT_FOUND_CODE);
        assert!(
            err.message.contains("method not found"),
            "error should say method not found, got: {}",
            err.message
        );
    }

    // ── Auth rejection ─────────────────────────────────────────────────

    #[tokio::test]
    async fn auth_failure_rejects_before_dispatch() {
        let ctx = test_ctx("correct-token");
        let req = test_req(5, methods::DAEMON_HEALTH, "wrong-token");

        let resp = route(req, &ctx).await;
        assert_eq!(resp.id, 5);

        let err = resp.error.expect("bad auth should return error");
        assert_eq!(err.code, -32002, "should use AUTH_FAILED code");
        assert!(
            err.message.contains("authentication failed"),
            "should say auth failed, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn auth_failure_does_not_reveal_method() {
        // Even for a valid method, a bad token should return auth error,
        // not "method not found" or method-specific results.
        let ctx = test_ctx("correct-token");
        let req = test_req(6, methods::DAEMON_SHUTDOWN, "bad-token");

        let resp = route(req, &ctx).await;
        let err = resp.error.expect("bad auth should return error");
        assert_eq!(err.code, -32002);
        // Crucially: the shutdown flag should NOT be set.
        assert!(
            !ctx.is_shutdown_requested(),
            "unauthenticated shutdown should not set the flag"
        );
    }

    // ── DaemonContext construction ──────────────────────────────────────

    #[test]
    fn context_new_uses_cargo_version() {
        let ctx = DaemonContext::new("token".to_string());
        // env!("CARGO_PKG_VERSION") is resolved at compile time from Cargo.toml.
        assert!(!ctx.version.is_empty(), "version should not be empty");
        assert!(
            !ctx.is_shutdown_requested(),
            "fresh context should not be shutdown"
        );
    }
}
