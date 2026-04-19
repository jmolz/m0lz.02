//! `pice evaluate` handler — grade contract criteria with dual-model evaluation.

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, EvaluateRequest, ExitJsonStatus};
use pice_core::plan_parser::ParsedPlan;
use pice_core::prompt::helpers::{get_git_diff, read_claude_md};
use pice_protocol::CriterionScore;
use serde_json::json;

use crate::metrics;
use crate::orchestrator::{ProviderOrchestrator, StreamSink};
use crate::server::router::DaemonContext;

/// Build the `MetricsPersistFailed` response in either JSON or text mode.
/// Shared by the startup DB-open / header-insert paths (Pass-7) and the
/// post-evaluation finalize / seam-finding paths (Pass-6). Exit code is
/// 1 — this is a persistence failure, not a contract failure (exit 2).
fn metrics_persist_failed_response(json_mode: bool, errors: Vec<String>) -> CommandResponse {
    if json_mode {
        let value = json!({
            "status": ExitJsonStatus::MetricsPersistFailed.as_str(),
            "errors": errors,
            "hint": "Metrics persistence failed. The verification manifest at \
                     ~/.pice/state/.../manifest.json is authoritative for state; \
                     the metrics DB is the audit trail. Inspect daemon logs for \
                     the SQLite error.",
        });
        return CommandResponse::ExitJson { code: 1, value };
    }
    let mut message = String::from("evaluation halted: metrics persistence failed:\n");
    for e in &errors {
        message.push_str(&format!("  - {e}\n"));
    }
    message.push_str(
        "\nThe verification manifest is authoritative; the audit trail is \
         incomplete. See daemon logs for the SQLite error.\n",
    );
    CommandResponse::Exit { code: 1, message }
}

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
                    "status": ExitJsonStatus::PlanNotFound.as_str(),
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
                        "status": ExitJsonStatus::PlanParseFailed.as_str(),
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
                        "status": ExitJsonStatus::NoContractSection.as_str(),
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
                    "status": ExitJsonStatus::WorkflowValidationFailed.as_str(),
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
                    "status": ExitJsonStatus::SeamFloorViolation.as_str(),
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
                    "status": ExitJsonStatus::MergedSeamValidationFailed.as_str(),
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

        // Phase 4.1 Pass-6 Codex High #2: acquire the per-manifest
        // single-writer lock BEFORE touching any state. The lock serializes
        // two concurrent `pice evaluate` calls on the SAME
        // {project_hash, feature_id} pair so they don't race on the
        // atomic-rename dance at `~/.pice/state/.../manifest.json` +
        // `metrics.db` writes. Distinct features still run in parallel —
        // the lock key includes feature_id, so different contracts get
        // distinct mutexes.
        //
        // The lock is held for the full evaluation (manifest writes,
        // run_stack_loops, finalize). `tokio::sync::Mutex` so it survives
        // the `.await` on `run_stack_loops`.
        //
        // Phase 4.1 Pass-8 Codex High #3: the lock identity MUST match
        // the on-disk manifest identity. `stack_loops.rs` derives
        // `feature_id` from `plan_path.file_stem()` and writes the
        // manifest at `~/.pice/state/{project_hash}/{feature_id}.manifest.json`.
        // Previously, the lock keyed on `contract.feature` (the free-form
        // JSON `"feature"` string from `## Contract`, e.g. "PRDv2 Phase 4
        // — Adaptive Evaluation"). Two evaluate calls with different
        // `contract.feature` strings but the same plan filename would hit
        // DIFFERENT mutexes while writing the SAME manifest — the single-
        // writer guarantee was a lie. Keying on the plan file stem
        // restores the invariant: one lock per on-disk manifest path.
        let project_namespace =
            pice_core::layers::manifest::manifest_project_namespace(project_root);
        // Matches `stack_loops.rs::run_stack_loops` line 88-92 verbatim.
        // Any divergence breaks the single-writer guarantee.
        let manifest_feature_id = std::path::Path::new(&plan.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let _manifest_lock = ctx.manifest_lock_for(&project_namespace, &manifest_feature_id);
        let _manifest_guard = _manifest_lock.lock().await;

        // Phase 4.1 Pass-10 Codex HIGH #2: acquire the cross-process
        // advisory file lock. The Pass-6 tokio mutex above only serializes
        // tasks WITHIN this daemon process; two separate processes (e.g.
        // two `PICE_DAEMON_INLINE=1` CLI invocations, or a background
        // daemon plus an inline CLI) each allocate their own mutex map
        // and therefore race on the atomic-rename dance at
        // `~/.pice/state/.../manifest.json`. The file lock closes that
        // gap via POSIX `flock` / Windows `LockFileEx`.
        //
        // The `tokio::task::spawn_blocking` wrap is intentional: `flock`
        // is a blocking syscall that stalls the worker thread until the
        // lock is released. On a multi-threaded tokio runtime a stalled
        // worker degrades throughput but does not deadlock. Running the
        // call on the blocking pool keeps the main runtime responsive
        // to unrelated tasks (e.g. another feature's `evaluate` or a
        // `daemon/health` probe).
        let ctx_for_lock = {
            let feature_id = manifest_feature_id.clone();
            let project_root = ctx.project_root().clone();
            (feature_id, project_root)
        };
        let _manifest_file_lock = {
            let (feature_id, project_root) = ctx_for_lock;
            tokio::task::spawn_blocking(move || {
                // Re-derive the path from project_root instead of threading
                // &DaemonContext through spawn_blocking — keeps lifetimes
                // simple. Matches `VerificationManifest::manifest_path_for`.
                use fs2::FileExt;
                use pice_core::layers::manifest::VerificationManifest;
                use std::fs::OpenOptions;
                let manifest_path =
                    VerificationManifest::manifest_path_for(&feature_id, &project_root)?;
                if let Some(parent) = manifest_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let lock_path = manifest_path.with_extension("manifest.lock");
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&lock_path)?;
                file.lock_exclusive()?;
                anyhow::Ok(file)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking joined with error: {e}"))??
        };

        // Phase 4: create the evaluation header BEFORE running stack loops.
        // This gives the adaptive loop a valid `evaluation_id` to FK-attach
        // `pass_events` to, persisted BEFORE each halt-decision check. The
        // placeholder row's `passed = 0` and `summary = NULL` are rewritten
        // by `finalize_evaluation` after the loop returns.
        //
        // `MetricsDb` is `!Sync` (prepared-statement cache uses `RefCell`),
        // so the sink holds `Arc<Mutex<MetricsDb>>` to stay `Send` across
        // the `run_stack_loops` await — see the sink's docstring in
        // `metrics::store`.
        use std::sync::{Arc, Mutex};
        let normalized_path = metrics::normalize_plan_path(&plan.path, project_root);

        // Phase 4.1 Pass-7 Codex High #2: distinguish a real DB-open failure
        // (corrupt file, permission error, disk) from `Ok(None)` (no DB file
        // yet — uninitialized project, legitimate "no metrics configured"
        // path). Before the fix, `.ok().flatten()` collapsed both cases to
        // `None` and the handler silently degraded to `NullPassSink`,
        // returning a green response with no `pass_events`, no `evaluations`
        // row, and no fail-closed signal — exactly the class of silent
        // corruption the Pass-6 `MetricsPersistFailed` fix closed on the
        // mid-loop path. Now fail closed at startup too.
        let db_arc: Option<Arc<Mutex<metrics::db::MetricsDb>>> =
            match metrics::open_metrics_db(project_root) {
                Ok(Some(db)) => Some(Arc::new(Mutex::new(db))),
                // DB file simply doesn't exist → proceed without metrics.
                // `pice init` hasn't run; this is a legitimate mode, not a
                // failure. Adaptive loop runs with `NullPassSink`.
                Ok(None) => None,
                Err(e) => {
                    let msg = format!("open_metrics_db: {e}");
                    tracing::error!("{msg}");
                    return Ok(metrics_persist_failed_response(req.json, vec![msg]));
                }
            };

        let eval_id = match db_arc.as_ref() {
            Some(db) => {
                // Mutex poisoning must not crash the daemon (CLAUDE.md
                // rust-core rule: no unwrap/expect in library code).
                let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                match metrics::store::insert_evaluation_header(
                    &guard,
                    &normalized_path,
                    &contract.feature,
                    contract.tier,
                    &config.evaluation.primary.provider,
                    &config.evaluation.primary.model,
                    None,
                    None,
                ) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        // Pass-7 Codex High #2: header insert failure means
                        // any later `pass_events` rows would have no FK
                        // parent. Fail closed — previously this logged
                        // `warn!` and set `eval_id = None`, which muted the
                        // sink and silently returned success.
                        drop(guard);
                        let msg = format!("insert_evaluation_header: {e}");
                        tracing::error!("{msg}");
                        return Ok(metrics_persist_failed_response(req.json, vec![msg]));
                    }
                }
            }
            None => None,
        };
        // Phase 5 cohort parallelism: `PassMetricsSink` is now `&self +
        // Send + Sync`; the orchestrator holds an `Arc<dyn ...>` and
        // clones it into each cohort task. Constructing the Arc here
        // keeps the sink alive for the whole evaluation (all cohort
        // tasks drop their clones before `run_stack_loops` returns).
        let pass_sink: std::sync::Arc<dyn crate::orchestrator::PassMetricsSink> =
            match (db_arc.as_ref(), eval_id) {
                (Some(db), Some(eid)) => std::sync::Arc::new(metrics::store::DbBackedPassSink {
                    db: db.clone(),
                    evaluation_id: eid,
                }),
                _ => std::sync::Arc::new(crate::orchestrator::NullPassSink),
            };
        let manifest = crate::orchestrator::stack_loops::run_stack_loops(
            &stack_cfg, sink, req.json, pass_sink,
        )
        .await?;

        // Seam-aware exit code: if any layer is Failed (including via a
        // seam finding), we exit 2. Overall status being InProgress from
        // Phase 1 (provider not wired) is NOT a failure — exit 0.
        use pice_core::layers::manifest::{CheckStatus, LayerStatus, ManifestStatus};
        // Phase 4.1 Pass-11 Codex HIGH #2: a per-pass sink failure marks the
        // layer Pending with `halted_by = "metrics_persist_failed:..."`. That
        // is operational (audit trail / SQLite broken) — NOT a contract
        // failure. Route it through `metrics_persist_failed_response` (exit 1)
        // before any contract-pass/fail accounting so CI sees "audit trail
        // broken, retry" rather than "evaluation failed (exit 2)". Pass-11.1
        // W2: prefix-check sourced from
        // `ExitJsonStatus::is_metrics_persist_failed` so this site, the
        // adaptive loop's halt-string construction, and
        // `build_adaptive_layer_result`'s status mapping all consult the
        // same constant. Drift between any two would silently misroute.
        let mid_loop_metrics_errors: Vec<String> = manifest
            .layers
            .iter()
            .filter_map(|l| l.halted_by.as_deref())
            .filter(|h| ExitJsonStatus::is_metrics_persist_failed(h))
            .map(|h| h.to_string())
            .collect();
        if !mid_loop_metrics_errors.is_empty() {
            return Ok(metrics_persist_failed_response(
                req.json,
                mid_loop_metrics_errors,
            ));
        }
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

        // Persist the evaluation summary + seam findings to the metrics DB.
        // The header was inserted pre-loop; finalize and attach children now.
        // Failures here are logged but non-fatal (per CLAUDE.md — metrics
        // writes must never crash the CLI).
        //
        // The sink has already been dropped back to `None` implicitly by
        // going out of scope at the end of `run_stack_loops`; we re-lock
        // `db_arc` here for the summary + seam writes. No contention.
        if let (Some(db_arc), Some(eval_id)) = (db_arc.as_ref(), eval_id) {
            // Mutex poisoning on a long-lived daemon must not crash the
            // handler. Recover the inner guard per CLAUDE.md "Never
            // unwrap()/expect() in library code" rule.
            let db = db_arc.lock().unwrap_or_else(|p| p.into_inner());
            // Phase 4 post-adversarial-review fix (Codex Critical #2): only
            // mark an evaluation `passed=1` when every layer actually
            // reached `Passed`. `Pending` layers (budget, max_passes, or the
            // phase-1-pending provider-fallback) indicate the loop DID NOT
            // grade the layer — persisting `passed=1` for such a run would
            // green-light unfinished work downstream in `pice status`.
            let all_layers_passed = manifest
                .layers
                .iter()
                .all(|l| l.status == LayerStatus::Passed || l.status == LayerStatus::Skipped);
            let stack_passed = all_layers_passed && !any_failed_layer && failed_seam_checks == 0;

            // Phase 4 Pass-5 Claude Evaluator B Critical #4: the two UPDATEs
            // (finalize + adaptive summary) used to run as separate statements.
            // A SIGKILL between them left `final_total_cost_usd = NULL` on a
            // row whose `pass_events` had already been written, and the
            // Criterion 16 reconciliation SQL silently excluded the row
            // because `ABS(NULL - SUM) > 1e-9` evaluates to NULL. We now fuse
            // them into one atomic UPDATE via
            // `finalize_evaluation_with_adaptive_summary`, closing the window
            // entirely.
            //
            // Aggregate adaptive summary columns across layers. The pass
            // count is total across layers (matches the `pass_events` row
            // count per `evaluation_id`, required for cost reconciliation).
            // `total_cost_usd` sums per-layer costs so it equals
            // `SUM(pass_events.cost_usd)` within 1e-9 — the Phase 4 contract
            // criterion #16 cost-reconciliation invariant.
            //
            // Phase 4.1 Pass-8 Codex Medium #1: exclude synthetic
            // placeholder passes from the count. `phase1_pending_layer_result`
            // and `runtime_failed_layer_result` both construct a single
            // `PassResult { index: 0, ... }` to keep the manifest shape
            // well-formed for downstream readers — but those passes
            // emitted NO `pass_events` rows. Summing `passes.len()` as-is
            // wrote `passes_used = 1` while the audit table had 0 rows,
            // breaking Criterion 17's "passes_used matches pass_events
            // row count" invariant on provider-not-started / init-failed
            // runs. Real adaptive passes are indexed `1..=max_passes`
            // (see adaptive_loop.rs:220), so `index > 0` cleanly
            // partitions real passes from placeholders.
            let passes_used: u32 = manifest
                .layers
                .iter()
                .flat_map(|l| l.passes.iter())
                .filter(|p| p.index > 0)
                .count() as u32;
            // Phase 4 Pass-4 fix for Codex High: previously `sum > 0.0` gated
            // emission, which collapsed `0.0` sums to None — breaking the
            // cost-reconciliation invariant that `SUM(pass_events.cost_usd) ==
            // evaluations.final_total_cost_usd` (layers can report Some(0.0)
            // for zero-cost providers; we must preserve that as Some(0.0)
            // upward, not None).
            let final_total_cost_usd: Option<f64> = {
                let any_reported = manifest.layers.iter().any(|l| l.total_cost_usd.is_some());
                if any_reported {
                    Some(
                        manifest
                            .layers
                            .iter()
                            .filter_map(|l| l.total_cost_usd)
                            .sum(),
                    )
                } else {
                    None
                }
            };
            // `halted_by`: prefer a failed layer's reason for triage; fall
            // back to the first non-pending layer that actually ran.
            let halted_by_wire: Option<String> = manifest
                .layers
                .iter()
                .find(|l| l.status == LayerStatus::Failed)
                .and_then(|l| l.halted_by.clone())
                .or_else(|| {
                    manifest
                        .layers
                        .iter()
                        .find(|l| {
                            l.status != LayerStatus::Pending && l.status != LayerStatus::Skipped
                        })
                        .and_then(|l| l.halted_by.clone())
                });
            // `final_confidence`: max across layers (optimistic; the per-layer
            // manifest carries the authoritative per-layer value anyway).
            let final_confidence: Option<f64> = manifest
                .layers
                .iter()
                .filter_map(|l| l.final_confidence)
                .fold(None, |acc, c| match acc {
                    Some(a) if a >= c => Some(a),
                    _ => Some(c),
                });
            // Project-wide algorithm wire form.
            let algo_wire = match workflow.phases.evaluate.adaptive_algorithm {
                pice_core::workflow::schema::AdaptiveAlgo::BayesianSprt => "bayesian_sprt",
                pice_core::workflow::schema::AdaptiveAlgo::Adts => "adts",
                pice_core::workflow::schema::AdaptiveAlgo::Vec => "vec",
                pice_core::workflow::schema::AdaptiveAlgo::None => "none",
            };
            // Phase 4.1 Pass-6 Codex High #4: before this fix, a failed
            // finalize UPDATE logged `warn!` and the handler returned
            // success — producing a DB row with placeholder/NULL summary
            // fields that dashboards would silently render as "pending"
            // forever. We now capture metrics-persistence errors and
            // surface them via the typed `ExitJsonStatus::MetricsPersistFailed`
            // discriminant before returning. The manifest file remains the
            // source of truth for state (per CLAUDE.md — it's crash-safe
            // atomic-rename); the metrics DB is the audit trail. A broken
            // audit trail with a green handler response is the exact
            // silent-corruption surface this fix closes.
            let mut metrics_errors: Vec<String> = Vec::new();
            if let Err(e) = metrics::store::finalize_evaluation_with_adaptive_summary(
                &db,
                eval_id,
                stack_passed,
                Some("stack-loops — adaptive evaluation; see pass_events and seam_findings"),
                passes_used,
                halted_by_wire.as_deref(),
                Some(algo_wire),
                final_confidence,
                final_total_cost_usd,
            ) {
                tracing::warn!("failed to finalize evaluation with adaptive summary: {e}");
                metrics_errors.push(format!("finalize_evaluation: {e}"));
            }

            // Seam findings attach via FK to `evaluation_id`.
            {
                // Phase 3 round-4 adversarial review fix: when both
                // sides of a boundary are active, run_seams_for_layer
                // attributes the SAME (boundary, check_id) result to
                // BOTH layers' `seam_checks`. The per-layer manifest
                // copy is intentional (each layer's view is a complete
                // picture). But persisting both as separate rows would
                // double-count category analytics. Dedupe here on
                // (boundary, check_id) and attribute the canonical row
                // to the first layer encountered in `manifest.layers`
                // iteration order (layers.toml declaration order, which
                // is deterministic across runs).
                let mut seen: std::collections::HashSet<(String, String)> =
                    std::collections::HashSet::new();
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
                        // Bilateral dedupe: one DB row per
                        // (eval_id, boundary, check_id).
                        let key = (sc.boundary.clone(), sc.name.clone());
                        if !seen.insert(key) {
                            continue;
                        }
                        let row = metrics::store::SeamFindingRow {
                            layer: &layer.name,
                            boundary: &sc.boundary,
                            check_id: &sc.name,
                            category,
                            status: status_wire,
                            details: sc.details.as_deref(),
                        };
                        if let Err(e) = metrics::store::insert_seam_finding(&db, eval_id, &row) {
                            tracing::warn!(
                                layer = %layer.name,
                                check = %sc.name,
                                "failed to insert seam finding: {e}"
                            );
                            metrics_errors
                                .push(format!("seam_finding[{}/{}]: {e}", layer.name, sc.name));
                        }
                    }
                }
            }

            // Phase 4.1 Pass-6 Codex High #4 fail-close: any metrics-persist
            // error becomes an observable failure. The evaluation's contract
            // result is still computed from the manifest (returned above),
            // but the handler response reflects the persistence failure so
            // CI pipelines treat it as a real incident rather than rolling
            // past a ghost success.
            if !metrics_errors.is_empty() {
                return Ok(metrics_persist_failed_response(req.json, metrics_errors));
            }
        }

        // Format and return results from manifest.
        if req.json {
            let mut value = serde_json::to_value(&manifest)?;
            if any_failed_layer {
                // Phase 4 contract criterion #11: inject the typed status
                // discriminant so CLI-boundary tests can pattern-match on
                // `ExitJsonStatus::EvaluationFailed.as_str()` rather than
                // a literal wire string. The manifest fields remain
                // top-level for backwards compatibility with existing
                // exit-2 fixture consumers (e.g. `evaluate_integration.rs`
                // reads `json["layers"]` directly).
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "status".to_string(),
                        serde_json::json!(ExitJsonStatus::EvaluationFailed.as_str()),
                    );
                }
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

    // Record to metrics DB. Pass-8 Codex High #2: distinguish a real DB-open
    // failure (corrupt, permission error, directory-in-place-of-file) from
    // `Ok(None)` (no DB file yet — legitimate "no metrics configured" path).
    // Before the fix, `.ok().flatten()`-style `if let Ok(Some(db)) = ...`
    // collapsed both cases to "skip persistence" and the legacy v0.1 branch
    // silently returned a green response with no audit-trail entry even
    // when the DB was unreadable — exactly the silent-corruption class the
    // Pass-7 Stack Loops branch closed via `metrics_persist_failed_response`.
    // Fail closed on the legacy path too so `MetricsPersistFailed` is the
    // single source of truth for both branches of this handler.
    let normalized_path = metrics::normalize_plan_path(&plan.path, project_root);
    let db_opt = match metrics::open_metrics_db(project_root) {
        Ok(Some(db)) => Some(db),
        Ok(None) => None,
        Err(e) => {
            let msg = format!("open_metrics_db (legacy v0.1 path): {e}");
            tracing::error!("{msg}");
            return Ok(metrics_persist_failed_response(req.json, vec![msg]));
        }
    };
    if let Some(db) = db_opt {
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
            // Pass-8 Codex High #2: a successful DB open followed by a
            // failed write is also fail-closed on the legacy path. The
            // record contains the feature, tier, pass/fail verdict, and
            // model identity — dropping it silently would leave downstream
            // `pice status` and `pice metrics` unable to distinguish
            // "never ran" from "ran and disappeared."
            let msg = format!("record_evaluation (legacy v0.1 path): {e}");
            tracing::error!("{msg}");
            return Ok(metrics_persist_failed_response(req.json, vec![msg]));
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
            // Exit code 2 when evaluation fails (contract criteria not met).
            // Per .claude/rules/daemon.md: JSON-mode failure MUST use ExitJson
            // (stdout), never Exit with stringified JSON (stderr). The v0.1
            // path previously violated this — fixed in Phase 3 code review.
            if req.json {
                Ok(CommandResponse::ExitJson {
                    code: 2,
                    value: result,
                })
            } else {
                Ok(CommandResponse::Exit {
                    code: 2,
                    message: serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|_| "evaluation failed".to_string()),
                })
            }
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
