use crate::provider::host::{NotificationHandler, ProviderHost};
use anyhow::{Context, Result};
use pice_core::config::PiceConfig;
use pice_core::provider::registry;
use pice_protocol::{
    EvaluateCreateParams, EvaluateCreateResult, EvaluateResultParams, EvaluateScoreParams,
    InitializeParams,
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

/// Per-pass outcome returned by [`ProviderOrchestrator::evaluate_one_pass`].
///
/// Bundles the final `evaluate/result` notification with the per-pass
/// `costUsd` / `confidence` fields that were emitted on the `evaluate/create`
/// reply. The adaptive loop owns the Beta-posterior confidence math;
/// `confidence` here is the provider's own self-report (secondary signal).
#[derive(Debug, Clone)]
pub struct PerPassOutcome {
    pub result: EvaluateResultParams,
    pub cost_usd: Option<f64>,
    pub confidence: Option<f64>,
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
            pass_index: None,
            fresh_context: None,
            effort_override: None,
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

    /// Run a single adaptive pass. Phase 4 sibling of [`Self::evaluate`] that
    /// threads `pass_index` through to the provider and captures the per-pass
    /// `costUsd` / `confidence` fields emitted on the `evaluate/create` reply.
    ///
    /// The adaptive loop invokes this once per pass; the old `evaluate` path
    /// stays in place for legacy Tier 1/2 non-adaptive callers.
    #[allow(clippy::too_many_arguments)]
    pub async fn evaluate_one_pass(
        &mut self,
        contract: Value,
        diff: String,
        claude_md: String,
        model: Option<String>,
        effort: Option<String>,
        pass_index: Option<u32>,
        // Phase 4 ADTS signals — set by the adaptive loop on Level 1+ / Level 2
        // escalations only. Providers unaware of these fields tolerate their
        // absence; the stub provider records them for isolation-test capture.
        fresh_context: Option<bool>,
        effort_override: Option<String>,
    ) -> Result<PerPassOutcome> {
        let (tx, rx) = tokio::sync::oneshot::channel::<EvaluateResultParams>();
        let tx = std::sync::Mutex::new(Some(tx));

        self.on_notification(Box::new(move |method, params| {
            if method == "evaluate/result" {
                if let Some(params) = params {
                    if let Ok(result) = serde_json::from_value::<EvaluateResultParams>(params) {
                        if let Ok(mut guard) = tx.lock() {
                            if let Some(tx) = guard.take() {
                                tx.send(result).ok();
                            }
                        }
                    }
                }
            }
        }));

        let create_params = serde_json::to_value(EvaluateCreateParams {
            contract,
            diff,
            claude_md,
            model,
            effort,
            seam_checks: None,
            pass_index,
            fresh_context,
            effort_override,
        })?;
        let raw = self.request("evaluate/create", Some(create_params)).await?;
        let create_res: EvaluateCreateResult = serde_json::from_value(raw)
            .context("evaluate/create response was not a valid EvaluateCreateResult")?;

        let deadline = tokio::time::Instant::now() + EVAL_NOTIFICATION_TIMEOUT;
        let score_params = serde_json::to_value(EvaluateScoreParams {
            session_id: create_res.session_id.clone(),
        })?;
        self.request_with_timeout(
            "evaluate/score",
            Some(score_params),
            EVAL_NOTIFICATION_TIMEOUT,
        )
        .await?;

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

        Ok(PerPassOutcome {
            result,
            cost_usd: create_res.cost_usd,
            confidence: create_res.confidence,
        })
    }

    /// Gracefully shutdown the provider.
    pub async fn shutdown(self) -> Result<()> {
        self.host.shutdown(Duration::from_secs(10)).await
    }

    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }
}
