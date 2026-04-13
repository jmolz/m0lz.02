use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, PrimeRequest};

#[derive(Args, Debug, Clone)]
pub struct PrimeArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<PrimeArgs> for PrimeRequest {
    fn from(args: PrimeArgs) -> Self {
        PrimeRequest { json: args.json }
    }
}

pub async fn run(args: &PrimeArgs) -> Result<()> {
    let req = CommandRequest::Prime(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
