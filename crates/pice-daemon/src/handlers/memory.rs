//! `pice memory` governance handler.

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, MemoryRequest, MemorySubcommand};
use pice_core::layers::manifest::VerificationManifest;
use serde_json::json;

use crate::memory::store::{
    compare_created_at, delete_record, load_records, prune_records_before, stats, MemoryPaths,
};
use crate::metrics;
use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;

pub async fn run(
    req: MemoryRequest,
    ctx: &DaemonContext,
    _sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();
    let state_dir = VerificationManifest::state_dir()?;
    let paths = MemoryPaths::new(project_root, &state_dir);

    match req.subcommand {
        MemorySubcommand::Status => {
            let stats = stats(&paths, config.memory.store)?;
            let warnings = record_event(
                project_root,
                "memory_read",
                None,
                &paths.project_hash,
                None,
                None,
                None,
                Some("memory"),
                None,
                None,
                None,
                "ok",
                Some(json!({"command": "status"})),
            );
            let value = json!({
                "status": "complete",
                "enabled": config.memory.enabled,
                "store": config.memory.store.as_str(),
                "project_hash": paths.project_hash,
                "record_count": stats.record_count,
                "retention_days": config.memory.retention_days,
                "last_write_at": stats.last_write_at,
                "warnings": warnings,
            });
            json_or_text(req.json, value, || {
                format!(
                    "Memory: {}\nStore: {}\nProject hash: {}\nRecords: {}\nRetention days: {}\n",
                    if config.memory.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    config.memory.store.as_str(),
                    paths.project_hash,
                    stats.record_count,
                    config.memory.retention_days,
                )
            })
        }
        MemorySubcommand::List { limit, feature } => {
            let mut records = load_records(&paths, config.memory.store)?;
            if let Some(feature) = feature.as_deref() {
                records.retain(|record| record.feature_id.as_deref() == Some(feature));
            }
            records.sort_by(|a, b| {
                compare_created_at(&b.created_at, &a.created_at).then(a.id.cmp(&b.id))
            });
            if let Some(limit) = limit {
                records.truncate(limit);
            }
            let warnings = record_event(
                project_root,
                "memory_read",
                None,
                &paths.project_hash,
                None,
                feature.as_deref(),
                None,
                Some("memory"),
                None,
                None,
                None,
                "ok",
                Some(json!({"command": "list", "count": records.len()})),
            );
            let entries: Vec<_> = records
                .iter()
                .map(|record| {
                    json!({
                        "id": record.id,
                        "created_at": record.created_at,
                        "source": record.source.as_str(),
                        "store": record.store.as_str(),
                        "feature_id": record.feature_id,
                        "plan_path": record.plan_path,
                        "title": record.title,
                        "tags": record.tags,
                    })
                })
                .collect();
            let value = json!({
                "status": "complete",
                "records": entries,
                "warnings": warnings,
            });
            json_or_text(req.json, value, || {
                let mut out = String::new();
                for record in &records {
                    out.push_str(&format!(
                        "{}  {}  {}  {}\n",
                        record.id,
                        record.created_at,
                        record.source.as_str(),
                        record.title
                    ));
                }
                if out.is_empty() {
                    out.push_str("No memory records found.\n");
                }
                out
            })
        }
        MemorySubcommand::Show { record_id } => {
            let records = load_records(&paths, config.memory.store)?;
            let Some(record) = records.into_iter().find(|record| record.id == record_id) else {
                if req.json {
                    return Ok(CommandResponse::ExitJson {
                        code: 1,
                        value: json!({
                            "status": "not_found",
                            "record_id": record_id,
                            "message": format!("memory record not found: {record_id}"),
                        }),
                    });
                }
                return Ok(CommandResponse::Exit {
                    code: 1,
                    message: format!("memory record not found: {record_id}"),
                });
            };
            let warnings = record_event(
                project_root,
                "memory_read",
                Some(&record.id),
                &paths.project_hash,
                record.plan_path.as_deref(),
                record.feature_id.as_deref(),
                record.run_id.as_deref(),
                Some("memory"),
                None,
                None,
                Some(pice_core::memory::estimate_tokens(&record.body)),
                "ok",
                Some(json!({"command": "show"})),
            );
            let value = json!({
                "status": "complete",
                "record": record,
                "warnings": warnings,
            });
            json_or_text(req.json, value, || {
                format!(
                    "{}\n{}\n{}\n\n{}\n",
                    record.id, record.created_at, record.title, record.body
                )
            })
        }
        MemorySubcommand::Delete { record_id } => {
            let removed = delete_record(&paths, config.memory.store, &record_id)?;
            let mut warnings = record_event(
                project_root,
                "memory_delete",
                Some(&record_id),
                &paths.project_hash,
                None,
                None,
                None,
                Some("memory"),
                None,
                None,
                None,
                if removed > 0 { "ok" } else { "warning" },
                Some(json!({"removed": removed})),
            );
            if config.memory.store.includes_project_learnings() {
                warnings.push(".pice/learnings.md deletion edits the current file only; prior git history may still contain the record.".to_string());
            }
            let value = json!({
                "status": "complete",
                "record_id": record_id,
                "removed": removed,
                "warnings": warnings,
            });
            json_or_text(req.json, value, || {
                format!("Removed {removed} memory record(s).\n")
            })
        }
        MemorySubcommand::Prune { before } => {
            let boundary = prune_boundary(before.as_deref(), config.memory.retention_days)
                .context("failed to determine prune boundary")?;
            let removed = prune_records_before(&paths, config.memory.store, &boundary)?;
            let mut warnings = record_event(
                project_root,
                "memory_prune",
                None,
                &paths.project_hash,
                None,
                None,
                None,
                Some("memory"),
                None,
                None,
                None,
                "ok",
                Some(json!({"removed": removed, "before": boundary})),
            );
            if config.memory.store.includes_project_learnings() {
                warnings.push(".pice/learnings.md pruning edits the current file only; prior git history may still contain pruned records.".to_string());
            }
            let value = json!({
                "status": "complete",
                "before": boundary,
                "removed": removed,
                "warnings": warnings,
            });
            json_or_text(req.json, value, || {
                format!("Pruned {removed} memory record(s) before {boundary}.\n")
            })
        }
    }
}

fn prune_boundary(before: Option<&str>, retention_days: u32) -> Result<String> {
    if let Some(day) = before {
        let date = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .with_context(|| format!("expected YYYY-MM-DD for --before, got {day}"))?;
        let dt = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid UTC day boundary"))?
            .and_utc();
        return Ok(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    if retention_days == 0 {
        anyhow::bail!("prune requires --before when memory.retention_days = 0");
    }
    let boundary = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
    Ok(boundary.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[allow(clippy::too_many_arguments)]
fn record_event(
    project_root: &std::path::Path,
    event_type: &str,
    record_id: Option<&str>,
    project_hash: &str,
    plan_path: Option<&str>,
    feature_id: Option<&str>,
    run_id: Option<&str>,
    command: Option<&str>,
    consumer: Option<&str>,
    writer: Option<&str>,
    estimated_tokens: Option<usize>,
    result: &str,
    details: Option<serde_json::Value>,
) -> Vec<String> {
    let Some(details_json) = details.map(|value| value.to_string()) else {
        return try_record_event(
            project_root,
            event_type,
            record_id,
            project_hash,
            plan_path,
            feature_id,
            run_id,
            command,
            consumer,
            writer,
            estimated_tokens,
            result,
            None,
        );
    };
    try_record_event(
        project_root,
        event_type,
        record_id,
        project_hash,
        plan_path,
        feature_id,
        run_id,
        command,
        consumer,
        writer,
        estimated_tokens,
        result,
        Some(details_json.as_str()),
    )
}

#[allow(clippy::too_many_arguments)]
fn try_record_event(
    project_root: &std::path::Path,
    event_type: &str,
    record_id: Option<&str>,
    project_hash: &str,
    plan_path: Option<&str>,
    feature_id: Option<&str>,
    run_id: Option<&str>,
    command: Option<&str>,
    consumer: Option<&str>,
    writer: Option<&str>,
    estimated_tokens: Option<usize>,
    result: &str,
    details_json: Option<&str>,
) -> Vec<String> {
    let Ok(Some(db)) = metrics::open_metrics_db(project_root) else {
        return Vec::new();
    };
    let row = metrics::store::MemoryEventRow {
        event_type,
        record_id,
        project_hash,
        plan_path,
        feature_id,
        run_id,
        command,
        consumer,
        writer,
        estimated_tokens,
        result,
        details_json,
    };
    if let Err(e) = metrics::store::insert_memory_event(&db, &row) {
        return vec![format!("failed to record memory metadata event: {e}")];
    }
    Vec::new()
}

fn json_or_text<F>(json_mode: bool, value: serde_json::Value, text: F) -> Result<CommandResponse>
where
    F: FnOnce() -> String,
{
    if json_mode {
        Ok(CommandResponse::Json { value })
    } else {
        Ok(CommandResponse::Text { content: text() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_before_uses_utc_day_boundary() {
        assert_eq!(
            prune_boundary(Some("2026-05-19"), 90).unwrap(),
            "2026-05-19T00:00:00Z"
        );
    }
}
