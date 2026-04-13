use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, EvaluateRequest};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct EvaluateArgs {
    /// Path to the plan file to evaluate against
    pub plan_path: PathBuf,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<EvaluateArgs> for EvaluateRequest {
    fn from(args: EvaluateArgs) -> Self {
        EvaluateRequest {
            plan_path: args.plan_path,
            json: args.json,
        }
    }
}

pub async fn run(args: &EvaluateArgs) -> Result<()> {
    let req = CommandRequest::Evaluate(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
