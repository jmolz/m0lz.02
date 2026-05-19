//! `pice execute` handler — run a plan through the provider.

use std::sync::Arc;

use anyhow::Result;
use pice_core::cli::{CancelledReason, CommandResponse, ExecuteRequest, ExitJsonStatus};
use pice_core::layers::manifest::ManifestStatus;
use pice_core::plan_parser::{ParsedPlan, PlanTrace};
use serde_json::json;

use super::background::{
    dispatch_background, feature_id_from_plan_path, finalize_terminal_manifest,
    transition_queued_to_in_progress, BackgroundDispatchInputs, PlanDispatchSnapshot,
};
use super::to_shared_sink;
use crate::memory::recall::{build_memory_recall, record_read_metrics, MemoryReadMetrics};
use crate::memory::recorder::{
    deterministic_execute_summary, record_write_metrics, MemoryWriteOutcome, SessionMemoryRecorder,
    SessionRunContext,
};
use crate::memory::store::MemoryPaths;
use crate::orchestrator::session::{self, streaming_handler};
use crate::orchestrator::{ProviderOrchestrator, StreamSink};
use crate::prompt::builders;
use crate::server::router::DaemonContext;

fn workflow_resolve_failed_response(error: &anyhow::Error, json_mode: bool) -> CommandResponse {
    let message = format!("{error:#}");
    if json_mode {
        return CommandResponse::ExitJson {
            code: ExitJsonStatus::WorkflowValidationFailed.exit_code(),
            value: json!({
                "status": ExitJsonStatus::WorkflowValidationFailed.as_str(),
                "errors": [{
                    "field": "workflow.yaml",
                    "message": message,
                }],
                "hint": "Run `pice validate` for full details.",
            }),
        };
    }
    CommandResponse::Exit {
        code: ExitJsonStatus::WorkflowValidationFailed.exit_code(),
        message: format!(
            "failed to resolve workflow.yaml:\n  - workflow.yaml: {message}\n\nRun `pice validate` for full details.\n"
        ),
    }
}

fn workflow_validation_failed_response(
    errors: &[pice_core::workflow::validate::ValidationError],
    json_mode: bool,
) -> CommandResponse {
    if json_mode {
        let errors: Vec<serde_json::Value> = errors
            .iter()
            .map(|e| {
                json!({
                    "field": e.field,
                    "message": e.message,
                })
            })
            .collect();
        return CommandResponse::ExitJson {
            code: ExitJsonStatus::WorkflowValidationFailed.exit_code(),
            value: json!({
                "status": ExitJsonStatus::WorkflowValidationFailed.as_str(),
                "errors": errors,
                "hint": "Run `pice validate` for full details.",
            }),
        };
    }
    let mut message = String::from("workflow.yaml has validation errors:\n");
    for e in errors {
        message.push_str(&format!("  - {}: {}\n", e.field, e.message));
    }
    message.push_str("\nRun `pice validate` for full details.\n");
    CommandResponse::Exit {
        code: ExitJsonStatus::WorkflowValidationFailed.exit_code(),
        message,
    }
}

fn resolve_background_workflow_snapshot(
    project_root: &std::path::Path,
    json_mode: bool,
) -> Result<std::result::Result<pice_core::workflow::WorkflowConfig, CommandResponse>> {
    let workflow = match pice_core::workflow::loader::resolve(project_root) {
        Ok(workflow) => workflow,
        Err(e) => return Ok(Err(workflow_resolve_failed_response(&e, json_mode))),
    };
    let report = pice_core::workflow::validate::validate_all(
        &workflow,
        None,
        None,
        Some(&pice_core::seam::default_registry()),
    );
    if !report.is_ok() {
        return Ok(Err(workflow_validation_failed_response(
            &report.errors,
            json_mode,
        )));
    }
    Ok(Ok(workflow))
}

struct PreparedExecutePlan {
    plan: ParsedPlan,
    trace: PlanTrace,
}

struct ExecuteMemoryWrite<'a> {
    project_root: &'a std::path::Path,
    provider_name: &'a str,
    memory_config: &'a pice_core::config::MemoryConfig,
    feature_id: Option<String>,
    plan_path: Option<std::path::PathBuf>,
    trace: Option<&'a PlanTrace>,
    title: &'a str,
    body: &'a str,
}

fn plan_not_found_response(plan_path: &std::path::Path, json_mode: bool) -> CommandResponse {
    if json_mode {
        return CommandResponse::ExitJson {
            code: ExitJsonStatus::PlanNotFound.exit_code(),
            value: json!({
                "status": ExitJsonStatus::PlanNotFound.as_str(),
                "plan_path": plan_path.display().to_string(),
            }),
        };
    }
    CommandResponse::Exit {
        code: ExitJsonStatus::PlanNotFound.exit_code(),
        message: format!("plan file not found: {}", plan_path.display()),
    }
}

fn plan_parse_failed_response(
    plan_path: &std::path::Path,
    error: &anyhow::Error,
    json_mode: bool,
) -> CommandResponse {
    if json_mode {
        return CommandResponse::ExitJson {
            code: ExitJsonStatus::PlanParseFailed.exit_code(),
            value: json!({
                "status": ExitJsonStatus::PlanParseFailed.as_str(),
                "plan_path": plan_path.display().to_string(),
                "error": error.to_string(),
            }),
        };
    }
    CommandResponse::Exit {
        code: ExitJsonStatus::PlanParseFailed.exit_code(),
        message: format!("failed to parse plan: {error}"),
    }
}

fn plan_contract_required_response(
    plan_path: &std::path::Path,
    json_mode: bool,
) -> CommandResponse {
    let status = ExitJsonStatus::PlanContractRequired;
    if json_mode {
        return CommandResponse::ExitJson {
            code: status.exit_code(),
            value: json!({
                "status": status.as_str(),
                "plan_path": plan_path.display().to_string(),
                "hint": "`pice execute` requires a parseable ## Contract so implementation can stay tied to the approved plan.",
            }),
        };
    }
    CommandResponse::Exit {
        code: status.exit_code(),
        message: format!(
            "plan contract required: {} has no parseable ## Contract section",
            plan_path.display()
        ),
    }
}

fn load_execute_plan(
    plan_path: &std::path::Path,
    project_root: &std::path::Path,
    json_mode: bool,
) -> Result<std::result::Result<PreparedExecutePlan, CommandResponse>> {
    if !plan_path.exists() {
        return Ok(Err(plan_not_found_response(plan_path, json_mode)));
    }
    let plan = match ParsedPlan::load(plan_path) {
        Ok(plan) => plan,
        Err(e) => return Ok(Err(plan_parse_failed_response(plan_path, &e, json_mode))),
    };
    let Some(contract) = plan.contract.as_ref() else {
        return Ok(Err(plan_contract_required_response(plan_path, json_mode)));
    };
    let trace = plan.derive_trace(project_root, contract)?;
    Ok(Ok(PreparedExecutePlan { plan, trace }))
}

fn maybe_record_execute_memory(input: ExecuteMemoryWrite<'_>) -> Result<()> {
    let writer = pice_core::memory::MemoryWriter::ExecuteSummary;
    let recorder = SessionMemoryRecorder::new(input.memory_config);
    if recorder.preflight_write(writer).is_some() {
        return Ok(());
    }

    let run_ctx = SessionRunContext::foreground(
        input.project_root,
        input.provider_name,
        pice_core::memory::MemoryConsumer::Execute,
        input.feature_id,
        input.plan_path,
        input.trace,
    )?;
    let write_result = recorder.record_summary(
        &run_ctx,
        writer,
        input.title,
        input.body,
        vec!["execute".to_string(), "summary".to_string()],
    )?;
    record_write_metrics(input.project_root, &run_ctx, writer, &write_result);
    if matches!(write_result, MemoryWriteOutcome::Rejected { .. }) {
        tracing::warn!("execute memory write rejected: {write_result:?}");
    }
    Ok(())
}

pub async fn run(
    req: ExecuteRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();

    // Resolve plan path (relative paths are resolved against project root)
    let plan_path = if req.plan_path.is_absolute() {
        req.plan_path.clone()
    } else {
        project_root.join(&req.plan_path)
    };

    if req.background {
        if let Some(resp) =
            super::background::reject_inline_background_if_active(&plan_path, req.json)
        {
            return Ok(resp);
        }
        let prepared = match load_execute_plan(&plan_path, project_root, req.json)? {
            Ok(prepared) => prepared,
            Err(resp) => return Ok(resp),
        };
        return run_background(req, ctx, &plan_path, prepared).await;
    }

    let prepared = match load_execute_plan(&plan_path, project_root, req.json)? {
        Ok(prepared) => prepared,
        Err(resp) => return Ok(resp),
    };
    let trace = prepared.trace;
    let plan = prepared.plan;
    let feature_id = feature_id_from_plan_path(&plan_path);
    let relative_plan_path = plan_path
        .strip_prefix(project_root)
        .unwrap_or(&plan_path)
        .to_string_lossy()
        .to_string();
    let memory = if config
        .memory
        .policy()
        .can_read(pice_core::memory::MemoryConsumer::Execute)
    {
        let state_dir = pice_core::layers::manifest::VerificationManifest::state_dir()?;
        let paths = MemoryPaths::new(project_root, &state_dir);
        let recall = build_memory_recall(
            &paths,
            &config.memory,
            pice_core::memory::MemoryConsumer::Execute,
            Some(&feature_id),
            Some(&relative_plan_path),
            &chrono::Utc::now().to_rfc3339(),
        )?;
        record_read_metrics(
            project_root,
            &paths,
            MemoryReadMetrics {
                consumer: pice_core::memory::MemoryConsumer::Execute,
                feature_id: Some(&feature_id),
                plan_path: Some(&relative_plan_path),
                run_id: None,
                brief: recall.brief.as_ref(),
                warning: recall.warning,
            },
        );
        recall.brief
    } else {
        None
    };
    let prompt = builders::build_execute_prompt(
        &plan.content,
        project_root,
        &config.provider.name,
        memory.as_ref(),
    )?;

    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, config).await?;
    if !req.json {
        // SAFETY INVARIANT: session is awaited to completion before this handler
        // returns, so the Arc from to_shared_sink is dropped while `sink` is alive.
        orchestrator.on_notification(streaming_handler(to_shared_sink(sink)));
    }

    let result = session::run_session(&mut orchestrator, project_root, prompt).await;
    orchestrator.shutdown().await.ok();
    result?;

    let (title, body) = deterministic_execute_summary(&plan.title, &plan_path);
    maybe_record_execute_memory(ExecuteMemoryWrite {
        project_root,
        provider_name: &config.provider.name,
        memory_config: &config.memory,
        feature_id: Some(feature_id),
        plan_path: Some(plan_path.clone()),
        trace: Some(&trace),
        title: &title,
        body: &body,
    })?;

    if req.json {
        Ok(CommandResponse::Json {
            value: json!({"status": "complete", "plan": plan.title}),
        })
    } else {
        Ok(CommandResponse::Empty)
    }
}

/// Background dispatch for `pice execute --background`.
///
/// `execute` has no Stack Loops / cohort structure — it runs a single
/// provider session. The spawned future:
/// 1. Transitions `Queued → InProgress` without a start event; execute
///    has no DAG layer to report.
/// 2. Starts the provider, runs `session::run_session`, captures
///    any provider error.
/// 3. Writes the terminal manifest (`Passed` on success, `Failed`
///    on provider error) and fires the `FeatureComplete` event.
async fn run_background(
    req: ExecuteRequest,
    ctx: &DaemonContext,
    plan_path: &std::path::Path,
    prepared: PreparedExecutePlan,
) -> Result<CommandResponse> {
    let feature_id = feature_id_from_plan_path(plan_path);

    // Resolve the workflow snapshot for the JobEnv. `execute` does not
    // consult workflow directly, but the snapshot is part of the
    // JobEnv contract (Criterion #16). Missing project/user workflow files
    // already resolve to embedded defaults; malformed or invalid files must
    // fail closed instead of silently dispatching against framework defaults.
    let workflow_snapshot =
        match resolve_background_workflow_snapshot(ctx.project_root(), req.json)? {
            Ok(workflow) => workflow,
            Err(resp) => return Ok(resp),
        };

    // Capture the bits the spawned future needs. `ctx` cannot cross
    // the `'static` spawn boundary; the fields we consume are either
    // Arc-backed (events, logs) or cheap-to-clone (config, plan path).
    let config_owned = ctx.config().clone();
    let events_for_spawn = ctx.events().clone();
    let logs_for_spawn = ctx.logs().clone();

    dispatch_background(
        feature_id,
        req.json,
        plan_path,
        ctx,
        BackgroundDispatchInputs {
            workflow_snapshot,
            plan_snapshot: Some(PlanDispatchSnapshot {
                content: prepared.plan.content,
                trace: Some(prepared.trace),
            }),
            layers_config: None,
        },
        move |args, permit, cancel| async move {
            let _global_provider_permit = permit;
            // Step 1: Queued → InProgress. `execute` has no per-layer
            // cohorts, so this checkpoint is a no-event manifest save.
            let mut manifest = transition_queued_to_in_progress(&args)?;

            // Step 2: run the provider session under a cancel token.
            // Prompt build / provider start happen INSIDE the spawned future
            // so their latency is absorbed by the background task, not the
            // dispatch-return SLO. Plan content itself is snapshotted by the
            // dispatch helper before admission can drift.
            let run_outcome = run_execute_session(
                &args.plan_content,
                args.plan_path.as_path(),
                args.plan_trace.as_ref(),
                &config_owned,
                args.env.project_root.as_path(),
                args.env.state_dir.as_path(),
                &logs_for_spawn,
                &args.feature_id,
                &args.run_id,
                &cancel,
            )
            .await;

            // Step 3: finalize. Success → Passed; error → Failed with
            // halted_by carrying the error string.
            match run_outcome {
                Ok(()) => {
                    manifest.overall_status = ManifestStatus::Passed;
                }
                Err(e) => {
                    manifest.overall_status = ManifestStatus::Failed;
                    let halted_by = if cancel.is_cancelled() {
                        CancelledReason::InFlight.as_halted_by()
                    } else {
                        format!("runtime_error:{e}")
                    };
                    // Record the error on the manifest's gates-free
                    // layers list as a synthetic `execute` layer so
                    // downstream readers can surface the reason. If
                    // we ever add a top-level `halted_by` field,
                    // prefer that.
                    manifest
                        .layers
                        .push(pice_core::layers::manifest::LayerResult {
                            name: "execute".to_string(),
                            status: pice_core::layers::manifest::LayerStatus::Failed,
                            passes: Vec::new(),
                            seam_checks: Vec::new(),
                            halted_by: Some(halted_by),
                            final_confidence: None,
                            total_cost_usd: None,
                            escalation_events: None,
                        });
                }
            }
            finalize_terminal_manifest(&manifest, &args.manifest_path, &events_for_spawn)?;

            // Emit a terminal log frame so `pice logs --follow`
            // subscribers observe clean end-of-stream. Reason string
            // matches the manifest's overall status wire name.
            let reason = match manifest.overall_status {
                ManifestStatus::Passed => "passed",
                ManifestStatus::Failed => "failed",
                _ => "complete",
            };
            logs_for_spawn
                .append_terminal_frame(&args.feature_id, &args.run_id, reason)
                .await;

            Ok(manifest)
        },
    )
    .await
}

/// Run a single `pice execute` provider session, routing chunks into
/// the daemon's [`LogStore`] so `pice logs --follow <feature>` can
/// replay them. Returns `Ok(())` on success.
#[allow(clippy::too_many_arguments)]
async fn run_execute_session(
    plan_content: &str,
    plan_path: &std::path::Path,
    plan_trace: Option<&PlanTrace>,
    config: &pice_core::config::PiceConfig,
    project_root: &std::path::Path,
    state_dir: &std::path::Path,
    logs: &crate::logs::LogStore,
    feature_id: &str,
    run_id: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    // Honor cancel BEFORE touching the provider — if the dispatcher
    // fired cancel between dispatch and the spawned future running,
    // we never start the provider at all.
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled before provider startup");
    }

    let relative_plan_path = plan_path
        .strip_prefix(project_root)
        .unwrap_or(plan_path)
        .to_string_lossy()
        .to_string();
    let memory = if config
        .memory
        .policy()
        .can_read(pice_core::memory::MemoryConsumer::Execute)
    {
        let paths = MemoryPaths::new(project_root, state_dir);
        let recall = build_memory_recall(
            &paths,
            &config.memory,
            pice_core::memory::MemoryConsumer::Execute,
            Some(feature_id),
            Some(&relative_plan_path),
            &chrono::Utc::now().to_rfc3339(),
        )?;
        record_read_metrics(
            project_root,
            &paths,
            MemoryReadMetrics {
                consumer: pice_core::memory::MemoryConsumer::Execute,
                feature_id: Some(feature_id),
                plan_path: Some(&relative_plan_path),
                run_id: Some(run_id),
                brief: recall.brief.as_ref(),
                warning: recall.warning,
            },
        );
        recall.brief
    } else {
        None
    };
    let prompt = builders::build_execute_prompt(
        plan_content,
        project_root,
        &config.provider.name,
        memory.as_ref(),
    )?;

    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, config).await?;

    // Install a LogStore-backed sink. Chunks fan out to the store
    // keyed on (feature_id, run_id, layer = "execute"). The shared
    // sink bridge is allocation-free on the hot path (a single
    // `Arc::clone` on each chunk).
    let log_sink = Arc::new(crate::logs::LogStoreSink::new(
        logs.clone(),
        feature_id,
        run_id,
        "execute",
    ));
    let shared_sink: Arc<dyn StreamSink> = log_sink.clone();
    orchestrator.on_notification(streaming_handler(shared_sink));

    // Race the session against cancellation.
    let result = tokio::select! {
        r = session::run_session(&mut orchestrator, project_root, prompt) => r,
        _ = cancel.cancelled() => Err(anyhow::anyhow!("cancelled")),
    };
    orchestrator.shutdown().await.ok();
    log_sink.flush().await;
    result?;

    let (title, body) = deterministic_execute_summary("background execute", plan_path);
    let recorder = SessionMemoryRecorder::new(&config.memory);
    let writer = pice_core::memory::MemoryWriter::ExecuteSummary;
    if recorder.preflight_write(writer).is_some() {
        return Ok(());
    }
    let run_ctx = SessionRunContext::with_state_dir(
        project_root,
        state_dir,
        &config.provider.name,
        pice_core::memory::MemoryConsumer::Execute,
        Some(feature_id.to_string()),
        Some(plan_path.to_path_buf()),
        plan_trace,
        run_id.to_string(),
    );
    let write_result = recorder.record_summary(
        &run_ctx,
        writer,
        &title,
        &body,
        vec!["execute".to_string(), "summary".to_string()],
    )?;
    record_write_metrics(project_root, &run_ctx, writer, &write_result);
    if matches!(write_result, MemoryWriteOutcome::Rejected { .. }) {
        tracing::warn!("background execute memory write rejected: {write_result:?}");
    }
    Ok(())
}
