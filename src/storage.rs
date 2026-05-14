//! SQLite-backed persistent storage for quorum review and telemetry data.
//!
//! `StorageHandle` is a shared, thread-safe connection wrapper suitable for
//! passing across async boundaries. `initialize` opens (or creates) the
//! database at `<quorum_home>/quorum.db`, enables WAL journal mode, and runs
//! idempotent schema migrations keyed on `PRAGMA user_version`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::Connection;

/// Shared, thread-safe database connection handle.
///
/// Wrapped in `Arc<Mutex<_>>` so it can be cloned across pipeline stages
/// and (later) passed into async tasks that need write access.
pub type StorageHandle = Arc<Mutex<Connection>>;

/// Current schema version. Bumped by each `migrate_vN_to_vN+1` function.
const SCHEMA_VERSION: u32 = 1;

/// Open (or create) the quorum SQLite database and run any pending
/// migrations. Returns a shared connection handle ready for use.
///
/// The database file lives at `<quorum_home>/quorum.db`. The parent
/// directory is created if it does not exist.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, the database
/// cannot be opened, or a migration fails.
pub fn initialize(quorum_home: &Path) -> anyhow::Result<StorageHandle> {
    std::fs::create_dir_all(quorum_home)
        .with_context(|| format!("failed to create quorum home: {}", quorum_home.display()))?;

    let db_path = quorum_home.join("quorum.db");
    let conn = Connection::open(&db_path)
        .with_context(|| format!("failed to open database: {}", db_path.display()))?;

    // WAL mode for concurrent readers + single writer.
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("failed to set WAL journal mode")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable foreign keys")?;

    run_migrations(&conn)?;

    Ok(Arc::new(Mutex::new(conn)))
}

/// Read the current schema version from `PRAGMA user_version`.
fn current_version(conn: &Connection) -> anyhow::Result<u32> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    Ok(version)
}

/// Dispatch pending migrations in order.
fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    let version = current_version(conn)?;

    if version < 1 {
        migrate_v0_to_v1(conn).context("schema migration v0 -> v1 failed")?;
    }

    // Future migrations slot in here:
    // if version < 2 { migrate_v1_to_v2(conn)?; }

    Ok(())
}

/// Schema v1: reviews, review_finding_ids, telemetry tables.
fn migrate_v0_to_v1(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;

    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS reviews (
            run_id            TEXT PRIMARY KEY,
            timestamp         TEXT NOT NULL,
            quorum_version    TEXT NOT NULL,
            repo              TEXT,
            invoked_from      TEXT NOT NULL,
            model             TEXT NOT NULL,
            files_reviewed    INTEGER NOT NULL,
            lines_added       INTEGER,
            lines_removed     INTEGER,
            critical          INTEGER NOT NULL DEFAULT 0,
            high              INTEGER NOT NULL DEFAULT 0,
            medium            INTEGER NOT NULL DEFAULT 0,
            low               INTEGER NOT NULL DEFAULT 0,
            info              INTEGER NOT NULL DEFAULT 0,
            suppressed_by_rule TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(suppressed_by_rule)),
            tokens_in         INTEGER NOT NULL,
            tokens_out        INTEGER NOT NULL,
            tokens_cache_read INTEGER NOT NULL DEFAULT 0,
            duration_ms       INTEGER NOT NULL,
            flag_deep         INTEGER NOT NULL DEFAULT 0,
            flag_parallel_n   INTEGER NOT NULL DEFAULT 0,
            flag_ensemble     INTEGER NOT NULL DEFAULT 0,
            mode              TEXT,
            context           TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(context))
        );

        CREATE TABLE IF NOT EXISTS review_finding_ids (
            run_id      TEXT NOT NULL REFERENCES reviews(run_id),
            finding_id  TEXT NOT NULL,
            PRIMARY KEY (run_id, finding_id)
        );
        CREATE INDEX IF NOT EXISTS idx_finding_id ON review_finding_ids(finding_id);

        CREATE TABLE IF NOT EXISTS telemetry (
            id                       INTEGER PRIMARY KEY AUTOINCREMENT,
            ts                       TEXT NOT NULL,
            files                    TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(files)),
            findings                 TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(findings)),
            model                    TEXT NOT NULL,
            tokens_in                INTEGER NOT NULL,
            tokens_out               INTEGER NOT NULL,
            duration_ms              INTEGER NOT NULL,
            suppressed               INTEGER NOT NULL DEFAULT 0,
            context7_resolved        INTEGER NOT NULL DEFAULT 0,
            context7_resolve_failed  INTEGER NOT NULL DEFAULT 0,
            context7_query_failed    INTEGER NOT NULL DEFAULT 0,
            context7_skipped_popular INTEGER NOT NULL DEFAULT 0,
            context7_budget_reduced  INTEGER NOT NULL DEFAULT 0,
            fp_kind_utilization_rate REAL,
            judge_calls              INTEGER NOT NULL DEFAULT 0,
            judge_approved           INTEGER NOT NULL DEFAULT 0,
            judge_rejected           INTEGER NOT NULL DEFAULT 0,
            judge_uncertain          INTEGER NOT NULL DEFAULT 0,
            judge_skipped            INTEGER NOT NULL DEFAULT 0,
            judge_cache_hits         INTEGER NOT NULL DEFAULT 0,
            judge_latency_ms         INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_telemetry_ts ON telemetry(ts);",
    )?;

    tx.pragma_update(None, "user_version", 1)?;
    tx.commit()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn initialize_creates_tables() {
        let dir = TempDir::new().unwrap();
        let handle = initialize(dir.path()).expect("initialize should succeed");
        let conn = handle.lock().unwrap();

        // Verify user_version is 1.
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // Verify all three tables exist by querying sqlite_master.
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name IN ('reviews', 'review_finding_ids', 'telemetry')
                     ORDER BY name",
                )
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(tables, vec!["review_finding_ids", "reviews", "telemetry"]);

        // Verify the index on review_finding_ids exists.
        let idx_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_finding_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 1);

        // Verify the index on telemetry(ts) exists.
        let ts_idx_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_telemetry_ts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ts_idx_count, 1);

        // Verify WAL journal mode is active.
        let journal: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
    }

    #[test]
    fn initialize_is_idempotent() {
        let dir = TempDir::new().unwrap();

        // First call creates the schema.
        let handle1 = initialize(dir.path()).expect("first initialize should succeed");
        {
            let conn = handle1.lock().unwrap();
            let version: u32 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 1);
        }
        // Drop the first handle so the connection is closed.
        drop(handle1);

        // Second call on the same directory is a no-op (no errors, same version).
        let handle2 = initialize(dir.path()).expect("second initialize should succeed");
        let conn = handle2.lock().unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);

        // Tables still exist and are intact.
        let table_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('reviews', 'review_finding_ids', 'telemetry')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 3);
    }
}
