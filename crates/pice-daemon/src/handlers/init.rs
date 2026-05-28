//! `pice init` handler — scaffold developer commands and `.pice/` config.

use anyhow::Result;
use pice_core::cli::{CommandResponse, InitRequest};
use pice_core::config::PiceConfig;
use serde_json::json;
use tracing::info;

use crate::metrics;
use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;
use crate::templates::extract_templates;

fn developer_template(developer: &str) -> Option<(&'static str, &'static str)> {
    match developer {
        "claude-code" => Some((".claude", "claude/")),
        "codex" => Some((".codex", "codex/")),
        _ => None,
    }
}

fn scaffold_codex_agents_md(
    project_root: &std::path::Path,
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let target = project_root.join("AGENTS.md");
    if target.exists() && !force {
        return Ok((Vec::new(), vec!["AGENTS.md".to_string()]));
    }

    let template = project_root.join(".codex/templates/AGENTS-template.md");
    let content = std::fs::read_to_string(&template)?;
    std::fs::write(target, content)?;
    Ok((vec!["AGENTS.md".to_string()], Vec::new()))
}

fn configure_codex_primary_provider(config_path: &std::path::Path) -> Result<()> {
    let content = std::fs::read_to_string(config_path)?;
    let updated = set_provider_name(&content, "codex");
    std::fs::write(config_path, updated)?;
    Ok(())
}

fn set_provider_name(content: &str, provider: &str) -> String {
    let mut output = Vec::new();
    let mut in_provider = false;
    let mut saw_provider_section = false;
    let mut replaced = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_provider && !replaced {
                output.push(format!("name = \"{provider}\""));
                replaced = true;
            }
            in_provider = trimmed == "[provider]";
            saw_provider_section |= in_provider;
            output.push(line.to_string());
            continue;
        }

        if in_provider && trimmed.starts_with("name") {
            let indent_len = line.len() - trimmed.len();
            let indent = &line[..indent_len];
            output.push(format!("{indent}name = \"{provider}\""));
            replaced = true;
        } else {
            output.push(line.to_string());
        }
    }

    if in_provider && !replaced {
        output.push(format!("name = \"{provider}\""));
        replaced = true;
    }

    if !saw_provider_section {
        let mut with_provider = vec!["[provider]".to_string(), format!("name = \"{provider}\"")];
        if !content.is_empty() {
            with_provider.push(String::new());
        }
        with_provider.extend(output);
        output = with_provider;
    } else if !replaced {
        output.insert(0, format!("name = \"{provider}\""));
    }

    let mut result = output.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Initialize a project with PICE scaffolding.
///
/// 1. Extracts templates to `.claude/` or `.codex/` and `.pice/`
/// 2. Validates the scaffolded config
/// 3. Initializes (or migrates) the metrics database
/// 4. Returns created/skipped file counts
pub async fn run(
    req: InitRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let Some((developer_dir_name, developer_prefix)) = developer_template(&req.developer) else {
        return Ok(CommandResponse::Exit {
            code: 1,
            message: format!(
                "unsupported developer '{}'; expected claude-code or codex",
                req.developer
            ),
        });
    };

    let developer_dir = project_root.join(developer_dir_name);
    let pice_dir = project_root.join(".pice");

    // Handle --upgrade early: generate layers.toml + contract templates for v0.1 projects.
    // Upgrade is a standalone operation that requires an existing PICE project.
    if req.upgrade {
        let config_path = pice_dir.join("config.toml");
        if !config_path.exists() {
            return Ok(CommandResponse::Exit {
                code: 1,
                message: "Not a PICE project. Run `pice init` first.".to_string(),
            });
        }

        let layers_path = pice_dir.join("layers.toml");
        let mut upgrade_created: Vec<String> = Vec::new();
        let mut upgrade_skipped: Vec<String> = Vec::new();

        if !layers_path.exists() || req.force {
            let detected = pice_core::layers::detect::detect_layers(project_root)
                .map_err(|e| anyhow::anyhow!("layer detection failed: {e}"))?;
            let layers_config = detected.to_layers_config();
            let toml_content = layers_config
                .to_toml_string()
                .map_err(|e| anyhow::anyhow!("failed to serialize layers config: {e}"))?;
            std::fs::write(&layers_path, &toml_content)
                .map_err(|e| anyhow::anyhow!("failed to write layers.toml: {e}"))?;
            upgrade_created.push("layers.toml".to_string());

            if !req.json {
                sink.send_chunk(&format!(
                    "Generated .pice/layers.toml with {} layers\n",
                    layers_config.layers.order.len()
                ));
            }
        } else {
            upgrade_skipped.push("layers.toml".to_string());
        }

        // Extract contract templates
        let contracts_dir = pice_dir.join("contracts");
        let contract_result = extract_templates(&contracts_dir, "pice/contracts/", req.force)?;
        for f in &contract_result.created {
            upgrade_created.push(format!("contracts/{f}"));
        }
        for f in &contract_result.skipped {
            upgrade_skipped.push(format!("contracts/{f}"));
        }

        if req.json {
            return Ok(CommandResponse::Json {
                value: serde_json::json!({
                    "upgraded": true,
                    "created": upgrade_created,
                    "skipped": upgrade_skipped,
                }),
            });
        } else {
            let mut output = String::from("Upgrade to v0.2 complete.\n");
            if !upgrade_created.is_empty() {
                output.push_str(&format!("  Created {} files:\n", upgrade_created.len()));
                for f in &upgrade_created {
                    output.push_str(&format!("    .pice/{f}\n"));
                }
            }
            if !upgrade_skipped.is_empty() {
                output.push_str(&format!(
                    "  Skipped {} existing files (use --force to overwrite)\n",
                    upgrade_skipped.len()
                ));
            }
            return Ok(CommandResponse::Text { content: output });
        }
    }

    if !req.json {
        sink.send_chunk(&format!("Scaffolding {developer_dir_name}/ directory...\n"));
    }
    let developer_result = extract_templates(&developer_dir, developer_prefix, req.force)?;

    let (extra_created, extra_skipped) = if req.developer == "codex" {
        scaffold_codex_agents_md(project_root, req.force)?
    } else {
        (Vec::new(), Vec::new())
    };

    if !req.json {
        sink.send_chunk("Scaffolding .pice/ directory...\n");
    }
    let pice_result = extract_templates(&pice_dir, "pice/", req.force)?;
    if req.developer == "codex" {
        configure_codex_primary_provider(&pice_dir.join("config.toml"))?;
    }

    // Verify the scaffolded config is valid
    let config_path = pice_dir.join("config.toml");
    if config_path.exists() {
        match PiceConfig::load(&config_path) {
            Ok(config) => {
                info!(
                    provider = %config.provider.name,
                    eval_model = %config.evaluation.primary.model,
                    "loaded config"
                );
            }
            Err(e) => {
                tracing::warn!("config.toml exists but failed to parse: {e}");
            }
        }
    }

    // Initialize or migrate the metrics database
    let metrics_db_path = metrics::resolve_metrics_db_path(project_root);
    if !metrics_db_path.exists() {
        if let Some(parent) = metrics_db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        metrics::db::MetricsDb::open(&metrics_db_path)?;
        info!(path = %metrics_db_path.display(), "initialized metrics database");
    } else if req.force {
        // Run migrations on existing DB without destroying data
        metrics::db::MetricsDb::open(&metrics_db_path)?;
        info!(path = %metrics_db_path.display(), "migrated existing metrics database");
    }

    let total_created =
        developer_result.created.len() + pice_result.created.len() + extra_created.len();
    let total_skipped =
        developer_result.skipped.len() + pice_result.skipped.len() + extra_skipped.len();

    if req.json {
        let created: Vec<String> = developer_result
            .created
            .iter()
            .map(|f| format!("{developer_dir_name}/{f}"))
            .chain(pice_result.created.iter().map(|f| format!(".pice/{f}")))
            .chain(extra_created.iter().cloned())
            .collect();
        let skipped: Vec<String> = developer_result
            .skipped
            .iter()
            .map(|f| format!("{developer_dir_name}/{f}"))
            .chain(pice_result.skipped.iter().map(|f| format!(".pice/{f}")))
            .chain(extra_skipped.iter().cloned())
            .collect();
        Ok(CommandResponse::Json {
            value: json!({
                "created": created,
                "skipped": skipped,
                "totalCreated": total_created,
                "totalSkipped": total_skipped,
            }),
        })
    } else {
        let mut output = String::new();
        if total_created > 0 {
            output.push_str(&format!("\nCreated {} files:\n", total_created));
            for f in &developer_result.created {
                output.push_str(&format!("  {developer_dir_name}/{f}\n"));
            }
            for f in &pice_result.created {
                output.push_str(&format!("  .pice/{f}\n"));
            }
            for f in &extra_created {
                output.push_str(&format!("  {f}\n"));
            }
        }
        if total_skipped > 0 {
            output.push_str(&format!(
                "Skipped {} existing files (use --force to overwrite)\n",
                total_skipped
            ));
        }
        output.push_str("\nPICE initialized. Run `pice prime` to orient on your codebase.\n");
        Ok(CommandResponse::Text { content: output })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NullSink;
    use crate::server::router::DaemonContext;

    #[tokio::test]
    async fn init_creates_claude_and_pice_directories() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = InitRequest {
            force: false,
            upgrade: false,
            json: false,
            developer: "claude-code".to_string(),
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("PICE initialized"),
                    "should mention initialization, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }

        assert!(dir.path().join(".claude/commands/plan-feature.md").exists());
        assert!(dir
            .path()
            .join(".claude/templates/plan-template.md")
            .exists());
        assert!(dir.path().join(".claude/docs/PLAYBOOK.md").exists());
        assert!(dir.path().join(".pice/config.toml").exists());
        assert!(dir.path().join(".pice/metrics.db").exists());
    }

    #[tokio::test]
    async fn init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = InitRequest {
            force: false,
            upgrade: false,
            json: false,
            developer: "claude-code".to_string(),
        };

        run(req.clone(), &ctx, &NullSink).await.unwrap();

        // Modify a file
        let plan_path = dir.path().join(".claude/commands/plan-feature.md");
        std::fs::write(&plan_path, "custom content").unwrap();

        // Run again — should not overwrite
        run(req, &ctx, &NullSink).await.unwrap();

        let content = std::fs::read_to_string(&plan_path).unwrap();
        assert_eq!(content, "custom content");
    }

    #[tokio::test]
    async fn init_force_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());

        // First init (no force)
        let req = InitRequest {
            force: false,
            upgrade: false,
            json: false,
            developer: "claude-code".to_string(),
        };
        run(req, &ctx, &NullSink).await.unwrap();

        // Modify a file
        let plan_path = dir.path().join(".claude/commands/plan-feature.md");
        std::fs::write(&plan_path, "custom content").unwrap();

        // Force init should overwrite
        let req = InitRequest {
            force: true,
            upgrade: false,
            json: false,
            developer: "claude-code".to_string(),
        };
        run(req, &ctx, &NullSink).await.unwrap();

        let content = std::fs::read_to_string(&plan_path).unwrap();
        assert_ne!(content, "custom content");
    }

    #[tokio::test]
    async fn init_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = InitRequest {
            force: false,
            upgrade: false,
            json: true,
            developer: "claude-code".to_string(),
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Json { value } => {
                assert!(
                    value["totalCreated"].as_u64().unwrap() > 0,
                    "should have created files"
                );
                assert!(
                    !value["created"].as_array().unwrap().is_empty(),
                    "created array should not be empty"
                );
            }
            other => panic!("expected Json response in json mode, got: {other:?}"),
        }

        assert!(dir.path().join(".claude/commands/plan-feature.md").exists());
        assert!(dir.path().join(".pice/config.toml").exists());
    }

    #[tokio::test]
    async fn init_codex_creates_codex_scaffold_agents_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = InitRequest {
            force: false,
            upgrade: false,
            json: true,
            developer: "codex".to_string(),
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Json { value } => {
                let created = value["created"].as_array().unwrap();
                assert!(created.iter().any(|v| v == "AGENTS.md"));
                assert!(created.iter().any(|v| v == ".codex/commands/self-heal.md"));
            }
            other => panic!("expected Json response in json mode, got: {other:?}"),
        }

        assert!(dir.path().join(".codex/commands/plan-feature.md").exists());
        assert!(dir.path().join(".codex/commands/self-heal.md").exists());
        assert!(dir.path().join("AGENTS.md").exists());
        let config = std::fs::read_to_string(dir.path().join(".pice/config.toml")).unwrap();
        assert!(config.contains("name = \"codex\""));
    }

    #[tokio::test]
    async fn init_codex_updates_existing_config_provider_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());

        run(
            InitRequest {
                force: false,
                upgrade: false,
                json: true,
                developer: "claude-code".to_string(),
            },
            &ctx,
            &NullSink,
        )
        .await
        .unwrap();

        run(
            InitRequest {
                force: false,
                upgrade: false,
                json: true,
                developer: "codex".to_string(),
            },
            &ctx,
            &NullSink,
        )
        .await
        .unwrap();

        let config = std::fs::read_to_string(dir.path().join(".pice/config.toml")).unwrap();
        assert!(config.contains("[provider]"));
        assert!(config.contains("name = \"codex\""));
        let parsed = PiceConfig::load(&dir.path().join(".pice/config.toml")).unwrap();
        assert_eq!(parsed.provider.name, "codex");
    }

    #[test]
    fn set_provider_name_preserves_existing_config_shape() {
        let updated = set_provider_name(
            r#"[provider]
# Keep custom comments.
name = "claude-code"

[evaluation.primary]
provider = "claude-code"
"#,
            "codex",
        );

        assert!(updated.contains("# Keep custom comments."));
        assert!(updated.contains("[provider]\n# Keep custom comments.\nname = \"codex\""));
        assert!(updated.contains("[evaluation.primary]\nprovider = \"claude-code\""));
    }

    #[tokio::test]
    async fn init_second_run_reports_skipped_in_json() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());

        // First run
        let req = InitRequest {
            force: false,
            upgrade: false,
            json: true,
            developer: "claude-code".to_string(),
        };
        run(req.clone(), &ctx, &NullSink).await.unwrap();

        // Second run — everything skipped
        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Json { value } => {
                assert_eq!(
                    value["totalCreated"].as_u64().unwrap(),
                    0,
                    "second run should create nothing"
                );
                assert!(
                    value["totalSkipped"].as_u64().unwrap() > 0,
                    "second run should skip existing files"
                );
            }
            other => panic!("expected Json response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn init_upgrade_generates_layers_toml() {
        let dir = tempfile::tempdir().unwrap();

        // Set up a v0.1 project: .pice/config.toml exists, package.json with Next.js
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("config.toml"),
            r#"
[provider]
name = "claude-code"
[evaluation]
[evaluation.primary]
provider = "claude-code"
model = "claude-sonnet-4-20250514"
[evaluation.adversarial]
provider = "codex"
model = "o3-mini"
effort = "xhigh"
enabled = false
[evaluation.tiers]
tier1_models = ["claude-sonnet-4-20250514"]
tier2_models = ["claude-sonnet-4-20250514"]
tier3_models = ["claude-sonnet-4-20250514"]
tier3_agent_team = false
[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"
[metrics]
db_path = ".pice/metrics.db"
"#,
        )
        .unwrap();

        // Package.json with Next.js deps for detection
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"next":"14.0.0","react":"18.0.0"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(dir.path().join("app/page.tsx"), "").unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = InitRequest {
            force: false,
            upgrade: true,
            json: false,
            developer: "claude-code".to_string(),
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("Upgrade") || content.contains("upgrade"),
                    "should mention upgrade, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }

        assert!(
            pice_dir.join("layers.toml").exists(),
            ".pice/layers.toml should be created by upgrade"
        );
    }

    #[tokio::test]
    async fn init_upgrade_no_pice_dir() {
        let dir = tempfile::tempdir().unwrap();
        // No .pice/config.toml — upgrade should fail

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = InitRequest {
            force: false,
            upgrade: true,
            json: false,
            developer: "claude-code".to_string(),
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(*code, 1);
                assert!(
                    message.contains("Not a PICE project"),
                    "should mention not a PICE project, got: {message}"
                );
            }
            other => panic!("expected Exit response, got: {other:?}"),
        }
    }
}
