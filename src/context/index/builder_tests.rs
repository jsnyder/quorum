use rusqlite::Connection;
use tempfile::tempdir;

use super::builder::{IndexBuilder, SCHEMA_VERSION};
use super::traits::{Embedder, FixedClock, HashEmbedder};
use crate::context::store::ChunkStore;
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn table_names(db: &std::path::Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    conn.prepare("SELECT name FROM sqlite_master WHERE type IN ('table') ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn new_creates_db_with_expected_tables() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    let builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    assert_eq!(builder.schema_version(), SCHEMA_VERSION);
    drop(builder);

    let tables = table_names(&db);
    for required in ["chunks", "chunks_fts", "chunks_vec", "state"] {
        assert!(
            tables.iter().any(|t| t == required),
            "missing table {required}: have {tables:?}"
        );
    }
}

#[test]
fn new_is_idempotent_on_existing_db() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    {
        let b = IndexBuilder::new(&db, &clock, &emb).unwrap();
        assert_eq!(b.schema_version(), SCHEMA_VERSION);
    }
    let b2 = IndexBuilder::new(&db, &clock, &emb).unwrap();
    assert_eq!(b2.schema_version(), SCHEMA_VERSION);
    assert!(!b2.requires_reembedding().unwrap());
}

#[test]
fn model_hash_mismatch_is_detectable() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");

    let clock = FixedClock::epoch();
    {
        let emb = HashEmbedder::new(384);
        let _b = IndexBuilder::new(&db, &clock, &emb).unwrap();
    }

    // Different dim -> different model_hash. The vec0 table from the first
    // run already exists (with dim=384); IF NOT EXISTS prevents re-creation,
    // so the builder opens cleanly and state-row comparison reports mismatch.
    let emb2 = HashEmbedder::new(512);
    let b2 = IndexBuilder::new(&db, &clock, &emb2).unwrap();
    assert!(b2.requires_reembedding().unwrap());
}

#[test]
fn tokenizer_preserves_underscores_in_identifiers() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    let builder = IndexBuilder::new(&db, &clock, &emb).unwrap();

    let conn = builder.conn();
    conn.execute(
        "INSERT INTO chunks_fts(id, content, qualified_name, signature) \
         VALUES ('a', 'verify_token', '', ''), \
                ('b', 'verify token', '', '')",
        [],
    )
    .unwrap();

    let matches_verify: Vec<String> = conn
        .prepare("SELECT id FROM chunks_fts WHERE chunks_fts MATCH 'verify' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // Underscore is a tokenchar, so 'verify_token' stays one token; only 'b'
    // (which has a space separator) matches 'verify'.
    assert_eq!(matches_verify, vec!["b".to_string()]);

    let matches_full: Vec<String> = conn
        .prepare("SELECT id FROM chunks_fts WHERE chunks_fts MATCH 'verify_token' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(matches_full, vec!["a".to_string()]);
}

#[test]
fn vec0_table_created_with_correct_dim() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    let _b = IndexBuilder::new(&db, &clock, &emb).unwrap();

    let conn = Connection::open(&db).unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'chunks_vec'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        sql.contains("FLOAT[384]"),
        "expected FLOAT[384] in chunks_vec schema, got: {sql}"
    );
}

#[test]
fn builder_exposes_conn_for_downstream_writes() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    let builder = IndexBuilder::new(&db, &clock, &emb).unwrap();

    builder
        .conn()
        .execute(
            "INSERT INTO chunks(
                id, source, kind, content, source_path, line_start, line_end,
                commit_sha, indexed_at, is_exported, neighboring_symbols,
                extractor, confidence, source_uri
            ) VALUES (
                'c1', 'repo', 'function', 'fn f() {}', 'src/lib.rs', 1, 1,
                'deadbeef', '1970-01-01T00:00:00Z', 0, '[]',
                'rust-ast', 1.0, 'file://src/lib.rs#L1-L1'
            )",
            [],
        )
        .unwrap();

    let count: i64 = builder
        .conn()
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn initial_state_rows_populated() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let emb = HashEmbedder::new(384);
    let clock = FixedClock::epoch();
    let builder = IndexBuilder::new(&db, &clock, &emb).unwrap();

    let version: String = builder
        .conn()
        .query_row(
            "SELECT value FROM state WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION.to_string());

    let hash: String = builder
        .conn()
        .query_row(
            "SELECT value FROM state WHERE key = 'embedder_model_hash'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hash, emb.model_hash());
}

#[test]
fn two_different_db_paths_are_isolated() {
    let dir = tempdir().unwrap();
    let db_a = dir.path().join("a.db");
    let db_b = dir.path().join("b.db");

    let emb_a = HashEmbedder::new(384);
    let emb_b = HashEmbedder::new(256);
    let clock = FixedClock::epoch();

    let b_a = IndexBuilder::new(&db_a, &clock, &emb_a).unwrap();
    let b_b = IndexBuilder::new(&db_b, &clock, &emb_b).unwrap();

    b_a.conn()
        .execute(
            "INSERT INTO chunks(
                id, source, kind, content, source_path, line_start, line_end,
                commit_sha, indexed_at, is_exported, neighboring_symbols,
                extractor, confidence, source_uri
            ) VALUES (
                'only-a', 'repo', 'function', '', 'x', 1, 1,
                '0', '1970-01-01T00:00:00Z', 0, '[]', 'e', 1.0, 'u'
            )",
            [],
        )
        .unwrap();

    let count_a: i64 = b_a
        .conn()
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    let count_b: i64 = b_b
        .conn()
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_a, 1);
    assert_eq!(count_b, 0);

    let hash_a: String = b_a
        .conn()
        .query_row(
            "SELECT value FROM state WHERE key = 'embedder_model_hash'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let hash_b: String = b_b
        .conn()
        .query_row(
            "SELECT value FROM state WHERE key = 'embedder_model_hash'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(hash_a, hash_b);
}

// ---------------------------------------------------------------------------
// rebuild_from_jsonl tests
// ---------------------------------------------------------------------------

fn write_jsonl(path: &std::path::Path, chunks: &[Chunk]) {
    let mut store = ChunkStore::new(path);
    for c in chunks {
        store.append(c).unwrap();
    }
}

fn sample_chunk(source: &str, id: &str, content: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: Some(id.to_string()),
        signature: Some(format!("fn {id}")),
        content: content.to_string(),
        metadata: ChunkMeta {
            source_path: "src/x.rs".to_string(),
            line_range: LineRange::new(1, 1).unwrap(),
            commit_sha: "abc".to_string(),
            indexed_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
            source_version: None,
            language: Some("rust".into()),
            is_exported: true,
            neighboring_symbols: vec![],
        },
        provenance: Provenance::new("ast-grep-rust", 0.9, "src/x.rs").unwrap(),
    }
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn rebuild_inserts_chunks_into_all_three_tables() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl = dir.path().join("chunks.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    let chunks = vec![
        sample_chunk("mini-rust", "a", "fn a() {}"),
        sample_chunk("mini-rust", "b", "fn b() {}"),
        sample_chunk("mini-rust", "c", "fn c() {}"),
    ];
    write_jsonl(&jsonl, &chunks);

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    let report = builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();

    assert_eq!(report.chunks_loaded, 3);
    assert_eq!(report.chunks_embedded, 3);
    assert_eq!(report.chunks_inserted, 3);
    assert_eq!(report.prior_source_chunks_removed, 0);
    assert!(report.parse_errors.is_empty());

    let conn = builder.conn();
    assert_eq!(count(conn, "SELECT count(*) FROM chunks"), 3);
    assert_eq!(count(conn, "SELECT count(*) FROM chunks_fts"), 3);
    assert_eq!(count(conn, "SELECT count(*) FROM chunks_vec"), 3);
}

#[test]
fn rebuild_replaces_prior_source_chunks_atomically() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl1 = dir.path().join("v1.jsonl");
    let jsonl2 = dir.path().join("v2.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    write_jsonl(
        &jsonl1,
        &[
            sample_chunk("mini-rust", "a", "fn a() {}"),
            sample_chunk("mini-rust", "b", "fn b() {}"),
            sample_chunk("mini-rust", "c", "fn c() {}"),
        ],
    );
    write_jsonl(
        &jsonl2,
        &[
            sample_chunk("mini-rust", "x", "fn x() {}"),
            sample_chunk("mini-rust", "y", "fn y() {}"),
        ],
    );

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    builder.rebuild_from_jsonl("mini-rust", &jsonl1).unwrap();
    let r2 = builder.rebuild_from_jsonl("mini-rust", &jsonl2).unwrap();

    assert_eq!(r2.prior_source_chunks_removed, 3);

    let conn = builder.conn();
    assert_eq!(
        count(
            conn,
            "SELECT count(*) FROM chunks WHERE source = 'mini-rust'"
        ),
        2
    );
    for gone in ["a", "b", "c"] {
        let present: i64 = conn
            .query_row("SELECT count(*) FROM chunks WHERE id = ?1", [gone], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(present, 0, "chunk {gone} should have been removed");
    }
}

#[test]
fn rebuild_preserves_other_sources() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl_a = dir.path().join("a.jsonl");
    let jsonl_b = dir.path().join("b.jsonl");
    let jsonl_a2 = dir.path().join("a2.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    write_jsonl(
        &jsonl_a,
        &[
            sample_chunk("A", "a1", "fn a1() {}"),
            sample_chunk("A", "a2", "fn a2() {}"),
        ],
    );
    write_jsonl(
        &jsonl_b,
        &[
            sample_chunk("B", "b1", "fn b1() {}"),
            sample_chunk("B", "b2", "fn b2() {}"),
            sample_chunk("B", "b3", "fn b3() {}"),
        ],
    );
    write_jsonl(&jsonl_a2, &[sample_chunk("A", "a_only", "fn a_only() {}")]);

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    builder.rebuild_from_jsonl("A", &jsonl_a).unwrap();
    builder.rebuild_from_jsonl("B", &jsonl_b).unwrap();
    builder.rebuild_from_jsonl("A", &jsonl_a2).unwrap();

    let conn = builder.conn();
    assert_eq!(
        count(conn, "SELECT count(*) FROM chunks WHERE source = 'A'"),
        1
    );
    assert_eq!(
        count(conn, "SELECT count(*) FROM chunks WHERE source = 'B'"),
        3
    );
    assert_eq!(count(conn, "SELECT count(*) FROM chunks_vec"), 4);
    assert_eq!(count(conn, "SELECT count(*) FROM chunks_fts"), 4);
}

#[test]
fn rebuild_lenient_handles_malformed_lines() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl = dir.path().join("chunks.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    write_jsonl(
        &jsonl,
        &[
            sample_chunk("mini-rust", "a", "fn a() {}"),
            sample_chunk("mini-rust", "b", "fn b() {}"),
        ],
    );
    // Append a garbage line.
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl)
        .unwrap();
    f.write_all(b"{this is not json}\n").unwrap();
    drop(f);

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    let report = builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();

    assert_eq!(report.chunks_loaded, 2);
    assert_eq!(report.parse_errors.len(), 1);
    assert_eq!(count(builder.conn(), "SELECT count(*) FROM chunks"), 2);
}

#[test]
fn rebuild_rejects_mismatched_source_chunks() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl = dir.path().join("chunks.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    write_jsonl(
        &jsonl,
        &[
            sample_chunk("A", "a1", "fn a1() {}"),
            sample_chunk("B", "b1", "fn b1() {}"),
        ],
    );

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    let report = builder.rebuild_from_jsonl("A", &jsonl).unwrap();

    assert_eq!(report.chunks_loaded, 1);
    assert_eq!(report.parse_errors.len(), 1);
    assert!(
        report.parse_errors[0].message.contains("b1"),
        "expected error to mention mis-sourced chunk id, got {:?}",
        report.parse_errors[0].message
    );
    assert_eq!(
        count(
            builder.conn(),
            "SELECT count(*) FROM chunks WHERE source = 'A'"
        ),
        1
    );
    assert_eq!(
        count(
            builder.conn(),
            "SELECT count(*) FROM chunks WHERE source = 'B'"
        ),
        0
    );
}

#[test]
fn rebuild_fts_is_queryable() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl = dir.path().join("chunks.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    write_jsonl(
        &jsonl,
        &[sample_chunk("mini", "tok1", "verify_token validates jwt")],
    );

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    builder.rebuild_from_jsonl("mini", &jsonl).unwrap();

    let id: String = builder
        .conn()
        .query_row(
            "SELECT id FROM chunks_fts WHERE chunks_fts MATCH 'jwt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(id, "tok1");
}

#[test]
fn rebuild_vec_insert_succeeds_with_correct_dim() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl = dir.path().join("chunks.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    let chunks = vec![
        sample_chunk("mini", "a", "content one"),
        sample_chunk("mini", "b", "content two"),
        sample_chunk("mini", "c", "content three"),
    ];
    write_jsonl(&jsonl, &chunks);

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    builder.rebuild_from_jsonl("mini", &jsonl).unwrap();

    assert_eq!(
        count(builder.conn(), "SELECT count(*) FROM chunks_vec"),
        chunks.len() as i64
    );
}

#[test]
fn rebuild_is_atomic_on_error() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.db");
    let jsonl = dir.path().join("chunks.jsonl");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    // Two chunks with identical ids. load_all_lenient does not dedup, so the
    // second INSERT hits a PRIMARY KEY violation and the txn must roll back.
    let dup_a = sample_chunk("mini", "dup", "fn one() {}");
    let dup_b = sample_chunk("mini", "dup", "fn two() {}");
    write_jsonl(&jsonl, &[dup_a, dup_b]);

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    let result = builder.rebuild_from_jsonl("mini", &jsonl);
    assert!(result.is_err(), "expected duplicate-id insert to error");

    let conn = builder.conn();
    assert_eq!(count(conn, "SELECT count(*) FROM chunks"), 0);
    assert_eq!(count(conn, "SELECT count(*) FROM chunks_fts"), 0);
    assert_eq!(count(conn, "SELECT count(*) FROM chunks_vec"), 0);
}
