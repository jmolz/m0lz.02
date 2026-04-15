use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Wrapper around a SQLite connection for the PICE metrics database.
/// Opens with WAL mode and runs schema migrations on startup.
pub struct MetricsDb {
    conn: Connection,
}

impl MetricsDb {
    /// Open (or create) a metrics database at the given path.
    /// Runs migrations to bring the schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open metrics database: {}", path.display()))?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Open an in-memory SQLite database with the full schema applied.
    ///
    /// Intended for tests — both this crate's own tests and downstream crates
    /// (such as `pice-cli`'s `metrics::aggregator` tests) rely on it. Not gated
    /// behind `#[cfg(test)]` because `#[cfg(test)]` is crate-local: when
    /// `pice-cli` runs its tests, `pice-daemon` is compiled as a non-test
    /// dependency and its `#[cfg(test)]` items are invisible. Keeping this
    /// function unconditionally public is the simplest way to share the
    /// test helper across crates.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory database")?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Borrow the underlying connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    fn init(&self) -> Result<()> {
        // Enable WAL mode for concurrent read access
        self.conn
            .pragma_update(None, "journal_mode", "WAL")
            .context("failed to set WAL mode")?;

        // Enable foreign-key enforcement. SQLite defaults to OFF for
        // backwards compatibility — without this, `ON DELETE CASCADE` on
        // `seam_findings` (v2) would silently not cascade. Must be set on
        // every connection; rusqlite does not persist it.
        self.conn
            .pragma_update(None, "foreign_keys", "ON")
            .context("failed to enable foreign_keys")?;

        // Create schema_version table if not exists
        self.conn
            .execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
            .context("failed to create schema_version table")?;

        let current_version = self.current_schema_version()?;
        self.run_migrations(current_version)?;

        Ok(())
    }

    fn current_schema_version(&self) -> Result<i64> {
        let mut stmt = self
            .conn
            .prepare("SELECT version FROM schema_version ORDER BY version DESC LIMIT 1")
            .context("failed to query schema_version")?;
        let version = stmt.query_row([], |row| row.get(0)).unwrap_or(0);
        Ok(version)
    }

    fn run_migrations(&self, current: i64) -> Result<()> {
        if current < 1 {
            self.migrate_v1()?;
        }
        if current < 2 {
            self.migrate_v2()?;
        }
        Ok(())
    }

    fn migrate_v1(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS evaluations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                plan_path TEXT NOT NULL,
                feature_name TEXT NOT NULL,
                tier INTEGER NOT NULL,
                passed INTEGER NOT NULL,
                primary_provider TEXT NOT NULL,
                primary_model TEXT NOT NULL,
                adversarial_provider TEXT,
                adversarial_model TEXT,
                summary TEXT,
                timestamp TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS criteria_scores (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                evaluation_id INTEGER NOT NULL REFERENCES evaluations(id),
                name TEXT NOT NULL,
                score INTEGER NOT NULL,
                threshold INTEGER NOT NULL,
                passed INTEGER NOT NULL,
                findings TEXT
            );

            CREATE TABLE IF NOT EXISTS loop_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                plan_path TEXT,
                timestamp TEXT NOT NULL,
                data_json TEXT
            );

            CREATE TABLE IF NOT EXISTS telemetry_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                payload_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                sent INTEGER NOT NULL DEFAULT 0
            );

            INSERT INTO schema_version (version) VALUES (1);
            ",
            )
            .context("failed to run v1 migration")?;
        Ok(())
    }

    /// Phase 3 — create the `seam_findings` table with CHECK constraints
    /// and FK-cascade on `evaluations`. Idempotent via `IF NOT EXISTS`.
    fn migrate_v2(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS seam_findings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                evaluation_id INTEGER NOT NULL REFERENCES evaluations(id) ON DELETE CASCADE,
                layer TEXT NOT NULL,
                boundary TEXT NOT NULL,
                check_id TEXT NOT NULL,
                category INTEGER NOT NULL CHECK(category BETWEEN 1 AND 12),
                status TEXT NOT NULL CHECK(status IN ('passed','warning','failed')),
                details TEXT,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_seam_findings_evaluation
                ON seam_findings(evaluation_id);
            CREATE INDEX IF NOT EXISTS idx_seam_findings_category
                ON seam_findings(category);

            INSERT INTO schema_version (version) VALUES (2);
            ",
            )
            .context("failed to run v2 migration")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_all_tables() {
        let db = MetricsDb::open_in_memory().unwrap();
        // Verify all four tables exist by querying their count
        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"evaluations".to_string()));
        assert!(tables.contains(&"criteria_scores".to_string()));
        assert!(tables.contains(&"loop_events".to_string()));
        assert!(tables.contains(&"telemetry_queue".to_string()));
        assert!(tables.contains(&"schema_version".to_string()));
    }

    #[test]
    fn wal_mode_is_set() {
        let db = MetricsDb::open_in_memory().unwrap();
        let mode: String = db
            .conn()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        // In-memory databases may report "memory" instead of "wal"
        // File-based DBs report "wal"
        assert!(mode == "wal" || mode == "memory");
    }

    #[test]
    fn wal_mode_on_file_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MetricsDb::open(&db_path).unwrap();
        let mode: String = db
            .conn()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn migration_is_idempotent() {
        let db = MetricsDb::open_in_memory().unwrap();
        let v = db.current_schema_version().unwrap();
        assert_eq!(v, 2);

        // Running init again should not fail or duplicate version rows
        db.init().unwrap();
        let v_again = db.current_schema_version().unwrap();
        assert_eq!(v_again, 2);
    }

    #[test]
    fn schema_version_matches_current() {
        let db = MetricsDb::open_in_memory().unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 2);
    }

    #[test]
    fn open_file_db_creates_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metrics.db");

        // Create
        {
            let _db = MetricsDb::open(&db_path).unwrap();
        }
        assert!(db_path.exists());

        // Reopen
        let db = MetricsDb::open(&db_path).unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 2);
    }

    #[test]
    fn seam_findings_table_exists_after_migration() {
        let db = MetricsDb::open_in_memory().unwrap();
        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"seam_findings".to_string()));
    }

    #[test]
    fn seam_findings_insert_and_select_roundtrip() {
        let db = MetricsDb::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO evaluations (plan_path, feature_name, tier, passed, \
                 primary_provider, primary_model, timestamp) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    "plan.md",
                    "feat",
                    2,
                    1,
                    "claude-code",
                    "opus",
                    "2026-04-15T00:00:00Z"
                ],
            )
            .unwrap();
        let evaluation_id = db.conn().last_insert_rowid();
        db.conn()
            .execute(
                "INSERT INTO seam_findings (evaluation_id, layer, boundary, check_id, \
                 category, status, details, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    evaluation_id,
                    "backend",
                    "backend↔infrastructure",
                    "config_mismatch",
                    1,
                    "failed",
                    "FOO not consumed",
                    "2026-04-15T00:00:01Z"
                ],
            )
            .unwrap();
        let row: (i64, String, String, u8, String) = db
            .conn()
            .query_row(
                "SELECT evaluation_id, boundary, check_id, category, status \
                 FROM seam_findings WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(row.0, evaluation_id);
        assert_eq!(row.1, "backend↔infrastructure");
        assert_eq!(row.2, "config_mismatch");
        assert_eq!(row.3, 1);
        assert_eq!(row.4, "failed");
    }

    #[test]
    fn seam_findings_category_check_rejects_out_of_range() {
        let db = MetricsDb::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO evaluations (plan_path, feature_name, tier, passed, \
                 primary_provider, primary_model, timestamp) \
                 VALUES ('p', 'f', 2, 1, 'x', 'y', 't')",
                [],
            )
            .unwrap();
        let eid = db.conn().last_insert_rowid();
        let err = db.conn().execute(
            "INSERT INTO seam_findings (evaluation_id, layer, boundary, check_id, \
             category, status, created_at) \
             VALUES (?, 'x', 'x', 'x', 13, 'passed', 't')",
            rusqlite::params![eid],
        );
        assert!(err.is_err(), "category=13 should fail CHECK");
    }

    #[test]
    fn seam_findings_status_check_rejects_bogus_value() {
        let db = MetricsDb::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO evaluations (plan_path, feature_name, tier, passed, \
                 primary_provider, primary_model, timestamp) \
                 VALUES ('p', 'f', 2, 1, 'x', 'y', 't')",
                [],
            )
            .unwrap();
        let eid = db.conn().last_insert_rowid();
        let err = db.conn().execute(
            "INSERT INTO seam_findings (evaluation_id, layer, boundary, check_id, \
             category, status, created_at) \
             VALUES (?, 'x', 'x', 'x', 1, 'bogus', 't')",
            rusqlite::params![eid],
        );
        assert!(err.is_err(), "status='bogus' should fail CHECK");
    }

    #[test]
    fn seam_findings_fk_cascade_on_delete() {
        let db = MetricsDb::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO evaluations (plan_path, feature_name, tier, passed, \
                 primary_provider, primary_model, timestamp) \
                 VALUES ('p', 'f', 2, 1, 'x', 'y', 't')",
                [],
            )
            .unwrap();
        let eid = db.conn().last_insert_rowid();
        db.conn()
            .execute(
                "INSERT INTO seam_findings (evaluation_id, layer, boundary, check_id, \
                 category, status, created_at) \
                 VALUES (?, 'x', 'x', 'x', 1, 'passed', 't')",
                rusqlite::params![eid],
            )
            .unwrap();
        let before: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM seam_findings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 1);
        db.conn()
            .execute("DELETE FROM evaluations WHERE id = ?", rusqlite::params![eid])
            .unwrap();
        let after: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM seam_findings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after, 0,
            "FK cascade should have deleted the seam finding"
        );
    }
}
