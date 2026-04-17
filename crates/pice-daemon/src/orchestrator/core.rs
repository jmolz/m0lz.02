use crate::provider::host::{NotificationHandler, ProviderHost};
use anyhow::{Context, Result};
use pice_core::config::PiceConfig;
use pice_core::provider::registry;
use pice_protocol::{
    EvaluateCreateParams, EvaluateCreateResult, EvaluateResultParams, EvaluateScoreParams,
    InitializeParams, InitializeResult, ProviderCapabilities,
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
    /// Phase 4.1: capabilities declared by the provider during `initialize`.
    /// Used by the adaptive-budget capability gate in `stack_loops.rs` to
    /// fail closed when `budget_usd > 0` is requested against a provider
    /// that cannot supply real per-pass `costUsd` (Pass-6 Codex Critical #1).
    capabilities: ProviderCapabilities,
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
        let init_value = host
            .request("initialize", Some(init_params), INIT_TIMEOUT)
            .await
            .context("provider initialization failed")?;

        // Phase 4.1 (Pass-6 Codex Critical #1): capture declared capabilities.
        // Providers that predate `costTelemetry` deserialize with the field
        // defaulted to `false` — the fail-closed semantic that forces the
        // capability gate in `stack_loops.rs` to reject adaptive budgets on
        // legacy providers rather than silently running on synthetic spend.
        let init_result: InitializeResult = serde_json::from_value(init_value)
            .context("provider returned an invalid initialize response")?;
        let capabilities = init_result.capabilities;

        Ok(Self {
            host,
            provider_name: name.to_string(),
            capabilities,
        })
    }

    /// Phase 4.1: capabilities the provider declared during `initialize`.
    /// Used by the adaptive-budget capability gate (see `stack_loops.rs`).
    pub fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
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
        // Create evaluation session FIRST so we can filter notifications by
        // session_id. Phase 4 Pass-5 Codex Critical #2: registering the
        // handler before `evaluate/create` returns means the handler has no
        // way to tell which session a notification belongs to, so a late or
        // duplicate `evaluate/result` from a prior evaluation on the same
        // provider process would corrupt THIS evaluation's oneshot.
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

        // Now wire the notification handler, filtering by session_id captured
        // above. `evaluate/result` is emitted by the provider in response to
        // `evaluate/score` below; between `evaluate/create` returning and the
        // handler being installed, the provider has not been asked to score
        // yet, so no legitimate `evaluate/result` for THIS session can fire
        // inside that window.
        let (tx, rx) = tokio::sync::oneshot::channel::<EvaluateResultParams>();
        let tx = std::sync::Mutex::new(Some(tx));
        let expected_session_id = session_id.clone();
        self.on_notification(Box::new(move |method, params| {
            if method != "evaluate/result" {
                return;
            }
            let Some(params) = params else { return };
            let result = match serde_json::from_value::<EvaluateResultParams>(params) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("warning: received evaluate/result but failed to deserialize: {e}");
                    return;
                }
            };
            if result.session_id != expected_session_id {
                // Stale/late notification from a prior session on this
                // provider process — drop it. Without this guard a duplicate
                // or delayed `evaluate/result` from pass N-1 would satisfy
                // pass N's oneshot with wrong scores/findings.
                return;
            }
            if let Ok(mut guard) = tx.lock() {
                if let Some(tx) = guard.take() {
                    tx.send(result).ok();
                }
            }
        }));

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
        // Phase 4 Pass-5 Codex Critical #2: create FIRST, then register
        // handler with session_id captured by value. In a multi-pass run the
        // same provider process handles sequential sessions, so a late or
        // duplicate `evaluate/result` notification from pass N-1 could
        // otherwise satisfy pass N's oneshot — corrupting scores, SPRT/VEC
        // observations, and halt reason for the wrong pass. The provider
        // emits `evaluate/result` in response to `evaluate/score` (below);
        // between the create RPC returning and the handler being installed
        // no legitimate `evaluate/result` for THIS session can arrive.
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

        let (tx, rx) = tokio::sync::oneshot::channel::<EvaluateResultParams>();
        let tx = std::sync::Mutex::new(Some(tx));
        let expected_session_id = create_res.session_id.clone();
        self.on_notification(Box::new(move |method, params| {
            if method != "evaluate/result" {
                return;
            }
            let Some(params) = params else { return };
            let Ok(result) = serde_json::from_value::<EvaluateResultParams>(params) else {
                return;
            };
            if result.session_id != expected_session_id {
                // Stale/late notification from a prior pass — ignore.
                return;
            }
            if let Ok(mut guard) = tx.lock() {
                if let Some(tx) = guard.take() {
                    tx.send(result).ok();
                }
            }
        }));

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
