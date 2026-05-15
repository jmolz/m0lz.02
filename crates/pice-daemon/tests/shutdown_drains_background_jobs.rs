//! Phase 7 Criterion 17 integration test.
//!
//! Dispatches a long-running background job through a real daemon
//! socket. Sends `daemon/shutdown` and asserts:
//!
//! 1. The job's `CancellationToken` fires within 100ms of the shutdown
//!    RPC arriving (verified by the job writing a `cancel-observed`
//!    marker file on cancel).
//! 2. The job's simulated "final manifest save" runs to completion
//!    BEFORE the socket closes (verified by the `flushed` marker
//!    existing when the shutdown response comes back).
//! 3. The `daemon/shutdown` response body includes
//!    `drained_remaining: 0` — the handler awaited
//!    `FeatureJobManager::drain_on_shutdown` before returning.
//!
//! Seeds the background job via `DaemonContext::jobs().spawn(...)`
//! directly rather than going through `cli/dispatch` + a real
//! `pice evaluate --background` flow. That keeps the test hermetic
//! (no provider process, no plan file parsing) while still exercising
//! the actual handler wiring — `daemon/shutdown` → `route` →
//! `handle_shutdown` → `drain_on_shutdown` over a real socket round-trip.

#![cfg(unix)]

use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::VerificationManifest;
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
use pice_daemon::server::auth;
use pice_daemon::server::router::DaemonContext;
use pice_daemon::server::unix::UnixConnection;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UnixStream;

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() && UnixStream::connect(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket did not appear at {}", path.display());
}

fn stub_env(state_dir: &std::path::Path, project: &std::path::Path) -> Arc<JobEnv> {
    Arc::new(JobEnv {
        state_dir: state_dir.to_path_buf(),
        project_root: project.to_path_buf(),
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
    })
}

/// End-to-end over a real Unix socket: a long-running background job
/// is cancelled by `daemon/shutdown`, its final flush completes, and
/// the RPC response arrives AFTER both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_shutdown_drains_background_jobs_before_responding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let marker_dir = dir.path().join("markers");
    std::fs::create_dir_all(&marker_dir).unwrap();
    let cancel_marker = marker_dir.join("cancel-observed");
    let flushed_marker = marker_dir.join("flushed");

    // Boot the daemon.
    //
    // We don't go through `lifecycle::run_with_paths` here because we
    // need to pre-spawn a background job in the SAME DaemonContext
    // that the socket accept-loop will use. Instead, build the ctx
    // manually, seed the job, then hand the ctx to a custom accept
    // loop that mirrors `run_unix`.
    let token = auth::generate_token().expect("token");
    auth::write_token_file(&token_path, &token).expect("write token");

    let project = dir.path().to_path_buf();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let ctx = Arc::new(DaemonContext::new(token.clone(), project.clone()));

    // Seed the background job BEFORE the socket is up. The closure
    // waits for the cancel token, records cancel observation, sleeps
    // 80ms simulating a final manifest save, records flush
    // observation, returns Ok.
    let cancel_marker_c = cancel_marker.clone();
    let flushed_marker_c = flushed_marker.clone();
    let cancel_ts = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let cancel_ts_c = cancel_ts.clone();

    ctx.jobs()
        .spawn(
            "feat-drain-int",
            ctx.jobs().next_run_id(),
            stub_env(&state_dir, &project),
            move |_env, permit, cancel| async move {
                let _hold = permit;
                cancel.cancelled().await;
                *cancel_ts_c.lock().unwrap() = Some(Instant::now());
                std::fs::write(&cancel_marker_c, b"cancel").unwrap();
                // Simulate a final manifest save flush.
                tokio::time::sleep(Duration::from_millis(80)).await;
                std::fs::write(&flushed_marker_c, b"flushed").unwrap();
                Ok(VerificationManifest::new("feat-drain-int", &project))
            },
        )
        .expect("spawn");

    assert_eq!(
        ctx.jobs().active_count(),
        1,
        "background job must be live before shutdown"
    );
    assert!(!cancel_marker.exists());
    assert!(!flushed_marker.exists());

    // Start a minimal accept loop mirroring `lifecycle::run_unix`.
    let socket_path = SocketPath::Unix(sock_path.clone());
    let ctx_for_loop = Arc::clone(&ctx);
    let sock_path_for_loop = sock_path.clone();
    let handle = tokio::spawn(async move {
        let listener = match socket_path {
            SocketPath::Unix(ref p) => {
                pice_daemon::server::unix::UnixSocketListener::bind(p).await?
            }
            _ => unreachable!(),
        };
        let _ = sock_path_for_loop;
        loop {
            tokio::select! {
                result = listener.accept() => {
                    if let Ok(mut conn) = result {
                        let ctx = Arc::clone(&ctx_for_loop);
                        tokio::spawn(async move {
                            loop {
                                let req: DaemonRequest = match conn.read_message().await {
                                    Ok(Some(r)) => r,
                                    _ => break,
                                };
                                let resp = pice_daemon::server::router::route(req, &ctx).await;
                                if conn.write_message(&resp).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if ctx_for_loop.is_shutdown_requested() {
                        break;
                    }
                }
            }
        }
        // Mirror `lifecycle::run_unix` cleanup drain.
        let _ = ctx_for_loop
            .jobs()
            .drain_on_shutdown(Duration::from_secs(10))
            .await;
        Ok::<(), anyhow::Error>(())
    });

    wait_for_socket(&sock_path).await;

    // Issue daemon/shutdown and measure round-trip.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(1, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));

    let t_dispatch = Instant::now();
    conn.write_message(&shutdown_req).await.expect("write");
    let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    let elapsed = t_dispatch.elapsed();

    // (3) response shape — drained_remaining present + zero.
    assert!(resp.error.is_none(), "shutdown must succeed");
    let result = resp.result.expect("result body");
    assert_eq!(result["shutting_down"], true);
    assert_eq!(
        result["drained_remaining"], 0,
        "handler must have awaited drain to completion before responding"
    );

    // (1) cancel fired within 100ms of dispatch.
    let cancel_at = cancel_ts
        .lock()
        .unwrap()
        .expect("cancel token must have fired");
    assert!(
        cancel_at.duration_since(t_dispatch) <= Duration::from_millis(100),
        "cancel token must fire within 100ms of daemon/shutdown; actual = {:?}",
        cancel_at.duration_since(t_dispatch)
    );

    // (2) final flush marker exists at response time (response emitted
    // AFTER the manifest-save simulation completed).
    assert!(
        cancel_marker.exists(),
        "cancel marker must be on disk when response arrives"
    );
    assert!(
        flushed_marker.exists(),
        "flushed marker must be on disk when response arrives — \
         drain_on_shutdown awaited the supervised task, preventing torn \
         state"
    );

    // Sanity: elapsed is at least the flush-simulation 80ms (response
    // was NOT emitted early).
    assert!(
        elapsed >= Duration::from_millis(70),
        "response must not beat the drain; elapsed = {:?}",
        elapsed
    );

    drop(conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}
