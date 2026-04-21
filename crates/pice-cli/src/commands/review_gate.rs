//! `pice review-gate` command — list pending review gates or record a
//! decision.
//!
//! Two modes:
//! - `--list`: dispatch `ReviewGateRequest::List`, render table/JSON.
//! - `--gate-id <id> --decision <approve|reject|skip> [--reason <txt>]`:
//!   record a decision. When `--decision` is omitted AND stdin is a TTY,
//!   prompt via `TtyDecisionSource` (see `crates/pice-cli/src/input/`).
//!
//! Reviewer resolution is CLI-side only: `$USER` or `$USERNAME` env, with
//! `unknown` as the last-resort fallback. The daemon never reads env.
//! See `.claude/rules/daemon.md` → "Channel ownership invariant".

use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, ReviewGateRequest, ReviewGateSubcommand};
use pice_core::gate::GateDecision;

#[derive(Args, Debug, Clone)]
pub struct ReviewGateArgs {
    /// List all pending review gates across features.
    #[arg(long)]
    pub list: bool,

    /// Target a specific pending gate by its id (required for `--decision`).
    #[arg(long)]
    pub gate_id: Option<String>,

    /// Record a decision on the gate named by `--gate-id`.
    /// Valid values: `approve`, `reject`, `skip`.
    #[arg(long, value_enum)]
    pub decision: Option<DecisionArg>,

    /// Optional free-text rationale for the decision. Persisted to the
    /// `gate_decisions.reason` column of the SQLite audit trail.
    #[arg(long)]
    pub reason: Option<String>,

    /// Filter `--list` to a specific feature id.
    #[arg(long, requires = "list")]
    pub feature_id: Option<String>,

    /// Emit JSON to stdout (suppresses human-friendly rendering).
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum DecisionArg {
    Approve,
    Reject,
    Skip,
}

impl From<DecisionArg> for GateDecision {
    fn from(d: DecisionArg) -> Self {
        match d {
            DecisionArg::Approve => GateDecision::Approve,
            DecisionArg::Reject => GateDecision::Reject,
            DecisionArg::Skip => GateDecision::Skip,
        }
    }
}

/// Resolve the reviewer name for audit attribution. CLI-side only so the
/// daemon never touches the caller's environment.
fn resolve_reviewer() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

pub async fn run(args: &ReviewGateArgs) -> Result<()> {
    // Mode selection.
    if args.list {
        let req = CommandRequest::ReviewGate(ReviewGateRequest {
            json: args.json,
            subcommand: ReviewGateSubcommand::List {
                feature_id: args.feature_id.clone(),
            },
        });
        let resp = crate::adapter::dispatch(req).await?;
        return super::render_response(resp);
    }

    // Decide mode: requires --gate-id.
    let Some(gate_id) = args.gate_id.clone() else {
        // No --list, no --gate-id → emit MissingDecision.
        use pice_core::cli::ExitJsonStatus;
        if args.json {
            let payload = serde_json::json!({
                "status": ExitJsonStatus::MissingDecision.as_str(),
                "error": "pice review-gate requires --list or --gate-id <id>",
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            eprintln!("error: pice review-gate requires either --list or --gate-id <id>");
            eprintln!("hint: run `pice review-gate --list` to see pending gates");
        }
        std::process::exit(1);
    };

    // Decision: either provided via flag, or prompted via TTY.
    let decision: GateDecision = if let Some(d) = args.decision {
        d.into()
    } else if is_tty() {
        // TTY prompt — future work (Phase 6 Task 18 will weave this into
        // `pice evaluate`'s auto-resume loop too). For the standalone
        // `pice review-gate` command, dispatch a lightweight TTY prompt
        // via DecisionSource.
        prompt_tty_for_decision(&gate_id)?
    } else {
        use pice_core::cli::ExitJsonStatus;
        if args.json {
            let payload = serde_json::json!({
                "status": ExitJsonStatus::MissingDecision.as_str(),
                "error": "non-TTY invocation requires --decision",
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            eprintln!("error: non-TTY invocation of pice review-gate requires --decision");
        }
        std::process::exit(1);
    };

    let reviewer = resolve_reviewer();
    let req = CommandRequest::ReviewGate(ReviewGateRequest {
        json: args.json,
        subcommand: ReviewGateSubcommand::Decide {
            gate_id,
            decision,
            reviewer,
            reason: args.reason.clone(),
        },
    });
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Drive the TTY prompt loop for a single gate. Returns the reviewer's
/// chosen decision. A `[d]etails` response re-prompts once with details;
/// after that, the loop expects a terminal decision.
///
/// Implementation note: this function reads directly from `stdin` /
/// writes to `stderr` rather than through the `DecisionSource` trait
/// because `StdinLock` is not `Send` (the trait requires Send for
/// future cross-thread uses the orchestrator may take). The trait is
/// still used in unit tests via `ScriptedDecisionSource`.
fn prompt_tty_for_decision(gate_id: &str) -> Result<GateDecision> {
    use crate::input::decision_source::render_prompt;
    use std::io::{stderr, stdin, Write};
    let prompt_body = format!("gate id: {gate_id}");
    let rendered = render_prompt(&prompt_body, None);
    for _attempt in 0..5 {
        {
            let mut err = stderr();
            writeln!(err, "{rendered}")?;
            err.flush()?;
        }
        let mut line = String::new();
        stdin().read_line(&mut line)?;
        let ch = line
            .trim()
            .chars()
            .next()
            .map(|c| c.to_ascii_lowercase())
            .unwrap_or(' ');
        match ch {
            'a' => return Ok(GateDecision::Approve),
            'r' => return Ok(GateDecision::Reject),
            's' => return Ok(GateDecision::Skip),
            'd' => {
                eprintln!(
                    "details view is not yet wired (Phase 7); please pick [a]pprove, [r]eject, or [s]kip",
                );
                continue;
            }
            other => {
                eprintln!("invalid response '{other}'; please pick one of a/r/d/s");
                continue;
            }
        }
    }
    anyhow::bail!("no valid decision after 5 prompts")
}
