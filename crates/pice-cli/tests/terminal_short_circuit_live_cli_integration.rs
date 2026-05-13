//! Phase 7 Criterion 19: real CLI late-subscribe short-circuit paths.
//!
//! Uses the compiled `pice` binary and a fake Unix-socket daemon that
//! speaks the daemon protocol. The daemon returns already-terminal
//! subscribe snapshots/history in the initial response body, and the CLI
//! must exit promptly without waiting for live notifications.

#![cfg(unix)]

use assert_cmd::Command;
use pice_core::cli::ExitJsonStatus;
use pice_core::events::{LogChunk, StreamJsonFrame};
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::protocol::methods::{DAEMON_HEALTH, LOGS_STREAM, MANIFEST_SUBSCRIBE};
use pice_core::protocol::subscribe::{LogsStreamResponse, SubscribeManifestResponse};
use pice_core::protocol::{DaemonRequest, DaemonResponse};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn socket_tempdir() -> tempfile::TempDir {
    if std::path::Path::new("/private/tmp").is_dir() {
        tempfile::tempdir_in("/private/tmp").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    }
}

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

fn prepare_home_and_socket() -> (tempfile::TempDir, std::path::PathBuf, UnixListener) {
    let home = socket_tempdir();
    let pice_dir = home.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();
    let socket_path = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    (home, socket_path, listener)
}

fn serve_terminal_manifest_subscribe(
    listener: UnixListener,
    feature_id: &'static str,
    sent_terminal_snapshot_at: mpsc::Sender<Instant>,
) {
    let (mut stream, _) = listener.accept().expect("accept client");

    let health = read_request(&stream);
    assert_eq!(health.auth, TOKEN);
    assert_eq!(health.method, DAEMON_HEALTH);
    write_response(
        &mut stream,
        health.id,
        serde_json::json!({
            "status": "ok",
            "version": "test",
            "uptime_seconds": 0,
        }),
    );

    let subscribe = read_request(&stream);
    assert_eq!(subscribe.auth, TOKEN);
    assert_eq!(subscribe.method, MANIFEST_SUBSCRIBE);
    assert_eq!(subscribe.params["feature_id"], feature_id);

    let mut manifest = VerificationManifest::new(feature_id, std::path::Path::new("/tmp/pice"));
    manifest.overall_status = ManifestStatus::Passed;
    manifest.run_id = Some("r-terminal".to_string());
    let response = SubscribeManifestResponse {
        snapshots: vec![manifest],
        run_ids: BTreeMap::from([(feature_id.to_string(), "r-terminal".to_string())]),
    };
    write_response(
        &mut stream,
        subscribe.id,
        serde_json::to_value(response).expect("serialize snapshot"),
    );
    sent_terminal_snapshot_at
        .send(Instant::now())
        .expect("send terminal snapshot timestamp");
}

fn serve_terminal_logs_stream(
    listener: UnixListener,
    feature_id: &'static str,
    sent_terminal_history_at: mpsc::Sender<Instant>,
) {
    let (mut stream, _) = listener.accept().expect("accept client");

    let health = read_request(&stream);
    assert_eq!(health.auth, TOKEN);
    assert_eq!(health.method, DAEMON_HEALTH);
    write_response(
        &mut stream,
        health.id,
        serde_json::json!({
            "status": "ok",
            "version": "test",
            "uptime_seconds": 0,
        }),
    );

    let subscribe = read_request(&stream);
    assert_eq!(subscribe.auth, TOKEN);
    assert_eq!(subscribe.method, LOGS_STREAM);
    assert_eq!(subscribe.params["feature_id"], feature_id);
    assert_eq!(subscribe.params["follow"], true);

    let response = LogsStreamResponse {
        run_id: "r-terminal".to_string(),
        history: vec![
            LogChunk {
                feature_id: feature_id.to_string(),
                run_id: "r-terminal".to_string(),
                layer: "backend".to_string(),
                text: "already done\n".to_string(),
                timestamp: "2026-05-11T12:00:00.000Z".to_string(),
                terminal: false,
                reason: None,
            },
            LogChunk {
                feature_id: feature_id.to_string(),
                run_id: "r-terminal".to_string(),
                layer: "".to_string(),
                text: "".to_string(),
                timestamp: "2026-05-11T12:00:01.000Z".to_string(),
                terminal: true,
                reason: Some("passed".to_string()),
            },
        ],
    };
    write_response(
        &mut stream,
        subscribe.id,
        serde_json::to_value(response).expect("serialize logs snapshot"),
    );
    sent_terminal_history_at
        .send(Instant::now())
        .expect("send terminal history timestamp");
}

fn serve_empty_logs_stream(listener: UnixListener, feature_id: &'static str) {
    let (mut stream, _) = listener.accept().expect("accept client");

    let health = read_request(&stream);
    assert_eq!(health.auth, TOKEN);
    assert_eq!(health.method, DAEMON_HEALTH);
    write_response(
        &mut stream,
        health.id,
        serde_json::json!({
            "status": "ok",
            "version": "test",
            "uptime_seconds": 0,
        }),
    );

    let subscribe = read_request(&stream);
    assert_eq!(subscribe.auth, TOKEN);
    assert_eq!(subscribe.method, LOGS_STREAM);
    assert_eq!(subscribe.params["feature_id"], feature_id);
    assert_eq!(subscribe.params["follow"], true);

    let response = LogsStreamResponse {
        run_id: String::new(),
        history: Vec::new(),
    };
    write_response(
        &mut stream,
        subscribe.id,
        serde_json::to_value(response).expect("serialize empty logs snapshot"),
    );
}

#[test]
fn status_follow_short_circuits_terminal_snapshot_within_500ms() {
    let (home, socket_path, listener) = prepare_home_and_socket();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        serve_terminal_manifest_subscribe(listener, "term-follow", tx);
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["status", "--follow", "term-follow", "--stream-json"])
        .output()
        .unwrap();
    let exited_at = Instant::now();

    server.join().expect("fake daemon thread");
    let response_sent_at = rx.recv().expect("terminal snapshot timestamp");
    let elapsed = exited_at.duration_since(response_sent_at);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        elapsed < Duration::from_millis(500),
        "terminal status follow should short-circuit under 500ms, got {elapsed:?}"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    assert_eq!(stdout.lines().count(), 2, "snapshot + terminal expected");
    assert!(matches!(
        serde_json::from_str::<StreamJsonFrame>(stdout.lines().last().unwrap()).unwrap(),
        StreamJsonFrame::Terminal {
            exit_code: 0,
            status: Some(ref status)
        } if status == "passed"
    ));
}

#[test]
fn status_wait_json_short_circuits_terminal_snapshot_within_500ms() {
    let (home, socket_path, listener) = prepare_home_and_socket();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        serve_terminal_manifest_subscribe(listener, "term-wait", tx);
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["status", "--wait", "term-wait", "--json"])
        .output()
        .unwrap();
    let exited_at = Instant::now();

    server.join().expect("fake daemon thread");
    let response_sent_at = rx.recv().expect("terminal snapshot timestamp");
    let elapsed = exited_at.duration_since(response_sent_at);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        elapsed < Duration::from_millis(500),
        "terminal status wait should short-circuit under 500ms, got {elapsed:?}"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], "passed");
    assert_eq!(json["feature_id"], "term-wait");
}

#[test]
fn logs_follow_short_circuits_terminal_history_within_500ms() {
    let (home, socket_path, listener) = prepare_home_and_socket();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        serve_terminal_logs_stream(listener, "term-logs", tx);
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["logs", "term-logs", "--follow"])
        .output()
        .unwrap();
    let exited_at = Instant::now();

    server.join().expect("fake daemon thread");
    let response_sent_at = rx.recv().expect("terminal history timestamp");
    let elapsed = exited_at.duration_since(response_sent_at);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        elapsed < Duration::from_millis(500),
        "terminal logs follow should short-circuit under 500ms, got {elapsed:?}"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    assert!(stdout.contains("already done"));
}

#[test]
fn logs_follow_stream_json_terminal_frame_carries_logs_stream_ended_status() {
    let (home, socket_path, listener) = prepare_home_and_socket();
    let (tx, _rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        serve_terminal_logs_stream(listener, "term-logs-json", tx);
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["logs", "term-logs-json", "--follow", "--stream-json"])
        .output()
        .unwrap();

    server.join().expect("fake daemon thread");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        3,
        "history chunk + terminal chunk + terminal frame"
    );
    let terminal: StreamJsonFrame = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert!(matches!(
        terminal,
        StreamJsonFrame::Terminal {
            exit_code: 0,
            status: Some(ref status)
        } if status == "logs-stream-ended"
    ));
}

#[test]
fn logs_follow_stream_json_exits_feature_not_found_on_empty_snapshot() {
    let (home, socket_path, listener) = prepare_home_and_socket();
    let server = std::thread::spawn(move || {
        serve_empty_logs_stream(listener, "missing-logs");
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["logs", "missing-logs", "--follow", "--stream-json"])
        .output()
        .unwrap();

    server.join().expect("fake daemon thread");
    assert_eq!(
        output.status.code(),
        Some(ExitJsonStatus::FeatureNotFound.exit_code()),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "feature-not-found terminal only");
    let terminal: StreamJsonFrame = serde_json::from_str(lines[0]).unwrap();
    assert!(matches!(
        terminal,
        StreamJsonFrame::Terminal {
            exit_code,
            status: Some(ref status)
        } if exit_code == ExitJsonStatus::FeatureNotFound.exit_code()
            && status == ExitJsonStatus::FeatureNotFound.as_str()
    ));
}
