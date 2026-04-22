use super::plan::{plan_injection, TokenCounter};
use crate::context::config::ContextConfig;
use crate::context::retrieve::{ScoreBreakdown, ScoredChunk};
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn tok_counter() -> Box<TokenCounter> {
    Box::new(|s: &str| s.split_whitespace().count())
}

fn mock_chunk(id: &str, kind: ChunkKind, content: &str) -> Chunk {
    Chunk {
        id: id.into(),
        source: "src".into(),
        kind,
        subtype: None,
        qualified_name: Some(id.into()),
        signature: None,
        content: content.into(),
        metadata: ChunkMeta {
            source_path: "x.rs".into(),
            line_range: LineRange::new(1, 1).unwrap(),
            commit_sha: "c".into(),
            indexed_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
            source_version: None,
            language: Some("rust".into()),
            is_exported: true,
            neighboring_symbols: vec![],
        },
        provenance: Provenance::new("test", 0.9, "x.rs").unwrap(),
    }
}

fn scored(id: &str, kind: ChunkKind, content: &str, score: f32) -> ScoredChunk {
    ScoredChunk {
        chunk: mock_chunk(id, kind, content),
        score,
        components: ScoreBreakdown {
            bm25_norm: 0.0,
            vec_norm: 0.0,
            id_boost: 0.0,
            path_boost: 0.0,
            recency_mul: 1.0,
            score,
        },
        source_legs: vec![],
    }
}

fn config_with(budget: u32, tau: f32, max_chunks: u32) -> ContextConfig {
    ContextConfig {
        auto_inject: true,
        inject_budget_tokens: budget,
        inject_min_score: tau,
        inject_max_chunks: max_chunks,
        rerank_recency_halflife_days: 90,
        rerank_recency_floor: 0.25,
        max_source_size_mb: 100,
        ignore: vec![],
    }
}

/// Build a content string with exactly `n` whitespace-separated tokens.
fn tokens(n: usize) -> String {
    (0..n)
        .map(|i| format!("t{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn empty_hits_yields_empty_plan() {
    let cfg = config_with(1500, 0.65, 4);
    let counter = tok_counter();
    let plan = plan_injection(vec![], vec![], &cfg, &*counter);
    assert!(plan.injected.is_empty());
    assert_eq!(plan.token_count, 0);
    assert!(!plan.adaptive_threshold_applied);
    assert_eq!(plan.below_threshold_count, 0);
}

#[test]
fn below_threshold_never_injected() {
    let cfg = config_with(1500, 0.65, 4);
    let counter = tok_counter();
    let sym = scored("s1", ChunkKind::Symbol, "a b c", 0.5);
    let plan = plan_injection(vec![sym], vec![], &cfg, &*counter);
    assert!(plan.injected.is_empty());
    assert_eq!(plan.below_threshold_count, 1);
}

#[test]
fn symbol_passing_included() {
    // Budget 5, chunk is 3 tokens -> 3/5 = 60% which clears the 40% floor.
    let cfg = config_with(5, 0.65, 4);
    let counter = tok_counter();
    let sym = scored("s1", ChunkKind::Symbol, "a b c", 0.8);
    let plan = plan_injection(vec![sym], vec![], &cfg, &*counter);
    assert_eq!(plan.injected.len(), 1);
    assert_eq!(plan.token_count, 3);
    assert!(!plan.adaptive_threshold_applied);
}

#[test]
fn symbol_starvation_lowers_prose_threshold() {
    // Budget 10, prose at 0.55 is below tau=0.65 but above tau-0.10=0.55.
    // Content is 6 tokens -> 6/10 = 60% clears the 40% floor.
    let cfg = config_with(10, 0.65, 4);
    let counter = tok_counter();
    let prose = scored("p1", ChunkKind::Doc, &tokens(6), 0.55);
    let plan = plan_injection(vec![], vec![prose], &cfg, &*counter);
    assert!(plan.adaptive_threshold_applied);
    assert!((plan.effective_prose_threshold - 0.55).abs() < 1e-6);
    assert_eq!(plan.injected.len(), 1);
    assert_eq!(plan.token_count, 6);
}

#[test]
fn unused_symbol_budget_spills_to_prose() {
    // 2 symbols x 200 = 400, plus 4 prose x 250 = 1000, total 1400 <= 1500.
    let cfg = config_with(1500, 0.65, 10);
    let counter = tok_counter();
    let symbols = vec![
        scored("s1", ChunkKind::Symbol, &tokens(200), 0.8),
        scored("s2", ChunkKind::Symbol, &tokens(200), 0.8),
    ];
    let prose: Vec<_> = (0..4)
        .map(|i| {
            scored(
                &format!("p{i}"),
                ChunkKind::Doc,
                &tokens(250),
                0.7,
            )
        })
        .collect();
    let plan = plan_injection(symbols, prose, &cfg, &*counter);
    assert_eq!(plan.injected.len(), 6);
    assert_eq!(plan.token_count, 1400);
}

#[test]
fn budget_clip_never_splits_a_chunk() {
    // 3 chunks of 800 tokens each, budget 1500. Only chunk 0 fits.
    let cfg = config_with(1500, 0.65, 10);
    let counter = tok_counter();
    let symbols: Vec<_> = (0..3)
        .map(|i| scored(&format!("s{i}"), ChunkKind::Symbol, &tokens(800), 0.9))
        .collect();
    let plan = plan_injection(symbols, vec![], &cfg, &*counter);
    assert_eq!(plan.injected.len(), 1);
    assert_eq!(plan.token_count, 800);
}

#[test]
fn max_chunks_caps_output() {
    // 10 chunks at 50 tokens each, budget 1500, max_chunks 4.
    let cfg = config_with(1500, 0.65, 4);
    let counter = tok_counter();
    let symbols: Vec<_> = (0..10)
        .map(|i| scored(&format!("s{i}"), ChunkKind::Symbol, &tokens(50), 0.9))
        .collect();
    let plan = plan_injection(symbols, vec![], &cfg, &*counter);
    // 4 chunks x 50 tokens = 200, 200/1500 = 13% < 40% floor -> cleared.
    // So to actually test the cap alone, use a bigger budget where 4 chunks
    // fill >= 40%. Here we use max_chunks cap but not floor; assert floor
    // wiped since 200/1500 < 40%.
    assert!(plan.injected.is_empty());
    assert_eq!(plan.token_count, 0);

    // Now test max_chunks cap directly with a small budget so floor passes.
    // 4 * 50 = 200 tokens, budget 300, 200/300 = 66% passes floor.
    let cfg2 = config_with(300, 0.65, 4);
    let symbols2: Vec<_> = (0..10)
        .map(|i| scored(&format!("s{i}"), ChunkKind::Symbol, &tokens(50), 0.9))
        .collect();
    let plan2 = plan_injection(symbols2, vec![], &cfg2, &*counter);
    assert_eq!(plan2.injected.len(), 4);
    assert_eq!(plan2.token_count, 200);
}

#[test]
fn under_40pct_floor_skips_injection() {
    // Budget 1500, floor = 600. One chunk at 100 tokens -> 100 < 600.
    let cfg = config_with(1500, 0.65, 4);
    let counter = tok_counter();
    let sym = scored("s1", ChunkKind::Symbol, &tokens(100), 0.9);
    let plan = plan_injection(vec![sym], vec![], &cfg, &*counter);
    assert!(plan.injected.is_empty());
    assert_eq!(plan.token_count, 0);
}

#[test]
fn exactly_40pct_floor_passes() {
    // Budget 1500, floor = 600. One chunk at 600 tokens -> 600 >= 600.
    let cfg = config_with(1500, 0.65, 4);
    let counter = tok_counter();
    let sym = scored("s1", ChunkKind::Symbol, &tokens(600), 0.9);
    let plan = plan_injection(vec![sym], vec![], &cfg, &*counter);
    assert_eq!(plan.injected.len(), 1);
    assert_eq!(plan.token_count, 600);
}

#[test]
fn below_threshold_count_is_reported() {
    // 3 above tau, 5 below. All symbols, tau=0.65.
    let cfg = config_with(1500, 0.65, 10);
    let counter = tok_counter();
    let mut symbols = Vec::new();
    for i in 0..3 {
        symbols.push(scored(
            &format!("hi{i}"),
            ChunkKind::Symbol,
            &tokens(200),
            0.9,
        ));
    }
    for i in 0..5 {
        symbols.push(scored(
            &format!("lo{i}"),
            ChunkKind::Symbol,
            &tokens(50),
            0.4,
        ));
    }
    let plan = plan_injection(symbols, vec![], &cfg, &*counter);
    assert_eq!(plan.below_threshold_count, 5);
}

#[test]
fn adaptive_threshold_not_applied_when_symbols_pass() {
    let cfg = config_with(1500, 0.65, 4);
    let counter = tok_counter();
    let sym = scored("s1", ChunkKind::Symbol, &tokens(700), 0.8);
    let prose = scored("p1", ChunkKind::Doc, &tokens(300), 0.55);
    let plan = plan_injection(vec![sym], vec![prose], &cfg, &*counter);
    assert!(!plan.adaptive_threshold_applied);
    assert!((plan.effective_prose_threshold - 0.65).abs() < 1e-6);
    // Only the symbol should be in; prose 0.55 < tau 0.65.
    assert_eq!(plan.injected.len(), 1);
    assert_eq!(plan.injected[0].chunk.id, "s1");
}

#[test]
fn preserves_order_within_stream() {
    // 3 symbols in descending score order; all fit.
    let cfg = config_with(1500, 0.65, 10);
    let counter = tok_counter();
    let symbols = vec![
        scored("first", ChunkKind::Symbol, &tokens(200), 0.9),
        scored("second", ChunkKind::Symbol, &tokens(200), 0.8),
        scored("third", ChunkKind::Symbol, &tokens(200), 0.7),
    ];
    let plan = plan_injection(symbols, vec![], &cfg, &*counter);
    assert_eq!(plan.injected.len(), 3);
    assert_eq!(plan.injected[0].chunk.id, "first");
    assert_eq!(plan.injected[1].chunk.id, "second");
    assert_eq!(plan.injected[2].chunk.id, "third");
}

#[test]
fn symbols_precede_prose_in_output() {
    // Prose has a higher score but symbol must still come first.
    let cfg = config_with(1500, 0.65, 10);
    let counter = tok_counter();
    let sym = scored("sym", ChunkKind::Symbol, &tokens(400), 0.7);
    let prose = scored("prose", ChunkKind::Doc, &tokens(400), 0.9);
    let plan = plan_injection(vec![sym], vec![prose], &cfg, &*counter);
    assert_eq!(plan.injected.len(), 2);
    assert!(matches!(plan.injected[0].chunk.kind, ChunkKind::Symbol));
    assert!(matches!(plan.injected[1].chunk.kind, ChunkKind::Doc));
}

#[test]
fn skipping_oversized_chunk_allows_smaller_one() {
    // Budget 1500. Chunks at tokens [1600, 500, 500].
    // Chunk 0 oversized -> skipped. Chunks 1 and 2 both fit.
    let cfg = config_with(1500, 0.65, 10);
    let counter = tok_counter();
    let symbols = vec![
        scored("big", ChunkKind::Symbol, &tokens(1600), 0.9),
        scored("mid1", ChunkKind::Symbol, &tokens(500), 0.8),
        scored("mid2", ChunkKind::Symbol, &tokens(500), 0.7),
    ];
    let plan = plan_injection(symbols, vec![], &cfg, &*counter);
    assert_eq!(plan.injected.len(), 2);
    assert_eq!(plan.token_count, 1000);
    for c in &plan.injected {
        assert_ne!(c.chunk.id, "big");
    }
}
