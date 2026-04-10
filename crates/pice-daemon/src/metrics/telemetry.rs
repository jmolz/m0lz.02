use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pice_core::config::TelemetryConfig;

use super::db::MetricsDb;
use super::store;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Client for opt-in anonymous telemetry.
/// Queues events locally, logs to JSONL, and sends via HTTP when an endpoint is configured.
pub struct TelemetryClient {
    #[allow(dead_code)]
    endpoint: String,
    log_path: PathBuf,
}

/// A telemetry event to queue. Contains only anonymous aggregate data.
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEvent {
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_avg: Option<f64>,
    pub provider_type: String,
    pub timestamp: String,
}

impl TelemetryClient {
    pub fn new(config: &TelemetryConfig, project_root: &Path) -> Self {
        Self {
            endpoint: config.endpoint.clone(),
            log_path: project_root.join(".pice/telemetry-log.jsonl"),
        }
    }

    /// Queue a telemetry event. Anonymizes the payload before storing.
    /// Also appends to the JSONL log for transparency.
    pub fn queue_event(&self, db: &MetricsDb, event: &TelemetryEvent) -> Result<()> {
        let anonymized = anonymize(event);
        let payload = serde_json::to_string(&anonymized).context("failed to serialize event")?;

        // Append to JSONL log (transparency)
        self.append_to_log(&payload)?;

        // Queue in DB for future sending
        store::queue_telemetry(db, &payload)?;

        Ok(())
    }

    /// Send pending telemetry events via HTTP POST, then mark as sent.
    /// Fully non-fatal: all errors are handled internally at debug level.
    /// Production evaluate uses a fire-and-forget spawned task instead of
    /// awaiting this method, to avoid blocking output with HTTP latency.
    #[allow(dead_code)]
    pub async fn flush(&self, db: &MetricsDb) {
        if let Err(e) = self.flush_inner(db).await {
            tracing::debug!("telemetry flush failed: {e}");
        }
    }

    #[allow(dead_code)]
    async fn flush_inner(&self, db: &MetricsDb) -> Result<()> {
        let pending = store::get_pending_telemetry(db, 50)?;
        if pending.is_empty() {
            return Ok(());
        }

        let payloads: Vec<serde_json::Value> = pending
            .iter()
            .filter_map(|e| serde_json::from_str(&e.payload_json).ok())
            .collect();

        if payloads.is_empty() {
            return Ok(());
        }

        send_batch(&self.endpoint, &payloads).await?;

        let ids: Vec<i64> = pending.iter().map(|e| e.id).collect();
        store::mark_telemetry_sent(db, &ids)?;
        tracing::debug!(count = ids.len(), "flushed telemetry queue via HTTP");
        Ok(())
    }

    /// Read recent entries from the JSONL log.
    /// Used by `pice telemetry show` (not yet implemented as a CLI command).
    #[allow(dead_code)]
    pub fn read_log(&self, limit: usize) -> Result<Vec<String>> {
        if !self.log_path.exists() {
            return Ok(Vec::new());
        }
        let content =
            std::fs::read_to_string(&self.log_path).context("failed to read telemetry log")?;
        let lines: Vec<String> = content
            .lines()
            .rev()
            .take(limit)
            .map(|s| s.to_string())
            .collect();
        Ok(lines)
    }

    fn append_to_log(&self, payload: &str) -> Result<()> {
        use std::io::Write;

        if let Some(parent) = self.log_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .with_context(|| {
                format!("failed to open telemetry log: {}", self.log_path.display())
            })?;
        writeln!(file, "{payload}").context("failed to write to telemetry log")?;
        Ok(())
    }
}

/// Wire-format struct for telemetry payloads. This is a SEPARATE type from
/// `TelemetryEvent` — it whitelists exactly the fields that are safe to send.
/// If a new field is added to `TelemetryEvent`, the compiler will NOT automatically
/// include it here, preventing accidental data leakage.
#[derive(Debug, Clone, Serialize)]
struct AnonymizedPayload {
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    passed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score_avg: Option<f64>,
    provider_type: String,
    timestamp: String,
}

/// Send a batch of telemetry payloads via HTTP POST.
/// Returns `Ok(())` on a 2xx response, or an error otherwise.
///
/// This is the single implementation of the HTTP send logic — used by both
/// `flush_inner()` (library/test path) and the fire-and-forget spawn in
/// `commands::evaluate::flush_telemetry()`.
pub async fn send_batch(endpoint: &str, payloads: &[serde_json::Value]) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("failed to build HTTP client")?;

    let resp = client
        .post(endpoint)
        .json(payloads)
        .send()
        .await
        .context("telemetry HTTP request failed")?;

    if resp.status().is_success() {
        Ok(())
    } else {
        anyhow::bail!("telemetry endpoint returned status {}", resp.status());
    }
}

/// Convert a TelemetryEvent into an AnonymizedPayload by explicitly copying
/// only the safe fields. The destructuring pattern ensures that adding a new
/// field to TelemetryEvent causes a compile error here, forcing the developer
/// to decide whether the new field belongs in the wire format.
fn anonymize(event: &TelemetryEvent) -> AnonymizedPayload {
    let TelemetryEvent {
        event_type,
        tier,
        passed,
        score_avg,
        provider_type,
        timestamp,
    } = event;

    AnonymizedPayload {
        event_type: event_type.clone(),
        tier: *tier,
        passed: *passed,
        score_avg: *score_avg,
        provider_type: provider_type.clone(),
        timestamp: timestamp.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> TelemetryConfig {
        TelemetryConfig {
            enabled: true,
            endpoint: "https://telemetry.pice.dev/v1/events".to_string(),
        }
    }

    #[test]
    fn anonymize_produces_separate_wire_type() {
        let event = TelemetryEvent {
            event_type: "evaluation".to_string(),
            tier: Some(2),
            passed: Some(true),
            score_avg: Some(8.5),
            provider_type: "claude-code".to_string(),
            timestamp: "2026-04-03T12:00:00Z".to_string(),
        };
        let anon = anonymize(&event);
        let json = serde_json::to_string(&anon).unwrap();

        // Verify only the whitelisted fields are present by parsing as generic JSON
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = parsed.as_object().unwrap();
        let allowed_keys: std::collections::HashSet<&str> = [
            "event_type",
            "tier",
            "passed",
            "score_avg",
            "provider_type",
            "timestamp",
        ]
        .into();
        for key in obj.keys() {
            assert!(
                allowed_keys.contains(key.as_str()),
                "unexpected field in anonymized payload: {key}"
            );
        }

        // Must NOT contain file paths, project names, or code
        assert!(!json.contains("/"));
        assert!(!json.contains("\\\\"));
    }

    #[test]
    fn anonymize_preserves_values() {
        let event = TelemetryEvent {
            event_type: "evaluation".to_string(),
            tier: Some(3),
            passed: Some(false),
            score_avg: Some(5.0),
            provider_type: "codex".to_string(),
            timestamp: "2026-04-03T12:00:00Z".to_string(),
        };
        let anon = anonymize(&event);
        assert_eq!(anon.event_type, "evaluation");
        assert_eq!(anon.tier, Some(3));
        assert_eq!(anon.passed, Some(false));
        assert_eq!(anon.provider_type, "codex");
    }

    /// This test documents the compile-time safety guarantee: if a new field is
    /// added to TelemetryEvent, the destructuring in anonymize() will fail to compile,
    /// forcing the developer to explicitly decide whether it belongs in the wire format.
    /// The separate AnonymizedPayload type means new fields are NOT automatically sent.
    #[test]
    fn anonymize_wire_format_is_distinct_from_event() {
        // AnonymizedPayload is a private type — it can only be constructed by anonymize().
        // This test verifies that the JSON output is a strict subset of safe fields.
        let event = TelemetryEvent {
            event_type: "evaluation".to_string(),
            tier: None,
            passed: None,
            score_avg: None,
            provider_type: "claude-code".to_string(),
            timestamp: "2026-04-03T12:00:00Z".to_string(),
        };
        let anon = anonymize(&event);
        let json = serde_json::to_string(&anon).unwrap();

        // With None fields skipped, only event_type, provider_type, timestamp remain
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = parsed.as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert!(obj.contains_key("event_type"));
        assert!(obj.contains_key("provider_type"));
        assert!(obj.contains_key("timestamp"));
    }

    #[test]
    fn queue_event_and_log() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetricsDb::open_in_memory().unwrap();
        let config = test_config();
        let client = TelemetryClient::new(&config, dir.path());

        let event = TelemetryEvent {
            event_type: "evaluation".to_string(),
            tier: Some(2),
            passed: Some(true),
            score_avg: Some(8.0),
            provider_type: "claude-code".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        client.queue_event(&db, &event).unwrap();

        // Verify queued in DB
        let pending = store::get_pending_telemetry(&db, 10).unwrap();
        assert_eq!(pending.len(), 1);

        // Verify logged to JSONL
        let log_path = dir.path().join(".pice/telemetry-log.jsonl");
        assert!(log_path.exists());
        let log_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(!log_content.is_empty());
        // Verify it's valid JSON
        let _: serde_json::Value = serde_json::from_str(log_content.trim()).unwrap();
    }

    #[tokio::test]
    async fn flush_handles_unreachable_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetricsDb::open_in_memory().unwrap();
        let config = test_config();
        let client = TelemetryClient::new(&config, dir.path());

        let event = TelemetryEvent {
            event_type: "evaluation".to_string(),
            tier: Some(1),
            passed: Some(true),
            score_avg: Some(9.0),
            provider_type: "claude-code".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        client.queue_event(&db, &event).unwrap();

        // flush() is fully non-fatal — it never returns an error
        client.flush(&db).await;

        // Events stay in queue since HTTP failed (not marked sent)
        let pending = store::get_pending_telemetry(&db, 10).unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[tokio::test]
    async fn flush_empty_queue_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetricsDb::open_in_memory().unwrap();
        let config = test_config();
        let client = TelemetryClient::new(&config, dir.path());

        // flush on empty queue is a no-op
        client.flush(&db).await;
    }

    #[test]
    fn read_log_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let client = TelemetryClient::new(&config, dir.path());
        let entries = client.read_log(10).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn jsonl_log_no_file_paths() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetricsDb::open_in_memory().unwrap();
        let config = test_config();
        let client = TelemetryClient::new(&config, dir.path());

        let event = TelemetryEvent {
            event_type: "evaluation".to_string(),
            tier: Some(2),
            passed: Some(true),
            score_avg: Some(7.5),
            provider_type: "claude-code".to_string(),
            timestamp: "2026-04-03T12:00:00Z".to_string(),
        };
        client.queue_event(&db, &event).unwrap();

        let log_path = dir.path().join(".pice/telemetry-log.jsonl");
        let content = std::fs::read_to_string(&log_path).unwrap();
        // Verify no file paths or project names leak
        assert!(!content.contains(".claude/"));
        assert!(!content.contains(".pice/"));
        assert!(!content.contains("plan.md"));
    }
}
