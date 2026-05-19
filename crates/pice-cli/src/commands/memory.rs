use anyhow::Result;
use clap::{Args, Subcommand};
use pice_core::cli::{CommandRequest, MemoryRequest, MemorySubcommand};

#[derive(Args, Debug, Clone)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub subcommand: MemoryCommand,

    /// Output as JSON
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum MemoryCommand {
    /// Report memory configuration and record counts.
    Status,
    /// List memory record metadata.
    List {
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        feature: Option<String>,
    },
    /// Show one redacted memory record by id.
    Show { record_id: String },
    /// Prune records before a UTC day boundary (YYYY-MM-DD).
    Prune {
        #[arg(long)]
        before: Option<String>,
    },
    /// Delete one memory record by id.
    Delete { record_id: String },
}

impl From<MemoryArgs> for MemoryRequest {
    fn from(args: MemoryArgs) -> Self {
        let subcommand = match args.subcommand {
            MemoryCommand::Status => MemorySubcommand::Status,
            MemoryCommand::List { limit, feature } => MemorySubcommand::List { limit, feature },
            MemoryCommand::Show { record_id } => MemorySubcommand::Show { record_id },
            MemoryCommand::Prune { before } => MemorySubcommand::Prune { before },
            MemoryCommand::Delete { record_id } => MemorySubcommand::Delete { record_id },
        };
        MemoryRequest {
            subcommand,
            json: args.json,
        }
    }
}

pub async fn run(args: &MemoryArgs) -> Result<()> {
    let req = CommandRequest::Memory(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
