//! Per-command async handlers — one module per `CommandRequest` variant.
//!
//! Each handler has the signature:
//!
//! ```ignore
//! pub async fn run(
//!     req: XxxRequest,
//!     ctx: &DaemonContext,
//!     sink: &dyn StreamSink,
//! ) -> anyhow::Result<CommandResponse>
//! ```
//!
//! The [`dispatch`] function matches on `CommandRequest` and calls the
//! appropriate handler. The router invokes `dispatch` from `handle_dispatch`
//! after authenticating the request.
//!
//! ## Phase 0 status
//!
//! All 11 handlers are stubbed — they return placeholder `CommandResponse`
//! values. The full body port from `pice-cli/src/commands/` happens
//! incrementally as dependencies (templates, provider sessions) become
//! available in the daemon crate.
//!
//! `Completions` is NOT a handler (handled by `clap_complete` at the CLI
//! layer). The `Daemon` subcommand (T24) is also CLI-only.

pub mod benchmark;
pub mod commit;
pub mod evaluate;
pub mod execute;
pub mod handoff;
pub mod init;
pub mod layers;
pub mod metrics;
pub mod plan;
pub mod prime;
pub mod review;
pub mod status;
pub mod validate;

use std::sync::Arc;

use anyhow::Result;
use pice_core::cli::{CommandRequest, CommandResponse};

use crate::orchestrator::{SharedSink, StreamEvent, StreamSink};
use crate::server::router::DaemonContext;

/// Bridge a borrowed `&dyn StreamSink` to a `SharedSink` for the session runner.
///
/// Safety: the returned `SharedSink` must not outlive the borrowed `sink`.
/// This is guaranteed by the handler pattern: the session is awaited to
/// completion before the handler returns, so the `Arc` is dropped before
/// the borrow expires.
pub(crate) fn to_shared_sink(sink: &dyn StreamSink) -> SharedSink {
    // SAFETY: We erase the lifetime by going through a raw pointer. The
    // returned Arc must not outlive the borrowed sink. This is guaranteed by
    // the handler pattern: the session is awaited to completion before the
    // handler returns, so the Arc is dropped before the borrow expires.
    let ptr: *const dyn StreamSink = sink;
    let static_ptr: *const (dyn StreamSink + 'static) = unsafe { std::mem::transmute(ptr) };
    Arc::new(SinkBridge(static_ptr))
}

struct SinkBridge(*const dyn StreamSink);

// SAFETY: The pointer is only dereferenced while the original reference is alive.
// The handler awaits the session to completion, then drops the Arc, before returning.
unsafe impl Send for SinkBridge {}
unsafe impl Sync for SinkBridge {}

impl StreamSink for SinkBridge {
    fn send_chunk(&self, text: &str) {
        // SAFETY: guaranteed alive by handler pattern (see to_shared_sink doc)
        unsafe { &*self.0 }.send_chunk(text);
    }

    fn send_event(&self, event: StreamEvent) {
        // SAFETY: guaranteed alive by handler pattern (see to_shared_sink doc)
        unsafe { &*self.0 }.send_event(event);
    }
}

/// Dispatch a `CommandRequest` to the appropriate handler.
///
/// Called by the router's `handle_dispatch` after deserializing `params` into
/// a `CommandRequest`. Streams output via `sink` during execution, then
/// returns the final `CommandResponse`.
pub async fn dispatch(
    req: CommandRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    match req {
        CommandRequest::Init(r) => init::run(r, ctx, sink).await,
        CommandRequest::Prime(r) => prime::run(r, ctx, sink).await,
        CommandRequest::Plan(r) => plan::run(r, ctx, sink).await,
        CommandRequest::Execute(r) => execute::run(r, ctx, sink).await,
        CommandRequest::Evaluate(r) => evaluate::run(r, ctx, sink).await,
        CommandRequest::Review(r) => review::run(r, ctx, sink).await,
        CommandRequest::Commit(r) => commit::run(r, ctx, sink).await,
        CommandRequest::Handoff(r) => handoff::run(r, ctx, sink).await,
        CommandRequest::Status(r) => status::run(r, ctx, sink).await,
        CommandRequest::Metrics(r) => metrics::run(r, ctx, sink).await,
        CommandRequest::Benchmark(r) => benchmark::run(r, ctx, sink).await,
        CommandRequest::Layers(r) => layers::run(r, ctx, sink).await,
        CommandRequest::Validate(r) => validate::run(r, ctx, sink).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NullSink;

    /// Helper: create a test DaemonContext.
    fn test_ctx() -> DaemonContext {
        DaemonContext::new_for_test("test-token")
    }

    #[tokio::test]
    async fn dispatch_init() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Init(pice_core::cli::InitRequest {
            force: false,
            upgrade: false,
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("PICE initialized"),
                    "init should report success, got: {content}"
                );
            }
            other => panic!("expected Text response from init, got: {other:?}"),
        }
        assert!(dir.path().join(".claude/commands/plan-feature.md").exists());
        assert!(dir.path().join(".pice/config.toml").exists());
    }

    /// Provider-backed handlers (prime, plan, execute, review) require a real
    /// provider binary and are tested in integration tests. Unit tests verify
    /// early-exit paths (e.g., missing plan file) or that the handler returns
    /// an error when the provider is unavailable.

    #[tokio::test]
    async fn dispatch_prime_errors_without_provider() {
        let ctx = test_ctx();
        let req = CommandRequest::Prime(pice_core::cli::PrimeRequest { json: false });
        // Provider startup will fail in the test environment — that's expected.
        let result = dispatch(req, &ctx, &NullSink).await;
        assert!(result.is_err(), "prime should error without a provider");
    }

    #[tokio::test]
    async fn dispatch_plan_errors_without_provider() {
        let ctx = test_ctx();
        let req = CommandRequest::Plan(pice_core::cli::PlanRequest {
            description: "test feature".to_string(),
            json: false,
        });
        let result = dispatch(req, &ctx, &NullSink).await;
        assert!(result.is_err(), "plan should error without a provider");
    }

    #[tokio::test]
    async fn dispatch_execute_missing_plan_returns_exit() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Execute(pice_core::cli::ExecuteRequest {
            plan_path: std::path::PathBuf::from("nonexistent-plan.md"),
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(*code, 1);
                assert!(
                    message.contains("not found"),
                    "should mention plan not found, got: {message}"
                );
            }
            other => panic!("expected Exit response for missing plan, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_evaluate_missing_plan() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Evaluate(pice_core::cli::EvaluateRequest {
            plan_path: std::path::PathBuf::from("plan.md"),
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(*code, 1);
                assert!(
                    message.contains("plan file not found"),
                    "expected 'plan file not found', got: {message}"
                );
            }
            other => panic!("expected Exit{{code:1}} for missing plan, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_evaluate_v01_fallback_emits_warning() {
        // A valid plan with contract, but NO .pice/layers.toml → v0.1 fallback
        // with warning message via sink.send_chunk().
        let dir = tempfile::tempdir().unwrap();

        // Create a plan with a contract section
        let plans_dir = dir.path().join(".claude/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        std::fs::write(
            plans_dir.join("test-plan.md"),
            r#"# Plan

## Contract

```json
{
  "feature": "test",
  "tier": 1,
  "pass_threshold": 8,
  "criteria": [
    {"name": "works", "threshold": 8, "validation": "manual"}
  ]
}
```
"#,
        )
        .unwrap();

        // Create .pice/config.toml so the project is initialized
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("config.toml"),
            r#"
[provider]
name = "claude-code"
[evaluation]
[evaluation.primary]
provider = "claude-code"
model = "claude-sonnet-4-20250514"
[evaluation.adversarial]
provider = "codex"
model = "o3-mini"
effort = "high"
enabled = false
[evaluation.tiers]
tier1_models = ["claude-sonnet-4-20250514"]
tier2_models = ["claude-sonnet-4-20250514"]
tier3_models = ["claude-sonnet-4-20250514"]
tier3_agent_team = false
[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"
[metrics]
db_path = ".pice/metrics.db"
"#,
        )
        .unwrap();

        // NO .pice/layers.toml — this is the v0.1 case

        // Use a CaptureSink to verify the warning
        use std::sync::Mutex;
        struct CaptureSink {
            chunks: Mutex<Vec<String>>,
        }
        impl StreamSink for CaptureSink {
            fn send_chunk(&self, text: &str) {
                self.chunks.lock().unwrap().push(text.to_string());
            }
            fn send_event(&self, _event: StreamEvent) {}
        }

        let sink = CaptureSink {
            chunks: Mutex::new(Vec::new()),
        };
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Evaluate(pice_core::cli::EvaluateRequest {
            plan_path: std::path::PathBuf::from(".claude/plans/test-plan.md"),
            json: false,
        });

        // The handler will emit the v0.1 warning, then try to start a provider
        // (which will fail). We care about the warning being emitted.
        let _result = dispatch(req, &ctx, &sink).await;

        let chunks = sink.chunks.lock().unwrap();
        let all_output = chunks.join("");
        assert!(
            all_output.contains("No .pice/layers.toml found"),
            "should emit v0.1 fallback warning, got chunks: {:?}",
            *chunks
        );
        assert!(
            all_output.contains("single-loop evaluation"),
            "should mention single-loop evaluation, got chunks: {:?}",
            *chunks
        );
    }

    #[tokio::test]
    async fn dispatch_review_errors_without_provider() {
        let ctx = test_ctx();
        let req = CommandRequest::Review(pice_core::cli::ReviewRequest { json: false });
        let result = dispatch(req, &ctx, &NullSink).await;
        assert!(result.is_err(), "review should error without a provider");
    }

    #[tokio::test]
    async fn dispatch_commit_nothing_staged() {
        // Use a temp dir with a git repo but no staged changes
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Commit(pice_core::cli::CommitRequest {
            message: None,
            dry_run: false,
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(*code, 1);
                assert!(
                    message.contains("nothing staged"),
                    "expected 'nothing staged', got: {message}"
                );
            }
            other => panic!("expected Exit response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_commit_with_message_dry_run() {
        // Use a temp dir with a git repo and staged changes
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("file.rs"), "fn main() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "file.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Commit(pice_core::cli::CommitRequest {
            message: Some("test: dry run commit".to_string()),
            dry_run: true,
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("Dry run"),
                    "expected dry run output, got: {content}"
                );
                assert!(content.contains("test: dry run commit"));
            }
            other => panic!("expected Text response for dry run, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_handoff_errors_without_provider() {
        // Handoff requires a provider. Point config at a nonexistent provider
        // so ProviderOrchestrator::start fails immediately.
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("config.toml"),
            r#"
[provider]
name = "nonexistent-provider"
[evaluation]
[evaluation.primary]
provider = "nonexistent-provider"
model = "fake"
[evaluation.adversarial]
provider = "nonexistent-provider"
model = "fake"
effort = "high"
enabled = false
[evaluation.tiers]
tier1_models = ["fake"]
tier2_models = ["fake"]
tier3_models = ["fake"]
tier3_agent_team = false
[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"
[metrics]
db_path = ".pice/metrics.db"
"#,
        )
        .unwrap();

        // Construct context — it loads config from the temp dir
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());

        let req = CommandRequest::Handoff(pice_core::cli::HandoffRequest {
            output: None,
            json: false,
        });
        // Provider start will fail — verify it returns an error, not a panic
        let result = dispatch(req, &ctx, &NullSink).await;
        assert!(result.is_err(), "expected provider start error");
    }

    #[tokio::test]
    async fn dispatch_status() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Status(pice_core::cli::StatusRequest { json: false });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("PICE Status"),
                    "status should contain header, got: {content}"
                );
            }
            other => panic!("expected Text response for status, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Metrics(pice_core::cli::MetricsRequest {
            json: false,
            csv: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("metrics")
                        || content.contains("Metrics")
                        || content.contains("pice init"),
                    "metrics should return text output, got: {content}"
                );
            }
            other => panic!("expected Text response for metrics, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Benchmark(pice_core::cli::BenchmarkRequest { json: false });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("Benchmark"),
                    "benchmark should contain header, got: {content}"
                );
            }
            other => panic!("expected Text response for benchmark, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_json_mode_returns_json_variant() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Init(pice_core::cli::InitRequest {
            force: false,
            upgrade: false,
            json: true,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        match resp {
            CommandResponse::Json { value } => {
                assert!(
                    value["totalCreated"].as_u64().unwrap() > 0,
                    "init json should report created files"
                );
            }
            other => panic!("json mode should return Json variant, got: {other:?}"),
        }
    }
}
