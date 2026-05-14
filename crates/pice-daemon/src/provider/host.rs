use anyhow::{bail, Context, Result};
use pice_protocol::{
    JsonRpcErrorResponse, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, trace, warn};

/// Callback for provider notifications received during request/response cycles.
pub type NotificationHandler = Box<dyn Fn(String, Option<Value>) + Send>;

/// Manages a single provider process lifecycle and JSON-RPC communication.
pub struct ProviderHost {
    child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    notification_handler: Option<NotificationHandler>,
}

impl ProviderHost {
    /// Spawn a provider process with piped stdin/stdout and inherited stderr.
    ///
    /// `kill_on_drop(true)` and this type's `Drop` impl are load-bearing for
    /// cancellation: if a task is aborted while the provider is supervising a
    /// CLI child, dropping `ProviderHost` must terminate both the provider and
    /// its process group rather than leaving the child running.
    pub async fn spawn(command: &str, args: &[&str]) -> Result<Self> {
        debug!(command, ?args, "spawning provider process");
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn provider: {command}"))?;

        let stdin = child
            .stdin
            .take()
            .context("failed to capture provider stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture provider stdout")?;
        let reader = BufReader::new(stdout);

        Ok(Self {
            child,
            stdin: Some(stdin),
            reader,
            next_id: 1,
            notification_handler: None,
        })
    }

    /// Set a handler for notifications received from the provider.
    /// Called with (method, params) for each notification during request/response cycles.
    pub fn on_notification(&mut self, handler: NotificationHandler) {
        self.notification_handler = Some(handler);
    }

    /// Send a JSON-RPC request and wait for a response with a timeout.
    pub async fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        let id = RequestId::Number(self.next_id);
        self.next_id += 1;

        let request = JsonRpcRequest::new(id.clone(), method, params);
        self.send_message(&request).await?;

        let result = tokio::time::timeout(timeout, self.read_response(id)).await;
        match result {
            Ok(inner) => inner,
            Err(_) => bail!(
                "provider request timed out after {}ms: {method}",
                timeout.as_millis()
            ),
        }
    }

    /// Send a JSON-RPC notification (fire-and-forget, no response expected).
    /// Used by interactive session commands in Phase 3+.
    #[allow(dead_code)]
    pub async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = JsonRpcNotification::new(method, params);
        let json = serde_json::to_string(&notification)?;
        trace!(method, "sending notification");
        let stdin = self
            .stdin
            .as_mut()
            .context("provider stdin already closed")?;
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Gracefully shutdown the provider. The entire shutdown sequence
    /// (RPC + process exit wait) is bounded by `timeout`.
    pub async fn shutdown(mut self, timeout: Duration) -> Result<()> {
        debug!("shutting down provider");

        let deadline = tokio::time::Instant::now() + timeout;

        // Send shutdown request within the overall timeout budget
        let rpc_timeout = timeout.min(Duration::from_secs(5));
        let shutdown_result = self.request("shutdown", None, rpc_timeout).await;
        if let Err(e) = shutdown_result {
            warn!("shutdown request failed (provider may have already exited): {e}");
        }

        // Close stdin to signal EOF
        drop(self.stdin.take());

        // Wait for process to exit with remaining budget
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let exit = tokio::time::timeout(remaining, self.child.wait()).await;
        match exit {
            Ok(Ok(status)) => {
                debug!(?status, "provider exited");
                Ok(())
            }
            Ok(Err(e)) => {
                warn!("error waiting for provider exit: {e}");
                Ok(())
            }
            Err(_) => {
                warn!("provider did not exit within timeout, killing process tree");
                terminate_child_process_tree(&mut self.child).await;
                Ok(())
            }
        }
    }

    async fn send_message<T: serde::Serialize>(&mut self, message: &T) -> Result<()> {
        let json = serde_json::to_string(message)?;
        trace!(json = %json, "sending message");
        let stdin = self
            .stdin
            .as_mut()
            .context("provider stdin already closed")?;
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_response(&mut self, expected_id: RequestId) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = self.reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                bail!("provider process exited unexpectedly (EOF)");
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            trace!(raw = %trimmed, "received message from provider");

            // Try to parse as a success response
            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
                if resp.id == expected_id {
                    return Ok(resp.result);
                }
                debug!(id = ?resp.id, expected = ?expected_id, "ignoring response with different id");
                continue;
            }

            // Try to parse as an error response
            if let Ok(err_resp) = serde_json::from_str::<JsonRpcErrorResponse>(trimmed) {
                if err_resp.id.as_ref() == Some(&expected_id) {
                    bail!(
                        "provider returned error {}: {}",
                        err_resp.error.code,
                        err_resp.error.message
                    );
                }
                continue;
            }

            // Notification — forward to handler if registered
            if let Ok(notif) = serde_json::from_str::<JsonRpcNotification>(trimmed) {
                debug!(method = %notif.method, "received notification from provider");
                if let Some(handler) = &self.notification_handler {
                    handler(notif.method, notif.params);
                }
                continue;
            }

            warn!(line = %trimmed, "unparseable message from provider");
        }
    }
}

impl Drop for ProviderHost {
    fn drop(&mut self) {
        terminate_child_process_tree_on_drop(&mut self.child);
    }
}

#[cfg(unix)]
async fn terminate_child_process_tree(child: &mut Child) {
    let Some(pid) = child.id() else {
        let _ = child.kill().await;
        return;
    };

    let pgid = -(pid as libc::pid_t);
    // Providers are spawned into their own process group. Terminating the
    // group catches CLI subprocesses that the provider is supervising.
    unsafe {
        libc::kill(pgid, libc::SIGTERM);
    }
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_ok()
    {
        return;
    }

    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
    let _ = child.wait().await;
}

#[cfg(unix)]
fn terminate_child_process_tree_on_drop(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }

    let Some(pid) = child.id() else {
        let _ = child.start_kill();
        return;
    };

    let pgid = -(pid as libc::pid_t);
    unsafe {
        libc::kill(pgid, libc::SIGTERM);
    }
    let _ = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
    });
}

#[cfg(not(unix))]
async fn terminate_child_process_tree(child: &mut Child) {
    let _ = child.kill().await;
}

#[cfg(not(unix))]
fn terminate_child_process_tree_on_drop(child: &mut Child) {
    let _ = child.start_kill();
}
