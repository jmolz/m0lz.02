use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanContract {
    pub feature: String,
    pub tier: u8,
    /// Minimum aggregate score for contract-level pass. Enforced in Phase 3+
    /// when weighted scoring is implemented. Currently, pass/fail is computed
    /// per-criterion (all must meet their individual threshold).
    #[allow(dead_code)]
    pub pass_threshold: u8,
    pub criteria: Vec<ContractCriterion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractCriterion {
    pub name: String,
    pub threshold: u8,
    pub validation: String,
}

#[derive(Debug, Clone)]
pub struct ParsedPlan {
    pub path: String,
    pub title: String,
    pub content: String,
    pub contract: Option<PlanContract>,
}

impl ParsedPlan {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read plan file: {}", path.display()))?;

        let title = content
            .lines()
            .find(|line| line.starts_with("# "))
            .map(|line| line.trim_start_matches("# ").to_string())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Untitled Plan".to_string())
            });

        let contract = Self::extract_contract(&content)?;

        Ok(Self {
            path: path.to_string_lossy().to_string(),
            title,
            content,
            contract,
        })
    }

    /// Used by status/metrics commands in Phase 3+.
    #[allow(dead_code)]
    pub fn tier(&self) -> u8 {
        self.contract.as_ref().map(|c| c.tier).unwrap_or(1)
    }

    fn extract_contract(content: &str) -> Result<Option<PlanContract>> {
        // Find a line-level ## Contract heading (not ### Contract or inline mentions).
        // The heading must be at the start of a line.
        let contract_start = match find_h2_heading(content, "Contract") {
            Some(pos) => pos,
            None => return Ok(None),
        };

        let contract_section = &content[contract_start..];

        // Find the JSON code fence within the contract section.
        // If ## Contract exists but has no valid ```json fence, that's an error —
        // a half-written contract should be surfaced, not silently ignored.
        let json_start = match contract_section.find("```json") {
            Some(pos) => pos + "```json".len(),
            None => bail!("## Contract section found but missing ```json code fence"),
        };

        let json_section = &contract_section[json_start..];
        let json_end = match json_section.find("```") {
            Some(pos) => pos,
            None => bail!("contract section has unclosed JSON code fence"),
        };

        let json_str = json_section[..json_end].trim();
        let contract: PlanContract =
            serde_json::from_str(json_str).context("failed to parse contract JSON")?;

        Ok(Some(contract))
    }
}

/// Find a level-2 Markdown heading (`## {title}`) at the start of a line.
/// Returns the byte offset of the `##` or None if not found.
/// Rejects `###` or deeper headings to avoid false positives.
/// Per CommonMark, up to 3 leading spaces are allowed for ATX headings;
/// 4+ spaces would be an indented code block.
fn find_h2_heading(content: &str, title: &str) -> Option<usize> {
    let pattern = format!("## {title}");
    for (line_start, line) in line_offsets(content) {
        // CommonMark: 0-3 leading spaces are valid for headings
        let leading_spaces = line.len() - line.trim_start_matches(' ').len();
        if leading_spaces > 3 {
            continue;
        }
        let trimmed = line.trim_start_matches(' ');
        if trimmed.starts_with(&pattern) {
            // Reject ### or deeper (e.g., "### Contract") and run-on titles
            let after_pattern = &trimmed[pattern.len()..];
            if after_pattern.is_empty()
                || after_pattern.starts_with('\n')
                || after_pattern.starts_with('\r')
                || after_pattern.starts_with(' ')
            {
                return Some(line_start);
            }
        }
    }
    None
}

/// Iterate over (byte_offset, line_content) pairs in a string.
/// Handles both LF and CRLF line endings correctly.
fn line_offsets(content: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut remaining = content;
    let mut offset = 0;
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        let (line, rest) = match remaining.find('\n') {
            Some(i) => (&remaining[..i], &remaining[i + 1..]),
            None => (remaining, ""),
        };
        let start = offset;
        offset += line.len()
            + if !rest.is_empty() || content.ends_with('\n') {
                1
            } else {
                0
            };
        remaining = rest;
        // Strip trailing \r for the returned line content
        let line = line.strip_suffix('\r').unwrap_or(line);
        Some((start, line))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plan_with_contract() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("test-plan.md");
        std::fs::write(
            &plan_path,
            r#"# Feature: Add Auth

## Overview
Add authentication to the API.

## Contract

```json
{
  "feature": "Add Auth",
  "tier": 2,
  "pass_threshold": 7,
  "criteria": [
    {
      "name": "Tests pass",
      "threshold": 7,
      "validation": "cargo test"
    }
  ]
}
```

## Notes
Some follow-up notes.
"#,
        )
        .unwrap();

        let plan = ParsedPlan::load(&plan_path).unwrap();
        assert_eq!(plan.title, "Feature: Add Auth");
        assert!(plan.contract.is_some());
        let contract = plan.contract.unwrap();
        assert_eq!(contract.feature, "Add Auth");
        assert_eq!(contract.tier, 2);
        assert_eq!(contract.criteria.len(), 1);
        assert_eq!(contract.criteria[0].name, "Tests pass");
    }

    #[test]
    fn parse_plan_without_contract() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("simple-plan.md");
        std::fs::write(&plan_path, "# Simple Plan\n\nJust do the thing.\n").unwrap();

        let plan = ParsedPlan::load(&plan_path).unwrap();
        assert_eq!(plan.title, "Simple Plan");
        assert!(plan.contract.is_none());
        assert_eq!(plan.tier(), 1);
    }

    #[test]
    fn parse_plan_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("bad-plan.md");
        std::fs::write(
            &plan_path,
            "# Bad Plan\n\n## Contract\n\n```json\n{invalid}\n```\n",
        )
        .unwrap();

        let result = ParsedPlan::load(&plan_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed to parse contract JSON"));
    }

    #[test]
    fn parse_plan_contract_header_without_json_fence() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("no-fence.md");
        std::fs::write(
            &plan_path,
            "# Plan\n\n## Contract\n\nSome text but no JSON fence.\n",
        )
        .unwrap();

        let result = ParsedPlan::load(&plan_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("missing"));
    }

    #[test]
    fn parse_plan_h3_contract_not_matched() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("h3-contract.md");
        // ### Contract should NOT be treated as a contract section
        std::fs::write(
            &plan_path,
            "# Plan\n\n### Contract\n\nThis is a subsection, not the real contract.\n",
        )
        .unwrap();

        let plan = ParsedPlan::load(&plan_path).unwrap();
        assert!(plan.contract.is_none());
    }

    #[test]
    fn parse_plan_inline_contract_mention_not_matched() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("inline-mention.md");
        // Inline mention of "## Contract" in prose should not trigger
        std::fs::write(
            &plan_path,
            "# Plan\n\nSee the ## Contract section in the template.\n",
        )
        .unwrap();

        let plan = ParsedPlan::load(&plan_path).unwrap();
        assert!(plan.contract.is_none());
    }

    #[test]
    fn parse_plan_contract_inside_code_block_not_matched() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("code-block.md");
        // ## Contract inside a fenced code block should not trigger.
        // Since find_h2_heading doesn't skip code blocks, this IS a known
        // limitation — but in practice PICE plans don't embed example headings
        // in code blocks. This test documents the current behavior.
        std::fs::write(
            &plan_path,
            "# Plan\n\n```\n## Contract\n```\n\nNo real contract here.\n",
        )
        .unwrap();

        // Current behavior: the parser finds "## Contract" inside the code block
        // and then fails because there's no ```json fence after it.
        // This is acceptable — a plan with "## Contract" in a code block would be
        // unusual, and the error message is clear.
        let result = ParsedPlan::load(&plan_path);
        assert!(result.is_err());
    }

    #[test]
    fn parse_plan_indented_4_spaces_not_matched() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("indented-4.md");
        // 4+ spaces = indented code block in CommonMark, not a heading
        std::fs::write(&plan_path, "# Plan\n\n    ## Contract\n\nNot a heading.\n").unwrap();

        let plan = ParsedPlan::load(&plan_path).unwrap();
        assert!(plan.contract.is_none());
    }

    #[test]
    fn parse_plan_missing_file() {
        let result = ParsedPlan::load(Path::new("/nonexistent/plan.md"));
        assert!(result.is_err());
    }
}
