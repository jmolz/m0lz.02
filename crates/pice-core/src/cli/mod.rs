//! Shared `CommandRequest` / `CommandResponse` enums — the serialization
//! boundary between `pice-cli` and `pice-daemon`.
//!
//! The CLI parses clap args, converts them to a `CommandRequest` via the
//! `From<XxxArgs>` impls defined in each command module, and sends the request
//! to the daemon as the `params` of a `cli/dispatch` RPC. The daemon dispatches
//! based on the enum variant. Both sides depend on the SAME enum here —
//! divergence is a bug (see `.claude/rules/rust-core.md` "Crate boundary rule").
//!
//! ## Mirroring rule
//!
//! Every variant of this enum corresponds 1:1 with a variant of the clap
//! `Commands` enum in `pice-cli/src/main.rs`, EXCEPT:
//! - `Completions` — handled entirely at the CLI layer (clap_complete),
//!   never crosses the socket.
//! - `Daemon` (added in T24) — manages the daemon process itself, handled
//!   at the CLI layer.
//!
//! Every request struct mirrors the corresponding `XxxArgs` struct from
//! `pice-cli/src/commands/*.rs`. When a field is added to the clap args, the
//! corresponding field must be added here too, otherwise the CLI can't
//! communicate the new option to the daemon.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A command request from the CLI adapter to the daemon.
///
/// Serialized into the `params` of a `cli/dispatch` daemon RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum CommandRequest {
    Init(InitRequest),
    Prime(PrimeRequest),
    Plan(PlanRequest),
    Execute(ExecuteRequest),
    Evaluate(EvaluateRequest),
    Review(ReviewRequest),
    Commit(CommitRequest),
    Handoff(HandoffRequest),
    Status(StatusRequest),
    Metrics(MetricsRequest),
    Benchmark(BenchmarkRequest),
    // NOTE: Completions is handled entirely by clap at the CLI layer.
    // NOTE: Daemon subcommand (start/stop/etc.) is also CLI-only.
}

/// The final result of a dispatched command, sent via `cli/stream-done`.
///
/// Uses struct variants (not newtype) for `Json` and `Text` because serde's
/// internally-tagged enum representation cannot serialize a tagged newtype
/// variant containing a primitive. Struct variants serialize as objects with
/// the tag and fields coexisting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CommandResponse {
    /// The command produced machine-readable JSON output (--json mode).
    Json { value: serde_json::Value },
    /// The command produced human-readable text output.
    Text { content: String },
    /// The command succeeded with no user-visible payload.
    Empty,
    /// The command failed and the CLI should exit with the given code.
    Exit { code: i32, message: String },
}

// ─── Request structs ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    pub force: bool,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimeRequest {
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRequest {
    pub description: String,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub plan_path: PathBuf,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateRequest {
    pub plan_path: PathBuf,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRequest {
    pub message: Option<String>,
    pub dry_run: bool,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffRequest {
    pub output: Option<PathBuf>,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRequest {
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsRequest {
    pub json: bool,
    pub csv: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkRequest {
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_request_roundtrip() {
        let req = CommandRequest::Init(InitRequest {
            force: true,
            json: false,
        });
        let wire = serde_json::to_string(&req).unwrap();
        // Tag-based serialization: {"command":"init",...}
        assert!(wire.contains("\"command\":\"init\""));
        let parsed: CommandRequest = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandRequest::Init(r) => {
                assert!(r.force);
                assert!(!r.json);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn plan_request_with_description_roundtrip() {
        let req = CommandRequest::Plan(PlanRequest {
            description: "add auth".to_string(),
            json: true,
        });
        let wire = serde_json::to_string(&req).unwrap();
        assert!(wire.contains("\"command\":\"plan\""));
        assert!(wire.contains("\"description\":\"add auth\""));
        let parsed: CommandRequest = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandRequest::Plan(r) => {
                assert_eq!(r.description, "add auth");
                assert!(r.json);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn execute_request_with_path_roundtrip() {
        let req = CommandRequest::Execute(ExecuteRequest {
            plan_path: PathBuf::from(".claude/plans/auth.md"),
            json: false,
        });
        let wire = serde_json::to_string(&req).unwrap();
        let parsed: CommandRequest = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandRequest::Execute(r) => {
                assert_eq!(r.plan_path, PathBuf::from(".claude/plans/auth.md"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn commit_request_with_optional_message_none() {
        let req = CommandRequest::Commit(CommitRequest {
            message: None,
            dry_run: true,
            json: false,
        });
        let wire = serde_json::to_string(&req).unwrap();
        let parsed: CommandRequest = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandRequest::Commit(r) => {
                assert!(r.message.is_none());
                assert!(r.dry_run);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn commit_request_with_optional_message_some() {
        let req = CommandRequest::Commit(CommitRequest {
            message: Some("fix(bug): resolve race".to_string()),
            dry_run: false,
            json: false,
        });
        let wire = serde_json::to_string(&req).unwrap();
        let parsed: CommandRequest = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandRequest::Commit(r) => {
                assert_eq!(r.message.as_deref(), Some("fix(bug): resolve race"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn handoff_request_with_optional_path() {
        let req = CommandRequest::Handoff(HandoffRequest {
            output: Some(PathBuf::from("HANDOFF.md")),
            json: false,
        });
        let wire = serde_json::to_string(&req).unwrap();
        let parsed: CommandRequest = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandRequest::Handoff(r) => {
                assert_eq!(r.output, Some(PathBuf::from("HANDOFF.md")));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn metrics_request_with_csv_flag() {
        let req = CommandRequest::Metrics(MetricsRequest {
            json: false,
            csv: true,
        });
        let wire = serde_json::to_string(&req).unwrap();
        let parsed: CommandRequest = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandRequest::Metrics(r) => {
                assert!(!r.json);
                assert!(r.csv);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_request_kebab_case_tag() {
        // Verify every variant uses kebab-case tags matching the clap command names.
        for (req, expected_tag) in [
            (CommandRequest::Prime(PrimeRequest { json: false }), "prime"),
            (
                CommandRequest::Review(ReviewRequest { json: false }),
                "review",
            ),
            (
                CommandRequest::Status(StatusRequest { json: false }),
                "status",
            ),
            (
                CommandRequest::Benchmark(BenchmarkRequest { json: false }),
                "benchmark",
            ),
            (
                CommandRequest::Evaluate(EvaluateRequest {
                    plan_path: PathBuf::from("plan.md"),
                    json: false,
                }),
                "evaluate",
            ),
        ] {
            let wire = serde_json::to_string(&req).unwrap();
            assert!(
                wire.contains(&format!("\"command\":\"{expected_tag}\"")),
                "variant should serialize with tag {expected_tag}, got: {wire}"
            );
        }
    }

    // ─── CommandResponse tests ───────────────────────────────────────────────

    #[test]
    fn command_response_json_roundtrip() {
        let resp = CommandResponse::Json {
            value: serde_json::json!({"plans": 3, "tier": 2}),
        };
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(wire.contains("\"type\":\"json\""));
        let parsed: CommandResponse = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandResponse::Json { value } => {
                assert_eq!(value, serde_json::json!({"plans": 3, "tier": 2}));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_response_text_roundtrip() {
        let resp = CommandResponse::Text {
            content: "done".to_string(),
        };
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(wire.contains("\"type\":\"text\""));
        let parsed: CommandResponse = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandResponse::Text { content } => assert_eq!(content, "done"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_response_empty_roundtrip() {
        let resp = CommandResponse::Empty;
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(wire.contains("\"type\":\"empty\""));
        let parsed: CommandResponse = serde_json::from_str(&wire).unwrap();
        matches!(parsed, CommandResponse::Empty);
    }

    #[test]
    fn command_response_exit_with_reason() {
        // Evaluation failed (contract criteria not met) — exit 2 per CLI conventions.
        let resp = CommandResponse::Exit {
            code: 2,
            message: "contract criteria not met".to_string(),
        };
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(wire.contains("\"type\":\"exit\""));
        let parsed: CommandResponse = serde_json::from_str(&wire).unwrap();
        match parsed {
            CommandResponse::Exit { code, message } => {
                assert_eq!(code, 2);
                assert_eq!(message, "contract criteria not met");
            }
            _ => panic!("wrong variant"),
        }
    }
}
