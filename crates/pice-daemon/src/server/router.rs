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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use pice_core::cli::CommandRequest;
use pice_core::config::PiceConfig;
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use serde_json::json;
use tokio::sync::Mutex as TokioMutex;

use super::auth;
use crate::handlers;
use crate::orchestrator::NullSink;

/// Phase 4.1 Pass-6 Codex High #2: per-manifest single-writer lock map.
///
/// Keyed by `(project_hash, feature_id)` so two `pice evaluate` calls on
/// DIFFERENT features can still run concurrently, while two calls on the
/// SAME feature serialize. The inner lock is a `tokio::sync::Mutex` so it
/// can be held across `.await` points for the duration of the evaluation.
/// The outer `StdMutex<HashMap<..>>` is held only for the brief
/// insert-or-get operation — it never crosses an await point.
///
/// `Arc<TokioMutex<()>>` rather than `TokioMutex<()>` directly so a clone
/// of the Arc can live in both the map (for future acquirers) and the
/// current holder (so it stays alive for the evaluation's lifetime).
pub type ManifestLockMap = Arc<StdMutex<HashMap<(String, String), Arc<TokioMutex<()>>>>>;

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

    /// The project root directory. Handlers use this to find `.claude/plans/`,
    /// `.pice/config.toml`, the metrics DB, and other project-relative paths.
    project_root: PathBuf,

    /// Parsed `.pice/config.toml`. Falls back to `PiceConfig::default()` when
    /// the config file doesn't exist (uninitialized project).
    config: PiceConfig,

    /// Phase 4.1 Pass-6 Codex High #2: single-writer-per-manifest lock map.
    /// See [`ManifestLockMap`] for the keying scheme. Shared across all
    /// handler invocations in the daemon process — two concurrent
    /// `pice evaluate` calls on the same `{project_hash, feature_id}` pair
    /// serialize on the inner mutex, preventing the atomic-rename race at
    /// `VerificationManifest::save()` + `~/.pice/state/.../manifest.json`.
    manifest_locks: ManifestLockMap,
}

impl DaemonContext {
    /// Construct a new context. Called once during daemon startup.
    ///
    /// `token` is the hex-encoded bearer token from [`auth::generate_token`].
    /// `project_root` is the working directory the daemon serves.
    pub fn new(token: String, project_root: PathBuf) -> Self {
        let config = load_config(&project_root);
        Self {
            active_token: token,
            version: env!("CARGO_PKG_VERSION"),
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
            project_root,
            config,
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Construct a minimal context for inline mode (no socket, no auth).
    ///
    /// Used by `PICE_DAEMON_INLINE=1` and integration tests. Skips: socket
    /// setup, auth token generation, stale-cleanup, watchdog. The token is
    /// set to an empty string since inline mode never validates auth.
    /// Uses the process's current working directory as project root.
    pub fn inline() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let config = load_config(&project_root);
        Self {
            active_token: String::new(),
            version: env!("CARGO_PKG_VERSION"),
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
            project_root,
            config,
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// The project root directory.
    pub fn project_root(&self) -> &PathBuf {
        &self.project_root
    }

    /// The parsed PICE config.
    pub fn config(&self) -> &PiceConfig {
        &self.config
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
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            config: PiceConfig::default(),
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Test-only constructor with a custom project root.
    ///
    /// Loads config from the project root's `.pice/config.toml` if present,
    /// otherwise uses defaults.
    #[cfg(test)]
    pub(crate) fn new_for_test_with_root(token: &str, project_root: PathBuf) -> Self {
        let config = load_config(&project_root);
        Self {
            active_token: token.to_string(),
            version: "0.1.0-test",
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
            project_root,
            config,
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Phase 4.1 Pass-6: acquire the per-manifest single-writer lock for the
    /// given `{project_hash, feature_id}` pair. Returns a clone of the
    /// `Arc<tokio::sync::Mutex<()>>` so the caller can `.lock().await` to
    /// serialize the full evaluation. Different features return distinct
    /// mutex Arcs; repeat calls for the SAME feature return the SAME Arc,
    /// guaranteeing only one evaluation per manifest runs at a time.
    ///
    /// The outer `StdMutex<HashMap>` is held only for the brief
    /// insert-or-get — it NEVER crosses an await point (caller drops this
    /// function's scope before awaiting on the inner mutex).
    ///
    /// Recovers from a poisoned outer mutex by taking the inner map — the
    /// map state itself is still consistent; poisoning is an artifact of a
    /// panic in an unrelated code path.
    pub fn manifest_lock_for(&self, project_hash: &str, feature_id: &str) -> Arc<TokioMutex<()>> {
        let mut map = self
            .manifest_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = (project_hash.to_string(), feature_id.to_string());
        map.entry(key)
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    }
}

/// Load config from `.pice/config.toml`, falling back to defaults.
fn load_config(project_root: &std::path::Path) -> PiceConfig {
    let config_path = project_root.join(".pice/config.toml");
    PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default())
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

    // ── Phase 4.1 Pass-6 per-manifest lock map ─────────────────────────────

    /// Same `{project_hash, feature_id}` must resolve to the SAME
    /// `Arc<Mutex<()>>` (pointer equality). Without this identity, two
    /// concurrent runs on the same feature would hold distinct mutexes
    /// and serialize on nothing — the C17 race reopens.
    #[test]
    fn manifest_lock_for_is_shared_per_feature() {
        let ctx = test_ctx("t");
        let a = ctx.manifest_lock_for("abc123", "feat-x");
        let b = ctx.manifest_lock_for("abc123", "feat-x");
        assert!(
            Arc::ptr_eq(&a, &b),
            "same (project_hash, feature_id) must share one mutex Arc",
        );
    }

    /// Distinct feature ids must resolve to DISTINCT mutex Arcs — otherwise
    /// different features would serialize on each other, eliminating the
    /// intended cross-feature parallelism.
    #[test]
    fn manifest_lock_for_different_features_are_distinct() {
        let ctx = test_ctx("t");
        let a = ctx.manifest_lock_for("abc123", "feat-a");
        let b = ctx.manifest_lock_for("abc123", "feat-b");
        assert!(
            !Arc::ptr_eq(&a, &b),
            "different features must get distinct mutexes",
        );
    }

    /// Distinct project hashes must resolve to DISTINCT mutex Arcs — two
    /// repos that happen to use the same feature name must not serialize
    /// against each other.
    #[test]
    fn manifest_lock_for_different_projects_are_distinct() {
        let ctx = test_ctx("t");
        let a = ctx.manifest_lock_for("project-a", "feat-x");
        let b = ctx.manifest_lock_for("project-b", "feat-x");
        assert!(
            !Arc::ptr_eq(&a, &b),
            "different project hashes must get distinct mutexes",
        );
    }

    /// The acquired mutex actually serializes holders. Spawns two tasks on
    /// the SAME key; task A holds the lock across a short sleep, task B
    /// tries to acquire. Assert that B's acquire completes AFTER A releases
    /// (observable via order of timestamps). Without the shared mutex Arc
    /// (the previous two tests) or without the mutex's `.lock().await`
    /// semantics, B would proceed concurrently and the test's ordering
    /// assertion would fail.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manifest_lock_serializes_concurrent_holders_on_same_key() {
        let ctx = Arc::new(test_ctx("t"));
        let lock_a = ctx.manifest_lock_for("proj", "feat");
        let lock_b = ctx.manifest_lock_for("proj", "feat");

        // Sanity: same Arc.
        assert!(Arc::ptr_eq(&lock_a, &lock_b));

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<std::time::Instant>();
        let started = Arc::new(tokio::sync::Notify::new());

        // Task A: acquire first, signal started, hold for 50ms, release.
        let started_clone = started.clone();
        let task_a = tokio::spawn(async move {
            let _g = lock_a.lock().await;
            started_clone.notify_one();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let released = std::time::Instant::now();
            let _ = done_tx.send(released);
        });

        // Wait until A is holding the lock before starting B — otherwise
        // B might race A and acquire first on the thread-pool scheduler.
        started.notified().await;

        // Task B: acquire second, timestamp the successful acquire.
        let task_b = tokio::spawn(async move {
            let _g = lock_b.lock().await;
            std::time::Instant::now()
        });

        let a_released = done_rx.await.unwrap();
        let b_acquired = task_b.await.unwrap();
        task_a.await.unwrap();

        assert!(
            b_acquired >= a_released,
            "B must acquire AFTER A releases (b_acquired={:?}, a_released={:?})",
            b_acquired,
            a_released,
        );
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
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("valid-token", dir.path().to_path_buf());
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
        // The init handler returns a Text response with initialization output.
        assert_eq!(result["type"], "text");
        assert!(
            result["content"]
                .as_str()
                .unwrap()
                .contains("PICE initialized"),
            "init should report success"
        );
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
        let ctx = DaemonContext::new("token".to_string(), PathBuf::from("."));
        // env!("CARGO_PKG_VERSION") is resolved at compile time from Cargo.toml.
        assert!(!ctx.version.is_empty(), "version should not be empty");
        assert!(
            !ctx.is_shutdown_requested(),
            "fresh context should not be shutdown"
        );
    }
}
