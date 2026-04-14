//! `pice evaluate` handler — grade contract criteria with dual-model evaluation.

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, EvaluateRequest};
use pice_core::plan_parser::ParsedPlan;
use pice_core::prompt::helpers::{get_git_diff, read_claude_md};
use pice_protocol::CriterionScore;
use serde_json::json;

use crate::metrics;
use crate::orchestrator::{ProviderOrchestrator, StreamSink};
use crate::server::router::DaemonContext;

pub async fn run(
    req: EvaluateRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();

    // Validate plan file exists
    let plan_path = if req.plan_path.is_absolute() {
        req.plan_path.clone()
    } else {
        project_root.join(&req.plan_path)
    };

    if !plan_path.exists() {
        return Ok(CommandResponse::Exit {
            code: 1,
            message: format!("plan file not found: {}", plan_path.display()),
        });
    }

    // Parse plan and extract contract
    let plan = match ParsedPlan::load(&plan_path) {
        Ok(p) => p,
        Err(e) => {
            return Ok(CommandResponse::Exit {
                code: 1,
                message: format!("failed to parse plan: {e}"),
            });
        }
    };

    let contract = match &plan.contract {
        Some(c) => c.clone(),
        None => {
            return Ok(CommandResponse::Exit {
                code: 2,
                message: format!("no contract section found in {}", plan_path.display()),
            });
        }
    };

    // Check for Stack Loops (v0.2): if .pice/layers.toml exists, run per-layer evaluation
    let layers_path = project_root.join(".pice/layers.toml");
    if layers_path.exists() {
        let layers_config = pice_core::layers::LayersConfig::load(&layers_path)
            .context("failed to load layers config")?;

        let workflow = pice_core::workflow::loader::resolve(project_root)
            .context("failed to resolve workflow.yaml")?;

        let stack_cfg = crate::orchestrator::stack_loops::StackLoopsConfig {
            layers: &layers_config,
            plan_path: &plan_path,
            project_root,
            primary_provider: &config.evaluation.primary.provider,
            primary_model: &config.evaluation.primary.model,
            pice_config: config,
            workflow: &workflow,
        };
        let manifest =
            crate::orchestrator::stack_loops::run_stack_loops(&stack_cfg, sink, req.json).await?;

        // Format and return results from manifest
        if req.json {
            return Ok(CommandResponse::Json {
                value: serde_json::to_value(&manifest)?,
            });
        } else {
            let mut output = format!(
                "\nStack Loops Evaluation — {} layers\n",
                manifest.layers.len()
            );
            output.push_str(&"=".repeat(39));
            output.push('\n');
            for lr in &manifest.layers {
                let status_str = match lr.status {
                    pice_core::layers::manifest::LayerStatus::Passed => "PASS",
                    pice_core::layers::manifest::LayerStatus::Failed => "FAIL",
                    pice_core::layers::manifest::LayerStatus::Pending => "PENDING",
                    pice_core::layers::manifest::LayerStatus::InProgress => "IN-PROGRESS",
                    pice_core::layers::manifest::LayerStatus::Skipped => "SKIP",
                };
                let detail = lr
                    .halted_by
                    .as_ref()
                    .map(|r| format!(" — {r}"))
                    .unwrap_or_default();
                output.push_str(&format!("  [{status_str}] {}{detail}\n", lr.name));
            }
            let overall = match manifest.overall_status {
                pice_core::layers::manifest::ManifestStatus::Passed => "PASS",
                pice_core::layers::manifest::ManifestStatus::InProgress => "IN-PROGRESS",
                _ => "FAIL",
            };
            output.push_str(&format!("\nOverall: {overall}\n"));

            if matches!(
                manifest.overall_status,
                pice_core::layers::manifest::ManifestStatus::Passed
            ) {
                return Ok(CommandResponse::Text { content: output });
            } else {
                return Ok(CommandResponse::Exit {
                    code: 2,
                    message: output,
                });
            }
        }
    }

    // v0.1: Single-loop evaluation (existing code below)
    if !req.json {
        sink.send_chunk(
            "No .pice/layers.toml found — running single-loop evaluation (v0.1 behavior).\n",
        );
        sink.send_chunk("  Run `pice layers detect --write` to enable per-layer evaluation.\n\n");
    }

    let tier = contract.tier;
    let contract_json = serde_json::to_value(&contract).context("failed to serialize contract")?;

    // Get diff and CLAUDE.md for evaluator context — evaluators see ONLY
    // contract, diff, and CLAUDE.md. Never implementation context.
    let diff = get_git_diff(project_root)?;
    let claude_md = read_claude_md(project_root)?;

    if !req.json {
        sink.send_chunk(&format!(
            "Evaluating {} (Tier {tier})...\n",
            contract.feature
        ));
    }

    // Run evaluation based on tier
    let primary_result;
    let mut adversarial_result: Option<Result<pice_protocol::EvaluateResultParams>> = None;

    let primary_provider = &config.evaluation.primary.provider;
    let primary_model = &config.evaluation.primary.model;

    if tier >= 2 && config.evaluation.adversarial.enabled {
        // Tier 2+: dual-model evaluation in parallel via tokio::join!
        let adversarial_provider = &config.evaluation.adversarial.provider;
        let adversarial_model = &config.evaluation.adversarial.model;
        let adversarial_effort = &config.evaluation.adversarial.effort;

        // Clone data for the two parallel tasks
        let contract_json_clone = contract_json.clone();
        let diff_clone = diff.clone();
        let claude_md_clone = claude_md.clone();

        let primary_model_clone = primary_model.clone();
        let adversarial_model_clone = adversarial_model.clone();
        let adversarial_effort_clone = adversarial_effort.clone();

        // Start both providers in parallel
        let primary_start = ProviderOrchestrator::start(primary_provider, config);
        let adversarial_start = ProviderOrchestrator::start(adversarial_provider, config);

        let (primary_orch, adversarial_orch) = tokio::join!(primary_start, adversarial_start);

        // Primary evaluation
        let primary_eval = async {
            let mut orch = primary_orch?;
            let result = orch
                .evaluate(
                    contract_json.clone(),
                    diff.clone(),
                    claude_md.clone(),
                    Some(primary_model_clone),
                    None,
                )
                .await;
            orch.shutdown().await.ok();
            result
        };

        // Adversarial evaluation (may fail gracefully)
        let adversarial_eval = async {
            match adversarial_orch {
                Ok(mut orch) => {
                    let result = orch
                        .evaluate(
                            contract_json_clone,
                            diff_clone,
                            claude_md_clone,
                            Some(adversarial_model_clone),
                            Some(adversarial_effort_clone),
                        )
                        .await;
                    orch.shutdown().await.ok();
                    result
                }
                Err(e) => Err(e),
            }
        };

        let (p_result, a_result) = tokio::join!(primary_eval, adversarial_eval);
        primary_result = p_result;
        adversarial_result = Some(a_result);
    } else {
        // Tier 1: single evaluator
        let mut orch = ProviderOrchestrator::start(primary_provider, config).await?;
        primary_result = orch
            .evaluate(
                contract_json.clone(),
                diff.clone(),
                claude_md.clone(),
                Some(primary_model.clone()),
                None,
            )
            .await;
        orch.shutdown().await.ok();
    }

    // Process primary result — this must succeed for the evaluation to proceed.
    let primary = primary_result.context("primary evaluation failed")?;
    let overall_passed = primary.passed;
    let scores: Vec<CriterionScore> = primary.scores.clone();

    // Process adversarial result with graceful degradation —
    // provider failures are non-fatal per CLAUDE.md rules.
    let adversarial_summary = match adversarial_result {
        Some(Ok(adv)) => {
            if !req.json {
                sink.send_chunk("Adversarial review complete.\n");
            }
            Some(json!({
                "passed": adv.passed,
                "provider": config.evaluation.adversarial.provider,
                "model": config.evaluation.adversarial.model,
                "summary": adv.summary,
            }))
        }
        Some(Err(e)) => {
            tracing::warn!("adversarial evaluation failed (graceful degradation): {e}");
            if !req.json {
                sink.send_chunk(&format!("Warning: adversarial evaluation failed: {e}\n"));
            }
            Some(json!({
                "error": e.to_string(),
                "degraded": true,
            }))
        }
        None => None,
    };

    // Record to metrics DB (non-fatal — per CLAUDE.md and metrics.md rules).
    let normalized_path = metrics::normalize_plan_path(&plan.path, project_root);
    if let Ok(Some(db)) = metrics::open_metrics_db(project_root) {
        let adv_provider = if adversarial_summary.is_some() {
            Some(config.evaluation.adversarial.provider.as_str())
        } else {
            None
        };
        let adv_model = if adversarial_summary.is_some() {
            Some(config.evaluation.adversarial.model.as_str())
        } else {
            None
        };
        if let Err(e) = metrics::store::record_evaluation(
            &db,
            &normalized_path,
            &contract.feature,
            tier,
            overall_passed,
            &config.evaluation.primary.provider,
            &config.evaluation.primary.model,
            adv_provider,
            adv_model,
            primary.summary.as_deref(),
            &scores,
        ) {
            tracing::warn!("failed to record evaluation metrics: {e}");
        }
    }

    // Build response
    if req.json {
        let mut result = json!({
            "passed": overall_passed,
            "tier": tier,
            "feature": contract.feature,
            "criteria": scores.iter().map(|s| json!({
                "name": s.name,
                "score": s.score,
                "threshold": s.threshold,
                "passed": s.passed,
                "findings": s.findings,
            })).collect::<Vec<_>>(),
        });
        if let Some(adv) = adversarial_summary {
            result["adversarial"] = adv;
        }
        if overall_passed {
            Ok(CommandResponse::Json { value: result })
        } else {
            // Exit code 2 when evaluation fails (contract criteria not met)
            Ok(CommandResponse::Exit {
                code: 2,
                message: serde_json::to_string_pretty(&result)
                    .unwrap_or_else(|_| "evaluation failed".to_string()),
            })
        }
    } else {
        let mut output = String::new();
        output.push_str(&format!(
            "\n{} — Tier {} Evaluation\n",
            contract.feature, tier
        ));
        output.push_str(&"=".repeat(39));
        output.push_str("\n\n");

        for score in &scores {
            let status = if score.passed { "PASS" } else { "FAIL" };
            output.push_str(&format!(
                "  [{status}] {} — {}/10 (threshold: {})\n",
                score.name, score.score, score.threshold
            ));
            if let Some(findings) = &score.findings {
                output.push_str(&format!("         {findings}\n"));
            }
        }

        output.push_str(&format!(
            "\nResult: {}\n",
            if overall_passed { "PASS" } else { "FAIL" }
        ));

        if overall_passed {
            Ok(CommandResponse::Text { content: output })
        } else {
            Ok(CommandResponse::Exit {
                code: 2,
                message: output,
            })
        }
    }
}
