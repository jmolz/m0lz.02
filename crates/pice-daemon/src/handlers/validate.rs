//! `pice validate` handler — validates `.pice/workflow.yaml` + layers + models.
//!
//! Phase 2 scope: workflow schema, trigger expressions, cross-references,
//! floor violations. Model-capability checks use `check_models` to opt into
//! provider-list query (not yet wired into this handler — flagged as a warning
//! when `check_models` is true; real impl in Phase 2+ when the daemon context
//! exposes provider capabilities synchronously).

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, ValidateRequest};
use pice_core::layers::LayersConfig;
use pice_core::workflow::{loader, validate};
use serde_json::json;

use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;

pub async fn run(
    req: ValidateRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();

    // Load project workflow (None ⇒ framework defaults only)
    let project_cfg =
        loader::load_project(project_root).context("loading project workflow.yaml")?;
    let using_defaults = project_cfg.is_none();

    // Resolve merged effective workflow — catches floor violations.
    let resolved = match loader::resolve(project_root) {
        Ok(r) => r,
        Err(e) => {
            let message = format!("workflow.yaml load/merge failed: {e:#}");
            if req.json {
                let value = json!({
                    "ok": false,
                    "errors": [{"field": "workflow", "message": message}],
                    "warnings": [],
                });
                return Ok(CommandResponse::ExitJson { code: 1, value });
            }
            return Ok(CommandResponse::Exit { code: 1, message });
        }
    };

    if !req.json && using_defaults {
        sink.send_chunk("Note: no .pice/workflow.yaml found; validating framework defaults.\n");
    }

    // Load layers config if present (cross-reference validation requires it).
    let layers_path = project_root.join(".pice").join("layers.toml");
    let layers = if layers_path.exists() {
        Some(LayersConfig::load(&layers_path).context("loading .pice/layers.toml")?)
    } else {
        None
    };

    // Model-capability list: not queryable in this phase without blocking the
    // daemon on a provider start. `check_models` is accepted but treated as a
    // request for a warning note rather than a hard check.
    let known_models: Option<Vec<String>> = None;
    if req.check_models && !req.json {
        sink.send_chunk(
            "Note: --check-models will query the provider in Phase 2+; treating as advisory.\n",
        );
    }

    let seam_registry = pice_core::seam::default_registry();
    let report = validate::validate_all(
        &resolved,
        layers.as_ref(),
        known_models.as_deref(),
        Some(&seam_registry),
    );

    if req.json {
        let value = json!({
            "ok": report.is_ok(),
            "errors": report.errors,
            "warnings": report.warnings,
            "using_framework_defaults": using_defaults,
        });
        if report.is_ok() {
            Ok(CommandResponse::Json { value })
        } else {
            // JSON-mode failure: emit the full structured report on stdout
            // (via `ExitJson` in the renderer) AND signal nonzero exit so CI
            // pipelines like `pice validate --json && deploy` fail closed.
            Ok(CommandResponse::ExitJson { code: 1, value })
        }
    } else if !report.is_ok() {
        let mut message = format!("Validation failed with {} error(s):\n", report.errors.len());
        for e in &report.errors {
            let loc = match (e.line, e.column) {
                (Some(l), Some(c)) => format!(" (line {l}, col {c})"),
                _ => String::new(),
            };
            message.push_str(&format!("  - {}{loc}: {}\n", e.field, e.message));
        }
        if !report.warnings.is_empty() {
            message.push_str(&format!("\n{} warning(s):\n", report.warnings.len()));
            for w in &report.warnings {
                message.push_str(&format!("  - {}: {}\n", w.field, w.message));
            }
        }
        Ok(CommandResponse::Exit { code: 1, message })
    } else {
        let mut text = if report.warnings.is_empty() {
            "Workflow is valid.\n".to_string()
        } else {
            format!(
                "Workflow is valid ({} warning(s)):\n",
                report.warnings.len()
            )
        };
        for w in &report.warnings {
            text.push_str(&format!("  - {}: {}\n", w.field, w.message));
        }
        Ok(CommandResponse::Text { content: text })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NullSink;

    fn write_file(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[tokio::test]
    async fn valid_workflow_exits_ok_text() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".pice/layers.toml"),
            r#"
[layers]
order = ["backend"]
[layers.backend]
paths = ["src/**"]
"#,
        );
        let ctx = DaemonContext::new_for_test_with_root("t", dir.path().to_path_buf());
        let req = ValidateRequest {
            json: false,
            check_models: false,
        };
        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match resp {
            CommandResponse::Text { content } => {
                assert!(content.contains("valid"), "got: {content}");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bad_trigger_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".pice/layers.toml"),
            "[layers]\norder=[\"backend\"]\n[layers.backend]\npaths=[\"src/**\"]\n",
        );
        write_file(
            &dir.path().join(".pice/workflow.yaml"),
            r#"
schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.9
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
review:
  enabled: true
  trigger: "tier =="
  timeout_hours: 24
  on_timeout: reject
  notification: stdout
"#,
        );
        let ctx = DaemonContext::new_for_test_with_root("t", dir.path().to_path_buf());
        let req = ValidateRequest {
            json: false,
            check_models: false,
        };
        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(code, 1);
                assert!(
                    message.contains("review.trigger") && message.contains("line"),
                    "got: {message}"
                );
            }
            other => panic!("expected Exit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_layer_override_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".pice/layers.toml"),
            "[layers]\norder=[\"backend\"]\n[layers.backend]\npaths=[\"src/**\"]\n",
        );
        write_file(
            &dir.path().join(".pice/workflow.yaml"),
            r#"
schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.9
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
layer_overrides:
  ghost:
    tier: 3
"#,
        );
        let ctx = DaemonContext::new_for_test_with_root("t", dir.path().to_path_buf());
        let req = ValidateRequest {
            json: false,
            check_models: false,
        };
        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(code, 1);
                assert!(message.contains("ghost"), "got: {message}");
            }
            other => panic!("expected Exit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn json_mode_emits_structured_report() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".pice/layers.toml"),
            "[layers]\norder=[\"backend\"]\n[layers.backend]\npaths=[\"src/**\"]\n",
        );
        write_file(
            &dir.path().join(".pice/workflow.yaml"),
            r#"
schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.9
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
layer_overrides:
  ghost:
    tier: 3
"#,
        );
        let ctx = DaemonContext::new_for_test_with_root("t", dir.path().to_path_buf());
        let req = ValidateRequest {
            json: true,
            check_models: false,
        };
        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match resp {
            // JSON-mode validation failures return ExitJson{code:1, value}
            // so `pice validate --json` in CI scripts fails the process while
            // still emitting a parseable report on stdout via the renderer.
            CommandResponse::ExitJson { code, value } => {
                assert_eq!(code, 1);
                assert_eq!(value["ok"], false);
                let errs = value["errors"].as_array().unwrap();
                assert!(!errs.is_empty());
            }
            other => panic!("expected ExitJson, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn floor_violation_in_project_passes_because_no_floor_on_framework() {
        // Framework→project is simple overlay (no floor). Even a relaxed
        // project should load. Only schema/validation checks apply.
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".pice/layers.toml"),
            "[layers]\norder=[\"backend\"]\n[layers.backend]\npaths=[\"src/**\"]\n",
        );
        write_file(
            &dir.path().join(".pice/workflow.yaml"),
            r#"
schema_version: "0.2"
defaults:
  tier: 1
  min_confidence: 0.80
  max_passes: 2
  model: sonnet
  budget_usd: 100.0
  cost_cap_behavior: warn
"#,
        );
        let ctx = DaemonContext::new_for_test_with_root("t", dir.path().to_path_buf());
        let req = ValidateRequest {
            json: false,
            check_models: false,
        };
        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match resp {
            CommandResponse::Text { content } => {
                assert!(content.contains("valid"), "got: {content}");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
