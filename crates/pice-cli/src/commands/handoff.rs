use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, HandoffRequest};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct HandoffArgs {
    /// Custom output path (default: HANDOFF.md in project root)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<HandoffArgs> for HandoffRequest {
    fn from(args: HandoffArgs) -> Self {
        HandoffRequest {
            output: args.output,
            json: args.json,
        }
    }
}

pub async fn run(args: &HandoffArgs) -> Result<()> {
    let req = CommandRequest::Handoff(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
