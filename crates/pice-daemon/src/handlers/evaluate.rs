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
        // Phase 3 third-round adversarial review fix: pre-orchestrator
        // failures under `--json` must use ExitJson so machine callers can
        // parse the structured failure payload on stdout instead of a plain
        // text message on stderr.
        if req.json {
            return Ok(CommandResponse::ExitJson {
                code: 1,
                value: json!({
                    "status": "plan-not-found",
                    "plan_path": plan_path.display().to_string(),
                }),
            });
        }
        return Ok(CommandResponse::Exit {
            code: 1,
            message: format!("plan file not found: {}", plan_path.display()),
        });
    }

    // Parse plan and extract contract
    let plan = match ParsedPlan::load(&plan_path) {
        Ok(p) => p,
        Err(e) => {
            if req.json {
                return Ok(CommandResponse::ExitJson {
                    code: 1,
                    value: json!({
                        "status": "plan-parse-failed",
                        "plan_path": plan_path.display().to_string(),
                        "error": e.to_string(),
                    }),
                });
            }
            return Ok(CommandResponse::Exit {
                code: 1,
                message: format!("failed to parse plan: {e}"),
            });
        }
    };

    let contract = match &plan.contract {
        Some(c) => c.clone(),
        None => {
            if req.json {
                return Ok(CommandResponse::ExitJson {
                    code: 2,
                    value: json!({
                        "status": "no-contract-section",
                        "plan_path": plan_path.display().to_string(),
                    }),
                });
            }
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

        // Fail closed on semantic workflow errors (bad triggers, unknown
        // layer overrides, out-of-range tiers, unknown seam boundaries,
        // and — as of the Phase 3 evaluator findings — seam checks whose
        // `applies_to()` returns false for their declared boundary).
        // Without this check, a broken workflow.yaml would silently drive
        // orchestration. `pice validate` runs the same checks; this
        // mirrors them at execution time.
        let seam_registry = pice_core::seam::default_registry();
        let report = pice_core::workflow::validate::validate_all(
            &workflow,
            Some(&layers_config),
            None,
            Some(&seam_registry),
        );
        if !report.is_ok() {
            if req.json {
                let errors: Vec<serde_json::Value> = report
                    .errors
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "field": e.field,
                            "message": e.message,
                        })
                    })
                    .collect();
                let value = serde_json::json!({
                    "status": "workflow-validation-failed",
                    "errors": errors,
                    "hint": "Run `pice validate` for full details.",
                });
                return Ok(CommandResponse::ExitJson { code: 1, value });
            }
            let mut message = String::from("workflow.yaml has validation errors:\n");
            for e in &report.errors {
                message.push_str(&format!("  - {}: {}\n", e.field, e.message));
            }
            message.push_str("\nRun `pice validate` for full details.\n");
            return Ok(CommandResponse::Exit { code: 1, message });
        }

        // Merge `layers.toml [seams]` with `workflow.yaml.seams` under the
        // project-floor contract: the user overlay may REPLACE a project
        // boundary's check list but cannot REMOVE a boundary or empty-list
        // it. Floor violations are a HARD fail: running with a silently-
        // disabled required boundary was a critical silent-bypass route.
        let mut merged_seams_opt = layers_config.seams.clone();
        let mut seam_violations: Vec<pice_core::workflow::merge::FloorViolation> = Vec::new();
        pice_core::workflow::merge::merge_seams(
            &mut merged_seams_opt,
            workflow.seams.as_ref(),
            &mut seam_violations,
        );
        if !seam_violations.is_empty() {
            if req.json {
                let violations: Vec<serde_json::Value> = seam_violations
                    .iter()
                    .map(|v| {
                        serde_json::json!({
                            "field": v.field,
                            "reason": v.reason,
                            "project": v.project,
                            "user": v.user,
                        })
                    })
                    .collect();
                let value = serde_json::json!({
                    "status": "seam-floor-violation",
                    "violations": violations,
                    "hint": "workflow.yaml [seams] may REPLACE a layers.toml boundary's \
                            check list but cannot empty-list it. Omit the key to inherit \
                            the project list.",
                });
                return Ok(CommandResponse::ExitJson { code: 1, value });
            }
            let mut message = String::from("seam configuration floor violations:\n");
            for v in &seam_violations {
                message.push_str(&format!(
                    "  - {}: {} (project: {}, user: {})\n",
                    v.field, v.reason, v.project, v.user
                ));
            }
            message.push_str(
                "\nworkflow.yaml [seams] may REPLACE a layers.toml boundary's check list \
                 but cannot empty-list it. Omit the key to inherit the project list.\n",
            );
            return Ok(CommandResponse::Exit { code: 1, message });
        }
        let merged_seams: std::collections::BTreeMap<String, Vec<String>> =
            merged_seams_opt.unwrap_or_default();

        // Re-validate the MERGED seam map against the registry. `validate_all`
        // above checked `workflow.seams` alone — but the floor merge may
        // yield a map with boundaries from `layers.toml` that the workflow
        // validator never saw. Running the same validator against the
        // merged view catches unknown check IDs and applies_to mismatches
        // in layers.toml-declared boundaries too.
        let mut merged_workflow = workflow.clone();
        merged_workflow.seams = Some(merged_seams.clone());
        let merged_report = pice_core::workflow::validate::validate_seams(
            &merged_workflow,
            &layers_config,
            &seam_registry,
        );
        if !merged_report.is_ok() {
            if req.json {
                let errors: Vec<serde_json::Value> = merged_report
                    .errors
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "field": e.field,
                            "message": e.message,
                        })
                    })
                    .collect();
                let value = serde_json::json!({
                    "status": "merged-seam-validation-failed",
                    "errors": errors,
                });
                return Ok(CommandResponse::ExitJson { code: 1, value });
            }
            let mut message = String::from(
                "merged seam map has validation errors (layers.toml + workflow.yaml):\n",
            );
            for e in &merged_report.errors {
                message.push_str(&format!("  - {}: {}\n", e.field, e.message));
            }
            return Ok(CommandResponse::Exit { code: 1, message });
        }

        let stack_cfg = crate::orchestrator::stack_loops::StackLoopsConfig {
            layers: &layers_config,
            plan_path: &plan_path,
            project_root,
            primary_provider: &config.evaluation.primary.provider,
            primary_model: &config.evaluation.primary.model,
            pice_config: config,
            workflow: &workflow,
            merged_seams: &merged_seams,
        };
        let manifest =
            crate::orchestrator::stack_loops::run_stack_loops(&stack_cfg, sink, req.json).await?;

        // Seam-aware exit code: if any layer is Failed (including via a
        // seam finding), we exit 2. Overall status being InProgress from
        // Phase 1 (provider not wired) is NOT a failure — exit 0.
        use pice_core::layers::manifest::{CheckStatus, LayerStatus, ManifestStatus};
        let any_failed_layer = manifest
            .layers
            .iter()
            .any(|l| l.status == LayerStatus::Failed);
        let total_seam_checks: usize = manifest.layers.iter().map(|l| l.seam_checks.len()).sum();
        let failed_seam_checks: usize = manifest
            .layers
            .iter()
            .flat_map(|l| l.seam_checks.iter())
            .filter(|c| c.status == CheckStatus::Failed)
            .count();

        // Persist the evaluation + seam findings to the metrics DB. Without
        // this write, the new `seam_findings` table, FK cascade, and CHECK
        // constraints are exercised only by tests, never by production —
        // that was one of the critical silent-bypass findings in the
        // adversarial review. Failures here are logged but non-fatal
        // (per CLAUDE.md — metrics writes must never crash the CLI).
        let normalized_path = metrics::normalize_plan_path(&plan.path, project_root);
        if let Ok(Some(db)) = metrics::open_metrics_db(project_root) {
            // Stack-loops Phase 1 has no per-criterion scores to emit yet —
            // record the evaluation header so seam findings can FK-attach.
            let stack_passed = !any_failed_layer && failed_seam_checks == 0;
            match metrics::store::record_evaluation(
                &db,
                &normalized_path,
                &contract.feature,
                contract.tier,
                stack_passed,
                &config.evaluation.primary.provider,
                &config.evaluation.primary.model,
                None,
                None,
                Some("stack-loops Phase 1 — contract grading pending; seam findings below"),
                &[],
            ) {
                Ok(eval_id) => {
                    for layer in &manifest.layers {
                        for sc in &layer.seam_checks {
                            let status_wire = match sc.status {
                                CheckStatus::Passed => "passed",
                                CheckStatus::Warning => "warning",
                                CheckStatus::Failed => "failed",
                                // Skipped seam findings don't map to a DB
                                // status — the CHECK constraint allows only
                                // passed/warning/failed. Drop the row.
                                CheckStatus::Skipped => continue,
                            };
                            // Skip rows the CHECK constraint would reject
                            // (unregistered-check findings carry no category).
                            let Some(category) = sc.category else {
                                continue;
                            };
                            let row = metrics::store::SeamFindingRow {
                                layer: &layer.name,
                                boundary: &sc.boundary,
                                check_id: &sc.name,
                                category,
                                status: status_wire,
                                details: sc.details.as_deref(),
                            };
                            if let Err(e) = metrics::store::insert_seam_finding(&db, eval_id, &row)
                            {
                                tracing::warn!(
                                    layer = %layer.name,
                                    check = %sc.name,
                                    "failed to insert seam finding: {e}"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to record stack-loops evaluation header: {e}");
                }
            }
        }

        // Format and return results from manifest.
        if req.json {
            let value = serde_json::to_value(&manifest)?;
            if any_failed_layer {
                // Structured JSON-mode failure — `ExitJson` routes to stdout
                // with exit 2. See `.claude/rules/daemon.md` → "Structured
                // JSON failure responses".
                return Ok(CommandResponse::ExitJson { code: 2, value });
            }
            return Ok(CommandResponse::Json { value });
        }

        let mut output = format!(
            "\nStack Loops Evaluation — {} layers\n",
            manifest.layers.len()
        );
        output.push_str(&"=".repeat(39));
        output.push('\n');
        for lr in &manifest.layers {
            let status_str = match lr.status {
                LayerStatus::Passed => "PASS",
                LayerStatus::Failed => "FAIL",
                LayerStatus::Pending => "PENDING",
                LayerStatus::InProgress => "IN-PROGRESS",
                LayerStatus::Skipped => "SKIP",
            };
            let detail = lr
                .halted_by
                .as_ref()
                .map(|r| format!(" — {r}"))
                .unwrap_or_default();
            output.push_str(&format!("  [{status_str}] {}{detail}\n", lr.name));

            if !lr.seam_checks.is_empty() {
                let passed = lr
                    .seam_checks
                    .iter()
                    .filter(|c| c.status == CheckStatus::Passed)
                    .count();
                output.push_str(&format!(
                    "    seam: {}/{} passed\n",
                    passed,
                    lr.seam_checks.len()
                ));
                for c in &lr.seam_checks {
                    if c.status == CheckStatus::Failed {
                        let details = c.details.as_deref().unwrap_or("");
                        output.push_str(&format!(
                            "      ✗ {} ({}): {}\n",
                            c.name, c.boundary, details
                        ));
                    } else if c.status == CheckStatus::Warning {
                        let details = c.details.as_deref().unwrap_or("");
                        output.push_str(&format!(
                            "      ! {} ({}): {}\n",
                            c.name, c.boundary, details
                        ));
                    }
                }
            }
        }
        if total_seam_checks > 0 {
            output.push_str(&format!(
                "\nSeam checks: {}/{} passed ({} failed)\n",
                total_seam_checks - failed_seam_checks,
                total_seam_checks,
                failed_seam_checks
            ));
        }
        let overall = match manifest.overall_status {
            ManifestStatus::Passed => "PASS",
            ManifestStatus::InProgress => "IN-PROGRESS",
            _ => "FAIL",
        };
        output.push_str(&format!("\nOverall: {overall}\n"));

        if any_failed_layer {
            return Ok(CommandResponse::Exit {
                code: 2,
                message: output,
            });
        }
        return Ok(CommandResponse::Text { content: output });
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
