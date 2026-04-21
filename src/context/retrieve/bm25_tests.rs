use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tempfile::tempdir;

use super::bm25::{Bm25Hit, Filters, bm25_search, build_match_expression};
use crate::context::index::builder::IndexBuilder;
use crate::context::index::traits::{FixedClock, HashEmbedder};
use crate::context::store::ChunkStore;
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn sample_chunk(id: &str, source: &str, content: &str, kind: ChunkKind) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind,
        subtype: None,
        qualified_name: None,
        signature: None,
        content: content.to_string(),
        metadata: ChunkMeta {
            source_path: format!("{id}.rs"),
            line_range: LineRange::new(1, 1).unwrap(),
            commit_sha: "0".to_string(),
            indexed_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            source_version: None,
            language: Some("rust".to_string()),
            is_exported: true,
            neighboring_symbols: Vec::new(),
        },
        provenance: Provenance::new("test", 1.0, "file://test").unwrap(),
    }
}

fn build_test_db(dir: &Path, chunks: Vec<Chunk>) -> Connection {
    let db = dir.join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    {
        let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();

        let mut sources: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for c in &chunks {
            sources.insert(c.source.clone());
        }
        for src in &sources {
            let src_jsonl = dir.join(format!("{src}.jsonl"));
            let mut src_store = ChunkStore::new(&src_jsonl);
            for c in chunks.iter().filter(|c| &c.source == src) {
                src_store.append(c).unwrap();
            }
            builder.rebuild_from_jsonl(src, &src_jsonl).unwrap();
        }
    }

    Connection::open(&db).unwrap()
}

fn ids(hits: &[Bm25Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.chunk_id.as_str()).collect()
}

#[test]
fn empty_query_returns_no_hits() {
    let dir = tempdir().unwrap();
    let conn = build_test_db(
        dir.path(),
        vec![sample_chunk("a", "src", "verify_token helps", ChunkKind::Symbol)],
    );
    let hits = bm25_search(&conn, "", &Filters::default(), 10).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn single_term_match_returns_relevant_chunk() {
    let dir = tempdir().unwrap();
    let conn = build_test_db(
        dir.path(),
        vec![
            sample_chunk("a", "src", "verify_token helps secure things", ChunkKind::Symbol),
            sample_chunk("b", "src", "random code doing stuff", ChunkKind::Symbol),
            sample_chunk("c", "src", "database query with params", ChunkKind::Symbol),
        ],
    );
    let expr = build_match_expression(&["verify_token".to_string()]).unwrap();
    let hits = bm25_search(&conn, &expr, &Filters::default(), 10).unwrap();
    assert!(ids(&hits).contains(&"a"), "expected 'a' in hits {:?}", ids(&hits));
    assert!(hits.iter().all(|h| h.chunk_id != "b" && h.chunk_id != "c"));
}

#[test]
fn higher_term_frequency_scores_higher() {
    let dir = tempdir().unwrap();
    let conn = build_test_db(
        dir.path(),
        vec![
            sample_chunk(
                "dense",
                "src",
                "alpha alpha alpha alpha alpha filler",
                ChunkKind::Symbol,
            ),
            sample_chunk("sparse", "src", "alpha once only here", ChunkKind::Symbol),
        ],
    );
    let hits = bm25_search(&conn, "alpha", &Filters::default(), 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].chunk_id, "dense");
    assert_eq!(hits[1].chunk_id, "sparse");
    assert!(hits[0].score >= hits[1].score);
}

#[test]
fn source_filter_excludes_other_sources() {
    let dir = tempdir().unwrap();
    let conn = build_test_db(
        dir.path(),
        vec![
            sample_chunk("a1", "A", "token verification logic", ChunkKind::Symbol),
            sample_chunk("b1", "B", "token verification logic", ChunkKind::Symbol),
        ],
    );
    let filters = Filters {
        sources: vec!["A".to_string()],
        kinds: Vec::new(),
    };
    let hits = bm25_search(&conn, "token", &filters, 10).unwrap();
    assert_eq!(ids(&hits), vec!["a1"]);
}

#[test]
fn kind_filter_excludes_other_kinds() {
    let dir = tempdir().unwrap();
    let conn = build_test_db(
        dir.path(),
        vec![
            sample_chunk("sym", "src", "token rotation schedule", ChunkKind::Symbol),
            sample_chunk("doc", "src", "token rotation schedule", ChunkKind::Doc),
        ],
    );
    let filters = Filters {
        sources: Vec::new(),
        kinds: vec![ChunkKind::Doc],
    };
    let hits = bm25_search(&conn, "token", &filters, 10).unwrap();
    assert_eq!(ids(&hits), vec!["doc"]);
}

#[test]
fn combined_source_and_kind_filter() {
    let dir = tempdir().unwrap();
    let conn = build_test_db(
        dir.path(),
        vec![
            sample_chunk("a_sym", "A", "token flow handler", ChunkKind::Symbol),
            sample_chunk("a_doc", "A", "token flow handler", ChunkKind::Doc),
            sample_chunk("b_doc", "B", "token flow handler", ChunkKind::Doc),
        ],
    );
    let filters = Filters {
        sources: vec!["A".to_string()],
        kinds: vec![ChunkKind::Doc],
    };
    let hits = bm25_search(&conn, "token", &filters, 10).unwrap();
    assert_eq!(ids(&hits), vec!["a_doc"]);
}

#[test]
fn limit_is_honored() {
    let dir = tempdir().unwrap();
    let mut chunks = Vec::new();
    for i in 0..6 {
        chunks.push(sample_chunk(
            &format!("c{i}"),
            "src",
            "lookup lookup lookup thing",
            ChunkKind::Symbol,
        ));
    }
    let conn = build_test_db(dir.path(), chunks);
    let hits = bm25_search(&conn, "lookup", &Filters::default(), 3).unwrap();
    assert_eq!(hits.len(), 3);
}

#[test]
fn match_expression_joins_identifiers_with_or() {
    let expr = build_match_expression(&["foo".to_string(), "bar".to_string()]);
    assert_eq!(expr.as_deref(), Some("\"foo\" OR \"bar\""));
}

#[test]
fn match_expression_empty_list_returns_none() {
    assert_eq!(build_match_expression(&[]), None);
    assert_eq!(
        build_match_expression(&["".to_string(), "   ".to_string()]),
        None
    );
}

#[test]
fn match_expression_filters_whitespace() {
    let expr = build_match_expression(&[
        "foo".to_string(),
        "  ".to_string(),
        "".to_string(),
    ]);
    assert_eq!(expr.as_deref(), Some("\"foo\""));
}

#[test]
fn identifier_with_double_quote_is_escaped() {
    let expr = build_match_expression(&[r#"foo"bar"#.to_string()]).unwrap();
    assert!(expr.contains("foo\"\"bar"), "got: {expr}");
}

#[test]
fn identifier_with_underscores_matches_as_single_token() {
    let dir = tempdir().unwrap();
    let conn = build_test_db(
        dir.path(),
        vec![
            sample_chunk("hit", "src", "verify_token helps", ChunkKind::Symbol),
            sample_chunk("miss", "src", "other code here", ChunkKind::Symbol),
        ],
    );
    let expr = build_match_expression(&["verify_token".to_string()]).unwrap();
    let hits = bm25_search(&conn, &expr, &Filters::default(), 10).unwrap();
    assert_eq!(ids(&hits), vec!["hit"]);
}
