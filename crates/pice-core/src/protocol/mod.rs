//! Daemon RPC protocol types.
//!
//! This is the SECOND JSON-RPC 2.0 protocol in the repository. Do NOT conflate
//! it with `pice-protocol`, which is the provider protocol (daemon ↔ provider
//! over stdio). This module defines `pice-cli ↔ pice-daemon` RPC (over Unix
//! socket / Windows named pipe), see `.claude/rules/protocol.md` ("Two separate
//! protocols") and `.claude/rules/daemon.md` for the architectural separation.
//!
//! ## Authentication
//!
//! Every `DaemonRequest` carries a top-level `auth` field (NOT inside `params`)
//! containing the hex-encoded bearer token from `~/.pice/daemon.token`. The
//! daemon validates the token with a constant-time compare on every request
//! and rejects mismatches with error code `-32002`.
//!
//! ## Framing
//!
//! Newline-delimited JSON: one `DaemonRequest`/`DaemonResponse`/`DaemonNotification`
//! JSON object per line over the socket. Multi-line JSON is not supported.
//!
//! ## Notifications
//!
//! Notifications have no `id` (fire-and-forget, per JSON-RPC 2.0 spec).
//! Streaming responses — provider text chunks, evaluation events, final
//! dispatch results — arrive as notifications on the same connection between
//! the request and the final response.

use serde::{Deserialize, Serialize};

pub mod methods;

/// A daemon RPC request from the CLI adapter.
///
/// The `auth` field lives at the top level (not inside `params`) so the
/// daemon's authenticator can reject unauthenticated requests before parsing
/// the method-specific payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub auth: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl DaemonRequest {
    /// Construct a request with the JSON-RPC 2.0 version string.
    pub fn new(
        id: u64,
        method: impl Into<String>,
        auth: impl Into<String>,
        params: serde_json::Value,
    ) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            auth: auth.into(),
            params,
        }
    }
}

/// A daemon RPC response to a request. Either `result` or `error` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonError>,
}

impl DaemonResponse {
    /// Construct a successful response.
    pub fn success(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Construct an error response with the given code and message.
    pub fn error(id: u64, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(DaemonError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// A daemon RPC notification — fire-and-forget, no response expected.
/// Used for streaming output during a dispatched command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: serde_json::Value,
}

impl DaemonNotification {
    /// Construct a notification with the JSON-RPC 2.0 version string.
    pub fn new(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

/// A daemon RPC error payload.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("daemon error {code}: {message}")]
pub struct DaemonError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn daemon_request_roundtrip() {
        let req = DaemonRequest::new(
            42,
            methods::CLI_DISPATCH,
            "deadbeef",
            json!({"command": "status"}),
        );
        let wire = serde_json::to_string(&req).unwrap();
        let parsed: DaemonRequest = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed.jsonrpc, "2.0");
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.method, methods::CLI_DISPATCH);
        assert_eq!(parsed.auth, "deadbeef");
        assert_eq!(parsed.params, json!({"command": "status"}));
    }

    #[test]
    fn daemon_request_default_params_empty() {
        // When params is omitted from the wire format, it should default to Null.
        let wire = r#"{"jsonrpc":"2.0","id":1,"method":"daemon/health","auth":"token"}"#;
        let parsed: DaemonRequest = serde_json::from_str(wire).unwrap();
        assert_eq!(parsed.params, serde_json::Value::Null);
    }

    #[test]
    fn daemon_response_success_roundtrip() {
        let resp = DaemonResponse::success(42, json!({"status": "ok"}));
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(!wire.contains("\"error\""));
        let parsed: DaemonResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed.id, 42);
        assert!(parsed.error.is_none());
        assert_eq!(parsed.result, Some(json!({"status": "ok"})));
    }

    #[test]
    fn daemon_response_error_roundtrip() {
        let resp = DaemonResponse::error(42, -32002, "auth failed");
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(!wire.contains("\"result\""));
        let parsed: DaemonResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed.id, 42);
        assert!(parsed.result.is_none());
        let err = parsed.error.unwrap();
        assert_eq!(err.code, -32002);
        assert_eq!(err.message, "auth failed");
    }

    #[test]
    fn daemon_notification_roundtrip() {
        let notif = DaemonNotification::new(methods::CLI_STREAM_CHUNK, json!({"text": "hello"}));
        let wire = serde_json::to_string(&notif).unwrap();
        // Notifications have no id field
        assert!(!wire.contains("\"id\""));
        let parsed: DaemonNotification = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed.method, methods::CLI_STREAM_CHUNK);
        assert_eq!(parsed.params, json!({"text": "hello"}));
    }

    #[test]
    fn daemon_error_display() {
        let err = DaemonError {
            code: -32601,
            message: "method not found".to_string(),
            data: None,
        };
        assert_eq!(err.to_string(), "daemon error -32601: method not found");
    }

    #[test]
    fn daemon_error_with_data_roundtrip() {
        let err = DaemonError {
            code: -32003,
            message: "rate limited".to_string(),
            data: Some(json!({"retry_after": 30})),
        };
        let wire = serde_json::to_string(&err).unwrap();
        let parsed: DaemonError = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed.code, -32003);
        assert_eq!(parsed.data, Some(json!({"retry_after": 30})));
    }
}
