//! `pice status` handler — show project state and recent evaluations.
//!
//! Phase 7 Task 12 extends the handler to branch on [`StatusMode`]:
//!
//! - [`StatusMode::List`] (default): scans `.claude/plans/` for plan files,
//!   decorates each with the latest evaluation + verification manifest
//!   snapshot. Historical behavior — unchanged so existing tests pass.
//! - [`StatusMode::Detail`]: looks up a single feature's
//!   [`VerificationManifest`] by `feature_id` from the project-namespaced
//!   state directory and returns it verbatim (JSON) or pretty-printed (text).
//! - [`StatusMode::Follow`] / [`StatusMode::Wait`]: rejected here — the CLI
//!   is expected to bypass `cli/dispatch` and call `manifest/subscribe`
//!   directly. If one of these reaches the dispatch handler it is a CLI
//!   routing bug; the handler surfaces it with a structured `Exit` so the
//!   mis-routing is debuggable rather than silent.

use anyhow::Result;
use pice_core::cli::{CommandResponse, ExitJsonStatus, StatusMode, StatusRequest};
use pice_core::layers::manifest::{manifest_project_namespace, VerificationManifest};
use pice_core::plan_parser::ParsedPlan;
use serde_json::{json, Value};

use crate::metrics;
use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;

pub async fn run(
    req: StatusRequest,
    ctx: &DaemonContext,
    _sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    match req.mode {
        StatusMode::Detail => run_detail(req, ctx).await,
        StatusMode::Follow | StatusMode::Wait => Ok(CommandResponse::Exit {
            code: 1,
            message: format!(
                "pice status {:?} must route via manifest/subscribe, not cli/dispatch \
                 (CLI routing bug)",
                req.mode
            ),
        }),
        StatusMode::List => run_list(req, ctx).await,
    }
}

/// `StatusMode::Detail` — return one manifest by feature_id.
///
/// Resolves `state_dir/{project_hash}/{feature_id}.manifest.json`. Missing
/// or unreadable manifests surface as [`ExitJsonStatus::FeatureNotFound`]
/// (JSON mode) or `Exit { code: 1, message }` (text mode) — the Phase 7
/// structured-failure discriminant the CLI tests pin against.
async fn run_detail(req: StatusRequest, ctx: &DaemonContext) -> Result<CommandResponse> {
    let feature_id = match req.feature_id.as_deref() {
        Some(id) if !id.is_empty() => id,
        _ => {
            return Ok(CommandResponse::Exit {
                code: 1,
                message: "pice status <feature_id>: feature_id positional required for detail mode"
                    .to_string(),
            });
        }
    };
    let project_root = ctx.project_root();
    let manifest_path = VerificationManifest::manifest_path_for(feature_id, project_root)?;
    if !manifest_path.exists() {
        if req.json {
            return Ok(CommandResponse::ExitJson {
                code: ExitJsonStatus::FeatureNotFound.exit_code(),
                value: json!({
                    "status": ExitJsonStatus::FeatureNotFound.as_str(),
                    "feature_id": feature_id,
                }),
            });
        }
        return Ok(CommandResponse::Exit {
            code: ExitJsonStatus::FeatureNotFound.exit_code(),
            message: format!("no manifest found for feature_id '{feature_id}'"),
        });
    }
    let manifest = VerificationManifest::load(&manifest_path)?;
    if req.json {
        return Ok(CommandResponse::Json {
            value: serde_json::to_value(&manifest)?,
        });
    }
    Ok(CommandResponse::Text {
        content: render_manifest_detail(&manifest),
    })
}

/// Pretty-print a [`VerificationManifest`] for text-mode `pice status <id>`.
///
/// Matches the summary-table aesthetic of list mode — header with feature
/// id + overall status, one line per layer with key adaptive fields, plus
/// a pending-gates block when applicable.
fn render_manifest_detail(m: &VerificationManifest) -> String {
    use pice_core::layers::manifest::GateStatus;
    let mut out = String::new();
    out.push_str(&format!("Feature: {}\n", m.feature_id));
    if let Some(run_id) = &m.run_id {
        out.push_str(&format!("Run ID: {run_id}\n"));
    }
    let overall = serde_json::to_value(&m.overall_status)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "?".to_string());
    out.push_str(&format!("Overall: {overall}\n"));
    out.push_str(&format!("Layers ({}):\n", m.layers.len()));
    for layer in &m.layers {
        let status = serde_json::to_value(&layer.status)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "?".to_string());
        let passes = layer.passes.len();
        let conf = layer
            .final_confidence
            .map(|c| format!("{c:.3}"))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "  {:<16} {:<14} passes={passes:<2}  conf={conf}\n",
            layer.name, status,
        ));
        if let Some(halted) = &layer.halted_by {
            out.push_str(&format!("    halted_by: {halted}\n"));
        }
    }
    let pending: Vec<_> = m
        .gates
        .iter()
        .filter(|g| g.status == GateStatus::Pending)
        .collect();
    if !pending.is_empty() {
        out.push_str(&format!("\nPending review gates ({}):\n", pending.len()));
        for g in pending {
            out.push_str(&format!(
                "  {} (layer: {}, timeout_at: {})\n",
                g.id, g.layer, g.timeout_at
            ));
        }
        out.push_str("Run `pice review-gate --list` to act.\n");
    }
    out
}

/// `StatusMode::List` — historical plan-scan behavior. Preserves the
/// pre-Phase-7 rendering so existing CLI integration and inline tests pass
/// unchanged.
async fn run_list(req: StatusRequest, ctx: &DaemonContext) -> Result<CommandResponse> {
    // Phase 7 list-mode augmentation: gather ManifestSummary for every
    // manifest under the project's state directory so the list view can
    // surface cross-project state (features with no plan file, live runs
    // from `pice evaluate --background`). Appended under `summaries` in
    // JSON mode; text mode is unchanged.
    let summaries = collect_project_summaries(ctx);

    let project_root = ctx.project_root();

    // Scan .claude/plans/ for plan files
    let plans_dir = project_root.join(".claude/plans");
    let mut plans = Vec::new();

    if plans_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&plans_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }

                let plan_info = match ParsedPlan::load(&path) {
                    Ok(plan) => {
                        let normalized = metrics::normalize_plan_path(&plan.path, project_root);
                        // Look up latest evaluation (non-fatal)
                        let eval = metrics::open_metrics_db(project_root)
                            .ok()
                            .flatten()
                            .and_then(|db| {
                                metrics::store::get_latest_evaluation(&db, &normalized)
                                    .ok()
                                    .flatten()
                            });

                        let mut info = json!({
                            "title": plan.title,
                            "path": normalized,
                            "has_contract": plan.contract.is_some(),
                            "tier": plan.tier(),
                        });

                        if let Some(eval) = eval {
                            info["last_eval"] = json!({
                                "passed": eval.passed,
                                "avg_score": eval.avg_score,
                                "timestamp": eval.timestamp,
                            });
                        }

                        // Phase 4: surface per-layer adaptive fields when a
                        // verification manifest exists for this plan. Best-effort:
                        // a missing or malformed manifest is silently skipped.
                        if let Some(snapshot) = load_manifest_snapshot(&path, project_root) {
                            if let Some(layers) = snapshot.layers {
                                info["layers"] = layers;
                            }
                            // Phase 6: surface pending gates so the
                            // CLI can advise the user to run
                            // `pice review-gate --list`.
                            if !snapshot.gates.is_empty() {
                                info["gates"] = serde_json::Value::Array(snapshot.gates);
                            }
                            if let Some(ms) = snapshot.overall_status {
                                info["overall_status"] = serde_json::Value::String(ms);
                            }
                        }

                        info
                    }
                    Err(e) => {
                        // Malformed plans surface with parse_error (per rust-core.md)
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        json!({
                            "title": name,
                            "path": path.to_string_lossy(),
                            "has_contract": false,
                            "parse_error": e.to_string(),
                        })
                    }
                };
                plans.push(plan_info);
            }
        }
    }

    // Git info (non-fatal)
    let git_info = get_git_info(project_root);

    if req.json {
        Ok(CommandResponse::Json {
            value: json!({
                "plans": plans,
                "git": git_info,
                "summaries": summaries,
            }),
        })
    } else {
        let mut output = String::new();
        output.push_str("PICE Status\n");
        output.push_str("═══════════════════════════════════════\n\n");

        if let Some(branch) = git_info.get("branch").and_then(|b| b.as_str()) {
            output.push_str(&format!("Branch: {branch}\n\n"));
        }

        if plans.is_empty() {
            output.push_str("No plans found.\n");
        } else {
            output.push_str(&format!(
                "{:<30} {:>4}  {:>8}  {:>10}  {:>5}\n",
                "Plan", "Tier", "Contract", "Last Eval", "Score"
            ));
            output.push_str(&format!("{}\n", "─".repeat(70)));

            for plan in &plans {
                let title = plan["title"].as_str().unwrap_or("?");
                let tier = plan.get("tier").and_then(|t| t.as_u64()).unwrap_or(0);
                let contract = if plan["has_contract"].as_bool() == Some(true) {
                    "✓"
                } else {
                    "✗"
                };

                let (eval_str, score_str) = if let Some(eval) = plan.get("last_eval") {
                    let passed = eval["passed"].as_bool().unwrap_or(false);
                    let score = eval["avg_score"].as_f64().unwrap_or(0.0);
                    (
                        if passed { "PASS" } else { "FAIL" }.to_string(),
                        format!("{score:.1}"),
                    )
                } else if plan.get("parse_error").is_some() {
                    ("ERROR".to_string(), "-".to_string())
                } else {
                    ("-".to_string(), "-".to_string())
                };

                // Truncate title to 28 chars
                let display_title = if title.len() > 28 {
                    format!("{}…", &title[..27])
                } else {
                    title.to_string()
                };

                output.push_str(&format!(
                    "{:<30} {:>4}  {:>8}  {:>10}  {:>5}\n",
                    display_title, tier, contract, eval_str, score_str
                ));

                // Phase 4: adaptive per-layer block. Rendered as a compact
                // Unicode-box indented beneath the plan row when any layer
                // has adaptive fields populated.
                if let Some(layers) = plan.get("layers").and_then(|v| v.as_array()) {
                    render_adaptive_layer_block(&mut output, layers);
                    // Phase 6: surface pending-review layers with a
                    // prominent line so reviewers know to run
                    // `pice review-gate`. This complements the
                    // compact adaptive block above.
                    for layer in layers {
                        let status = layer.get("status").and_then(|v| v.as_str()).unwrap_or("");
                        if status == "pending-review" {
                            let name = layer.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            output.push_str(&format!("  ⏸ pending review: {name}\n"));
                        }
                    }
                }
            }
        }

        Ok(CommandResponse::Text { content: output })
    }
}

/// The subset of a verification manifest that `pice status` surfaces.
///
/// Split out from the flat `Value` return that Phase 4 used so Phase 6
/// can carry the pending-gates list + overall-status alongside the
/// per-layer adaptive fields without overloading the JSON shape. A
/// `None` on any field means "nothing to report" and the handler
/// skips emitting it.
struct StatusManifestSnapshot {
    layers: Option<Value>,
    gates: Vec<Value>,
    overall_status: Option<String>,
}

/// Attempt to load the verification manifest for a plan file and extract
/// the status-report snapshot.
///
/// Returns `None` when the manifest does not exist, fails to read, or
/// fails to parse — `pice status` must remain best-effort regardless
/// of manifest state.
fn load_manifest_snapshot(
    plan_path: &std::path::Path,
    project_root: &std::path::Path,
) -> Option<StatusManifestSnapshot> {
    let feature_id = plan_path.file_stem().and_then(|s| s.to_str())?;
    let manifest_path = VerificationManifest::manifest_path_for(feature_id, project_root).ok()?;
    if !manifest_path.exists() {
        return None;
    }
    let manifest = VerificationManifest::load(&manifest_path).ok()?;
    let layers: Vec<Value> = manifest
        .layers
        .iter()
        .map(|layer| {
            let mut layer_json = json!({
                "name": layer.name,
                "status": layer.status,
                "passes_used": layer.passes.len(),
            });
            if let Some(halted_by) = &layer.halted_by {
                layer_json["halted_by"] = json!(halted_by);
            }
            if let Some(conf) = layer.final_confidence {
                // Phase 4.1 Pass-10 Codex MEDIUM #1: defense-in-depth clamp.
                // The compute path (`adaptive_loop.rs`) caps confidence
                // via `cap_confidence()` before writing the manifest, but
                // the report boundary was re-emitting whatever was on
                // disk without re-clamping. A stale, hand-edited, or
                // schema-drifted manifest with `final_confidence > 0.966`
                // would then leak past the ceiling invariant at the
                // `pice status` output — inverting the compute-side
                // guarantee. Clamping here makes the invariant hold at
                // EVERY trust boundary, not just at compute time.
                layer_json["final_confidence"] = json!(pice_core::adaptive::cap_confidence(conf));
            }
            if let Some(cost) = layer.total_cost_usd {
                layer_json["total_cost_usd"] = json!(cost);
            }
            if let Some(events) = &layer.escalation_events {
                layer_json["escalation_events"] = serde_json::to_value(events).unwrap_or(json!([]));
            }
            layer_json
        })
        .collect();

    // Phase 6: surface pending gates so the dashboard + JSON consumers
    // can enumerate them without a separate review-gate/list RPC.
    // Filter to Pending status — decided gates are historical and live
    // in the `gate_decisions` audit table, not the live manifest.
    let gates: Vec<Value> = manifest
        .gates
        .iter()
        .filter(|g| g.status == pice_core::layers::manifest::GateStatus::Pending)
        .map(|g| {
            json!({
                "id": g.id,
                "layer": g.layer,
                "trigger_expression": g.trigger_expression,
                "timeout_at": g.timeout_at,
            })
        })
        .collect();

    // Serialize ManifestStatus via serde so the kebab-case wire form
    // is used — callers check for `"pending-review"` against this
    // exact string.
    let overall_status = serde_json::to_value(&manifest.overall_status)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()));

    Some(StatusManifestSnapshot {
        layers: Some(Value::Array(layers)),
        gates,
        overall_status,
    })
}

/// Render a per-layer adaptive block beneath a plan row in text mode.
///
/// Only prints layers that have at least one adaptive field populated —
/// legacy manifests from Phase 3 (or earlier) produce an empty block.
fn render_adaptive_layer_block(output: &mut String, layers: &[Value]) {
    let has_adaptive = layers.iter().any(|l| {
        l.get("halted_by").is_some()
            || l.get("final_confidence").is_some()
            || l.get("total_cost_usd").is_some()
            || l.get("passes_used").and_then(|v| v.as_u64()).unwrap_or(0) > 0
    });
    if !has_adaptive {
        return;
    }

    output.push_str("  \u{2554}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2557}\n");
    output.push_str("  \u{2551} Adaptive (per-layer)                \u{2551}\n");
    output.push_str("  \u{2560}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2563}\n");

    for layer in layers {
        let name = layer.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let passes = layer
            .get("passes_used")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let halted_by = layer
            .get("halted_by")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let conf = layer
            .get("final_confidence")
            .and_then(|v| v.as_f64())
            .map(|c| format!("{:.3}", c))
            .unwrap_or_else(|| "-".to_string());

        let display_name = truncate(name, 12);
        let display_halted = truncate(halted_by, 14);
        output.push_str(&format!(
            "  \u{2551} {name:<12} p={passes:<2} {halted:<14} c={conf:<6} \u{2551}\n",
            name = display_name,
            passes = passes,
            halted = display_halted,
            conf = conf,
        ));
    }
    output.push_str("  \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}\n");
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}\u{2026}")
    }
}

/// Scan `state_dir/{project_hash}/*.manifest.json` and build a
/// [`ManifestSummary`] for every manifest. Enriches each with its live
/// `run_id` from the daemon's [`FeatureJobManager`] when one exists.
///
/// Best-effort: a single unreadable manifest is skipped with a `warn!`;
/// a missing state dir returns an empty vec. The list view must always
/// render, even when the state tree is empty or partially corrupt.
fn collect_project_summaries(ctx: &DaemonContext) -> Vec<serde_json::Value> {
    use pice_core::protocol::subscribe::ManifestSummary;
    let mut out = Vec::new();
    let Ok(state_root) = VerificationManifest::state_dir() else {
        return out;
    };
    let namespace = manifest_project_namespace(ctx.project_root());
    let project_dir = state_root.join(&namespace);
    let Ok(entries) = std::fs::read_dir(&project_dir) else {
        return out;
    };
    let live_runs = ctx.jobs().live_runs();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(manifest) = VerificationManifest::load(&path) else {
            tracing::warn!(path = %path.display(), "skipping unreadable manifest");
            continue;
        };
        let run_id = live_runs.get(&manifest.feature_id).cloned();
        let summary = ManifestSummary::from_manifest(&manifest, run_id);
        if let Ok(v) = serde_json::to_value(&summary) {
            out.push(v);
        }
    }
    // Stable ordering so list output bytes are deterministic across runs.
    out.sort_by(|a, b| {
        a.get("feature_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("feature_id").and_then(|v| v.as_str()).unwrap_or(""))
    });
    out
}

fn get_git_info(project_root: &std::path::Path) -> serde_json::Value {
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let status = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            let lines: Vec<&str> = text.lines().collect();
            let staged = lines
                .iter()
                .filter(|l| l.starts_with('M') || l.starts_with('A') || l.starts_with('D'))
                .count();
            let unstaged = lines
                .iter()
                .filter(|l| {
                    l.chars().nth(1).map(|c| c != ' ').unwrap_or(false) && !l.starts_with('?')
                })
                .count();
            let untracked = lines.iter().filter(|l| l.starts_with("??")).count();
            json!({"staged": staged, "unstaged": unstaged, "untracked": untracked})
        })
        .unwrap_or_else(|| json!({}));

    let mut git = json!({});
    if let Some(b) = branch {
        git["branch"] = json!(b);
    }
    git["status"] = status;
    git
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::adaptive::EscalationEvent;
    use pice_core::layers::manifest::{
        LayerResult, LayerStatus, ManifestStatus, PassResult, VerificationManifest,
    };
    use tempfile::TempDir;

    /// Construct a manifest with two layers — one adaptive, one legacy — and
    /// save it to `manifest_path_for(feature_id, project_root)`. Uses the
    /// `HOME=<tmp>` override so the manifest lands under the temp directory.
    fn setup_manifest_at(
        feature_id: &str,
        project_root: &std::path::Path,
        adaptive_layer: LayerResult,
    ) {
        let path = VerificationManifest::manifest_path_for(feature_id, project_root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut m = VerificationManifest::new(feature_id, project_root);
        m.layers.push(adaptive_layer);
        // Include one legacy (pre-adaptive) layer with no halted_by/confidence.
        m.layers.push(LayerResult {
            name: "legacy".to_string(),
            status: LayerStatus::Passed,
            passes: vec![],
            seam_checks: vec![],
            halted_by: None,
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        });
        m.overall_status = ManifestStatus::InProgress;
        // Test fixture: route through `NullSaver` so this file contains
        // zero raw low-level save call sites (Task 9 grep-coverage
        // invariant enforced by `manifest_saver_trait_coverage.rs`).
        let saver = crate::events::NullSaver;
        crate::events::ManifestSaver::save_and_emit(
            &saver,
            &m,
            &path,
            crate::events::SaveIntent::FeatureCompleted,
        )
        .unwrap();
    }

    fn adaptive_layer_fixture() -> LayerResult {
        LayerResult {
            name: "backend".to_string(),
            status: LayerStatus::Passed,
            passes: vec![
                PassResult {
                    index: 1,
                    model: "stub-echo".to_string(),
                    score: Some(9.5),
                    cost_usd: Some(0.01),
                    timestamp: "2026-04-16T00:00:00Z".to_string(),
                    findings: vec![],
                },
                PassResult {
                    index: 2,
                    model: "stub-echo".to_string(),
                    score: Some(9.5),
                    cost_usd: Some(0.01),
                    timestamp: "2026-04-16T00:00:01Z".to_string(),
                    findings: vec![],
                },
            ],
            seam_checks: vec![],
            halted_by: Some("sprt_confidence_reached".to_string()),
            final_confidence: Some(0.91),
            total_cost_usd: Some(0.02),
            escalation_events: Some(vec![EscalationEvent::Level1FreshContext { at_pass: 1 }]),
        }
    }

    // Phase 7 remediation: `home_lock()` removed. Tests migrated to
    // `crate::test_support::StateDirGuard`, which serializes on the
    // workspace-wide `state_dir_lock()` + drives `PICE_STATE_DIR`
    // directly (the same env var `VerificationManifest::state_dir()`
    // reads first). The old `HOME`-based pattern only serialized among
    // tests in this module and raced against other modules that set
    // `PICE_STATE_DIR` concurrently — the documented flake.

    #[test]
    fn load_layer_snapshot_returns_none_when_manifest_missing() {
        // Phase 7 remediation: use `StateDirGuard` from `test_support`
        // so this test serializes on the SAME `state_dir_lock` as every
        // other `PICE_STATE_DIR` consumer in the workspace. The prior
        // pattern (`home_lock` + `HOME` mutation) only serialized among
        // tests in THIS module — a concurrent `review_gate::tests` case
        // setting `PICE_STATE_DIR` would silently redirect
        // `VerificationManifest::state_dir()` away from the tempdir
        // this test seeds, causing spurious failures under parallel
        // `cargo test --workspace` runs.
        let tmp = TempDir::new().unwrap();
        let _g = crate::test_support::StateDirGuard::new(tmp.path());
        let project_root = tmp.path();
        let plan_path = project_root.join(".claude/plans/feature-x.md");
        let got = load_manifest_snapshot(&plan_path, project_root);
        assert!(got.is_none());
    }

    #[test]
    fn load_layer_snapshot_surfaces_adaptive_fields_in_json() {
        // Phase 7 remediation: use `StateDirGuard` from `test_support`
        // so this test serializes on the SAME `state_dir_lock` as every
        // other `PICE_STATE_DIR` consumer in the workspace. The prior
        // pattern (`home_lock` + `HOME` mutation) only serialized among
        // tests in THIS module — a concurrent `review_gate::tests` case
        // setting `PICE_STATE_DIR` would silently redirect
        // `VerificationManifest::state_dir()` away from the tempdir
        // this test seeds, causing spurious failures under parallel
        // `cargo test --workspace` runs.
        let tmp = TempDir::new().unwrap();
        let _g = crate::test_support::StateDirGuard::new(tmp.path());
        let project_root = tmp.path();
        setup_manifest_at("feature-x", project_root, adaptive_layer_fixture());

        let plan_path = project_root.join(".claude/plans/feature-x.md");
        let snapshot = load_manifest_snapshot(&plan_path, project_root).expect("manifest loaded");
        let layers = snapshot
            .layers
            .as_ref()
            .expect("layers")
            .as_array()
            .expect("array");
        assert_eq!(layers.len(), 2);

        // Adaptive layer carries all adaptive fields.
        let backend = &layers[0];
        assert_eq!(backend["name"], "backend");
        assert_eq!(backend["passes_used"], 2);
        assert_eq!(backend["halted_by"], "sprt_confidence_reached");
        assert_eq!(backend["final_confidence"].as_f64().unwrap(), 0.91);
        assert_eq!(backend["total_cost_usd"].as_f64().unwrap(), 0.02);
        assert!(backend["escalation_events"].is_array());

        // Legacy layer omits adaptive fields entirely (forward-compat: Phase 3
        // manifests must surface without spurious nulls).
        let legacy = &layers[1];
        assert_eq!(legacy["name"], "legacy");
        assert_eq!(legacy["passes_used"], 0);
        assert!(legacy.get("halted_by").is_none());
        assert!(legacy.get("final_confidence").is_none());
        assert!(legacy.get("total_cost_usd").is_none());
        assert!(legacy.get("escalation_events").is_none());

        // Phase 6: gates list should be empty for this fixture (no
        // PendingReview gates), and overall status passed through.
        assert!(snapshot.gates.is_empty());
        assert_eq!(snapshot.overall_status.as_deref(), Some("in-progress"));
    }

    /// Phase 6 Task 13: pending gates in the manifest surface under
    /// `snapshot.gates`. The status handler JSON output maps this to a
    /// top-level `gates: [{id, layer, trigger_expression, timeout_at}]`
    /// field.
    #[test]
    fn load_layer_snapshot_surfaces_pending_gates() {
        use pice_core::layers::manifest::{GateEntry, GateStatus};
        use pice_core::workflow::schema::OnTimeout;

        // Phase 7 remediation: use `StateDirGuard` from `test_support`
        // so this test serializes on the SAME `state_dir_lock` as every
        // other `PICE_STATE_DIR` consumer in the workspace. The prior
        // pattern (`home_lock` + `HOME` mutation) only serialized among
        // tests in THIS module — a concurrent `review_gate::tests` case
        // setting `PICE_STATE_DIR` would silently redirect
        // `VerificationManifest::state_dir()` away from the tempdir
        // this test seeds, causing spurious failures under parallel
        // `cargo test --workspace` runs.
        let tmp = TempDir::new().unwrap();
        let _g = crate::test_support::StateDirGuard::new(tmp.path());
        let project_root = tmp.path();

        // Build manifest with a PendingReview layer + its gate.
        let path = VerificationManifest::manifest_path_for("feature-gated", project_root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut m = VerificationManifest::new("feature-gated", project_root);
        m.layers.push(LayerResult {
            name: "infrastructure".to_string(),
            status: LayerStatus::PendingReview,
            passes: vec![],
            seam_checks: vec![],
            halted_by: None,
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        });
        m.gates.push(GateEntry {
            id: "feature-gated:infrastructure:01".to_string(),
            layer: "infrastructure".to_string(),
            status: GateStatus::Pending,
            trigger_expression: "layer == infrastructure".to_string(),
            requested_at: "2026-04-20T09:00:00Z".to_string(),
            timeout_at: "2026-04-21T09:00:00Z".to_string(),
            on_timeout_action: OnTimeout::Reject,
            reject_attempts_remaining: 1,
            decision: None,
            decided_at: None,
        });
        m.compute_overall_status();
        // Test fixture: route through `NullSaver` so this file contains
        // zero raw low-level save call sites (Task 9 grep-coverage
        // invariant).
        let saver = crate::events::NullSaver;
        crate::events::ManifestSaver::save_and_emit(
            &saver,
            &m,
            &path,
            crate::events::SaveIntent::FeatureCompleted,
        )
        .unwrap();

        let plan_path = project_root.join(".claude/plans/feature-gated.md");
        let snapshot = load_manifest_snapshot(&plan_path, project_root).expect("manifest loaded");
        assert_eq!(snapshot.gates.len(), 1);
        assert_eq!(snapshot.gates[0]["id"], "feature-gated:infrastructure:01");
        assert_eq!(snapshot.gates[0]["layer"], "infrastructure");
        assert_eq!(snapshot.overall_status.as_deref(), Some("pending-review"));

        // Decided gates (non-Pending) must NOT surface — they live in
        // the audit trail, not the live-blocking list.
        let mut m2 = m.clone();
        m2.gates[0].status = GateStatus::Approved;
        m2.gates[0].decision = Some("approve".to_string());
        crate::events::ManifestSaver::save_and_emit(
            &saver,
            &m2,
            &path,
            crate::events::SaveIntent::FeatureCompleted,
        )
        .unwrap();
        let snapshot2 = load_manifest_snapshot(&plan_path, project_root).expect("reload");
        assert!(snapshot2.gates.is_empty());
    }

    #[test]
    fn render_manifest_detail_surfaces_run_id_in_text_mode() {
        let mut manifest =
            VerificationManifest::new("feature-with-run-id", std::path::Path::new("/tmp/project"));
        manifest.run_id = Some("r-public-status".to_string());

        let rendered = render_manifest_detail(&manifest);

        assert!(rendered.contains("Feature: feature-with-run-id"));
        assert!(
            rendered.contains("Run ID: r-public-status"),
            "text-mode pice status detail must surface run_id: {rendered}"
        );
    }

    #[test]
    fn render_adaptive_layer_block_renders_passes_halted_by_and_confidence() {
        let layers = vec![json!({
            "name": "backend",
            "status": "passed",
            "passes_used": 3,
            "halted_by": "sprt_confidence_reached",
            "final_confidence": 0.912,
            "total_cost_usd": 0.03,
        })];

        let mut out = String::new();
        render_adaptive_layer_block(&mut out, &layers);

        // Header and layer row both present.
        assert!(
            out.contains("Adaptive (per-layer)"),
            "missing box header: {out}"
        );
        assert!(out.contains("backend"), "missing layer name: {out}");
        assert!(out.contains("p=3"), "missing pass count: {out}");
        // "sprt_confidence_reached" gets truncated to fit the 14-char column.
        assert!(
            out.contains("sprt_confiden"),
            "missing halted_by prefix: {out}"
        );
        assert!(out.contains("c=0.912"), "missing confidence: {out}");
    }

    /// Phase 7 Task 12: `StatusMode::Detail` returns the requested manifest
    /// verbatim as JSON.
    #[tokio::test]
    // `home_lock` serializes `std::env::set_var("HOME", ...)` across the
    // single-threaded test consumers below. The guard is held across
    // `.await` only because the test body does no cross-task work —
    // `run(...)` awaits synchronous IO on the current task. Using a
    // `tokio::sync::Mutex` would defeat the test's atomic-env guarantee
    // (per `.claude/rules/rust-core.md` "Holding MutexGuard across .await").
    #[allow(clippy::await_holding_lock)]
    async fn detail_mode_returns_manifest_when_feature_exists() {
        // Phase 7 remediation: use `StateDirGuard` from `test_support`
        // so this test serializes on the SAME `state_dir_lock` as every
        // other `PICE_STATE_DIR` consumer in the workspace. The prior
        // pattern (`home_lock` + `HOME` mutation) only serialized among
        // tests in THIS module — a concurrent `review_gate::tests` case
        // setting `PICE_STATE_DIR` would silently redirect
        // `VerificationManifest::state_dir()` away from the tempdir
        // this test seeds, causing spurious failures under parallel
        // `cargo test --workspace` runs.
        let tmp = TempDir::new().unwrap();
        let _g = crate::test_support::StateDirGuard::new(tmp.path());
        let project_root = tmp.path();

        setup_manifest_at("feature-detail", project_root, adaptive_layer_fixture());

        let ctx = DaemonContext::new_for_test_with_root("test-token", project_root.to_path_buf());
        let req = StatusRequest {
            json: true,
            mode: StatusMode::Detail,
            feature_id: Some("feature-detail".to_string()),
            stream_json: false,
            timeout_secs: None,
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::Json { value } => {
                assert_eq!(value["feature_id"], "feature-detail");
                // The adaptive_layer_fixture adds one layer; setup_manifest_at
                // adds another legacy layer.
                let layers = value["layers"].as_array().expect("layers array");
                assert_eq!(layers.len(), 2);
            }
            other => panic!("expected Json, got: {other:?}"),
        }
    }

    /// Phase 7 Task 12: `StatusMode::Detail` with a missing feature_id
    /// returns `ExitJsonStatus::FeatureNotFound`.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn detail_mode_json_surfaces_feature_not_found() {
        // Phase 7 remediation: use `StateDirGuard` from `test_support`
        // so this test serializes on the SAME `state_dir_lock` as every
        // other `PICE_STATE_DIR` consumer in the workspace. The prior
        // pattern (`home_lock` + `HOME` mutation) only serialized among
        // tests in THIS module — a concurrent `review_gate::tests` case
        // setting `PICE_STATE_DIR` would silently redirect
        // `VerificationManifest::state_dir()` away from the tempdir
        // this test seeds, causing spurious failures under parallel
        // `cargo test --workspace` runs.
        let tmp = TempDir::new().unwrap();
        let _g = crate::test_support::StateDirGuard::new(tmp.path());
        let project_root = tmp.path();

        let ctx = DaemonContext::new_for_test_with_root("test-token", project_root.to_path_buf());
        let req = StatusRequest {
            json: true,
            mode: StatusMode::Detail,
            feature_id: Some("does-not-exist".to_string()),
            stream_json: false,
            timeout_secs: None,
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::ExitJson { code, value } => {
                assert_eq!(code, ExitJsonStatus::FeatureNotFound.exit_code());
                assert_eq!(value["status"], ExitJsonStatus::FeatureNotFound.as_str());
                assert_eq!(value["feature_id"], "does-not-exist");
            }
            other => panic!("expected ExitJson, got: {other:?}"),
        }
    }

    /// Phase 7 Task 12: `StatusMode::Follow` / `Wait` at cli/dispatch is a
    /// CLI routing bug — the daemon surfaces it with a structured `Exit`.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn follow_and_wait_modes_rejected_at_dispatch() {
        // Phase 7 remediation: use `StateDirGuard` from `test_support`
        // so this test serializes on the SAME `state_dir_lock` as every
        // other `PICE_STATE_DIR` consumer in the workspace. The prior
        // pattern (`home_lock` + `HOME` mutation) only serialized among
        // tests in THIS module — a concurrent `review_gate::tests` case
        // setting `PICE_STATE_DIR` would silently redirect
        // `VerificationManifest::state_dir()` away from the tempdir
        // this test seeds, causing spurious failures under parallel
        // `cargo test --workspace` runs.
        let tmp = TempDir::new().unwrap();
        let _g = crate::test_support::StateDirGuard::new(tmp.path());
        let project_root = tmp.path();
        let ctx = DaemonContext::new_for_test_with_root("test-token", project_root.to_path_buf());

        for mode in [StatusMode::Follow, StatusMode::Wait] {
            let req = StatusRequest {
                json: false,
                mode,
                feature_id: Some("f".to_string()),
                stream_json: false,
                timeout_secs: None,
            };
            let resp = run(req, &ctx, &crate::orchestrator::NullSink)
                .await
                .expect("run");
            match resp {
                CommandResponse::Exit { code, message } => {
                    assert_eq!(code, 1);
                    assert!(
                        message.contains("manifest/subscribe"),
                        "expected routing-bug message for {mode:?}, got: {message}"
                    );
                }
                other => panic!("expected Exit, got: {other:?}"),
            }
        }
    }

    /// Phase 7 Task 12: List-mode JSON output now includes `summaries` —
    /// one [`ManifestSummary`] per project-namespaced manifest.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn list_mode_json_includes_manifest_summaries() {
        // Phase 7 remediation: use `StateDirGuard` from `test_support`
        // so this test serializes on the SAME `state_dir_lock` as every
        // other `PICE_STATE_DIR` consumer in the workspace. The prior
        // pattern (`home_lock` + `HOME` mutation) only serialized among
        // tests in THIS module — a concurrent `review_gate::tests` case
        // setting `PICE_STATE_DIR` would silently redirect
        // `VerificationManifest::state_dir()` away from the tempdir
        // this test seeds, causing spurious failures under parallel
        // `cargo test --workspace` runs.
        let tmp = TempDir::new().unwrap();
        let _g = crate::test_support::StateDirGuard::new(tmp.path());
        let project_root = tmp.path();
        setup_manifest_at("feature-a", project_root, adaptive_layer_fixture());

        let ctx = DaemonContext::new_for_test_with_root("test-token", project_root.to_path_buf());
        let req = StatusRequest {
            json: true,
            mode: StatusMode::List,
            feature_id: None,
            stream_json: false,
            timeout_secs: None,
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::Json { value } => {
                let summaries = value["summaries"].as_array().expect("summaries array");
                assert!(
                    summaries.iter().any(|s| s["feature_id"] == "feature-a"),
                    "feature-a should be in summaries; got {summaries:?}"
                );
            }
            other => panic!("expected Json, got: {other:?}"),
        }
    }

    #[test]
    fn render_adaptive_layer_block_skips_legacy_only_layers() {
        // All layers are legacy (no adaptive fields) — block should be empty.
        let layers = vec![json!({
            "name": "legacy",
            "status": "passed",
            "passes_used": 0,
        })];

        let mut out = String::new();
        render_adaptive_layer_block(&mut out, &layers);
        assert!(
            out.is_empty(),
            "expected empty render for legacy-only layers; got: {out}"
        );
    }
}
