use anyhow::{Context, Result};
use clap::Args;
use pice_protocol::EvaluateResultParams;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::config::PiceConfig;
use crate::engine::{orchestrator::ProviderOrchestrator, output, plan_parser, prompt};
use crate::metrics;

#[derive(Args, Debug)]
pub struct EvaluateArgs {
    /// Path to the plan file to evaluate against
    pub plan_path: PathBuf,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &EvaluateArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    // 1. Parse plan and extract contract
    let plan = plan_parser::ParsedPlan::load(&args.plan_path)?;
    let contract = plan
        .contract
        .as_ref()
        .context("plan has no contract section -- cannot evaluate")?;
    let tier = contract.tier;

    // 2. Load config
    let config = PiceConfig::load(&project_root.join(".pice/config.toml"))
        .unwrap_or_else(|_| PiceConfig::default());

    // 3. Gather evaluation context (blocking IO, called before tokio::join!)
    let diff = prompt::get_git_diff(&project_root)?;
    let claude_md = prompt::read_claude_md(&project_root)?;
    let contract_json = serde_json::to_value(contract)?;

    if !args.json {
        println!("Evaluating: {} (Tier {})", plan.title, tier);
        if tier >= 3 {
            println!(
                "Note: Tier 3 agent-team orchestration is not yet implemented. \
                 Running Tier 2 dual-model evaluation instead."
            );
        }
    }

    // 4. Run evaluation based on tier
    if tier >= 2 && config.evaluation.adversarial.enabled {
        // Tier 2+: run primary and adversarial in parallel
        let primary_config = config.clone();
        let adversarial_config = config.clone();
        let primary_contract = contract_json.clone();
        let adversarial_contract = contract_json.clone();
        let primary_diff = diff.clone();
        let adversarial_diff = diff.clone();
        let primary_claude_md = claude_md.clone();
        let adversarial_claude_md = claude_md.clone();

        info!("running parallel dual-model evaluation (Tier {tier})");

        let (primary_result, adversarial_result) = tokio::join!(
            run_primary_evaluation(
                &primary_config,
                primary_contract,
                primary_diff,
                primary_claude_md,
            ),
            run_adversarial_evaluation(
                &adversarial_config,
                adversarial_contract,
                adversarial_diff,
                adversarial_claude_md,
            ),
        );

        let mut primary = primary_result?;
        let (adversarial, adversarial_degraded) = match adversarial_result {
            Ok(result) => (Some(result), false),
            Err(e) => {
                warn!("adversarial evaluation failed: {e}");
                if !args.json {
                    println!("Warning: Adversarial evaluation skipped ({e})");
                }
                (None, true)
            }
        };

        // 5. Enforce contract: recompute pass/fail from scores and thresholds
        enforce_contract(&mut primary, contract);

        // 5b. Record to metrics DB (non-fatal)
        record_metrics(
            &project_root,
            &plan.path,
            &contract.feature,
            tier,
            &primary,
            &config,
            Some(&config.evaluation.adversarial.provider),
            Some(&config.evaluation.adversarial.model),
        );

        // 6. Display results
        if args.json {
            let mut json = output::evaluation_json(&primary, adversarial.as_ref(), tier);
            if adversarial_degraded {
                json["adversarialDegraded"] = serde_json::json!(true);
            }
            println!("{}", serde_json::to_string_pretty(&json)?);
        } else {
            output::print_evaluation_report(&primary, adversarial.as_ref(), tier);
        }

        // 7. Exit code: 0 = pass, 2 = fail
        if !primary.passed {
            std::process::exit(2);
        }
    } else {
        // Tier 1: single-model evaluation only
        info!("running single-model evaluation (Tier {tier})");
        let mut primary = run_primary_evaluation(&config, contract_json, diff, claude_md).await?;

        // Enforce contract: recompute pass/fail from scores and thresholds
        enforce_contract(&mut primary, contract);

        // Record to metrics DB (non-fatal)
        record_metrics(
            &project_root,
            &plan.path,
            &contract.feature,
            tier,
            &primary,
            &config,
            None,
            None,
        );

        if args.json {
            let json = output::evaluation_json(&primary, None, tier);
            println!("{}", serde_json::to_string_pretty(&json)?);
        } else {
            output::print_evaluation_report(&primary, None, tier);
        }

        if !primary.passed {
            std::process::exit(2);
        }
    }

    Ok(())
}

/// Recompute pass/fail from the actual scores and contract thresholds.
/// The provider's `passed` field is NOT trusted — the Rust core enforces the contract.
///
/// Contract enforcement rules:
/// 1. Returned scores matching contract criteria get their threshold enforced
/// 2. Contract criteria with NO matching returned score are treated as failures (score 0)
/// 3. Overall pass requires ALL contract criteria to meet their thresholds
fn enforce_contract(result: &mut EvaluateResultParams, contract: &plan_parser::PlanContract) {
    use pice_protocol::CriterionScore;

    // Match returned scores against contract criteria by name and enforce thresholds
    for score in &mut result.scores {
        if let Some(criterion) = contract.criteria.iter().find(|c| c.name == score.name) {
            score.passed = score.score >= criterion.threshold;
            score.threshold = criterion.threshold;
        }
    }

    // Check for contract criteria that have NO matching returned score — these are failures
    for criterion in &contract.criteria {
        if !result.scores.iter().any(|s| s.name == criterion.name) {
            warn!(
                "contract criterion '{}' has no matching score from provider — marking as failed",
                criterion.name
            );
            result.scores.push(CriterionScore {
                name: criterion.name.clone(),
                score: 0,
                threshold: criterion.threshold,
                passed: false,
                findings: Some("No score returned by provider for this criterion".to_string()),
            });
        }
    }

    // Overall pass: computed ONLY from contract criteria, not extra provider scores.
    // Extra scores are kept for display but don't affect the verdict.
    let contract_names: Vec<&str> = contract.criteria.iter().map(|c| c.name.as_str()).collect();
    let all_contract_passed = result
        .scores
        .iter()
        .filter(|s| contract_names.contains(&s.name.as_str()))
        .all(|s| s.passed);
    if result.passed != all_contract_passed {
        warn!(
            "provider reported passed={}, but core computed passed={} from contract criteria — using core result",
            result.passed, all_contract_passed
        );
    }
    result.passed = all_contract_passed;
}

#[allow(clippy::too_many_arguments)]
fn record_metrics(
    project_root: &Path,
    plan_path: &str,
    feature_name: &str,
    tier: u8,
    result: &EvaluateResultParams,
    config: &PiceConfig,
    adversarial_provider: Option<&str>,
    adversarial_model: Option<&str>,
) {
    if let Ok(Some(db)) = metrics::open_metrics_db(project_root) {
        // Normalize plan path to canonical form for consistent DB keys
        let normalized_path = metrics::normalize_plan_path(plan_path, project_root);

        // Compute average score for telemetry
        let avg_score = if result.scores.is_empty() {
            0.0
        } else {
            result.scores.iter().map(|s| s.score as f64).sum::<f64>() / result.scores.len() as f64
        };

        if let Err(e) = metrics::store::record_evaluation(
            &db,
            &normalized_path,
            feature_name,
            tier,
            result.passed,
            &config.evaluation.primary.provider,
            &config.evaluation.primary.model,
            adversarial_provider,
            adversarial_model,
            result.summary.as_deref(),
            &result.scores,
        ) {
            warn!("failed to record evaluation metrics: {e}");
        }

        // Queue telemetry (opt-in, non-fatal)
        if config.telemetry.enabled {
            let client = metrics::telemetry::TelemetryClient::new(&config.telemetry, project_root);
            let event = metrics::telemetry::TelemetryEvent {
                event_type: "evaluation".to_string(),
                tier: Some(tier),
                passed: Some(result.passed),
                score_avg: Some(avg_score),
                provider_type: config.evaluation.primary.provider.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            if let Err(e) = client.queue_event(&db, &event) {
                tracing::debug!("telemetry queue failed: {e}");
            }
        }
    }
}

async fn run_primary_evaluation(
    config: &PiceConfig,
    contract_json: serde_json::Value,
    diff: String,
    claude_md: String,
) -> Result<EvaluateResultParams> {
    let mut orchestrator =
        ProviderOrchestrator::start(&config.evaluation.primary.provider, config).await?;

    let result = orchestrator
        .evaluate(
            contract_json,
            diff,
            claude_md,
            Some(config.evaluation.primary.model.clone()),
            None,
        )
        .await;

    // Always shutdown the provider, even if evaluate() failed.
    // Shutdown errors are non-fatal — the evaluation result is what matters.
    if let Err(e) = orchestrator.shutdown().await {
        warn!("provider shutdown failed: {e}");
    }
    result
}

async fn run_adversarial_evaluation(
    config: &PiceConfig,
    contract_json: serde_json::Value,
    diff: String,
    claude_md: String,
) -> Result<serde_json::Value> {
    let mut orchestrator =
        ProviderOrchestrator::start(&config.evaluation.adversarial.provider, config).await?;

    let result = orchestrator
        .evaluate(
            contract_json,
            diff,
            claude_md,
            Some(config.evaluation.adversarial.model.clone()),
            Some(config.evaluation.adversarial.effort.clone()),
        )
        .await;

    // Always shutdown the provider, even if evaluate() failed.
    // Shutdown errors are non-fatal.
    if let Err(e) = orchestrator.shutdown().await {
        warn!("adversarial provider shutdown failed: {e}");
    }

    // Convert to generic JSON for the adversarial side
    Ok(serde_json::to_value(result?)?)
}
