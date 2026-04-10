//! Context-assembly prompt builders for PICE commands.
//!
//! These functions compose user-facing prompts by combining project context
//! (CLAUDE.md, git state, project tree) with command-specific instructions.
//! They depend on the pure helpers in `pice_core::prompt::helpers`.
//!
//! ## Architecture note
//!
//! In T6 of the Phase 0 refactor, the pure helpers (`read_claude_md`,
//! `get_git_diff`, `get_git_log`, `get_git_status_summary`, `get_staged_diff`,
//! `get_project_tree`) moved to `pice-core::prompt::helpers`. The builder
//! functions remain here temporarily; T13 moves them to `pice-daemon::prompt`
//! alongside the rest of the orchestration code.

use anyhow::{Context, Result};
use std::path::Path;

use pice_core::prompt::helpers::{
    get_git_diff, get_git_log, get_git_status_summary, get_project_tree, get_staged_diff,
    read_claude_md,
};

/// Build the planning prompt from a user description.
pub fn build_plan_prompt(description: &str, project_root: &Path) -> Result<String> {
    let claude_md = read_claude_md(project_root)?;
    Ok(format!(
        "You are creating a detailed implementation plan.\n\n\
         ## Project Context\n\n{claude_md}\n\n\
         ## Task\n\n\
         Create a comprehensive plan for: {description}\n\n\
         Write the plan to `.claude/plans/` following the plan template format.\n\
         Include a ## Contract section with JSON criteria for adversarial evaluation.\n\
         Set the tier based on complexity (1=simple, 2=feature, 3=architectural)."
    ))
}

/// Build the execution prompt from a parsed plan.
pub fn build_execute_prompt(plan_content: &str, project_root: &Path) -> Result<String> {
    let claude_md = read_claude_md(project_root)?;
    Ok(format!(
        "You are implementing from an approved plan.\n\n\
         ## Project Conventions\n\n{claude_md}\n\n\
         ## Plan\n\n{plan_content}\n\n\
         ## Instructions\n\n\
         Execute the plan tasks in order. After each task, run its validation command.\n\
         Fix errors immediately before proceeding to the next task."
    ))
}

/// Build the evaluation prompt from contract + diff + CLAUDE.md.
/// This is the ONLY context evaluators see — no implementation rationale.
/// Available for use when Rust-side prompt assembly is needed in Phase 3+.
#[allow(dead_code)]
pub fn build_evaluate_prompt(
    contract: &serde_json::Value,
    diff: &str,
    claude_md: &str,
) -> Result<String> {
    let contract_str =
        serde_json::to_string_pretty(contract).context("failed to serialize contract")?;
    Ok(format!(
        "You are an ADVERSARIAL EVALUATOR. Your job is to find failures, not confirm success.\n\n\
         ## Contract\n\n```json\n{contract_str}\n```\n\n\
         ## Code Changes\n\n```diff\n{diff}\n```\n\n\
         ## Project Conventions\n\n{claude_md}\n\n\
         ## Task\n\n\
         For EACH criterion: read the code, try to break it, score 1-10 with evidence.\n\
         Output structured JSON with scores for each criterion."
    ))
}

/// Build the adversarial design challenge prompt (for Codex/GPT).
/// Available for use when Rust-side prompt assembly is needed in Phase 3+.
#[allow(dead_code)]
pub fn build_adversarial_prompt(
    contract: &serde_json::Value,
    diff: &str,
    claude_md: &str,
) -> Result<String> {
    let contract_str =
        serde_json::to_string_pretty(contract).context("failed to serialize contract")?;
    Ok(format!(
        "You are a DESIGN CHALLENGER reviewing code changes.\n\n\
         ## Contract\n\n```json\n{contract_str}\n```\n\n\
         ## Code Changes\n\n```diff\n{diff}\n```\n\n\
         ## Project Conventions\n\n{claude_md}\n\n\
         ## Task\n\n\
         Challenge the APPROACH, not just correctness:\n\
         - Was this the right design? What assumptions does it depend on?\n\
         - Where could it fail under real-world conditions?\n\
         - What alternative approaches were overlooked?\n\
         Categorize findings as: critical, consider, or acknowledged."
    ))
}

/// Build the prime (codebase orientation) prompt.
pub fn build_prime_prompt(project_root: &Path) -> Result<String> {
    let claude_md = read_claude_md(project_root)?;
    let tree = get_project_tree(project_root)?;
    let git_log = get_git_log(project_root, 15)?;
    let git_status = get_git_status_summary(project_root)?;
    let existing_handoff = read_existing_handoff(project_root);

    let handoff_section = if existing_handoff.is_empty() {
        String::new()
    } else {
        format!(
            "## Prior Session Handoff (HANDOFF.md)\n\n\
             Reconcile these items against git history — resolve completed items, \
             carry forward open ones:\n\n{existing_handoff}\n\n"
        )
    };

    let handoff_bullet = if existing_handoff.is_empty() {
        ""
    } else {
        "\n         - Handoff status: which items from prior session are resolved vs. still open"
    };

    Ok(format!(
        "You are orienting on a codebase.\n\n\
         ## Project Conventions\n\n{claude_md}\n\n\
         ## Project Structure\n\n```\n{tree}\n```\n\n\
         ## Recent Git History\n\n```\n{git_log}\n```\n\n\
         ## Git Status\n\n```\n{git_status}\n```\n\n\
         {handoff_section}\
         ## Instructions\n\n\
         Orient on this codebase. Summarize:\n\
         - Project overview and purpose\n\
         - Tech stack and key libraries\n\
         - Architecture and code organization\n\
         - Current state and recent work{handoff_bullet}\n\
         - Recommended next actions"
    ))
}

/// Build the review (code review) prompt.
pub fn build_review_prompt(project_root: &Path) -> Result<String> {
    let claude_md = read_claude_md(project_root)?;
    let diff = get_git_diff(project_root)?;
    let git_status = get_git_status_summary(project_root)?;

    Ok(format!(
        "You are reviewing code changes.\n\n\
         ## Project Conventions\n\n{claude_md}\n\n\
         ## Code Changes\n\n```diff\n{diff}\n```\n\n\
         ## Git Status\n\n```\n{git_status}\n```\n\n\
         ## Instructions\n\n\
         Review these code changes. Check for:\n\
         - Logic errors and edge cases\n\
         - Security issues\n\
         - Convention violations\n\
         - Regressions and breaking changes\n\
         Report findings by severity: critical, warning, info."
    ))
}

/// Build the commit message generation prompt.
/// Uses only the staged diff so the message describes exactly what will be committed.
pub fn build_commit_prompt(project_root: &Path) -> Result<String> {
    let claude_md = read_claude_md(project_root)?;
    let diff = get_staged_diff(project_root)?;
    let git_log = get_git_log(project_root, 5)?;

    Ok(format!(
        "You are generating a commit message.\n\n\
         ## Project Conventions\n\n{claude_md}\n\n\
         ## Changes to Commit\n\n```diff\n{diff}\n```\n\n\
         ## Recent Commits (for style reference)\n\n```\n{git_log}\n```\n\n\
         ## Instructions\n\n\
         Generate a commit message following the project conventions.\n\
         Use the tag(scope): description format.\n\
         Output ONLY the commit message text, no markdown fences or extra commentary."
    ))
}

/// Build the session handoff prompt.
pub fn build_handoff_prompt(project_root: &Path) -> Result<String> {
    let claude_md = read_claude_md(project_root)?;
    let git_log = get_git_log(project_root, 10)?;
    let git_status = get_git_status_summary(project_root)?;

    // Include existing HANDOFF.md so the new handoff can preserve unresolved items
    let existing_handoff = read_existing_handoff(project_root);

    // List active plan files
    let plans_dir = project_root.join(".claude/plans");
    let plans_list = if plans_dir.is_dir() {
        let mut plans = Vec::new();
        for entry in
            std::fs::read_dir(&plans_dir).context("failed to read .claude/plans directory")?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    plans.push(format!("- .claude/plans/{name}"));
                }
            }
        }
        plans.sort();
        plans.join("\n")
    } else {
        "No plan files found.".to_string()
    };

    let prior_handoff_section = if existing_handoff.is_empty() {
        String::new()
    } else {
        format!(
            "## Prior Handoff (HANDOFF.md)\n\n\
             Review this prior handoff and carry forward any unresolved items:\n\n\
             {existing_handoff}\n\n"
        )
    };

    Ok(format!(
        "You are creating a session handoff summary.\n\n\
         ## Project Conventions\n\n{claude_md}\n\n\
         ## Recent Git History\n\n```\n{git_log}\n```\n\n\
         ## Git Status (uncommitted changes)\n\n```\n{git_status}\n```\n\n\
         ## Active Plans\n\n{plans_list}\n\n\
         {prior_handoff_section}\
         ## Instructions\n\n\
         Create a HANDOFF.md summarizing:\n\
         - What was accomplished this session\n\
         - Open items and incomplete work (include unresolved items from prior handoff)\n\
         - Recommended next steps\n\
         - Gotchas or context the next session needs\n\
         Output the full HANDOFF.md content."
    ))
}

/// Read existing HANDOFF.md, returning empty string if not found.
fn read_existing_handoff(project_root: &Path) -> String {
    let handoff_path = project_root.join("HANDOFF.md");
    std::fs::read_to_string(handoff_path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_plan_prompt_includes_description() {
        let dir = tempfile::tempdir().unwrap();
        // No CLAUDE.md — should still work
        let prompt = build_plan_prompt("add user auth", dir.path()).unwrap();
        assert!(prompt.contains("add user auth"));
        assert!(prompt.contains("## Contract"));
    }

    #[test]
    fn build_plan_prompt_includes_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Project Rules\nUse Rust.").unwrap();
        let prompt = build_plan_prompt("add tests", dir.path()).unwrap();
        assert!(prompt.contains("# Project Rules"));
        assert!(prompt.contains("Use Rust."));
    }

    #[test]
    fn build_execute_prompt_includes_plan() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = build_execute_prompt("# My Plan\n\nDo stuff.", dir.path()).unwrap();
        assert!(prompt.contains("# My Plan"));
        assert!(prompt.contains("Do stuff."));
    }

    #[test]
    fn build_evaluate_prompt_format() {
        let contract = serde_json::json!({"feature": "auth", "tier": 2});
        let prompt = build_evaluate_prompt(&contract, "+added line", "# Rules").unwrap();
        assert!(prompt.contains("ADVERSARIAL EVALUATOR"));
        assert!(prompt.contains("+added line"));
        assert!(prompt.contains("# Rules"));
    }

    #[test]
    fn build_adversarial_prompt_format() {
        let contract = serde_json::json!({"feature": "auth"});
        let prompt = build_adversarial_prompt(&contract, "+line", "# Rules").unwrap();
        assert!(prompt.contains("DESIGN CHALLENGER"));
        assert!(prompt.contains("critical, consider, or acknowledged"));
    }

    #[test]
    fn build_prime_prompt_includes_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();

        let prompt = build_prime_prompt(dir.path()).unwrap();
        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("Orient on this codebase"));
    }

    #[test]
    fn build_prime_prompt_includes_git_history() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "--allow-empty",
                "-m",
                "test commit for history",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let prompt = build_prime_prompt(dir.path()).unwrap();
        assert!(prompt.contains("test commit for history"));
    }

    #[test]
    fn build_prime_prompt_includes_handoff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("HANDOFF.md"),
            "# Handoff\n\n- Open: fix auth bug\n",
        )
        .unwrap();

        let prompt = build_prime_prompt(dir.path()).unwrap();
        assert!(prompt.contains("Prior Session Handoff"));
        assert!(prompt.contains("fix auth bug"));
    }

    #[test]
    fn build_review_prompt_includes_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("changed.rs"), "fn changed() {}").unwrap();

        let prompt = build_review_prompt(dir.path()).unwrap();
        assert!(prompt.contains("changed.rs"));
        assert!(prompt.contains("Review these code changes"));
    }

    #[test]
    fn build_commit_prompt_includes_staged() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("staged.rs"), "fn staged() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "staged.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let prompt = build_commit_prompt(dir.path()).unwrap();
        assert!(prompt.contains("staged.rs"));
        assert!(prompt.contains("Generate a commit message"));
    }

    #[test]
    fn build_commit_prompt_excludes_unstaged() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Untracked file — not staged, should NOT appear in commit prompt
        std::fs::write(dir.path().join("unstaged.rs"), "fn unstaged() {}").unwrap();

        let prompt = build_commit_prompt(dir.path()).unwrap();
        assert!(!prompt.contains("unstaged.rs"));
        assert!(prompt.contains("Generate a commit message"));
    }

    #[test]
    fn build_handoff_prompt_includes_log() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "--allow-empty",
                "-m",
                "handoff test commit",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let prompt = build_handoff_prompt(dir.path()).unwrap();
        assert!(prompt.contains("handoff test commit"));
        assert!(prompt.contains("HANDOFF.md"));
    }

    #[test]
    fn build_handoff_prompt_lists_plans() {
        let dir = tempfile::tempdir().unwrap();
        let plans_dir = dir.path().join(".claude/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        std::fs::write(plans_dir.join("my-plan.md"), "# Plan").unwrap();

        let prompt = build_handoff_prompt(dir.path()).unwrap();
        assert!(prompt.contains("my-plan.md"));
    }

    #[test]
    fn build_handoff_prompt_includes_prior_handoff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("HANDOFF.md"),
            "# Prior Session\n\n- Unresolved: fix the auth bug\n",
        )
        .unwrap();

        let prompt = build_handoff_prompt(dir.path()).unwrap();
        assert!(prompt.contains("Prior Handoff"));
        assert!(prompt.contains("fix the auth bug"));
        assert!(prompt.contains("carry forward any unresolved items"));
    }

    #[test]
    fn build_handoff_prompt_no_prior_handoff() {
        let dir = tempfile::tempdir().unwrap();
        // No HANDOFF.md exists
        let prompt = build_handoff_prompt(dir.path()).unwrap();
        assert!(!prompt.contains("Prior Handoff"));
    }
}
