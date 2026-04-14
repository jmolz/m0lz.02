//! `pice layers` handler — detect, list, check, and graph project layers.

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, LayersRequest, LayersSubcommand};
use pice_core::layers::detect::detect_layers;
use pice_core::layers::{tag_file_to_layers, LayersConfig};
use serde_json::json;

use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;
use crate::templates::extract_templates;

pub async fn run(
    req: LayersRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();

    match req.subcommand {
        LayersSubcommand::Detect { write, force } => {
            handle_detect(project_root, write, force, req.json, sink).await
        }
        LayersSubcommand::List => handle_list(project_root, req.json),
        LayersSubcommand::Check => handle_check(project_root, req.json),
        LayersSubcommand::Graph => handle_graph(project_root, req.json),
    }
}

async fn handle_detect(
    project_root: &std::path::Path,
    write: bool,
    force: bool,
    json_mode: bool,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    if !json_mode {
        sink.send_chunk("Detecting project layers...\n");
    }

    let detected = detect_layers(project_root).context("layer detection failed")?;
    let layers_config = detected.to_layers_config();

    // Validate the generated config before writing or presenting it.
    // Monorepo-only projects can produce an empty top-level order with
    // only stacks — later commands (list, graph, evaluate) require a
    // non-empty order, so refuse to write an unusable config.
    if layers_config.layers.order.is_empty() {
        let has_stacks = layers_config
            .stacks
            .as_ref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let hint = if has_stacks {
            " Monorepo stacks were detected but no top-level layers. \
             Create .pice/layers.toml manually with the layers relevant to your root project."
        } else {
            " Try adding framework dependencies or organizing code into recognizable directories."
        };
        return Ok(CommandResponse::Exit {
            code: 1,
            message: format!("No layers detected.{hint}"),
        });
    }

    if write {
        let pice_dir = project_root.join(".pice");
        let layers_path = pice_dir.join("layers.toml");

        if layers_path.exists() && !force {
            return Ok(CommandResponse::Exit {
                code: 1,
                message: ".pice/layers.toml already exists. Use --force to overwrite.".to_string(),
            });
        }

        std::fs::create_dir_all(&pice_dir).context("failed to create .pice/ directory")?;

        let toml_content = layers_config.to_toml_string()?;
        std::fs::write(&layers_path, &toml_content).context("failed to write .pice/layers.toml")?;

        // Extract contract templates
        let contracts_dir = pice_dir.join("contracts");
        let contract_result = extract_templates(&contracts_dir, "pice/contracts/", force)?;

        if json_mode {
            Ok(CommandResponse::Json {
                value: json!({
                    "layers": layers_config.layers.order,
                    "written": true,
                    "path": ".pice/layers.toml",
                    "contracts_created": contract_result.created,
                    "contracts_skipped": contract_result.skipped,
                }),
            })
        } else {
            let mut output = format!(
                "Wrote .pice/layers.toml with {} layers:\n",
                layers_config.layers.order.len()
            );
            for name in &layers_config.layers.order {
                if let Some(def) = layers_config.layers.defs.get(name) {
                    let always = if def.always_run { " (always_run)" } else { "" };
                    output.push_str(&format!("  - {name}{always}\n"));
                }
            }
            if !contract_result.created.is_empty() {
                output.push_str(&format!(
                    "\nCreated {} contract templates in .pice/contracts/\n",
                    contract_result.created.len()
                ));
            }
            Ok(CommandResponse::Text { content: output })
        }
    } else {
        // Dry-run: show proposed TOML
        let toml_content = layers_config.to_toml_string()?;

        if json_mode {
            Ok(CommandResponse::Json {
                value: json!({
                    "layers": layers_config.layers.order,
                    "written": false,
                    "toml": toml_content,
                }),
            })
        } else {
            let mut output = format!(
                "Detected {} layers (dry run — use --write to persist):\n\n",
                layers_config.layers.order.len()
            );
            output.push_str(&toml_content);
            Ok(CommandResponse::Text { content: output })
        }
    }
}

fn handle_list(project_root: &std::path::Path, json_mode: bool) -> Result<CommandResponse> {
    let layers_path = project_root.join(".pice/layers.toml");

    if !layers_path.exists() {
        return Ok(CommandResponse::Exit {
            code: 1,
            message: "No .pice/layers.toml found. Run `pice layers detect --write` first."
                .to_string(),
        });
    }

    let config = LayersConfig::load(&layers_path).context("failed to load .pice/layers.toml")?;

    if json_mode {
        let layers: Vec<serde_json::Value> = config
            .layers
            .order
            .iter()
            .filter_map(|name| {
                config.layers.defs.get(name).map(|def| {
                    json!({
                        "name": name,
                        "paths": def.paths.len(),
                        "always_run": def.always_run,
                        "depends_on": def.depends_on,
                    })
                })
            })
            .collect();
        Ok(CommandResponse::Json {
            value: json!({ "layers": layers }),
        })
    } else {
        let mut output = String::from("Layers:\n\n");
        output.push_str(&format!(
            "  {:<20} {:>5}  {:>10}  {}\n",
            "NAME", "PATHS", "ALWAYS_RUN", "DEPENDS_ON"
        ));
        output.push_str(&format!("  {}\n", "-".repeat(65)));
        for name in &config.layers.order {
            if let Some(def) = config.layers.defs.get(name) {
                let deps = if def.depends_on.is_empty() {
                    "-".to_string()
                } else {
                    def.depends_on.join(", ")
                };
                output.push_str(&format!(
                    "  {:<20} {:>5}  {:>10}  {}\n",
                    name,
                    def.paths.len(),
                    def.always_run,
                    deps,
                ));
            }
        }
        Ok(CommandResponse::Text { content: output })
    }
}

fn handle_check(project_root: &std::path::Path, json_mode: bool) -> Result<CommandResponse> {
    let layers_path = project_root.join(".pice/layers.toml");

    if !layers_path.exists() {
        return Ok(CommandResponse::Exit {
            code: 1,
            message: "No .pice/layers.toml found. Run `pice layers detect --write` first."
                .to_string(),
        });
    }

    let config = LayersConfig::load(&layers_path).context("failed to load .pice/layers.toml")?;

    // Walk project files using a simple recursive walk (no glob crate needed)
    let mut unmatched: Vec<String> = Vec::new();
    let mut total_files: usize = 0;
    let mut matched_files: usize = 0;

    walk_project_files(project_root, project_root, &mut |relative_path| {
        total_files += 1;
        let layers = tag_file_to_layers(&config, &relative_path);
        if layers.is_empty() {
            unmatched.push(relative_path);
        } else {
            matched_files += 1;
        }
    })?;

    if json_mode {
        Ok(CommandResponse::Json {
            value: json!({
                "total_files": total_files,
                "matched_files": matched_files,
                "unmatched_files": unmatched.len(),
                "unmatched": unmatched,
            }),
        })
    } else {
        let mut output = format!(
            "Layer coverage check:\n\n  Total files:     {total_files}\n  Matched:         {matched_files}\n  Unmatched:       {}\n",
            unmatched.len()
        );
        if !unmatched.is_empty() {
            output.push_str("\nFiles not covered by any layer:\n");
            for f in &unmatched {
                output.push_str(&format!("  {f}\n"));
            }
        } else {
            output.push_str("\nAll project files are covered by at least one layer.\n");
        }
        Ok(CommandResponse::Text { content: output })
    }
}

fn handle_graph(project_root: &std::path::Path, json_mode: bool) -> Result<CommandResponse> {
    let layers_path = project_root.join(".pice/layers.toml");

    if !layers_path.exists() {
        return Ok(CommandResponse::Exit {
            code: 1,
            message: "No .pice/layers.toml found. Run `pice layers detect --write` first."
                .to_string(),
        });
    }

    let config = LayersConfig::load(&layers_path).context("failed to load .pice/layers.toml")?;

    let dag = config.build_dag().context("failed to build layer DAG")?;

    if json_mode {
        Ok(CommandResponse::Json {
            value: json!({
                "cohorts": dag.cohorts,
                "edges": dag.edges.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
            }),
        })
    } else {
        let mut output = String::from("Layer DAG:\n\n");
        for (i, cohort) in dag.cohorts.iter().enumerate() {
            output.push_str(&format!("  Cohort {} (parallel):\n", i + 1));
            for layer in cohort {
                if let Some(def) = config.layers.defs.get(layer) {
                    let deps_str = if def.depends_on.is_empty() {
                        String::new()
                    } else {
                        format!(" → depends on: {}", def.depends_on.join(", "))
                    };
                    output.push_str(&format!("    - {layer}{deps_str}\n"));
                } else {
                    output.push_str(&format!("    - {layer}\n"));
                }
            }
        }
        if !dag.edges.is_empty() {
            output.push_str("\n  Edges:\n");
            for (dependent, dependency) in &dag.edges {
                output.push_str(&format!("    {dependency} → {dependent}\n"));
            }
        }
        Ok(CommandResponse::Text { content: output })
    }
}

/// Walk project files recursively, skipping hidden dirs, node_modules, target, etc.
fn walk_project_files(
    base: &std::path::Path,
    dir: &std::path::Path,
    callback: &mut dyn FnMut(String),
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Skip hidden directories and common noise
        if name_str.starts_with('.') {
            continue;
        }
        if matches!(
            name_str.as_ref(),
            "node_modules" | "target" | "dist" | "build" | "__pycache__" | ".git"
        ) {
            continue;
        }

        if path.is_dir() {
            walk_project_files(base, &path, callback)?;
        } else if path.is_file() {
            if let Ok(relative) = path.strip_prefix(base) {
                callback(relative.to_string_lossy().to_string());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NullSink;
    use crate::server::router::DaemonContext;

    #[tokio::test]
    async fn dispatch_layers_detect() {
        let dir = tempfile::tempdir().unwrap();
        // Set up a minimal Next.js project
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"next":"14.0.0","react":"18.0.0"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(
            dir.path().join("app/page.tsx"),
            "export default function() {}",
        )
        .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Detect {
                write: false,
                force: false,
            },
            json: false,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("Detected"),
                    "should mention detection, got: {content}"
                );
                // Should detect at least frontend from app/ + Next.js
                assert!(
                    content.contains("frontend") || content.contains("layers"),
                    "should mention layers, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_detect_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"next":"14.0.0","react":"18.0.0"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(dir.path().join("app/page.tsx"), "").unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Detect {
                write: true,
                force: false,
            },
            json: false,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("Wrote"),
                    "should confirm write, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }

        assert!(
            dir.path().join(".pice/layers.toml").exists(),
            "layers.toml should be created"
        );
    }

    #[tokio::test]
    async fn dispatch_layers_list_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::List,
            json: false,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(*code, 1);
                assert!(
                    message.contains("No .pice/layers.toml"),
                    "should mention missing config, got: {message}"
                );
            }
            other => panic!("expected Exit response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_list_with_config() {
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["backend", "frontend"]

[layers.backend]
paths = ["src/server/**"]

[layers.frontend]
paths = ["src/client/**"]
depends_on = ["backend"]
"#,
        )
        .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::List,
            json: false,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("backend"),
                    "should list backend, got: {content}"
                );
                assert!(
                    content.contains("frontend"),
                    "should list frontend, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_graph_with_config() {
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["backend", "database", "api"]

[layers.backend]
paths = ["src/server/**"]

[layers.database]
paths = ["prisma/**"]

[layers.api]
paths = ["src/api/**"]
depends_on = ["backend", "database"]
"#,
        )
        .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Graph,
            json: false,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("Cohort"),
                    "should show cohorts, got: {content}"
                );
                assert!(
                    content.contains("api"),
                    "should mention api layer, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }
    }

    // ─── JSON mode tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_layers_detect_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"next":"14.0.0","react":"18.0.0"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(dir.path().join("app/page.tsx"), "").unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Detect {
                write: false,
                force: false,
            },
            json: true,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Json { value } => {
                assert!(
                    value["layers"].is_array(),
                    "should have layers array, got: {value}"
                );
                assert!(
                    !value["layers"].as_array().unwrap().is_empty(),
                    "layers array should not be empty"
                );
                assert!(
                    !value["written"].as_bool().unwrap(),
                    "dry run should not write"
                );
                assert!(value["toml"].is_string(), "should include TOML string");
            }
            other => panic!("expected Json response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_list_json() {
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["backend", "frontend"]

[layers.backend]
paths = ["src/server/**"]

[layers.frontend]
paths = ["src/client/**"]
depends_on = ["backend"]
"#,
        )
        .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::List,
            json: true,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Json { value } => {
                let layers = value["layers"].as_array().unwrap();
                assert_eq!(layers.len(), 2, "should list 2 layers");
                assert_eq!(layers[0]["name"], "backend");
                assert_eq!(layers[1]["name"], "frontend");
                assert_eq!(layers[1]["depends_on"][0], "backend");
            }
            other => panic!("expected Json response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_check_text() {
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#,
        )
        .unwrap();

        // Create a file that matches and one that doesn't
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Hello").unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Check,
            json: false,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("coverage"),
                    "should mention coverage, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_check_json() {
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#,
        )
        .unwrap();

        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Hello").unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Check,
            json: true,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Json { value } => {
                assert!(
                    value["total_files"].as_u64().unwrap() > 0,
                    "should have files"
                );
                assert!(
                    value["matched_files"].as_u64().unwrap() > 0,
                    "should have matched files"
                );
                assert!(value["unmatched"].is_array(), "should have unmatched array");
            }
            other => panic!("expected Json response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_graph_json() {
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["backend", "database", "api"]

[layers.backend]
paths = ["src/server/**"]

[layers.database]
paths = ["prisma/**"]

[layers.api]
paths = ["src/api/**"]
depends_on = ["backend", "database"]
"#,
        )
        .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Graph,
            json: true,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Json { value } => {
                let cohorts = value["cohorts"].as_array().unwrap();
                assert!(cohorts.len() >= 2, "should have at least 2 cohorts");
                let edges = value["edges"].as_array().unwrap();
                assert_eq!(edges.len(), 2, "should have 2 dependency edges");
            }
            other => panic!("expected Json response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_layers_detect_write_refuses_existing() {
        let dir = tempfile::tempdir().unwrap();
        let pice_dir = dir.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            "[layers]\norder = [\"a\"]\n\n[layers.a]\npaths = [\"a/**\"]\n",
        )
        .unwrap();

        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"next":"14.0.0"}}"#,
        )
        .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("test-token", dir.path().to_path_buf());
        let req = LayersRequest {
            subcommand: LayersSubcommand::Detect {
                write: true,
                force: false,
            },
            json: false,
        };

        let resp = run(req, &ctx, &NullSink).await.unwrap();
        match &resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(*code, 1);
                assert!(
                    message.contains("already exists"),
                    "should refuse overwrite, got: {message}"
                );
            }
            other => panic!("expected Exit response, got: {other:?}"),
        }
    }
}
