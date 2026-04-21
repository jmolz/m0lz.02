//! `pice audit` command — export audit trails (Phase 6: gate decisions).
//!
//! The first and only subcommand today is `gates`; later phases may
//! add `seams` or `costs`. Follows the `pice layers` subcommand
//! pattern so flags + dispatch wiring stays uniform across the CLI.

use anyhow::Result;
use clap::{Args, Subcommand};
use pice_core::cli::{AuditRequest, AuditSubcommand, CommandRequest};

#[derive(Args, Debug, Clone)]
pub struct AuditArgs {
    #[command(subcommand)]
    pub subcommand: AuditCommand,

    /// Emit JSON to stdout (suppresses human-friendly rendering).
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuditCommand {
    /// Export gate decisions (CSV by default, `--json` for JSON).
    Gates {
        /// Filter to a single feature id.
        #[arg(long)]
        feature: Option<String>,
        /// RFC3339 lower bound on `requested_at` (inclusive).
        #[arg(long)]
        since: Option<String>,
        /// Emit CSV. Mutually exclusive with `--json` on the parent.
        #[arg(long)]
        csv: bool,
    },
}

impl From<AuditArgs> for AuditRequest {
    fn from(args: AuditArgs) -> Self {
        let subcommand = match args.subcommand {
            AuditCommand::Gates {
                feature,
                since,
                csv,
            } => AuditSubcommand::Gates {
                feature_id: feature,
                since,
                csv,
            },
        };
        AuditRequest {
            subcommand,
            json: args.json,
        }
    }
}

pub async fn run(args: &AuditArgs) -> Result<()> {
    let req = CommandRequest::Audit(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
