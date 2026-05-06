use std::path::Path;

use rusqlite::Connection;
use tempfile::tempdir;

use super::Filters;
use super::vector::{embedding_to_le_bytes, vec_search};
use crate::context::index::builder::IndexBuilder;
use crate::context::index::traits::{Embedder, FixedClock, HashEmbedder};
use crate::context::store::ChunkStore;
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn mk_chunk(source: &str, id: &str, content: &str, kind: ChunkKind) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind,
        subtype: None,
        qualified_name: Some(id.to_string()),
        signature: None,
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
        provenance: Provenance::new("test", 0.9, "src/x.rs").unwrap(),
    }
}

/// Build an index populated with `chunks`, one JSONL per distinct source.
/// Returns an open connection to the resulting database.
fn build_test_db_with_embeddings(dir: &Path, chunks: Vec<Chunk>) -> Connection {
    let db = dir.join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();

    let mut by_source: std::collections::BTreeMap<String, Vec<Chunk>> =
        std::collections::BTreeMap::new();
    for c in chunks {
        by_source.entry(c.source.clone()).or_default().push(c);
    }

    for (source, src_chunks) in &by_source {
        let jsonl = dir.join(format!("{source}.jsonl"));
        let mut store = ChunkStore::new(&jsonl);
        for c in src_chunks {
            store.append(c).unwrap();
        }
        builder.rebuild_from_jsonl(source, &jsonl).unwrap();
    }

    drop(builder);
    Connection::open(&db).unwrap()
}

fn query_vec(text: &str) -> Vec<f32> {
    HashEmbedder::new(384).embed(text)
}

#[test]
fn knn_returns_nearest_chunks_first() {
    let dir = tempdir().unwrap();
    let chunks = vec![
        mk_chunk("s", "jwt", "jwt auth signing", ChunkKind::Symbol),
        mk_chunk("s", "db", "database query", ChunkKind::Symbol),
        mk_chunk(
            "s",
            "weather",
            "unrelated text about weather",
            ChunkKind::Symbol,
        ),
    ];
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("jwt authentication");
    let hits = vec_search(&conn, &q, &Filters::default(), 3).unwrap();

    assert!(!hits.is_empty());
    assert_eq!(hits[0].chunk_id, "jwt");
}

#[test]
fn k_limits_results() {
    let dir = tempdir().unwrap();
    let chunks: Vec<Chunk> = (0..6)
        .map(|i| {
            mk_chunk(
                "s",
                &format!("id{i}"),
                &format!("content number {i}"),
                ChunkKind::Symbol,
            )
        })
        .collect();
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("content");
    let hits = vec_search(&conn, &q, &Filters::default(), 2).unwrap();

    assert_eq!(hits.len(), 2);
}

#[test]
fn source_filter_limits_results() {
    let dir = tempdir().unwrap();
    let mut chunks = Vec::new();
    for i in 0..3 {
        chunks.push(mk_chunk(
            "A",
            &format!("a{i}"),
            &format!("alpha {i}"),
            ChunkKind::Symbol,
        ));
        chunks.push(mk_chunk(
            "B",
            &format!("b{i}"),
            &format!("beta {i}"),
            ChunkKind::Symbol,
        ));
    }
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("alpha beta");
    let filters = Filters {
        sources: vec!["A".into()],
        kinds: vec![],
        exclude_source_paths: vec![],
    };
    let hits = vec_search(&conn, &q, &filters, 6).unwrap();

    assert!(!hits.is_empty());
    for h in &hits {
        assert!(h.chunk_id.starts_with('a'), "unexpected id: {}", h.chunk_id);
    }
}

#[test]
fn kind_filter_limits_results() {
    let dir = tempdir().unwrap();
    let chunks = vec![
        mk_chunk("s", "sym1", "token one", ChunkKind::Symbol),
        mk_chunk("s", "sym2", "token two", ChunkKind::Symbol),
        mk_chunk("s", "doc1", "documentation about tokens", ChunkKind::Doc),
        mk_chunk("s", "doc2", "more token docs", ChunkKind::Doc),
    ];
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("token");
    let filters = Filters {
        sources: vec![],
        kinds: vec![ChunkKind::Doc],
        exclude_source_paths: vec![],
    };
    let hits = vec_search(&conn, &q, &filters, 4).unwrap();

    assert!(!hits.is_empty());
    for h in &hits {
        assert!(
            h.chunk_id.starts_with("doc"),
            "unexpected id: {}",
            h.chunk_id
        );
    }
}

#[test]
fn empty_filters_returns_all() {
    let dir = tempdir().unwrap();
    let chunks = vec![
        mk_chunk("s", "a", "alpha content", ChunkKind::Symbol),
        mk_chunk("s", "b", "beta content", ChunkKind::Symbol),
        mk_chunk("s", "c", "gamma content", ChunkKind::Symbol),
    ];
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("content");
    let hits = vec_search(&conn, &q, &Filters::default(), 3).unwrap();

    assert_eq!(hits.len(), 3);
}

#[test]
fn distance_is_non_negative() {
    let dir = tempdir().unwrap();
    let chunks = vec![
        mk_chunk("s", "a", "alpha", ChunkKind::Symbol),
        mk_chunk("s", "b", "beta", ChunkKind::Symbol),
    ];
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("alpha");
    let hits = vec_search(&conn, &q, &Filters::default(), 2).unwrap();

    for h in &hits {
        assert!(h.distance >= 0.0, "distance must be non-negative");
        assert!(h.distance.is_finite(), "distance must be finite");
    }
}

#[test]
fn embedding_to_le_bytes_roundtrip() {
    let bytes = embedding_to_le_bytes(&[1.0, 2.0, 3.0]);
    assert_eq!(bytes.len(), 12);

    let a = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let b = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let c = f32::from_le_bytes(bytes[8..12].try_into().unwrap());
    assert_eq!(a, 1.0);
    assert_eq!(b, 2.0);
    assert_eq!(c, 3.0);
}

#[test]
fn k_zero_returns_empty() {
    let dir = tempdir().unwrap();
    let chunks = vec![mk_chunk("s", "a", "alpha", ChunkKind::Symbol)];
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("alpha");
    let hits = vec_search(&conn, &q, &Filters::default(), 0).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn mismatched_dim_returns_error_or_empty() {
    let dir = tempdir().unwrap();
    let chunks = vec![mk_chunk("s", "a", "alpha content", ChunkKind::Symbol)];
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    // Index is 384-dim; pass a 128-dim vector.
    let q = vec![0.1f32; 128];
    let result = vec_search(&conn, &q, &Filters::default(), 3);

    if let Ok(hits) = result { assert!(hits.is_empty()) }
}

#[test]
fn filter_narrows_to_empty_when_no_matches() {
    let dir = tempdir().unwrap();
    let chunks = vec![
        mk_chunk("real", "a", "alpha", ChunkKind::Symbol),
        mk_chunk("real", "b", "beta", ChunkKind::Symbol),
    ];
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("alpha");
    let filters = Filters {
        sources: vec!["nonexistent".into()],
        kinds: vec![],
        exclude_source_paths: vec![],
    };
    let hits = vec_search(&conn, &q, &filters, 3).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn strict_filter_still_returns_k_when_possible() {
    // 10 chunks across two sources (5 each). A strict filter on source "A"
    // should still return k=4 results; the adaptive overfetch should grow
    // past the initial k*4 if filters drop candidates below k.
    let dir = tempdir().unwrap();
    let mut chunks = Vec::new();
    for i in 0..5 {
        chunks.push(mk_chunk(
            "A",
            &format!("a{i}"),
            &format!("alpha match target {i}"),
            ChunkKind::Symbol,
        ));
        chunks.push(mk_chunk(
            "B",
            &format!("b{i}"),
            &format!("beta distractor {i}"),
            ChunkKind::Symbol,
        ));
    }
    let conn = build_test_db_with_embeddings(dir.path(), chunks);

    let q = query_vec("alpha match target");
    let filters = Filters {
        sources: vec!["A".into()],
        kinds: vec![],
        exclude_source_paths: vec![],
    };
    let hits = vec_search(&conn, &q, &filters, 4).unwrap();
    assert_eq!(hits.len(), 4);
    for h in &hits {
        assert!(h.chunk_id.starts_with('a'), "unexpected id: {}", h.chunk_id);
    }
}
