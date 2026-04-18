//! `pice status` handler — show project state and recent evaluations.

use anyhow::Result;
use pice_core::cli::{CommandResponse, StatusRequest};
use pice_core::layers::manifest::VerificationManifest;
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
                        if let Some(layers_json) = load_layer_snapshot(&path, project_root) {
                            info["layers"] = layers_json;
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
                }
            }
        }

        Ok(CommandResponse::Text { content: output })
    }
}

/// Attempt to load the verification manifest for a plan file and extract
/// the per-layer adaptive snapshot used by `pice status`.
///
/// Returns `None` when the manifest does not exist, fails to read, or fails
/// to parse — `pice status` must remain best-effort regardless of manifest
/// state.
fn load_layer_snapshot(
    plan_path: &std::path::Path,
    project_root: &std::path::Path,
) -> Option<Value> {
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
    Some(Value::Array(layers))
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
        let mut manifest = VerificationManifest::new(feature_id, project_root);
        manifest.layers.push(adaptive_layer);
        // Include one legacy (pre-adaptive) layer with no halted_by/confidence.
        manifest.layers.push(LayerResult {
            name: "legacy".to_string(),
            status: LayerStatus::Passed,
            passes: vec![],
            seam_checks: vec![],
            halted_by: None,
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        });
        manifest.overall_status = ManifestStatus::InProgress;
        manifest.save(&path).unwrap();
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

    /// Process-global mutex serializing tests that set `HOME`. The variable
    /// is process-wide, so unsynchronized tests would race on it (and on
    /// `~/.pice/state/` resolution).
    fn home_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn load_layer_snapshot_returns_none_when_manifest_missing() {
        let _g = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let project_root = tmp.path();
        let plan_path = project_root.join(".claude/plans/feature-x.md");
        let got = load_layer_snapshot(&plan_path, project_root);
        assert!(got.is_none());
    }

    #[test]
    fn load_layer_snapshot_surfaces_adaptive_fields_in_json() {
        let _g = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let project_root = tmp.path();
        setup_manifest_at("feature-x", project_root, adaptive_layer_fixture());

        let plan_path = project_root.join(".claude/plans/feature-x.md");
        let got = load_layer_snapshot(&plan_path, project_root).expect("manifest loaded");
        let layers = got.as_array().expect("array");
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
