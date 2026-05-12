//! Phase 7 Criterion 5: `pice status --follow` on a TTY preserves DAG event
//! order and closes its subscribe socket cleanly on SIGINT.

#![cfg(unix)]

use pice_core::events::{ManifestEvent, ManifestEventPayload};
use pice_core::layers::manifest::{LayerResult, LayerStatus, ManifestStatus, VerificationManifest};
use pice_core::protocol::methods::{DAEMON_HEALTH, MANIFEST_EVENT, MANIFEST_SUBSCRIBE};
use pice_core::protocol::subscribe::SubscribeManifestResponse;
use pice_core::protocol::{DaemonNotification, DaemonRequest, DaemonResponse};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Stdio};
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
    writeln!(stream, "{}", serde_json::to_string(&response).unwrap()).unwrap();
    stream.flush().unwrap();
}

fn write_notification(stream: &mut UnixStream, payload: ManifestEventPayload) {
    let notification =
        DaemonNotification::new(MANIFEST_EVENT, serde_json::to_value(payload).unwrap());
    writeln!(stream, "{}", serde_json::to_string(&notification).unwrap()).unwrap();
    stream.flush().unwrap();
}

fn manifest_with_layers(feature_id: &str, run_id: &str, layers: &[&str]) -> VerificationManifest {
    let mut manifest = VerificationManifest::new(feature_id, std::path::Path::new("/tmp/pice"));
    manifest.overall_status = ManifestStatus::InProgress;
    manifest.run_id = Some(run_id.to_string());
    manifest.layers = layers
        .iter()
        .map(|name| LayerResult {
            name: (*name).to_string(),
            status: LayerStatus::Pending,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: None,
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        })
        .collect();
    manifest
}

fn open_pty() -> (File, File) {
    let mut master = -1;
    let mut slave = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}

fn spawn_pice_status_follow_on_pty(
    home: &std::path::Path,
    socket_path: &std::path::Path,
    feature_id: &str,
) -> (Child, File) {
    let (master, slave) = open_pty();
    let stdin = slave.try_clone().unwrap();
    let stdout = slave.try_clone().unwrap();
    let stderr = slave;
    let bin = assert_cmd::cargo::cargo_bin("pice");
    let child = std::process::Command::new(bin)
        .env("HOME", home)
        .env("PICE_DAEMON_SOCKET", socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["status", "--follow", feature_id])
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn pice status --follow");
    (child, master)
}

fn read_pty_to_string(mut master: File) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) => panic!("read pty output: {e}"),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn status_follow_pty_preserves_manifest_dag_order() {
    let home = tempfile::tempdir().unwrap();
    let pice_dir = home.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();

    let socket_path = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");

        let health = read_request(&stream);
        assert_eq!(health.method, DAEMON_HEALTH);
        write_response(
            &mut stream,
            health.id,
            serde_json::json!({"status":"ok","version":"test","uptime_seconds":0}),
        );

        let subscribe = read_request(&stream);
        assert_eq!(subscribe.method, MANIFEST_SUBSCRIBE);
        let layers = ["database", "api", "frontend"];
        let response = SubscribeManifestResponse {
            snapshots: vec![manifest_with_layers("pty-dag", "run-pty-dag", &layers)],
            run_ids: BTreeMap::from([("pty-dag".to_string(), "run-pty-dag".to_string())]),
        };
        write_response(
            &mut stream,
            subscribe.id,
            serde_json::to_value(response).unwrap(),
        );

        for layer in layers {
            write_notification(
                &mut stream,
                ManifestEventPayload {
                    feature_id: "pty-dag".to_string(),
                    run_id: "run-pty-dag".to_string(),
                    event: ManifestEvent::LayerStarted,
                    layer: Some(layer.to_string()),
                    data: serde_json::Value::Null,
                    timestamp: "2026-05-11T12:00:00.000Z".to_string(),
                },
            );
        }
        write_notification(
            &mut stream,
            ManifestEventPayload {
                feature_id: "pty-dag".to_string(),
                run_id: "run-pty-dag".to_string(),
                event: ManifestEvent::FeatureComplete,
                layer: None,
                data: serde_json::json!({"overall_status":"passed","status":"passed"}),
                timestamp: "2026-05-11T12:00:01.000Z".to_string(),
            },
        );
    });

    let (mut child, master) = spawn_pice_status_follow_on_pty(home.path(), &socket_path, "pty-dag");
    let reader = std::thread::spawn(move || read_pty_to_string(master));
    let status = child.wait().expect("wait child");
    server.join().expect("fake daemon thread");
    let output = reader.join().expect("pty reader thread");

    assert_eq!(status.code(), Some(0), "pty output:\n{output}");
    let db = output
        .find("layer=database")
        .unwrap_or_else(|| panic!("database event missing; output:\n{output}"));
    let api = output
        .find("layer=api")
        .unwrap_or_else(|| panic!("api event missing; output:\n{output}"));
    let frontend = output
        .find("layer=frontend")
        .unwrap_or_else(|| panic!("frontend event missing; output:\n{output}"));
    assert!(
        db < api && api < frontend,
        "status --follow must preserve manifest DAG order; output:\n{output}"
    );
}

#[test]
fn status_follow_pty_sigint_closes_socket_and_exits_130() {
    let home = tempfile::tempdir().unwrap();
    let pice_dir = home.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(pice_dir.join("daemon.token"), TOKEN).unwrap();

    let socket_path = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");

        let health = read_request(&stream);
        write_response(
            &mut stream,
            health.id,
            serde_json::json!({"status":"ok","version":"test","uptime_seconds":0}),
        );

        let subscribe = read_request(&stream);
        let response = SubscribeManifestResponse {
            snapshots: vec![manifest_with_layers(
                "pty-sigint",
                "run-pty-sigint",
                &["api"],
            )],
            run_ids: BTreeMap::from([("pty-sigint".to_string(), "run-pty-sigint".to_string())]),
        };
        write_response(
            &mut stream,
            subscribe.id,
            serde_json::to_value(response).unwrap(),
        );
        ready_tx.send(()).unwrap();

        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut buf = [0_u8; 1];
        let closed = matches!(stream.read(&mut buf), Ok(0));
        closed_tx.send(closed).unwrap();
    });

    let (child, master) = spawn_pice_status_follow_on_pty(home.path(), &socket_path, "pty-sigint");
    let reader = std::thread::spawn(move || read_pty_to_string(master));
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("fake daemon ready");
    std::thread::sleep(Duration::from_millis(100));

    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(rc, 0, "send SIGINT");
    let output = child.wait_with_output().expect("wait child");
    let _pty_output = reader.join().expect("pty reader thread");
    server.join().expect("fake daemon thread");

    assert_eq!(output.status.code(), Some(130));
    assert!(
        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        "SIGINT path must close the subscribe socket before exiting"
    );
}
