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

/// A single seam finding row to insert.
#[derive(Debug, Clone)]
pub struct SeamFindingRow<'a> {
    pub layer: &'a str,
    pub boundary: &'a str,
    pub check_id: &'a str,
    pub category: u8,
    /// Lower-case wire form: `passed`, `warning`, or `failed`. The CHECK
    /// constraint on `seam_findings.status` rejects anything else.
    pub status: &'a str,
    pub details: Option<&'a str>,
}

/// Insert a seam finding attached to the given evaluation. Returns the new
/// row id. The caller is expected to be within the same transaction as the
/// evaluation insert, or to call this after `record_evaluation` returns the
/// evaluation id.
pub fn insert_seam_finding(
    db: &MetricsDb,
    evaluation_id: i64,
    finding: &SeamFindingRow<'_>,
) -> Result<i64> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    db.conn()
        .execute(
            "INSERT INTO seam_findings (evaluation_id, layer, boundary, check_id, \
             category, status, details, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                evaluation_id,
                finding.layer,
                finding.boundary,
                finding.check_id,
                finding.category,
                finding.status,
                finding.details,
                timestamp,
            ],
        )
        .context("failed to insert seam finding")?;
    Ok(db.conn().last_insert_rowid())
}

/// Insert an evaluation header with placeholder values for fields that are
/// only known after the adaptive loop completes. Returns the new row id so
/// the caller can attach `pass_events`, `seam_findings`, and (later)
/// finalize the adaptive summary via [`finalize_evaluation`].
///
/// The `passed` column is seeded to 0 and `summary` to NULL — both are
/// rewritten in `finalize_evaluation`. This split lets the adaptive loop
/// write per-pass rows BEFORE the loop halts (the crash-safety invariant
/// called out in the Phase 4 plan).
#[allow(clippy::too_many_arguments)]
pub fn insert_evaluation_header(
    db: &MetricsDb,
    plan_path: &str,
    feature_name: &str,
    tier: u8,
    primary_provider: &str,
    primary_model: &str,
    adversarial_provider: Option<&str>,
    adversarial_model: Option<&str>,
) -> Result<i64> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    db.conn()
        .execute(
            "INSERT INTO evaluations (plan_path, feature_name, tier, passed, \
             primary_provider, primary_model, adversarial_provider, adversarial_model, \
             summary, timestamp) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, NULL, ?8)",
            rusqlite::params![
                plan_path,
                feature_name,
                tier,
                primary_provider,
                primary_model,
                adversarial_provider,
                adversarial_model,
                timestamp,
            ],
        )
        .context("failed to insert evaluation header")?;
    Ok(db.conn().last_insert_rowid())
}

/// Update an evaluation row with the final `passed` verdict and summary
/// string. Called after the adaptive loop completes. This UPDATE is separate
/// from [`update_evaluation_adaptive_summary`] because the existing non-
/// adaptive path in `handlers/evaluate.rs` still uses [`record_evaluation`]
/// and neither call path should pay for columns it doesn't need.
pub fn finalize_evaluation(
    db: &MetricsDb,
    evaluation_id: i64,
    passed: bool,
    summary: Option<&str>,
) -> Result<()> {
    db.conn()
        .execute(
            "UPDATE evaluations SET passed = ?1, summary = ?2 WHERE id = ?3",
            rusqlite::params![passed as i32, summary, evaluation_id],
        )
        .context("failed to finalize evaluation")?;
    Ok(())
}

/// A single pass event row to insert. Mirrors the `pass_events` table
/// schema from the v3 migration.
#[derive(Debug, Clone)]
pub struct PassEventRow<'a> {
    pub pass_index: u32,
    pub model: &'a str,
    pub score: Option<f64>,
    pub cost_usd: Option<f64>,
}

/// [`PassMetricsSink`] implementation that writes to SQLite. Errors are
/// logged via `tracing` and do not abort the adaptive loop — metrics
/// failures must never crash the CLI per the daemon rules.
///
/// Owns an `Arc<Mutex<MetricsDb>>` so the future holding the sink across
/// await points is `Send`. `MetricsDb` wraps a rusqlite `Connection` whose
/// prepared-statement cache contains `RefCell` — making it `!Sync`, so
/// holding `&MetricsDb` across an await inside a `tokio::spawn`'d task
/// won't compile. The mutex keeps the handle thread-safe while remaining
/// lock-free for the single sequential caller.
///
/// [`PassMetricsSink`]: crate::orchestrator::PassMetricsSink
pub struct DbBackedPassSink {
    pub db: std::sync::Arc<std::sync::Mutex<MetricsDb>>,
    pub evaluation_id: i64,
}

impl crate::orchestrator::PassMetricsSink for DbBackedPassSink {
    fn record_pass(
        &self,
        pass_index: u32,
        model: &str,
        score: Option<f64>,
        cost_usd: Option<f64>,
    ) -> anyhow::Result<()> {
        let row = PassEventRow {
            pass_index,
            model,
            score,
            cost_usd,
        };
        // Mutex poisoning is recoverable — a prior panic elsewhere left the
        // mutex poisoned but the DB state is still valid for writes.
        let guard = self.db.lock().unwrap_or_else(|p| p.into_inner());
        // Phase 4.1 Pass-6 Codex High #3: insert errors propagate to the
        // adaptive loop, which turns them into `LayerStatus::Failed` via
        // `LayerAdaptiveResult::RuntimeError`. We still emit a structured
        // warn log with the evaluation_id + pass_index so operators have
        // the same forensic breadcrumb the old fail-open path produced —
        // just with a non-swallowing exit path attached.
        if let Err(e) = insert_pass_event(&guard, self.evaluation_id, &row) {
            tracing::warn!(
                evaluation_id = self.evaluation_id,
                pass_index,
                model,
                "failed to persist pass_event: {e}"
            );
            return Err(e);
        }
        Ok(())
    }
}

/// Insert a pass event attached to the given evaluation. Returns the new row
/// id. Called by the adaptive loop BEFORE the halt-decision check for pass
/// `pass_index` — this guarantees budget-halted passes still have their
/// triggering cost persisted. The caller passes the evaluation id returned
/// by `record_evaluation` or by a prior `insert_pass_event`.
pub fn insert_pass_event(
    db: &MetricsDb,
    evaluation_id: i64,
    event: &PassEventRow<'_>,
) -> Result<i64> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    db.conn()
        .execute(
            "INSERT INTO pass_events (evaluation_id, pass_index, model, score, cost_usd, timestamp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                evaluation_id,
                event.pass_index,
                event.model,
                event.score,
                event.cost_usd,
                timestamp,
            ],
        )
        .context("failed to insert pass event")?;
    Ok(db.conn().last_insert_rowid())
}

/// Atomically finalize an evaluation row AND populate all adaptive summary
/// columns in a single UPDATE. Phase 4 Pass-5 Claude Evaluator B Critical
/// fix (money-disappearance under partial-state halt).
///
/// Before this function existed, the handler called [`finalize_evaluation`]
/// (sets `passed` + `summary`) and then [`update_evaluation_adaptive_summary`]
/// (sets `passes_used` + `halted_by` + `adaptive_algorithm` +
/// `final_confidence` + `final_total_cost_usd`) as two separate UPDATEs. A
/// SIGKILL between them left the row with `passed` + `summary` populated
/// but `final_total_cost_usd = NULL` — defeating the Criterion 16 cost-
/// reconciliation invariant:
///
/// ```text
/// SELECT evaluation_id,
///        (SUM(cost_usd) - (SELECT final_total_cost_usd
///                          FROM evaluations
///                          WHERE id = evaluation_id)) AS diff
/// FROM pass_events
/// GROUP BY evaluation_id
/// HAVING ABS(diff) > 1e-9
/// ```
///
/// With a NULL `final_total_cost_usd`, `ABS(NULL - SUM) > 1e-9` evaluates
/// to NULL (not TRUE), so the row is silently dropped from the HAVING
/// output and the "money disappeared" invariant escapes detection. See the
/// test `partial_state_halt_is_detectable_via_coalesce_sql` for the
/// defense-in-depth check at the SQL layer.
///
/// The two legacy functions remain for callers that only need to update
/// one facet (e.g. tests, future bulk-update paths); production handlers
/// should prefer this combined variant.
#[allow(clippy::too_many_arguments)]
pub fn finalize_evaluation_with_adaptive_summary(
    db: &MetricsDb,
    evaluation_id: i64,
    passed: bool,
    summary: Option<&str>,
    passes_used: u32,
    halted_by: Option<&str>,
    adaptive_algorithm: Option<&str>,
    final_confidence: Option<f64>,
    final_total_cost_usd: Option<f64>,
) -> Result<()> {
    db.conn()
        .execute(
            "UPDATE evaluations SET \
                passed = ?1, \
                summary = ?2, \
                passes_used = ?3, \
                halted_by = ?4, \
                adaptive_algorithm = ?5, \
                final_confidence = ?6, \
                final_total_cost_usd = ?7 \
             WHERE id = ?8",
            rusqlite::params![
                passed as i32,
                summary,
                passes_used,
                halted_by,
                adaptive_algorithm,
                final_confidence,
                final_total_cost_usd,
                evaluation_id,
            ],
        )
        .context("failed to finalize evaluation with adaptive summary")?;
    Ok(())
}

/// Populate the adaptive summary columns on an existing `evaluations` row.
/// Called at the end of the adaptive loop once the per-layer outcome is
/// known. `adaptive_algorithm` is the snake_case wire form of the enum
/// variant (e.g. `"bayesian_sprt"`, `"none"`); `halted_by` is the
/// [`pice_core::adaptive::HaltReason`] wire form or a seam-prefixed string
/// (e.g. `"seam:config_mismatch"`).
///
/// Production handlers should prefer
/// [`finalize_evaluation_with_adaptive_summary`] which fuses this UPDATE
/// with [`finalize_evaluation`] into a single atomic write, closing the
/// SIGKILL-between-writes window documented on that function.
#[allow(clippy::too_many_arguments)]
pub fn update_evaluation_adaptive_summary(
    db: &MetricsDb,
    evaluation_id: i64,
    passes_used: u32,
    halted_by: Option<&str>,
    adaptive_algorithm: Option<&str>,
    final_confidence: Option<f64>,
    final_total_cost_usd: Option<f64>,
) -> Result<()> {
    db.conn()
        .execute(
            "UPDATE evaluations SET \
                passes_used = ?1, \
                halted_by = ?2, \
                adaptive_algorithm = ?3, \
                final_confidence = ?4, \
                final_total_cost_usd = ?5 \
             WHERE id = ?6",
            rusqlite::params![
                passes_used,
                halted_by,
                adaptive_algorithm,
                final_confidence,
                final_total_cost_usd,
                evaluation_id,
            ],
        )
        .context("failed to update evaluation adaptive summary")?;
    Ok(())
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

    // ─── Phase 4 pass_events & adaptive summary tests ─────────────────

    /// Insert a pass event and read it back through a raw SELECT. Proves the
    /// column mapping matches and NULLable fields round-trip correctly.
    #[test]
    fn pass_event_insert_and_read_roundtrip() {
        let db = test_db();
        let scores = vec![CriterionScore {
            name: "t".to_string(),
            score: 8,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        let eval_id = record_evaluation(
            &db,
            "plan.md",
            "feature",
            2,
            true,
            "claude-code",
            "opus",
            None,
            None,
            None,
            &scores,
        )
        .unwrap();

        let row_id = insert_pass_event(
            &db,
            eval_id,
            &PassEventRow {
                pass_index: 1,
                model: "claude-sonnet-4",
                score: Some(9.25),
                cost_usd: Some(0.0123),
            },
        )
        .unwrap();
        assert!(row_id > 0);

        let (evid, pi, model, score, cost): (i64, i64, String, f64, f64) = db
            .conn()
            .query_row(
                "SELECT evaluation_id, pass_index, model, score, cost_usd \
                 FROM pass_events WHERE id = ?",
                rusqlite::params![row_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(evid, eval_id);
        assert_eq!(pi, 1);
        assert_eq!(model, "claude-sonnet-4");
        assert!((score - 9.25).abs() < 1e-12);
        assert!((cost - 0.0123).abs() < 1e-12);
    }

    /// NULL score + NULL cost must round-trip without type coercion errors.
    #[test]
    fn pass_event_null_score_and_cost_roundtrip() {
        let db = test_db();
        let scores = vec![CriterionScore {
            name: "t".to_string(),
            score: 8,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        let eval_id = record_evaluation(
            &db, "p.md", "f", 1, true, "x", "y", None, None, None, &scores,
        )
        .unwrap();
        insert_pass_event(
            &db,
            eval_id,
            &PassEventRow {
                pass_index: 1,
                model: "m",
                score: None,
                cost_usd: None,
            },
        )
        .unwrap();

        let (score, cost): (Option<f64>, Option<f64>) = db
            .conn()
            .query_row(
                "SELECT score, cost_usd FROM pass_events WHERE evaluation_id = ?",
                rusqlite::params![eval_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(score.is_none());
        assert!(cost.is_none());
    }

    /// Deleting an evaluation cascades to pass_events via FK.
    ///
    /// Uses a raw SQL insert (not `record_evaluation`) because
    /// `criteria_scores.evaluation_id` is a plain FK without CASCADE — a
    /// future v3 migration could add cascade there too, but for now we
    /// exercise the pass_events cascade in isolation.
    #[test]
    fn pass_events_cascade_delete() {
        let db = test_db();
        db.conn()
            .execute(
                "INSERT INTO evaluations (plan_path, feature_name, tier, passed, \
                 primary_provider, primary_model, timestamp) \
                 VALUES ('p.md', 'f', 1, 1, 'x', 'y', 't')",
                [],
            )
            .unwrap();
        let eval_id = db.conn().last_insert_rowid();
        for i in 1..=3 {
            insert_pass_event(
                &db,
                eval_id,
                &PassEventRow {
                    pass_index: i,
                    model: "m",
                    score: Some(f64::from(i) + 5.0),
                    cost_usd: Some(0.01),
                },
            )
            .unwrap();
        }
        let before: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pass_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 3);

        db.conn()
            .execute(
                "DELETE FROM evaluations WHERE id = ?",
                rusqlite::params![eval_id],
            )
            .unwrap();
        let after: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pass_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0, "FK cascade should clear pass_events");
    }

    /// Updating the adaptive summary columns on a known evaluation. Default
    /// columns are NULL until this runs.
    #[test]
    fn update_evaluation_adaptive_summary_populates_columns() {
        let db = test_db();
        let scores = vec![CriterionScore {
            name: "t".to_string(),
            score: 8,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        let eval_id = record_evaluation(
            &db, "p.md", "f", 2, true, "x", "y", None, None, None, &scores,
        )
        .unwrap();

        // Before the update, all adaptive columns should be NULL.
        type AdaptiveRow = (
            Option<u32>,
            Option<String>,
            Option<String>,
            Option<f64>,
            Option<f64>,
        );
        let (pu, hb, algo, conf, cost): AdaptiveRow = db
            .conn()
            .query_row(
                "SELECT passes_used, halted_by, adaptive_algorithm, final_confidence, \
                 final_total_cost_usd FROM evaluations WHERE id = ?",
                rusqlite::params![eval_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert!(pu.is_none());
        assert!(hb.is_none());
        assert!(algo.is_none());
        assert!(conf.is_none());
        assert!(cost.is_none());

        update_evaluation_adaptive_summary(
            &db,
            eval_id,
            4,
            Some("sprt_confidence_reached"),
            Some("bayesian_sprt"),
            Some(0.951),
            Some(0.089),
        )
        .unwrap();

        let (pu, hb, algo, conf, cost): (u32, String, String, f64, f64) = db
            .conn()
            .query_row(
                "SELECT passes_used, halted_by, adaptive_algorithm, final_confidence, \
                 final_total_cost_usd FROM evaluations WHERE id = ?",
                rusqlite::params![eval_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(pu, 4);
        assert_eq!(hb, "sprt_confidence_reached");
        assert_eq!(algo, "bayesian_sprt");
        assert!((conf - 0.951).abs() < 1e-12);
        assert!((cost - 0.089).abs() < 1e-12);
    }

    // Phase 4 Pass-5 Claude Evaluator B Critical #4 — the atomic-finalize
    // variant must populate every summary column in one UPDATE. This is the
    // defense against the SIGKILL-mid-handler failure mode that previously
    // silently passed the reconciliation SQL.
    #[test]
    fn finalize_with_adaptive_summary_writes_all_columns_atomically() {
        let db = test_db();
        let scores = vec![CriterionScore {
            name: "t".to_string(),
            score: 8,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        let eval_id = record_evaluation(
            &db, "p.md", "f", 2, false, "x", "y", None, None, None, &scores,
        )
        .unwrap();

        finalize_evaluation_with_adaptive_summary(
            &db,
            eval_id,
            true,
            Some("stack-loops"),
            3,
            Some("sprt_confidence_reached"),
            Some("bayesian_sprt"),
            Some(0.94),
            Some(0.06),
        )
        .unwrap();

        type FullRow = (i64, Option<String>, u32, String, String, f64, f64);
        let row: FullRow = db
            .conn()
            .query_row(
                "SELECT passed, summary, passes_used, halted_by, adaptive_algorithm, \
                 final_confidence, final_total_cost_usd FROM evaluations WHERE id = ?",
                rusqlite::params![eval_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, 1);
        assert_eq!(row.1.as_deref(), Some("stack-loops"));
        assert_eq!(row.2, 3);
        assert_eq!(row.3, "sprt_confidence_reached");
        assert_eq!(row.4, "bayesian_sprt");
        assert!((row.5 - 0.94).abs() < 1e-12);
        assert!((row.6 - 0.06).abs() < 1e-12);
    }

    // Phase 4 Pass-5 Claude Evaluator B Critical #4 SQL defense-in-depth:
    // the contract's reconciliation query (Criterion 16) must detect a
    // partial-state halt (pass_events present but final_total_cost_usd = NULL).
    // The original SQL used `ABS(SUM - final_total_cost_usd) > 1e-9` which
    // silently drops NULL rows. With `COALESCE(final_total_cost_usd, -1.0)`,
    // a NULL slot yields a large diff → row surfaces in the HAVING output.
    #[test]
    fn partial_state_halt_is_detectable_via_coalesce_sql() {
        let db = test_db();
        let scores = vec![CriterionScore {
            name: "t".to_string(),
            score: 8,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        // Record an evaluation header without final_total_cost_usd, then
        // insert pass_events with real costs. This simulates the SIGKILL-
        // mid-handler state where the combined UPDATE never ran.
        let eval_id = record_evaluation(
            &db, "p.md", "f", 2, false, "x", "y", None, None, None, &scores,
        )
        .unwrap();
        for (i, cost) in [0.02f64, 0.03, 0.04].iter().enumerate() {
            insert_pass_event(
                &db,
                eval_id,
                &PassEventRow {
                    pass_index: (i + 1) as u32,
                    model: "stub",
                    score: Some(8.0),
                    cost_usd: Some(*cost),
                },
            )
            .unwrap();
        }

        // Naive reconciliation SQL (the original contract formulation):
        // HAVING ABS(SUM - final_total_cost_usd) > 1e-9 silently drops NULL.
        let naive_rows: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM (
                SELECT evaluation_id,
                       (SUM(cost_usd) - (SELECT final_total_cost_usd FROM evaluations WHERE id = evaluation_id)) AS diff
                FROM pass_events
                GROUP BY evaluation_id
                HAVING ABS(diff) > 1e-9
             )",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(
            naive_rows, 0,
            "naive SQL silently misses partial-state halts (baseline)"
        );

        // COALESCE-hardened SQL surfaces the partial-state row. Picking a
        // sentinel that cannot equal any real cost (costs are >= 0) makes
        // the diff blow up past any sane tolerance.
        let coalesce_rows: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM (
                SELECT evaluation_id,
                       (SUM(cost_usd) - COALESCE((SELECT final_total_cost_usd FROM evaluations WHERE id = evaluation_id), -1.0)) AS diff
                FROM pass_events
                GROUP BY evaluation_id
                HAVING ABS(diff) > 1e-9
             )",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(
            coalesce_rows, 1,
            "COALESCE-hardened SQL must surface the partial-state halt"
        );

        // Now run the atomic finalize — the partial state resolves, and both
        // naive AND coalesce SQL should see zero unreconciled rows.
        finalize_evaluation_with_adaptive_summary(
            &db,
            eval_id,
            true,
            Some("stack-loops"),
            3,
            Some("sprt_confidence_reached"),
            Some("bayesian_sprt"),
            Some(0.94),
            Some(0.09),
        )
        .unwrap();

        let after_rows: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM (
                SELECT evaluation_id,
                       (SUM(cost_usd) - COALESCE((SELECT final_total_cost_usd FROM evaluations WHERE id = evaluation_id), -1.0)) AS diff
                FROM pass_events
                GROUP BY evaluation_id
                HAVING ABS(diff) > 1e-9
             )",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(
            after_rows, 0,
            "atomic finalize must reconcile SUM(pass_events.cost_usd) with final_total_cost_usd"
        );
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

    /// Phase 5 cohort parallelism — contract criterion #4: the SQLite
    /// `PassMetricsSink` must tolerate `record_pass` calls from multiple
    /// cohort tasks simultaneously without losing writes or corrupting
    /// state. Spawns 4 tokio tasks × 250 `record_pass` calls (= 1000
    /// total) against a shared `Arc<DbBackedPassSink>`; asserts the
    /// final `pass_events` row count is exactly 1000. The interior
    /// `Mutex<MetricsDb>` serializes INSERTs while the `Arc` allows
    /// every cohort task to hold its own cheap clone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn db_backed_pass_sink_concurrent_record_no_lost_writes() {
        use crate::orchestrator::PassMetricsSink;
        use std::sync::Arc;

        let db = MetricsDb::open_in_memory().unwrap();
        let scores = vec![CriterionScore {
            name: "concurrent".to_string(),
            score: 8,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        let eval_id = record_evaluation(
            &db,
            ".claude/plans/concurrent.md",
            "Concurrent",
            2,
            true,
            "stub",
            "stub-model",
            None,
            None,
            None,
            &scores,
        )
        .unwrap();

        let db_arc = Arc::new(std::sync::Mutex::new(db));
        let sink = Arc::new(DbBackedPassSink {
            db: Arc::clone(&db_arc),
            evaluation_id: eval_id,
        });

        const TASKS: u32 = 4;
        const CALLS_PER_TASK: u32 = 250;
        let mut handles = Vec::with_capacity(TASKS as usize);
        for task_idx in 0..TASKS {
            // Cast per-task to Arc<dyn PassMetricsSink> to prove the
            // trait object is what cohort tasks actually hold.
            let sink_dyn: Arc<dyn PassMetricsSink> = Arc::clone(&sink) as Arc<dyn PassMetricsSink>;
            handles.push(tokio::spawn(async move {
                for i in 0..CALLS_PER_TASK {
                    // Unique pass_index per insert across all tasks —
                    // lets us detect duplicate writes or dropped ones.
                    let pass_index = task_idx * CALLS_PER_TASK + i;
                    sink_dyn
                        .record_pass(pass_index, "stub", Some(8.0), Some(0.001))
                        .expect("record_pass must not fail under concurrency");
                }
            }));
        }
        for h in handles {
            h.await.expect("task panicked");
        }

        let db_guard = db_arc.lock().unwrap();
        let count: i64 = db_guard
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM pass_events WHERE evaluation_id = ?1",
                [eval_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count,
            (TASKS * CALLS_PER_TASK) as i64,
            "expected {} total pass_event rows (4 tasks × 250 calls); got {count} — \
             indicates lost writes under concurrent record_pass",
            TASKS * CALLS_PER_TASK
        );
    }

    // ── Phase 6 — gate_decisions writer + reader tests ────────────────

    fn sample_row<'a>() -> GateDecisionRow<'a> {
        GateDecisionRow {
            gate_id: "g1",
            feature_id: "feat",
            layer: "infra",
            trigger_expression: "layer == infra",
            decision: "approve",
            reviewer: Some("jacob"),
            reason: None,
            requested_at: "2026-04-20T00:00:00Z",
            decided_at: "2026-04-20T00:05:00Z",
            elapsed_seconds: 300,
        }
    }

    #[test]
    fn insert_gate_decision_roundtrip() {
        let db = MetricsDb::open_in_memory().unwrap();
        let id = insert_gate_decision(&db, &sample_row()).unwrap();
        assert!(id > 0);

        let rows = query_gate_decisions(&db, &GateDecisionsFilter::default()).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.gate_id, "g1");
        assert_eq!(r.feature_id, "feat");
        assert_eq!(r.decision, "approve");
        assert_eq!(r.reviewer.as_deref(), Some("jacob"));
        assert_eq!(r.elapsed_seconds, 300);
    }

    #[test]
    fn insert_gate_decision_duplicate_surfaces_as_gate_conflict() {
        // Second insert on the same gate_id must hit the UNIQUE
        // constraint and return `GateInsertError::DuplicateGateId` —
        // the ReviewGate::Decide handler relies on this to produce
        // `ExitJsonStatus::ReviewGateConflict` without inspecting
        // SQLite error strings.
        let db = MetricsDb::open_in_memory().unwrap();
        insert_gate_decision(&db, &sample_row()).unwrap();
        let err = insert_gate_decision(&db, &sample_row()).unwrap_err();
        assert!(matches!(
            err,
            GateInsertError::DuplicateGateId { ref gate_id } if gate_id == "g1"
        ));
    }

    #[test]
    fn query_gate_decisions_filters_by_feature() {
        let db = MetricsDb::open_in_memory().unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "g1",
                ..sample_row()
            },
        )
        .unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "g2",
                feature_id: "other",
                ..sample_row()
            },
        )
        .unwrap();

        let rows = query_gate_decisions(
            &db,
            &GateDecisionsFilter {
                feature_id: Some("feat".to_string()),
                ..GateDecisionsFilter::default()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "feat");
    }

    #[test]
    fn query_gate_decisions_filters_by_since() {
        let db = MetricsDb::open_in_memory().unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "g1",
                requested_at: "2026-01-01T00:00:00Z",
                ..sample_row()
            },
        )
        .unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "g2",
                requested_at: "2026-04-01T00:00:00Z",
                ..sample_row()
            },
        )
        .unwrap();

        let rows = query_gate_decisions(
            &db,
            &GateDecisionsFilter {
                since: Some("2026-03-01T00:00:00Z".to_string()),
                ..GateDecisionsFilter::default()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gate_id, "g2");
    }

    #[test]
    fn canonicalize_rfc3339_normalizes_plus_zero_to_z() {
        // Same instant, two encodings → one canonical form.
        assert_eq!(
            canonicalize_rfc3339("2026-04-20T12:00:00+00:00"),
            canonicalize_rfc3339("2026-04-20T12:00:00Z"),
        );
    }

    #[test]
    fn canonicalize_rfc3339_passes_through_unparseable_with_warn() {
        // Don't reject — legacy rows stay loadable.
        assert_eq!(canonicalize_rfc3339("not-a-timestamp"), "not-a-timestamp");
    }

    #[test]
    fn query_gate_decisions_since_matches_mixed_utc_encodings() {
        // Phase 6 Codex review finding #5: filter bound in `+00:00` form
        // must match rows stored in `Z` form. Without canonicalization
        // the ASCII compare '+' (0x2B) < 'Z' (0x5A) would make the row
        // sort-compare as earlier and fall outside a `+00:00` bound.
        let db = MetricsDb::open_in_memory().unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "z_form",
                requested_at: "2026-04-20T12:00:00Z",
                ..sample_row()
            },
        )
        .unwrap();
        let rows = query_gate_decisions(
            &db,
            &GateDecisionsFilter {
                since: Some("2026-04-20T12:00:00+00:00".to_string()),
                ..GateDecisionsFilter::default()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "since-filter must match across encodings");
    }

    #[test]
    fn query_gate_decisions_orders_deterministically_across_mixed_encodings() {
        // Same instant different encoding → canonical form sorts stably.
        let db = MetricsDb::open_in_memory().unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "plus_form",
                requested_at: "2026-04-20T12:00:00+00:00",
                ..sample_row()
            },
        )
        .unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "z_form",
                requested_at: "2026-04-20T12:00:01Z",
                ..sample_row()
            },
        )
        .unwrap();
        let rows = query_gate_decisions(&db, &GateDecisionsFilter::default()).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.gate_id.as_str()).collect::<Vec<_>>(),
            vec!["plus_form", "z_form"],
            "canonical Z-form sorts by instant, not lexicographic encoding"
        );
    }

    #[test]
    fn query_gate_decisions_orders_by_requested_at_ascending() {
        // Ascending order is deterministic for CSV output. A later
        // phase may add a `--reverse` flag; the default orientation
        // must stay stable.
        let db = MetricsDb::open_in_memory().unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "late",
                requested_at: "2026-05-01T00:00:00Z",
                ..sample_row()
            },
        )
        .unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: "early",
                requested_at: "2026-01-01T00:00:00Z",
                ..sample_row()
            },
        )
        .unwrap();
        let rows = query_gate_decisions(&db, &GateDecisionsFilter::default()).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.gate_id.as_str()).collect::<Vec<_>>(),
            vec!["early", "late"]
        );
    }
}

// ─── Phase 6: gate_decisions writer + reader helpers ──────────────────────
//
// Mirror the `insert_seam_finding` pattern: a `Row<'a>` struct for
// writes + a records struct for reads. The writer returns a typed
// `GateInsertError` so the `ReviewGate::Decide` handler can map the
// UNIQUE-violation branch onto `ExitJsonStatus::ReviewGateConflict`
// without string-sniffing SQLite error messages.

/// Params for a single `gate_decisions` row insert. Borrow-by-reference
/// so the caller doesn't have to clone RFC3339 strings.
#[derive(Debug, Clone)]
pub struct GateDecisionRow<'a> {
    pub gate_id: &'a str,
    pub feature_id: &'a str,
    pub layer: &'a str,
    pub trigger_expression: &'a str,
    pub decision: &'a str,
    pub reviewer: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub requested_at: &'a str,
    pub decided_at: &'a str,
    pub elapsed_seconds: i64,
}

/// Read-side row shape for `query_gate_decisions`. Owned strings so the
/// caller can drop the `MetricsDb` handle after the query returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateDecisionRecord {
    pub id: i64,
    pub gate_id: String,
    pub feature_id: String,
    pub layer: String,
    pub trigger_expression: String,
    pub decision: String,
    pub reviewer: Option<String>,
    pub reason: Option<String>,
    pub requested_at: String,
    pub decided_at: String,
    pub elapsed_seconds: i64,
}

/// Filter spec for `query_gate_decisions`. All fields are optional; an
/// empty filter returns every row.
#[derive(Debug, Clone, Default)]
pub struct GateDecisionsFilter {
    pub feature_id: Option<String>,
    /// Lower bound on `requested_at` (RFC3339). Inclusive.
    pub since: Option<String>,
    pub limit: Option<i64>,
}

/// Typed error for `insert_gate_decision`. The handler downcasts to
/// map `DuplicateGateId` → `ExitJsonStatus::ReviewGateConflict`;
/// everything else bubbles as a generic `MetricsPersistFailed`.
#[derive(Debug, thiserror::Error)]
pub enum GateInsertError {
    #[error("gate_id '{gate_id}' already has a decision row")]
    DuplicateGateId { gate_id: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Canonicalize an RFC3339 timestamp to `YYYY-MM-DDTHH:MM:SS[.fff]Z`
/// (UTC, `Z` suffix). Storing timestamps as raw TEXT with mixed
/// `+00:00` and `Z` encodings breaks lexicographic ORDER BY and
/// inclusive range filters — the same instant sorts before or after
/// depending on encoding. Parsing + re-emitting with `SecondsFormat::Secs`
/// normalizes everything to one canonical form at the write boundary.
///
/// If parsing fails, return the original string unchanged — the CHECK
/// constraints on `requested_at`/`decided_at` are NOT NULL only, not
/// format-specific, so we don't want to reject a legacy row. Log the
/// parse failure so the operator can catch drift.
///
/// Phase 6 Codex review finding #5.
pub(crate) fn canonicalize_rfc3339(s: &str) -> String {
    use chrono::{DateTime, SecondsFormat, Utc};
    match DateTime::parse_from_rfc3339(s) {
        Ok(dt) => dt
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        Err(e) => {
            tracing::warn!(
                input = %s,
                error = %e,
                "gate_decisions: non-RFC3339 timestamp stored verbatim"
            );
            s.to_string()
        }
    }
}

/// Insert a gate_decisions row. The `UNIQUE(gate_id)` constraint turns
/// a concurrent double-decide into a typed `DuplicateGateId` error at
/// the first SQLite call — no in-process CAS needed. Timestamps are
/// canonicalized to RFC3339-Z at write time so `ORDER BY requested_at`
/// and `requested_at >= ?` work correctly regardless of whether the
/// caller supplied `Z` or `+00:00`.
pub fn insert_gate_decision(
    db: &MetricsDb,
    row: &GateDecisionRow<'_>,
) -> std::result::Result<i64, GateInsertError> {
    let requested_at = canonicalize_rfc3339(row.requested_at);
    let decided_at = canonicalize_rfc3339(row.decided_at);
    let result = db.conn().execute(
        "INSERT INTO gate_decisions (gate_id, feature_id, layer, trigger_expression, \
         decision, reviewer, reason, requested_at, decided_at, elapsed_seconds) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            row.gate_id,
            row.feature_id,
            row.layer,
            row.trigger_expression,
            row.decision,
            row.reviewer,
            row.reason,
            requested_at,
            decided_at,
            row.elapsed_seconds,
        ],
    );
    match result {
        Ok(_) => Ok(db.conn().last_insert_rowid()),
        Err(e) if is_gate_id_unique_violation(&e) => Err(GateInsertError::DuplicateGateId {
            gate_id: row.gate_id.to_string(),
        }),
        Err(e) => Err(GateInsertError::Other(
            anyhow::Error::new(e).context("failed to insert gate decision"),
        )),
    }
}

/// Fetch a single `gate_decisions` row by `gate_id`. Used for crash-
/// recovery: after the audit insert succeeds but before the manifest
/// mutation lands, the process can die. On the next decide attempt the
/// UNIQUE(gate_id) violation fires; the handler then calls this helper
/// to recover the recorded decision and complete the manifest mutation
/// idempotently. Returns `Ok(None)` when no row matches — the gate_id
/// truly does not exist yet (distinct from the resumable case).
pub fn find_gate_decision_by_id(
    db: &MetricsDb,
    gate_id: &str,
) -> anyhow::Result<Option<GateDecisionRecord>> {
    use anyhow::Context;
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT id, gate_id, feature_id, layer, trigger_expression, \
             decision, reviewer, reason, requested_at, decided_at, elapsed_seconds \
             FROM gate_decisions WHERE gate_id = ?1 LIMIT 1",
        )
        .context("prepare find_gate_decision_by_id")?;
    let mut rows = stmt
        .query(rusqlite::params![gate_id])
        .context("execute find_gate_decision_by_id")?;
    if let Some(row) = rows.next().context("step find_gate_decision_by_id")? {
        Ok(Some(GateDecisionRecord {
            id: row.get(0)?,
            gate_id: row.get(1)?,
            feature_id: row.get(2)?,
            layer: row.get(3)?,
            trigger_expression: row.get(4)?,
            decision: row.get(5)?,
            reviewer: row.get(6)?,
            reason: row.get(7)?,
            requested_at: row.get(8)?,
            decided_at: row.get(9)?,
            elapsed_seconds: row.get(10)?,
        }))
    } else {
        Ok(None)
    }
}

/// Query helpers share the UNIQUE-violation classifier so downstream
/// callers can't regress by inlining their own `e.to_string().contains(...)`.
fn is_gate_id_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            libsqlite3_sys::Error {
                code: libsqlite3_sys::ErrorCode::ConstraintViolation,
                ..
            },
            Some(msg),
        ) if msg.contains("gate_decisions.gate_id")
    )
}

use rusqlite::ffi as libsqlite3_sys;

/// Read `gate_decisions` rows matching the filter, ordered by
/// `requested_at` ascending for deterministic CSV output. Passes the
/// LIMIT clause through `LIMIT ?`; an omitted limit defaults to a
/// large sentinel so the code path is uniform.
pub fn query_gate_decisions(
    db: &MetricsDb,
    filter: &GateDecisionsFilter,
) -> Result<Vec<GateDecisionRecord>> {
    // Build the WHERE clause dynamically but with placeholder bind
    // positions — all user input flows through `?N` params, never
    // string-interpolated.
    let mut sql = String::from(
        "SELECT id, gate_id, feature_id, layer, trigger_expression, decision, \
         reviewer, reason, requested_at, decided_at, elapsed_seconds \
         FROM gate_decisions WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(fid) = &filter.feature_id {
        sql.push_str(" AND feature_id = ?");
        params.push(Box::new(fid.clone()));
    }
    if let Some(since) = &filter.since {
        // Canonicalize the filter bound to match the stored form
        // (insert_gate_decision canonicalizes at write time). Without
        // this, a caller passing "2026-04-20T00:00:00+00:00" would miss
        // rows stored as "2026-04-20T00:00:00Z" and vice versa.
        sql.push_str(" AND requested_at >= ?");
        params.push(Box::new(canonicalize_rfc3339(since)));
    }
    sql.push_str(" ORDER BY requested_at ASC");
    if let Some(limit) = filter.limit {
        sql.push_str(" LIMIT ?");
        params.push(Box::new(limit));
    }

    let conn = db.conn();
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare gate_decisions query")?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| &**b).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(GateDecisionRecord {
                id: row.get(0)?,
                gate_id: row.get(1)?,
                feature_id: row.get(2)?,
                layer: row.get(3)?,
                trigger_expression: row.get(4)?,
                decision: row.get(5)?,
                reviewer: row.get(6)?,
                reason: row.get(7)?,
                requested_at: row.get(8)?,
                decided_at: row.get(9)?,
                elapsed_seconds: row.get(10)?,
            })
        })
        .context("failed to query gate_decisions")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("failed to read gate_decisions row")?);
    }
    Ok(out)
}
