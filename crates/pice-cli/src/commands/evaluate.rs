//! `pice evaluate` — contract evaluation + Phase 6 TTY auto-resume loop.
//!
//! When the daemon returns a `ReviewGatePending` exit-3 payload AND
//! stdin/stderr are a TTY AND `--json` is not set, the CLI drives the
//! prompt → decide → re-invoke loop in-process until the feature reaches
//! a terminal status. In all other cases (CI, pipes, `--json` scripts),
//! exit 3 propagates so the caller can `while [ $? -eq 3 ]; do ...; done`.
//!
//! See `.claude/rules/daemon.md` → "Channel ownership invariant" for why
//! prompts land on stderr.

use anyhow::Result;
use clap::Args;
use pice_core::cli::{
    CommandRequest, CommandResponse, EvaluateRequest, ExitJsonStatus, ReviewGateRequest,
    ReviewGateSubcommand,
};
use pice_core::gate::GateDecision;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct EvaluateArgs {
    /// Path to the plan file to evaluate against
    pub plan_path: PathBuf,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl From<EvaluateArgs> for EvaluateRequest {
    fn from(args: EvaluateArgs) -> Self {
        EvaluateRequest {
            plan_path: args.plan_path,
            json: args.json,
        }
    }
}

pub async fn run(args: &EvaluateArgs) -> Result<()> {
    // Phase 6 TTY auto-resume: loop evaluate → (on pending-review) prompt
    // → decide → evaluate, until a terminal status. In non-TTY mode a
    // pending-review response propagates as exit 3 immediately.
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        let req = CommandRequest::Evaluate(args.clone().into());
        let resp = crate::adapter::dispatch(req).await?;

        if !is_review_gate_pending(&resp) {
            return super::render_response(resp);
        }

        // TTY auto-resume: --json or non-TTY callers propagate exit 3.
        if args.json || !is_tty() {
            return super::render_response(resp);
        }

        // TTY mode: prompt for each pending gate, issue decide RPCs.
        let pending_gates = extract_pending_gates(&resp);
        if pending_gates.is_empty() {
            // Nothing to action — render and bail so we don't loop forever.
            return super::render_response(resp);
        }
        for gate in &pending_gates {
            let decision = prompt_decision_for_gate(gate)?;
            let decide = CommandRequest::ReviewGate(ReviewGateRequest {
                json: false,
                subcommand: ReviewGateSubcommand::Decide {
                    gate_id: gate.id.clone(),
                    decision,
                    reviewer: resolve_reviewer(),
                    reason: None,
                },
            });
            let dresp = crate::adapter::dispatch(decide).await?;
            // If the decision halted the feature (exit 2 =
            // review-gate-rejected), render + exit.
            if let CommandResponse::ExitJson { code, .. } | CommandResponse::Exit { code, .. } =
                &dresp
            {
                if *code == 2 {
                    return super::render_response(dresp);
                }
            }
        }

        // Safety: bound the loop so a misbehaving daemon cannot spin
        // forever. After 10 rounds something is clearly wrong.
        if attempts >= 10 {
            eprintln!(
                "evaluate: exceeded 10 auto-resume attempts; giving up. Check pending gates manually."
            );
            std::process::exit(1);
        }
        // Fall through — loop re-invokes evaluate, daemon picks up from
        // the next cohort.
    }
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn is_review_gate_pending(resp: &CommandResponse) -> bool {
    match resp {
        CommandResponse::ExitJson { code: 3, value } => value
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s == ExitJsonStatus::ReviewGatePending.as_str())
            .unwrap_or(false),
        CommandResponse::Exit { code: 3, .. } => true,
        _ => false,
    }
}

/// Minimal view of a pending gate pulled from the daemon's JSON response.
struct PendingGateSummary {
    id: String,
    layer: String,
    trigger_expression: String,
}

fn extract_pending_gates(resp: &CommandResponse) -> Vec<PendingGateSummary> {
    let value = match resp {
        CommandResponse::ExitJson { value, .. } => value,
        _ => return Vec::new(),
    };
    let Some(arr) = value.get("pending_gates").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|g| {
            Some(PendingGateSummary {
                id: g.get("id")?.as_str()?.to_string(),
                layer: g.get("layer")?.as_str()?.to_string(),
                trigger_expression: g.get("trigger_expression")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn prompt_decision_for_gate(gate: &PendingGateSummary) -> Result<GateDecision> {
    use crate::input::decision_source::render_prompt;
    use std::io::{stderr, stdin, Write};
    let body = format!(
        "layer: {}\ntrigger: {}\nid: {}",
        gate.layer, gate.trigger_expression, gate.id
    );
    let rendered = render_prompt(&body, None);
    for _ in 0..5 {
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
            'd' => eprintln!("details view deferred to Phase 7; pick a/r/s"),
            other => eprintln!("invalid '{other}'; expect a/r/d/s"),
        }
    }
    anyhow::bail!("no valid decision after 5 prompts for gate {}", gate.id)
}

fn resolve_reviewer() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}
