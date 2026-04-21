//! SQLite-backed index with FTS5 full-text search and sqlite-vec vector search.
//!
//! `IndexBuilder::new` opens (or creates) a database at `db_path`, runs
//! idempotent schema migrations, and records the embedder's model hash so
//! callers can detect when re-embedding is required.

use std::path::Path;
use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, params};

use super::traits::{Clock, Embedder};

pub const SCHEMA_VERSION: u32 = 1;

static VEC_INIT: OnceLock<()> = OnceLock::new();

/// Register the sqlite-vec extension as an auto-extension so every subsequent
/// `Connection::open` loads `vec0`. Idempotent and thread-safe.
fn ensure_vec_loaded() {
    VEC_INIT.get_or_init(|| unsafe {
        // The exported `sqlite3_vec_init` has a no-arg signature in the
        // `sqlite-vec` crate, but `sqlite3_auto_extension` expects the real
        // 3-arg extension init signature. Transmute via a raw pointer — this
        // matches the pattern documented by sqlite-vec's own tests.
        type ExtInit = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        let init: ExtInit =
            std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
        rusqlite::ffi::sqlite3_auto_extension(Some(init));
    });
}

pub struct IndexBuilder<'a, C: Clock, E: Embedder> {
    conn: Connection,
    #[allow(dead_code)]
    clock: &'a C,
    #[allow(dead_code)]
    embedder: &'a E,
}

impl<'a, C: Clock, E: Embedder> IndexBuilder<'a, C, E> {
    pub fn new(db_path: &Path, clock: &'a C, embedder: &'a E) -> rusqlite::Result<Self> {
        let conn = Self::open_with_vec(db_path)?;
        Self::run_migrations(&conn, embedder)?;
        Ok(Self {
            conn,
            clock,
            embedder,
        })
    }

    pub fn schema_version(&self) -> u32 {
        self.conn
            .query_row(
                "SELECT value FROM state WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// True when the stored embedder model hash differs from the current
    /// embedder's `model_hash()` — callers should drop the `chunks_vec` rows
    /// and re-embed on mismatch.
    pub fn requires_reembedding(&self) -> rusqlite::Result<bool> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM state WHERE key = 'embedder_model_hash'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(stored.as_deref() != Some(self.embedder.model_hash().as_str()))
    }

    #[allow(dead_code)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    #[allow(dead_code)]
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    fn open_with_vec(db_path: &Path) -> rusqlite::Result<Connection> {
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
                })?;
            }
        }
        ensure_vec_loaded();
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    fn run_migrations(conn: &Connection, embedder: &E) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS chunks (
                id                  TEXT PRIMARY KEY,
                source              TEXT NOT NULL,
                kind                TEXT NOT NULL,
                subtype             TEXT,
                qualified_name      TEXT,
                signature           TEXT,
                content             TEXT NOT NULL,
                source_path         TEXT NOT NULL,
                line_start          INTEGER NOT NULL,
                line_end            INTEGER NOT NULL,
                commit_sha          TEXT NOT NULL,
                indexed_at          TEXT NOT NULL,
                source_version      TEXT,
                language            TEXT,
                is_exported         INTEGER NOT NULL,
                neighboring_symbols TEXT NOT NULL,
                extractor           TEXT NOT NULL,
                confidence          REAL NOT NULL,
                source_uri          TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source);
            CREATE INDEX IF NOT EXISTS idx_chunks_kind   ON chunks(kind);
            CREATE INDEX IF NOT EXISTS idx_chunks_qname  ON chunks(qualified_name);

            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                id UNINDEXED,
                content,
                qualified_name,
                signature,
                tokenize = 'unicode61 tokenchars ''_::$'''
            );",
        )?;

        let vec_sql = format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(
                id TEXT PRIMARY KEY,
                embedding FLOAT[{}]
            )",
            embedder.dim()
        );
        conn.execute_batch(&vec_sql)?;

        conn.execute(
            "INSERT OR IGNORE INTO state(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO state(key, value) VALUES ('embedder_model_hash', ?1)",
            params![embedder.model_hash()],
        )?;

        Ok(())
    }
}
