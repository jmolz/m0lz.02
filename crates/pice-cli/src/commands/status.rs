//! `pice status` — display active plans and workflow state.
//!
//! Phase 7 introduces four invocation shapes, discriminated by the
//! CLI via [`StatusMode`]:
//! - `pice status`                           → `StatusMode::List`
//! - `pice status <feature_id>`              → `StatusMode::Detail`
//! - `pice status --follow [<feature_id>]`   → `StatusMode::Follow`
//! - `pice status --wait <feature_id>`       → `StatusMode::Wait`
//!
//! Clap-enforced conflict rules (locked by `stream_json_flag_validation`):
//! - `--follow` conflicts with `--wait`
//! - `--wait` requires a `feature_id`
//! - `--json` conflicts with `--follow` (single-JSON-object invariant)
//! - `--stream-json` requires `--follow`
//!
//! `Follow` and `Wait` bypass `cli/dispatch` at the CLI layer and
//! route directly to the router-level `manifest/subscribe` RPC. See
//! Task 12 for the subscribe plumbing.

use anyhow::{Context, Result};
use clap::Args;
use pice_core::cli::{CommandRequest, StatusMode, StatusRequest};

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Feature id to inspect. Required for `--wait`; optional for
    /// `--follow` (omit to tail every manifest).
    pub feature_id: Option<String>,

    /// Stream live manifest updates as events arrive. Conflicts with
    /// `--wait` and `--json`.
    #[arg(long, conflicts_with_all = ["wait", "json"])]
    pub follow: bool,

    /// Block until the feature reaches a terminal status. Requires a
    /// `feature_id` positional.
    #[arg(long, requires = "feature_id", conflicts_with = "follow")]
    pub wait: bool,

    /// Max seconds to wait before returning exit 4 (`WaitTimeout`).
    /// Requires `--wait`.
    #[arg(long, value_name = "N", requires = "wait")]
    pub timeout_secs: Option<u64>,

    /// Output as a single JSON object. Conflicts with `--follow` (which
    /// emits an NDJSON stream) — the JSON mode guarantees a single
    /// top-level object.
    #[arg(long)]
    pub json: bool,

    /// Emit heterogeneous `StreamJsonFrame` NDJSON frames. Requires
    /// `--follow`.
    #[arg(long, requires = "follow")]
    pub stream_json: bool,
}

impl StatusArgs {
    /// Compute the [`StatusMode`] from parsed flags. Called after clap
    /// has validated the flag combinations.
    pub fn mode(&self) -> StatusMode {
        if self.follow {
            StatusMode::Follow
        } else if self.wait {
            StatusMode::Wait
        } else if self.feature_id.is_some() {
            StatusMode::Detail
        } else {
            StatusMode::List
        }
    }
}

impl TryFrom<StatusArgs> for StatusRequest {
    type Error = anyhow::Error;

    fn try_from(args: StatusArgs) -> Result<Self> {
        let mode = args.mode();
        // Defense-in-depth: clap enforces `--wait requires feature_id`,
        // but the daemon handler trusts this invariant, so we double-check.
        if mode == StatusMode::Wait && args.feature_id.is_none() {
            anyhow::bail!("--wait requires a feature_id positional");
        }
        Ok(StatusRequest {
            json: args.json,
            mode,
            feature_id: args.feature_id,
            stream_json: args.stream_json,
            timeout_secs: args.timeout_secs,
        })
    }
}

pub async fn run(args: &StatusArgs) -> Result<()> {
    let req: StatusRequest = args
        .clone()
        .try_into()
        .context("invalid pice status arguments")?;
    let req = CommandRequest::Status(req);
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}
