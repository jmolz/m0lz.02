//! Daemon RPC router — method dispatch table and shared daemon context.
//!
//! The router sits between authentication ([`super::auth`]) and the per-command
//! handlers (`crate::handlers::*`). It receives an already-framed
//! [`pice_core::protocol::DaemonRequest`], validates the bearer token, then dispatches to the
//! appropriate method handler.
//!
//! ## Phase 0 method surface
//!
//! | Method | Handler | Purpose |
//! |--------|---------|---------|
//! | `daemon/health` | `handle_health` | Liveness probe + version |
//! | `daemon/shutdown` | `handle_shutdown` | Orderly shutdown request |
//! | `cli/dispatch` | `handle_dispatch` | Execute a `CommandRequest` (T19 stub) |
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
use std::time::{Duration, Instant};

use pice_core::cli::CommandRequest;
use pice_core::config::PiceConfig;
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use serde_json::json;
use tokio::sync::{oneshot, Mutex as TokioMutex, Notify};

use super::auth;
use crate::events::EventBus;
use crate::handlers;
use crate::jobs::FeatureJobManager;
use crate::logs::LogStore;
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
type BackgroundAdmissionLockMap = Arc<StdMutex<HashMap<(String, String), Arc<TokioMutex<()>>>>>;
type DeferredBackgroundStartMap = Arc<StdMutex<HashMap<(String, String), oneshot::Sender<()>>>>;

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

    /// Set by the connection task after it attempts to write the
    /// `daemon/shutdown` response. The lifecycle accept loop observes
    /// [`shutdown_requested`] before that write can happen, so it must wait on
    /// this signal before returning from `lifecycle::run`.
    shutdown_response_observed: AtomicBool,
    shutdown_response_notify: Notify,

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

    /// Short-lived background admission locks keyed by `(project_hash,
    /// feature_id)`.
    ///
    /// This lock is deliberately separate from [`manifest_locks`]:
    /// background workers hold the manifest lock until completion, while
    /// admission only needs to serialize the brief check → Queued write →
    /// `FeatureJobManager` insertion window.
    background_admission_locks: BackgroundAdmissionLockMap,

    /// Background dispatch start gates keyed by `(feature_id, run_id)`.
    ///
    /// `dispatch_background` registers a spawned worker here after it writes
    /// the Queued manifest. The connection handler releases the gate only
    /// after it has attempted to write the background-dispatched RPC response,
    /// preserving the hard ordering that dispatch returns before provider
    /// work can begin.
    deferred_background_starts: DeferredBackgroundStartMap,

    /// Phase 7 Task 4: manifest-event pub/sub bus. The orchestrator
    /// publishes `ManifestEvent`s via the typed `emit_*` helpers; the
    /// `manifest/subscribe` router handler (Task 6) acquires receivers
    /// and forwards payloads as `manifest/event` notifications. Clone-
    /// cheap (`Arc`-backed) so handler borrows are free.
    events: EventBus,

    /// Phase 7 Task 5: captured-provider-session log store. Orchestrator
    /// writes chunks via `append_chunk` (Task 9 wires this); the
    /// `logs/stream` router handler (Task 6) reads via `snapshot` +
    /// `subscribe`.
    logs: LogStore,

    /// Phase 7 Task 7/10: detached-task tracker for background evaluate /
    /// execute dispatches. Shared across every handler invocation so a
    /// `pice evaluate --background` returning within the dispatch-SLO
    /// leaves a running future the `manifest/subscribe` handler can
    /// observe and `pice status --wait` can synchronize against. The
    /// manager clamps the global provider-concurrency cap to
    /// [`pice_core::workflow::schema::MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP`].
    jobs: FeatureJobManager,
}

impl DaemonContext {
    /// Construct a new context. Called once during daemon startup.
    ///
    /// `token` is the hex-encoded bearer token from [`auth::generate_token`].
    /// `project_root` is the working directory the daemon serves.
    pub fn new(token: String, project_root: PathBuf) -> Self {
        let config = load_config(&project_root);
        let events = EventBus::new();
        let jobs = FeatureJobManager::new(
            events.clone(),
            resolve_global_provider_concurrency(&project_root),
        );
        Self {
            active_token: token,
            version: env!("CARGO_PKG_VERSION"),
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
            shutdown_response_observed: AtomicBool::new(false),
            shutdown_response_notify: Notify::new(),
            project_root,
            config,
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
            background_admission_locks: Arc::new(StdMutex::new(HashMap::new())),
            deferred_background_starts: Arc::new(StdMutex::new(HashMap::new())),
            events,
            logs: LogStore::new(),
            jobs,
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
        let events = EventBus::new();
        let jobs = FeatureJobManager::new(
            events.clone(),
            resolve_global_provider_concurrency(&project_root),
        );
        Self {
            active_token: String::new(),
            version: env!("CARGO_PKG_VERSION"),
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
            shutdown_response_observed: AtomicBool::new(false),
            shutdown_response_notify: Notify::new(),
            project_root,
            config,
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
            background_admission_locks: Arc::new(StdMutex::new(HashMap::new())),
            deferred_background_starts: Arc::new(StdMutex::new(HashMap::new())),
            events,
            logs: LogStore::new(),
            jobs,
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

    /// Request daemon shutdown. The accept loop will stop accepting new
    /// connections, but must not let `lifecycle::run` return until the
    /// connection task has attempted to write the shutdown RPC response.
    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
    }

    /// Mark the shutdown RPC response as written or attempted.
    pub fn mark_shutdown_response_observed(&self) {
        if !self
            .shutdown_response_observed
            .swap(true, Ordering::Relaxed)
        {
            self.shutdown_response_notify.notify_waiters();
        }
    }

    /// Wait until the shutdown connection task has attempted its response.
    pub async fn wait_for_shutdown_response_observed(&self, timeout: Duration) -> bool {
        let notified = self.shutdown_response_notify.notified();
        if self.shutdown_response_observed.load(Ordering::Relaxed) {
            return true;
        }
        tokio::time::timeout(timeout, notified).await.is_ok()
            || self.shutdown_response_observed.load(Ordering::Relaxed)
    }

    /// Phase 7: the process-wide manifest-event pub/sub bus.
    /// Producers go through the typed `emit_*` helpers on
    /// [`EventBus`]; subscribers (the `manifest/subscribe` router
    /// handler) acquire receivers via `subscribe_feature` /
    /// `subscribe_wildcard`.
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// Phase 7: the captured-provider-session log store. Orchestrator
    /// chunks flow in via `append_chunk` (Task 9 wires the forwarding);
    /// the `logs/stream` router handler reads via `snapshot` /
    /// `subscribe`.
    pub fn logs(&self) -> &LogStore {
        &self.logs
    }

    /// Phase 7: the detached-task tracker for background dispatches.
    /// Task 10's dispatch helper uses this to enforce
    /// one-feature-at-a-time (`run_id_for`) and to spawn the
    /// orchestrator future on the daemon runtime.
    pub fn jobs(&self) -> &FeatureJobManager {
        &self.jobs
    }

    /// Validate a request's `auth` token against the active daemon
    /// token. Returns `Ok(())` on match; on mismatch returns an
    /// already-formatted `DaemonResponse` error (code `-32002`) that
    /// the caller writes back to the connection.
    ///
    /// Used by the Phase 7 subscribe handlers, which branch on the
    /// method name BEFORE entering the normal [`route`] path and
    /// therefore must authenticate independently.
    #[allow(clippy::result_large_err)] // DaemonResponse is returned unboxed so the caller can send it directly.
    pub fn validate_auth(
        &self,
        req: &pice_core::protocol::DaemonRequest,
    ) -> Result<(), pice_core::protocol::DaemonResponse> {
        auth::validate_request(req, &self.active_token)
    }

    /// Test-only constructor with a custom version string.
    ///
    /// Uses a fixed version instead of `env!("CARGO_PKG_VERSION")` so tests
    /// can assert on a known value without depending on Cargo.toml.
    #[cfg(test)]
    pub(crate) fn new_for_test(token: &str) -> Self {
        let events = EventBus::new();
        let jobs = FeatureJobManager::new(events.clone(), 4);
        Self {
            active_token: token.to_string(),
            version: "0.1.0-test",
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
            shutdown_response_observed: AtomicBool::new(false),
            shutdown_response_notify: Notify::new(),
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            config: PiceConfig::default(),
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
            background_admission_locks: Arc::new(StdMutex::new(HashMap::new())),
            deferred_background_starts: Arc::new(StdMutex::new(HashMap::new())),
            events,
            logs: LogStore::new(),
            jobs,
        }
    }

    /// Test-only constructor with a custom project root.
    ///
    /// Loads config from the project root's `.pice/config.toml` if present,
    /// otherwise uses defaults.
    #[cfg(test)]
    pub(crate) fn new_for_test_with_root(token: &str, project_root: PathBuf) -> Self {
        let config = load_config(&project_root);
        let events = EventBus::new();
        let jobs = FeatureJobManager::new(events.clone(), 4);
        Self {
            active_token: token.to_string(),
            version: "0.1.0-test",
            start_time: Instant::now(),
            shutdown_requested: AtomicBool::new(false),
            shutdown_response_observed: AtomicBool::new(false),
            shutdown_response_notify: Notify::new(),
            project_root,
            config,
            manifest_locks: Arc::new(StdMutex::new(HashMap::new())),
            background_admission_locks: Arc::new(StdMutex::new(HashMap::new())),
            deferred_background_starts: Arc::new(StdMutex::new(HashMap::new())),
            events,
            logs: LogStore::new(),
            jobs,
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

    /// Acquire the short-lived admission mutex for a background feature.
    ///
    /// Callers hold this only until a Queued manifest has been persisted and
    /// the corresponding `FeatureJobManager` entry has been inserted. It
    /// prevents a duplicate caller from observing the pre-Queued admission
    /// gap without forcing the duplicate to wait for the entire background
    /// run's long-held manifest lock.
    pub fn background_admission_lock_for(
        &self,
        project_hash: &str,
        feature_id: &str,
    ) -> Arc<TokioMutex<()>> {
        let mut map = self
            .background_admission_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = (project_hash.to_string(), feature_id.to_string());
        map.entry(key)
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    }

    /// Register a background worker start gate that must be released only
    /// after the daemon has attempted to write the dispatch response.
    pub fn defer_background_start(
        &self,
        feature_id: &str,
        run_id: &str,
        start_tx: oneshot::Sender<()>,
    ) {
        let mut map = self
            .deferred_background_starts
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        map.insert((feature_id.to_string(), run_id.to_string()), start_tx);
    }

    /// Release a deferred background worker start gate.
    ///
    /// Returns `true` when a matching gate was found. Sending may still fail
    /// if the worker exited before release; that is not actionable here.
    pub fn release_background_start(&self, feature_id: &str, run_id: &str) -> bool {
        let tx = {
            let mut map = self
                .deferred_background_starts
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            map.remove(&(feature_id.to_string(), run_id.to_string()))
        };
        let Some(tx) = tx else {
            return false;
        };
        let _ = tx.send(());
        true
    }

    /// If `resp` is a background-dispatched command response, release the
    /// corresponding worker start gate. Called by connection handlers after
    /// the response write completes or fails; on write failure there is no
    /// caller left to observe the response, but the detached job must still
    /// outlive the originating RPC connection.
    pub fn release_background_start_from_response(&self, resp: &DaemonResponse) -> bool {
        let Some(result) = resp.result.as_ref() else {
            return false;
        };
        if result.get("type").and_then(|v| v.as_str()) != Some("json") {
            return false;
        }
        let Some(value) = result.get("value") else {
            return false;
        };
        if value.get("status").and_then(|v| v.as_str()) != Some("background-dispatched") {
            return false;
        }
        let Some(feature_id) = value.get("feature_id").and_then(|v| v.as_str()) else {
            return false;
        };
        let Some(run_id) = value.get("run_id").and_then(|v| v.as_str()) else {
            return false;
        };
        self.release_background_start(feature_id, run_id)
    }

    #[cfg(test)]
    pub(crate) fn deferred_background_start_count(&self) -> usize {
        self.deferred_background_starts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }
}

/// Load config from `.pice/config.toml`, falling back to defaults.
fn load_config(project_root: &std::path::Path) -> PiceConfig {
    let config_path = project_root.join(".pice/config.toml");
    PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default())
}

/// Resolve the global-provider-concurrency cap for this daemon's
/// [`FeatureJobManager`].
///
/// Precedence:
/// 1. `workflow.yaml defaults.max_global_provider_concurrency` (if
///    present)
/// 2. `workflow.yaml defaults.max_parallelism` (if present) — matches
///    v0.6 single-feature behavior
/// 3. `num_cpus::get()` — parity with per-feature cohort default
///
/// The final value is clamped by [`FeatureJobManager::new`] to the
/// hard cap. Returns `u32` so clamping math stays type-consistent with
/// the schema field.
///
/// Non-existent / malformed `.pice/workflow.yaml` silently falls
/// through to `num_cpus` — the daemon must start without a workflow
/// config (uninitialized project), and a broken workflow file will be
/// surfaced separately at evaluate-dispatch time.
fn resolve_global_provider_concurrency(project_root: &std::path::Path) -> u32 {
    let default = num_cpus::get().max(1) as u32;
    match pice_core::workflow::loader::resolve(project_root) {
        Ok(wf) => wf
            .defaults
            .max_global_provider_concurrency
            .or(wf.defaults.max_parallelism)
            .unwrap_or(default),
        Err(_) => default,
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
        methods::DAEMON_SHUTDOWN => handle_shutdown(req.id, ctx).await,
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
/// Phase 7 Criterion 17 contract: the response is emitted AFTER
/// [`FeatureJobManager::drain_on_shutdown`] returns, so any in-flight
/// background-dispatch job has a chance to observe its cancel token and
/// flush its final manifest save BEFORE the socket closes. Ordering:
///
/// 1. Set `shutdown_requested` (so the accept loop stops accepting new
///    connections on its 100ms poll).
/// 2. `await ctx.jobs().drain_on_shutdown(SHUTDOWN_TIMEOUT)` — fires
///    every feature's `CancellationToken` and waits up to 10s for the
///    supervisor tasks to exit.
/// 3. Emit the success response only if all jobs drained and any forced
///    terminalization saves succeeded.
///
/// The `drained_remaining` field reports how many jobs were still live
/// when the 10s budget elapsed; nonzero remaining jobs make shutdown
/// fail closed. The flag-
/// based poll in `lifecycle::run_unix` also calls drain for the
/// SIGTERM path (no caller waiting there), keeping the two entry
/// points symmetric.
async fn handle_shutdown(id: u64, ctx: &DaemonContext) -> DaemonResponse {
    ctx.request_shutdown();
    let remaining = ctx
        .jobs()
        .drain_on_shutdown(crate::lifecycle::SHUTDOWN_TIMEOUT)
        .await;
    if !remaining.is_clean() {
        return DaemonResponse::error(
            id,
            INTERNAL_ERROR_CODE,
            format!(
                "shutdown failed to drain {} background job(s); terminalization failures: {}",
                remaining.remaining,
                remaining.terminalization_failures.len()
            ),
        );
    }
    DaemonResponse::success(
        id,
        json!({
            "shutting_down": true,
            "drained_remaining": remaining.remaining,
        }),
    )
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
        // Phase 7 Criterion 17: drained_remaining is reported. With no
        // background jobs, drain returns 0 immediately.
        assert_eq!(
            result["drained_remaining"], 0,
            "idle context should drain with zero remaining jobs"
        );
        assert!(
            ctx.is_shutdown_requested(),
            "shutdown flag should be set after handler"
        );
    }

    /// Phase 7 Criterion 17: the shutdown handler MUST await
    /// `drain_on_shutdown` BEFORE emitting its response. This test
    /// spawns a supervised background job that holds across a short
    /// sleep after its cancel token fires; asserts the shutdown
    /// response lands AFTER the job exits (not before), and that the
    /// final `drained_remaining == 0`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_awaits_drain_before_returning() {
        use pice_core::jobs::JobEnv;
        use pice_core::layers::manifest::VerificationManifest;
        use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
        use std::path::PathBuf;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc as StdArc;

        let ctx = test_ctx("valid-token");
        let env = StdArc::new(JobEnv {
            state_dir: PathBuf::from("/tmp/state"),
            project_root: PathBuf::from("/tmp/project"),
            workflow_snapshot: WorkflowConfig {
                schema_version: "0.2".into(),
                defaults: Defaults {
                    tier: 2,
                    min_confidence: 0.90,
                    max_passes: 5,
                    model: "sonnet".into(),
                    budget_usd: 2.0,
                    cost_cap_behavior: CostCapBehavior::Halt,
                    max_parallelism: None,
                    max_global_provider_concurrency: None,
                },
                phases: Phases::default(),
                layer_overrides: Default::default(),
                review: None,
                seams: None,
            },
            contracts: Default::default(),
            pice_state_dir_override: None,
            pice_user_workflow_file: None,
            plan_trace: None,
        });

        let job_exited = StdArc::new(AtomicBool::new(false));
        let job_exited_c = job_exited.clone();
        let job_started = StdArc::new(tokio::sync::Notify::new());
        let job_started_c = job_started.clone();

        ctx.jobs()
            .spawn(
                "feat-drain",
                ctx.jobs().next_run_id(),
                env,
                move |_env, permit, cancel| async move {
                    let _hold = permit;
                    job_started_c.notify_one();
                    cancel.cancelled().await;
                    // Simulate a manifest-save flush after the cancel.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    job_exited_c.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(VerificationManifest::new(
                        "feat-drain",
                        std::path::Path::new("/irrelevant"),
                    ))
                },
            )
            .expect("spawn should succeed");

        assert_eq!(ctx.jobs().active_count(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(1), job_started.notified())
            .await
            .expect("job should enter closure before shutdown");

        // Issue daemon/shutdown. It must block until the job exits.
        let req = test_req(99, methods::DAEMON_SHUTDOWN, "valid-token");
        let before = std::time::Instant::now();
        let resp = route(req, &ctx).await;
        let elapsed = before.elapsed();

        assert!(resp.error.is_none());
        let result = resp.result.expect("result");
        assert_eq!(result["drained_remaining"], 0);
        assert!(
            job_exited.load(std::sync::atomic::Ordering::SeqCst),
            "job must have exited BEFORE the shutdown response returned",
        );
        assert!(
            elapsed >= std::time::Duration::from_millis(80),
            "shutdown response must wait for the job's cancel + flush (elapsed={:?})",
            elapsed,
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
