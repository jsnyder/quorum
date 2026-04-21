//! Phase 2 integration tests: extract_source produces chunks that roundtrip
//! through the JSONL store without loss.

use std::path::PathBuf;

use tempfile::tempdir;

use super::config::{SourceEntry, SourceKind, SourceLocation};
use super::extract::dispatch::{extract_source, ExtractConfig, FixedClock};
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

#[test]
fn extract_source_writes_jsonl_that_loads_back_mini_rust() {
    let source = fixture_source("mini-rust");
    let dir = tempdir().unwrap();
    let chunks_path = dir.path().join("chunks.jsonl");

    let result = extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    let mut store = ChunkStore::new(&chunks_path);
    for c in &result.chunks {
        store.append(c).unwrap();
    }

    let loaded = ChunkStore::load_all(&chunks_path).unwrap();
    assert_eq!(loaded.len(), result.chunks.len());
    assert_eq!(loaded, result.chunks);
    assert!(loaded
        .iter()
        .any(|c| c.qualified_name.as_deref() == Some("verify_token")));
    assert!(loaded.iter().any(|c| matches!(c.kind, ChunkKind::Doc)));
    assert!(loaded
        .iter()
        .any(|c| c.subtype.as_deref() == Some("ADR")));
}

#[test]
fn extract_source_roundtrips_mini_ts_and_mini_terraform() {
    for name in ["mini-ts", "mini-terraform"] {
        let source = fixture_source(name);
        let dir = tempdir().unwrap();
        let chunks_path = dir.path().join("chunks.jsonl");

        let result =
            extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
        assert!(
            !result.chunks.is_empty(),
            "expected non-empty extraction for {name}"
        );
        let mut store = ChunkStore::new(&chunks_path);
        for c in &result.chunks {
            store.append(c).unwrap();
        }

        let loaded = ChunkStore::load_all(&chunks_path).unwrap();
        assert_eq!(loaded, result.chunks, "roundtrip mismatch for {name}");

        let validation = ChunkStore::validate(&loaded);
        assert!(
            !validation.has_errors(),
            "structural validation failed for {name}: {:?}",
            validation.errors
        );
    }
}

#[test]
fn validation_passes_on_combined_multi_source_extraction() {
    let dir = tempdir().unwrap();
    let chunks_path = dir.path().join("chunks.jsonl");
    let mut store = ChunkStore::new(&chunks_path);

    for name in ["mini-rust", "mini-ts", "mini-terraform"] {
        let source = fixture_source(name);
        let result =
            extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
        for c in &result.chunks {
            store.append(c).unwrap();
        }
    }

    let loaded = ChunkStore::load_all(&chunks_path).unwrap();
    let validation = ChunkStore::validate(&loaded);
    assert!(
        !validation.has_errors(),
        "validation errors across combined sources: {:?}",
        validation.errors
    );

    // Spot-check a symbol from each source appears.
    assert!(loaded
        .iter()
        .any(|c| c.qualified_name.as_deref() == Some("verify_token")));
    assert!(loaded
        .iter()
        .any(|c| c.qualified_name.as_deref() == Some("verifyToken")));
    assert!(loaded
        .iter()
        .any(|c| c.qualified_name.as_deref() == Some("aws_vpc.this")));
}
