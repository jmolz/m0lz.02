use anyhow::Result;
use pice_core::config::MemoryConfig;
use pice_core::memory::{
    estimate_tokens, MemoryBrief, MemoryBriefRecord, MemoryConsumer, MemoryRecord, RedactionStatus,
};

use super::store::{compare_created_at, load_records_for_workflow, MemoryLoadWarning, MemoryPaths};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecallResult {
    pub brief: Option<MemoryBrief>,
    pub warning: Option<MemoryLoadWarning>,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryReadMetrics<'a> {
    pub consumer: MemoryConsumer,
    pub feature_id: Option<&'a str>,
    pub plan_path: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub brief: Option<&'a MemoryBrief>,
    pub warning: Option<MemoryLoadWarning>,
}

pub fn build_memory_brief(
    paths: &MemoryPaths,
    config: &MemoryConfig,
    consumer: MemoryConsumer,
    feature_id: Option<&str>,
    plan_path: Option<&str>,
    now_rfc3339: &str,
) -> Result<Option<MemoryBrief>> {
    Ok(build_memory_recall(paths, config, consumer, feature_id, plan_path, now_rfc3339)?.brief)
}

pub fn build_memory_recall(
    paths: &MemoryPaths,
    config: &MemoryConfig,
    consumer: MemoryConsumer,
    feature_id: Option<&str>,
    plan_path: Option<&str>,
    now_rfc3339: &str,
) -> Result<MemoryRecallResult> {
    let policy = config.policy();
    if !policy.can_read(consumer) {
        return Ok(MemoryRecallResult {
            brief: None,
            warning: None,
        });
    }

    let loaded = load_records_for_workflow(paths, config.store)?;
    if loaded.warning.is_some() {
        return Ok(MemoryRecallResult {
            brief: None,
            warning: loaded.warning,
        });
    }

    let mut candidates: Vec<MemoryRecord> = loaded
        .records
        .into_iter()
        .filter(|record| record.project_hash == paths.project_hash)
        .filter(|record| record.redaction_status == RedactionStatus::Clean)
        .filter(|record| within_retention(record, config.retention_days, now_rfc3339))
        .collect();

    candidates.sort_by(|a, b| compare_records(a, b, feature_id, plan_path));

    let mut selected = Vec::new();
    let mut total_tokens = 0;
    for record in candidates {
        let tokens = estimate_tokens(&record.title) + estimate_tokens(&record.body);
        if selected.len() >= policy.max_recalled_items {
            break;
        }
        if policy.max_tokens > 0 && total_tokens + tokens > policy.max_tokens {
            continue;
        }
        total_tokens += tokens;
        selected.push(MemoryBriefRecord::from(&record));
    }

    if selected.is_empty() {
        return Ok(MemoryRecallResult {
            brief: None,
            warning: None,
        });
    }

    Ok(MemoryRecallResult {
        brief: Some(MemoryBrief {
            source: format!("pice-memory:{}", config.store.as_str()),
            records: selected,
            estimated_tokens: total_tokens,
        }),
        warning: None,
    })
}

pub fn record_read_metrics(
    project_root: &std::path::Path,
    paths: &MemoryPaths,
    metrics: MemoryReadMetrics<'_>,
) {
    let Ok(Some(db)) = crate::metrics::open_metrics_db(project_root) else {
        return;
    };
    let estimated_tokens = metrics.brief.map(|brief| brief.estimated_tokens);
    let count = metrics.brief.map(|brief| brief.records.len()).unwrap_or(0);
    let result = if metrics.warning.is_some() {
        "warning"
    } else {
        "ok"
    };
    let details = match metrics.warning {
        Some(warning) => {
            serde_json::json!({"record_count": count, "reason": warning.as_str()}).to_string()
        }
        None => serde_json::json!({"record_count": count}).to_string(),
    };
    let row = crate::metrics::store::MemoryEventRow {
        event_type: "memory_read",
        record_id: None,
        project_hash: &paths.project_hash,
        plan_path: metrics.plan_path,
        feature_id: metrics.feature_id,
        run_id: metrics.run_id,
        command: Some(metrics.consumer.as_str()),
        consumer: Some(metrics.consumer.as_str()),
        writer: None,
        estimated_tokens,
        result,
        details_json: Some(details.as_str()),
    };
    if let Err(e) = crate::metrics::store::insert_memory_event(&db, &row) {
        tracing::warn!("failed to record memory read event: {e}");
    }
}

fn within_retention(record: &MemoryRecord, retention_days: u32, now_rfc3339: &str) -> bool {
    if retention_days == 0 {
        return true;
    }
    let Ok(created) = chrono::DateTime::parse_from_rfc3339(&record.created_at) else {
        return false;
    };
    let Ok(now) = chrono::DateTime::parse_from_rfc3339(now_rfc3339) else {
        return true;
    };
    let age = now.signed_duration_since(created);
    age.num_days() <= retention_days as i64
}

fn compare_records(
    a: &MemoryRecord,
    b: &MemoryRecord,
    feature_id: Option<&str>,
    plan_path: Option<&str>,
) -> std::cmp::Ordering {
    let a_key = rank(a, feature_id, plan_path);
    let b_key = rank(b, feature_id, plan_path);
    b_key
        .cmp(&a_key)
        .then_with(|| compare_created_at(&b.created_at, &a.created_at))
        .then_with(|| a.id.cmp(&b.id))
}

fn rank(record: &MemoryRecord, feature_id: Option<&str>, plan_path: Option<&str>) -> u8 {
    let durable = record
        .tags
        .iter()
        .any(|tag| matches!(tag.as_str(), "durable" | "project" | "lesson"));
    let feature_match = feature_id.is_some() && record.feature_id.as_deref() == feature_id;
    let plan_match = plan_path.is_some() && record.plan_path.as_deref() == plan_path;
    (durable as u8) * 4 + (feature_match as u8) * 2 + (plan_match as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::config::MemoryConfig;
    use pice_core::memory::{MemoryStore, MemoryWriter, RedactionStatus};

    fn record(id: &str, created_at: &str, tags: &[&str]) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            created_at: created_at.to_string(),
            source: MemoryWriter::HandoffSummary,
            store: MemoryStore::PrivateState,
            project_hash: "abcdef123456".to_string(),
            feature_id: None,
            plan_path: None,
            plan_hash: None,
            contract_hash: None,
            run_id: None,
            source_commit: None,
            redaction_status: RedactionStatus::Clean,
            redaction_reason: None,
            title: format!("Title {id}"),
            body: "Body".to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
        }
    }

    #[test]
    fn recall_orders_durable_before_recent() {
        let mut records = [
            record("mem_recent", "2026-05-19T00:00:00Z", &[]),
            record("mem_durable", "2026-05-18T00:00:00Z", &["durable"]),
        ];
        records.sort_by(|a, b| compare_records(a, b, None, None));
        assert_eq!(records[0].id, "mem_durable");
    }

    #[test]
    fn workflow_recall_returns_warning_and_empty_brief_for_corrupt_private_state() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let paths = MemoryPaths::new(dir.path(), &state_dir);
        let private_path = paths.private_records_path();
        std::fs::create_dir_all(private_path.parent().unwrap()).unwrap();
        std::fs::write(&private_path, "not-json\n").unwrap();
        let config = MemoryConfig {
            enabled: true,
            store: MemoryStore::PrivateState,
            ..MemoryConfig::default()
        };

        let result = build_memory_recall(
            &paths,
            &config,
            MemoryConsumer::Prime,
            None,
            None,
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        assert!(result.brief.is_none());
        assert_eq!(
            result.warning,
            Some(MemoryLoadWarning::PrivateStateUnreadable)
        );
    }
}
