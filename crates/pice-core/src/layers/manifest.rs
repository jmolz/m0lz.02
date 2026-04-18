//! Verification manifest schema and helpers.
//!
//! The verification manifest is the source of truth for per-layer evaluation
//! state. The daemon writes it; all adapters (CLI, dashboard, CI) read it.
//!
//! Location: `~/.pice/state/{feature-id}.manifest.json`
//!
//! Writes are atomic: write to `.tmp` then `std::fs::rename()`.

use crate::adaptive::EscalationEvent;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Current schema version. New manifests always use this value; `load()`
/// rejects anything that doesn't match.
const SCHEMA_VERSION: &str = "0.2";

// ─── Core types ─────────────────────────────────────────────────────────────

/// Top-level verification manifest stored at
/// `~/.pice/state/{feature-id}.manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationManifest {
    pub schema_version: String,
    pub feature_id: String,
    pub project_root_hash: String,
    pub layers: Vec<LayerResult>,
    pub gates: Vec<GateEntry>,
    pub overall_status: ManifestStatus,
}

/// Per-layer evaluation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerResult {
    pub name: String,
    pub status: LayerStatus,
    pub passes: Vec<PassResult>,
    pub seam_checks: Vec<SeamCheckResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halted_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    /// ADTS escalation audit trail. `None` when the adaptive loop did not run
    /// or ran a non-ADTS algorithm; `Some(vec)` records the level transitions
    /// in occurrence order. Required by Phase 4 contract criterion #9
    /// (determinism) and contract criterion for the audit trail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_events: Option<Vec<EscalationEvent>>,
}

/// Result of a single evaluation pass within a layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassResult {
    pub index: u32,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub timestamp: String,
    pub findings: Vec<String>,
}

/// Result of a seam check between layer boundaries.
///
/// `boundary` is the canonical `"A↔B"` key of the boundary the check ran
/// against. `category` is the PRDv2 failure category (1..=12) declared by
/// the check; it is `None` for plugin checks that do not self-identify with
/// the default taxonomy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeamCheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub boundary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Approval gate entry. Phase 6 fills these — parsed but unused in Phase 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateEntry {
    pub layer: String,
    pub status: GateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

// ─── Status enums ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestStatus {
    Pending,
    InProgress,
    Passed,
    Failed,
    FailedInterrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LayerStatus {
    Pending,
    InProgress,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    Passed,
    Warning,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GateStatus {
    Pending,
    Approved,
    Rejected,
    Skipped,
    TimedOut,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// SHA-256 hash of the project root path, hex-encoded. Used to namespace
/// manifests so that identically named features in different projects don't
/// collide.
fn hash_project_root(project_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_root.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Phase 4.1 Pass-6: the 12-character project namespace used in manifest
/// paths under `~/.pice/state/{namespace}/`. Exposed so the daemon's
/// per-manifest lock map (`DaemonContext::manifest_lock_for`) can key on
/// the SAME namespace the manifest IO writes to — otherwise the lock and
/// the writer would disagree on identity and the race window would reopen.
pub fn manifest_project_namespace(project_root: &Path) -> String {
    let full = hash_project_root(project_root);
    full[..12.min(full.len())].to_string()
}

/// Return the user's home directory via environment variables.
/// Uses `HOME` on Unix, `USERPROFILE` on Windows, with cross-fallback.
fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .context("could not determine home directory (neither HOME nor USERPROFILE is set)")
}

// ─── Implementation ─────────────────────────────────────────────────────────

impl VerificationManifest {
    /// Create a new manifest with all layers pending.
    pub fn new(feature_id: &str, project_root: &Path) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            feature_id: feature_id.to_string(),
            project_root_hash: hash_project_root(project_root),
            layers: Vec::new(),
            gates: Vec::new(),
            overall_status: ManifestStatus::Pending,
        }
    }

    /// Load a manifest from a JSON file. Rejects `schema_version` != `"0.2"`.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read manifest from {}", path.display()))?;
        let mut manifest: Self = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse manifest from {}", path.display()))?;
        if manifest.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported manifest schema version '{}' (expected '{}')",
                manifest.schema_version,
                SCHEMA_VERSION,
            );
        }
        // Phase 4.1 Pass-10 Codex MEDIUM #1: defense-in-depth ceiling clamp
        // at the load boundary. The compute path caps `final_confidence`
        // via `cap_confidence()` before writing, but a stale, hand-edited,
        // or foreign-written manifest can still carry a value above the
        // correlated-Condorcet ceiling (0.966). Clamping on ingest makes
        // EVERY downstream consumer (status handler, dashboard, CI
        // adapter) observe the invariant without having to remember to
        // clamp at their own report boundary. A warning surfaces the
        // discrepancy rather than silently swallowing it.
        for layer in &mut manifest.layers {
            if let Some(conf) = layer.final_confidence {
                if conf > crate::adaptive::CONFIDENCE_CEILING {
                    tracing::warn!(
                        layer = %layer.name,
                        found = conf,
                        ceiling = crate::adaptive::CONFIDENCE_CEILING,
                        path = %path.display(),
                        "manifest layer.final_confidence exceeds ceiling; clamping on load",
                    );
                    layer.final_confidence = Some(crate::adaptive::cap_confidence(conf));
                }
            }
        }
        Ok(manifest)
    }

    /// Crash-safe atomic write: serialize to `{path}.tmp`, fsync, rename, fsync dir.
    ///
    /// The fsync on the temp file ensures data reaches disk before the rename.
    /// The fsync on the parent directory ensures the rename (directory entry
    /// update) is durable. This guarantees that after `save()` returns, the
    /// manifest survives a power loss or kernel panic.
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::fs::{File, OpenOptions};
        use std::io::Write;

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create manifest directory {}", parent.display())
            })?;
        }

        let tmp_path = path.with_extension("json.tmp");
        let json =
            serde_json::to_string_pretty(self).context("failed to serialize manifest to JSON")?;

        // Write + fsync the temp file
        {
            let mut file = File::create(&tmp_path).with_context(|| {
                format!(
                    "failed to create temporary manifest at {}",
                    tmp_path.display()
                )
            })?;
            file.write_all(json.as_bytes()).with_context(|| {
                format!(
                    "failed to write temporary manifest at {}",
                    tmp_path.display()
                )
            })?;
            file.sync_all().with_context(|| {
                format!(
                    "failed to fsync temporary manifest at {}",
                    tmp_path.display()
                )
            })?;
        }

        // Atomic rename
        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "failed to atomically rename {} → {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        // Fsync parent directory to make the rename durable
        if let Some(parent) = path.parent() {
            if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
                let _ = dir.sync_all(); // Best-effort — some filesystems don't support dir fsync
            }
        }

        Ok(())
    }

    /// Returns `~/.pice/state/`.
    pub fn state_dir() -> Result<PathBuf> {
        let home = home_dir()?;
        Ok(home.join(".pice").join("state"))
    }

    /// Returns `~/.pice/state/{project_hash}/{feature_id}.manifest.json`.
    ///
    /// Namespaced by project hash so that unrelated repos using the same
    /// plan name (e.g. `plan.md`) don't collide on disk.
    pub fn manifest_path_for(feature_id: &str, project_root: &Path) -> Result<PathBuf> {
        let dir = Self::state_dir()?;
        let hash = hash_project_root(project_root);
        // Use first 12 hex chars for a readable-but-unique subdirectory
        let namespace = &hash[..12.min(hash.len())];
        Ok(dir
            .join(namespace)
            .join(format!("{feature_id}.manifest.json")))
    }

    /// Returns `~/.pice/state/{feature_id}.manifest.json` (legacy, no project namespace).
    ///
    /// Prefer `manifest_path_for()` which namespaces by project hash.
    pub fn manifest_path(feature_id: &str) -> Result<PathBuf> {
        let dir = Self::state_dir()?;
        Ok(dir.join(format!("{feature_id}.manifest.json")))
    }

    /// Append a layer result.
    pub fn add_layer_result(&mut self, result: LayerResult) {
        self.layers.push(result);
    }

    /// Compute `overall_status` from `layers`:
    ///
    /// - If any layer is `Failed` → `Failed`
    /// - If every layer is `Passed` (or `Skipped`) → `Passed`
    /// - Otherwise (some `Pending`/`InProgress`) → `InProgress`
    ///
    /// An empty `layers` vec is treated as `Pending`.
    pub fn compute_overall_status(&mut self) {
        if self.layers.is_empty() {
            self.overall_status = ManifestStatus::Pending;
            return;
        }

        let any_failed = self.layers.iter().any(|l| l.status == LayerStatus::Failed);
        if any_failed {
            self.overall_status = ManifestStatus::Failed;
            return;
        }

        let all_terminal = self
            .layers
            .iter()
            .all(|l| l.status == LayerStatus::Passed || l.status == LayerStatus::Skipped);
        if all_terminal {
            self.overall_status = ManifestStatus::Passed;
            return;
        }

        self.overall_status = ManifestStatus::InProgress;
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a `LayerResult` with the given name and status.
    fn layer(name: &str, status: LayerStatus) -> LayerResult {
        LayerResult {
            name: name.to_string(),
            status,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: None,
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.manifest.json");

        let mut manifest = VerificationManifest::new("feat-123", Path::new("/tmp/project"));
        manifest.add_layer_result(LayerResult {
            name: "backend".to_string(),
            status: LayerStatus::Passed,
            passes: vec![PassResult {
                index: 0,
                model: "claude-sonnet-4-20250514".to_string(),
                score: Some(0.92),
                cost_usd: Some(0.003),
                timestamp: "2026-04-13T10:00:00Z".to_string(),
                findings: vec!["All criteria met".to_string()],
            }],
            seam_checks: vec![SeamCheckResult {
                name: "schema_match".to_string(),
                status: CheckStatus::Passed,
                boundary: "backend↔database".to_string(),
                category: Some(9),
                details: None,
            }],
            halted_by: None,
            final_confidence: Some(0.95),
            total_cost_usd: Some(0.003),
            escalation_events: None,
        });
        manifest.gates.push(GateEntry {
            layer: "backend".to_string(),
            status: GateStatus::Approved,
            triggered_by: Some("cost_threshold".to_string()),
            timeout_at: None,
            decision: Some("auto-approved".to_string()),
        });

        manifest.save(&path).unwrap();
        let loaded = VerificationManifest::load(&path).unwrap();

        assert_eq!(loaded.schema_version, "0.2");
        assert_eq!(loaded.feature_id, "feat-123");
        assert_eq!(loaded.project_root_hash, manifest.project_root_hash);
        assert_eq!(loaded.layers.len(), 1);
        assert_eq!(loaded.layers[0].name, "backend");
        assert_eq!(loaded.layers[0].status, LayerStatus::Passed);
        assert_eq!(loaded.layers[0].passes.len(), 1);
        assert_eq!(loaded.layers[0].passes[0].index, 0);
        assert_eq!(loaded.layers[0].passes[0].score, Some(0.92));
        assert_eq!(loaded.layers[0].passes[0].findings.len(), 1);
        assert_eq!(loaded.layers[0].seam_checks.len(), 1);
        assert_eq!(loaded.layers[0].seam_checks[0].status, CheckStatus::Passed);
        assert_eq!(loaded.layers[0].seam_checks[0].boundary, "backend↔database");
        assert_eq!(loaded.layers[0].seam_checks[0].category, Some(9));
        assert_eq!(loaded.layers[0].final_confidence, Some(0.95));
        assert_eq!(loaded.gates.len(), 1);
        assert_eq!(loaded.gates[0].status, GateStatus::Approved);
        assert_eq!(loaded.overall_status, ManifestStatus::Pending);
    }

    #[test]
    fn overall_status_all_pass() {
        let mut manifest = VerificationManifest::new("feat-pass", Path::new("/project"));
        manifest.add_layer_result(layer("backend", LayerStatus::Passed));
        manifest.add_layer_result(layer("database", LayerStatus::Passed));
        manifest.add_layer_result(layer("api", LayerStatus::Passed));

        manifest.compute_overall_status();
        assert_eq!(manifest.overall_status, ManifestStatus::Passed);
    }

    #[test]
    fn overall_status_one_fail() {
        let mut manifest = VerificationManifest::new("feat-fail", Path::new("/project"));
        manifest.add_layer_result(layer("backend", LayerStatus::Passed));
        manifest.add_layer_result(layer("database", LayerStatus::Failed));
        manifest.add_layer_result(layer("api", LayerStatus::Passed));

        manifest.compute_overall_status();
        assert_eq!(manifest.overall_status, ManifestStatus::Failed);
    }

    #[test]
    fn overall_status_in_progress() {
        let mut manifest = VerificationManifest::new("feat-wip", Path::new("/project"));
        manifest.add_layer_result(layer("backend", LayerStatus::Pending));
        manifest.add_layer_result(layer("database", LayerStatus::InProgress));
        manifest.add_layer_result(layer("api", LayerStatus::Passed));

        manifest.compute_overall_status();
        assert_eq!(manifest.overall_status, ManifestStatus::InProgress);
    }

    #[test]
    fn overall_status_empty_is_pending() {
        let mut manifest = VerificationManifest::new("feat-empty", Path::new("/project"));
        manifest.compute_overall_status();
        assert_eq!(manifest.overall_status, ManifestStatus::Pending);
    }

    #[test]
    fn overall_status_skipped_layers_count_as_pass() {
        let mut manifest = VerificationManifest::new("feat-skip", Path::new("/project"));
        manifest.add_layer_result(layer("backend", LayerStatus::Passed));
        manifest.add_layer_result(layer("database", LayerStatus::Skipped));

        manifest.compute_overall_status();
        assert_eq!(manifest.overall_status, ManifestStatus::Passed);
    }

    #[test]
    fn atomic_write_no_tmp_remains() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clean.manifest.json");
        let tmp_path = path.with_extension("json.tmp");

        let manifest = VerificationManifest::new("feat-atomic", Path::new("/project"));
        manifest.save(&path).unwrap();

        assert!(path.exists(), "manifest file should exist");
        assert!(!tmp_path.exists(), ".tmp file should not remain after save");
    }

    #[test]
    fn schema_version_check_rejects_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.manifest.json");

        // Write a manifest with a wrong schema version directly.
        let json = serde_json::json!({
            "schema_version": "0.1",
            "feature_id": "feat-old",
            "project_root_hash": "abc123",
            "layers": [],
            "gates": [],
            "overall_status": "pending"
        });
        std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let err = VerificationManifest::load(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("0.1"),
            "error should mention the bad version, got: {msg}"
        );
        assert!(
            msg.contains("0.2"),
            "error should mention the expected version, got: {msg}"
        );
    }

    #[test]
    fn manifest_path_deterministic() {
        let path_a = VerificationManifest::manifest_path("my-feature").unwrap();
        let path_b = VerificationManifest::manifest_path("my-feature").unwrap();
        assert_eq!(path_a, path_b);
        assert!(
            path_a
                .to_string_lossy()
                .contains("my-feature.manifest.json"),
            "path should contain the feature id: {}",
            path_a.display()
        );
    }

    #[test]
    fn project_root_hash_is_stable() {
        let hash_a = hash_project_root(Path::new("/home/user/project"));
        let hash_b = hash_project_root(Path::new("/home/user/project"));
        assert_eq!(hash_a, hash_b);
        // SHA-256 hex is 64 chars.
        assert_eq!(hash_a.len(), 64);
    }

    #[test]
    fn seam_check_result_roundtrip_with_boundary_and_category() {
        let original = SeamCheckResult {
            name: "config_mismatch".to_string(),
            status: CheckStatus::Failed,
            boundary: "backend↔infrastructure".to_string(),
            category: Some(1),
            details: Some("env var 'FOO' missing".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("\"category\":1"));
        assert!(json.contains("\"boundary\":\"backend↔infrastructure\""));
        let back: SeamCheckResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, original.name);
        assert_eq!(back.status, CheckStatus::Failed);
        assert_eq!(back.boundary, original.boundary);
        assert_eq!(back.category, Some(1));
        assert_eq!(back.details, original.details);
    }

    #[test]
    fn seam_check_result_omits_none_category() {
        let result = SeamCheckResult {
            name: "plugin_check".to_string(),
            status: CheckStatus::Passed,
            boundary: "a↔b".to_string(),
            category: None,
            details: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            !json.contains("category"),
            "None category should be skipped: {json}"
        );
    }

    #[test]
    fn check_status_warning_kebab_case() {
        let json = serde_json::to_string(&CheckStatus::Warning).unwrap();
        assert_eq!(json, "\"warning\"");
        let back: CheckStatus = serde_json::from_str("\"warning\"").unwrap();
        assert_eq!(back, CheckStatus::Warning);
    }

    #[test]
    fn serde_enum_kebab_case() {
        // Verify enums serialize to kebab-case as expected.
        let json = serde_json::to_string(&ManifestStatus::FailedInterrupted).unwrap();
        assert_eq!(json, "\"failed-interrupted\"");

        let json = serde_json::to_string(&LayerStatus::InProgress).unwrap();
        assert_eq!(json, "\"in-progress\"");

        let json = serde_json::to_string(&GateStatus::TimedOut).unwrap();
        assert_eq!(json, "\"timed-out\"");

        // And deserialize back.
        let status: ManifestStatus = serde_json::from_str("\"failed-interrupted\"").unwrap();
        assert_eq!(status, ManifestStatus::FailedInterrupted);
    }

    #[test]
    fn optional_fields_omitted_when_none() {
        let result = LayerResult {
            name: "backend".to_string(),
            status: LayerStatus::Pending,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: None,
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            !json.contains("halted_by"),
            "None fields should be omitted: {json}"
        );
        assert!(
            !json.contains("final_confidence"),
            "None fields should be omitted: {json}"
        );
        assert!(
            !json.contains("total_cost_usd"),
            "None fields should be omitted: {json}"
        );
        assert!(
            !json.contains("escalation_events"),
            "None fields should be omitted: {json}"
        );
    }

    // ── Phase 4 — escalation_events round-trip ────────────────────────────

    #[test]
    fn escalation_events_roundtrip_with_all_three_levels() {
        let result = LayerResult {
            name: "backend".to_string(),
            status: LayerStatus::Failed,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: Some("adts_escalation_exhausted".to_string()),
            final_confidence: None,
            total_cost_usd: Some(0.09),
            escalation_events: Some(vec![
                EscalationEvent::Level1FreshContext { at_pass: 1 },
                EscalationEvent::Level2ElevatedEffort { at_pass: 2 },
                EscalationEvent::Level3Exhausted { at_pass: 3 },
            ]),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            json.contains("\"escalation_events\""),
            "populated field must appear: {json}"
        );
        assert!(json.contains("\"level1_fresh_context\""), "{json}");
        assert!(json.contains("\"level2_elevated_effort\""), "{json}");
        assert!(json.contains("\"level3_exhausted\""), "{json}");

        let back: LayerResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.escalation_events.as_deref().map(|v| v.len()), Some(3));
        let events = back.escalation_events.unwrap();
        assert_eq!(
            events[0],
            EscalationEvent::Level1FreshContext { at_pass: 1 }
        );
        assert_eq!(
            events[1],
            EscalationEvent::Level2ElevatedEffort { at_pass: 2 }
        );
        assert_eq!(events[2], EscalationEvent::Level3Exhausted { at_pass: 3 });
    }

    #[test]
    fn escalation_events_absent_from_legacy_manifests() {
        // Manifest written before Phase 4 omits the field entirely — must still parse.
        let legacy = r#"{
            "name": "api",
            "status": "passed",
            "passes": [],
            "seam_checks": []
        }"#;
        let back: LayerResult = serde_json::from_str(legacy).unwrap();
        assert!(back.escalation_events.is_none());
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let result = VerificationManifest::load(Path::new("/nonexistent/manifest.json"));
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.manifest.json");
        std::fs::write(&path, "not valid json {{{").unwrap();
        let result = VerificationManifest::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("deep")
            .join("test.manifest.json");

        let manifest = VerificationManifest::new("feat-nested", Path::new("/project"));
        manifest.save(&path).unwrap();

        assert!(path.exists());
        let loaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(loaded.feature_id, "feat-nested");
    }

    #[test]
    fn manifest_path_for_namespaced_by_project() {
        let path_a =
            VerificationManifest::manifest_path_for("plan", Path::new("/repo/alpha")).unwrap();
        let path_b =
            VerificationManifest::manifest_path_for("plan", Path::new("/repo/beta")).unwrap();
        // Same feature_id but different projects → different paths
        assert_ne!(
            path_a, path_b,
            "different projects should produce different manifest paths"
        );
        // Both contain the feature id
        assert!(path_a.to_string_lossy().contains("plan.manifest.json"));
        assert!(path_b.to_string_lossy().contains("plan.manifest.json"));
        // Path includes a hash subdirectory
        let parent_a = path_a
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert!(
            parent_a.len() == 12,
            "namespace dir should be 12 hex chars, got: {parent_a}"
        );
    }

    #[test]
    fn manifest_path_for_deterministic() {
        let path_a =
            VerificationManifest::manifest_path_for("feat", Path::new("/project")).unwrap();
        let path_b =
            VerificationManifest::manifest_path_for("feat", Path::new("/project")).unwrap();
        assert_eq!(path_a, path_b);
    }

    #[test]
    fn repeated_saves_work() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint.manifest.json");

        let mut manifest = VerificationManifest::new("feat-repeat", Path::new("/project"));
        manifest.save(&path).unwrap();

        // Add a layer and save again (simulates checkpoint)
        manifest.add_layer_result(layer("backend", LayerStatus::Pending));
        manifest.save(&path).unwrap();

        // Add another and save again
        manifest.add_layer_result(layer("frontend", LayerStatus::Skipped));
        manifest.save(&path).unwrap();

        // Load and verify final state
        let loaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(loaded.layers.len(), 2);
        assert_eq!(loaded.layers[0].name, "backend");
        assert_eq!(loaded.layers[1].name, "frontend");
    }
}
