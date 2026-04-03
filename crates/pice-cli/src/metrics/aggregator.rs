use anyhow::{Context, Result};
use serde::Serialize;

use super::db::MetricsDb;

/// Full metrics report for `pice metrics`.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsReport {
    pub total_evaluations: u64,
    pub total_loops: u64,
    pub pass_rate: f64,
    pub avg_score: f64,
    pub last_30_days: TrendData,
    pub tier_distribution: TierDistribution,
    pub top_failing_criteria: Vec<FailingCriterion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrendData {
    pub evaluations: u64,
    pub distinct_plans: u64,
    pub pass_rate: f64,
    pub avg_score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierDistribution {
    pub tier1: u64,
    pub tier2: u64,
    pub tier3: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailingCriterion {
    pub name: String,
    pub fail_count: u64,
    pub total_count: u64,
}

/// Run all aggregation queries and return a full report.
pub fn aggregate(db: &MetricsDb) -> Result<MetricsReport> {
    let conn = db.conn();

    // Total evaluations
    let total_evaluations: u64 = conn
        .query_row("SELECT COUNT(*) FROM evaluations", [], |row| row.get(0))
        .context("failed to count evaluations")?;

    // Total distinct PICE loops: distinct plan_path values with at least one evaluation
    let total_loops: u64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT plan_path) FROM evaluations",
            [],
            |row| row.get(0),
        )
        .context("failed to count loops")?;

    // Pass rate
    let pass_rate = if total_evaluations > 0 {
        let passed: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM evaluations WHERE passed = 1",
                [],
                |row| row.get(0),
            )
            .context("failed to count passed evaluations")?;
        (passed as f64 / total_evaluations as f64) * 100.0
    } else {
        0.0
    };

    // Average score across all criteria
    let avg_score: f64 = conn
        .query_row(
            "SELECT COALESCE(AVG(CAST(score AS REAL)), 0.0) FROM criteria_scores",
            [],
            |row| row.get(0),
        )
        .context("failed to compute avg score")?;

    // Last 30 days trend
    let last_30_days = aggregate_trend(conn, 30)?;

    // Tier distribution
    let tier_distribution = aggregate_tiers(conn)?;

    // Top failing criteria (up to 5)
    let top_failing_criteria = aggregate_failing_criteria(conn, 5)?;

    Ok(MetricsReport {
        total_evaluations,
        total_loops,
        pass_rate,
        avg_score,
        last_30_days,
        tier_distribution,
        top_failing_criteria,
    })
}

fn aggregate_trend(conn: &rusqlite::Connection, days: i64) -> Result<TrendData> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    let cutoff_str = cutoff.to_rfc3339();

    let evaluations: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM evaluations WHERE timestamp >= ?1",
            rusqlite::params![cutoff_str],
            |row| row.get(0),
        )
        .context("failed to count recent evaluations")?;

    let pass_rate = if evaluations > 0 {
        let passed: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM evaluations WHERE passed = 1 AND timestamp >= ?1",
                rusqlite::params![cutoff_str],
                |row| row.get(0),
            )
            .context("failed to count recent passed evaluations")?;
        (passed as f64 / evaluations as f64) * 100.0
    } else {
        0.0
    };

    let distinct_plans: u64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT plan_path) FROM evaluations WHERE timestamp >= ?1",
            rusqlite::params![cutoff_str],
            |row| row.get(0),
        )
        .context("failed to count recent distinct plans")?;

    let avg_score: f64 = conn
        .query_row(
            "SELECT COALESCE(AVG(CAST(cs.score AS REAL)), 0.0)
             FROM criteria_scores cs
             JOIN evaluations e ON cs.evaluation_id = e.id
             WHERE e.timestamp >= ?1",
            rusqlite::params![cutoff_str],
            |row| row.get(0),
        )
        .context("failed to compute recent avg score")?;

    Ok(TrendData {
        evaluations,
        distinct_plans,
        pass_rate,
        avg_score,
    })
}

fn aggregate_tiers(conn: &rusqlite::Connection) -> Result<TierDistribution> {
    let count_tier = |tier: u8| -> Result<u64> {
        conn.query_row(
            "SELECT COUNT(*) FROM evaluations WHERE tier = ?1",
            rusqlite::params![tier],
            |row| row.get(0),
        )
        .with_context(|| format!("failed to count tier {tier} evaluations"))
    };

    Ok(TierDistribution {
        tier1: count_tier(1)?,
        tier2: count_tier(2)?,
        tier3: count_tier(3)?,
    })
}

fn aggregate_failing_criteria(
    conn: &rusqlite::Connection,
    limit: usize,
) -> Result<Vec<FailingCriterion>> {
    let mut stmt = conn
        .prepare(
            "SELECT name,
                    SUM(CASE WHEN passed = 0 THEN 1 ELSE 0 END) as fail_count,
                    COUNT(*) as total_count
             FROM criteria_scores
             GROUP BY name
             HAVING fail_count > 0
             ORDER BY fail_count DESC
             LIMIT ?1",
        )
        .context("failed to prepare failing criteria query")?;

    let criteria = stmt
        .query_map(rusqlite::params![limit as i64], |row| {
            Ok(FailingCriterion {
                name: row.get(0)?,
                fail_count: row.get(1)?,
                total_count: row.get(2)?,
            })
        })
        .context("failed to query failing criteria")?
        .filter_map(|r| r.ok())
        .collect();

    Ok(criteria)
}

/// Format a metrics report as an ASCII table for terminal display.
pub fn format_table(report: &MetricsReport) -> String {
    let mut out = String::new();
    out.push_str("PICE Metrics\n");
    out.push_str("\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\n\n");

    out.push_str(&format!(
        "Total evaluations:   {:>5}\n",
        report.total_evaluations
    ));
    out.push_str(&format!("Total PICE loops:    {:>5}\n", report.total_loops));
    out.push_str(&format!(
        "Pass rate:           {:>4.1}%\n",
        report.pass_rate
    ));
    out.push_str(&format!(
        "Average score:       {:>4.1}/10\n",
        report.avg_score
    ));

    out.push_str(&format!(
        "\nLast 30 days:\n  Evaluations:       {:>5}\n  Pass rate:         {:>4.1}%\n  Average score:     {:>4.1}/10\n",
        report.last_30_days.evaluations,
        report.last_30_days.pass_rate,
        report.last_30_days.avg_score,
    ));

    out.push_str(&format!(
        "\nTier distribution:\n  Tier 1:            {:>5}\n  Tier 2:            {:>5}\n  Tier 3:            {:>5}\n",
        report.tier_distribution.tier1,
        report.tier_distribution.tier2,
        report.tier_distribution.tier3,
    ));

    if !report.top_failing_criteria.is_empty() {
        out.push_str("\nMost common failures:\n");
        for c in &report.top_failing_criteria {
            out.push_str(&format!(
                "  {:<20} {}/{} failures\n",
                c.name, c.fail_count, c.total_count
            ));
        }
    }

    out
}

/// Escape a string for RFC 4180 CSV: wrap in quotes if it contains
/// commas, quotes, or newlines; double any internal quotes.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        format!("\"{s}\"")
    }
}

/// Export per-evaluation rows as CSV.
pub fn format_csv(db: &MetricsDb) -> Result<String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT e.id, e.plan_path, e.feature_name, e.tier, e.passed,
                    COALESCE(AVG(CAST(cs.score AS REAL)), 0.0) as avg_score, e.timestamp
             FROM evaluations e
             LEFT JOIN criteria_scores cs ON cs.evaluation_id = e.id
             GROUP BY e.id
             ORDER BY e.timestamp ASC",
        )
        .context("failed to prepare CSV export query")?;

    let mut csv = String::from("id,plan_path,feature_name,tier,passed,avg_score,timestamp\n");

    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let plan_path: String = row.get(1)?;
            let feature_name: String = row.get(2)?;
            let tier: u8 = row.get(3)?;
            let passed: i32 = row.get(4)?;
            let avg_score: f64 = row.get(5)?;
            let timestamp: String = row.get(6)?;
            Ok(format!(
                "{id},{},{},{tier},{},{avg_score:.1},{timestamp}",
                csv_escape(&plan_path),
                csv_escape(&feature_name),
                if passed != 0 { "true" } else { "false" }
            ))
        })
        .context("failed to query evaluations for CSV")?;

    for row in rows {
        csv.push_str(&row.context("failed to format CSV row")?);
        csv.push('\n');
    }

    Ok(csv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::store;
    use pice_protocol::CriterionScore;

    fn test_db() -> MetricsDb {
        MetricsDb::open_in_memory().unwrap()
    }

    fn make_scores(pass: bool) -> Vec<CriterionScore> {
        vec![CriterionScore {
            name: "Tests pass".to_string(),
            score: if pass { 8 } else { 4 },
            threshold: 7,
            passed: pass,
            findings: None,
        }]
    }

    #[test]
    fn empty_db_returns_zeroes() {
        let db = test_db();
        let report = aggregate(&db).unwrap();
        assert_eq!(report.total_evaluations, 0);
        assert_eq!(report.total_loops, 0);
        assert_eq!(report.pass_rate, 0.0);
        assert_eq!(report.avg_score, 0.0);
        assert_eq!(report.last_30_days.evaluations, 0);
        assert_eq!(report.tier_distribution.tier1, 0);
        assert!(report.top_failing_criteria.is_empty());
    }

    #[test]
    fn single_evaluation() {
        let db = test_db();
        store::record_evaluation(
            &db,
            "plan.md",
            "Feature",
            2,
            true,
            "claude",
            "opus",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();

        let report = aggregate(&db).unwrap();
        assert_eq!(report.total_evaluations, 1);
        assert_eq!(report.total_loops, 1);
        assert_eq!(report.pass_rate, 100.0);
        assert!((report.avg_score - 8.0).abs() < 0.01);
        assert_eq!(report.tier_distribution.tier2, 1);
        assert!(report.top_failing_criteria.is_empty());
    }

    #[test]
    fn mixed_pass_fail() {
        let db = test_db();
        store::record_evaluation(
            &db,
            "a.md",
            "A",
            1,
            true,
            "claude",
            "opus",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();
        store::record_evaluation(
            &db,
            "b.md",
            "B",
            2,
            false,
            "claude",
            "opus",
            None,
            None,
            None,
            &make_scores(false),
        )
        .unwrap();

        let report = aggregate(&db).unwrap();
        assert_eq!(report.total_evaluations, 2);
        assert_eq!(report.total_loops, 2);
        assert_eq!(report.pass_rate, 50.0);
        assert!((report.avg_score - 6.0).abs() < 0.01); // (8 + 4) / 2
        assert_eq!(report.tier_distribution.tier1, 1);
        assert_eq!(report.tier_distribution.tier2, 1);
    }

    #[test]
    fn failing_criteria_ranked() {
        let db = test_db();
        let scores = vec![
            CriterionScore {
                name: "Tests pass".to_string(),
                score: 4,
                threshold: 7,
                passed: false,
                findings: None,
            },
            CriterionScore {
                name: "Lint clean".to_string(),
                score: 9,
                threshold: 8,
                passed: true,
                findings: None,
            },
        ];
        store::record_evaluation(
            &db, "a.md", "A", 1, false, "claude", "opus", None, None, None, &scores,
        )
        .unwrap();

        let report = aggregate(&db).unwrap();
        assert_eq!(report.top_failing_criteria.len(), 1);
        assert_eq!(report.top_failing_criteria[0].name, "Tests pass");
        assert_eq!(report.top_failing_criteria[0].fail_count, 1);
    }

    #[test]
    fn tier_distribution() {
        let db = test_db();
        store::record_evaluation(
            &db,
            "a.md",
            "A",
            1,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();
        store::record_evaluation(
            &db,
            "b.md",
            "B",
            2,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();
        store::record_evaluation(
            &db,
            "c.md",
            "C",
            3,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();
        store::record_evaluation(
            &db,
            "d.md",
            "D",
            2,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();

        let report = aggregate(&db).unwrap();
        assert_eq!(report.tier_distribution.tier1, 1);
        assert_eq!(report.tier_distribution.tier2, 2);
        assert_eq!(report.tier_distribution.tier3, 1);
    }

    #[test]
    fn format_table_nonempty() {
        let db = test_db();
        store::record_evaluation(
            &db,
            "a.md",
            "A",
            2,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();

        let report = aggregate(&db).unwrap();
        let table = format_table(&report);
        assert!(table.contains("PICE Metrics"));
        assert!(table.contains("Total evaluations:"));
        assert!(table.contains("Pass rate:"));
    }

    #[test]
    fn format_csv_output() {
        let db = test_db();
        store::record_evaluation(
            &db,
            "plan.md",
            "Feature",
            2,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();

        let csv = format_csv(&db).unwrap();
        assert!(csv.starts_with("id,plan_path,feature_name,tier,passed,avg_score,timestamp\n"));
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2); // header + 1 row
        assert!(lines[1].contains("\"plan.md\""));
        assert!(lines[1].contains("true"));
    }

    #[test]
    fn format_csv_empty_db() {
        let db = test_db();
        let csv = format_csv(&db).unwrap();
        assert_eq!(csv.lines().count(), 1); // header only
    }

    #[test]
    fn last_30_days_includes_recent() {
        let db = test_db();
        // Records inserted now will be within the 30-day window
        store::record_evaluation(
            &db,
            "a.md",
            "A",
            1,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();

        let report = aggregate(&db).unwrap();
        assert_eq!(report.last_30_days.evaluations, 1);
        assert_eq!(report.last_30_days.distinct_plans, 1);
        assert_eq!(report.last_30_days.pass_rate, 100.0);
    }

    #[test]
    fn old_records_excluded_from_30_day_trend() {
        let db = test_db();
        // Insert a record with a timestamp 60 days ago
        let old_timestamp = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();
        db.conn()
            .execute(
                "INSERT INTO evaluations (plan_path, feature_name, tier, passed, primary_provider, primary_model, summary, timestamp)
                 VALUES ('old.md', 'Old', 1, 1, 'c', 'm', NULL, ?1)",
                rusqlite::params![old_timestamp],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO criteria_scores (evaluation_id, name, score, threshold, passed)
                 VALUES (1, 'Test', 8, 7, 1)",
                [],
            )
            .unwrap();

        let report = aggregate(&db).unwrap();
        // The old record should appear in totals but NOT in the 30-day trend
        assert_eq!(report.total_evaluations, 1);
        assert_eq!(report.last_30_days.evaluations, 0);
        assert_eq!(report.last_30_days.distinct_plans, 0);
    }

    #[test]
    fn csv_escapes_quotes_and_commas() {
        let db = test_db();
        // Feature name with embedded quotes and commas
        store::record_evaluation(
            &db,
            "plan.md",
            "Fix \"auth\" flow, part 1",
            1,
            true,
            "c",
            "m",
            None,
            None,
            None,
            &make_scores(true),
        )
        .unwrap();

        let csv = format_csv(&db).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2);
        // RFC 4180: embedded quotes are doubled, field is wrapped in quotes
        assert!(lines[1].contains("\"Fix \"\"auth\"\" flow, part 1\""));
    }
}
