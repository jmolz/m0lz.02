//! Pure prompt helpers — file reads and git subprocess calls that do not
//! depend on the provider orchestrator or any async runtime.
//!
//! Extracted from `pice-cli/src/engine/prompt.rs` in T6 of the Phase 0 refactor.
//! The context-assembly builders (`build_*_prompt`) stay in `pice-cli` for now
//! and will move to `pice-daemon::prompt` in T13.
//!
//! These helpers use `std::process::Command` (not tokio) because they are
//! called from synchronous command handlers. The daemon's async handlers also
//! call them directly — this is safe because the subprocess calls are short
//! and bounded (git diff, git log, find).

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Read CLAUDE.md from the project root, returning empty string if not found.
pub fn read_claude_md(project_root: &Path) -> Result<String> {
    let claude_md_path = project_root.join("CLAUDE.md");
    match std::fs::read_to_string(&claude_md_path) {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).context("failed to read CLAUDE.md"),
    }
}

/// Get the git diff for evaluation context.
/// Includes both tracked changes (staged + unstaged) and untracked new files.
/// Handles repos with no commits (unborn HEAD) by skipping tracked diff.
pub fn get_git_diff(project_root: &Path) -> Result<String> {
    let mut parts = Vec::new();

    // Tracked changes against HEAD (may fail on unborn HEAD — that's OK)
    let tracked = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff HEAD")?;
    if tracked.status.success() {
        let tracked_diff = String::from_utf8_lossy(&tracked.stdout).to_string();
        if !tracked_diff.trim().is_empty() {
            parts.push(tracked_diff);
        }
    } else {
        // HEAD doesn't exist (unborn repo) — collect both staged and unstaged changes.
        // git diff --cached: staged content (index vs empty tree)
        let staged = Command::new("git")
            .args(["diff", "--cached"])
            .current_dir(project_root)
            .output()
            .context("failed to run git diff --cached")?;
        if staged.status.success() {
            let staged_diff = String::from_utf8_lossy(&staged.stdout).to_string();
            if !staged_diff.trim().is_empty() {
                parts.push(staged_diff);
            }
        }
        // git diff (no args): unstaged changes to staged files (worktree vs index)
        let unstaged = Command::new("git")
            .args(["diff"])
            .current_dir(project_root)
            .output()
            .context("failed to run git diff (unstaged)")?;
        if unstaged.status.success() {
            let unstaged_diff = String::from_utf8_lossy(&unstaged.stdout).to_string();
            if !unstaged_diff.trim().is_empty() {
                parts.push(unstaged_diff);
            }
        }
    }

    // Untracked new files — show their full content as diffs
    let untracked = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(project_root)
        .output()
        .context("failed to list untracked files")?;
    if untracked.status.success() {
        let files = String::from_utf8_lossy(&untracked.stdout);
        for file in files.lines() {
            let file = file.trim();
            if file.is_empty() {
                continue;
            }
            let file_path = project_root.join(file);
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                parts.push(format!(
                    "diff --git a/{file} b/{file}\nnew file mode 100644\n--- /dev/null\n+++ b/{file}\n{}",
                    content.lines().map(|l| format!("+{l}")).collect::<Vec<_>>().join("\n")
                ));
            }
        }
    }

    // If nothing found, try diff against the previous commit (may not exist in fresh repos)
    if parts.is_empty() {
        let output = Command::new("git")
            .args(["diff", "HEAD~1", "HEAD"])
            .current_dir(project_root)
            .output()
            .context("failed to run git diff HEAD~1")?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }
        // HEAD~1 doesn't exist (single-commit repo) — return empty diff
    }

    Ok(parts.join("\n"))
}

/// Get the last N git commit messages as one-line summaries.
/// Returns empty string if not in a git repo or HEAD is unborn.
pub fn get_git_log(project_root: &Path, count: usize) -> Result<String> {
    let output = Command::new("git")
        .args(["log", "--oneline", &format!("-{count}")])
        .current_dir(project_root)
        .output()
        .context("failed to run git log")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Ok(String::new())
    }
}

/// Get a short git status summary (staged, modified, untracked files).
/// Returns empty string if not in a git repo.
pub fn get_git_status_summary(project_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(project_root)
        .output()
        .context("failed to run git status")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Ok(String::new())
    }
}

/// Get the staged diff (git diff --cached).
/// Returns empty string if nothing is staged.
pub fn get_staged_diff(project_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(project_root)
        .output()
        .context("failed to run git diff --cached")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Ok(String::new())
    }
}

/// Get a tree-like directory listing (depth 3), excluding common noise directories.
/// Uses POSIX `find` — macOS/Linux only. Not available on Windows.
pub fn get_project_tree(project_root: &Path) -> Result<String> {
    let output = Command::new("find")
        .args([
            ".",
            "-maxdepth",
            "3",
            "-not",
            "-path",
            "*/node_modules/*",
            "-not",
            "-path",
            "*/.git/*",
            "-not",
            "-path",
            "*/dist/*",
            "-not",
            "-path",
            "*/target/*",
            "-not",
            "-path",
            "*/.next/*",
            "-not",
            "-path",
            "*/__pycache__/*",
        ])
        .current_dir(project_root)
        .output()
        .context("failed to run find for project tree")?;
    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout).to_string();
        // Sort lines for deterministic output
        let mut lines: Vec<&str> = raw.lines().collect();
        lines.sort();
        Ok(lines.join("\n"))
    } else {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_claude_md_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_claude_md(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn get_git_diff_unborn_head() {
        let dir = tempfile::tempdir().unwrap();
        // Init a repo but don't commit — HEAD is unborn
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Create a file so there's something to find
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

        // Should NOT panic or error — returns the untracked file as diff
        let result = get_git_diff(dir.path()).unwrap();
        assert!(result.contains("test.txt"));
        assert!(result.contains("+hello"));
    }

    #[test]
    fn get_git_diff_includes_untracked_files() {
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
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Create an untracked file
        std::fs::write(dir.path().join("new-file.rs"), "fn main() {}").unwrap();

        let result = get_git_diff(dir.path()).unwrap();
        assert!(result.contains("new-file.rs"));
        assert!(result.contains("+fn main() {}"));
    }

    #[test]
    fn get_git_diff_staged_files_unborn_head() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Create and stage a file — no commit yet
        std::fs::write(dir.path().join("staged.rs"), "fn staged() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "staged.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Should capture staged file via git diff --cached
        let result = get_git_diff(dir.path()).unwrap();
        assert!(result.contains("staged.rs"));
    }

    #[test]
    fn get_git_log_no_repo() {
        let dir = tempfile::tempdir().unwrap();
        let result = get_git_log(dir.path(), 5).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn get_git_log_with_commits() {
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
                "first commit",
            ])
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
                "second commit",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let result = get_git_log(dir.path(), 5).unwrap();
        assert!(result.contains("first commit"));
        assert!(result.contains("second commit"));
    }

    #[test]
    fn get_git_status_summary_clean() {
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
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let result = get_git_status_summary(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn get_git_status_summary_with_changes() {
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
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("new.txt"), "hello").unwrap();

        let result = get_git_status_summary(dir.path()).unwrap();
        assert!(result.contains("new.txt"));
    }

    #[test]
    fn get_staged_diff_nothing_staged() {
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
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let result = get_staged_diff(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn get_staged_diff_with_staged_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("file.rs"), "fn first() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "file.rs"])
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
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Modify and stage
        std::fs::write(dir.path().join("file.rs"), "fn second() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "file.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let result = get_staged_diff(dir.path()).unwrap();
        assert!(result.contains("file.rs"));
    }

    #[test]
    fn get_project_tree_includes_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let result = get_project_tree(dir.path()).unwrap();
        // Check for both Unix and Windows path separators
        assert!(
            result.contains("src/main.rs") || result.contains("src\\main.rs"),
            "expected tree to contain src/main.rs or src\\main.rs, got:\n{result}"
        );
    }
}
