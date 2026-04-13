use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, ReviewRequest};

#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<ReviewArgs> for ReviewRequest {
    fn from(args: ReviewArgs) -> Self {
        ReviewRequest { json: args.json }
    }
}

pub async fn run(args: &ReviewArgs) -> Result<()> {
    let req = CommandRequest::Review(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
