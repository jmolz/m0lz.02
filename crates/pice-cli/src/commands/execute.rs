//! `pice execute` — implement from a plan file.
//!
//! Phase 7 adds three background-execution flags:
//! - `--background`: dispatch the execute orchestrator as a detached
//!   tokio task in the daemon and return immediately with
//!   `{feature_id, run_id, status: background-dispatched}`.
//! - `--wait`: combined with `--background`, the CLI opens a second
//!   subscribe connection and blocks until the feature reaches a
//!   terminal status (requires `--background`).
//! - `--timeout-secs N`: bound the `--wait` path (requires `--wait`).
//!
//! The waiting semantics are CLI-side only. The daemon dispatch handler
//! never reads `wait` / `timeout_secs` — the CLI opens its own
//! `manifest/subscribe` connection after dispatch.

use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, ExecuteRequest};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct ExecuteArgs {
    /// Path to the plan file to execute
    pub plan_path: PathBuf,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Phase 7: dispatch the execute orchestrator as a detached tokio
    /// task in the daemon. Returns `{feature_id, run_id, status:
    /// background-dispatched}` within the 500ms p95 SLO.
    #[arg(long)]
    pub background: bool,

    /// Phase 7: with `--background`, block until terminal status via a
    /// second subscribe connection. Requires `--background`.
    #[arg(long, requires = "background")]
    pub wait: bool,

    /// Phase 7: max seconds to wait before returning exit 4
    /// (`WaitTimeout`). Requires `--wait`.
    #[arg(long, value_name = "N", requires = "wait")]
    pub timeout_secs: Option<u64>,
}

impl From<ExecuteArgs> for ExecuteRequest {
    fn from(args: ExecuteArgs) -> Self {
        ExecuteRequest {
            plan_path: args.plan_path,
            json: args.json,
            background: args.background,
            wait: args.wait,
            timeout_secs: args.timeout_secs,
        }
    }
}

pub async fn run(args: &ExecuteArgs) -> Result<()> {
    let req = CommandRequest::Execute(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
