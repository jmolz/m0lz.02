use std::path::{Path, PathBuf};

use anyhow::Result;
use pice_core::config::MemoryConfig;
use pice_core::layers::manifest::{manifest_project_namespace, VerificationManifest};
use pice_core::memory::{
    estimate_tokens, stable_record_id, MemoryConsumer, MemoryRecord, MemoryWriter, RedactionStatus,
};
use pice_core::plan_parser::PlanTrace;

use super::redaction::validate_summary;
use super::store::{append_record, MemoryPaths};

#[derive(Debug, Clone)]
pub struct SessionRunContext {
    pub project_root: PathBuf,
    pub project_hash: String,
    pub state_dir: PathBuf,
    pub feature_id: Option<String>,
    pub plan_path: Option<PathBuf>,
    pub plan_hash: Option<String>,
    pub contract_hash: Option<String>,
    pub provider_name: String,
    pub command: MemoryConsumer,
    pub run_id: String,
}

impl SessionRunContext {
    pub fn foreground(
        project_root: &Path,
        provider_name: &str,
        command: MemoryConsumer,
        feature_id: Option<String>,
        plan_path: Option<PathBuf>,
        trace: Option<&PlanTrace>,
    ) -> Result<Self> {
        let state_dir = VerificationManifest::state_dir()?;
        Ok(Self::with_state_dir(
            project_root,
            &state_dir,
            provider_name,
            command,
            feature_id,
            plan_path,
            trace,
            format!("run_{}", chrono::Utc::now().timestamp_millis()),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_state_dir(
        project_root: &Path,
        state_dir: &Path,
        provider_name: &str,
        command: MemoryConsumer,
        feature_id: Option<String>,
        plan_path: Option<PathBuf>,
        trace: Option<&PlanTrace>,
        run_id: String,
    ) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            project_hash: manifest_project_namespace(project_root),
            state_dir: state_dir.to_path_buf(),
            feature_id,
            plan_path,
            plan_hash: trace.map(|t| t.plan_sha256.clone()),
            contract_hash: trace.map(|t| t.contract_sha256.clone()),
            provider_name: provider_name.to_string(),
            command,
            run_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryWriteOutcome {
    Disabled,
    Denied,
    Written {
        record_ids: Vec<String>,
        estimated_tokens: usize,
    },
    Rejected {
        reason: String,
    },
}

impl MemoryWriteOutcome {
    pub fn result_str(&self) -> &'static str {
        match self {
            Self::Disabled | Self::Denied => "warning",
            Self::Written { .. } => "ok",
            Self::Rejected { .. } => "rejected",
        }
    }
}

pub struct SessionMemoryRecorder<'a> {
    config: &'a MemoryConfig,
}

impl<'a> SessionMemoryRecorder<'a> {
    pub fn new(config: &'a MemoryConfig) -> Self {
        Self { config }
    }

    pub fn preflight_write(&self, writer: MemoryWriter) -> Option<MemoryWriteOutcome> {
        let policy = self.config.policy();
        if !self.config.enabled {
            return Some(MemoryWriteOutcome::Disabled);
        }
        if !policy.can_write(writer) {
            return Some(MemoryWriteOutcome::Denied);
        }
        None
    }

    pub fn record_summary(
        &self,
        ctx: &SessionRunContext,
        writer: MemoryWriter,
        title: &str,
        body: &str,
        tags: Vec<String>,
    ) -> Result<MemoryWriteOutcome> {
        if let Some(outcome) = self.preflight_write(writer) {
            return Ok(outcome);
        }
        let policy = self.config.policy();
        let summary = match validate_summary(title, body, policy.max_tokens) {
            Ok(summary) => summary,
            Err(e) => {
                return Ok(MemoryWriteOutcome::Rejected {
                    reason: e.to_string(),
                })
            }
        };

        let source_commit = current_commit(&ctx.project_root);
        let mut record_ids = Vec::new();
        for concrete_store in self.config.store.concrete_stores() {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let plan_path = ctx.plan_path.as_ref().map(|p| {
                p.strip_prefix(&ctx.project_root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string()
            });
            let seed = format!(
                "{}:{}:{}:{}:{}:{}:{}",
                ctx.project_hash,
                ctx.run_id,
                writer.as_str(),
                concrete_store.as_str(),
                now,
                summary.title,
                summary.body
            );
            let record = MemoryRecord {
                id: stable_record_id(&seed),
                created_at: now,
                source: writer,
                store: concrete_store,
                project_hash: ctx.project_hash.clone(),
                feature_id: ctx.feature_id.clone(),
                plan_path,
                plan_hash: ctx.plan_hash.clone(),
                contract_hash: ctx.contract_hash.clone(),
                run_id: Some(ctx.run_id.clone()),
                source_commit: source_commit.clone(),
                redaction_status: RedactionStatus::Clean,
                redaction_reason: None,
                title: summary.title.clone(),
                body: summary.body.clone(),
                tags: tags.clone(),
            };
            let paths = MemoryPaths::new(&ctx.project_root, &ctx.state_dir);
            append_record(&paths, &record)?;
            record_ids.push(record.id);
        }

        Ok(MemoryWriteOutcome::Written {
            record_ids,
            estimated_tokens: summary.estimated_tokens,
        })
    }
}

fn current_commit(project_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

pub fn record_write_metrics(
    project_root: &Path,
    ctx: &SessionRunContext,
    writer: MemoryWriter,
    outcome: &MemoryWriteOutcome,
) {
    let Ok(Some(db)) = crate::metrics::open_metrics_db(project_root) else {
        return;
    };
    let details = match outcome {
        MemoryWriteOutcome::Disabled => serde_json::json!({"reason": "memory_disabled"}),
        MemoryWriteOutcome::Denied => serde_json::json!({"reason": "writer_denied"}),
        MemoryWriteOutcome::Written { record_ids, .. } => {
            serde_json::json!({"record_count": record_ids.len()})
        }
        MemoryWriteOutcome::Rejected { reason } => serde_json::json!({"reason": reason}),
    }
    .to_string();
    let event_type = if matches!(outcome, MemoryWriteOutcome::Rejected { .. }) {
        "memory_write_rejected"
    } else {
        "memory_write"
    };
    let (record_id, estimated_tokens) = match outcome {
        MemoryWriteOutcome::Written {
            record_ids,
            estimated_tokens,
        } => (
            record_ids.first().map(String::as_str),
            Some(*estimated_tokens),
        ),
        _ => (None, None),
    };
    let plan_path = ctx.plan_path.as_ref().map(|p| {
        p.strip_prefix(project_root)
            .unwrap_or(p)
            .to_string_lossy()
            .to_string()
    });
    let row = crate::metrics::store::MemoryEventRow {
        event_type,
        record_id,
        project_hash: &ctx.project_hash,
        plan_path: plan_path.as_deref(),
        feature_id: ctx.feature_id.as_deref(),
        run_id: Some(&ctx.run_id),
        command: Some(ctx.command.as_str()),
        consumer: None,
        writer: Some(writer.as_str()),
        estimated_tokens,
        result: outcome.result_str(),
        details_json: Some(details.as_str()),
    };
    if let Err(e) = crate::metrics::store::insert_memory_event(&db, &row) {
        tracing::warn!("failed to record memory write event: {e}");
    }
}

pub fn deterministic_execute_summary(plan_title: &str, plan_path: &Path) -> (String, String) {
    let title = format!("Executed {plan_title}");
    let body = format!(
        "The approved plan `{}` completed successfully through the workflow provider. \
         This record is a summary-only lifecycle note; the plan and contract remain authoritative.",
        plan_path.display()
    );
    (title, body)
}

pub fn handoff_summary_from_capture(captured: &str) -> (String, String) {
    let mut lines = captured
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let title = "Captured handoff summary".to_string();
    let body = lines
        .by_ref()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1200)
        .collect::<String>();
    let body = if body.trim().is_empty() {
        "Handoff completed; see HANDOFF.md for the authoritative session handoff.".to_string()
    } else {
        body
    };
    (title, body)
}

#[allow(dead_code)]
fn _summary_tokens(title: &str, body: &str) -> usize {
    estimate_tokens(title) + estimate_tokens(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::memory::MemoryStore;

    #[test]
    fn disabled_recorder_is_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MemoryConfig::default();
        let ctx = SessionRunContext::with_state_dir(
            dir.path(),
            dir.path(),
            "codex",
            MemoryConsumer::Execute,
            None,
            None,
            None,
            "run_1".to_string(),
        );
        let out = SessionMemoryRecorder::new(&cfg)
            .record_summary(
                &ctx,
                MemoryWriter::ExecuteSummary,
                "Title",
                "Summary body",
                vec![],
            )
            .unwrap();
        assert_eq!(out, MemoryWriteOutcome::Disabled);
    }

    #[test]
    fn enabled_recorder_writes_private_state() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MemoryConfig {
            enabled: true,
            store: MemoryStore::PrivateState,
            ..MemoryConfig::default()
        };
        let ctx = SessionRunContext::with_state_dir(
            dir.path(),
            dir.path(),
            "codex",
            MemoryConsumer::Execute,
            None,
            None,
            None,
            "run_1".to_string(),
        );
        let out = SessionMemoryRecorder::new(&cfg)
            .record_summary(
                &ctx,
                MemoryWriter::ExecuteSummary,
                "Title",
                "Summary body",
                vec!["durable".to_string()],
            )
            .unwrap();
        match out {
            MemoryWriteOutcome::Written { record_ids, .. } => {
                assert_eq!(record_ids.len(), 1);
                assert!(record_ids[0].starts_with("mem_"));
                let records_path = dir
                    .path()
                    .join(manifest_project_namespace(dir.path()))
                    .join("memory")
                    .join("records.jsonl");
                assert!(
                    records_path.exists(),
                    "private-state writes must use state_dir/<project_hash>/memory/records.jsonl"
                );
                let content = std::fs::read_to_string(records_path).unwrap();
                assert!(content.contains("\"store\":\"private_state\""));
                assert!(content.contains("\"project_hash\""));
                assert!(content.contains("Summary body"));
            }
            other => panic!("expected write, got {other:?}"),
        }
    }
}
