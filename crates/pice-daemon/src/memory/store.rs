use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pice_core::layers::manifest::manifest_project_namespace;
use pice_core::memory::{MemoryRecord, MemoryStore};

#[derive(Debug, Clone)]
pub struct MemoryPaths {
    pub project_root: PathBuf,
    pub project_hash: String,
    pub state_dir: PathBuf,
}

impl MemoryPaths {
    pub fn new(project_root: &Path, state_dir: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            project_hash: manifest_project_namespace(project_root),
            state_dir: state_dir.to_path_buf(),
        }
    }

    pub fn learnings_path(&self) -> PathBuf {
        self.project_root.join(".pice").join("learnings.md")
    }

    pub fn private_records_path(&self) -> PathBuf {
        self.state_dir
            .join(&self.project_hash)
            .join("memory")
            .join("records.jsonl")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryStoreStats {
    pub record_count: usize,
    pub last_write_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLoadWarning {
    PrivateStateUnreadable,
}

impl MemoryLoadWarning {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PrivateStateUnreadable => "private_state_unreadable",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedMemoryRecords {
    pub records: Vec<MemoryRecord>,
    pub warning: Option<MemoryLoadWarning>,
}

#[derive(Debug, Clone)]
struct ProjectBlock {
    record: MemoryRecord,
    start: usize,
    end: usize,
}

pub fn load_records(paths: &MemoryPaths, store: MemoryStore) -> Result<Vec<MemoryRecord>> {
    let mut records = Vec::new();
    if store.includes_project_learnings() {
        records.extend(load_project_records(paths)?);
    }
    if store.includes_private_state() {
        records.extend(load_private_records(paths)?);
    }
    sort_records_by_created_at(&mut records);
    Ok(records)
}

pub fn load_records_for_workflow(
    paths: &MemoryPaths,
    store: MemoryStore,
) -> Result<LoadedMemoryRecords> {
    let mut records = Vec::new();
    if store.includes_project_learnings() {
        records.extend(load_project_records(paths)?);
    }
    if store.includes_private_state() {
        match load_private_records(paths) {
            Ok(private) => records.extend(private),
            Err(e) => {
                tracing::warn!(
                    project_hash = %paths.project_hash,
                    error = %format!("{e:#}"),
                    "private-state memory unreadable; workflow recall will use an empty brief"
                );
                return Ok(LoadedMemoryRecords {
                    records: Vec::new(),
                    warning: Some(MemoryLoadWarning::PrivateStateUnreadable),
                });
            }
        }
    }
    sort_records_by_created_at(&mut records);
    Ok(LoadedMemoryRecords {
        records,
        warning: None,
    })
}

pub fn stats(paths: &MemoryPaths, store: MemoryStore) -> Result<MemoryStoreStats> {
    let records = load_records(paths, store)?;
    let last_write_at = records
        .iter()
        .max_by(|a, b| compare_created_at(&a.created_at, &b.created_at).then(a.id.cmp(&b.id)))
        .map(|r| r.created_at.clone());
    Ok(MemoryStoreStats {
        record_count: records.len(),
        last_write_at,
    })
}

pub fn append_record(paths: &MemoryPaths, record: &MemoryRecord) -> Result<()> {
    match record.store {
        MemoryStore::ProjectLearnings => append_project_record(paths, record),
        MemoryStore::PrivateState => append_private_record(paths, record),
        MemoryStore::Both => anyhow::bail!("record.store must be concrete, not both"),
    }
}

pub fn delete_record(paths: &MemoryPaths, store: MemoryStore, record_id: &str) -> Result<usize> {
    let mut removed = 0;
    if store.includes_project_learnings() {
        removed += remove_project_records(paths, |record| record.id == record_id)?;
    }
    if store.includes_private_state() {
        removed += remove_private_records(paths, |record| record.id == record_id)?;
    }
    Ok(removed)
}

pub fn prune_records_before(
    paths: &MemoryPaths,
    store: MemoryStore,
    before_rfc3339: &str,
) -> Result<usize> {
    let mut removed = 0;
    if store.includes_project_learnings() {
        removed += remove_project_records(paths, |record| {
            created_at_before(&record.created_at, before_rfc3339)
        })?;
    }
    if store.includes_private_state() {
        removed += remove_private_records(paths, |record| {
            created_at_before(&record.created_at, before_rfc3339)
        })?;
    }
    Ok(removed)
}

fn append_project_record(paths: &MemoryPaths, record: &MemoryRecord) -> Result<()> {
    ensure_markdown_record_safe(record)?;
    let path = paths.learnings_path();
    ensure_under_project(paths, &path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
    };
    if !content.is_empty() {
        parse_project_blocks(&content)
            .with_context(|| format!("refusing to write malformed {}", path.display()))?;
    }
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&record_to_markdown(record));
    parse_project_blocks(&content)
        .with_context(|| format!("refusing to write malformed {}", path.display()))?;
    atomic_write(&path, content.as_bytes())
}

fn ensure_markdown_record_safe(record: &MemoryRecord) -> Result<()> {
    if record.title.lines().count() > 1 {
        anyhow::bail!("memory title must be a single line");
    }
    if record.body.lines().chain(record.title.lines()).any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("<!-- pice-memory ") || trimmed == "<!-- /pice-memory -->"
    }) {
        anyhow::bail!("memory record contains reserved pice-memory marker");
    }
    Ok(())
}

fn append_private_record(paths: &MemoryPaths, record: &MemoryRecord) -> Result<()> {
    let mut records = load_private_records(paths)?;
    records.push(record.clone());
    write_private_records(paths, &records)
}

fn load_project_records(paths: &MemoryPaths) -> Result<Vec<MemoryRecord>> {
    let path = paths.learnings_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_project_blocks(&content)?
        .into_iter()
        .map(|block| block.record)
        .collect())
}

fn load_private_records(paths: &MemoryPaths) -> Result<Vec<MemoryRecord>> {
    let path = paths.private_records_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: MemoryRecord = serde_json::from_str(line)
            .with_context(|| format!("failed to parse JSONL memory record on line {}", idx + 1))?;
        if !seen.insert(record.id.clone()) {
            anyhow::bail!("duplicate memory id in private state: {}", record.id);
        }
        records.push(record);
    }
    Ok(records)
}

fn remove_project_records<F>(paths: &MemoryPaths, mut should_remove: F) -> Result<usize>
where
    F: FnMut(&MemoryRecord) -> bool,
{
    let path = paths.learnings_path();
    if !path.exists() {
        return Ok(0);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let blocks = parse_project_blocks(&content)?;
    let mut output = String::new();
    let mut cursor = 0;
    let mut removed = 0;
    for block in blocks {
        if should_remove(&block.record) {
            output.push_str(&content[cursor..block.start]);
            cursor = block.end;
            removed += 1;
        }
    }
    output.push_str(&content[cursor..]);
    if removed > 0 {
        atomic_write(&path, output.as_bytes())?;
    }
    Ok(removed)
}

fn remove_private_records<F>(paths: &MemoryPaths, mut should_remove: F) -> Result<usize>
where
    F: FnMut(&MemoryRecord) -> bool,
{
    let records = load_private_records(paths)?;
    let before = records.len();
    let kept: Vec<_> = records
        .into_iter()
        .filter(|record| !should_remove(record))
        .collect();
    let removed = before.saturating_sub(kept.len());
    if removed > 0 {
        write_private_records(paths, &kept)?;
    }
    Ok(removed)
}

fn write_private_records(paths: &MemoryPaths, records: &[MemoryRecord]) -> Result<()> {
    let path = paths.private_records_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut content = String::new();
    for record in records {
        content.push_str(&serde_json::to_string(record)?);
        content.push('\n');
    }
    atomic_write(&path, content.as_bytes())
}

fn ensure_under_project(paths: &MemoryPaths, path: &Path) -> Result<()> {
    if !path.starts_with(&paths.project_root) {
        anyhow::bail!(
            "memory project-learnings path escaped project root: {}",
            path.display()
        );
    }
    Ok(())
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::fs::{File, OpenOptions};
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp_path = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp_path)
            .with_context(|| format!("failed to create {}", tmp_path.display()))?;
        file.write_all(content)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to fsync {}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to atomically rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

pub(crate) fn compare_created_at(a: &str, b: &str) -> Ordering {
    match (parse_rfc3339_utc(a), parse_rfc3339_utc(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

fn created_at_before(created_at: &str, boundary: &str) -> bool {
    compare_created_at(created_at, boundary).is_lt()
}

fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn sort_records_by_created_at(records: &mut [MemoryRecord]) {
    records.sort_by(|a, b| compare_created_at(&a.created_at, &b.created_at).then(a.id.cmp(&b.id)));
}

fn record_to_markdown(record: &MemoryRecord) -> String {
    let mut attrs = BTreeMap::new();
    attrs.insert("id", record.id.as_str());
    attrs.insert("created_at", record.created_at.as_str());
    attrs.insert("source", record.source.as_str());
    attrs.insert("store", record.store.as_str());
    attrs.insert("project_hash", record.project_hash.as_str());
    if let Some(value) = record.feature_id.as_deref() {
        attrs.insert("feature_id", value);
    }
    if let Some(value) = record.plan_path.as_deref() {
        attrs.insert("plan_path", value);
    }
    if let Some(value) = record.plan_hash.as_deref() {
        attrs.insert("plan_hash", value);
    }
    if let Some(value) = record.contract_hash.as_deref() {
        attrs.insert("contract_hash", value);
    }
    if let Some(value) = record.run_id.as_deref() {
        attrs.insert("run_id", value);
    }
    if let Some(value) = record.source_commit.as_deref() {
        attrs.insert("source_commit", value);
    }
    attrs.insert("redaction_status", record.redaction_status.as_str());
    if let Some(value) = record.redaction_reason.as_deref() {
        attrs.insert("redaction_reason", value);
    }
    let tags = record.tags.join(",");
    if !tags.is_empty() {
        attrs.insert("tags", tags.as_str());
    }

    let rendered_attrs = attrs
        .into_iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape_attr(v)))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<!-- pice-memory {rendered_attrs} -->\n### {}\n\n{}\n<!-- /pice-memory -->\n",
        record.title.trim(),
        record.body.trim()
    )
}

fn parse_project_blocks(content: &str) -> Result<Vec<ProjectBlock>> {
    let mut blocks = Vec::new();
    let mut seen = BTreeSet::new();
    let lines: Vec<(usize, &str)> = content
        .split_inclusive('\n')
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .collect();
    let mut i = 0;
    while i < lines.len() {
        let (start_offset, line) = lines[i];
        let trimmed = line.trim();
        if !trimmed.starts_with("<!-- pice-memory ") {
            i += 1;
            continue;
        }
        let attrs = parse_attrs(trimmed)?;
        let heading_idx = i + 1;
        let Some((_, heading_line)) = lines.get(heading_idx) else {
            anyhow::bail!("missing heading for memory block");
        };
        let title = heading_line
            .trim()
            .strip_prefix("### ")
            .ok_or_else(|| anyhow::anyhow!("missing memory block title heading"))?
            .trim()
            .to_string();

        let mut body_lines = Vec::new();
        let mut end_offset = None;
        let mut j = heading_idx + 1;
        while j < lines.len() {
            let (line_offset, current) = lines[j];
            if current.trim() == "<!-- /pice-memory -->" {
                end_offset = Some(line_offset + current.len());
                break;
            }
            body_lines.push(current.trim_end_matches('\n').trim_end_matches('\r'));
            j += 1;
        }
        let Some(end) = end_offset else {
            anyhow::bail!("missing end marker for memory block");
        };
        let record = record_from_attrs(attrs, title, body_lines.join("\n").trim().to_string())?;
        if !seen.insert(record.id.clone()) {
            anyhow::bail!("duplicate memory id in project learnings: {}", record.id);
        }
        blocks.push(ProjectBlock {
            record,
            start: start_offset,
            end,
        });
        i = j + 1;
    }
    Ok(blocks)
}

fn record_from_attrs(
    attrs: BTreeMap<String, String>,
    title: String,
    body: String,
) -> Result<MemoryRecord> {
    let required = |key: &str| -> Result<String> {
        attrs
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing memory attribute {key}"))
    };
    let source = serde_json::from_value(serde_json::Value::String(required("source")?))
        .context("invalid memory source")?;
    let store = serde_json::from_value(serde_json::Value::String(required("store")?))
        .context("invalid memory store")?;
    let redaction_status =
        serde_json::from_value(serde_json::Value::String(required("redaction_status")?))
            .context("invalid memory redaction_status")?;
    let tags = attrs
        .get("tags")
        .map(|tags| {
            tags.split(',')
                .filter(|tag| !tag.trim().is_empty())
                .map(|tag| tag.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    Ok(MemoryRecord {
        id: required("id")?,
        created_at: required("created_at")?,
        source,
        store,
        project_hash: required("project_hash")?,
        feature_id: attrs.get("feature_id").cloned(),
        plan_path: attrs.get("plan_path").cloned(),
        plan_hash: attrs.get("plan_hash").cloned(),
        contract_hash: attrs.get("contract_hash").cloned(),
        run_id: attrs.get("run_id").cloned(),
        source_commit: attrs.get("source_commit").cloned(),
        redaction_status,
        redaction_reason: attrs.get("redaction_reason").cloned(),
        title,
        body,
        tags,
    })
}

fn parse_attrs(line: &str) -> Result<BTreeMap<String, String>> {
    let inner = line
        .strip_prefix("<!-- pice-memory ")
        .and_then(|s| s.strip_suffix(" -->"))
        .ok_or_else(|| anyhow::anyhow!("malformed pice-memory marker"))?;
    let mut attrs = BTreeMap::new();
    let chars: Vec<char> = inner.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let key_start = i;
        while i < chars.len() && chars[i] != '=' {
            i += 1;
        }
        if i == key_start || i >= chars.len() {
            anyhow::bail!("malformed memory attribute marker");
        }
        let key: String = chars[key_start..i].iter().collect();
        if key.chars().any(|ch| ch.is_whitespace()) {
            anyhow::bail!("malformed memory attribute key {key}");
        }
        i += 1;
        if i >= chars.len() || chars[i] != '"' {
            anyhow::bail!("malformed memory attribute value for {key}");
        }
        i += 1;
        let value_start = i;
        while i < chars.len() && chars[i] != '"' {
            i += 1;
        }
        if i >= chars.len() {
            anyhow::bail!("unterminated memory attribute value for {key}");
        }
        let value: String = chars[value_start..i].iter().collect();
        i += 1;
        if attrs.insert(key.clone(), unescape_attr(&value)).is_some() {
            anyhow::bail!("duplicate memory attribute {key}");
        }
    }
    Ok(attrs)
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape_attr(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::memory::{MemoryWriter, RedactionStatus};

    fn record(id: &str) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            created_at: "2026-05-19T00:00:00Z".to_string(),
            source: MemoryWriter::HandoffSummary,
            store: MemoryStore::ProjectLearnings,
            project_hash: "abcdef123456".to_string(),
            feature_id: Some("feat".to_string()),
            plan_path: Some(".codex/plans/feat.md".to_string()),
            plan_hash: Some("a".repeat(64)),
            contract_hash: Some("b".repeat(64)),
            run_id: Some("run_1".to_string()),
            source_commit: Some("abc123".to_string()),
            redaction_status: RedactionStatus::Clean,
            redaction_reason: None,
            title: "Short title".to_string(),
            body: "Redacted summary body.".to_string(),
            tags: vec!["durable".to_string()],
        }
    }

    #[test]
    fn project_markdown_roundtrip() {
        let mut record = record("mem_a");
        record.plan_path = Some(".codex/plans/path with spaces.md".to_string());
        let content = record_to_markdown(&record);
        let blocks = parse_project_blocks(&content).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].record.id, "mem_a");
        assert_eq!(blocks[0].record.title, "Short title");
        assert_eq!(
            blocks[0].record.plan_path.as_deref(),
            Some(".codex/plans/path with spaces.md")
        );
        assert_eq!(blocks[0].record.tags, vec!["durable"]);
        assert_eq!(blocks[0].record.redaction_status, RedactionStatus::Clean);
    }

    #[test]
    fn project_markdown_rejects_duplicate_ids() {
        let content = format!(
            "{}{}",
            record_to_markdown(&record("mem_dup")),
            record_to_markdown(&record("mem_dup"))
        );
        let err = parse_project_blocks(&content).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn project_markdown_rejects_missing_end() {
        let content = "<!-- pice-memory id=\"mem_x\" created_at=\"2026-05-19T00:00:00Z\" source=\"handoff_summary\" store=\"project_learnings\" project_hash=\"abc\" redaction_status=\"clean\" -->\n### Title\nbody\n";
        let err = parse_project_blocks(content).unwrap_err();
        assert!(err.to_string().contains("missing end"));
    }

    #[test]
    fn project_markdown_requires_redaction_status() {
        let content = "<!-- pice-memory id=\"mem_x\" created_at=\"2026-05-19T00:00:00Z\" source=\"handoff_summary\" store=\"project_learnings\" project_hash=\"abc\" -->\n### Title\n\nbody\n<!-- /pice-memory -->\n";
        let err = parse_project_blocks(content).unwrap_err();
        assert!(err.to_string().contains("redaction_status"));
    }

    #[test]
    fn project_markdown_write_rejects_reserved_marker_body() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MemoryPaths::new(dir.path(), dir.path().join("state").as_path());
        let mut record = record("mem_marker");
        record.body = "Useful note\n<!-- /pice-memory -->\nInjected tail".to_string();
        let err = append_project_record(&paths, &record).unwrap_err();
        assert!(err.to_string().contains("reserved pice-memory marker"));
    }

    #[test]
    fn project_markdown_write_refuses_malformed_existing_file_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MemoryPaths::new(dir.path(), dir.path().join("state").as_path());
        let path = paths.learnings_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let malformed = "<!-- pice-memory id=\"mem_x\" created_at=\"2026-05-19T00:00:00Z\" source=\"handoff_summary\" store=\"project_learnings\" project_hash=\"abc\" redaction_status=\"clean\" -->\n### Title\nbody\n";
        std::fs::write(&path, malformed).unwrap();

        let err = append_project_record(&paths, &record("mem_new")).unwrap_err();

        assert!(format!("{err:#}").contains("refusing to write malformed"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), malformed);
    }

    #[test]
    fn private_jsonl_rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MemoryPaths::new(dir.path(), dir.path().join("state").as_path());
        let path = paths.private_records_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"id":"mem_x","created_at":"2026-05-19T00:00:00Z","source":"handoff_summary","store":"private_state","project_hash":"abc","redaction_status":"clean","title":"Title","body":"Body","tags":[],"unknown":true}"#,
        )
        .unwrap();
        let err = load_private_records(&paths).unwrap_err();
        assert!(format!("{err:#}").contains("unknown"));
    }

    #[test]
    fn workflow_load_warns_and_empties_on_private_state_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MemoryPaths::new(dir.path(), dir.path().join("state").as_path());
        let path = paths.private_records_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json\n").unwrap();

        let loaded = load_records_for_workflow(&paths, MemoryStore::PrivateState).unwrap();

        assert_eq!(loaded.records.len(), 0);
        assert_eq!(
            loaded.warning,
            Some(MemoryLoadWarning::PrivateStateUnreadable)
        );
        assert!(load_records(&paths, MemoryStore::PrivateState).is_err());
    }

    #[test]
    fn prune_records_before_compares_mixed_rfc3339_offsets_by_instant() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MemoryPaths::new(dir.path(), dir.path().join("state").as_path());
        let mut equal_boundary = record("mem_equal");
        equal_boundary.store = MemoryStore::PrivateState;
        equal_boundary.created_at = "2026-05-19T00:00:00+00:00".to_string();
        let mut before_boundary = record("mem_before");
        before_boundary.store = MemoryStore::PrivateState;
        before_boundary.created_at = "2026-05-18T23:59:59+00:00".to_string();
        write_private_records(&paths, &[equal_boundary, before_boundary]).unwrap();

        let removed =
            prune_records_before(&paths, MemoryStore::PrivateState, "2026-05-19T00:00:00Z")
                .unwrap();
        let remaining = load_private_records(&paths).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "mem_equal");
    }
}
