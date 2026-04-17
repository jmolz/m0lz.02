use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Pass-3 Codex Medium #4 helper: classify a rusqlite error as "duplicate
/// column name" so [`MetricsDb::migrate_v3`] can swallow it idempotently.
/// SQLite returns this as `SqliteFailure(code, Some(msg))` with the message
/// literally starting with "duplicate column name:" — we match
/// case-insensitively to guard against minor version drift.
fn is_duplicate_column_error(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(_, Some(msg)) = err {
        msg.to_lowercase().contains("duplicate column name")
    } else {
        false
    }
}

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

        // Phase 4 post-adversarial-review fix (Codex High #5): concurrent
        // evaluations against the SAME project open separate `MetricsDb`
        // handles to the same SQLite file. Without `busy_timeout` set,
        // writer contention on `pass_events` would surface as an immediate
        // `SQLITE_BUSY` error, losing rows. 5s gives ample headroom for the
        // sub-ms-per-write workload and WAL's single-writer serialization.
        self.conn
            .busy_timeout(std::time::Duration::from_secs(5))
            .context("failed to set busy_timeout")?;

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
        if current < 3 {
            self.migrate_v3()?;
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

    /// Phase 4 — add adaptive-evaluation columns to `evaluations` and create
    /// the `pass_events` table for per-pass audit trails.
    ///
    /// Concurrent-first-open safe (Pass-3 Codex Medium #4 fix): the
    /// introspect-then-ALTER sequence is wrapped in `BEGIN IMMEDIATE`, which
    /// acquires a RESERVED lock before any writes and blocks a racing
    /// migrator until commit. Combined with the 5s `busy_timeout` set in
    /// `init()`, this serializes concurrent openers at the file level. The
    /// loser re-reads `current_schema_version` under the lock and no-ops if
    /// version 3 is already present.
    ///
    /// Belt-and-suspenders: ALTER errors whose message contains "duplicate
    /// column name" are tolerated idempotently. This covers the edge case
    /// where a process crashed mid-migration (committing one column, missing
    /// the version row) and a follow-up open tries to re-run.
    ///
    /// Idempotent otherwise: column adds check `PRAGMA table_info` first;
    /// the table is created with `IF NOT EXISTS`. `ON DELETE CASCADE` on
    /// `evaluation_id` deletes a pass's events when its evaluation row is
    /// deleted (matches the `seam_findings` cascade contract).
    fn migrate_v3(&self) -> Result<()> {
        // Acquire a RESERVED lock before any writes. Any concurrent opener
        // blocks here until this migrator commits, thanks to the busy_timeout
        // set in init(). Without this, two openers can both read "no columns,
        // version=2" from PRAGMA, both try ALTER, and the loser hits the
        // duplicate-column error reported in Codex Pass-2 Medium #4.
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .context("failed to begin migrate_v3 transaction")?;

        let result = self.migrate_v3_body();

        match result {
            Ok(()) => {
                self.conn
                    .execute_batch("COMMIT")
                    .context("failed to commit migrate_v3 transaction")?;
                Ok(())
            }
            Err(e) => {
                // Best-effort rollback — failure to rollback is logged but
                // the original migration error is what the caller needs to see.
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Body of the v3 migration, executed inside the `BEGIN IMMEDIATE`
    /// transaction opened by `migrate_v3`. Split for readability and so the
    /// transaction wrapper can see a single `Result` to commit / rollback on.
    fn migrate_v3_body(&self) -> Result<()> {
        // Re-read version UNDER the write lock. If another process already
        // completed v3 migration between our initial `run_migrations` check
        // and this transaction, no-op.
        let current = self.current_schema_version()?;
        if current >= 3 {
            return Ok(());
        }

        // Check which adaptive columns already exist on `evaluations`
        // (needed for idempotent re-run and for migrating v1→v3 or v2→v3
        // on databases older than current).
        let existing_cols: std::collections::HashSet<String> = self
            .conn
            .prepare("PRAGMA table_info(evaluations)")
            .context("failed to introspect evaluations columns")?
            .query_map([], |row| row.get::<_, String>(1))
            .context("failed to read evaluations columns")?
            .filter_map(|r| r.ok())
            .collect();

        // Adaptive summary columns. Each ALTER is a separate statement and
        // guarded so migrating a fresh v3 DB or re-running is a no-op.
        let adaptive_cols: &[(&str, &str)] = &[
            (
                "passes_used",
                "ALTER TABLE evaluations ADD COLUMN passes_used INTEGER",
            ),
            (
                "halted_by",
                "ALTER TABLE evaluations ADD COLUMN halted_by TEXT",
            ),
            (
                "adaptive_algorithm",
                "ALTER TABLE evaluations ADD COLUMN adaptive_algorithm TEXT",
            ),
            (
                "final_confidence",
                "ALTER TABLE evaluations ADD COLUMN final_confidence REAL",
            ),
            (
                "final_total_cost_usd",
                "ALTER TABLE evaluations ADD COLUMN final_total_cost_usd REAL",
            ),
        ];
        for (col, sql) in adaptive_cols {
            if existing_cols.contains(*col) {
                continue;
            }
            match self.conn.execute_batch(sql) {
                Ok(()) => {}
                Err(e) if is_duplicate_column_error(&e) => {
                    // A concurrent migrator added this column between our
                    // PRAGMA read and ALTER. The transaction wrapper should
                    // prevent this in practice (BEGIN IMMEDIATE serializes
                    // migrators), but belt-and-suspenders: tolerate it
                    // idempotently so a crashed partial migration does not
                    // leave the DB unrecoverable.
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e))
                        .with_context(|| format!("failed to add evaluations.{col}"));
                }
            }
        }

        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS pass_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                evaluation_id INTEGER NOT NULL REFERENCES evaluations(id) ON DELETE CASCADE,
                pass_index INTEGER NOT NULL,
                model TEXT NOT NULL,
                score REAL,
                cost_usd REAL,
                timestamp TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_pass_events_evaluation
                ON pass_events(evaluation_id);

            INSERT INTO schema_version (version) VALUES (3);
            ",
            )
            .context("failed to run v3 migration")?;
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
        assert_eq!(v, 3);

        // Running init again should not fail or duplicate version rows
        db.init().unwrap();
        let v_again = db.current_schema_version().unwrap();
        assert_eq!(v_again, 3);
    }

    #[test]
    fn schema_version_matches_current() {
        let db = MetricsDb::open_in_memory().unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 3);
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
        assert_eq!(db.current_schema_version().unwrap(), 3);
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

    // ─── Phase 4 v3 migration tests ────────────────────────────────────

    /// Fresh in-memory DB starts at v3 with all adaptive columns and the
    /// `pass_events` table present.
    #[test]
    fn v3_schema_has_adaptive_columns_and_pass_events_table() {
        let db = MetricsDb::open_in_memory().unwrap();

        // Columns on `evaluations`
        let cols: Vec<String> = db
            .conn()
            .prepare("PRAGMA table_info(evaluations)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for required in [
            "passes_used",
            "halted_by",
            "adaptive_algorithm",
            "final_confidence",
            "final_total_cost_usd",
        ] {
            assert!(
                cols.iter().any(|c| c == required),
                "evaluations must have column {required}: got {cols:?}"
            );
        }

        // `pass_events` table
        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"pass_events".to_string()));
    }

    /// Re-running `init()` on a v3 database is a no-op: schema_version stays
    /// at 3, no duplicate columns are added, and `pass_events` still exists.
    #[test]
    fn migrate_v3_is_idempotent() {
        let db = MetricsDb::open_in_memory().unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 3);

        // `init()` gates at `if current < N` so it's a proper no-op.
        db.init().unwrap();
        db.init().unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 3);

        // The gated version rows are: 1, 2, 3 — one per migration, no dupes.
        let rows: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 3, "init should never duplicate version rows");

        // Adaptive columns still exist exactly once (no ALTER ... duplicate error).
        let passes_used_count: i64 = db
            .conn()
            .prepare("PRAGMA table_info(evaluations)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .filter(|c| c == "passes_used")
            .count() as i64;
        assert_eq!(passes_used_count, 1);
    }

    /// Opening a file-DB at v1 (only migrate_v1 applied manually) and then
    /// running the full `init()` flow should migrate v1 → v2 → v3.
    #[test]
    fn migrate_from_v1_to_v3() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("v1.db");

        // Open raw connection and run only v1 to create a stale DB.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
            )
            .unwrap();
            let stub = MetricsDb { conn };
            stub.migrate_v1().unwrap();
            assert_eq!(stub.current_schema_version().unwrap(), 1);
        }

        // Open via the public API — should run v2 and v3 migrations.
        let db = MetricsDb::open(&db_path).unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 3);

        // All v3 artifacts present.
        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"seam_findings".to_string()));
        assert!(tables.contains(&"pass_events".to_string()));
    }

    /// Opening a file-DB at v2 and then running the full `init()` flow
    /// should migrate v2 → v3 without touching v2 tables.
    #[test]
    fn migrate_from_v2_to_v3() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("v2.db");

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
            )
            .unwrap();
            let stub = MetricsDb { conn };
            stub.migrate_v1().unwrap();
            stub.migrate_v2().unwrap();
            assert_eq!(stub.current_schema_version().unwrap(), 2);
        }

        let db = MetricsDb::open(&db_path).unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 3);

        // v2 seam_findings must still be there and still accepting inserts.
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
    }

    /// Inserting a row into `pass_events` with an FK to a real `evaluations`
    /// row and then deleting the evaluation cascades the pass_events row.
    #[test]
    fn pass_events_fk_cascade_on_delete() {
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
                "INSERT INTO pass_events (evaluation_id, pass_index, model, score, \
                 cost_usd, timestamp) VALUES (?, 1, 'claude-code', 9.0, 0.02, 't')",
                rusqlite::params![eid],
            )
            .unwrap();
        let before: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pass_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 1);
        db.conn()
            .execute(
                "DELETE FROM evaluations WHERE id = ?",
                rusqlite::params![eid],
            )
            .unwrap();
        let after: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pass_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0, "FK cascade should have deleted pass_events");
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
            .execute(
                "DELETE FROM evaluations WHERE id = ?",
                rusqlite::params![eid],
            )
            .unwrap();
        let after: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM seam_findings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0, "FK cascade should have deleted the seam finding");
    }

    // ─── Pass-3 regression: concurrent first-open on pre-v3 DB ────────────

    /// Phase 4 Pass-3 regression for Codex Medium #4.
    ///
    /// Earlier `migrate_v3` read `PRAGMA table_info` and then issued
    /// `ALTER TABLE ... ADD COLUMN` statements OUTSIDE a transaction. Two
    /// processes opening the same pre-v3 DB simultaneously could both
    /// observe "no columns, version=2", both attempt to ALTER, and the
    /// loser would hit a `duplicate column name` error. Since metrics-open
    /// failures are non-fatal per CLAUDE.md, this surfaced as a silent
    /// metrics drop — the losing process's evaluation never got recorded.
    ///
    /// With the Pass-3 fix, `BEGIN IMMEDIATE` acquires a RESERVED lock
    /// that serializes migrators at the file level. The loser blocks
    /// (thanks to `busy_timeout`), then re-reads schema_version under
    /// the lock and no-ops when it sees version=3. Belt-and-suspenders:
    /// `is_duplicate_column_error` tolerates duplicate-column errors in
    /// case a prior process crashed between ALTER and version insert.
    ///
    /// This test stages a v2 DB on disk, spawns two threads that both
    /// call `MetricsDb::open(&path)` on a barrier, and asserts BOTH
    /// succeed. Without the fix, one of the two would fail with
    /// `failed to add evaluations.passes_used: duplicate column name`.
    #[test]
    fn migrate_v3_survives_concurrent_first_open() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("pre_v3.db");

        // Stage a pre-v3 (v2) database: run v1 and v2 migrations, then stop.
        // Using a raw connection lets us sidestep `init()` which auto-runs v3.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "
                CREATE TABLE schema_version (version INTEGER NOT NULL);

                CREATE TABLE evaluations (
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

                CREATE TABLE criteria_scores (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    evaluation_id INTEGER NOT NULL REFERENCES evaluations(id),
                    name TEXT NOT NULL,
                    score INTEGER NOT NULL,
                    threshold INTEGER NOT NULL,
                    passed INTEGER NOT NULL,
                    findings TEXT
                );

                CREATE TABLE loop_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_type TEXT NOT NULL,
                    plan_path TEXT,
                    timestamp TEXT NOT NULL,
                    data_json TEXT
                );

                CREATE TABLE telemetry_queue (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    payload_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    sent INTEGER NOT NULL DEFAULT 0
                );

                CREATE TABLE seam_findings (
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
                CREATE INDEX idx_seam_findings_evaluation ON seam_findings(evaluation_id);
                CREATE INDEX idx_seam_findings_category ON seam_findings(category);

                INSERT INTO schema_version (version) VALUES (1);
                INSERT INTO schema_version (version) VALUES (2);
                ",
            )
            .unwrap();
        }

        // Race two MetricsDb::open calls against the staged v2 DB. Both
        // must succeed — the BEGIN IMMEDIATE wrapper in `migrate_v3`
        // serializes them, and the loser re-reads version and no-ops.
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let b = barrier.clone();
                let p = db_path.clone();
                thread::spawn(move || {
                    b.wait();
                    MetricsDb::open(&p)
                })
            })
            .collect();

        for (i, h) in handles.into_iter().enumerate() {
            let r = h.join().unwrap();
            assert!(
                r.is_ok(),
                "concurrent opener {i} failed: {:?}",
                r.as_ref().err(),
            );
        }

        // Post-race invariants: schema is at v3, all adaptive columns
        // are present exactly once, and `pass_events` table exists.
        let db = MetricsDb::open(&db_path).unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 3);

        let cols: Vec<String> = db
            .conn()
            .prepare("PRAGMA table_info(evaluations)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for required in [
            "passes_used",
            "halted_by",
            "adaptive_algorithm",
            "final_confidence",
            "final_total_cost_usd",
        ] {
            let occurrences = cols.iter().filter(|c| *c == required).count();
            assert_eq!(
                occurrences, 1,
                "column {required} should appear exactly once; got {occurrences} in {cols:?}",
            );
        }

        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"pass_events".to_string()));

        // Version rows should be 1, 2, 3 — at most one v3 row. (The
        // transaction wrapper prevents duplicates; if it regressed, we'd
        // see a row per migrator attempt.)
        let v3_rows: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM schema_version WHERE version = 3",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            v3_rows, 1,
            "concurrent migrators must not duplicate schema_version rows",
        );
    }

    /// Isolated check that the `is_duplicate_column_error` classifier
    /// matches SQLite's actual error shape. Locks the helper so a
    /// rusqlite upgrade that changes the error message can't silently
    /// break idempotency in `migrate_v3`.
    #[test]
    fn is_duplicate_column_error_matches_sqlite_message() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a INTEGER); ALTER TABLE t ADD COLUMN b INTEGER;")
            .unwrap();
        // Re-add the same column — must produce a "duplicate column name" error.
        let err = conn
            .execute_batch("ALTER TABLE t ADD COLUMN b INTEGER")
            .unwrap_err();
        assert!(
            is_duplicate_column_error(&err),
            "expected duplicate-column detection on err = {err:?}",
        );

        // Unrelated error should NOT match.
        let err = conn
            .execute_batch("ALTER TABLE nonexistent ADD COLUMN x INTEGER")
            .unwrap_err();
        assert!(
            !is_duplicate_column_error(&err),
            "no-such-table error must not be classified as duplicate-column; got {err:?}",
        );
    }
}
