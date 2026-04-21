//! Phase 1 integration tests: prove config + store + types compose correctly
//! through their public APIs.

use super::config::*;
use super::store::*;
use super::types::*;

use chrono::{DateTime, Utc};
use tempfile::tempdir;

/// The example fixture config should load cleanly and reference the three
/// mini repo fixtures.
#[test]
fn example_sources_fixture_loads_all_three_mini_repos() {
    let path = std::path::Path::new("tests/fixtures/context/sources/example-sources.toml");
    let config = SourcesConfig::load(path).unwrap();
    assert_eq!(config.sources.len(), 3);

    let by_name: std::collections::HashMap<_, _> =
        config.sources.iter().map(|s| (s.name.as_str(), s)).collect();
    assert!(by_name.contains_key("mini-rust"));
    assert!(by_name.contains_key("mini-ts"));
    assert!(by_name.contains_key("mini-terraform"));

    assert_eq!(by_name["mini-rust"].kind, SourceKind::Rust);
    assert_eq!(by_name["mini-ts"].kind, SourceKind::Typescript);
    assert_eq!(by_name["mini-terraform"].kind, SourceKind::Terraform);

    for src in &config.sources {
        assert!(
            matches!(src.location, SourceLocation::Path(_)),
            "fixture sources should all be path-based"
        );
    }
}

/// Chunks can be written via ChunkStore, roundtrip through JSONL, and pass
/// validation. The source field on chunks ties back to a source from config.
#[test]
fn config_and_store_compose_for_a_valid_workflow() {
    let dir = tempdir().unwrap();
    let chunks_path = dir.path().join("sources/mini-rust/chunks.jsonl");

    let config_toml = r#"
[[source]]
name = "mini-rust"
path = "tests/fixtures/context/repos/mini-rust"
kind = "rust"
"#;
    let config = SourcesConfig::from_str(config_toml).unwrap();
    let source_name = config.sources[0].name.clone();

    let c1 = make_chunk(
        &source_name,
        "mini-rust:src/token.rs:verify_token",
        "Validates a JWT against the signing key.",
    );
    let c2 = make_chunk(
        &source_name,
        "mini-rust:src/util.rs:clamp",
        "Clamps a value between lo and hi.",
    );

    let mut store = ChunkStore::new(&chunks_path);
    store.append(&c1).unwrap();
    store.append(&c2).unwrap();

    let loaded = ChunkStore::load_all(&chunks_path).unwrap();
    assert_eq!(loaded, vec![c1, c2]);

    let report = ChunkStore::validate(&loaded);
    assert!(
        !report.has_errors(),
        "clean chunks should validate: {:?}",
        report.errors
    );

    for chunk in &loaded {
        assert_eq!(chunk.source, source_name);
    }
}

/// Lenient load surfaces malformed lines without dropping the good ones, and
/// validate flags duplicate ids from the load.
#[test]
fn lenient_load_plus_validate_surfaces_mixed_file_issues() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chunks.jsonl");

    let c_good = make_chunk("s", "good", "content");
    let c_dup1 = make_chunk("s", "dup", "content1");
    let c_dup2 = make_chunk("s", "dup", "content2");
    let good_line = serde_json::to_string(&c_good).unwrap();
    let dup1_line = serde_json::to_string(&c_dup1).unwrap();
    let dup2_line = serde_json::to_string(&c_dup2).unwrap();

    let content = format!("{good_line}\n{{bad json}}\n{dup1_line}\n{dup2_line}\n");
    std::fs::write(&path, content).unwrap();

    let report = ChunkStore::load_all_lenient(&path).unwrap();
    assert_eq!(report.chunks.len(), 3);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].line_number, 2);

    let validation = ChunkStore::validate(&report.chunks);
    assert!(validation.has_errors());
    assert!(
        validation.errors.iter().any(|e| e.contains("dup")),
        "got: {:?}",
        validation.errors
    );
}

fn make_chunk(source: &str, id: &str, content: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: None,
        signature: None,
        content: content.to_string(),
        metadata: ChunkMeta {
            source_path: "src/x.rs".to_string(),
            line_range: LineRange { start: 1, end: 10 },
            commit_sha: "abc".into(),
            indexed_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            source_version: None,
            language: Some("rust".into()),
            is_exported: true,
            neighboring_symbols: vec![],
        },
        provenance: Provenance {
            extractor: "ast-grep-rust".into(),
            confidence: 1.0,
            source_uri: format!("git://{source}@abc/src/x.rs#L1-10"),
        },
    }
}
