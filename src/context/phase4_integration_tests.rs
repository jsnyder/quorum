//! Phase 4 integration tests: end-to-end retrieval over a real extracted
//! fixture — extract → index → query returns the expected chunk first.

use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::tempdir;

use super::config::{SourceEntry, SourceKind, SourceLocation};
use super::extract::dispatch::{extract_source, ExtractConfig};
use super::index::builder::IndexBuilder;
use super::index::traits::{FixedClock, HashEmbedder};
use super::retrieve::{Filters, RetrievalQuery, Retriever};
use super::store::ChunkStore;
use super::types::ChunkKind;

fn fixture_source(name: &str) -> SourceEntry {
    SourceEntry {
        name: name.to_string(),
        kind: SourceKind::Rust,
        location: SourceLocation::Path(PathBuf::from(format!(
            "tests/fixtures/context/repos/{name}"
        ))),
        paths: Vec::new(),
        weight: None,
        ignore: Vec::new(),
    }
}

fn build_retriever_for(source_name: &str) -> (tempfile::TempDir, Connection, HashEmbedder, FixedClock) {
    let dir = tempdir().unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    let db = dir.path().join("index.db");

    let source = fixture_source(source_name);
    let extracted =
        extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    assert!(!extracted.chunks.is_empty(), "empty extraction for {source_name}");

    let mut store = ChunkStore::new(&jsonl);
    for c in &extracted.chunks {
        store.append(c).unwrap();
    }

    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    {
        let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
        builder
            .rebuild_from_jsonl(source_name, &jsonl)
            .unwrap();
    }
    let conn = Connection::open(&db).unwrap();
    (dir, conn, emb, clock)
}

#[test]
fn retrieval_over_mini_rust_returns_verify_token_for_jwt_query() {
    let (_dir, conn, emb, clock) = build_retriever_for("mini-rust");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "jwt validation signing key".to_string(),
        identifiers: vec!["verify_token".to_string()],
        filters: Filters::default(),
        k: 5,
        min_score: 0.0,
        reviewed_file_language: Some("rust".to_string()),
    };
    let hits = retriever.query(q).unwrap();
    assert!(!hits.is_empty(), "expected at least one retrieval hit");
    assert_eq!(
        hits[0].chunk.qualified_name.as_deref(),
        Some("verify_token"),
        "top hit should be verify_token, got {:?}",
        hits.iter()
            .map(|h| h.chunk.qualified_name.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn retrieval_over_mini_ts_finds_verify_token_export() {
    let (_dir, conn, emb, clock) = build_retriever_for("mini-ts");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "jwt signing key verifier".to_string(),
        identifiers: vec!["verifyToken".to_string()],
        filters: Filters::default(),
        k: 5,
        min_score: 0.0,
        reviewed_file_language: Some("typescript".to_string()),
    };
    let hits = retriever.query(q).unwrap();
    assert!(!hits.is_empty(), "expected at least one retrieval hit");
    assert!(
        hits.iter()
            .any(|h| h.chunk.qualified_name.as_deref() == Some("verifyToken")),
        "expected verifyToken in hits: {:?}",
        hits.iter()
            .map(|h| h.chunk.qualified_name.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn kind_filter_restricts_to_docs() {
    let (_dir, conn, emb, clock) = build_retriever_for("mini-rust");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "authentication design decision".to_string(),
        identifiers: Vec::new(),
        filters: Filters {
            sources: Vec::new(),
            kinds: vec![ChunkKind::Doc],
            exclude_source_paths: vec![],
        },
        k: 10,
        min_score: 0.0,
        reviewed_file_language: None,
    };
    let hits = retriever.query(q).unwrap();
    assert!(!hits.is_empty(), "expected at least one doc hit");
    for h in &hits {
        assert_eq!(h.chunk.kind, ChunkKind::Doc);
    }
}

#[test]
fn unrelated_query_respects_min_score_threshold() {
    let (_dir, conn, emb, clock) = build_retriever_for("mini-rust");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "zzzzzzzzzz totally unrelated string".to_string(),
        identifiers: Vec::new(),
        filters: Filters::default(),
        k: 5,
        min_score: 2.0,
        reviewed_file_language: None,
    };
    let hits = retriever.query(q).unwrap();
    assert!(
        hits.is_empty(),
        "expected no hits above min_score=2.0, got {} hits",
        hits.len()
    );
}
