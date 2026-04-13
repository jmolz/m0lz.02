use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, PlanRequest};

#[derive(Args, Debug, Clone)]
pub struct PlanArgs {
    /// Description of what to plan
    pub description: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<PlanArgs> for PlanRequest {
    fn from(args: PlanArgs) -> Self {
        PlanRequest {
            description: args.description,
            json: args.json,
        }
    }
}

pub async fn run(args: &PlanArgs) -> Result<()> {
    let req = CommandRequest::Plan(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
