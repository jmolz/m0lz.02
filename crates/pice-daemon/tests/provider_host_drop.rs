//! ProviderHost drop-path process-tree cleanup tests.

#[cfg(unix)]
mod unix {
    use pice_daemon::provider::host::ProviderHost;
    use serde::Deserialize;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[derive(Deserialize)]
    struct Pids {
        provider: i32,
        child: i32,
    }

    #[tokio::test]
    async fn dropping_provider_host_kills_stubborn_child_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("pids.json");
        let script = dir.path().join("provider-with-child.mjs");
        std::fs::write(
            &script,
            r#"
import { spawn } from 'node:child_process';
import { writeFileSync } from 'node:fs';

const marker = process.argv[2];
const child = spawn(process.execPath, ['-e', `
process.on('SIGTERM', () => {});
setInterval(() => undefined, 1000);
`], { stdio: 'ignore' });

writeFileSync(marker, JSON.stringify({ provider: process.pid, child: child.pid }));
setInterval(() => undefined, 1000);
"#,
        )
        .unwrap();

        let host = ProviderHost::spawn(
            "node",
            &[script.to_str().unwrap(), marker.to_str().unwrap()],
        )
        .await
        .expect("spawn provider host");
        let pids = read_pids(&marker).await;
        assert!(
            pid_alive(pids.provider),
            "provider process should be alive before drop"
        );
        assert!(
            pid_alive(pids.child),
            "child process should be alive before drop"
        );

        drop(host);

        wait_for_dead(pids.provider, Duration::from_secs(3)).await;
        wait_for_dead(pids.child, Duration::from_secs(3)).await;
    }

    async fn read_pids(path: &Path) -> Pids {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(pids) = serde_json::from_str::<Pids>(&text) {
                    return pids;
                }
            }
            assert!(
                Instant::now() < deadline,
                "provider did not write PID marker"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_for_dead(pid: i32, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while pid_alive(pid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(!pid_alive(pid), "pid {pid} survived ProviderHost drop");
    }

    fn pid_alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }
}
