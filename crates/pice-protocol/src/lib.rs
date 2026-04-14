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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateCreateParams {
    pub contract: serde_json::Value,
    pub diff: String,
    #[serde(rename = "claudeMd")]
    pub claude_md: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// Result of the `evaluate/create` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateCreateResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
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
            },
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: InitializeResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.capabilities.workflow);
        assert!(parsed.capabilities.evaluation);
        assert!(!parsed.capabilities.agent_teams);
        assert_eq!(parsed.capabilities.models.len(), 1);
    }

    #[test]
    fn capabilities_camel_case_serialization() {
        let caps = ProviderCapabilities {
            workflow: true,
            evaluation: false,
            agent_teams: true,
            models: vec![],
            default_eval_model: None,
        };
        let json = serde_json::to_string(&caps).unwrap();
        assert!(json.contains("\"agentTeams\""));
        assert!(!json.contains("\"agent_teams\""));
        assert!(!json.contains("defaultEvalModel"));
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
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"claudeMd\""));
        assert!(!json.contains("\"model\""));
        assert!(!json.contains("\"effort\""));
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
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"effort\""));
        let parsed: EvaluateCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model.unwrap(), "gpt-5.4");
        assert_eq!(parsed.effort.unwrap(), "high");
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
