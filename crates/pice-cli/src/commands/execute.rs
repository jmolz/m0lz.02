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
}

impl From<ExecuteArgs> for ExecuteRequest {
    fn from(args: ExecuteArgs) -> Self {
        ExecuteRequest {
            plan_path: args.plan_path,
            json: args.json,
        }
    }
}

pub async fn run(args: &ExecuteArgs) -> Result<()> {
    let req = CommandRequest::Execute(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
