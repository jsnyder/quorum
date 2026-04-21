use rusqlite::Connection;
use tempfile::tempdir;

use super::builder::{IndexBuilder, SCHEMA_VERSION};
use super::traits::{Embedder, FixedClock, HashEmbedder};

fn table_names(db: &std::path::Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    conn.prepare(
        "SELECT name FROM sqlite_master WHERE type IN ('table') ORDER BY name",
    )
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
        .prepare(
            "SELECT id FROM chunks_fts WHERE chunks_fts MATCH 'verify_token' ORDER BY id",
        )
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
