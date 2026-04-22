use super::store::*;
use super::types::*;
use chrono::{DateTime, Utc};
use tempfile::tempdir;

fn test_chunk(id: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: "s".into(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: None,
        signature: None,
        content: "content".into(),
        metadata: ChunkMeta {
            source_path: "x.rs".into(),
            line_range: LineRange::new(1, 2).unwrap(),
            commit_sha: "c".into(),
            indexed_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            source_version: None,
            language: None,
            is_exported: true,
            neighboring_symbols: vec![],
        },
        provenance: Provenance::new("e", 1.0, "u").unwrap(),
    }
}

#[test]
fn append_creates_file_and_parent_dir() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nested/sub/chunks.jsonl");
    let mut store = ChunkStore::new(&path);
    store.append(&test_chunk("a")).unwrap();
    assert!(path.exists());
}

#[test]
fn append_and_load_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chunks.jsonl");
    let mut store = ChunkStore::new(&path);
    let c1 = test_chunk("a");
    let c2 = test_chunk("b");
    store.append(&c1).unwrap();
    store.append(&c2).unwrap();
    let loaded = ChunkStore::load_all(&path).unwrap();
    assert_eq!(loaded, vec![c1, c2]);
}

#[test]
fn append_writes_one_line_per_chunk() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chunks.jsonl");
    let mut store = ChunkStore::new(&path);
    store.append(&test_chunk("a")).unwrap();
    store.append(&test_chunk("b")).unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = contents.split_terminator('\n').collect();
    assert_eq!(lines.len(), 2, "expected 2 lines, got: {contents:?}");
    for line in lines {
        assert!(!line.contains('\n'));
    }
}

#[test]
fn load_all_returns_empty_when_file_missing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("does_not_exist.jsonl");
    let loaded = ChunkStore::load_all(&path).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn load_all_strict_errors_on_malformed_line() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bad.jsonl");
    std::fs::write(&path, "{ this is not json }\n").unwrap();
    assert!(ChunkStore::load_all(&path).is_err());
}

#[test]
fn load_all_lenient_skips_malformed_and_reports_errors() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("mixed.jsonl");
    let valid = serde_json::to_string(&test_chunk("good")).unwrap();
    let content = format!("{{not json}}\n{valid}\n{{also not json}}\n");
    std::fs::write(&path, content).unwrap();
    let report = ChunkStore::load_all_lenient(&path).unwrap();
    assert_eq!(report.chunks.len(), 1);
    assert_eq!(report.chunks[0].id, "good");
    assert_eq!(report.errors.len(), 2);
    assert_eq!(report.errors[0].line_number, 1);
    assert_eq!(report.errors[1].line_number, 3);
}

#[test]
fn load_all_lenient_skips_blank_lines() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse.jsonl");
    let valid = serde_json::to_string(&test_chunk("x")).unwrap();
    let content = format!("\n{valid}\n\n\n");
    std::fs::write(&path, content).unwrap();
    let report = ChunkStore::load_all_lenient(&path).unwrap();
    assert_eq!(report.chunks.len(), 1);
    assert!(report.errors.is_empty(), "blank lines shouldn't error");
}

#[test]
fn validate_detects_duplicate_ids() {
    let chunks = vec![test_chunk("dup"), test_chunk("dup")];
    let report = ChunkStore::validate(&chunks);
    assert!(report.has_errors());
    assert!(
        report.errors.iter().any(|e| e.contains("duplicate")),
        "got: {:?}",
        report.errors
    );
}

#[test]
fn validate_detects_empty_id() {
    let mut bad = test_chunk("");
    bad.id = String::new();
    let report = ChunkStore::validate(&[bad]);
    assert!(report.has_errors());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.to_lowercase().contains("empty") && e.contains("id")),
        "got: {:?}",
        report.errors
    );
}

#[test]
fn validate_accepts_valid_chunks() {
    let chunks = vec![test_chunk("a"), test_chunk("b")];
    let report = ChunkStore::validate(&chunks);
    assert!(!report.has_errors());
}
