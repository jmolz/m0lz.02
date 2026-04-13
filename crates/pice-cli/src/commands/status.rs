use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, StatusRequest};

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<StatusArgs> for StatusRequest {
    fn from(args: StatusArgs) -> Self {
        StatusRequest { json: args.json }
    }
}

pub async fn run(args: &StatusArgs) -> Result<()> {
    let req = CommandRequest::Status(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
