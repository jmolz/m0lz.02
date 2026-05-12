//! Phase 7 `FeatureJobManager` — detached-task lifecycle for background
//! `pice evaluate --background` / `pice execute --background` dispatches.
//!
//! The manager owns:
//! - A `DashMap<FeatureId, JobHandle>` tracking every live background feature.
//! - An `Arc<Semaphore>` representing the global
//!   `max_global_provider_concurrency` cap (clamped to
//!   `MAX_GLOBAL_PROVIDER_CONCURRENCY_HARD_CAP = 32`). This bounds TOTAL
//!   concurrent provider sessions across all features. The existing Phase 5
//!   `max_parallelism` semaphore lives inside each feature's orchestrator
//!   invocation and bounds intra-cohort parallelism — it is NOT touched
//!   here. The two limits are independent.
//! - A `EventBus` handle for emitting `ManifestEvent::Cancelled` on
//!   panicked futures (the only event the manager itself is responsible
//!   for; everything else is emitted by the orchestrator through
//!   `ManifestSaver::save_and_emit`).
//! - An atomic run-id counter for stable-order run ids within a process.
//!
//! See `.claude/plans/phase-7-background-execution.md` → Task 7 for the
//! canonical semaphore lifecycle (single authoritative flow): dispatch
//! writes `Queued` to disk; `spawn(...)` immediately `tokio::spawn`s the
//! future; the spawned future's FIRST action is `global_sem.acquire_owned().await`;
//! only after the permit is held does the future transition
//! `Queued → InProgress`.

pub mod manager;
pub mod recovery;

pub use manager::{FeatureJobManager, JobHandle, RunId, SpawnError};
pub use recovery::{reconcile_on_startup, ReconciliationReport, FAILED_INTERRUPTED};
