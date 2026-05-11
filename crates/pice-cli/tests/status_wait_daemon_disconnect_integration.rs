//! Phase 7 Criterion 7: binary-level wait disconnect path.
//!
//! This pins the real `pice status --wait --json` CLI behavior when the
//! daemon connection closes after a non-terminal subscribe snapshot. Earlier
//! coverage mirrored the private wait loop; this test exercises the adapter,
//! socket transport, subscribe reader, status command, and process exit code.

#![cfg(unix)]

use assert_cmd::Command;
use pice_core::cli::ExitJsonStatus;
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::protocol::methods::{DAEMON_HEALTH, MANIFEST_SUBSCRIBE};
use pice_core::protocol::subscribe::SubscribeManifestResponse;
use pice_core::protocol::{DaemonRequest, DaemonResponse};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};

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

#[test]
fn status_wait_json_exits_five_when_subscribe_connection_closes() {
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
            VerificationManifest::new("disconnect-feat", std::path::Path::new("/tmp/pice-test"));
        manifest.overall_status = ManifestStatus::InProgress;
        manifest.run_id = Some("r-disconnect".to_string());
        let response = SubscribeManifestResponse {
            snapshots: vec![manifest],
            run_ids: BTreeMap::from([("disconnect-feat".to_string(), "r-disconnect".to_string())]),
        };
        write_response(
            &mut stream,
            subscribe.id,
            serde_json::to_value(response).expect("serialize snapshot"),
        );
        // Drop the stream without a terminal notification. The CLI's
        // subscribe reader must translate EOF into daemon-disconnected.
    });

    let output = Command::cargo_bin("pice")
        .unwrap()
        .env("HOME", home.path())
        .env("PICE_DAEMON_SOCKET", &socket_path)
        .env_remove("PICE_DAEMON_INLINE")
        .args(["status", "--wait", "disconnect-feat", "--json"])
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
    let json: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("parse stdout: {e}; {stdout}"));
    assert_eq!(json["status"], ExitJsonStatus::DaemonDisconnected.as_str());
    assert_eq!(json["feature_id"], "disconnect-feat");
}
