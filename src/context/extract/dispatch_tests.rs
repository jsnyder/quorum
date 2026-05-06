use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::dispatch::{ExtractConfig, FixedClock, extract_source};
use crate::context::config::{SourceEntry, SourceKind, SourceLocation};
use crate::context::types::ChunkKind;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/context/repos")
        .join(name)
}

fn mk_entry(name: &str, kind: SourceKind, path: PathBuf) -> SourceEntry {
    SourceEntry {
        name: name.to_string(),
        kind,
        location: SourceLocation::Path(path),
        paths: vec![],
        weight: None,
        ignore: vec![],
    }
}

#[test]
fn extracts_mini_rust_source_end_to_end() {
    let entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.qualified_name.as_deref() == Some("verify_token")),
        "expected verify_token symbol chunk"
    );
    assert!(
        result.chunks.iter().any(|c| c.kind == ChunkKind::Doc),
        "expected at least one doc chunk"
    );
    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.subtype.as_deref() == Some("ADR")),
        "expected ADR subtype chunk from docs/adr/001-foo.md"
    );
    assert!(
        result.diagnostics.extracted_files >= 4,
        "expected at least 4 extracted files, got {}",
        result.diagnostics.extracted_files
    );
}

#[test]
fn extracts_mini_ts_source() {
    let entry = mk_entry("mini-ts", SourceKind::Typescript, fixture_path("mini-ts"));
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.qualified_name.as_deref() == Some("verifyToken")),
        "expected verifyToken symbol"
    );
    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.qualified_name.as_deref() == Some("VerifyOpts")),
        "expected VerifyOpts interface symbol"
    );
    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.kind == ChunkKind::Doc && c.subtype.as_deref() == Some("README")),
        "expected README doc chunk"
    );
}

#[test]
fn extracts_mini_terraform_source() {
    let entry = mk_entry(
        "mini-tf",
        SourceKind::Terraform,
        fixture_path("mini-terraform"),
    );
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    let qn: Vec<&str> = result
        .chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();

    assert!(
        qn.contains(&"aws_vpc.this"),
        "expected resource aws_vpc.this, got {qn:?}"
    );
    assert!(
        qn.contains(&"vpc_id"),
        "expected output vpc_id, got {qn:?}"
    );
    assert!(qn.contains(&"name"), "expected variable name");
    assert!(
        qn.contains(&"cidr_block"),
        "expected variable cidr_block"
    );
    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.kind == ChunkKind::Doc && c.subtype.as_deref() == Some("README")),
        "expected README chunk"
    );
}

#[test]
fn respects_per_source_ignore_patterns() {
    let mut entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    entry.ignore = vec!["docs/**".into()];
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    assert!(
        !result
            .chunks
            .iter()
            .any(|c| c.metadata.source_path.starts_with("docs/")),
        "expected no chunks from docs/"
    );
    assert!(
        result.diagnostics.skipped_by_tier.per_source > 0,
        "expected per_source skip count > 0"
    );
}

#[test]
fn respects_global_ignore() {
    let entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    let config = ExtractConfig {
        global_ignore: vec!["*.md".into()],
        ..Default::default()
    };
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &config, &clock).unwrap();

    assert!(
        !result.chunks.iter().any(|c| c.kind == ChunkKind::Doc),
        "expected no doc chunks when *.md is globally ignored"
    );
    assert!(
        result.diagnostics.skipped_by_tier.global > 0,
        "expected global skip count > 0"
    );
}

#[test]
fn skips_file_larger_than_cap() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let big = root.join("big.rs");
    fs::write(&big, "a".repeat(2048)).unwrap();

    let small = root.join("small.rs");
    fs::write(&small, "pub fn hello() {}\n").unwrap();

    let entry = mk_entry("tmp", SourceKind::Rust, root.to_path_buf());
    let config = ExtractConfig {
        max_file_size_bytes: 1024,
        global_ignore: vec![],
    };
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &config, &clock).unwrap();

    assert_eq!(result.diagnostics.size_skipped, 1);
    assert_eq!(result.diagnostics.extracted_files, 1);
}

#[test]
fn extractor_error_on_one_file_does_not_abort() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Non-utf8 bytes in a .rs file: read_to_string will fail, populating
    // extractor_errors without aborting the whole run.
    let bad = root.join("bad.rs");
    fs::write(&bad, [0xff, 0xfe, 0xfd, 0xfc]).unwrap();

    let good = root.join("good.rs");
    fs::write(&good, "pub fn good_one() {}\n").unwrap();

    let entry = mk_entry("tmp", SourceKind::Rust, root.to_path_buf());
    let config = ExtractConfig {
        max_file_size_bytes: ExtractConfig::default().max_file_size_bytes,
        global_ignore: vec![],
    };
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &config, &clock).unwrap();

    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.qualified_name.as_deref() == Some("good_one")),
        "expected good_one symbol from the valid file"
    );
}

#[test]
fn unknown_extension_is_skipped_not_errored() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::write(root.join("foo.xyz"), "nothing interesting").unwrap();

    let entry = mk_entry("tmp", SourceKind::Docs, root.to_path_buf());
    let config = ExtractConfig {
        max_file_size_bytes: ExtractConfig::default().max_file_size_bytes,
        global_ignore: vec![],
    };
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &config, &clock).unwrap();

    assert_eq!(result.diagnostics.unknown_extension_skipped, 1);
    assert!(result.diagnostics.extractor_errors.is_empty());
    assert!(result.chunks.is_empty());
}

#[test]
fn source_path_uses_forward_slashes() {
    let entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    for chunk in &result.chunks {
        assert!(
            !chunk.metadata.source_path.contains('\\'),
            "source_path {:?} contains backslash",
            chunk.metadata.source_path
        );
    }
    // Sanity: at least one nested path present.
    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.metadata.source_path.contains('/')),
        "expected at least one nested source_path"
    );
}

#[test]
fn git_source_returns_error_for_mvp() {
    let entry = SourceEntry {
        name: "remote".into(),
        kind: SourceKind::Rust,
        location: SourceLocation::Git {
            url: "https://example.com/repo.git".into(),
            rev: None,
        },
        paths: vec![],
        weight: None,
        ignore: vec![],
    };
    let clock = FixedClock::epoch();
    let err = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("git"),
        "expected error mentioning git, got: {err}"
    );
}

#[test]
fn overlapping_paths_dedupe_and_extract_once() {
    // Baseline: scan the root once, capture the chunk id set.
    let baseline_entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    let clock = FixedClock::epoch();
    let baseline = extract_source(&baseline_entry, &ExtractConfig::default(), &clock).unwrap();
    let baseline_ids: std::collections::BTreeSet<String> =
        baseline.chunks.iter().map(|c| c.id.clone()).collect();
    assert_eq!(
        baseline_ids.len(),
        baseline.chunks.len(),
        "baseline chunk ids are already unique"
    );

    // Now point `paths` at both `.` (root) and `src` — overlapping. With the
    // dedupe in place, the descendant `src` must be dropped and chunk ids
    // should remain unique and match the baseline exactly.
    let mut entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    entry.paths = vec![PathBuf::from("."), PathBuf::from("src")];
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    let ids: Vec<String> = result.chunks.iter().map(|c| c.id.clone()).collect();
    let unique: std::collections::BTreeSet<String> = ids.iter().cloned().collect();
    assert_eq!(
        ids.len(),
        unique.len(),
        "overlapping scan roots produced duplicate chunk ids: {} total vs {} unique",
        ids.len(),
        unique.len()
    );
    assert_eq!(
        unique, baseline_ids,
        "overlapping scan roots should yield the same chunk set as a single root scan"
    );
}

#[test]
fn exact_duplicate_paths_dedupe() {
    // Two identical path entries must collapse to a single scan.
    let mut entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    entry.paths = vec![PathBuf::from("src"), PathBuf::from("src")];
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    let ids: Vec<String> = result.chunks.iter().map(|c| c.id.clone()).collect();
    let unique: std::collections::BTreeSet<String> = ids.iter().cloned().collect();
    assert_eq!(
        ids.len(),
        unique.len(),
        "duplicate path entries produced duplicate chunk ids"
    );
}

#[test]
fn bare_directory_name_in_global_ignore_is_honored() {
    let entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    let config = ExtractConfig {
        global_ignore: vec!["docs".into()], // bare name, no slash
        ..Default::default()
    };
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &config, &clock).unwrap();

    assert!(
        !result
            .chunks
            .iter()
            .any(|c| c.metadata.source_path.starts_with("docs/")),
        "bare `docs` ignore pattern should exclude docs/ subtree"
    );
    assert!(
        result.diagnostics.skipped_by_tier.global > 0,
        "expected global skip count > 0 from bare ignore pattern"
    );
}

#[test]
fn rejects_scan_path_escaping_source_root() {
    let entry = SourceEntry {
        name: "x".into(),
        kind: SourceKind::Rust,
        location: SourceLocation::Path(fixture_path("mini-rust")),
        paths: vec![PathBuf::from("../../../")],
        weight: None,
        ignore: vec![],
    };
    let result = extract_source(&entry, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    assert!(result.chunks.is_empty());
    assert_eq!(result.diagnostics.escaped_paths, 1);
}

#[test]
fn rejects_absolute_scan_path_outside_root() {
    let entry = SourceEntry {
        name: "x".into(),
        kind: SourceKind::Rust,
        location: SourceLocation::Path(fixture_path("mini-rust")),
        paths: vec![PathBuf::from("/etc")],
        weight: None,
        ignore: vec![],
    };
    let result = extract_source(&entry, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    assert!(result.chunks.is_empty());
    assert_eq!(result.diagnostics.escaped_paths, 1);
}

#[test]
fn empty_source_path_list_scans_root() {
    let entry = mk_entry("mini-rust", SourceKind::Rust, fixture_path("mini-rust"));
    let clock = FixedClock::epoch();
    let result = extract_source(&entry, &ExtractConfig::default(), &clock).unwrap();

    let files: std::collections::BTreeSet<String> = result
        .chunks
        .iter()
        .map(|c| c.metadata.source_path.clone())
        .collect();
    assert!(
        files.iter().any(|p| p.starts_with("src/")),
        "expected chunks from src/"
    );
    assert!(
        files.iter().any(|p| p == "README.md"),
        "expected root README chunk"
    );
}
