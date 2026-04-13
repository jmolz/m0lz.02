use anyhow::Result;
use clap::Args;
use pice_core::cli::{CommandRequest, CommitRequest};

#[derive(Args, Debug, Clone)]
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

impl From<CommitArgs> for CommitRequest {
    fn from(args: CommitArgs) -> Self {
        CommitRequest {
            message: args.message,
            dry_run: args.dry_run,
            json: args.json,
        }
    }
}

pub async fn run(args: &CommitArgs) -> Result<()> {
    let req = CommandRequest::Commit(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}

// ─── v0.1 helpers (retained for tests; migrates to daemon handlers) ───────

/// Clean up AI-generated commit message: trim whitespace, strip markdown fences.
#[allow(dead_code)]
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
