//! Phase 3 integration tests: extracted chunks become queryable via FTS5
//! and sqlite-vec after a full pipeline run (extract → jsonl → index).

use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::tempdir;

use super::config::{SourceEntry, SourceKind, SourceLocation};
use super::extract::dispatch::{extract_source, ExtractConfig};
use super::index::builder::IndexBuilder;
use super::index::state::{IndexState, StateCheck};
use super::index::traits::{Embedder, FixedClock, HashEmbedder};
use super::store::ChunkStore;

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

#[test]
fn extracted_chunks_become_queryable_via_fts_and_vec() {
    let dir = tempdir().unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    let db = dir.path().join("index.db");

    let source = fixture_source("mini-rust");
    let extracted =
        extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    assert!(!extracted.chunks.is_empty());

    let mut store = ChunkStore::new(&jsonl);
    for c in &extracted.chunks {
        store.append(c).unwrap();
    }

    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    let report = builder
        .rebuild_from_jsonl("mini-rust", &jsonl)
        .unwrap();
    assert_eq!(report.chunks_loaded, extracted.chunks.len());
    assert_eq!(report.chunks_inserted, extracted.chunks.len());

    let conn = Connection::open(&db).unwrap();
    let total: i64 = conn
        .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total as usize, extracted.chunks.len());

    let fts_rows: Vec<(String, String)> = conn
        .prepare("SELECT id, content FROM chunks_fts WHERE chunks_fts MATCH 'verify_token'")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!fts_rows.is_empty());
    assert!(
        fts_rows
            .iter()
            .any(|(id, _)| id.contains("verify_token")),
        "expected FTS hit id containing verify_token, got {fts_rows:?}"
    );
    assert!(
        fts_rows
            .iter()
            .any(|(_, content)| content.to_lowercase().contains("verify_token")),
        "expected at least one FTS match with verify_token in content, got {fts_rows:?}"
    );

    let vec_total: i64 = conn
        .query_row("SELECT count(*) FROM chunks_vec", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vec_total as usize, extracted.chunks.len());

    let q_embedding = emb.embed("jwt verification signing key");
    let mut q_bytes = Vec::with_capacity(q_embedding.len() * 4);
    for f in &q_embedding {
        q_bytes.extend_from_slice(&f.to_le_bytes());
    }
    let knn_hits: Vec<(String, f32)> = conn
        .prepare(
            "SELECT id, distance FROM chunks_vec \
             WHERE embedding MATCH ? AND k = 3 \
             ORDER BY distance",
        )
        .unwrap()
        .query_map([q_bytes], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f32>(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(knn_hits.len(), 3);
    for (_id, dist) in &knn_hits {
        assert!(dist.is_finite() && *dist >= 0.0);
    }
}

#[test]
fn state_file_tracks_model_hash_after_build() {
    let dir = tempdir().unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    let db = dir.path().join("index.db");
    let state_path = dir.path().join("state.json");

    let source = fixture_source("mini-rust");
    let extracted =
        extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    let mut store = ChunkStore::new(&jsonl);
    for c in &extracted.chunks {
        store.append(c).unwrap();
    }

    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();

    let state = IndexState::new(emb.model_hash());
    state.save(&state_path).unwrap();

    let loaded = IndexState::load(&state_path).unwrap().unwrap();
    assert_eq!(loaded.embedder_model_hash, emb.model_hash());
    assert_eq!(
        IndexState::check_against(Some(&loaded), &emb.model_hash()),
        StateCheck::Ok
    );

    let other_emb = HashEmbedder::new(512);
    match IndexState::check_against(Some(&loaded), &other_emb.model_hash()) {
        StateCheck::ReembedRequired { .. } => {}
        other => panic!("expected ReembedRequired, got {other:?}"),
    }
}

#[test]
fn rebuild_of_same_source_replaces_prior_vectors() {
    let dir = tempdir().unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    let db = dir.path().join("index.db");

    let source = fixture_source("mini-rust");
    let extracted =
        extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    let mut store = ChunkStore::new(&jsonl);
    for c in &extracted.chunks {
        store.append(c).unwrap();
    }

    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    let first = builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();
    let second = builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();

    assert_eq!(first.prior_source_chunks_removed, 0);
    assert_eq!(second.prior_source_chunks_removed, first.chunks_inserted);
    assert_eq!(second.chunks_inserted, first.chunks_inserted);

    let conn = Connection::open(&db).unwrap();
    let total: i64 = conn
        .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total as usize, extracted.chunks.len());

    let vec_total: i64 = conn
        .query_row("SELECT count(*) FROM chunks_vec", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vec_total as usize, extracted.chunks.len());

    let fts_total: i64 = conn
        .query_row("SELECT count(*) FROM chunks_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts_total as usize, extracted.chunks.len());
}
