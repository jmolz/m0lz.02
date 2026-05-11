//! Phase 7 Criterion 18 integration test — `run_id` is persisted on disk and
//! survives daemon restart + reconciliation.
//!
//! Flow:
//!   1. Boot a daemon via `lifecycle::run_with_paths`.
//!   2. Dispatch `CommandRequest::Evaluate { background: true }` via the
//!      `cli/dispatch` wire RPC.  Capture `run_id` from the
//!      `BackgroundDispatched` response.
//!   3. Verify the on-disk manifest carries `run_id == dispatched_run_id`.
//!   4. Shut down the daemon while the spawned future is blocked (via a live
//!      gate that never fires).  The daemon writes `Failed(failed-interrupted)`
//!      via graceful shutdown; if it times-out, the next restart's reconciler
//!      handles InProgress → Failed.
//!   5. Restart the daemon.  Startup reconciliation runs.
//!   6. Reload the manifest.  Assert `run_id` is UNCHANGED — reconciliation
//!      preserved the `run_id` even while rewriting `overall_status`.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use pice_core::cli::{
    CommandRequest, CommandResponse, EvaluateRequest, ExitJsonStatus, StatusMode, StatusRequest,
};
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_daemon::lifecycle;
use pice_daemon::server::auth;
use pice_daemon::server::unix::UnixConnection;
use pice_daemon::test_support::StateDirGuard;
use tokio::net::UnixStream;

// ─── Helpers ─────────────────────────────────────────────────────────────────

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() && UnixStream::connect(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket did not appear at {}", path.display());
}

fn write_plan_with_contract(root: &std::path::Path, file_stem: &str) -> std::path::PathBuf {
    let plans_dir = root.join(".claude/plans");
    std::fs::create_dir_all(&plans_dir).unwrap();
    let path = plans_dir.join(format!("{file_stem}.md"));
    std::fs::write(
        &path,
        r#"# Plan

## Contract

```json
{
  "feature": "test",
  "tier": 1,
  "pass_threshold": 8,
  "criteria": [
    {"name": "works", "threshold": 8, "validation": "manual"}
  ]
}
```
"#,
    )
    .unwrap();
    path
}

fn init_git(root: &std::path::Path) {
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output();
    let _ = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(root)
        .output();
}

struct LayersTomlGuard {
    path: PathBuf,
    previous: Option<Vec<u8>>,
}

impl Drop for LayersTomlGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(bytes) => {
                let _ = std::fs::write(&self.path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&self.path);
                if let Some(parent) = self.path.parent() {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
    }
}

fn ensure_layers_toml(root: &std::path::Path) -> LayersTomlGuard {
    let path = root.join(".pice/layers.toml");
    let previous = std::fs::read(&path).ok();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(
        &path,
        r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#,
    )
    .unwrap();
    LayersTomlGuard { path, previous }
}

// ─── Helper: dispatch cli/dispatch RPC over a Unix socket ────────────────────

/// Send a `CommandRequest` via the `cli/dispatch` wire RPC and return
/// the parsed `CommandResponse` or panic with a diagnostic.
async fn wire_dispatch(
    sock_path: &std::path::Path,
    token: &str,
    msg_id: u64,
    req: CommandRequest,
) -> CommandResponse {
    let stream = UnixStream::connect(sock_path)
        .await
        .expect("connect to daemon");
    let mut conn = UnixConnection::new(stream);

    let params = serde_json::to_value(&req).expect("serialize CommandRequest");
    let daemon_req = DaemonRequest::new(msg_id, methods::CLI_DISPATCH, token, params);
    conn.write_message(&daemon_req).await.expect("write");

    let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");

    assert!(
        resp.error.is_none(),
        "cli/dispatch returned error: {:?}",
        resp.error
    );

    let result = resp.result.expect("cli/dispatch must carry result");
    serde_json::from_value(result).expect("parse CommandResponse")
}

/// Send `daemon/shutdown` and wait for the daemon task to finish.
async fn shutdown_daemon(
    sock_path: &std::path::Path,
    token: &str,
    handle: tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let stream = UnixStream::connect(sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let req = DaemonRequest::new(99, methods::DAEMON_SHUTDOWN, token, serde_json::json!({}));
    conn.write_message(&req).await.expect("write shutdown");
    let _: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    drop(conn);
    let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
}

// ─── Main test ───────────────────────────────────────────────────────────────

/// Criterion 18: `run_id` on the Queued/InProgress manifest is preserved
/// through a daemon restart + startup reconciliation.
///
/// A minimal `.pice/layers.toml` keeps background evaluate on the Stack
/// Loops surface. We therefore:
///   a) Capture the `run_id` from the `BackgroundDispatched` response.
///   b) Read the Queued manifest from disk immediately after dispatch —
///      the Queued manifest carries `run_id` (written by
///      `handlers::background::write_queued_manifest`).
///   c) Let the feature run to completion (fast in the v0.1 fallback path).
///   d) Confirm the terminal manifest (Passed/Failed) STILL carries the
///      same `run_id` — the orchestrator closure preserves it.
///   e) Shut down and restart the daemon.  Startup reconciliation only
///      rewrites `InProgress` manifests; terminal ones are untouched.
///   f) Reload the manifest and assert `run_id` is unchanged.
///
/// For the interrupted-restart sub-case (step e produces an InProgress
/// manifest), we seed a second fixture that is InProgress at restart time
/// and verify reconciliation preserves its `run_id` while flipping status.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_id_persists_through_dispatch_and_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project tempdir");
    init_git(project.path());
    // The daemon uses `std::env::current_dir()` as project_root, so we
    // pre-seed the state dir namespace for the current working directory's
    // hash to make manifest_path_for work correctly.
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let _layers_guard = ensure_layers_toml(&project_root);
    let plan_path = write_plan_with_contract(&project_root, "run-id-persist-feat");

    // ── Boot daemon ──────────────────────────────────────────────────────────
    let sock_path = dir.path().join("d1.sock");
    let token_path = dir.path().join("d1.token");
    let socket_path = SocketPath::Unix(sock_path.clone());
    let tp = token_path.clone();
    let handle1 = tokio::spawn(lifecycle::run_with_paths(socket_path, tp));
    wait_for_socket(&sock_path).await;
    let token1 = auth::read_token_file(&token_path).expect("token");

    // ── Step 2: dispatch --background ────────────────────────────────────────
    let dispatch_resp = wire_dispatch(
        &sock_path,
        &token1,
        1,
        CommandRequest::Evaluate(EvaluateRequest {
            plan_path: plan_path.clone(),
            json: true,
            background: true,
            wait: false,
            timeout_secs: None,
        }),
    )
    .await;

    let dispatched_run_id = match &dispatch_resp {
        CommandResponse::Json { value } => {
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::BackgroundDispatched.as_str(),
                "expected background-dispatched, got: {value:?}"
            );
            value["run_id"]
                .as_str()
                .expect("run_id in BackgroundDispatched response")
                .to_string()
        }
        other => panic!("expected Json(background-dispatched), got {other:?}"),
    };

    // ── Step 3: check disk manifest carries A run_id (source of truth) ───────
    // The dispatch response and disk manifest must carry the same caller-minted
    // run_id. The disk manifest remains the source of truth for the restart
    // persistence assertion.
    let manifest_path =
        VerificationManifest::manifest_path_for("run-id-persist-feat", &project_root)
            .expect("manifest path");
    assert!(
        manifest_path.exists(),
        "Queued manifest must exist on disk at {}",
        manifest_path.display()
    );
    let queued_manifest = VerificationManifest::load(&manifest_path).expect("load queued");
    let disk_run_id = queued_manifest
        .run_id
        .clone()
        .expect("Queued manifest must have a run_id set by dispatch");
    assert!(
        disk_run_id.starts_with("r-"),
        "run_id on disk should match FeatureJobManager format, got {disk_run_id}"
    );
    assert_eq!(
        disk_run_id, dispatched_run_id,
        "dispatch response and Queued manifest must agree on run_id"
    );

    // ── Wait for the spawned future to complete (v0.1 fallback is fast) ──────
    // Poll until the manifest transitions out of Queued/InProgress.
    let terminal_manifest = {
        let mut m = queued_manifest;
        for _ in 0..100 {
            m = VerificationManifest::load(&manifest_path).unwrap_or(m.clone());
            if !matches!(
                m.overall_status,
                ManifestStatus::Queued | ManifestStatus::InProgress
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        m
    };

    assert_eq!(
        terminal_manifest.run_id.as_deref(),
        Some(disk_run_id.as_str()),
        "terminal manifest must preserve the dispatch run_id (step d)"
    );

    // ── Shutdown daemon 1 ────────────────────────────────────────────────────
    shutdown_daemon(&sock_path, &token1, handle1).await;

    // ── Step 5: restart daemon ────────────────────────────────────────────────
    let sock_path2 = dir.path().join("d2.sock");
    let token_path2 = dir.path().join("d2.token");
    let socket_path2 = SocketPath::Unix(sock_path2.clone());
    let tp2 = token_path2.clone();
    let handle2 = tokio::spawn(lifecycle::run_with_paths(socket_path2, tp2));
    wait_for_socket(&sock_path2).await;
    let token2 = auth::read_token_file(&token_path2).expect("token2");

    // ── Step 6: assert run_id preserved after restart ─────────────────────────
    // Startup reconciliation: terminal manifests are untouched; InProgress
    // manifests are rewritten to Failed but run_id is preserved (the
    // rewrite_interrupted function copies all fields except overall_status).
    let post_restart = VerificationManifest::load(&manifest_path).expect("load post-restart");
    // The run_id on disk after restart must equal the run_id we observed
    // BEFORE the restart (from the Queued manifest immediately after dispatch).
    assert_eq!(
        post_restart.run_id.as_deref(),
        Some(disk_run_id.as_str()),
        "run_id must be preserved after daemon restart + reconciliation (step f)"
    );

    // Public status surface must report the same persisted run_id after
    // restart. This pins the contract's `pice status {feature_id}` path,
    // not just direct disk reload.
    let status_resp = wire_dispatch(
        &sock_path2,
        &token2,
        2,
        CommandRequest::Status(StatusRequest {
            json: true,
            mode: StatusMode::Detail,
            feature_id: Some("run-id-persist-feat".to_string()),
            stream_json: false,
            timeout_secs: None,
        }),
    )
    .await;
    match status_resp {
        CommandResponse::Json { value } => {
            let manifest: VerificationManifest =
                serde_json::from_value(value).expect("status detail manifest");
            assert_eq!(
                manifest.run_id.as_deref(),
                Some(disk_run_id.as_str()),
                "pice status detail must surface the restart-preserved run_id"
            );
        }
        other => panic!("expected Json status detail, got {other:?}"),
    }

    // Cleanup.
    shutdown_daemon(&sock_path2, &token2, handle2).await;
    let _ = std::fs::remove_file(&manifest_path);
    let _ = std::fs::remove_file(&plan_path);
}

// ─── Sub-case: InProgress at restart time has run_id preserved ───────────────

/// If the daemon crashes while a feature is `InProgress`, the reconciler
/// rewrites `overall_status` to `Failed` — but the `run_id` field MUST
/// be preserved verbatim (it is audit-trail data; callers who observed
/// the initial `BackgroundDispatched` response rely on the run_id to
/// correlate their telemetry entries).
///
/// This test seeds an InProgress manifest with a known run_id, boots a
/// fresh daemon (no prior process, so it treats the manifest as interrupted),
/// and asserts the reconciler preserved the run_id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_id_preserved_when_reconciler_rewrites_in_progress_to_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let feature_id = "run-id-reconcile-feat";
    let expected_run_id = "r-preserved-across-restart";

    // Seed an InProgress manifest with a known run_id.
    let manifest_path =
        VerificationManifest::manifest_path_for(feature_id, &project_root).expect("manifest path");
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut m = VerificationManifest::new(feature_id, &project_root);
    m.overall_status = ManifestStatus::InProgress;
    m.run_id = Some(expected_run_id.to_string());
    m.save(&manifest_path).expect("save");

    // Boot daemon — reconciliation runs during startup.
    let sock_path = dir.path().join("d3.sock");
    let token_path = dir.path().join("d3.token");
    let socket_path = SocketPath::Unix(sock_path.clone());
    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(socket_path, tp));
    wait_for_socket(&sock_path).await;
    let token = auth::read_token_file(&token_path).expect("token");

    // At this point reconciliation has run. Reload the manifest.
    let reconciled = VerificationManifest::load(&manifest_path).expect("load reconciled");

    // The status must have been rewritten to Failed.
    assert_eq!(
        reconciled.overall_status,
        ManifestStatus::Failed,
        "reconciler must rewrite InProgress to Failed"
    );

    // The run_id must be UNCHANGED — reconciliation preserves it.
    assert_eq!(
        reconciled.run_id.as_deref(),
        Some(expected_run_id),
        "reconciler must preserve run_id when rewriting InProgress → Failed"
    );

    // Cleanup.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let req = DaemonRequest::new(1, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn.write_message(&req).await.expect("write");
    let _: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    drop(conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    let _ = std::fs::remove_file(&manifest_path);
}
