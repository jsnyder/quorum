use super::plan::InjectionPlan;
use super::render::render_context_block;
use super::stale::{NoStaleness, StalenessAnnotator};
use crate::context::retrieve::{PrecedenceLog, ScoreBreakdown, ScoredChunk};
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn plan_with(chunks: Vec<ScoredChunk>, tokens: usize) -> InjectionPlan {
    InjectionPlan {
        injected: chunks,
        token_count: tokens,
        below_threshold_count: 0,
        effective_prose_threshold: 0.65,
        adaptive_threshold_applied: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn scored(
    id: &str,
    source: &str,
    kind: ChunkKind,
    qname: Option<&str>,
    content: &str,
    lang: &str,
    path: &str,
    start: u32,
    end: u32,
    sha: &str,
) -> ScoredChunk {
    let language = if lang.is_empty() {
        None
    } else {
        Some(lang.to_string())
    };
    let chunk = Chunk {
        id: id.into(),
        source: source.into(),
        kind,
        subtype: None,
        qualified_name: qname.map(String::from),
        signature: None,
        content: content.into(),
        metadata: ChunkMeta {
            source_path: path.into(),
            line_range: LineRange::new(start, end).unwrap(),
            commit_sha: sha.into(),
            indexed_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
            source_version: None,
            language,
            is_exported: true,
            neighboring_symbols: vec![],
        },
        provenance: Provenance::new("test", 0.9, path).unwrap(),
    };
    ScoredChunk {
        chunk,
        score: 0.9,
        components: ScoreBreakdown {
            bm25_norm: 0.0,
            vec_norm: 0.0,
            id_boost: 0.0,
            path_boost: 0.0,
            recency_mul: 1.0,
            score: 0.9,
        },
    }
}

#[test]
fn renders_symbol_card_with_signature_code_fence() {
    let chunk = scored(
        "s1",
        "internal-auth",
        ChunkKind::Symbol,
        Some("verify_token"),
        "pub fn verify_token(x: &str) -> bool { true }",
        "rust",
        "src/token.rs",
        10,
        22,
        "abc1234deadbeef",
    );
    let plan = plan_with(vec![chunk], 12);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("### Symbol: "));
    assert!(out.contains("verify_token"));
    assert!(out.contains("```rust"));
    assert!(out.contains("pub fn verify_token"));
    assert!(out.contains("Source: "));
}

#[test]
fn h2_in_doc_body_is_demoted_to_h4() {
    let chunk = scored(
        "d1",
        "docs",
        ChunkKind::Doc,
        None,
        "## Usage\nSome text",
        "markdown",
        "src/README.md",
        5,
        18,
        "abc1234deadbeef",
    );
    let plan = plan_with(vec![chunk], 4);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(!out.contains("\n## "));
    assert!(out.contains("\n#### Usage"));
}

struct FakeStale;
impl StalenessAnnotator for FakeStale {
    fn annotate(&self, _: &crate::context::types::Chunk) -> Option<String> {
        Some("source has edits since last index (2h ago)".into())
    }
}

#[test]
fn annotates_stale_local_chunks() {
    let chunk = scored(
        "s1",
        "local",
        ChunkKind::Symbol,
        Some("foo"),
        "fn foo() {}",
        "rust",
        "src/lib.rs",
        1,
        2,
        "abc1234deadbeef",
    );
    let plan = plan_with(vec![chunk], 4);
    let out = render_context_block(&plan, &FakeStale, &PrecedenceLog::new());
    assert!(out.contains("WARNING: source has edits since last index"));
}

#[test]
fn shows_precedence_footer_when_suppression_occurred() {
    let chunk = scored(
        "s1",
        "internal-auth",
        ChunkKind::Symbol,
        Some("verify_token"),
        "fn verify_token() {}",
        "rust",
        "src/token.rs",
        1,
        2,
        "abc1234deadbeef",
    );
    let mut log = PrecedenceLog::new();
    log.record_winner(
        "verify_token",
        "internal-auth",
        "internal-auth-fork",
        "weight 10 > 5",
    );
    let plan = plan_with(vec![chunk], 4);
    let out = render_context_block(&plan, &NoStaleness, &log);
    assert!(out.contains("precedence: internal-auth wins over internal-auth-fork"));
}

#[test]
fn footer_reports_token_count_and_chunk_count() {
    let chunks = vec![
        scored(
            "s1",
            "a",
            ChunkKind::Symbol,
            Some("foo"),
            "fn foo(){}",
            "rust",
            "a.rs",
            1,
            2,
            "abc1234",
        ),
        scored(
            "s2",
            "a",
            ChunkKind::Symbol,
            Some("bar"),
            "fn bar(){}",
            "rust",
            "b.rs",
            1,
            2,
            "abc1234",
        ),
    ];
    let plan = plan_with(chunks, 450);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("450 tokens across 2 chunks"));
}

#[test]
fn footer_reports_unique_source_count() {
    let chunks = vec![
        scored(
            "s1",
            "alpha",
            ChunkKind::Symbol,
            Some("a"),
            "fn a(){}",
            "rust",
            "a.rs",
            1,
            2,
            "abc1234",
        ),
        scored(
            "s2",
            "alpha",
            ChunkKind::Symbol,
            Some("b"),
            "fn b(){}",
            "rust",
            "b.rs",
            1,
            2,
            "abc1234",
        ),
        scored(
            "s3",
            "beta",
            ChunkKind::Symbol,
            Some("c"),
            "fn c(){}",
            "rust",
            "c.rs",
            1,
            2,
            "abc1234",
        ),
    ];
    let plan = plan_with(chunks, 90);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("from 2 source(s)"));
}

#[test]
fn footer_mentions_below_threshold_when_nonzero() {
    let chunk = scored(
        "s1",
        "a",
        ChunkKind::Symbol,
        Some("foo"),
        "fn foo(){}",
        "rust",
        "a.rs",
        1,
        2,
        "abc1234",
    );
    let mut plan = plan_with(vec![chunk], 4);
    plan.below_threshold_count = 5;
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("5 candidate(s) below threshold"));
}

#[test]
fn empty_plan_renders_empty_string() {
    let plan = plan_with(vec![], 0);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert_eq!(out, "");
}

#[test]
fn doc_chunk_does_not_have_code_fence() {
    let chunk = scored(
        "d1",
        "docs",
        ChunkKind::Doc,
        None,
        "plain prose",
        "markdown",
        "src/README.md",
        1,
        3,
        "abc1234",
    );
    let plan = plan_with(vec![chunk], 2);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("### Doc:"));
    assert!(!out.contains("```"));
}

#[test]
fn symbol_chunk_without_language_falls_back_to_empty_fence() {
    let chunk = scored(
        "s1",
        "a",
        ChunkKind::Symbol,
        Some("foo"),
        "foo()",
        "",
        "a.txt",
        1,
        2,
        "abc1234",
    );
    let plan = plan_with(vec![chunk], 2);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("```\n"));
}

#[test]
fn short_sha_is_first_7_chars() {
    let chunk = scored(
        "s1",
        "a",
        ChunkKind::Symbol,
        Some("foo"),
        "fn foo(){}",
        "rust",
        "a.rs",
        1,
        2,
        "abcdef1234567890",
    );
    let plan = plan_with(vec![chunk], 4);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("abcdef1"));
    assert!(!out.contains("abcdef12"));
}

#[test]
fn short_sha_handles_sha_under_7_chars() {
    let chunk = scored(
        "s1",
        "a",
        ChunkKind::Symbol,
        Some("foo"),
        "fn foo(){}",
        "rust",
        "a.rs",
        1,
        2,
        "abc",
    );
    let plan = plan_with(vec![chunk], 4);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(out.contains("abc"));
}

#[test]
fn insta_snapshot_of_typical_block() {
    let chunks = vec![
        scored(
            "s1",
            "internal-auth",
            ChunkKind::Symbol,
            Some("verify_token"),
            "pub fn verify_token(x: &str) -> bool { true }",
            "rust",
            "src/token.rs",
            10,
            22,
            "abc1234deadbeef",
        ),
        scored(
            "d1",
            "docs",
            ChunkKind::Doc,
            None,
            "## Overview\nToken verification helpers.",
            "markdown",
            "src/README.md",
            5,
            18,
            "abc1234deadbeef",
        ),
        scored(
            "s2",
            "internal-auth",
            ChunkKind::Symbol,
            Some("refresh_token"),
            "pub fn refresh_token() {}",
            "rust",
            "src/token.rs",
            30,
            35,
            "abc1234deadbeef",
        ),
    ];
    let mut log = PrecedenceLog::new();
    log.record_winner(
        "verify_token",
        "internal-auth",
        "internal-auth-fork",
        "weight 10 > 5",
    );
    let plan = plan_with(chunks, 120);
    let out = render_context_block(&plan, &NoStaleness, &log);
    insta::assert_snapshot!(out);
}

#[test]
fn multiple_cards_separated_by_blank_lines() {
    let chunks = vec![
        scored(
            "s1",
            "a",
            ChunkKind::Symbol,
            Some("a"),
            "fn a(){}",
            "rust",
            "a.rs",
            1,
            2,
            "abc1234",
        ),
        scored(
            "s2",
            "a",
            ChunkKind::Symbol,
            Some("b"),
            "fn b(){}",
            "rust",
            "b.rs",
            1,
            2,
            "abc1234",
        ),
        scored(
            "s3",
            "a",
            ChunkKind::Symbol,
            Some("c"),
            "fn c(){}",
            "rust",
            "c.rs",
            1,
            2,
            "abc1234",
        ),
    ];
    let plan = plan_with(chunks, 12);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    // 3 cards means 3 "\n\n### " occurrences (one after "# Context\n\n",
    // two between subsequent cards). The 2 between-card separations are
    // what this test is about.
    let total = out.matches("\n\n### ").count();
    assert_eq!(
        total - 1,
        2,
        "expected 2 blank-line-separated subsequent headers, got {}\n{out}",
        total - 1
    );
}

#[test]
fn symbol_fence_survives_triple_backticks_in_content() {
    let content = "fn show() { let s = \"```rust\\nhi\\n```\"; }";
    let chunk = scored(
        "s1",
        "s",
        ChunkKind::Symbol,
        Some("show"),
        content,
        "rust",
        "src/x.rs",
        1,
        3,
        "abc1234deadbeef",
    );
    let plan = plan_with(vec![chunk], 10);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(
        out.contains("````rust"),
        "fence must be longer than any backtick run in body: {out}"
    );
    let closing = out.matches("````").count();
    assert!(closing >= 2, "expected matched ```` fence pair: {out}");
}

#[test]
fn short_sha_does_not_panic_on_non_ascii_sha() {
    let chunk = scored(
        "s1",
        "s",
        ChunkKind::Symbol,
        Some("x"),
        "fn x() {}",
        "rust",
        "src/x.rs",
        1,
        1,
        "ééééééé_more",
    );
    let plan = plan_with(vec![chunk], 2);
    let _ = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
}
