use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, MetricsRequest};

#[derive(Args, Debug, Clone)]
pub struct MetricsArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Output as CSV
    #[arg(long)]
    pub csv: bool,
}

impl From<MetricsArgs> for MetricsRequest {
    fn from(args: MetricsArgs) -> Self {
        MetricsRequest {
            json: args.json,
            csv: args.csv,
        }
    }
}

pub async fn run(args: &MetricsArgs) -> Result<()> {
    let req = CommandRequest::Metrics(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
