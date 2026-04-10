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
        let v1 = db.current_schema_version().unwrap();
        assert_eq!(v1, 1);

        // Running init again should not fail or duplicate version rows
        db.init().unwrap();
        let v2 = db.current_schema_version().unwrap();
        assert_eq!(v2, 1);
    }

    #[test]
    fn schema_version_starts_at_one() {
        let db = MetricsDb::open_in_memory().unwrap();
        assert_eq!(db.current_schema_version().unwrap(), 1);
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
        assert_eq!(db.current_schema_version().unwrap(), 1);
    }
}
