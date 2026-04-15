use crate::provider::host::{NotificationHandler, ProviderHost};
use anyhow::{Context, Result};
use pice_core::config::PiceConfig;
use pice_core::provider::registry;
use pice_protocol::{
    EvaluateCreateParams, EvaluateResultParams, EvaluateScoreParams, InitializeParams,
};
use serde_json::Value;
use std::time::Duration;
use tracing::debug;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
const EVAL_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(300);

pub struct ProviderOrchestrator {
    host: ProviderHost,
    provider_name: String,
}

impl ProviderOrchestrator {
    /// Resolve, spawn, and initialize a provider by name.
    pub async fn start(name: &str, config: &PiceConfig) -> Result<Self> {
        let resolved =
            registry::resolve(name, config).with_context(|| format!("unknown provider: {name}"))?;

        debug!(name, command = %resolved.command, "starting provider");

        let args: Vec<&str> = resolved.args.iter().map(|s| s.as_str()).collect();
        let mut host = ProviderHost::spawn(&resolved.command, &args).await?;

        // Initialize provider with config
        let init_params = serde_json::to_value(InitializeParams {
            config: serde_json::json!({}),
        })?;
        host.request("initialize", Some(init_params), INIT_TIMEOUT)
            .await
            .context("provider initialization failed")?;

        Ok(Self {
            host,
            provider_name: name.to_string(),
        })
    }

    /// Set notification handler for streaming responses.
    pub fn on_notification(&mut self, handler: NotificationHandler) {
        self.host.on_notification(handler);
    }

    /// Send a JSON-RPC request with the default timeout.
    pub async fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        self.host.request(method, params, DEFAULT_TIMEOUT).await
    }

    /// Send a request with a custom timeout.
    pub async fn request_with_timeout(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        self.host.request(method, params, timeout).await
    }

    /// Run a full evaluation cycle: create → score → capture result.
    pub async fn evaluate(
        &mut self,
        contract: Value,
        diff: String,
        claude_md: String,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<EvaluateResultParams> {
        // Set up oneshot channel to capture evaluate/result notification
        let (tx, rx) = tokio::sync::oneshot::channel::<EvaluateResultParams>();
        let tx = std::sync::Mutex::new(Some(tx));

        self.on_notification(Box::new(move |method, params| {
            if method == "evaluate/result" {
                if let Some(params) = params {
                    match serde_json::from_value::<EvaluateResultParams>(params) {
                        Ok(result) => {
                            if let Ok(mut guard) = tx.lock() {
                                if let Some(tx) = guard.take() {
                                    tx.send(result).ok();
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "warning: received evaluate/result but failed to deserialize: {e}"
                            );
                        }
                    }
                }
            }
        }));

        // Create evaluation session
        let create_params = serde_json::to_value(EvaluateCreateParams {
            contract,
            diff,
            claude_md,
            model,
            effort,
            seam_checks: None,
        })?;
        let create_result = self.request("evaluate/create", Some(create_params)).await?;
        let session_id = create_result
            .get("sessionId")
            .and_then(|s| s.as_str())
            .context("evaluate/create did not return sessionId")?
            .to_string();

        // Use a single shared deadline for the entire scoring + notification sequence.
        // The provider performs the model call inside evaluate/score and sends
        // evaluate/result before or alongside the RPC response.
        let deadline = tokio::time::Instant::now() + EVAL_NOTIFICATION_TIMEOUT;

        let score_params = serde_json::to_value(EvaluateScoreParams { session_id })?;
        self.request_with_timeout(
            "evaluate/score",
            Some(score_params),
            EVAL_NOTIFICATION_TIMEOUT,
        )
        .await?;

        // Wait for the result notification with remaining budget from the shared deadline
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let result = tokio::time::timeout(remaining, rx)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "evaluation timed out waiting for result notification ({}s budget)",
                    EVAL_NOTIFICATION_TIMEOUT.as_secs()
                )
            })?
            .context("evaluation result notification not received")?;
        Ok(result)
    }

    /// Gracefully shutdown the provider.
    pub async fn shutdown(self) -> Result<()> {
        self.host.shutdown(Duration::from_secs(10)).await
    }

    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }
}
