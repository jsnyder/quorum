//! SQLite-backed index with FTS5 full-text search and sqlite-vec vector search.
//!
//! `IndexBuilder::new` opens (or creates) a database at `db_path`, runs
//! idempotent schema migrations, and records the embedder's model hash so
//! callers can detect when re-embedding is required.

use std::path::Path;
use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, params};

use super::traits::{Clock, Embedder};
use crate::context::store::{ChunkStore, LoadError};
use crate::context::types::Chunk;

/// Summary of a single-source rebuild.
#[derive(Debug, Default)]
pub struct RebuildReport {
    pub source: String,
    pub chunks_loaded: usize,
    pub chunks_embedded: usize,
    pub chunks_inserted: usize,
    pub prior_source_chunks_removed: usize,
    pub parse_errors: Vec<LoadError>,
}

pub const SCHEMA_VERSION: u32 = 1;

/// Pack a `Vec<f32>` as the little-endian byte blob expected by sqlite-vec's
/// `vec0` virtual table.
fn f32_vec_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

static VEC_INIT: OnceLock<()> = OnceLock::new();

/// Register the sqlite-vec extension as an auto-extension so every subsequent
/// `Connection::open` loads `vec0`. Idempotent and thread-safe.
///
/// Safety note on the transmute below: the `sqlite-vec` crate (v0.1.x)
/// declares `sqlite3_vec_init` as `extern "C" fn()` with no arguments in its
/// Rust bindings, but the underlying C symbol produced by the amalgamation
/// actually implements the standard SQLite extension entrypoint
/// `int sqlite3_vec_init(sqlite3*, char**, const sqlite3_api_routines*)`.
/// The no-arg Rust declaration is a convenience lie; the real C ABI matches
/// `ExtInit`. This is the exact pattern documented in sqlite-vec's own
/// rusqlite test (see crate `tests` module). We verify the source pointer
/// via a `cast`-then-typed-binding so any future ABI divergence (e.g. the
/// crate switches to a correct signature) surfaces as a type error rather
/// than silent UB.
fn ensure_vec_loaded() {
    type ExtInit = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3,
        *mut *mut std::os::raw::c_char,
        *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;

    VEC_INIT.get_or_init(|| unsafe {
        // Force source type: `unsafe extern "C" fn()`. If sqlite-vec ever
        // corrects the declaration to match `ExtInit`, this `as *const ()`
        // plus the transmute becomes a redundant identity cast (still sound).
        let src: unsafe extern "C" fn() = sqlite_vec::sqlite3_vec_init;
        let init: ExtInit = std::mem::transmute::<unsafe extern "C" fn(), ExtInit>(src);
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

    /// Full rebuild for a single source: truncate the source's rows in
    /// `chunks`/`chunks_fts`/`chunks_vec`, lenient-load all chunks from the
    /// jsonl, embed each, and bulk-insert. Atomic: any failure rolls back all
    /// changes made by this call.
    ///
    /// Chunks whose `source` field differs from `source_name` are rejected and
    /// counted in `parse_errors`.
    pub fn rebuild_from_jsonl(
        &mut self,
        source_name: &str,
        jsonl_path: &Path,
    ) -> anyhow::Result<RebuildReport> {
        let load = ChunkStore::load_all_lenient(jsonl_path)?;
        let mut parse_errors = load.errors;

        let (matching, mismatched): (Vec<Chunk>, Vec<Chunk>) = load
            .chunks
            .into_iter()
            .partition(|c| c.source == source_name);

        for bad in &mismatched {
            parse_errors.push(LoadError {
                line_number: 0,
                message: format!(
                    "chunk '{}' belongs to source '{}', not '{}'",
                    bad.id, bad.source, source_name
                ),
            });
        }

        let mut report = RebuildReport {
            source: source_name.to_string(),
            chunks_loaded: matching.len(),
            parse_errors,
            ..RebuildReport::default()
        };

        // Pre-embed outside the transaction so embedding failures don't force
        // a rollback of a no-op transaction. Empty content is skipped
        // defensively (validate() rejects it at ingest).
        let mut embedded: Vec<(Chunk, Vec<f32>)> = Vec::with_capacity(matching.len());
        for chunk in matching {
            if chunk.content.is_empty() {
                continue;
            }
            let vec = self.embedder.embed(&chunk.content);
            embedded.push((chunk, vec));
        }
        report.chunks_embedded = embedded.len();

        let tx = self.conn.transaction()?;

        let prior_removed = {
            let mut del_vec = tx.prepare(
                "DELETE FROM chunks_vec WHERE id IN (SELECT id FROM chunks WHERE source = ?1)",
            )?;
            del_vec.execute(params![source_name])?;

            let mut del_fts = tx.prepare(
                "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE source = ?1)",
            )?;
            del_fts.execute(params![source_name])?;

            let mut del_chunks = tx.prepare("DELETE FROM chunks WHERE source = ?1")?;
            del_chunks.execute(params![source_name])?
        };
        report.prior_source_chunks_removed = prior_removed;

        {
            let mut ins_chunk = tx.prepare(
                "INSERT INTO chunks (
                    id, source, kind, subtype, qualified_name, signature, content,
                    source_path, line_start, line_end, commit_sha, indexed_at,
                    source_version, language, is_exported, neighboring_symbols,
                    extractor, confidence, source_uri
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16, ?17, ?18, ?19
                )",
            )?;
            let mut ins_fts = tx.prepare(
                "INSERT INTO chunks_fts (id, content, qualified_name, signature)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut ins_vec = tx.prepare(
                "INSERT INTO chunks_vec(id, embedding) VALUES (?1, ?2)",
            )?;

            for (chunk, vec) in &embedded {
                let kind_str = serde_json::to_value(&chunk.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                let neighbors_json =
                    serde_json::to_string(&chunk.metadata.neighboring_symbols)?;
                let indexed_at = chunk.metadata.indexed_at.to_rfc3339();

                ins_chunk.execute(params![
                    chunk.id,
                    chunk.source,
                    kind_str,
                    chunk.subtype,
                    chunk.qualified_name,
                    chunk.signature,
                    chunk.content,
                    chunk.metadata.source_path,
                    chunk.metadata.line_range.start(),
                    chunk.metadata.line_range.end(),
                    chunk.metadata.commit_sha,
                    indexed_at,
                    chunk.metadata.source_version,
                    chunk.metadata.language,
                    i32::from(chunk.metadata.is_exported),
                    neighbors_json,
                    chunk.provenance.extractor(),
                    chunk.provenance.confidence(),
                    chunk.provenance.source_uri(),
                ])?;

                ins_fts.execute(params![
                    chunk.id,
                    chunk.content,
                    chunk.qualified_name.clone().unwrap_or_default(),
                    chunk.signature.clone().unwrap_or_default(),
                ])?;

                let bytes = f32_vec_to_le_bytes(vec);
                ins_vec.execute(params![chunk.id, bytes])?;
            }
        }

        report.chunks_inserted = embedded.len();
        tx.commit()?;
        Ok(report)
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
        // All schema DDL + initial state rows run inside one transaction so a
        // failure mid-way cannot leave the DB partially initialized.
        conn.execute("BEGIN", [])?;
        let result = (|| -> rusqlite::Result<()> {
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
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }
}
