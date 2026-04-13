use anyhow::Result;
use clap::Args;
use pice_core::cli::{BenchmarkRequest, CommandRequest};

#[derive(Args, Debug, Clone)]
pub struct BenchmarkArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<BenchmarkArgs> for BenchmarkRequest {
    fn from(args: BenchmarkArgs) -> Self {
        BenchmarkRequest { json: args.json }
    }
}

pub async fn run(args: &BenchmarkArgs) -> Result<()> {
    let req = CommandRequest::Benchmark(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
