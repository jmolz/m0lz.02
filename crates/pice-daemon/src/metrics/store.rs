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
        &mut self,
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
}
