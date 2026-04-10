use anyhow::{bail, Result};
use clap::Args;
use std::path::Path;
use std::process::Command;
use tracing::info;

use crate::metrics;
use pice_core::config::PiceConfig;
use pice_daemon::orchestrator::{session, NullSink, ProviderOrchestrator, SharedSink};
use pice_daemon::prompt;
use std::sync::Arc;

#[derive(Args, Debug)]
pub struct CommitArgs {
    /// Override commit message (skip AI generation)
    #[arg(short, long)]
    pub message: Option<String>,

    /// Show generated message without committing
    #[arg(long)]
    pub dry_run: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &CommitArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    // Check if there's anything to commit
    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&project_root)
        .output()?;
    if !status_output.status.success() {
        bail!("git status failed — is this a git repository?");
    }
    if status_output.stdout.is_empty() {
        if args.json {
            let output = serde_json::json!({
                "status": "error",
                "error": "nothing to commit",
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("Nothing to commit.");
        }
        bail!("nothing to commit");
    }

    // Stage tracked changes if nothing is explicitly staged (before message generation
    // so the prompt reflects what will actually be committed)
    let auto_staged = {
        let staged_check = Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(&project_root)
            .status()?;
        if staged_check.success() {
            Command::new("git")
                .args(["add", "-u"])
                .current_dir(&project_root)
                .status()?;

            // Verify something is now staged (git add -u doesn't stage untracked files)
            let recheck = Command::new("git")
                .args(["diff", "--cached", "--quiet"])
                .current_dir(&project_root)
                .status()?;
            if recheck.success() {
                if args.json {
                    let output = serde_json::json!({
                        "status": "error",
                        "error": "nothing staged to commit -- use git add to stage new files",
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    println!("Nothing staged to commit. Use `git add` to stage new files.");
                }
                bail!("nothing staged to commit");
            }
            true
        } else {
            false
        }
    };

    // Helper: undo auto-staging on error/dry-run paths
    let rollback_staging = || {
        Command::new("git")
            .args(["reset"])
            .current_dir(&project_root)
            .status()
            .ok();
    };

    // Get or generate commit message (after staging, so the prompt matches the commit)
    let commit_message = match generate_or_use_message(args, &project_root).await {
        Ok(msg) if msg.is_empty() => {
            if auto_staged {
                rollback_staging();
            }
            bail!("generated commit message is empty — use --message to provide one manually");
        }
        Ok(msg) => msg,
        Err(e) => {
            if auto_staged {
                rollback_staging();
            }
            return Err(e);
        }
    };

    // Dry run: show message and exit, restoring index if we auto-staged
    if args.dry_run {
        if auto_staged {
            rollback_staging();
        }
        if args.json {
            let output = serde_json::json!({
                "status": "dry_run",
                "message": commit_message,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("{commit_message}");
        }
        return Ok(());
    }

    // Create the commit — capture output so JSON mode stays clean
    let commit_output = Command::new("git")
        .args(["commit", "-m", &commit_message])
        .current_dir(&project_root)
        .output()?;

    if !commit_output.status.success() {
        if auto_staged {
            rollback_staging();
        }
        let stderr = String::from_utf8_lossy(&commit_output.stderr);
        bail!("git commit failed: {}", stderr.trim());
    }

    // Record commit event (non-fatal, only on actual commits)
    if let Ok(Some(db)) = metrics::open_metrics_db(&project_root) {
        if let Err(e) = metrics::store::record_loop_event(&db, "commit", None, None) {
            tracing::warn!("failed to record commit event: {e}");
        }
    }

    if args.json {
        let output = serde_json::json!({
            "status": "committed",
            "message": commit_message,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // In text mode, show git's own output then the message
        let stdout = String::from_utf8_lossy(&commit_output.stdout);
        if !stdout.trim().is_empty() {
            print!("{stdout}");
        }
        println!("\nCommitted.");
    }

    Ok(())
}

async fn generate_or_use_message(args: &CommitArgs, project_root: &Path) -> Result<String> {
    if let Some(ref msg) = args.message {
        return Ok(msg.clone());
    }

    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());

    let commit_prompt = prompt::build_commit_prompt(project_root)?;

    if !args.json {
        println!("Generating commit message...");
    }

    info!(provider = %config.provider.name, "starting provider for commit message");
    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, &config).await?;

    // commit always captures silently — the generated message is printed
    // by caller logic after cleanup, not streamed during generation.
    let sink: SharedSink = Arc::new(NullSink);
    let session_result =
        session::run_session_and_capture(&mut orchestrator, project_root, commit_prompt, sink)
            .await;
    if let Err(e) = orchestrator.shutdown().await {
        tracing::warn!("provider shutdown failed: {e}");
    }

    Ok(clean_commit_message(&session_result?))
}

/// Clean up AI-generated commit message: trim whitespace, strip markdown fences.
fn clean_commit_message(raw: &str) -> String {
    let mut msg = raw.trim().to_string();

    // Strip leading/trailing markdown code fences
    if msg.starts_with("```") {
        if let Some(end) = msg.find('\n') {
            msg = msg[end + 1..].to_string();
        }
    }
    if msg.ends_with("```") {
        msg = msg[..msg.len() - 3].to_string();
    }

    msg.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_commit_message_plain() {
        assert_eq!(
            clean_commit_message("feat(auth): add login"),
            "feat(auth): add login"
        );
    }

    #[test]
    fn clean_commit_message_with_fences() {
        assert_eq!(
            clean_commit_message("```\nfeat(auth): add login\n```"),
            "feat(auth): add login"
        );
    }

    #[test]
    fn clean_commit_message_with_language_fence() {
        assert_eq!(
            clean_commit_message("```text\nfeat(auth): add login\n```"),
            "feat(auth): add login"
        );
    }

    #[test]
    fn clean_commit_message_trims_whitespace() {
        assert_eq!(
            clean_commit_message("  feat(auth): add login  \n\n"),
            "feat(auth): add login"
        );
    }
}
