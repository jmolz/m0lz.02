use anyhow::{Context, Result};
use pice_protocol::CriterionScore;

use super::db::MetricsDb;

/// Summary of the most recent evaluation for a plan.
#[derive(Debug, Clone)]
pub struct EvaluationSummary {
    #[allow(dead_code)]
    pub id: i64,
    #[allow(dead_code)]
    pub tier: u8,
    pub passed: bool,
    pub avg_score: f64,
    pub timestamp: String,
}

/// A pending telemetry entry from the queue.
#[derive(Debug, Clone)]
pub struct TelemetryEntry {
    pub id: i64,
    pub payload_json: String,
}

/// Record a completed evaluation and its per-criterion scores atomically.
/// Uses a transaction so partial failures don't leave orphaned rows.
/// Returns the evaluation row ID.
#[allow(clippy::too_many_arguments)]
pub fn record_evaluation(
    db: &MetricsDb,
    plan_path: &str,
    feature_name: &str,
    tier: u8,
    passed: bool,
    primary_provider: &str,
    primary_model: &str,
    adversarial_provider: Option<&str>,
    adversarial_model: Option<&str>,
    summary: Option<&str>,
    scores: &[CriterionScore],
) -> Result<i64> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let conn = db.conn();

    conn.execute_batch("BEGIN TRANSACTION")
        .context("failed to begin transaction")?;

    let result = (|| -> Result<i64> {
        conn.execute(
            "INSERT INTO evaluations (plan_path, feature_name, tier, passed, primary_provider, primary_model, adversarial_provider, adversarial_model, summary, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                plan_path,
                feature_name,
                tier,
                passed as i32,
                primary_provider,
                primary_model,
                adversarial_provider,
                adversarial_model,
                summary,
                timestamp,
            ],
        )
        .context("failed to insert evaluation")?;

        let eval_id = conn.last_insert_rowid();

        for score in scores {
            conn.execute(
                "INSERT INTO criteria_scores (evaluation_id, name, score, threshold, passed, findings)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    eval_id,
                    score.name,
                    score.score,
                    score.threshold,
                    score.passed as i32,
                    score.findings,
                ],
            )
            .context("failed to insert criterion score")?;
        }

        Ok(eval_id)
    })();

    match result {
        Ok(eval_id) => {
            conn.execute_batch("COMMIT")
                .context("failed to commit transaction")?;
            Ok(eval_id)
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK").ok();
            Err(e)
        }
    }
}

/// Record a lifecycle event (plan_created, execute_started, etc.).
pub fn record_loop_event(
    db: &MetricsDb,
    event_type: &str,
    plan_path: Option<&str>,
    data_json: Option<&str>,
) -> Result<()> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    db.conn()
        .execute(
            "INSERT INTO loop_events (event_type, plan_path, timestamp, data_json)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![event_type, plan_path, timestamp, data_json],
        )
        .context("failed to insert loop event")?;
    Ok(())
}

/// Get the most recent evaluation for a given plan path.
pub fn get_latest_evaluation(db: &MetricsDb, plan_path: &str) -> Result<Option<EvaluationSummary>> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT e.id, e.tier, e.passed, e.timestamp,
                    COALESCE(AVG(cs.score), 0.0) as avg_score
             FROM evaluations e
             LEFT JOIN criteria_scores cs ON cs.evaluation_id = e.id
             WHERE e.plan_path = ?1
             GROUP BY e.id
             ORDER BY e.timestamp DESC
             LIMIT 1",
        )
        .context("failed to prepare latest evaluation query")?;

    let result = stmt
        .query_row(rusqlite::params![plan_path], |row| {
            Ok(EvaluationSummary {
                id: row.get(0)?,
                tier: row.get(1)?,
                passed: row.get::<_, i32>(2)? != 0,
                timestamp: row.get(3)?,
                avg_score: row.get(4)?,
            })
        })
        .ok();

    Ok(result)
}

/// Queue a telemetry payload for later sending.
pub fn queue_telemetry(db: &MetricsDb, payload_json: &str) -> Result<()> {
    let created_at = chrono::Utc::now().to_rfc3339();
    db.conn()
        .execute(
            "INSERT INTO telemetry_queue (payload_json, created_at, sent) VALUES (?1, ?2, 0)",
            rusqlite::params![payload_json, created_at],
        )
        .context("failed to queue telemetry")?;
    Ok(())
}

/// Read pending (unsent) telemetry entries.
pub fn get_pending_telemetry(db: &MetricsDb, limit: usize) -> Result<Vec<TelemetryEntry>> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT id, payload_json FROM telemetry_queue WHERE sent = 0 ORDER BY id ASC LIMIT ?1",
        )
        .context("failed to prepare pending telemetry query")?;

    let entries = stmt
        .query_map(rusqlite::params![limit as i64], |row| {
            Ok(TelemetryEntry {
                id: row.get(0)?,
                payload_json: row.get(1)?,
            })
        })
        .context("failed to query pending telemetry")?
        .filter_map(|r| r.ok())
        .collect();

    Ok(entries)
}

/// Mark telemetry entries as sent.
pub fn mark_telemetry_sent(db: &MetricsDb, ids: &[i64]) -> Result<()> {
    let conn = db.conn();
    for id in ids {
        conn.execute(
            "UPDATE telemetry_queue SET sent = 1 WHERE id = ?1",
            rusqlite::params![id],
        )
        .context("failed to mark telemetry sent")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_protocol::CriterionScore;

    fn test_db() -> MetricsDb {
        MetricsDb::open_in_memory().unwrap()
    }

    #[test]
    fn record_and_retrieve_evaluation() {
        let db = test_db();
        let scores = vec![
            CriterionScore {
                name: "Tests pass".to_string(),
                score: 8,
                threshold: 7,
                passed: true,
                findings: Some("All tests pass".to_string()),
            },
            CriterionScore {
                name: "Lint clean".to_string(),
                score: 9,
                threshold: 8,
                passed: true,
                findings: None,
            },
        ];

        let eval_id = record_evaluation(
            &db,
            ".claude/plans/test.md",
            "Test Feature",
            2,
            true,
            "claude-code",
            "claude-opus-4-6",
            Some("codex"),
            Some("gpt-5.4"),
            Some("All criteria met"),
            &scores,
        )
        .unwrap();
        assert!(eval_id > 0);

        let latest = get_latest_evaluation(&db, ".claude/plans/test.md")
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, eval_id);
        assert_eq!(latest.tier, 2);
        assert!(latest.passed);
        assert!((latest.avg_score - 8.5).abs() < 0.01);
    }

    #[test]
    fn record_evaluation_without_adversarial() {
        let db = test_db();
        let scores = vec![CriterionScore {
            name: "Build passes".to_string(),
            score: 7,
            threshold: 7,
            passed: true,
            findings: None,
        }];

        let eval_id = record_evaluation(
            &db,
            ".claude/plans/simple.md",
            "Simple Fix",
            1,
            true,
            "claude-code",
            "claude-opus-4-6",
            None,
            None,
            None,
            &scores,
        )
        .unwrap();
        assert!(eval_id > 0);
    }

    #[test]
    fn get_latest_evaluation_empty_db() {
        let db = test_db();
        let result = get_latest_evaluation(&db, "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_latest_returns_most_recent() {
        let db = test_db();
        let scores = vec![CriterionScore {
            name: "Test".to_string(),
            score: 5,
            threshold: 7,
            passed: false,
            findings: None,
        }];

        // First evaluation: fail
        record_evaluation(
            &db,
            "plan.md",
            "Feature",
            1,
            false,
            "claude-code",
            "claude-opus-4-6",
            None,
            None,
            None,
            &scores,
        )
        .unwrap();

        // Second evaluation: pass
        let scores_pass = vec![CriterionScore {
            name: "Test".to_string(),
            score: 9,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        record_evaluation(
            &db,
            "plan.md",
            "Feature",
            1,
            true,
            "claude-code",
            "claude-opus-4-6",
            None,
            None,
            None,
            &scores_pass,
        )
        .unwrap();

        let latest = get_latest_evaluation(&db, "plan.md").unwrap().unwrap();
        assert!(latest.passed);
        assert!((latest.avg_score - 9.0).abs() < 0.01);
    }

    #[test]
    fn record_loop_event_happy_path() {
        let db = test_db();
        record_loop_event(
            &db,
            "plan_created",
            Some("plan.md"),
            Some(r#"{"desc":"test"}"#),
        )
        .unwrap();

        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM loop_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn record_loop_event_without_plan_path() {
        let db = test_db();
        record_loop_event(&db, "commit", None, None).unwrap();

        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM loop_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn telemetry_queue_roundtrip() {
        let db = test_db();
        queue_telemetry(&db, r#"{"event":"test"}"#).unwrap();
        queue_telemetry(&db, r#"{"event":"test2"}"#).unwrap();

        let pending = get_pending_telemetry(&db, 10).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].payload_json, r#"{"event":"test"}"#);

        mark_telemetry_sent(&db, &[pending[0].id]).unwrap();

        let still_pending = get_pending_telemetry(&db, 10).unwrap();
        assert_eq!(still_pending.len(), 1);
        assert_eq!(still_pending[0].payload_json, r#"{"event":"test2"}"#);
    }

    #[test]
    fn telemetry_queue_empty() {
        let db = test_db();
        let pending = get_pending_telemetry(&db, 10).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn telemetry_queue_limit() {
        let db = test_db();
        for i in 0..5 {
            queue_telemetry(&db, &format!(r#"{{"n":{i}}}"#)).unwrap();
        }
        let pending = get_pending_telemetry(&db, 3).unwrap();
        assert_eq!(pending.len(), 3);
    }
}
