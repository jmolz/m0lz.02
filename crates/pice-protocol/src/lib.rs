//! PICE Provider Protocol — JSON-RPC 2.0 types for communication between the
//! Rust CLI core and TypeScript provider processes.
//!
//! This crate defines the wire format. The TypeScript `@pice/provider-protocol`
//! package must define identical types and stay in sync.

use serde::{Deserialize, Serialize};

// ─── Error Codes ─────────────────────────────────────────────────────────────

/// Standard JSON-RPC 2.0 error codes and PICE-specific extensions.
pub mod error_codes {
    // Standard JSON-RPC 2.0
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    // PICE-specific (-32000 to -32099)
    pub const PROVIDER_NOT_INITIALIZED: i64 = -32000;
    pub const SESSION_NOT_FOUND: i64 = -32001;
    pub const AUTH_FAILED: i64 = -32002;
    pub const RATE_LIMITED: i64 = -32003;
    pub const MODEL_NOT_AVAILABLE: i64 = -32004;
}

// ─── Protocol Errors ─────────────────────────────────────────────────────────

#[derive(thiserror::Error, Debug)]
pub enum ProtocolError {
    #[error("JSON parse error: {0}")]
    ParseError(#[from] serde_json::Error),

    #[error("method not found: {0}")]
    MethodNotFound(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("provider not initialized")]
    NotInitialized,

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl ProtocolError {
    pub fn to_json_rpc_error(&self) -> JsonRpcError {
        match self {
            ProtocolError::ParseError(e) => JsonRpcError {
                code: error_codes::PARSE_ERROR,
                message: e.to_string(),
                data: None,
            },
            ProtocolError::MethodNotFound(m) => JsonRpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("method not found: {m}"),
                data: None,
            },
            ProtocolError::InvalidParams(msg) => JsonRpcError {
                code: error_codes::INVALID_PARAMS,
                message: msg.clone(),
                data: None,
            },
            ProtocolError::NotInitialized => JsonRpcError {
                code: error_codes::PROVIDER_NOT_INITIALIZED,
                message: "provider not initialized".to_string(),
                data: None,
            },
            ProtocolError::SessionNotFound(id) => JsonRpcError {
                code: error_codes::SESSION_NOT_FOUND,
                message: format!("session not found: {id}"),
                data: None,
            },
            ProtocolError::Internal(msg) => JsonRpcError {
                code: error_codes::INTERNAL_ERROR,
                message: msg.clone(),
                data: None,
            },
        }
    }
}

// ─── JSON-RPC 2.0 Core Types ────────────────────────────────────────────────

/// A JSON-RPC request ID — either a number or a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(u64),
    String(String),
}

/// A JSON-RPC 2.0 request (has an `id`, expects a response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    pub fn new(
        id: RequestId,
        method: impl Into<String>,
        params: Option<serde_json::Value>,
    ) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 successful response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    pub result: serde_json::Value,
}

impl JsonRpcResponse {
    pub fn success(id: RequestId, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result,
        }
    }
}

/// A JSON-RPC 2.0 error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    pub error: JsonRpcError,
}

impl JsonRpcErrorResponse {
    pub fn new(id: Option<RequestId>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            error,
        }
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 notification (no `id`, fire-and-forget).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

// ─── Provider-Specific Message Types ─────────────────────────────────────────

/// Parameters for the `initialize` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Result of the `initialize` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    pub capabilities: ProviderCapabilities,
    pub version: String,
}

/// Provider capabilities declared during initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub workflow: bool,
    pub evaluation: bool,
    #[serde(default, rename = "agentTeams")]
    pub agent_teams: bool,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "defaultEvalModel"
    )]
    pub default_eval_model: Option<String>,
    /// Phase 4.1: whether the provider emits real per-pass `costUsd` on
    /// `evaluate/create` responses. Adaptive evaluation with `budget_usd > 0`
    /// REQUIRES this — otherwise budget enforcement is synthetic (seed-only)
    /// and `final_total_cost_usd` misrepresents real spend. Default `false`
    /// keeps legacy providers from silently claiming cost telemetry they
    /// cannot deliver; providers must explicitly opt in.
    #[serde(default, rename = "costTelemetry")]
    pub cost_telemetry: bool,
}

/// Parameters for the `session/create` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateParams {
    #[serde(rename = "workingDirectory")]
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "systemPrompt"
    )]
    pub system_prompt: Option<String>,
    /// v0.2: layer name for stack loops (e.g., "backend", "frontend").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// v0.2: glob patterns for this layer's files.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "layerPaths"
    )]
    pub layer_paths: Option<Vec<String>>,
    /// v0.2: path to layer contract file.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "contractPath"
    )]
    pub contract_path: Option<String>,
}

/// Result of the `session/create` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

/// Parameters for the `session/send` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSendParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub message: String,
}

/// Result of the `session/send` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSendResult {
    pub ok: bool,
}

/// Parameters for the `session/destroy` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDestroyParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

/// Parameters for the `response/tool_use` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseToolUseParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    #[serde(rename = "toolInput")]
    pub tool_input: serde_json::Value,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "toolResult"
    )]
    pub tool_result: Option<serde_json::Value>,
}

/// Parameters for the `response/chunk` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseChunkParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub text: String,
}

/// Parameters for the `response/complete` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseCompleteParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub result: serde_json::Value,
}

/// Parameters for the `evaluate/create` method.
///
/// # Schema hardening
///
/// `deny_unknown_fields` rejects typos in the wire params — critical because
/// the adaptive loop's ADTS signals (`freshContext`, `effortOverride`) are
/// load-bearing and a silently-dropped field would disable escalation without
/// warning. Phase 4 contract criterion #9.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvaluateCreateParams {
    pub contract: serde_json::Value,
    pub diff: String,
    #[serde(rename = "claudeMd")]
    pub claude_md: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Seam check specs for this layer's boundaries. Omitted for v0.1
    /// providers; the daemon tolerates absence and defaults to no seam
    /// verification.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "seamChecks"
    )]
    pub seam_checks: Option<Vec<SeamCheckSpec>>,
    /// 0-indexed pass number within an adaptive evaluation loop. Absent
    /// for single-pass (v0.1) evaluations. Advisory — providers may ignore.
    /// The stub provider uses this to index `PICE_STUB_SCORES`.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "passIndex")]
    pub pass_index: Option<u32>,
    /// ADTS Level 1+ signal: recreate the provider session instead of
    /// reusing prior-pass context. Set on the pass immediately following a
    /// divergent-score detection. Absent for SPRT/VEC/None or when ADTS is
    /// in the `Continue` state. Phase 4 contract criterion #5.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "freshContext"
    )]
    pub fresh_context: Option<bool>,
    /// ADTS Level 2 signal: override the base `effort` for this pass only.
    /// Typically `"xhigh"` on a Level 2 escalation; absent otherwise. Takes
    /// precedence over `effort` when both are set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "effortOverride"
    )]
    pub effort_override: Option<String>,
}

/// Wire-form seam check specification, mirrored from `pice-core::seam::types`
/// so the protocol crate stays dependency-light. Kept PartialEq + strict
/// serde so roundtrip tests catch drift.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SeamCheckSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Result of the `evaluate/create` method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvaluateCreateResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// Estimated cost in USD for this pass. Provider-reported; absent when
    /// the provider has no cost metadata.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "costUsd")]
    pub cost_usd: Option<f64>,
    /// Provider's own confidence estimate for this pass (0.0–1.0). Used by
    /// the adaptive loop as a secondary signal; the primary confidence
    /// comes from the Bayesian posterior in `pice-core::adaptive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// Parameters for the `evaluate/score` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateScoreParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

/// Result of the `evaluate/score` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateScoreResult {
    pub ok: bool,
}

/// A single criterion score from evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionScore {
    pub name: String,
    pub score: u8,
    pub threshold: u8,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<String>,
}

/// Parameters for the `evaluate/result` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateResultParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub scores: Vec<CriterionScore>,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

// ─── Method Constants ────────────────────────────────────────────────────────

pub mod methods {
    // Core → Provider requests
    pub const INITIALIZE: &str = "initialize";
    pub const SHUTDOWN: &str = "shutdown";
    pub const CAPABILITIES: &str = "capabilities";
    pub const SESSION_CREATE: &str = "session/create";
    pub const SESSION_SEND: &str = "session/send";
    pub const SESSION_DESTROY: &str = "session/destroy";
    pub const EVALUATE_CREATE: &str = "evaluate/create";
    pub const EVALUATE_SCORE: &str = "evaluate/score";

    // Provider → Core notifications
    pub const RESPONSE_CHUNK: &str = "response/chunk";
    pub const RESPONSE_COMPLETE: &str = "response/complete";
    pub const RESPONSE_TOOL_USE: &str = "response/tool_use";
    pub const EVALUATE_RESULT: &str = "evaluate/result";
    pub const METRICS_EVENT: &str = "metrics/event";
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_id_number_roundtrip() {
        let id = RequestId::Number(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "42");
        let parsed: RequestId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn request_id_string_roundtrip() {
        let id = RequestId::String("abc-123".to_string());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"abc-123\"");
        let parsed: RequestId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn json_rpc_request_roundtrip() {
        let req = JsonRpcRequest::new(
            RequestId::Number(1),
            "session/create",
            Some(json!({"workingDirectory": "/tmp/project"})),
        );
        let json = serde_json::to_string(&req).unwrap();
        let parsed: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.jsonrpc, "2.0");
        assert_eq!(parsed.id, RequestId::Number(1));
        assert_eq!(parsed.method, "session/create");
    }

    #[test]
    fn json_rpc_request_without_params() {
        let req = JsonRpcRequest::new(RequestId::Number(1), "shutdown", None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("params"));
        let parsed: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert!(parsed.params.is_none());
    }

    #[test]
    fn json_rpc_response_roundtrip() {
        let resp = JsonRpcResponse::success(RequestId::Number(1), json!({"sessionId": "abc-123"}));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, RequestId::Number(1));
        assert_eq!(parsed.result["sessionId"], "abc-123");
    }

    #[test]
    fn json_rpc_error_response_roundtrip() {
        let err_resp = JsonRpcErrorResponse::new(
            Some(RequestId::Number(1)),
            JsonRpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: "method not found: foo/bar".to_string(),
                data: None,
            },
        );
        let json = serde_json::to_string(&err_resp).unwrap();
        let parsed: JsonRpcErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.error.code, error_codes::METHOD_NOT_FOUND);
        assert!(parsed.error.data.is_none());
    }

    #[test]
    fn json_rpc_error_response_with_null_id() {
        let err_resp = JsonRpcErrorResponse::new(
            None,
            JsonRpcError {
                code: error_codes::PARSE_ERROR,
                message: "invalid JSON".to_string(),
                data: None,
            },
        );
        let json = serde_json::to_string(&err_resp).unwrap();
        assert!(json.contains("\"id\":null"));
    }

    #[test]
    fn json_rpc_notification_roundtrip() {
        let notif = JsonRpcNotification::new(
            "response/chunk",
            Some(json!({"sessionId": "abc-123", "text": "Hello"})),
        );
        let json = serde_json::to_string(&notif).unwrap();
        assert!(!json.contains("\"id\""));
        let parsed: JsonRpcNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "response/chunk");
    }

    #[test]
    fn initialize_params_roundtrip() {
        let params = InitializeParams {
            config: json!({"apiKey": "test"}),
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: InitializeParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.config["apiKey"], "test");
    }

    #[test]
    fn initialize_result_roundtrip() {
        let result = InitializeResult {
            capabilities: ProviderCapabilities {
                workflow: true,
                evaluation: true,
                agent_teams: false,
                models: vec!["claude-opus-4-6".to_string()],
                default_eval_model: Some("claude-opus-4-6".to_string()),
                cost_telemetry: true,
            },
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: InitializeResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.capabilities.workflow);
        assert!(parsed.capabilities.evaluation);
        assert!(!parsed.capabilities.agent_teams);
        assert_eq!(parsed.capabilities.models.len(), 1);
        assert!(parsed.capabilities.cost_telemetry);
    }

    #[test]
    fn capabilities_camel_case_serialization() {
        let caps = ProviderCapabilities {
            workflow: true,
            evaluation: false,
            agent_teams: true,
            models: vec![],
            default_eval_model: None,
            cost_telemetry: false,
        };
        let json = serde_json::to_string(&caps).unwrap();
        assert!(json.contains("\"agentTeams\""));
        assert!(!json.contains("\"agent_teams\""));
        assert!(!json.contains("defaultEvalModel"));
        // Phase 4.1: cost_telemetry defaults to false and serializes camelCase.
        assert!(json.contains("\"costTelemetry\":false"));
        assert!(!json.contains("\"cost_telemetry\""));
    }

    /// Phase 4.1: a legacy provider that predates `costTelemetry` must
    /// deserialize with `cost_telemetry = false` — the fail-closed default.
    /// Without `#[serde(default)]` this would be a breaking change; with it,
    /// adaptive evaluation against a legacy provider fails safely in the
    /// capability gate instead of silently running on synthetic budgets.
    #[test]
    fn capabilities_legacy_provider_defaults_cost_telemetry_false() {
        let json = r#"{
            "workflow": true,
            "evaluation": true,
            "agentTeams": false,
            "models": ["claude-opus-4-6"]
        }"#;
        let parsed: ProviderCapabilities = serde_json::from_str(json).unwrap();
        assert!(!parsed.cost_telemetry, "legacy provider must default false");
    }

    #[test]
    fn session_create_roundtrip() {
        let params = SessionCreateParams {
            working_directory: "/tmp/project".to_string(),
            model: None,
            system_prompt: None,
            layer: None,
            layer_paths: None,
            contract_path: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"workingDirectory\""));
        assert!(!json.contains("\"model\""));
        assert!(!json.contains("\"systemPrompt\""));
        let parsed: SessionCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.working_directory, "/tmp/project");
    }

    #[test]
    fn session_create_with_optional_fields() {
        let params = SessionCreateParams {
            working_directory: "/tmp/project".to_string(),
            model: Some("claude-opus-4-6".to_string()),
            system_prompt: Some("You are a planner.".to_string()),
            layer: None,
            layer_paths: None,
            contract_path: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"systemPrompt\""));
        let parsed: SessionCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model.unwrap(), "claude-opus-4-6");
        assert_eq!(parsed.system_prompt.unwrap(), "You are a planner.");
    }

    #[test]
    fn session_create_with_layer_fields() {
        let params = SessionCreateParams {
            working_directory: "/tmp/worktree/backend".to_string(),
            model: None,
            system_prompt: None,
            layer: Some("backend".to_string()),
            layer_paths: Some(vec!["src/server/**".to_string(), "lib/**".to_string()]),
            contract_path: Some(".pice/contracts/backend.toml".to_string()),
        };
        let json = serde_json::to_string(&params).unwrap();
        // Verify camelCase serialization
        assert!(json.contains("\"layer\""));
        assert!(json.contains("\"layerPaths\""));
        assert!(json.contains("\"contractPath\""));
        // Verify snake_case is NOT in JSON
        assert!(!json.contains("\"layer_paths\""));
        assert!(!json.contains("\"contract_path\""));
        // Roundtrip
        let parsed: SessionCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.layer.unwrap(), "backend");
        assert_eq!(parsed.layer_paths.unwrap(), vec!["src/server/**", "lib/**"]);
        assert_eq!(
            parsed.contract_path.unwrap(),
            ".pice/contracts/backend.toml"
        );
    }

    #[test]
    fn session_create_without_layer_fields() {
        let params = SessionCreateParams {
            working_directory: "/tmp/project".to_string(),
            model: None,
            system_prompt: None,
            layer: None,
            layer_paths: None,
            contract_path: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        // Layer fields must be absent from JSON when None (backwards compatible)
        assert!(!json.contains("\"layer\""));
        assert!(!json.contains("\"layerPaths\""));
        assert!(!json.contains("\"contractPath\""));
        // Deserialize a v0.1 payload without layer fields
        let v1_json = r#"{"workingDirectory":"/old/project"}"#;
        let parsed: SessionCreateParams = serde_json::from_str(v1_json).unwrap();
        assert_eq!(parsed.working_directory, "/old/project");
        assert!(parsed.layer.is_none());
        assert!(parsed.layer_paths.is_none());
        assert!(parsed.contract_path.is_none());
    }

    #[test]
    fn session_create_result_roundtrip() {
        let result = SessionCreateResult {
            session_id: "session-abc".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"sessionId\""));
        let parsed: SessionCreateResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "session-abc");
    }

    #[test]
    fn response_chunk_roundtrip() {
        let params = ResponseChunkParams {
            session_id: "s1".to_string(),
            text: "Hello world".to_string(),
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: ResponseChunkParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "s1");
        assert_eq!(parsed.text, "Hello world");
    }

    #[test]
    fn evaluate_create_params_roundtrip() {
        let params = EvaluateCreateParams {
            contract: json!({"criteria": []}),
            diff: "+added line".to_string(),
            claude_md: "# Rules".to_string(),
            model: None,
            effort: None,
            seam_checks: None,
            pass_index: None,
            fresh_context: None,
            effort_override: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"claudeMd\""));
        assert!(!json.contains("\"model\""));
        assert!(!json.contains("\"effort\""));
        assert!(
            !json.contains("seamChecks"),
            "None seam_checks should be skipped: {json}"
        );
        assert!(
            !json.contains("passIndex"),
            "None pass_index should be skipped: {json}"
        );
        let parsed: EvaluateCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.diff, "+added line");
    }

    #[test]
    fn evaluate_create_params_with_optional_fields() {
        let params = EvaluateCreateParams {
            contract: json!({"criteria": []}),
            diff: "+line".to_string(),
            claude_md: "# Rules".to_string(),
            model: Some("gpt-5.4".to_string()),
            effort: Some("high".to_string()),
            seam_checks: None,
            pass_index: None,
            fresh_context: None,
            effort_override: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"effort\""));
        let parsed: EvaluateCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model.unwrap(), "gpt-5.4");
        assert_eq!(parsed.effort.unwrap(), "high");
    }

    #[test]
    fn evaluate_create_seam_checks_roundtrip() {
        let params = EvaluateCreateParams {
            contract: json!({"criteria": []}),
            diff: String::new(),
            claude_md: String::new(),
            model: None,
            effort: None,
            seam_checks: Some(vec![
                SeamCheckSpec {
                    id: "config_mismatch".into(),
                    boundary: Some("backend↔infrastructure".into()),
                    args: None,
                },
                SeamCheckSpec {
                    id: "schema_drift".into(),
                    boundary: Some("backend↔database".into()),
                    args: Some({
                        let mut m = serde_json::Map::new();
                        m.insert("strict".into(), json!(true));
                        m
                    }),
                },
            ]),
            pass_index: None,
            fresh_context: None,
            effort_override: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("seamChecks"));
        assert!(json.contains("backend↔infrastructure"));
        let parsed: EvaluateCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, params);
    }

    #[test]
    fn seam_check_spec_denies_unknown_fields_in_protocol_crate() {
        let bad = r#"{"id":"x","typo":1}"#;
        let res: Result<SeamCheckSpec, _> = serde_json::from_str(bad);
        assert!(res.is_err(), "unknown field should be rejected");
    }

    // ─── Phase 4 adaptive protocol roundtrips ──────────────────────────

    #[test]
    fn evaluate_create_params_with_pass_index_roundtrips() {
        let params = EvaluateCreateParams {
            contract: json!({"criteria": []}),
            diff: "+line".into(),
            claude_md: "# R".into(),
            model: None,
            effort: None,
            seam_checks: None,
            pass_index: Some(3),
            fresh_context: None,
            effort_override: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(
            json.contains("\"passIndex\":3"),
            "camelCase wire key: {json}"
        );
        let parsed: EvaluateCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pass_index, Some(3));
    }

    #[test]
    fn evaluate_create_params_without_pass_index_omits_field_in_json() {
        let params = EvaluateCreateParams {
            contract: json!({}),
            diff: String::new(),
            claude_md: String::new(),
            model: None,
            effort: None,
            seam_checks: None,
            pass_index: None,
            fresh_context: None,
            effort_override: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(
            !json.contains("passIndex"),
            "None pass_index must be omitted: {json}"
        );
    }

    #[test]
    fn evaluate_create_result_with_cost_and_confidence_roundtrips() {
        let result = EvaluateCreateResult {
            session_id: "eval-42".into(),
            cost_usd: Some(0.025),
            confidence: Some(0.93),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"costUsd\""), "camelCase wire key: {json}");
        assert!(
            json.contains("\"confidence\""),
            "confidence present: {json}"
        );
        let parsed: EvaluateCreateResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, result);
    }

    // ─── Phase 4 criterion #9: schema hardening + ADTS escalation fields ──

    #[test]
    fn evaluate_create_params_rejects_unknown_field() {
        // A typoed `passIndexx` would silently drop without deny_unknown_fields.
        // ADTS escalation signals are load-bearing; a dropped field is a
        // silent correctness bug.
        let bad = r#"{
            "contract":{},
            "diff":"",
            "claudeMd":"",
            "passIndexx":3
        }"#;
        let res: Result<EvaluateCreateParams, _> = serde_json::from_str(bad);
        assert!(res.is_err(), "unknown field must be rejected");
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("passIndexx") || err.contains("unknown field"),
            "error should name the offending field: {err}"
        );
    }

    #[test]
    fn evaluate_create_params_with_fresh_context_roundtrips() {
        let params = EvaluateCreateParams {
            contract: json!({}),
            diff: String::new(),
            claude_md: String::new(),
            model: None,
            effort: None,
            seam_checks: None,
            pass_index: Some(1),
            fresh_context: Some(true),
            effort_override: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(
            json.contains("\"freshContext\":true"),
            "camelCase wire key: {json}"
        );
        let parsed: EvaluateCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.fresh_context, Some(true));
    }

    #[test]
    fn evaluate_create_params_with_effort_override_roundtrips() {
        let params = EvaluateCreateParams {
            contract: json!({}),
            diff: String::new(),
            claude_md: String::new(),
            model: None,
            effort: Some("high".into()),
            seam_checks: None,
            pass_index: Some(2),
            fresh_context: Some(true),
            effort_override: Some("xhigh".into()),
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(
            json.contains("\"effortOverride\":\"xhigh\""),
            "camelCase wire key + xhigh value: {json}"
        );
        let parsed: EvaluateCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.effort_override.as_deref(), Some("xhigh"));
    }

    #[test]
    fn evaluate_create_params_omits_fresh_context_and_effort_override_when_none() {
        let params = EvaluateCreateParams {
            contract: json!({}),
            diff: String::new(),
            claude_md: String::new(),
            model: None,
            effort: None,
            seam_checks: None,
            pass_index: None,
            fresh_context: None,
            effort_override: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(
            !json.contains("freshContext"),
            "None fresh_context must be omitted: {json}"
        );
        assert!(
            !json.contains("effortOverride"),
            "None effort_override must be omitted: {json}"
        );
    }

    #[test]
    fn evaluate_create_result_without_cost_omits_optional_fields() {
        let result = EvaluateCreateResult {
            session_id: "eval-43".into(),
            cost_usd: None,
            confidence: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("costUsd"), "None must be omitted: {json}");
        assert!(!json.contains("confidence"), "None must be omitted: {json}");
        let parsed: EvaluateCreateResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.cost_usd, None);
        assert_eq!(parsed.confidence, None);
    }

    #[test]
    fn criterion_score_roundtrip() {
        let score = CriterionScore {
            name: "Tests pass".to_string(),
            score: 8,
            threshold: 7,
            passed: true,
            findings: Some("All 42 tests pass".to_string()),
        };
        let json = serde_json::to_string(&score).unwrap();
        let parsed: CriterionScore = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.score, 8);
        assert!(parsed.passed);
    }

    #[test]
    fn evaluate_result_roundtrip() {
        let result = EvaluateResultParams {
            session_id: "eval-1".to_string(),
            scores: vec![CriterionScore {
                name: "Build succeeds".to_string(),
                score: 9,
                threshold: 7,
                passed: true,
                findings: None,
            }],
            passed: true,
            summary: Some("All criteria met".to_string()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: EvaluateResultParams = serde_json::from_str(&json).unwrap();
        assert!(parsed.passed);
        assert_eq!(parsed.scores.len(), 1);
    }

    #[test]
    fn session_send_result_roundtrip() {
        let result = SessionSendResult { ok: true };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: SessionSendResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
    }

    #[test]
    fn evaluate_score_result_roundtrip() {
        let result = EvaluateScoreResult { ok: true };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: EvaluateScoreResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
    }

    #[test]
    fn response_tool_use_roundtrip() {
        let params = ResponseToolUseParams {
            session_id: "s1".to_string(),
            tool_name: "Read".to_string(),
            tool_input: json!({"path": "/tmp/file.rs"}),
            tool_result: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"toolName\""));
        assert!(json.contains("\"toolInput\""));
        assert!(!json.contains("\"toolResult\""));
        let parsed: ResponseToolUseParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "Read");
        assert!(parsed.tool_result.is_none());
    }

    #[test]
    fn response_tool_use_with_result() {
        let params = ResponseToolUseParams {
            session_id: "s1".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: json!({"command": "ls"}),
            tool_result: Some(json!({"output": "file.txt"})),
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"toolResult\""));
        let parsed: ResponseToolUseParams = serde_json::from_str(&json).unwrap();
        assert!(parsed.tool_result.is_some());
    }

    #[test]
    fn protocol_error_to_json_rpc_error() {
        let err = ProtocolError::MethodNotFound("foo/bar".to_string());
        let rpc_err = err.to_json_rpc_error();
        assert_eq!(rpc_err.code, error_codes::METHOD_NOT_FOUND);
        assert!(rpc_err.message.contains("foo/bar"));
    }

    #[test]
    fn full_request_response_wire_format() {
        // Simulate the exact wire format a provider would see
        let req_json = r#"{"jsonrpc":"2.0","id":1,"method":"session/create","params":{"workingDirectory":"/path/to/project"}}"#;
        let req: JsonRpcRequest = serde_json::from_str(req_json).unwrap();
        assert_eq!(req.method, methods::SESSION_CREATE);

        let params: SessionCreateParams = serde_json::from_value(req.params.unwrap()).unwrap();
        assert_eq!(params.working_directory, "/path/to/project");

        let result = SessionCreateResult {
            session_id: "abc-123".to_string(),
        };
        let resp = JsonRpcResponse::success(req.id, serde_json::to_value(&result).unwrap());
        let resp_json = serde_json::to_string(&resp).unwrap();
        assert!(resp_json.contains("\"sessionId\":\"abc-123\""));
    }

    #[test]
    fn full_notification_wire_format() {
        let notif_json = r###"{"jsonrpc":"2.0","method":"response/chunk","params":{"sessionId":"abc-123","text":"## Plan"}}"###;
        let notif: JsonRpcNotification = serde_json::from_str(notif_json).unwrap();
        assert_eq!(notif.method, methods::RESPONSE_CHUNK);

        let params: ResponseChunkParams = serde_json::from_value(notif.params.unwrap()).unwrap();
        assert_eq!(params.text, "## Plan");
    }
}
