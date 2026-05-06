use super::markdown::*;
use chrono::{DateTime, Utc};

fn when() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap()
}

#[test]
fn splits_by_h2_preserving_code_blocks() {
    let md = r#"# Project
intro

## Usage
Call `foo()`:
```rust
fn main() { foo(); }
```

## Design
some prose
"#;
    let chunks = split_markdown(
        md,
        "README.md",
        "test-src",
        DocSubtype::Readme,
        "abc",
        when(),
    );
    // Now: preamble ("# Project\nintro"), Usage, Design
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].id, "test-src:README.md:__preamble__");
    assert!(chunks[0].content.contains("# Project"));
    assert!(chunks[0].content.contains("intro"));
    assert_eq!(chunks[1].id, "test-src:README.md:usage");
    assert_eq!(chunks[1].subtype.as_deref(), Some("README"));
    assert!(chunks[1].content.contains("## Usage"));
    assert!(chunks[1].content.contains("```rust"));
    assert!(chunks[1].content.contains("fn main"));
    assert_eq!(chunks[2].id, "test-src:README.md:design");
}

#[test]
fn heading_slug_disambiguates_duplicates() {
    let md = "# Top\n## Same\nfirst\n## Same\nsecond\n";
    let chunks = split_markdown(md, "d.md", "s", DocSubtype::Doc, "c", when());
    // Now: preamble ("# Top"), same, same-2
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].id, "s:d.md:__preamble__");
    assert_eq!(chunks[1].id, "s:d.md:same");
    assert_eq!(chunks[2].id, "s:d.md:same-2");
}

#[test]
fn empty_markdown_yields_no_chunks() {
    let chunks = split_markdown("", "e.md", "s", DocSubtype::Doc, "c", when());
    assert!(chunks.is_empty());
}

#[test]
fn whitespace_only_markdown_yields_no_chunks() {
    let chunks = split_markdown("\n\n   \n\n", "e.md", "s", DocSubtype::Doc, "c", when());
    assert!(chunks.is_empty());
}

#[test]
fn markdown_without_h2_emits_single_preamble_chunk() {
    let md = "# Title\nonly an h1 and some prose here\nmore prose\n";
    let chunks = split_markdown(md, "x.md", "s", DocSubtype::Doc, "c", when());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].id, "s:x.md:__preamble__");
    assert!(chunks[0].content.contains("only an h1"));
    assert!(chunks[0].content.contains("more prose"));
}

#[test]
fn preamble_and_h2_both_emitted() {
    let md = "# Title\nintro prose\n\n## Usage\nusage here\n";
    let chunks = split_markdown(md, "r.md", "s", DocSubtype::Doc, "c", when());
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].id, "s:r.md:__preamble__");
    assert!(chunks[0].content.contains("intro prose"));
    assert_eq!(chunks[1].id, "s:r.md:usage");
    assert!(chunks[1].content.contains("usage here"));
}

#[test]
fn h2_only_doc_has_no_preamble() {
    let md = "## First\nbody\n";
    let chunks = split_markdown(md, "r.md", "s", DocSubtype::Doc, "c", when());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].id, "s:r.md:first");
}

#[test]
fn h3_stays_inside_parent_h2() {
    let md = "# T\n## Parent\nintro\n### Sub\nsub content\n## Other\nother\n";
    let chunks = split_markdown(md, "d.md", "s", DocSubtype::Doc, "c", when());
    // Now: preamble ("# T"), Parent, Other
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].id, "s:d.md:__preamble__");
    assert!(chunks[1].content.contains("### Sub"));
    assert!(chunks[1].content.contains("sub content"));
    assert!(!chunks[1].content.contains("Other"));
}

#[test]
fn line_ranges_are_1_indexed_inclusive() {
    let md = "# T\n## A\nfirst\nsecond\n## B\nthird\n";
    // Line 1: # T        (preamble)
    // Line 2: ## A
    // Line 3: first
    // Line 4: second
    // Line 5: ## B
    // Line 6: third
    let chunks = split_markdown(md, "d.md", "s", DocSubtype::Doc, "c", when());
    assert_eq!(chunks.len(), 3);
    // Preamble: line 1
    assert_eq!(chunks[0].id, "s:d.md:__preamble__");
    assert_eq!(chunks[0].metadata.line_range.start, 1);
    assert_eq!(chunks[0].metadata.line_range.end, 1);
    // Section A: lines 2..=4 (## A, first, second)
    assert_eq!(chunks[1].metadata.line_range.start, 2);
    assert_eq!(chunks[1].metadata.line_range.end, 4);
    // Section B: lines 5..=6
    assert_eq!(chunks[2].metadata.line_range.start, 5);
    assert_eq!(chunks[2].metadata.line_range.end, 6);
}

#[test]
fn metadata_reflects_source_and_commit() {
    // Use H2-only doc so chunks[0] is the section (not a preamble).
    let md = "## A\nfoo\n";
    let when = DateTime::<Utc>::from_timestamp(1700000000, 0).unwrap();
    let chunks = split_markdown(
        md,
        "docs/readme.md",
        "my-src",
        DocSubtype::Readme,
        "abc123",
        when,
    );
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].source, "my-src");
    assert_eq!(chunks[0].metadata.source_path, "docs/readme.md");
    assert_eq!(chunks[0].metadata.commit_sha, "abc123");
    assert_eq!(chunks[0].metadata.indexed_at, when);
    assert_eq!(chunks[0].metadata.language, Some("markdown".into()));
    assert_eq!(chunks[0].provenance.extractor, "markdown-splitter");
}

#[test]
fn section_with_only_whitespace_is_skipped() {
    let md = "# T\n## Empty\n\n\n## Real\ncontent\n";
    let chunks = split_markdown(md, "d.md", "s", DocSubtype::Doc, "c", when());
    // Now: preamble ("# T"), Real (Empty skipped)
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].id, "s:d.md:__preamble__");
    assert_eq!(chunks[1].id, "s:d.md:real");
}

#[test]
fn chunk_kind_is_doc() {
    let md = "## A\nfoo\n";
    let chunks = split_markdown(md, "d.md", "s", DocSubtype::Doc, "c", when());
    assert!(matches!(
        chunks[0].kind,
        super::super::types::ChunkKind::Doc
    ));
}

#[test]
fn mini_rust_readme_fixture_splits() {
    let md = std::fs::read_to_string("tests/fixtures/context/repos/mini-rust/README.md").unwrap();
    let chunks = split_markdown(
        &md,
        "README.md",
        "mini-rust",
        DocSubtype::Readme,
        "abc",
        when(),
    );
    // Fixture has >=2 H2 headings per Task 1.1 spec
    assert!(
        chunks.len() >= 2,
        "got: {:?}",
        chunks.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}
