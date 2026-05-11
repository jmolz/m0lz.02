//! Phase 7 live `pice status --follow --stream-json` integration coverage.
//!
//! This uses the real `pice` binary and a fake Unix-socket daemon that speaks
//! the daemon protocol. It verifies the CLI handles a live notification burst
//! over the transport, emits valid NDJSON frames, and closes with the correct
//! terminal exit code. The final `FeatureComplete` intentionally uses the
//! legacy `data.status` field so this also pins rolling-upgrade compatibility.

#![cfg(unix)]

use assert_cmd::Command;
use pice_core::cli::ExitJsonStatus;
use pice_core::events::{ManifestEvent, ManifestEventPayload, StreamJsonFrame};
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::protocol::methods::{DAEMON_HEALTH, MANIFEST_EVENT, MANIFEST_SUBSCRIBE};
use pice_core::protocol::subscribe::SubscribeManifestResponse;
use pice_core::protocol::{DaemonNotification, DaemonRequest, DaemonResponse};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
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
fn status_follow_stream_json_drains_live_burst_and_terminal_status_alias() {
    let home = tempfile::tempdir().unwrap();
    let pice_dir = home.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();

    let socket_path = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = std::thread::spawn(move || {
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

        let mut manifest =
            VerificationManifest::new("live-stream-feat", std::path::Path::new("/tmp/pice-test"));
        manifest.overall_status = ManifestStatus::InProgress;
        manifest.run_id = Some("r-live-stream".to_string());
        let response = SubscribeManifestResponse {
            snapshots: vec![manifest],
            run_ids: BTreeMap::from([(
                "live-stream-feat".to_string(),
                "r-live-stream".to_string(),
            )]),
        };
        write_response(
            &mut stream,
            subscribe.id,
            serde_json::to_value(response).expect("serialize snapshot"),
        );

        for i in 0..49 {
            write_notification(
                &mut stream,
                ManifestEventPayload {
                    feature_id: "live-stream-feat".to_string(),
                    run_id: "r-live-stream".to_string(),
                    event: ManifestEvent::LayerStarted,
                    layer: Some(format!("layer-{i:02}")),
                    data: serde_json::Value::Null,
                    timestamp: "2026-05-11T12:00:00.000Z".to_string(),
                },
            );
        }

        write_notification(
            &mut stream,
            ManifestEventPayload {
                feature_id: "live-stream-feat".to_string(),
                run_id: "r-live-stream".to_string(),
                event: ManifestEvent::FeatureComplete,
                layer: None,
                // Legacy daemon payload shape. The updated CLI must still
                // treat this as Passed instead of defaulting to Failed.
                data: serde_json::json!({"status": "passed"}),
                timestamp: "2026-05-11T12:00:01.000Z".to_string(),
            },
        );
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["status", "--follow", "live-stream-feat", "--stream-json"])
        .output()
        .unwrap();

    server.join().expect("fake daemon thread");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        52,
        "snapshot + 50 events + terminal frame expected; stdout={stdout}"
    );

    let first: StreamJsonFrame = serde_json::from_str(lines[0]).expect("snapshot frame");
    assert!(matches!(first, StreamJsonFrame::Snapshot { .. }));

    let event_frames = lines[1..51]
        .iter()
        .map(|line| serde_json::from_str::<StreamJsonFrame>(line).expect("event frame"))
        .collect::<Vec<_>>();
    assert_eq!(
        event_frames
            .iter()
            .filter(|frame| matches!(frame, StreamJsonFrame::Event { .. }))
            .count(),
        50
    );
    let layer_order = event_frames
        .iter()
        .filter_map(|frame| match frame {
            StreamJsonFrame::Event { event } if event.event == ManifestEvent::LayerStarted => {
                event.layer.clone()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected_order = (0..49).map(|i| format!("layer-{i:02}")).collect::<Vec<_>>();
    assert_eq!(
        layer_order, expected_order,
        "status --follow must preserve daemon event order for layer starts"
    );
    assert!(matches!(
        event_frames.last(),
        Some(StreamJsonFrame::Event { event }) if event.event == ManifestEvent::FeatureComplete
    ));

    let terminal: StreamJsonFrame = serde_json::from_str(lines[51]).expect("terminal frame");
    assert!(matches!(
        terminal,
        StreamJsonFrame::Terminal { exit_code: 0 }
    ));
}

#[test]
fn status_follow_stream_json_exits_five_on_disconnect_before_terminal() {
    let home = tempfile::tempdir().unwrap();
    let pice_dir = home.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();

    let socket_path = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");

        let health = read_request(&stream);
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
        let mut manifest =
            VerificationManifest::new("disconnect-follow", std::path::Path::new("/tmp/pice-test"));
        manifest.overall_status = ManifestStatus::InProgress;
        manifest.run_id = Some("r-disconnect-follow".to_string());
        let response = SubscribeManifestResponse {
            snapshots: vec![manifest],
            run_ids: BTreeMap::from([(
                "disconnect-follow".to_string(),
                "r-disconnect-follow".to_string(),
            )]),
        };
        write_response(
            &mut stream,
            subscribe.id,
            serde_json::to_value(response).expect("serialize snapshot"),
        );
        // Drop without terminal event. CLI should emit a terminal NDJSON frame
        // and return daemon-disconnected.
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["status", "--follow", "disconnect-follow", "--stream-json"])
        .output()
        .unwrap();

    server.join().expect("fake daemon thread");
    assert_eq!(
        output.status.code(),
        Some(ExitJsonStatus::DaemonDisconnected.exit_code()),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let terminal = stdout.lines().last().expect("terminal frame");
    let frame: StreamJsonFrame = serde_json::from_str(terminal).expect("parse terminal");
    assert!(matches!(
        frame,
        StreamJsonFrame::Terminal {
            exit_code
        } if exit_code == ExitJsonStatus::DaemonDisconnected.exit_code()
    ));
}

#[test]
fn status_follow_stream_json_sigint_emits_terminal_130() {
    let home = tempfile::tempdir().unwrap();
    let pice_dir = home.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();

    let socket_path = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");

        let health = read_request(&stream);
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
        let mut manifest =
            VerificationManifest::new("sigint-follow", std::path::Path::new("/tmp/pice-test"));
        manifest.overall_status = ManifestStatus::InProgress;
        manifest.run_id = Some("r-sigint-follow".to_string());
        let response = SubscribeManifestResponse {
            snapshots: vec![manifest],
            run_ids: BTreeMap::from([("sigint-follow".to_string(), "r-sigint-follow".to_string())]),
        };
        write_response(
            &mut stream,
            subscribe.id,
            serde_json::to_value(response).expect("serialize snapshot"),
        );
        ready_tx.send(()).expect("signal ready");

        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set timeout");
        let mut buf = [0_u8; 1];
        let _ = stream.read(&mut buf);
    });

    let bin = assert_cmd::cargo::cargo_bin("pice");
    let child = std::process::Command::new(bin)
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["status", "--follow", "sigint-follow", "--stream-json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn pice");

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("fake daemon ready");
    std::thread::sleep(Duration::from_millis(100));

    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(rc, 0, "failed to send SIGINT");

    let output = child.wait_with_output().expect("wait child");
    server.join().expect("fake daemon thread");

    assert_eq!(
        output.status.code(),
        Some(130),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let terminal = stdout.lines().last().expect("terminal frame");
    let frame: StreamJsonFrame = serde_json::from_str(terminal).expect("parse terminal");
    assert!(matches!(
        frame,
        StreamJsonFrame::Terminal { exit_code: 130 }
    ));
}
