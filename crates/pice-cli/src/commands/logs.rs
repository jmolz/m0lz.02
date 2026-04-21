//! `pice logs <feature_id>` — inspect or tail captured provider
//! session logs for a background feature run.
//!
//! Phase 7 shape:
//! - `pice logs <feature_id>`               → one-shot snapshot via
//!   `cli/dispatch::Logs`
//! - `pice logs <feature_id> --follow`      → router-level
//!   `logs/stream` RPC with `follow: true` (live tail)
//! - `pice logs <feature_id> --layer L`     → filter buffered history
//!   to a specific layer
//!
//! Clap-enforced conflict rules (parity with `pice status`):
//! - `--json` conflicts with `--follow` (single-JSON-object invariant)
//! - `--stream-json` requires `--follow`
//!
//! The Task 13 CLI-side `--follow` implementation will open the
//! dedicated `logs/stream` connection; the one-shot path continues
//! to dispatch through `cli/dispatch::Logs`.

use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, LogsRequest};

#[derive(Args, Debug, Clone)]
pub struct LogsArgs {
    /// Feature id whose captured session logs to inspect.
    pub feature_id: String,

    /// Restrict output to a specific layer (filters buffered history
    /// and live chunks).
    #[arg(long)]
    pub layer: Option<String>,

    /// Tail live log chunks as they are emitted. Conflicts with
    /// `--json`; the follow path emits an NDJSON stream instead of a
    /// single top-level JSON object.
    #[arg(long, conflicts_with = "json")]
    pub follow: bool,

    /// Output as a single JSON object. Conflicts with `--follow`.
    #[arg(long)]
    pub json: bool,

    /// Emit heterogeneous `StreamJsonFrame` NDJSON frames. Requires
    /// `--follow`.
    #[arg(long, requires = "follow")]
    pub stream_json: bool,

    /// Include buffered history in the response. Default `true`; set
    /// `--no-include-history` when you only want live chunks going
    /// forward under `--follow`.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub include_history: bool,
}

impl From<LogsArgs> for LogsRequest {
    fn from(args: LogsArgs) -> Self {
        LogsRequest {
            feature_id: args.feature_id,
            layer: args.layer,
            follow: args.follow,
            json: args.json,
            stream_json: args.stream_json,
            include_history: args.include_history,
        }
    }
}

pub async fn run(args: &LogsArgs) -> Result<()> {
    let req = CommandRequest::Logs(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
