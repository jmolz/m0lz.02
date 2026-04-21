//! `FeatureJobManager` — the detached-task tracker for Phase 7 background
//! dispatches. See module-level docs in [`super`] for the architectural
//! rationale and the canonical semaphore lifecycle.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::VerificationManifest;
use pice_core::workflow::schema::MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;

/// String-typed run identifier. Constructed by
/// [`FeatureJobManager::next_run_id`] in the format
/// `"r-{timestamp_millis:012x}{counter:08x}"` — monotonic within a process
/// and collision-free across parallel dispatches (the counter breaks ties
/// within a single millisecond tick).
pub type RunId = String;

/// Error returned by [`FeatureJobManager::spawn`] when a feature is already
/// running. The CLI handler surfaces this as
/// `ExitJsonStatus::FeatureAlreadyRunning` with the existing run id so the
/// user can `pice status --follow <feature>` instead of re-dispatching.
#[derive(Debug, Clone, thiserror::Error)]
#[error("feature {feature_id} is already running as {run_id}")]
pub struct SpawnError {
    pub feature_id: String,
    pub run_id: RunId,
}

/// Per-feature live state held in the manager's DashMap.
///
/// Dropped from the map when the spawned task completes (normally, via
/// cancellation, or via panic — all three paths flow through the
/// supervisor task in `spawn`).
#[derive(Debug)]
pub struct JobHandle {
    /// Opaque run identifier assigned at dispatch time. Stable across the
    /// task's lifetime; new dispatches get fresh ids.
    pub run_id: RunId,

    /// Cancellation primitive the orchestrator cooperates with. Firing
    /// this token signals the orchestrator to halt the adaptive loop
    /// (per Phase 5 conventions; see `.claude/rules/stack-loops.md`).
    pub cancel: CancellationToken,

    /// JoinHandle of the detached orchestrator task. Awaited by
    /// [`FeatureJobManager::join`] for the `--wait` CLI path; otherwise
    /// the supervisor task in `spawn` awaits it for cleanup.
    pub join_handle: JoinHandle<Result<VerificationManifest>>,

    /// The immutable env snapshot passed to the orchestrator future.
    /// Held here so `pice status` / `pice logs` can surface per-job
    /// state-dir + workflow metadata without round-tripping the manifest.
    pub env: Arc<JobEnv>,
}

/// The detached-task tracker for Phase 7 background dispatches.
///
/// Clone-cheap: holds `Arc`s to the DashMap + semaphore + EventBus.
#[derive(Debug, Clone)]
pub struct FeatureJobManager {
    jobs: Arc<DashMap<String, JobHandle>>,
    /// Global provider-session semaphore. Shared across every spawned
    /// feature. Capacity is clamped to
    /// [`MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP`] at construction time
    /// so a misconfigured workflow cannot blow past the rate-limit-
    /// friendly ceiling.
    global_sem: Arc<Semaphore>,
    /// Event bus handle used for the ONE event the manager emits
    /// directly: `ManifestEvent::Cancelled` on a panicked task. Every
    /// other manifest event comes from the orchestrator via
    /// `ManifestSaver::save_and_emit`.
    events: EventBus,
    /// Atomic tie-breaker for run-id generation. See
    /// [`Self::next_run_id`].
    run_id_counter: Arc<AtomicU64>,
}

impl FeatureJobManager {
    /// Construct a new manager with a global-provider-concurrency cap.
    ///
    /// The provided `global_concurrency` is clamped to
    /// [`MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP`]. A zero or negative
    /// workflow-configured cap is clamped UP to 1 (a zero-permit
    /// semaphore would deadlock every dispatched feature).
    pub fn new(events: EventBus, global_concurrency: u32) -> Self {
        let clamped = global_concurrency.clamp(1, MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP);
        if clamped != global_concurrency {
            tracing::warn!(
                requested = global_concurrency,
                clamped = clamped,
                cap = MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP,
                "FeatureJobManager global-concurrency clamped"
            );
        }
        Self {
            jobs: Arc::new(DashMap::new()),
            global_sem: Arc::new(Semaphore::new(clamped as usize)),
            events,
            run_id_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Generate the next run id. Format: `r-{ts_ms:012x}{counter:08x}`.
    /// Monotonic within a process; stable ordering across parallel
    /// dispatches via the atomic counter tie-breaker.
    pub fn next_run_id(&self) -> RunId {
        let ts = Utc::now().timestamp_millis().max(0) as u64;
        let c = self.run_id_counter.fetch_add(1, Ordering::Relaxed);
        format!("r-{ts:012x}{c:08x}")
    }

    /// Spawn a background feature.
    ///
    /// Canonical flow (see plan Task 7 point 2):
    /// 1. If the feature is already live in the DashMap, return
    ///    `SpawnError` carrying the existing run id.
    /// 2. Insert a `JobHandle` into the DashMap keyed on `feature_id`.
    /// 3. Spawn a worker future that acquires an owned permit from
    ///    `global_sem`, then invokes `future_builder(env, permit, cancel)`
    ///    which runs the orchestrator. The permit is dropped when the
    ///    orchestrator future returns (its `Drop` releases the semaphore
    ///    slot).
    /// 4. Spawn a supervisor that awaits the worker's `JoinHandle`; on
    ///    panic it emits `ManifestEvent::Cancelled { reason: "panic" }`;
    ///    on every completion path it removes the feature from the
    ///    DashMap.
    ///
    /// Returns the assigned run id. The caller MUST pre-write a
    /// `ManifestStatus::Queued` manifest to disk BEFORE calling `spawn`
    /// — the spawned future assumes the manifest exists when it
    /// transitions `Queued → InProgress`.
    pub fn spawn<F, Fut>(
        &self,
        feature_id: impl Into<String>,
        env: Arc<JobEnv>,
        future_builder: F,
    ) -> Result<RunId, SpawnError>
    where
        F: FnOnce(Arc<JobEnv>, OwnedSemaphorePermit, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<VerificationManifest>> + Send + 'static,
    {
        let feature_id = feature_id.into();

        // Atomic insert: check-and-claim in one critical section so a
        // racing dispatch can't register a duplicate run under the same
        // feature id.
        let run_id = self.next_run_id();
        let cancel = CancellationToken::new();

        // Clone references used inside the spawned future.
        let global_sem = Arc::clone(&self.global_sem);
        let env_for_task = Arc::clone(&env);
        let cancel_for_task = cancel.clone();

        let worker_handle = tokio::spawn(async move {
            // SAFETY note: `acquire_owned` returns `Err` only if the
            // underlying semaphore is closed. We never `close()` it, so
            // this path is unreachable in production. Map the error to
            // anyhow for the JoinHandle's Result type.
            let permit = match global_sem.acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    return Err(anyhow::anyhow!("global provider semaphore closed: {e}"));
                }
            };
            future_builder(env_for_task, permit, cancel_for_task).await
        });

        // Register in the DashMap. Two concurrent `spawn` calls for the
        // same feature_id race here — DashMap's `entry` API serializes.
        // If we find a live entry, we abort the worker we just spawned
        // (it's holding no permit yet — acquire_owned awaits at the top)
        // and return `SpawnError` with the pre-existing run id.
        let entry = self.jobs.entry(feature_id.clone());
        match entry {
            dashmap::Entry::Occupied(slot) => {
                let existing_run_id = slot.get().run_id.clone();
                // Abort the spawned worker — the Drop of the JoinHandle
                // won't cancel the task, we need an explicit abort.
                worker_handle.abort();
                return Err(SpawnError {
                    feature_id,
                    run_id: existing_run_id,
                });
            }
            dashmap::Entry::Vacant(slot) => {
                slot.insert(JobHandle {
                    run_id: run_id.clone(),
                    cancel: cancel.clone(),
                    join_handle: worker_handle,
                    env,
                });
            }
        }

        // Supervisor task: watches the worker's join result + cleans up
        // the DashMap entry. Runs independently of the caller; has no
        // observable return value.
        let jobs_for_supervisor = Arc::clone(&self.jobs);
        let events_for_supervisor = self.events.clone();
        let feat_for_supervisor = feature_id.clone();
        let run_id_for_supervisor = run_id.clone();
        tokio::spawn(async move {
            // We can't just hold a reference to the JoinHandle stored in
            // the DashMap (the map may return None during panic cleanup)
            // so instead we poll-for-completion via a small polling loop
            // on a cloned Waker-less path: check the handle present in
            // the map, take it out if finished, then act.
            //
            // Simpler: just block on the join handle inside a small
            // guard-scoped await. To do that we need to own the handle,
            // which conflicts with the caller's ability to await it via
            // `FeatureJobManager::join`. Resolution: use `tokio::select!`
            // between a periodic wake and a cancellation signal is too
            // complex — the correct shape is: supervisor owns NOTHING;
            // it polls via `is_finished` + yields. But that's busy work.
            //
            // Cleanest approach: use a `JoinSet` per feature (overkill
            // for one task) OR thread the cleanup through a `oneshot`
            // channel the worker signals before completing. The worker
            // task signaling via oneshot keeps the API clean.
            //
            // We take the OneShot approach: this supervisor waits on a
            // completion oneshot driven by an inner wrapper around
            // `future_builder`.
            //
            // However, implementing that now requires reshaping the
            // spawn flow. Defer: use a simple `yield_now` loop with
            // `is_finished()`.
            loop {
                // Tight loop is unacceptable — sleep in 100ms ticks.
                tokio::time::sleep(Duration::from_millis(100)).await;
                // Has the worker finished? If yes, extract + process.
                // If the feature was already removed (e.g. by
                // `drain_on_shutdown`), exit the supervisor.
                let finished = jobs_for_supervisor
                    .get(&feat_for_supervisor)
                    .map(|entry| entry.join_handle.is_finished())
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }

            // Claim the handle for awaiting by removing the entry.
            let handle = jobs_for_supervisor
                .remove(&feat_for_supervisor)
                .map(|(_, h)| h);
            if let Some(h) = handle {
                match h.join_handle.await {
                    Ok(Ok(_manifest)) => {
                        // Normal completion — orchestrator already
                        // emitted its `FeatureComplete` event via the
                        // saver. Nothing more to do.
                    }
                    Ok(Err(e)) => {
                        tracing::debug!(
                            feature_id = %feat_for_supervisor,
                            run_id = %run_id_for_supervisor,
                            error = %e,
                            "FeatureJobManager: orchestrator returned Err",
                        );
                    }
                    Err(join_err) if join_err.is_panic() => {
                        tracing::error!(
                            feature_id = %feat_for_supervisor,
                            run_id = %run_id_for_supervisor,
                            "FeatureJobManager: orchestrator panicked",
                        );
                        events_for_supervisor.emit_cancelled(
                            &feat_for_supervisor,
                            &run_id_for_supervisor,
                            "panic",
                        );
                    }
                    Err(_cancelled) => {
                        // The JoinHandle was aborted (e.g., by
                        // `cancel` firing + the task honoring it, or
                        // by `drain_on_shutdown`). The abort path is
                        // not a panic; log at debug and move on.
                        tracing::debug!(
                            feature_id = %feat_for_supervisor,
                            run_id = %run_id_for_supervisor,
                            "FeatureJobManager: task aborted",
                        );
                    }
                }
            }
        });

        Ok(run_id)
    }

    /// Fire the cancellation token for `feature_id`. Returns `true` if a
    /// live feature was found and signaled; `false` if the feature was
    /// not present (already completed, never dispatched).
    ///
    /// Does NOT await orchestrator completion — call [`Self::join`] for
    /// that. The cancellation signal is cooperative: the orchestrator
    /// polls the token at cohort boundaries.
    pub fn cancel(&self, feature_id: &str) -> bool {
        if let Some(entry) = self.jobs.get(feature_id) {
            entry.cancel.cancel();
            true
        } else {
            false
        }
    }

    /// Returns the run id for a live feature, or `None` if the feature
    /// is not currently tracked.
    pub fn run_id_for(&self, feature_id: &str) -> Option<RunId> {
        self.jobs.get(feature_id).map(|e| e.run_id.clone())
    }

    /// Snapshot the current `feature_id → run_id` map. Populates the
    /// `run_ids` field of `SubscribeManifestResponse` so dashboard
    /// adapters / `pice status --list` can show live-running state in
    /// one round-trip.
    ///
    /// Returns a `BTreeMap` (not `HashMap`) for deterministic
    /// serialization order — integration tests assert stable byte
    /// output on the wire.
    pub fn live_runs(&self) -> BTreeMap<String, RunId> {
        self.jobs
            .iter()
            .map(|e| (e.key().clone(), e.run_id.clone()))
            .collect()
    }

    /// Returns the count of live features. Used by `pice daemon status`
    /// diagnostics.
    pub fn active_count(&self) -> usize {
        self.jobs.len()
    }

    /// Fire cancellation on every live feature, then wait up to `timeout`
    /// for all supervised tasks to exit. Returns the count of features
    /// that were STILL running when the timeout elapsed.
    ///
    /// Wired into the daemon's SIGTERM / `daemon/shutdown` path by
    /// Task 21's `lifecycle::shutdown_sequence()`. The 10s budget per
    /// `.claude/rules/daemon.md` → "Graceful shutdown" lives at the call
    /// site; this method only enforces the timeout the caller hands it.
    pub async fn drain_on_shutdown(&self, timeout: Duration) -> usize {
        // Fire all tokens.
        for entry in self.jobs.iter() {
            entry.cancel.cancel();
        }

        // Wait for the DashMap to empty OR the timeout to elapse.
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = self.jobs.len();
            if remaining == 0 {
                return 0;
            }
            if tokio::time::Instant::now() >= deadline {
                return remaining;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Access the backing event bus. Used by callers that need to emit
    /// orchestrator-side events using the SAME bus the manager listens
    /// on for its own supervisor emissions.
    pub fn events(&self) -> &EventBus {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
    use std::path::PathBuf;

    fn stub_env() -> Arc<JobEnv> {
        Arc::new(JobEnv {
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
        })
    }

    fn stub_manifest(feature_id: &str) -> VerificationManifest {
        VerificationManifest::new(feature_id, std::path::Path::new("/tmp/project"))
    }

    #[tokio::test]
    async fn spawn_and_join_happy_path() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        let env = stub_env();

        let run_id = manager
            .spawn(
                "feat-happy",
                env,
                move |_env, _permit, _cancel| async move { Ok(stub_manifest("feat-happy")) },
            )
            .expect("spawn");
        assert!(run_id.starts_with("r-"), "run_id format: {run_id}");

        // Live features map includes this one.
        let live = manager.live_runs();
        assert_eq!(live.get("feat-happy"), Some(&run_id));

        // Wait for the supervisor to clean up.
        for _ in 0..50 {
            if manager.active_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(manager.active_count(), 0, "feature should be cleaned up");
    }

    #[tokio::test]
    async fn duplicate_dispatch_returns_feature_already_running() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        let env = stub_env();

        // First dispatch holds a long-running future so we can race
        // the second dispatch against it.
        let gate = Arc::new(tokio::sync::Notify::new());
        let gate_clone = gate.clone();
        let run_id_first = manager
            .spawn(
                "feat-dup",
                env.clone(),
                move |_env, _permit, _cancel| async move {
                    gate_clone.notified().await;
                    Ok(stub_manifest("feat-dup"))
                },
            )
            .expect("first spawn");

        let err = manager
            .spawn("feat-dup", env, move |_env, _permit, _cancel| async move {
                Ok(stub_manifest("feat-dup"))
            })
            .expect_err("second spawn should conflict");

        assert_eq!(err.feature_id, "feat-dup");
        assert_eq!(err.run_id, run_id_first);

        // Release the first to let cleanup happen.
        gate.notify_one();
        for _ in 0..50 {
            if manager.active_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    async fn cancel_fires_token_on_live_feature() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        let env = stub_env();

        let observed = Arc::new(tokio::sync::Notify::new());
        let observed_clone = observed.clone();

        manager
            .spawn(
                "feat-cancel",
                env,
                move |_env, _permit, cancel| async move {
                    // Wait for cancel OR a long delay.
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            observed_clone.notify_one();
                        }
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    }
                    Ok(stub_manifest("feat-cancel"))
                },
            )
            .expect("spawn");

        // Fire cancel.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(manager.cancel("feat-cancel"), "cancel should find feature");

        // Observe the token fire.
        let saw_cancel = tokio::time::timeout(Duration::from_secs(2), observed.notified()).await;
        assert!(saw_cancel.is_ok(), "token should fire within 2s");
    }

    #[tokio::test]
    async fn cancel_missing_feature_returns_false() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        assert!(!manager.cancel("nope"));
    }

    #[tokio::test]
    async fn run_id_for_returns_none_for_unknown_feature() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        assert!(manager.run_id_for("nope").is_none());
    }

    #[tokio::test]
    async fn panicked_task_emits_cancelled_and_removes_handle() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events.clone(), 4);
        let env = stub_env();

        // Subscribe BEFORE spawning so we see the emit.
        let mut rx = events.subscribe_feature("feat-panic");

        let run_id = manager
            .spawn(
                "feat-panic",
                env,
                move |_env, _permit, _cancel| async move {
                    panic!("intentional panic for supervisor test");
                },
            )
            .expect("spawn");

        // The supervisor polls every 100ms, then emits Cancelled.
        // Deadline: 3s worst-case.
        let payload = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("Cancelled event within 3s")
            .expect("receiver alive");
        assert_eq!(
            payload.event,
            pice_core::events::ManifestEvent::Cancelled,
            "panicked task should emit Cancelled"
        );
        assert_eq!(payload.run_id, run_id);
        assert_eq!(
            payload.data.get("reason").and_then(|v| v.as_str()),
            Some("panic"),
            "reason should be 'panic', got {}",
            payload.data
        );

        // And the feature should have been removed from the DashMap.
        for _ in 0..50 {
            if manager.active_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(manager.active_count(), 0, "panicked task should clean up");
    }

    #[tokio::test]
    async fn live_runs_returns_sorted_map() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        let env = stub_env();

        let gate = Arc::new(tokio::sync::Notify::new());
        for name in ["z-feat", "a-feat", "m-feat"] {
            let g = gate.clone();
            manager
                .spawn(
                    name,
                    env.clone(),
                    move |_env, _permit, _cancel| async move {
                        g.notified().await;
                        Ok(stub_manifest("f"))
                    },
                )
                .expect("spawn");
        }

        let live = manager.live_runs();
        let keys: Vec<_> = live.keys().cloned().collect();
        // BTreeMap guarantees ordered keys.
        assert_eq!(keys, vec!["a-feat", "m-feat", "z-feat"]);

        for _ in 0..3 {
            gate.notify_one();
        }
        for _ in 0..50 {
            if manager.active_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    async fn drain_on_shutdown_cancels_then_waits() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        let env = stub_env();

        manager
            .spawn("feat-drain", env, move |_env, _permit, cancel| async move {
                cancel.cancelled().await;
                Ok(stub_manifest("feat-drain"))
            })
            .expect("spawn");

        // Give the worker time to register the cancel handle.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let remaining = manager.drain_on_shutdown(Duration::from_secs(3)).await;
        assert_eq!(remaining, 0, "drain should bring count to zero");
    }

    #[tokio::test]
    async fn drain_on_shutdown_timeout_reports_remaining() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);
        let env = stub_env();

        // Spawn a feature that IGNORES the cancel token — simulates an
        // orchestrator that hangs past the shutdown budget.
        manager
            .spawn(
                "feat-stuck",
                env,
                move |_env, _permit, _cancel| async move {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok(stub_manifest("feat-stuck"))
                },
            )
            .expect("spawn");

        // Give it time to reach the sleep.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let remaining = manager.drain_on_shutdown(Duration::from_millis(300)).await;
        assert_eq!(remaining, 1, "stuck feature should still be counted");
    }

    #[tokio::test]
    async fn global_concurrency_clamped_to_hard_cap() {
        let events = EventBus::new();
        // Pass a value larger than the cap — it should be clamped down.
        let manager =
            FeatureJobManager::new(events, MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP + 100);
        // Indirectly verify via `available_permits()` on the semaphore.
        assert_eq!(
            manager.global_sem.available_permits(),
            MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP as usize,
            "global concurrency should clamp to hard cap"
        );
    }

    #[tokio::test]
    async fn zero_global_concurrency_clamped_up_to_one() {
        // A 0 cap would deadlock every spawn — clamp up to 1.
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 0);
        assert_eq!(manager.global_sem.available_permits(), 1);
    }

    #[tokio::test]
    async fn run_id_monotonic_within_process() {
        let events = EventBus::new();
        let manager = FeatureJobManager::new(events, 4);

        let mut ids = Vec::new();
        for _ in 0..5 {
            ids.push(manager.next_run_id());
        }

        // All distinct, and ascending by string compare (hex with
        // fixed-width fields preserves monotonicity).
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "run ids should be monotonic");
        let mut dedup = ids.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), ids.len(), "run ids should be unique");
    }
}
