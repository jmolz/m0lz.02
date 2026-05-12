//! Phase 7 Criterion 7: real CLI `pice evaluate --background --wait` path.
//!
//! The fake daemon speaks the wire protocol, but the tested path is the real
//! binary: clap parsing, background dispatch, second subscribe connection,
//! terminal notification parsing, JSON rendering, and process exit code.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use assert_cmd::Command;
use pice_core::cli::{CommandResponse, ExitJsonStatus};
use pice_core::events::{ManifestEvent, ManifestEventPayload};
use pice_core::layers::manifest::{LayerResult, LayerStatus, ManifestStatus, VerificationManifest};
use pice_core::protocol::methods::{
    CLI_DISPATCH, DAEMON_HEALTH, MANIFEST_EVENT, MANIFEST_SUBSCRIBE,
};
use pice_core::protocol::subscribe::SubscribeManifestResponse;
use pice_core::protocol::{DaemonNotification, DaemonRequest, DaemonResponse};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Output, Stdio};
use std::time::Duration;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn read_request(stream: &UnixStream) -> DaemonRequest {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read request line");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("parse request: {e}; line={line:?}"))
}

fn write_response(stream: &mut UnixStream, id: u64, result: serde_json::Value) {
    let response = DaemonResponse::success(id, result);
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&response).expect("serialize response")
    )
    .expect("write response");
    stream.flush().expect("flush response");
}

fn write_notification(stream: &mut UnixStream, payload: ManifestEventPayload) {
    let notification = DaemonNotification::new(
        MANIFEST_EVENT,
        serde_json::to_value(payload).expect("serialize payload"),
    );
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&notification).expect("serialize notification")
    )
    .expect("write notification");
    stream.flush().expect("flush notification");
}

#[test]
fn evaluate_background_wait_json_uses_second_subscribe_until_feature_complete() {
    let output = run_fake_evaluate_background_wait(Some("passed"), None, false);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("parse stdout: {e}; {stdout}"));
    assert_eq!(json["status"], "passed");
    assert_eq!(json["feature_id"], "eval-wait-feat");
    assert_eq!(json["run_id"], "r-eval-wait");
}

#[test]
fn evaluate_background_wait_json_exits_two_on_failed_feature_complete() {
    let output = run_fake_evaluate_background_wait(Some("failed"), None, false);
    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], "failed");
}

#[test]
fn evaluate_background_wait_json_exits_three_on_pending_review_feature_complete() {
    let output = run_fake_evaluate_background_wait(Some("pending-review"), None, false);
    assert_eq!(output.status.code(), Some(3));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], "pending-review");
}

#[test]
fn evaluate_background_wait_json_exits_four_on_timeout() {
    let output = run_fake_evaluate_background_wait(None, Some("1"), false);
    assert_eq!(output.status.code(), Some(4));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], ExitJsonStatus::WaitTimeout.as_str());
}

#[test]
fn evaluate_background_wait_json_exits_five_on_subscribe_disconnect() {
    let output = run_fake_evaluate_background_wait(None, None, true);
    assert_eq!(output.status.code(), Some(5));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], ExitJsonStatus::DaemonDisconnected.as_str());
}

#[test]
fn evaluate_background_wait_disconnect_then_restart_reconciles_failed_interrupted() {
    let home = tempfile::tempdir_in("/private/tmp").unwrap();
    let state_dir = home.path().join("state");
    let manifest_path = seed_in_progress_wait_manifest(&state_dir, home.path());

    let output = run_fake_evaluate_background_wait_in_home(home.path(), None, None, true);
    assert_eq!(output.status.code(), Some(5));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], ExitJsonStatus::DaemonDisconnected.as_str());

    let report = pice_daemon::jobs::reconcile_on_startup(&state_dir).expect("reconcile");
    assert_eq!(
        report.reconciled_interrupted,
        vec!["eval-wait-feat".to_string()]
    );
    let reconciled = VerificationManifest::load(&manifest_path).expect("load reconciled");
    assert_eq!(reconciled.overall_status, ManifestStatus::Failed);
    assert_eq!(
        reconciled
            .layers
            .first()
            .and_then(|layer| layer.halted_by.as_deref()),
        Some("failed-interrupted")
    );
}

#[test]
fn evaluate_background_wait_real_daemon_kill_then_restart_reconciles_failed_interrupted() {
    let home = tempfile::tempdir_in("/private/tmp").unwrap();
    let state_dir = home.path().join("state");
    let project = tempfile::tempdir_in("/private/tmp").unwrap();
    init_real_wait_project(project.path());
    let plan_path = write_real_wait_plan(project.path(), "real-restart-wait");
    let socket_path = home.path().join("daemon.sock");

    let mut daemon = spawn_real_daemon(home.path(), project.path(), &socket_path, &state_dir);
    wait_for_real_socket(&socket_path, &mut daemon);
    wait_for_real_health(home.path(), &socket_path, &mut daemon);

    let mut cli = std::process::Command::cargo_bin("pice")
        .unwrap()
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env("PICE_STATE_DIR", &state_dir)
        .env_remove("PICE_DAEMON_INLINE")
        .args([
            "evaluate",
            plan_path.to_str().unwrap(),
            "--background",
            "--wait",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pice evaluate --background --wait");

    let manifest_path = wait_for_real_manifest_status(
        &state_dir,
        project.path(),
        "real-restart-wait",
        &mut daemon,
        &mut cli,
        |manifest| {
            manifest.overall_status == ManifestStatus::InProgress
                && manifest
                    .layers
                    .iter()
                    .any(|layer| layer.name == "backend" && layer.status == LayerStatus::InProgress)
        },
    );

    daemon.kill().expect("kill daemon mid-wait");
    let _ = daemon.wait();

    let output = wait_child_output(&mut cli, Duration::from_secs(8));
    assert_eq!(
        output.status.code(),
        Some(ExitJsonStatus::DaemonDisconnected.exit_code()),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("parse stdout: {e}; {stdout}"));
    assert_eq!(json["status"], ExitJsonStatus::DaemonDisconnected.as_str());
    assert_eq!(json["feature_id"], "real-restart-wait");

    let mut restarted = spawn_real_daemon(home.path(), project.path(), &socket_path, &state_dir);
    wait_for_real_socket(&socket_path, &mut restarted);
    wait_for_real_health(home.path(), &socket_path, &mut restarted);

    let reconciled = VerificationManifest::load(&manifest_path).expect("load reconciled manifest");
    assert_eq!(reconciled.overall_status, ManifestStatus::Failed);
    assert_eq!(
        reconciled
            .layers
            .iter()
            .find(|layer| layer.name == "backend")
            .and_then(|layer| layer.halted_by.as_deref()),
        Some("failed-interrupted")
    );

    send_real_shutdown(home.path(), &socket_path);
    let _ = restarted.wait();
}

fn seed_in_progress_wait_manifest(
    state_dir: &std::path::Path,
    project_root: &std::path::Path,
) -> std::path::PathBuf {
    let path =
        VerificationManifest::manifest_path_in_state_dir("eval-wait-feat", project_root, state_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create manifest parent");
    }
    let mut manifest = VerificationManifest::new("eval-wait-feat", project_root);
    manifest.overall_status = ManifestStatus::InProgress;
    manifest.run_id = Some("r-eval-wait".to_string());
    manifest.layers.push(LayerResult {
        name: "backend".to_string(),
        status: LayerStatus::InProgress,
        passes: Vec::new(),
        seam_checks: Vec::new(),
        halted_by: None,
        final_confidence: None,
        total_cost_usd: None,
        escalation_events: None,
    });
    manifest.save(&path).expect("save in-progress manifest");
    path
}

fn init_real_wait_project(root: &std::path::Path) {
    std::fs::create_dir_all(root.join(".pice")).unwrap();
    std::fs::write(
        root.join(".pice/config.toml"),
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
    std::fs::write(
        root.join(".pice/workflow.yaml"),
        r#"schema_version: "0.2"
defaults:
  tier: 1
  min_confidence: 0.50
  max_passes: 1
  model: stub-model
  budget_usd: 2.0
  cost_cap_behavior: halt
  max_parallelism: 1
  max_global_provider_concurrency: 1
phases:
  evaluate:
    parallel: false
"#,
    )
    .unwrap();
    std::fs::write(
        root.join(".pice/layers.toml"),
        r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/lib.rs"]
always_run = true
"#,
    )
    .unwrap();
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
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
    std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
}

fn write_real_wait_plan(root: &std::path::Path, feature_id: &str) -> std::path::PathBuf {
    let plans = root.join(".claude/plans");
    std::fs::create_dir_all(&plans).unwrap();
    let path = plans.join(format!("{feature_id}.md"));
    std::fs::write(
        &path,
        format!(
            r#"# Plan

## Contract

```json
{{
  "feature": "{feature_id}",
  "tier": 1,
  "pass_threshold": 8,
  "criteria": [
    {{"name": "works", "threshold": 8, "validation": "manual"}}
  ]
}}
```
"#
        ),
    )
    .unwrap();
    path
}

fn spawn_real_daemon(
    home: &std::path::Path,
    project: &std::path::Path,
    socket_path: &std::path::Path,
    state_dir: &std::path::Path,
) -> Child {
    std::process::Command::cargo_bin("pice-daemon")
        .unwrap()
        .current_dir(project)
        .env("HOME", home)
        .env("PICE_DAEMON_SOCKET", socket_path)
        .env("PICE_STATE_DIR", state_dir)
        .env("PICE_STUB_SCORES", "9.5,0.001")
        .env("PICE_STUB_LATENCY_MS", "5000")
        .env_remove("PICE_DAEMON_INLINE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pice-daemon")
}

fn wait_for_real_socket(socket_path: &std::path::Path, daemon: &mut Child) {
    for _ in 0..300 {
        if socket_path.exists() && UnixStream::connect(socket_path).is_ok() {
            return;
        }
        if let Some(status) = daemon.try_wait().expect("poll daemon") {
            let mut stderr = String::new();
            if let Some(mut err) = daemon.stderr.take() {
                err.read_to_string(&mut stderr).expect("read daemon stderr");
            }
            panic!("daemon exited before socket bind: {status}; stderr:\n{stderr}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "daemon socket did not become ready at {}",
        socket_path.display()
    );
}

fn wait_for_real_health(home: &std::path::Path, socket_path: &std::path::Path, daemon: &mut Child) {
    let token_path = home.join(".pice/daemon.token");
    for _ in 0..300 {
        if let Ok(token) = std::fs::read_to_string(&token_path) {
            if let Ok(mut stream) = UnixStream::connect(socket_path) {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                let request =
                    DaemonRequest::new(1, DAEMON_HEALTH, token.trim(), serde_json::json!({}));
                if writeln!(stream, "{}", serde_json::to_string(&request).unwrap()).is_ok() {
                    let mut reader =
                        BufReader::new(stream.try_clone().expect("clone health stream"));
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                        if let Ok(response) = serde_json::from_str::<DaemonResponse>(&line) {
                            if response.error.is_none() {
                                return;
                            }
                        }
                    }
                }
            }
        }
        if let Some(status) = daemon.try_wait().expect("poll daemon") {
            let mut stderr = String::new();
            if let Some(mut err) = daemon.stderr.take() {
                err.read_to_string(&mut stderr).expect("read daemon stderr");
            }
            panic!("daemon exited before health response: {status}; stderr:\n{stderr}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "daemon did not become health-responsive at {}",
        socket_path.display()
    );
}

fn wait_for_real_manifest_status(
    state_dir: &std::path::Path,
    project_root: &std::path::Path,
    feature_id: &str,
    daemon: &mut Child,
    cli: &mut Child,
    pred: impl Fn(&VerificationManifest) -> bool,
) -> std::path::PathBuf {
    let path =
        VerificationManifest::manifest_path_in_state_dir(feature_id, project_root, state_dir);
    for _ in 0..500 {
        if let Ok(manifest) = VerificationManifest::load(&path) {
            if pred(&manifest) {
                return path;
            }
        }
        if let Some(status) = daemon.try_wait().expect("poll daemon") {
            let mut stderr = String::new();
            if let Some(mut err) = daemon.stderr.take() {
                err.read_to_string(&mut stderr).expect("read daemon stderr");
            }
            panic!("daemon exited before manifest reached expected status: {status}; stderr:\n{stderr}");
        }
        if let Some(status) = cli.try_wait().expect("poll cli") {
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut out) = cli.stdout.take() {
                out.read_to_string(&mut stdout).expect("read cli stdout");
            }
            if let Some(mut err) = cli.stderr.take() {
                err.read_to_string(&mut stderr).expect("read cli stderr");
            }
            let manifest = VerificationManifest::load(&path)
                .map(|m| format!("{m:#?}"))
                .unwrap_or_else(|e| format!("unloadable manifest: {e:#}"));
            panic!("pice evaluate exited before manifest reached expected status: {status}; stdout:\n{stdout}\nstderr:\n{stderr}\nmanifest:\n{manifest}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "manifest did not reach expected status at {}",
        path.display()
    );
}

fn wait_child_output(child: &mut Child, timeout: Duration) -> Output {
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut stdout).expect("read stdout");
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_end(&mut stderr).expect("read stderr");
    }
    Output {
        status,
        stdout,
        stderr,
    }
}

fn send_real_shutdown(home: &std::path::Path, socket_path: &std::path::Path) {
    let token = std::fs::read_to_string(home.join(".pice/daemon.token"))
        .expect("read daemon token")
        .trim()
        .to_string();
    let mut stream = UnixStream::connect(socket_path).expect("connect shutdown");
    let request = DaemonRequest::new(
        99,
        pice_core::protocol::methods::DAEMON_SHUTDOWN,
        &token,
        serde_json::json!({}),
    );
    writeln!(stream, "{}", serde_json::to_string(&request).unwrap()).expect("write shutdown");
    stream.flush().expect("flush shutdown");
    let mut reader = BufReader::new(stream.try_clone().expect("clone shutdown stream"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read shutdown response");
    let response: DaemonResponse = serde_json::from_str(&line).expect("parse shutdown response");
    assert!(
        response.error.is_none(),
        "shutdown failed: {:?}",
        response.error
    );
}

fn run_fake_evaluate_background_wait(
    terminal_status: Option<&'static str>,
    timeout_secs: Option<&'static str>,
    close_after_snapshot: bool,
) -> std::process::Output {
    let home = tempfile::tempdir_in("/private/tmp").unwrap();
    run_fake_evaluate_background_wait_in_home(
        home.path(),
        terminal_status,
        timeout_secs,
        close_after_snapshot,
    )
}

fn run_fake_evaluate_background_wait_in_home(
    home: &std::path::Path,
    terminal_status: Option<&'static str>,
    timeout_secs: Option<&'static str>,
    close_after_snapshot: bool,
) -> std::process::Output {
    let pice_dir = home.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();
    let plan_path = home.join("plan.md");
    std::fs::write(&plan_path, "# Plan\n\n## Contract\n\n```json\n{}\n```\n").unwrap();

    let socket_path = home.join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut dispatch_stream, _) = listener.accept().expect("accept dispatch client");

        let health = read_request(&dispatch_stream);
        assert_eq!(health.auth, TOKEN);
        assert_eq!(health.method, DAEMON_HEALTH);
        write_response(
            &mut dispatch_stream,
            health.id,
            serde_json::json!({
                "status": "ok",
                "version": "test",
                "uptime_seconds": 0,
            }),
        );

        let dispatch = read_request(&dispatch_stream);
        assert_eq!(dispatch.auth, TOKEN);
        assert_eq!(dispatch.method, CLI_DISPATCH);
        assert_eq!(dispatch.params["command"], "evaluate");
        assert_eq!(dispatch.params["background"], true);
        assert_eq!(dispatch.params["wait"], true);
        let response = CommandResponse::Json {
            value: serde_json::json!({
                "status": ExitJsonStatus::BackgroundDispatched.as_str(),
                "feature_id": "eval-wait-feat",
                "run_id": "r-eval-wait",
            }),
        };
        write_response(
            &mut dispatch_stream,
            dispatch.id,
            serde_json::to_value(response).expect("serialize command response"),
        );
        drop(dispatch_stream);

        let (mut subscribe_stream, _) = listener.accept().expect("accept subscribe client");
        let health = read_request(&subscribe_stream);
        assert_eq!(health.auth, TOKEN);
        assert_eq!(health.method, DAEMON_HEALTH);
        write_response(
            &mut subscribe_stream,
            health.id,
            serde_json::json!({
                "status": "ok",
                "version": "test",
                "uptime_seconds": 0,
            }),
        );

        let subscribe = read_request(&subscribe_stream);
        assert_eq!(subscribe.auth, TOKEN);
        assert_eq!(subscribe.method, MANIFEST_SUBSCRIBE);
        assert_eq!(subscribe.params["feature_id"], "eval-wait-feat");

        let mut manifest =
            VerificationManifest::new("eval-wait-feat", std::path::Path::new("/tmp/pice-test"));
        manifest.overall_status = ManifestStatus::InProgress;
        manifest.run_id = Some("r-eval-wait".to_string());
        let snapshot = SubscribeManifestResponse {
            snapshots: vec![manifest],
            run_ids: BTreeMap::from([("eval-wait-feat".to_string(), "r-eval-wait".to_string())]),
        };
        write_response(
            &mut subscribe_stream,
            subscribe.id,
            serde_json::to_value(snapshot).expect("serialize snapshot"),
        );

        if close_after_snapshot {
            return;
        }

        if let Some(status) = terminal_status {
            write_notification(
                &mut subscribe_stream,
                ManifestEventPayload {
                    feature_id: "eval-wait-feat".to_string(),
                    run_id: "r-eval-wait".to_string(),
                    event: ManifestEvent::FeatureComplete,
                    layer: None,
                    data: serde_json::json!({"overall_status": status}),
                    timestamp: "2026-05-11T12:00:00.000Z".to_string(),
                },
            );
        } else {
            std::thread::sleep(Duration::from_millis(1500));
        }
    });

    let mut cmd = Command::cargo_bin("pice").unwrap();
    cmd.env("HOME", home)
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args([
            "evaluate",
            plan_path.to_str().unwrap(),
            "--background",
            "--wait",
            "--json",
        ]);
    if let Some(timeout_secs) = timeout_secs {
        cmd.args(["--timeout-secs", timeout_secs]);
    }
    let output = cmd.output().unwrap();

    server.join().expect("fake daemon thread");
    output
}
