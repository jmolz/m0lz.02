//! Phase 7 Criterion 2 integration test.
//!
//! **Invariant pinned:** a background feature task MUST outlive the originating
//! RPC connection. Closing (dropping) any CLI connection MUST NOT cancel a
//! task that was already running in the daemon's `FeatureJobManager`.
//!
//! ## Test design
//!
//! Includes both:
//! - a hermetic `DaemonContext::jobs().spawn()` test for the supervisor and
//!   subscribe handler; and
//! - a real `cli/dispatch → Evaluate(background=true)` test proving the
//!   originating dispatch connection can close without cancelling the feature.
//!
//! ### Step-by-step
//!
//! 1. Build a `DaemonContext` and start a minimal accept loop over a real
//!    Unix socket (mirrors `lifecycle::run_unix` without `run_with_paths`).
//! 2. Seed a background feature via `ctx.jobs().spawn(...)`. The closure
//!    emits a `LayerStarted` event, then waits for an explicit
//!    `tokio::sync::Notify` "unblock" signal before emitting `FeatureComplete`
//!    and returning. This keeps the task ALIVE so we can observe its lifetime
//!    relative to dropped connections.
//! 3. Open **connection A** and send `manifest/subscribe` filtered to the
//!    feature. After receiving the initial snapshot, **drop connection A**.
//! 4. Assert `active_count() >= 1` — the drop did NOT cancel the task.
//! 5. Open **connection B** and subscribe to the same feature. Subscribe
//!    connection B sets up a live receiver BEFORE we unblock the job.
//! 6. Unblock the job via the `Notify`. The closure emits `FeatureComplete`.
//! 7. Drain events from connection B until `FeatureComplete` arrives —
//!    **no fixed sleep** is used; observation is event-driven.
//! 8. Assert `active_count() == 0` — the feature ran to completion after
//!    connection A was dropped, confirming the drop did NOT cancel the task.
//!
//! ## Why a custom accept loop?
//!
//! `lifecycle::run_with_paths` builds its own `DaemonContext` internally, so
//! we cannot inject a job before the socket is up. The custom loop mirrors
//! the `lifecycle::run_unix` implementation faithfully, using the exact same
//! primitives (`UnixSocketListener`, `route`, `subscribe::dispatch`).

#![cfg(unix)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use pice_core::cli::{CommandRequest, CommandResponse, EvaluateRequest, ExitJsonStatus};
use pice_core::events::ManifestEvent;
use pice_core::events::ManifestEventPayload;
use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::VerificationManifest;
use pice_core::protocol::{methods, DaemonNotification, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
use pice_daemon::server::auth;
use pice_daemon::server::router::DaemonContext;
use pice_daemon::server::unix::UnixConnection;
use pice_daemon::test_support::StateDirGuard;
use tokio::net::UnixStream;
use tokio::sync::Notify;

// ─── Helpers ────────────────────────────────────────────────────────────────

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
    })
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
  "feature": "connection-drop",
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

fn init_git_with_backend_change(root: &std::path::Path) {
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output();
    let _ = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(root)
        .output();
    std::fs::write(src.join("lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
}

fn write_layers_toml(root: &std::path::Path) {
    let pice_dir = root.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(
        pice_dir.join("layers.toml"),
        r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/lib.rs"]
always_run = true
"#,
    )
    .unwrap();
}

fn write_layers_toml_with_inactive_first(root: &std::path::Path) {
    let pice_dir = root.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(
        pice_dir.join("layers.toml"),
        r#"
[layers]
order = ["inactive", "backend"]

[layers.inactive]
paths = ["docs/**"]
always_run = false

[layers.backend]
paths = ["src/lib.rs"]
always_run = false
"#,
    )
    .unwrap();
}

fn write_stub_config(root: &std::path::Path) {
    let pice_dir = root.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(
        pice_dir.join("config.toml"),
        r#"
[provider]
name = "stub"

[evaluation.primary]
provider = "stub"
model = "stub-model"

[evaluation.adversarial]
provider = "stub"
model = "stub-model"
effort = ""
enabled = false

[evaluation.tiers]
tier1_models = []
tier2_models = []
tier3_models = []
tier3_agent_team = false

[telemetry]
enabled = false
endpoint = ""

[metrics]
db_path = ".pice/metrics.db"
"#,
    )
    .unwrap();
}

fn write_fast_workflow(root: &std::path::Path) {
    let pice_dir = root.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(
        pice_dir.join("workflow.yaml"),
        r#"schema_version: "0.2"
defaults:
  tier: 1
  min_confidence: 0.50
  max_passes: 1
  model: stub-model
  budget_usd: 2.0
  cost_cap_behavior: halt
  max_parallelism: 4
  max_global_provider_concurrency: 2
phases:
  evaluate:
    parallel: false
"#,
    )
    .unwrap();
}

fn stub_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct StubProviderEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubProviderEnv {
    fn new(latency_ms: u64) -> Self {
        Self::with_alive_file(latency_ms, None)
    }

    fn with_alive_file(latency_ms: u64, alive_file: Option<&std::path::Path>) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("PICE_STUB_SCORES", "9.5,0.001");
        std::env::set_var("PICE_STUB_LATENCY_MS", latency_ms.to_string());
        std::env::remove_var("PICE_STUB_ADVERSARIAL_SCORES");
        if let Some(path) = alive_file {
            std::env::set_var("PICE_STUB_ALIVE_FILE", path);
        } else {
            std::env::remove_var("PICE_STUB_ALIVE_FILE");
        }
        Self { _guard: guard }
    }
}

impl Drop for StubProviderEnv {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_SCORES");
        std::env::remove_var("PICE_STUB_LATENCY_MS");
        std::env::remove_var("PICE_STUB_ADVERSARIAL_SCORES");
        std::env::remove_var("PICE_STUB_ALIVE_FILE");
    }
}

fn active_stub_session_count(path: &std::path::Path) -> usize {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    let mut started = HashSet::<i32>::new();
    let mut finished = HashSet::<i32>::new();
    for line in contents.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 2 {
            continue;
        }
        let Ok(pid) = parts[1].parse::<i32>() else {
            continue;
        };
        match parts[0] {
            "alive" => {
                started.insert(pid);
            }
            "done" => {
                finished.insert(pid);
            }
            _ => {}
        }
    }
    started.difference(&finished).count()
}

async fn dispatch_background_evaluate(
    sock_path: &std::path::Path,
    token: &str,
    id: u64,
    plan_path: std::path::PathBuf,
) -> (String, String) {
    let stream = UnixStream::connect(sock_path)
        .await
        .expect("connect dispatch");
    let mut conn = UnixConnection::new(stream);
    let req = CommandRequest::Evaluate(EvaluateRequest {
        plan_path,
        json: true,
        background: true,
        wait: false,
        timeout_secs: None,
    });
    let daemon_req = DaemonRequest::new(
        id,
        methods::CLI_DISPATCH,
        token,
        serde_json::to_value(req).expect("serialize command request"),
    );
    conn.write_message(&daemon_req)
        .await
        .expect("write dispatch");
    let resp: DaemonResponse = conn
        .read_message()
        .await
        .expect("read dispatch")
        .expect("not EOF");
    assert!(resp.error.is_none(), "dispatch error: {:?}", resp.error);
    let response: CommandResponse =
        serde_json::from_value(resp.result.expect("dispatch result")).expect("command response");
    match response {
        CommandResponse::Json { value } => {
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::BackgroundDispatched.as_str()
            );
            (
                value["feature_id"].as_str().unwrap().to_string(),
                value["run_id"].as_str().unwrap().to_string(),
            )
        }
        other => panic!("expected background-dispatched, got {other:?}"),
    }
}

/// Read frames from a `UnixConnection` (as raw `serde_json::Value`) and return
/// the first that is a notification (no `id` field) with the given method.
/// Waits up to `timeout` total before panicking.
async fn await_notification_event(
    conn: &mut UnixConnection,
    method: &str,
    timeout: Duration,
) -> ManifestEventPayload {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            panic!("timed out waiting for notification method={method}");
        }
        let frame: Option<serde_json::Value> = tokio::time::timeout(remaining, conn.read_message())
            .await
            .expect("read_message did not time out")
            .expect("read_message succeeded");

        let value = match frame {
            Some(v) => v,
            None => panic!("connection closed before receiving notification method={method}"),
        };

        // Notifications have no `id` field; responses do.
        if value.get("id").is_some() {
            // This is a response frame (the subscribe snapshot); skip it.
            continue;
        }
        // It's a notification — parse it.
        let notif: DaemonNotification =
            serde_json::from_value(value).expect("parse DaemonNotification");
        if notif.method != method {
            continue;
        }
        let payload: ManifestEventPayload =
            serde_json::from_value(notif.params).expect("parse ManifestEventPayload");
        return payload;
    }
}

fn spawn_accept_loop(
    ctx: Arc<DaemonContext>,
    sock_path: std::path::PathBuf,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        let socket_path = SocketPath::Unix(sock_path);
        let listener = match socket_path {
            SocketPath::Unix(ref p) => {
                pice_daemon::server::unix::UnixSocketListener::bind(p).await?
            }
            _ => unreachable!(),
        };
        loop {
            tokio::select! {
                result = listener.accept() => {
                    if let Ok(mut conn) = result {
                        let ctx = Arc::clone(&ctx);
                        tokio::spawn(async move {
                            loop {
                                let req: DaemonRequest = match conn.read_message().await {
                                    Ok(Some(r)) => r,
                                    _ => break,
                                };
                                use pice_daemon::handlers::subscribe as sub_handler;
                                if sub_handler::is_subscribe_method(&req.method) {
                                    let _ = sub_handler::dispatch(&ctx, &mut conn, req).await;
                                    break;
                                }
                                let resp = pice_daemon::server::router::route(req, &ctx).await;
                                if conn.write_message(&resp).await.is_err() {
                                    ctx.release_background_start_from_response(&resp);
                                    break;
                                }
                                ctx.release_background_start_from_response(&resp);
                            }
                        });
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if ctx.is_shutdown_requested() {
                        break;
                    }
                }
            }
        }
        let _ = ctx.jobs().drain_on_shutdown(Duration::from_secs(10)).await;
        Ok::<(), anyhow::Error>(())
    })
}

async fn request_shutdown(sock_path: &std::path::Path, token: &str, id: u64) {
    let stream = UnixStream::connect(sock_path)
        .await
        .expect("connect shutdown");
    let mut conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(id, methods::DAEMON_SHUTDOWN, token, serde_json::json!({}));
    conn.write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _: DaemonResponse = conn
        .read_message()
        .await
        .expect("read shutdown")
        .expect("not EOF");
}

// ─── Test ────────────────────────────────────────────────────────────────────

/// Criterion 2: a feature spawned by the real `cli/dispatch` evaluate path
/// outlives the dispatch socket that created it.
///
/// This is the production shape the contract cares about: connection A sends
/// `CLI_DISPATCH` for `Evaluate(background=true)`, receives
/// `background-dispatched`, and closes. Connection B subscribes afterwards and
/// still observes the live feature complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn originating_cli_dispatch_drop_does_not_cancel_background_evaluate() {
    let _stub_guard = StubProviderEnv::new(800);
    let dir = tempfile::tempdir_in("/private/tmp").expect("tempdir");
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");

    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _state_guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project");
    init_git_with_backend_change(project.path());
    write_layers_toml(project.path());
    write_stub_config(project.path());
    write_fast_workflow(project.path());
    let plan_path = write_plan_with_contract(project.path(), "dispatch-drop-evaluate");
    let fixture_diff =
        pice_core::prompt::helpers::get_git_diff(project.path()).expect("fixture git diff");
    assert!(
        fixture_diff.contains("diff --git a/src/lib.rs b/src/lib.rs"),
        "fixture must carry a backend diff, got:\n{fixture_diff}"
    );
    assert!(
        !pice_core::layers::filter::filter_diff_by_globs(
            &fixture_diff,
            &["src/lib.rs".to_string()]
        )
        .is_empty(),
        "fixture backend diff must match layers.toml glob"
    );

    let token = auth::generate_token().expect("generate token");
    auth::write_token_file(&token_path, &token).expect("write token file");
    let ctx = Arc::new(DaemonContext::new(
        token.clone(),
        project.path().to_path_buf(),
    ));
    let accept_handle = spawn_accept_loop(Arc::clone(&ctx), sock_path.clone());

    wait_for_socket(&sock_path).await;

    let stream_a = UnixStream::connect(&sock_path)
        .await
        .expect("connect dispatch");
    let mut conn_a = UnixConnection::new(stream_a);
    let req = CommandRequest::Evaluate(EvaluateRequest {
        plan_path,
        json: true,
        background: true,
        wait: false,
        timeout_secs: None,
    });
    let daemon_req = DaemonRequest::new(
        1,
        methods::CLI_DISPATCH,
        &token,
        serde_json::to_value(req).expect("serialize command request"),
    );
    conn_a
        .write_message(&daemon_req)
        .await
        .expect("write dispatch");
    let resp: DaemonResponse = conn_a
        .read_message()
        .await
        .expect("read dispatch")
        .expect("not EOF");
    assert!(resp.error.is_none(), "dispatch error: {:?}", resp.error);
    let response: CommandResponse =
        serde_json::from_value(resp.result.expect("dispatch result")).expect("command response");
    let (feature_id, run_id) = match response {
        CommandResponse::Json { value } => {
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::BackgroundDispatched.as_str()
            );
            (
                value["feature_id"].as_str().unwrap().to_string(),
                value["run_id"].as_str().unwrap().to_string(),
            )
        }
        other => panic!("expected background-dispatched, got {other:?}"),
    };
    assert_eq!(feature_id, "dispatch-drop-evaluate");

    drop(conn_a);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        ctx.jobs().active_count() >= 1,
        "closing the originating cli/dispatch socket must not cancel the job"
    );

    let stream_b = UnixStream::connect(&sock_path)
        .await
        .expect("connect subscribe");
    let mut conn_b = UnixConnection::new(stream_b);
    let params = serde_json::to_value(pice_core::protocol::subscribe::SubscribeManifestRequest {
        feature_id: Some(feature_id.clone()),
    })
    .unwrap();
    let subscribe = DaemonRequest::new(2, methods::MANIFEST_SUBSCRIBE, &token, params);
    conn_b
        .write_message(&subscribe)
        .await
        .expect("write subscribe");
    let snap: DaemonResponse = conn_b
        .read_message()
        .await
        .expect("read snapshot")
        .expect("not EOF");
    assert!(snap.error.is_none(), "subscribe error: {:?}", snap.error);

    let terminal_payload = loop {
        let payload = await_notification_event(
            &mut conn_b,
            methods::MANIFEST_EVENT,
            Duration::from_secs(10),
        )
        .await;
        if payload.event == ManifestEvent::FeatureComplete {
            break payload;
        }
    };

    assert_eq!(terminal_payload.feature_id, feature_id);
    assert_eq!(terminal_payload.run_id, run_id);
    assert!(
        terminal_payload.data.get("overall_status").is_some(),
        "FeatureComplete emitted through the real dispatch path must carry overall_status"
    );

    let mut log_snapshot = Vec::new();
    for _ in 0..100 {
        let chunks = ctx.logs().snapshot(&feature_id, None).await;
        if chunks.iter().any(|c| c.terminal) {
            log_snapshot = chunks;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !log_snapshot.is_empty(),
        "background evaluate should leave a terminal log snapshot"
    );
    assert!(
        log_snapshot
            .iter()
            .any(|c| !c.terminal && !c.text.is_empty()),
        "background evaluate should capture non-terminal log chunks before terminal frame"
    );
    assert!(
        log_snapshot.iter().any(|c| c.terminal),
        "background evaluate logs should retain the terminal frame for already-terminal readers"
    );

    for _ in 0..50 {
        if ctx.jobs().active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        ctx.jobs().active_count(),
        0,
        "background evaluate should finish after dispatch connection drop"
    );

    let stream_s = UnixStream::connect(&sock_path)
        .await
        .expect("connect shutdown");
    let mut conn_s = UnixConnection::new(stream_s);
    let shutdown_req =
        DaemonRequest::new(3, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn_s
        .write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _: DaemonResponse = conn_s
        .read_message()
        .await
        .expect("read shutdown")
        .expect("not EOF");
    drop(conn_s);
    let _ = tokio::time::timeout(Duration::from_secs(5), accept_handle).await;
}

/// Criterion 5 regression: background `Queued → InProgress` must not emit a
/// synthetic `LayerStarted` for `layers.order[0]`. The real Stack Loops DAG
/// activation is the only source of start events, so an inactive first
/// configured layer never appears in the live stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_evaluate_emits_layer_started_only_after_active_dag_selection() {
    let _stub_guard = StubProviderEnv::new(500);
    let dir = tempfile::tempdir_in("/private/tmp").expect("tempdir");
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");

    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _state_guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project");
    init_git_with_backend_change(project.path());
    write_layers_toml_with_inactive_first(project.path());
    write_stub_config(project.path());
    write_fast_workflow(project.path());
    let plan_path = write_plan_with_contract(project.path(), "inactive-first-evaluate");

    let token = auth::generate_token().expect("generate token");
    auth::write_token_file(&token_path, &token).expect("write token file");
    let ctx = Arc::new(DaemonContext::new(
        token.clone(),
        project.path().to_path_buf(),
    ));
    let accept_handle = spawn_accept_loop(Arc::clone(&ctx), sock_path.clone());
    wait_for_socket(&sock_path).await;

    let stream_sub = UnixStream::connect(&sock_path)
        .await
        .expect("connect subscribe");
    let mut conn_sub = UnixConnection::new(stream_sub);
    let params = serde_json::to_value(pice_core::protocol::subscribe::SubscribeManifestRequest {
        feature_id: Some("inactive-first-evaluate".to_string()),
    })
    .unwrap();
    let subscribe = DaemonRequest::new(11, methods::MANIFEST_SUBSCRIBE, &token, params);
    conn_sub
        .write_message(&subscribe)
        .await
        .expect("write subscribe");
    let snap: DaemonResponse = conn_sub
        .read_message()
        .await
        .expect("read snapshot")
        .expect("not EOF");
    assert!(snap.error.is_none(), "subscribe error: {:?}", snap.error);

    let (feature_id, _run_id) =
        dispatch_background_evaluate(&sock_path, &token, 12, plan_path).await;
    assert_eq!(feature_id, "inactive-first-evaluate");

    let mut started_layers = Vec::new();
    loop {
        let payload = await_notification_event(
            &mut conn_sub,
            methods::MANIFEST_EVENT,
            Duration::from_secs(10),
        )
        .await;
        match payload.event {
            ManifestEvent::LayerStarted => {
                started_layers.push(payload.layer.unwrap_or_default());
            }
            ManifestEvent::FeatureComplete => break,
            _ => {}
        }
    }

    assert_eq!(
        started_layers,
        vec!["backend".to_string()],
        "background evaluate must not pre-emit inactive layers"
    );

    request_shutdown(&sock_path, &token, 13).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), accept_handle).await;
}

/// Criterion 6 regression: exercise the production `cli/dispatch →
/// Evaluate(background=true)` path with three concurrently live features and
/// real stub-provider sessions. The global provider cap must bound provider
/// processes across feature jobs, not just manager futures.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_background_evaluates_share_global_provider_session_cap() {
    let dir = tempfile::tempdir_in("/private/tmp").expect("tempdir");
    let alive_path = dir.path().join("stub-provider-alive.log");
    std::fs::write(&alive_path, "").unwrap();
    let _stub_guard = StubProviderEnv::with_alive_file(900, Some(&alive_path));
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");

    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _state_guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project");
    init_git_with_backend_change(project.path());
    write_layers_toml(project.path());
    write_stub_config(project.path());
    write_fast_workflow(project.path());
    let plans = [
        write_plan_with_contract(project.path(), "prod-cap-a"),
        write_plan_with_contract(project.path(), "prod-cap-b"),
        write_plan_with_contract(project.path(), "prod-cap-c"),
    ];

    let token = auth::generate_token().expect("generate token");
    auth::write_token_file(&token_path, &token).expect("write token file");
    let ctx = Arc::new(DaemonContext::new(
        token.clone(),
        project.path().to_path_buf(),
    ));
    assert_eq!(
        ctx.jobs().provider_capacity(),
        2,
        "fixture workflow must configure max_global_provider_concurrency=2"
    );
    let accept_handle = spawn_accept_loop(Arc::clone(&ctx), sock_path.clone());
    wait_for_socket(&sock_path).await;

    for (idx, plan_path) in plans.into_iter().enumerate() {
        let (feature_id, _run_id) =
            dispatch_background_evaluate(&sock_path, &token, 20 + idx as u64, plan_path).await;
        assert!(feature_id.starts_with("prod-cap-"));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let mut peak_active = 0;
    let mut saw_cap_filled = false;
    loop {
        let active = active_stub_session_count(&alive_path);
        peak_active = peak_active.max(active);
        assert!(
            active <= 2,
            "global provider cap exceeded: active={active}, peak={peak_active}"
        );
        saw_cap_filled |= active == 2;
        if ctx.jobs().active_count() == 0 && active == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background evaluates did not drain before timeout; active_jobs={}, active_providers={active}",
            ctx.jobs().active_count()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        saw_cap_filled,
        "fixture should prove at least two provider sessions ran concurrently"
    );
    let contents = std::fs::read_to_string(&alive_path).unwrap();
    assert!(
        contents
            .lines()
            .filter(|line| line.starts_with("alive "))
            .count()
            >= 3,
        "all three background features should have started provider sessions; log:\n{contents}"
    );
    assert!(
        contents
            .lines()
            .filter(|line| line.starts_with("done "))
            .count()
            >= 3,
        "all provider sessions should have exited cleanly; log:\n{contents}"
    );

    request_shutdown(&sock_path, &token, 99).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), accept_handle).await;
}

/// Criterion 2: background task outlives the originating RPC connection.
///
/// A dropped CLI connection MUST NOT cancel a running `FeatureJobManager` task.
/// The task runs to completion; its `FeatureComplete` event is observable on a
/// second independent subscribe connection opened after the first was dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_drop_does_not_cancel_background_task() {
    let dir = tempfile::tempdir_in("/private/tmp").expect("tempdir");
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");

    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _state_guard = StateDirGuard::new(&state_dir);

    let project = dir.path().to_path_buf();

    // Build the daemon context manually so we can seed a job before the
    // socket is up — `lifecycle::run_with_paths` builds its own context
    // internally, which would prevent pre-seeding.
    let token = auth::generate_token().expect("generate token");
    auth::write_token_file(&token_path, &token).expect("write token file");

    let ctx = Arc::new(DaemonContext::new(token.clone(), project.clone()));

    // "Unblock" notifier: the closure waits on this before completing.
    let unblock = Arc::new(Notify::new());
    let unblock_for_job = Arc::clone(&unblock);

    // Keep a handle to the event bus so we can subscribe to feature events
    // in the test body without going through the socket accept loop.
    let bus_handle = ctx.events().clone();

    // Spawn the long-running job. The closure:
    //   1. Emits `LayerStarted` so the subscribe stream has at least one
    //      event to confirm the task is running.
    //   2. Awaits the unblock signal (keeps the task alive for the drop test).
    //   3. Emits `FeatureComplete` — observable on connection B.
    //   4. Returns Ok so the supervisor removes it from the DashMap.
    //
    // Note: the bus must be cloned into the closure; the closure runs on a
    // tokio worker thread that does not share the outer scope.
    let bus_for_job = bus_handle.clone();
    let project_for_job = project.clone();
    ctx.jobs()
        .spawn(
            "drop-survives-feat",
            "run-drop-test".to_string(),
            stub_env(&state_dir, &project),
            move |_env, permit, _cancel| async move {
                let _hold = permit; // keep global semaphore slot for task lifetime
                bus_for_job.emit_layer_started("drop-survives-feat", "run-drop-test", "backend");
                // Wait for the test to signal us — this is the window where
                // connection A will be dropped.
                unblock_for_job.notified().await;
                bus_for_job.emit_feature_complete(
                    "drop-survives-feat",
                    "run-drop-test",
                    serde_json::json!({"status": "passed"}),
                );
                Ok(VerificationManifest::new(
                    "drop-survives-feat",
                    &project_for_job,
                ))
            },
        )
        .expect("spawn background job");

    assert_eq!(ctx.jobs().active_count(), 1, "job must be live before test");

    // Start a minimal accept loop mirroring `lifecycle::run_unix`.
    let ctx_for_loop = Arc::clone(&ctx);
    let sock_path_for_loop = sock_path.clone();
    let accept_handle = tokio::spawn(async move {
        let socket_path = SocketPath::Unix(sock_path_for_loop.clone());
        let listener = match socket_path {
            SocketPath::Unix(ref p) => {
                pice_daemon::server::unix::UnixSocketListener::bind(p).await?
            }
            _ => unreachable!(),
        };
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
                                // Route subscribe methods to the subscribe handler
                                // (takes ownership of the connection for the
                                // subscription lifetime).
                                use pice_daemon::handlers::subscribe as sub_handler;
                                if sub_handler::is_subscribe_method(&req.method) {
                                    let _ = sub_handler::dispatch(&ctx, &mut conn, req).await;
                                    break; // subscribe handler owns the connection
                                }
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
        // Drain remaining jobs (mirrors lifecycle cleanup).
        let _ = ctx_for_loop
            .jobs()
            .drain_on_shutdown(Duration::from_secs(10))
            .await;
        Ok::<(), anyhow::Error>(())
    });

    wait_for_socket(&sock_path).await;

    // ── Step 3: open connection A, subscribe, get snapshot, DROP it ──────

    let stream_a = UnixStream::connect(&sock_path).await.expect("connect A");
    let mut conn_a = UnixConnection::new(stream_a);

    let params_a = serde_json::to_value(pice_core::protocol::subscribe::SubscribeManifestRequest {
        feature_id: Some("drop-survives-feat".to_string()),
    })
    .unwrap();
    let req_a = DaemonRequest::new(1, methods::MANIFEST_SUBSCRIBE, &token, params_a);
    conn_a
        .write_message(&req_a)
        .await
        .expect("write subscribe A");

    // Consume the snapshot response to confirm connection A is live.
    let snap_a: DaemonResponse = conn_a
        .read_message()
        .await
        .expect("read A")
        .expect("not EOF");
    assert!(
        snap_a.error.is_none(),
        "subscribe A snapshot must succeed, got: {:?}",
        snap_a.error
    );

    // Drop connection A — the client side is gone.
    drop(conn_a);

    // Give the scheduler a brief yield so the server-side EOF propagates.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // ── Step 4: assert the job is STILL running after the drop ──────────

    assert!(
        ctx.jobs().active_count() >= 1,
        "task MUST still be active after connection A drop — \
         dropping a connection MUST NOT cancel background tasks"
    );

    // ── Step 5: open connection B, subscribe (before unblocking job) ─────

    let stream_b = UnixStream::connect(&sock_path).await.expect("connect B");
    let mut conn_b = UnixConnection::new(stream_b);

    let params_b = serde_json::to_value(pice_core::protocol::subscribe::SubscribeManifestRequest {
        feature_id: Some("drop-survives-feat".to_string()),
    })
    .unwrap();
    let req_b = DaemonRequest::new(2, methods::MANIFEST_SUBSCRIBE, &token, params_b);
    conn_b
        .write_message(&req_b)
        .await
        .expect("write subscribe B");

    // Read the snapshot response from connection B (confirms it's set up).
    let snap_b: DaemonResponse = conn_b
        .read_message()
        .await
        .expect("read B snap")
        .expect("not EOF");
    assert!(
        snap_b.error.is_none(),
        "subscribe B snapshot must succeed, got: {:?}",
        snap_b.error
    );

    // ── Step 6: unblock the job ──────────────────────────────────────────

    unblock.notify_one();

    // ── Step 7: await FeatureComplete on connection B (event-driven) ─────
    //
    // The stream may deliver a LayerStarted notification first (emitted
    // before the unblock), then FeatureComplete. We scan until we see
    // FeatureComplete — no fixed sleep.
    let fc_payload =
        await_notification_event(&mut conn_b, methods::MANIFEST_EVENT, Duration::from_secs(5))
            .await;

    // Keep reading if this was LayerStarted rather than FeatureComplete.
    let terminal_payload = if fc_payload.event == ManifestEvent::FeatureComplete {
        fc_payload
    } else {
        // We got an intermediate event (LayerStarted); wait for the next one.
        await_notification_event(&mut conn_b, methods::MANIFEST_EVENT, Duration::from_secs(5)).await
    };

    assert_eq!(
        terminal_payload.event,
        ManifestEvent::FeatureComplete,
        "background task must emit FeatureComplete after unblock"
    );
    assert_eq!(terminal_payload.feature_id, "drop-survives-feat");

    drop(conn_b);

    // ── Step 8: assert the job completed (not still running) ─────────────
    //
    // The supervisor loop polls every 100ms; give it up to 1s to clean up.
    for _ in 0..20 {
        if ctx.jobs().active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        ctx.jobs().active_count(),
        0,
        "FeatureJobManager must show 0 active jobs after FeatureComplete — \
         the task ran to completion despite connection A being dropped"
    );

    // Shutdown the daemon gracefully.
    let stream_s = UnixStream::connect(&sock_path)
        .await
        .expect("connect shutdown");
    let mut conn_s = UnixConnection::new(stream_s);
    let shutdown_req =
        DaemonRequest::new(3, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn_s
        .write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _: DaemonResponse = conn_s
        .read_message()
        .await
        .expect("read shutdown resp")
        .expect("not EOF");
    drop(conn_s);

    let _ = tokio::time::timeout(Duration::from_secs(5), accept_handle).await;
}
