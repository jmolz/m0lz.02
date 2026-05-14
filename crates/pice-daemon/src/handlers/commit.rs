//! `pice commit` handler — AI-generated commit message.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, CommitRequest};
use serde_json::json;

use crate::orchestrator::session;
use crate::orchestrator::{NullSink, ProviderOrchestrator, StreamSink};
use crate::prompt::builders;
use crate::server::router::DaemonContext;

/// RAII guard that restores the git index when dropped.
///
/// Created after auto-staging (`git add -u`). If the commit succeeds,
/// call `disarm()` to prevent rollback. On any other exit path — `?`,
/// early return, or panic — the `Drop` impl runs `git reset` to
/// restore the index, satisfying the git-index-safety contract.
struct AutoStageGuard<'a> {
    project_root: &'a Path,
    active: bool,
}

impl<'a> AutoStageGuard<'a> {
    fn new(project_root: &'a Path) -> Self {
        Self {
            project_root,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for AutoStageGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            restore_index(self.project_root);
        }
    }
}

pub async fn run(
    req: CommitRequest,
    ctx: &DaemonContext,
    _sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();

    // Check if there's anything staged already
    let staged_diff = pice_core::prompt::helpers::get_staged_diff(project_root)?;
    let mut guard: Option<AutoStageGuard<'_>> = None;

    if staged_diff.trim().is_empty() {
        // Try auto-staging tracked modified files
        let add_result = std::process::Command::new("git")
            .args(["add", "-u"])
            .current_dir(project_root)
            .output()
            .context("failed to run git add -u")?;

        if add_result.status.success() {
            guard = Some(AutoStageGuard::new(project_root));
            // Check again after staging
            let new_diff = pice_core::prompt::helpers::get_staged_diff(project_root)?;
            if new_diff.trim().is_empty() {
                // Guard drops here → restores index
                return Ok(CommandResponse::Exit {
                    code: 1,
                    message: "nothing staged to commit".to_string(),
                });
            }
        } else {
            return Ok(CommandResponse::Exit {
                code: 1,
                message: "nothing staged to commit".to_string(),
            });
        }
    }

    // Generate or use provided message.
    // If any `?` propagates here, the guard restores the index automatically.
    let commit_message = if let Some(msg) = &req.message {
        msg.clone()
    } else {
        let prompt = builders::build_commit_prompt(project_root, &config.provider.name)?;
        let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, config).await?;
        let captured = session::run_session_and_capture(
            &mut orchestrator,
            project_root,
            prompt,
            Arc::new(NullSink),
        )
        .await;
        orchestrator.shutdown().await.ok();
        let raw = captured?;
        clean_commit_message(&raw)
    };

    if commit_message.trim().is_empty() {
        // Guard drops here → restores index
        return Ok(CommandResponse::Exit {
            code: 1,
            message: "generated commit message was empty".to_string(),
        });
    }

    // Dry run — show message without committing
    if req.dry_run {
        // Guard drops here → restores index
        if req.json {
            return Ok(CommandResponse::Json {
                value: json!({"status": "dry_run", "message": commit_message}),
            });
        }
        return Ok(CommandResponse::Text {
            content: format!("Dry run — generated commit message:\n\n{commit_message}\n"),
        });
    }

    // Execute the commit
    let commit_result = std::process::Command::new("git")
        .args(["commit", "-m", &commit_message])
        .current_dir(project_root)
        .output()
        .context("failed to run git commit")?;

    if !commit_result.status.success() {
        // Guard drops here → restores index
        let stderr = String::from_utf8_lossy(&commit_result.stderr);
        return Ok(CommandResponse::Exit {
            code: 1,
            message: format!("git commit failed: {stderr}"),
        });
    }

    // Commit succeeded — disarm the guard so we don't undo the stage.
    if let Some(g) = &mut guard {
        g.disarm();
    }

    if req.json {
        Ok(CommandResponse::Json {
            value: json!({"status": "complete", "message": commit_message}),
        })
    } else {
        Ok(CommandResponse::Text {
            content: format!(
                "Committed: {}\n",
                commit_message.lines().next().unwrap_or("")
            ),
        })
    }
}

/// Clean up AI-generated commit message: trim whitespace, strip markdown fences.
fn clean_commit_message(raw: &str) -> String {
    let mut msg = raw.trim().to_string();

    // Strip leading markdown code fences (```text or ```)
    if msg.starts_with("```") {
        if let Some(end) = msg.find('\n') {
            msg = msg[end + 1..].to_string();
        }
    }
    // Strip trailing code fence
    if msg.ends_with("```") {
        msg = msg[..msg.len() - 3].to_string();
    }

    msg.trim().to_string()
}

/// Restore the git index after auto-staging (git reset).
fn restore_index(project_root: &std::path::Path) {
    let _ = std::process::Command::new("git")
        .args(["reset"])
        .current_dir(project_root)
        .output();
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

    #[test]
    fn clean_commit_message_empty() {
        assert_eq!(clean_commit_message(""), "");
        assert_eq!(clean_commit_message("   "), "");
    }
}
