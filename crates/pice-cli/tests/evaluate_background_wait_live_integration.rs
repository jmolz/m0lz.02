//! Phase 7 Criterion 7: real CLI `pice evaluate --background --wait` path.
//!
//! The fake daemon speaks the wire protocol, but the tested path is the real
//! binary: clap parsing, background dispatch, second subscribe connection,
//! terminal notification parsing, JSON rendering, and process exit code.

#![cfg(unix)]

use assert_cmd::Command;
use pice_core::cli::{CommandResponse, ExitJsonStatus};
use pice_core::events::{ManifestEvent, ManifestEventPayload};
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::protocol::methods::{
    CLI_DISPATCH, DAEMON_HEALTH, MANIFEST_EVENT, MANIFEST_SUBSCRIBE,
};
use pice_core::protocol::subscribe::SubscribeManifestResponse;
use pice_core::protocol::{DaemonNotification, DaemonRequest, DaemonResponse};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
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

fn run_fake_evaluate_background_wait(
    terminal_status: Option<&'static str>,
    timeout_secs: Option<&'static str>,
    close_after_snapshot: bool,
) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    let pice_dir = home.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();
    let plan_path = home.path().join("plan.md");
    std::fs::write(&plan_path, "# Plan\n\n## Contract\n\n```json\n{}\n```\n").unwrap();

    let socket_path = home.path().join("daemon.sock");
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
    cmd.env("HOME", home.path())
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
