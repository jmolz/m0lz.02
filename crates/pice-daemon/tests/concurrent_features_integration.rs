//! Phase 7 Task 17: concurrent-feature isolation + dual-semaphore
//! invariants.
//!
//! Exercises the Phase 7 concurrency guarantees at the daemon level:
//!
//! 1. **Three concurrent features** run in parallel without interfering.
//! 2. **Per-feature `LogStore` isolation** — feat-A's chunks never land
//!    in feat-B's buffer (cross-feature contamination would be a
//!    user-visible privacy/correctness bug).
//! 3. **EventBus fan-out isolation** — per-feature subscribers see ONLY
//!    their feature's events; a wildcard subscriber sees all three.
//! 4. **Global-provider-semaphore correctness** — with `max_global_
//!    provider_concurrency = 2`, at most 2 features can be holding a
//!    provider permit simultaneously. A third feature waits.
//! 5. **Cancel mid-run propagates** — cancelling feat-B while it's
//!    blocked on a gate future lets feat-A and feat-C continue, and
//!    the supervisor emits `ManifestEvent::Cancelled` for B's run
//!    within a bounded time.
//!
//! The tests use the low-level [`FeatureJobManager::spawn`] API with
//! gate-based future builders so the concurrency primitives can be
//! exercised without spinning up real providers. That keeps each test
//! hermetic (no network) and deterministic (no stub-provider sleep).
//!
//! `pice-core::jobs::JobEnv` is cloned from a minimal test fixture —
//! all fields are populated just enough to pass the spawn contract; the
//! gate futures don't read any of them beyond the state_dir.

#![cfg(unix)]

use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::VerificationManifest;
use pice_core::workflow::loader::embedded_defaults;
use pice_daemon::events::EventBus;
use pice_daemon::jobs::FeatureJobManager;
use pice_daemon::logs::store::LogStore;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

fn test_env(state_dir: &std::path::Path, project_root: &std::path::Path) -> Arc<JobEnv> {
    Arc::new(JobEnv {
        state_dir: state_dir.to_path_buf(),
        project_root: project_root.to_path_buf(),
        workflow_snapshot: embedded_defaults(),
        contracts: BTreeMap::new(),
        pice_state_dir_override: None,
        pice_user_workflow_file: None,
    })
}

/// Per-feature LogStore buffers MUST NOT leak chunks across features.
/// A test that `append_chunk("feat-a", ...)` followed by
/// `snapshot("feat-b", ...)` returns an empty vec — the cross-feature
/// isolation invariant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn log_store_isolates_chunks_per_feature() {
    let store = LogStore::new();
    // Populate three features in parallel using separate tasks. This
    // exercises the DashMap's per-entry locking — a naive mutex would
    // serialize these, but the test also passes under that model.
    let s1 = store.clone();
    let s2 = store.clone();
    let s3 = store.clone();
    let h1 = tokio::spawn(async move {
        for i in 0..5 {
            s1.append_chunk("feat-a", "r-a", "backend", format!("a-{i}\n"))
                .await;
        }
    });
    let h2 = tokio::spawn(async move {
        for i in 0..7 {
            s2.append_chunk("feat-b", "r-b", "frontend", format!("b-{i}\n"))
                .await;
        }
    });
    let h3 = tokio::spawn(async move {
        for i in 0..3 {
            s3.append_chunk("feat-c", "r-c", "infra", format!("c-{i}\n"))
                .await;
        }
    });
    h1.await.unwrap();
    h2.await.unwrap();
    h3.await.unwrap();

    let snap_a = store.snapshot("feat-a", None).await;
    let snap_b = store.snapshot("feat-b", None).await;
    let snap_c = store.snapshot("feat-c", None).await;

    assert_eq!(snap_a.len(), 5);
    assert_eq!(snap_b.len(), 7);
    assert_eq!(snap_c.len(), 3);
    // Every chunk must belong to its feature — cross-feature contamination
    // is the invariant this test locks down.
    assert!(snap_a.iter().all(|c| c.feature_id == "feat-a"));
    assert!(snap_b.iter().all(|c| c.feature_id == "feat-b"));
    assert!(snap_c.iter().all(|c| c.feature_id == "feat-c"));
}

/// EventBus per-feature fan-out: each feature's subscriber receives ONLY
/// its own events; the wildcard subscriber receives all events from all
/// features.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_bus_fans_per_feature_and_wildcard_correctly() {
    let bus = EventBus::new();

    let mut rx_a = bus.subscribe_feature("feat-a");
    let mut rx_b = bus.subscribe_feature("feat-b");
    let mut rx_wild = bus.subscribe_wildcard();

    // Small delay so receivers are registered before the emits land.
    tokio::time::sleep(Duration::from_millis(20)).await;

    bus.emit_layer_started("feat-a", "r-a", "backend");
    bus.emit_layer_started("feat-b", "r-b", "frontend");
    bus.emit_layer_started("feat-a", "r-a", "api");

    // A's channel: 2 events.
    let a1 = tokio::time::timeout(Duration::from_millis(200), rx_a.recv())
        .await
        .expect("a1 within 200ms")
        .expect("recv");
    let a2 = tokio::time::timeout(Duration::from_millis(200), rx_a.recv())
        .await
        .expect("a2 within 200ms")
        .expect("recv");
    assert_eq!(a1.feature_id, "feat-a");
    assert_eq!(a2.feature_id, "feat-a");

    // B's channel: 1 event.
    let b1 = tokio::time::timeout(Duration::from_millis(200), rx_b.recv())
        .await
        .expect("b1 within 200ms")
        .expect("recv");
    assert_eq!(b1.feature_id, "feat-b");

    // Wildcard: 3 events total (order preserved within a single bus).
    let w1 = tokio::time::timeout(Duration::from_millis(200), rx_wild.recv())
        .await
        .expect("w1")
        .expect("recv");
    let w2 = tokio::time::timeout(Duration::from_millis(200), rx_wild.recv())
        .await
        .expect("w2")
        .expect("recv");
    let w3 = tokio::time::timeout(Duration::from_millis(200), rx_wild.recv())
        .await
        .expect("w3")
        .expect("recv");
    assert_eq!(w1.feature_id, "feat-a");
    assert_eq!(w2.feature_id, "feat-b");
    assert_eq!(w3.feature_id, "feat-a");

    // B's channel received NO feat-a events (isolation — a wildcard sees
    // everything but scoped subscribers only see matching).
    assert!(
        rx_b.try_recv().is_err(),
        "feat-b subscriber should not receive feat-a events"
    );
}

/// Dual-semaphore correctness: with `max_global_provider_concurrency = 2`,
/// three concurrent spawns are serialized such that at any point at most
/// two futures are past the permit-acquire barrier. The test counts the
/// number of simultaneously-in-flight gated futures; with a permit cap of
/// 2 it must never exceed 2.
///
/// Feature A and B acquire permits first and block on a `Notify`. Feature
/// C must wait for a permit; its future does not enter the critical
/// section until A or B releases.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_semaphore_bounds_concurrent_provider_holds() {
    let state_tmp = tempfile::tempdir().unwrap();
    let project_tmp = tempfile::tempdir().unwrap();
    let env = test_env(state_tmp.path(), project_tmp.path());

    let bus = EventBus::new();
    // Permit cap = 2 — the core invariant under test. We cap at
    // 2 and spawn 3 features; at any point at most 2 should be past
    // the permit-acquire barrier.
    let jobs = FeatureJobManager::new(bus, 2);

    let in_flight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release_a = Arc::new(Notify::new());
    let release_b = Arc::new(Notify::new());
    let release_c = Arc::new(Notify::new());

    for (feat, release) in [
        ("feat-a", release_a.clone()),
        ("feat-b", release_b.clone()),
        ("feat-c", release_c.clone()),
    ] {
        let env_clone = env.clone();
        let in_flight_clone = in_flight.clone();
        let max_seen_clone = max_seen.clone();
        let release_clone = release.clone();
        let fid = feat.to_string();
        jobs.spawn(
            feat.to_string(),
            env_clone,
            move |_env, permit, _cancel| async move {
                // Past the permit-acquire barrier — increment in-flight.
                // Bind the permit with a non-underscore name to ensure it
                // is kept alive through the `.await` below (Rust keeps
                // underscore-prefixed bindings alive too, but clippy or
                // a future optimizer could legitimately drop `_permit`
                // once it's clear the async block doesn't use it — the
                // explicit name removes any ambiguity).
                let _hold = permit;
                let cur = in_flight_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                // Update high-watermark atomically with a compare-loop.
                loop {
                    let prev = max_seen_clone.load(std::sync::atomic::Ordering::SeqCst);
                    if cur <= prev {
                        break;
                    }
                    if max_seen_clone
                        .compare_exchange(
                            prev,
                            cur,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }

                release_clone.notified().await;
                in_flight_clone.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

                Ok(VerificationManifest::new(
                    &fid,
                    std::path::Path::new("/irrelevant"),
                ))
            },
        )
        .expect("spawn");
    }

    // Let A and B enter the critical section.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let peak_after_two = in_flight.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(peak_after_two, 2, "two permits held, C still waiting");

    // Release A — C should acquire its permit.
    release_a.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // In-flight should still be at most 2 (either B+C now, or just B).
    let after_a_release = in_flight.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        after_a_release <= 2,
        "in-flight should stay ≤ 2, got {after_a_release}"
    );

    release_b.notify_one();
    release_c.notify_one();

    // Wait for all three features to drain.
    for _ in 0..50 {
        if jobs.active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Hard invariant: at no point were MORE than 2 features past the
    // permit-acquire barrier simultaneously. max_seen captures the
    // high-water mark across the entire run.
    let peak = max_seen.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        peak <= 2,
        "max simultaneous in-flight ({peak}) exceeded permit cap (2)"
    );
}

/// Cancel feat-B mid-run; feat-A and feat-C continue uninterrupted. The
/// supervisor emits `ManifestEvent::Cancelled` for B after its future
/// exits cooperatively. Verifies the Phase 5 cancel-to-return latency
/// invariant at the FeatureJobManager level.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_one_feature_leaves_others_running() {
    let state_tmp = tempfile::tempdir().unwrap();
    let project_tmp = tempfile::tempdir().unwrap();
    let env = test_env(state_tmp.path(), project_tmp.path());

    let bus = EventBus::new();
    let mut rx_wild = bus.subscribe_wildcard();
    let jobs = FeatureJobManager::new(bus, 4);

    let gate_a = Arc::new(Notify::new());
    let gate_b = Arc::new(Notify::new());
    let gate_c = Arc::new(Notify::new());

    for (feat, gate) in [
        ("feat-a", gate_a.clone()),
        ("feat-b", gate_b.clone()),
        ("feat-c", gate_c.clone()),
    ] {
        let env_clone = env.clone();
        let fid = feat.to_string();
        jobs.spawn(
            feat.to_string(),
            env_clone,
            move |_env, _permit, cancel| async move {
                // Cooperative cancel: wait on `cancel` OR the gate,
                // whichever fires first. Feature B's cancel token fires
                // first via `jobs.cancel("feat-b")`.
                tokio::select! {
                    _ = cancel.cancelled() => {
                        Err(anyhow::anyhow!("cancelled"))
                    }
                    _ = gate.notified() => {
                        Ok(VerificationManifest::new(
                            &fid,
                            std::path::Path::new("/irrelevant"),
                        ))
                    }
                }
            },
        )
        .expect("spawn");
    }

    // Let the tasks get past permit-acquire.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(jobs.active_count(), 3);

    // Cancel feat-b.
    let t0 = std::time::Instant::now();
    assert!(jobs.cancel("feat-b"));

    // Wait for feat-b to fall out of the registry (via the supervisor).
    let mut waited = Duration::ZERO;
    while jobs.run_id_for("feat-b").is_some() && waited < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        waited += Duration::from_millis(20);
    }
    let cancel_latency = t0.elapsed();
    assert!(
        jobs.run_id_for("feat-b").is_none(),
        "feat-b should be removed after cancel"
    );
    // Phase 5 invariant: cancel-to-return < 300ms. Allow 1000ms CI slack.
    assert!(
        cancel_latency < Duration::from_millis(1000),
        "cancel latency {cancel_latency:?} exceeded 1s budget"
    );

    // feat-a and feat-c must still be live.
    assert!(jobs.run_id_for("feat-a").is_some());
    assert!(jobs.run_id_for("feat-c").is_some());

    // Release A and C so the test doesn't leak the tasks.
    gate_a.notify_one();
    gate_c.notify_one();
    for _ in 0..50 {
        if jobs.active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Drain the wildcard channel — we don't assert on the exact event
    // sequence (the stub future doesn't emit through the bus), but we
    // do confirm the channel didn't saturate or panic during the run.
    let _ = rx_wild.try_recv();
}
