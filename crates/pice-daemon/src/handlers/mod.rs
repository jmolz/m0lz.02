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
pub mod metrics;
pub mod plan;
pub mod prime;
pub mod review;
pub mod status;

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
    let static_ptr: *const (dyn StreamSink + 'static) =
        unsafe { std::mem::transmute(ptr) };
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

    /// Helper: assert a response is a non-empty Text or Json variant.
    fn assert_stub_response(resp: &CommandResponse) {
        match resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("stub"),
                    "stub should mention 'stub', got: {content}"
                );
            }
            CommandResponse::Json { value } => {
                assert_eq!(value["status"], "stub");
            }
            other => panic!("expected Text or Json stub, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_init() {
        let dir = tempfile::tempdir().unwrap();
        let ctx =
            DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Init(pice_core::cli::InitRequest {
            force: false,
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
        let ctx =
            DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
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
    async fn dispatch_evaluate() {
        let ctx = test_ctx();
        let req = CommandRequest::Evaluate(pice_core::cli::EvaluateRequest {
            plan_path: std::path::PathBuf::from("plan.md"),
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        assert_stub_response(&resp);
    }

    #[tokio::test]
    async fn dispatch_review_errors_without_provider() {
        let ctx = test_ctx();
        let req = CommandRequest::Review(pice_core::cli::ReviewRequest { json: false });
        let result = dispatch(req, &ctx, &NullSink).await;
        assert!(result.is_err(), "review should error without a provider");
    }

    #[tokio::test]
    async fn dispatch_commit() {
        let ctx = test_ctx();
        let req = CommandRequest::Commit(pice_core::cli::CommitRequest {
            message: None,
            dry_run: false,
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        assert_stub_response(&resp);
    }

    #[tokio::test]
    async fn dispatch_handoff() {
        let ctx = test_ctx();
        let req = CommandRequest::Handoff(pice_core::cli::HandoffRequest {
            output: None,
            json: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        assert_stub_response(&resp);
    }

    #[tokio::test]
    async fn dispatch_status() {
        let ctx = test_ctx();
        let req = CommandRequest::Status(pice_core::cli::StatusRequest { json: false });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        assert_stub_response(&resp);
    }

    #[tokio::test]
    async fn dispatch_metrics() {
        let ctx = test_ctx();
        let req = CommandRequest::Metrics(pice_core::cli::MetricsRequest {
            json: false,
            csv: false,
        });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        assert_stub_response(&resp);
    }

    #[tokio::test]
    async fn dispatch_benchmark() {
        let ctx = test_ctx();
        let req = CommandRequest::Benchmark(pice_core::cli::BenchmarkRequest { json: false });
        let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        assert_stub_response(&resp);
    }

    #[tokio::test]
    async fn dispatch_json_mode_returns_json_variant() {
        let dir = tempfile::tempdir().unwrap();
        let ctx =
            DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = CommandRequest::Init(pice_core::cli::InitRequest {
            force: false,
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
