use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tempfile::tempdir;

use super::structural::{StructuralHit, structural_search};
use crate::context::index::builder::IndexBuilder;
use crate::context::index::traits::{FixedClock, HashEmbedder};
use crate::context::store::ChunkStore;
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn sample_symbol(id: &str, source: &str, qname: &str, content: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: Some(qname.to_string()),
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
            let mut store = ChunkStore::new(&src_jsonl);
            for c in chunks.iter().filter(|c| &c.source == src) {
                store.append(c).unwrap();
            }
            builder.rebuild_from_jsonl(src, &src_jsonl).unwrap();
        }
    }

    Connection::open(&db).unwrap()
}

#[test]
fn empty_input_returns_empty_without_touching_db() {
    // Pass a connection that points nowhere useful; if the impl
    // runs any SQL we'd error. Empty vec is the only correct answer.
    let conn = Connection::open_in_memory().unwrap();
    let hits = structural_search(&conn, &[]).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn single_exact_match_returns_one_hit() {
    let dir = tempdir().unwrap();
    let chunks = vec![sample_symbol(
        "c1",
        "repo",
        "MyCrate::validate",
        "fn validate() {}",
    )];
    let conn = build_test_db(dir.path(), chunks);
    let hits =
        structural_search(&conn, &["MyCrate::validate".into()]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, "c1");
    assert_eq!(hits[0].qualified_name, "MyCrate::validate");
}

#[test]
fn no_match_returns_empty() {
    let dir = tempdir().unwrap();
    let chunks = vec![sample_symbol(
        "c1",
        "repo",
        "MyCrate::validate",
        "fn validate() {}",
    )];
    let conn = build_test_db(dir.path(), chunks);
    let hits =
        structural_search(&conn, &["nonexistent::fn".into()]).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn multiple_qnames_mixed_hit_and_miss() {
    let dir = tempdir().unwrap();
    let chunks = vec![
        sample_symbol("c1", "repo", "a::one", "fn one() {}"),
        sample_symbol("c2", "repo", "a::two", "fn two() {}"),
    ];
    let conn = build_test_db(dir.path(), chunks);
    let hits = structural_search(
        &conn,
        &["a::one".into(), "never::seen".into(), "a::two".into()],
    )
    .unwrap();
    assert_eq!(hits.len(), 2);
    // Order follows the input qname order, not DB order.
    assert_eq!(hits[0].qualified_name, "a::one");
    assert_eq!(hits[1].qualified_name, "a::two");
}

#[test]
fn duplicate_qname_in_different_chunks_returns_all() {
    // Two files both define a symbol named `a::validate` — legal
    // in a multi-source index. Both must come back.
    let dir = tempdir().unwrap();
    let chunks = vec![
        sample_symbol("c1", "lib-a", "a::validate", "fn validate() {}"),
        sample_symbol("c2", "lib-b", "a::validate", "fn validate() {}"),
    ];
    let conn = build_test_db(dir.path(), chunks);
    let hits = structural_search(&conn, &["a::validate".into()]).unwrap();
    assert_eq!(hits.len(), 2);
    let ids: Vec<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
    assert!(ids.contains(&"c1"));
    assert!(ids.contains(&"c2"));
}

#[test]
fn match_is_case_sensitive() {
    // Decision: Rust and TS both treat identifiers as
    // case-sensitive; any case-folding would produce false hits
    // and surprise users. Test pins the contract.
    let dir = tempdir().unwrap();
    let chunks = vec![sample_symbol(
        "c1",
        "repo",
        "MyCrate::Validate",
        "fn Validate() {}",
    )];
    let conn = build_test_db(dir.path(), chunks);
    let hits =
        structural_search(&conn, &["mycrate::validate".into()]).unwrap();
    assert!(hits.is_empty(), "case-folded match must NOT succeed");
}

#[test]
fn qname_with_sql_metachars_is_safely_bound() {
    // Parameterization contract: the value is never interpolated
    // as SQL. If this were vulnerable, the test DB would be
    // dropped and subsequent queries would fail.
    let dir = tempdir().unwrap();
    let chunks = vec![sample_symbol(
        "c1",
        "repo",
        "harmless",
        "fn harmless() {}",
    )];
    let conn = build_test_db(dir.path(), chunks);
    let payload = "'); DROP TABLE chunks;--".to_string();
    let hits = structural_search(&conn, &[payload]).unwrap();
    assert!(hits.is_empty());
    // Prove the table still exists:
    let surviving: Vec<StructuralHit> =
        structural_search(&conn, &["harmless".into()]).unwrap();
    assert_eq!(surviving.len(), 1);
}
