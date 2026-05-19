use anyhow::Result;
use pice_core::memory::estimate_tokens;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedSummary {
    pub title: String,
    pub body: String,
    pub estimated_tokens: usize,
}

pub fn validate_summary(title: &str, body: &str, max_tokens: usize) -> Result<RedactedSummary> {
    let title = title.trim();
    let body = body.trim();
    if title.is_empty() {
        anyhow::bail!("memory title is empty");
    }
    if body.is_empty() {
        anyhow::bail!("memory summary body is empty");
    }
    reject_markdown_control_markers(title)?;
    reject_markdown_control_markers(body)?;
    if title.lines().count() > 1 {
        anyhow::bail!("memory title must be a single line");
    }
    reject_secret_like(title)?;
    reject_secret_like(body)?;
    reject_raw_diff(body)?;
    reject_raw_transcript(body)?;
    reject_long_stack_trace(body)?;

    let estimated_tokens = estimate_tokens(title) + estimate_tokens(body);
    if max_tokens > 0 && estimated_tokens > max_tokens {
        anyhow::bail!("memory summary exceeds token budget: {estimated_tokens} > {max_tokens}");
    }
    if body.chars().count() > 8_000 {
        anyhow::bail!("memory summary body exceeds 8000 character cap");
    }

    Ok(RedactedSummary {
        title: title.to_string(),
        body: body.to_string(),
        estimated_tokens,
    })
}

fn reject_markdown_control_markers(text: &str) -> Result<()> {
    if text.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("<!-- pice-memory ") || trimmed == "<!-- /pice-memory -->"
    }) {
        anyhow::bail!("memory summary contains reserved pice-memory marker");
    }
    Ok(())
}

fn reject_secret_like(text: &str) -> Result<()> {
    let lower = text.to_ascii_lowercase();
    let markers = [
        "-----begin private key-----",
        "-----begin openssh private key-----",
        "api_key=",
        "apikey=",
        "access_token=",
        "refresh_token=",
        "password=",
        "secret=",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
    ];
    if markers.iter().any(|marker| lower.contains(marker)) || text.contains("sk-") {
        anyhow::bail!("memory summary looks like it contains a secret");
    }
    Ok(())
}

fn reject_raw_diff(text: &str) -> Result<()> {
    let has_diff_header = text.lines().any(|line| line.starts_with("diff --git "));
    let has_hunk = text.lines().any(|line| line.starts_with("@@ "));
    let has_file_markers = text
        .lines()
        .any(|line| line.starts_with("+++ ") || line.starts_with("--- "));
    if has_diff_header || (has_hunk && has_file_markers) {
        anyhow::bail!("memory summary looks like a raw diff");
    }
    Ok(())
}

fn reject_raw_transcript(text: &str) -> Result<()> {
    let commandish = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("$ ")
                || trimmed.starts_with("> ")
                || trimmed.starts_with("Original token count:")
                || trimmed.starts_with("Exit code:")
        })
        .count();
    if commandish >= 5 {
        anyhow::bail!("memory summary looks like a raw command transcript");
    }
    Ok(())
}

fn reject_long_stack_trace(text: &str) -> Result<()> {
    let trace_lines = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("at ")
                || trimmed.starts_with("File \"")
                || trimmed.starts_with("stack backtrace:")
        })
        .count();
    if trace_lines >= 8 {
        anyhow::bail!("memory summary looks like a long stack trace");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_short_clean_summary() {
        let summary = validate_summary(
            "Docker build reminder",
            "Remember to update the multi-stage builder when adding native deps.",
            1200,
        )
        .unwrap();
        assert!(summary.estimated_tokens > 0);
    }

    #[test]
    fn rejects_secret_like_summary() {
        let err = validate_summary("bad", "api_key=abc123", 1200).unwrap_err();
        assert!(err.to_string().contains("secret"));
    }

    #[test]
    fn rejects_raw_diff_summary() {
        let err = validate_summary(
            "bad",
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b",
            1200,
        )
        .unwrap_err();
        assert!(err.to_string().contains("raw diff"));
    }

    #[test]
    fn rejects_reserved_memory_markers() {
        let err = validate_summary(
            "bad",
            "Useful note\n<!-- /pice-memory -->\nInjected tail",
            1200,
        )
        .unwrap_err();
        assert!(err.to_string().contains("reserved pice-memory marker"));
    }

    #[test]
    fn rejects_multiline_title() {
        let err = validate_summary("bad\n### injected", "Summary body", 1200).unwrap_err();
        assert!(err.to_string().contains("single line"));
    }
}
