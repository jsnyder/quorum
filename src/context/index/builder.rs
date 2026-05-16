//! SQLite-backed index with FTS5 full-text search and sqlite-vec vector search.
//!
//! `IndexBuilder::new` opens (or creates) a database at `db_path`, runs
//! idempotent schema migrations, and records the embedder's model hash so
//! callers can detect when re-embedding is required.

use std::path::Path;
use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, params};

use super::traits::{Clock, Embedder};
use crate::context::extract::fingerprint::{FINGERPRINT_DIMS, FINGERPRINT_VERSION};
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

pub const SCHEMA_VERSION: u32 = 2;

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
/// Register sqlite-vec as a process-wide auto-extension so every subsequent
/// `Connection::open*` call transparently gets the `vec0` virtual table
/// available. Callers that open the index db without going through
/// `IndexBuilder` (e.g. the read-only `quorum context query` path) must
/// invoke this before `Connection::open*` — otherwise the first SQL that
/// touches `chunks_vec` fails with `no such module: vec0`.
pub(crate) fn ensure_vec_loaded() {
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
            let mut del_struct_vec = tx.prepare(
                "DELETE FROM chunks_struct_vec WHERE chunk_id IN \
                 (SELECT id FROM chunks WHERE source = ?1)",
            )?;
            del_struct_vec.execute(params![source_name])?;

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

        Self::insert_embedded_chunks(&tx, &embedded)?;

        report.chunks_inserted = embedded.len();
        tx.commit()?;
        Ok(report)
    }

    /// Surgical update: delete chunks belonging to `changed_files`, then insert
    /// `new_chunks` (which should come from re-extracting only those files).
    /// Chunks for files NOT in `changed_files` are untouched.
    pub fn update_files(
        &mut self,
        source_name: &str,
        new_chunks: &[Chunk],
        changed_files: &std::collections::HashSet<String>,
        fingerprints: &std::collections::HashMap<String, [f32; FINGERPRINT_DIMS]>,
    ) -> anyhow::Result<RebuildReport> {
        let mut report = RebuildReport {
            source: source_name.to_string(),
            chunks_loaded: new_chunks.len(),
            ..RebuildReport::default()
        };

        // Pre-embed outside the transaction.
        let mut embedded: Vec<(Chunk, Vec<f32>)> = Vec::with_capacity(new_chunks.len());
        for chunk in new_chunks {
            if chunk.content.is_empty() {
                continue;
            }
            let vec = self.embedder.embed(&chunk.content);
            embedded.push((chunk.clone(), vec));
        }
        report.chunks_embedded = embedded.len();

        let tx = self.conn.transaction()?;

        // Delete rows for changed files across all 4 tables.
        if !changed_files.is_empty() {
            let file_vec: Vec<&str> = changed_files.iter().map(|s| s.as_str()).collect();
            let placeholders: String = (0..file_vec.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");

            for table_sql in [
                format!(
                    "DELETE FROM chunks_struct_vec WHERE chunk_id IN \
                     (SELECT id FROM chunks WHERE source = ?1 AND source_path IN ({placeholders}))"
                ),
                format!(
                    "DELETE FROM chunks_vec WHERE id IN \
                     (SELECT id FROM chunks WHERE source = ?1 AND source_path IN ({placeholders}))"
                ),
                format!(
                    "DELETE FROM chunks_fts WHERE id IN \
                     (SELECT id FROM chunks WHERE source = ?1 AND source_path IN ({placeholders}))"
                ),
                format!(
                    "DELETE FROM chunks WHERE source = ?1 AND source_path IN ({placeholders})"
                ),
            ] {
                let mut stmt = tx.prepare(&table_sql)?;
                let mut all_params: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
                all_params.push(&source_name as &dyn rusqlite::types::ToSql);
                for f in &file_vec {
                    all_params.push(f as &dyn rusqlite::types::ToSql);
                }
                stmt.execute(all_params.as_slice())?;
            }
        }

        // Insert new chunks using the shared helper.
        Self::insert_embedded_chunks(&tx, &embedded)?;

        // Insert structural fingerprints for the new chunks.
        if !fingerprints.is_empty() {
            let mut fp_stmt = tx.prepare(
                "INSERT OR REPLACE INTO chunks_struct_vec(chunk_id, structural_vec)
                 SELECT ?1, ?2
                 WHERE EXISTS (SELECT 1 FROM chunks WHERE id = ?1)",
            )?;
            for (chunk_id, vec) in fingerprints {
                let bytes = f32_vec_to_le_bytes(vec);
                fp_stmt.execute(params![chunk_id, bytes])?;
            }
        }

        report.chunks_inserted = embedded.len();
        tx.commit()?;
        Ok(report)
    }

    /// Shared INSERT logic for `chunks`, `chunks_fts`, and `chunks_vec` tables.
    fn insert_embedded_chunks(
        tx: &rusqlite::Transaction,
        embedded: &[(Chunk, Vec<f32>)],
    ) -> anyhow::Result<()> {
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
        let mut ins_vec =
            tx.prepare("INSERT INTO chunks_vec(id, embedding) VALUES (?1, ?2)")?;

        for (chunk, vec) in embedded {
            let kind_str = serde_json::to_value(&chunk.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let neighbors_json = serde_json::to_string(&chunk.metadata.neighboring_symbols)?;
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

        Ok(())
    }

    fn open_with_vec(db_path: &Path) -> rusqlite::Result<Connection> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }
        ensure_vec_loaded();
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    /// Insert a structural fingerprint vector for a chunk.
    pub fn insert_structural_fingerprint(
        &self,
        conn: &Connection,
        chunk_id: &str,
        vector: &[f32; FINGERPRINT_DIMS],
    ) -> anyhow::Result<()> {
        let bytes = f32_vec_to_le_bytes(vector);
        conn.execute(
            "INSERT INTO chunks_struct_vec(chunk_id, structural_vec) VALUES (?1, ?2)",
            params![chunk_id, bytes],
        )?;
        Ok(())
    }

    /// Batch-insert structural fingerprints using the builder's own connection.
    /// Skips chunk ids that don't exist in the chunks table. Uses a transaction
    /// for atomicity.
    pub fn insert_structural_fingerprints_batch(
        &mut self,
        fingerprints: &std::collections::HashMap<String, [f32; FINGERPRINT_DIMS]>,
    ) -> anyhow::Result<usize> {
        if fingerprints.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO chunks_struct_vec(chunk_id, structural_vec)
                 SELECT ?1, ?2
                 WHERE EXISTS (SELECT 1 FROM chunks WHERE id = ?1)",
            )?;
            for (chunk_id, vec) in fingerprints {
                let bytes = f32_vec_to_le_bytes(vec);
                count += stmt.execute(params![chunk_id, bytes])?;
            }
        }
        tx.commit()?;
        Ok(count)
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

            let struct_vec_sql = format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_struct_vec USING vec0(
                    chunk_id TEXT PRIMARY KEY,
                    structural_vec FLOAT[{FINGERPRINT_DIMS}] distance_metric=cosine
                )"
            );
            conn.execute_batch(&struct_vec_sql)?;

            conn.execute(
                "INSERT OR IGNORE INTO state(key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO state(key, value) VALUES ('embedder_model_hash', ?1)",
                params![embedder.model_hash()],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO state(key, value) VALUES ('fingerprint_version', ?1)",
                params![FINGERPRINT_VERSION],
            )?;

            // --- v1 -> v2 migration ---
            Self::migrate_v1_to_v2(conn)?;

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

    /// Migrate an existing v1 database to v2: update schema_version in state
    /// and ensure the fingerprint_version row exists. The `CREATE VIRTUAL TABLE
    /// IF NOT EXISTS` above already handles `chunks_struct_vec` idempotently.
    fn migrate_v1_to_v2(conn: &Connection) -> rusqlite::Result<()> {
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM state WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;

        if stored.as_deref() == Some("1") {
            conn.execute(
                "UPDATE state SET value = ?1 WHERE key = 'schema_version'",
                params![SCHEMA_VERSION.to_string()],
            )?;
        }
        Ok(())
    }
}

/// Run a KNN query against the `chunks_struct_vec` table, returning the
/// `(chunk_id, distance)` pairs ordered by ascending distance.
///
/// Standalone function (not on `IndexBuilder`) so callers don't need to
/// thread the builder's generic parameters through unrelated code paths.
pub fn query_structural_knn(
    conn: &Connection,
    query_vec: &[f32; FINGERPRINT_DIMS],
    k: usize,
) -> anyhow::Result<Vec<(String, f32)>> {
    if k == 0 {
        return Ok(Vec::new());
    }
    let bytes = f32_vec_to_le_bytes(query_vec);
    let mut stmt = conn.prepare(
        "SELECT chunk_id, distance
         FROM chunks_struct_vec
         WHERE structural_vec MATCH ?1
         ORDER BY distance
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![bytes, k as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?))
    })?;
    let mut results = Vec::with_capacity(k);
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::extract::fingerprint::FINGERPRINT_DIMS;
    use crate::context::index::traits::{FixedClock, HashEmbedder};
    use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};
    use std::collections::{HashMap, HashSet};

    /// Build a test chunk with the given id, source, source_path, and content.
    fn make_chunk(id: &str, source: &str, source_path: &str, content: &str) -> Chunk {
        Chunk {
            id: id.to_string(),
            source: source.to_string(),
            kind: ChunkKind::Symbol,
            subtype: None,
            qualified_name: Some(format!("test::{id}")),
            signature: None,
            content: content.to_string(),
            metadata: ChunkMeta {
                source_path: source_path.to_string(),
                line_range: LineRange::new(1, 10).unwrap(),
                commit_sha: "abc123".to_string(),
                indexed_at: chrono::Utc::now(),
                source_version: None,
                language: Some("rust".to_string()),
                is_exported: true,
                neighboring_symbols: vec![],
            },
            provenance: Provenance::new("test-extractor", 0.95, "file:///test").unwrap(),
        }
    }

    /// Helper: count chunks in the `chunks` table matching given source and source_path.
    fn count_chunks(conn: &Connection, source: &str, source_path: &str) -> usize {
        conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE source = ?1 AND source_path = ?2",
            params![source, source_path],
            |r| r.get::<_, usize>(0),
        )
        .unwrap()
    }

    /// Helper: count all chunks in the `chunks` table for a given source.
    fn count_all_chunks(conn: &Connection, source: &str) -> usize {
        conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE source = ?1",
            params![source],
            |r| r.get::<_, usize>(0),
        )
        .unwrap()
    }

    /// Helper: get a specific chunk's content by id.
    fn get_chunk_content(conn: &Connection, id: &str) -> Option<String> {
        conn.query_row(
            "SELECT content FROM chunks WHERE id = ?1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .unwrap()
    }

    /// Helper: count rows in chunks_fts for a given chunk id.
    fn fts_exists(conn: &Connection, id: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM chunks_fts WHERE id = ?1",
            params![id],
            |r| r.get::<_, usize>(0),
        )
        .unwrap()
            > 0
    }

    /// Helper: count rows in chunks_vec for a given chunk id.
    fn vec_exists(conn: &Connection, id: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM chunks_vec WHERE id = ?1",
            params![id],
            |r| r.get::<_, usize>(0),
        )
        .unwrap()
            > 0
    }

    fn setup_builder() -> IndexBuilder<'static, FixedClock, HashEmbedder> {
        // Leak the clock and embedder so they live for 'static — acceptable in tests.
        let clock: &'static FixedClock = Box::leak(Box::new(FixedClock::epoch()));
        let embedder: &'static HashEmbedder = Box::leak(Box::new(HashEmbedder::new(8)));
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();
        // Drop the tempfile so rusqlite can create the file fresh.
        drop(tmp);
        ensure_vec_loaded();
        IndexBuilder::new(&db_path, clock, embedder).unwrap()
    }

    /// Insert chunks directly into the builder's DB for test setup.
    fn insert_chunks_direct(
        builder: &mut IndexBuilder<'static, FixedClock, HashEmbedder>,
        chunks: &[Chunk],
    ) {
        let embedder = HashEmbedder::new(8);
        let mut embedded: Vec<(Chunk, Vec<f32>)> = Vec::new();
        for c in chunks {
            let vec = embedder.embed(&c.content);
            embedded.push((c.clone(), vec));
        }
        let tx = builder.conn_mut().transaction().unwrap();
        IndexBuilder::<FixedClock, HashEmbedder>::insert_embedded_chunks(&tx, &embedded).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn test_update_files_only_deletes_changed_files() {
        let mut builder = setup_builder();

        // Insert chunks for two files: src/a.rs and src/b.rs
        let chunk_a1 = make_chunk("a1", "my-source", "src/a.rs", "fn foo() {}");
        let chunk_a2 = make_chunk("a2", "my-source", "src/a.rs", "fn bar() {}");
        let chunk_b1 = make_chunk("b1", "my-source", "src/b.rs", "fn baz() {}");

        insert_chunks_direct(&mut builder, &[chunk_a1, chunk_a2, chunk_b1]);

        // Verify initial state
        assert_eq!(count_chunks(builder.conn(), "my-source", "src/a.rs"), 2);
        assert_eq!(count_chunks(builder.conn(), "my-source", "src/b.rs"), 1);

        // Create a new replacement chunk for src/a.rs
        let new_a = make_chunk("a3", "my-source", "src/a.rs", "fn foo_updated() {}");

        let mut changed = HashSet::new();
        changed.insert("src/a.rs".to_string());

        let fingerprints: HashMap<String, [f32; FINGERPRINT_DIMS]> = HashMap::new();

        let report = builder
            .update_files("my-source", &[new_a], &changed, &fingerprints)
            .unwrap();

        // Old a.rs chunks should be gone
        assert!(get_chunk_content(builder.conn(), "a1").is_none());
        assert!(get_chunk_content(builder.conn(), "a2").is_none());
        assert!(!fts_exists(builder.conn(), "a1"));
        assert!(!fts_exists(builder.conn(), "a2"));
        assert!(!vec_exists(builder.conn(), "a1"));
        assert!(!vec_exists(builder.conn(), "a2"));

        // New a.rs chunk should be present
        assert_eq!(
            get_chunk_content(builder.conn(), "a3"),
            Some("fn foo_updated() {}".to_string())
        );
        assert!(fts_exists(builder.conn(), "a3"));
        assert!(vec_exists(builder.conn(), "a3"));

        // b.rs chunk should be untouched
        assert_eq!(
            get_chunk_content(builder.conn(), "b1"),
            Some("fn baz() {}".to_string())
        );
        assert!(fts_exists(builder.conn(), "b1"));
        assert!(vec_exists(builder.conn(), "b1"));

        // Report
        assert_eq!(report.source, "my-source");
        assert_eq!(report.chunks_loaded, 1);
        assert_eq!(report.chunks_embedded, 1);
        assert_eq!(report.chunks_inserted, 1);
    }

    #[test]
    fn test_update_files_with_fingerprints() {
        let mut builder = setup_builder();

        let chunk_a = make_chunk("a1", "my-source", "src/a.rs", "fn foo() {}");
        insert_chunks_direct(&mut builder, &[chunk_a]);

        let new_a = make_chunk("a2", "my-source", "src/a.rs", "fn foo_v2() {}");
        let mut changed = HashSet::new();
        changed.insert("src/a.rs".to_string());

        let mut fingerprints: HashMap<String, [f32; FINGERPRINT_DIMS]> = HashMap::new();
        fingerprints.insert("a2".to_string(), [0.5; FINGERPRINT_DIMS]);

        let _report = builder
            .update_files("my-source", &[new_a], &changed, &fingerprints)
            .unwrap();

        // Verify structural fingerprint was inserted
        let fp_count: usize = builder
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM chunks_struct_vec WHERE chunk_id = ?1",
                params!["a2"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fp_count, 1);
    }

    #[test]
    fn test_update_files_empty_changed_set_only_inserts() {
        let mut builder = setup_builder();

        let chunk_a = make_chunk("a1", "my-source", "src/a.rs", "fn foo() {}");
        insert_chunks_direct(&mut builder, &[chunk_a]);

        // Call update_files with empty changed set — should only add, not delete.
        let new_b = make_chunk("b1", "my-source", "src/b.rs", "fn bar() {}");
        let changed: HashSet<String> = HashSet::new();
        let fingerprints: HashMap<String, [f32; FINGERPRINT_DIMS]> = HashMap::new();

        let _report = builder
            .update_files("my-source", &[new_b], &changed, &fingerprints)
            .unwrap();

        // Both should exist
        assert_eq!(count_all_chunks(builder.conn(), "my-source"), 2);
        assert!(get_chunk_content(builder.conn(), "a1").is_some());
        assert!(get_chunk_content(builder.conn(), "b1").is_some());
    }

    #[test]
    fn test_rebuild_from_jsonl_uses_shared_insert_logic() {
        // This test verifies that rebuild_from_jsonl still works correctly
        // after refactoring to use insert_embedded_chunks.
        let mut builder = setup_builder();

        // Write a JSONL file with test chunks
        let tmp_jsonl = tempfile::NamedTempFile::new().unwrap();
        let chunk = make_chunk("c1", "my-source", "src/c.rs", "fn test() {}");
        let line = serde_json::to_string(&chunk).unwrap();
        std::fs::write(tmp_jsonl.path(), format!("{line}\n")).unwrap();

        let report = builder
            .rebuild_from_jsonl("my-source", tmp_jsonl.path())
            .unwrap();

        assert_eq!(report.chunks_loaded, 1);
        assert_eq!(report.chunks_inserted, 1);
        assert_eq!(
            get_chunk_content(builder.conn(), "c1"),
            Some("fn test() {}".to_string())
        );
        assert!(fts_exists(builder.conn(), "c1"));
        assert!(vec_exists(builder.conn(), "c1"));
    }
}
