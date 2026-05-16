use super::multi_source::{BoostContext, SourceBatch, merge_and_rerank};
use super::rerank::ScoreBreakdown;
use super::retriever::{RetrievalLeg, ScoredChunk};
use crate::context::config::MultiSourceConfig;
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};
use chrono::Utc;

fn dummy_chunk(id: &str, source: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: Some(format!("{source}::{id}")),
        signature: None,
        content: format!("fn {id}() {{}}"),
        metadata: ChunkMeta {
            source_path: format!("src/{id}.rs"),
            line_range: LineRange::new(1, 5).unwrap(),
            commit_sha: String::new(),
            indexed_at: Utc::now(),
            source_version: None,
            language: Some("rust".into()),
            is_exported: true,
            neighboring_symbols: vec![],
        },
        provenance: Provenance::new("test", 1.0, "test://").unwrap(),
    }
}

fn scored(id: &str, source: &str, score: f32) -> ScoredChunk {
    ScoredChunk {
        chunk: dummy_chunk(id, source),
        score,
        components: ScoreBreakdown {
            bm25_norm: score * 0.6,
            vec_norm: score * 0.4,
            id_boost: 0.0,
            path_boost: 0.0,
            struct_sim: 0.0,
            recency_mul: 1.0,
            score,
        },
        source_legs: vec![RetrievalLeg::Bm25, RetrievalLeg::Vector],
    }
}

fn default_config() -> MultiSourceConfig {
    MultiSourceConfig::default()
}

fn boost_ctx(current_repo: Option<&str>, dep_sources: &[&str]) -> BoostContext {
    BoostContext {
        current_repo_source: current_repo.map(|s| s.to_string()),
        dep_manifest_sources: dep_sources.iter().map(|s| s.to_string()).collect(),
        reviewed_language: Some("rust".to_string()),
    }
}

#[test]
fn single_source_passes_through() {
    let batch = SourceBatch {
        source_name: "alpha".into(),
        weight: 1.0,
        chunks: vec![scored("a1", "alpha", 0.9), scored("a2", "alpha", 0.7)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    assert_eq!(result.len(), 2);
    assert!(result[0].score >= result[1].score);
}

#[test]
fn multi_source_interleaves_by_score() {
    let b1 = SourceBatch {
        source_name: "alpha".into(),
        weight: 1.0,
        chunks: vec![scored("a1", "alpha", 0.9), scored("a2", "alpha", 0.1)],
    };
    let b2 = SourceBatch {
        source_name: "beta".into(),
        weight: 1.0,
        chunks: vec![scored("b1", "beta", 0.95), scored("b2", "beta", 0.1)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    assert_eq!(result.len(), 4);
    // RRF: rank-1 chunks from both sources get same score (1/61 * 1.1).
    // Tiebreak by chunk id ascending: a1 < b1. Both sources appear.
    let sources: Vec<_> = result.iter().map(|c| c.chunk.source.as_str()).collect();
    assert!(sources.contains(&"alpha"));
    assert!(sources.contains(&"beta"));
}

#[test]
fn current_repo_boost_promotes_chunks() {
    let b1 = SourceBatch {
        source_name: "current".into(),
        weight: 1.0,
        chunks: vec![scored("c1", "current", 0.7)],
    };
    let b2 = SourceBatch {
        source_name: "other".into(),
        weight: 1.0,
        chunks: vec![scored("o1", "other", 0.8)],
    };
    let config = default_config();
    let ctx = boost_ctx(Some("current"), &[]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    // Both rank-1: RRF = 1/61. current gets 1.3*1.1 boost, other gets 1.1 only.
    assert_eq!(result[0].chunk.source, "current");
}

#[test]
fn dep_manifest_boost_promotes_chunks() {
    let b1 = SourceBatch {
        source_name: "dep-lib".into(),
        weight: 1.0,
        chunks: vec![scored("d1", "dep-lib", 0.7)],
    };
    let b2 = SourceBatch {
        source_name: "unrelated".into(),
        weight: 1.0,
        chunks: vec![scored("u1", "unrelated", 0.75)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &["dep-lib"]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    // Both rank-1: RRF = 1/61. dep-lib gets 1.2*1.1 boost, unrelated gets 1.1 only.
    assert_eq!(result[0].chunk.source, "dep-lib");
}

#[test]
fn boosts_compose_multiplicatively() {
    // Use multi-chunk batches so normalization spreads scores
    let b1 = SourceBatch {
        source_name: "my-dep".into(),
        weight: 1.0,
        chunks: vec![scored("md1", "my-dep", 0.5), scored("md2", "my-dep", 0.1)],
    };
    let b2 = SourceBatch {
        source_name: "other".into(),
        weight: 1.0,
        chunks: vec![scored("o1", "other", 0.8), scored("o2", "other", 0.1)],
    };
    let config = default_config();
    // my-dep is both current repo AND a dep AND lang match → 1.3*1.2*1.1 = 1.716
    let ctx = boost_ctx(Some("my-dep"), &["my-dep"]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    // Both rank-1: RRF = 1/61. my-dep gets 1.716x boost, other gets 1.1x.
    assert_eq!(result[0].chunk.source, "my-dep");
    assert!(result[0].score > result[1].score);
}

#[test]
fn per_source_cap_limits_non_current_repo() {
    let mut chunks = Vec::new();
    for i in 0..5 {
        chunks.push(scored(&format!("e{i}"), "external", 0.9 - i as f32 * 0.01));
    }
    let batch = SourceBatch {
        source_name: "external".into(),
        weight: 1.0,
        chunks,
    };
    let mut config = default_config();
    config.per_source_cap = 2;
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    assert_eq!(result.len(), 2);
}

#[test]
fn current_repo_reserved_slots_guaranteed() {
    // current-repo has lower-scoring chunks, but reserved slots guarantee inclusion
    let b_current = SourceBatch {
        source_name: "current".into(),
        weight: 1.0,
        chunks: vec![scored("c1", "current", 0.3), scored("c2", "current", 0.25)],
    };
    let mut other_chunks = Vec::new();
    for i in 0..6 {
        other_chunks.push(scored(&format!("o{i}"), "other", 0.9 - i as f32 * 0.05));
    }
    let b_other = SourceBatch {
        source_name: "other".into(),
        weight: 1.0,
        chunks: other_chunks,
    };
    let mut config = default_config();
    config.current_repo_reserved = 2;
    config.per_source_cap = 3;
    let ctx = boost_ctx(Some("current"), &[]);
    let result = merge_and_rerank(&[b_current, b_other], &config, &ctx, 4);
    let current_count = result
        .iter()
        .filter(|c| c.chunk.source == "current")
        .count();
    assert!(
        current_count >= 2,
        "expected at least 2 current-repo chunks, got {current_count}"
    );
}

#[test]
fn per_source_cap_does_not_apply_to_current_repo() {
    let mut chunks = Vec::new();
    for i in 0..5 {
        chunks.push(scored(&format!("c{i}"), "current", 0.9 - i as f32 * 0.01));
    }
    let batch = SourceBatch {
        source_name: "current".into(),
        weight: 1.0,
        chunks,
    };
    let mut config = default_config();
    config.per_source_cap = 2;
    let ctx = boost_ctx(Some("current"), &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    assert!(
        result.len() > 2,
        "current repo should bypass per_source_cap"
    );
}

#[test]
fn empty_batches_returns_empty() {
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[], &config, &ctx, 10);
    assert!(result.is_empty());
}

#[test]
fn respects_top_k_limit() {
    let mut chunks = Vec::new();
    for i in 0..10 {
        chunks.push(scored(&format!("c{i}"), "src", 0.9 - i as f32 * 0.05));
    }
    let batch = SourceBatch {
        source_name: "src".into(),
        weight: 1.0,
        chunks,
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 3);
    assert_eq!(result.len(), 3);
}

#[test]
fn rrf_single_chunk_source_uses_rank_one() {
    let batch = SourceBatch {
        source_name: "solo".into(),
        weight: 1.0,
        chunks: vec![scored("s1", "solo", 0.5)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    assert_eq!(result.len(), 1);
    // RRF: weight=1.0 / (60 + 1) = 0.01639, lang_match boost 1.1 → ~0.01803
    let expected = 1.0 / 61.0 * 1.1;
    assert!(
        (result[0].score - expected).abs() < 0.001,
        "expected ~{expected:.5}, got {}",
        result[0].score
    );
}

#[test]
fn output_sorted_descending_by_score() {
    let b1 = SourceBatch {
        source_name: "a".into(),
        weight: 1.0,
        chunks: vec![scored("a1", "a", 0.3), scored("a2", "a", 0.9)],
    };
    let b2 = SourceBatch {
        source_name: "b".into(),
        weight: 1.0,
        chunks: vec![scored("b1", "b", 0.6)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    for w in result.windows(2) {
        assert!(
            w[0].score >= w[1].score,
            "not sorted: {} < {}",
            w[0].score,
            w[1].score
        );
    }
}

#[test]
fn candidates_preserve_source_legs() {
    let mut chunk = scored("a1", "alpha", 0.9);
    chunk.source_legs = vec![RetrievalLeg::Structural];
    let batch = SourceBatch {
        source_name: "alpha".into(),
        weight: 1.0,
        chunks: vec![chunk],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    assert_eq!(result[0].source_legs, vec![RetrievalLeg::Structural]);
}

#[test]
fn higher_weight_source_ranks_above_lower_weight() {
    let b1 = SourceBatch {
        source_name: "heavy".into(),
        weight: 3.0,
        chunks: vec![scored("h1", "heavy", 0.5)],
    };
    let b2 = SourceBatch {
        source_name: "light".into(),
        weight: 1.0,
        chunks: vec![scored("l1", "light", 0.9)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    // Both rank-1: heavy gets 3/61, light gets 1/61. Heavy wins regardless of raw score.
    assert_eq!(result[0].chunk.source, "heavy");
    assert_eq!(result[1].chunk.source, "light");
}

#[test]
fn rrf_rank_ordering_within_source() {
    let batch = SourceBatch {
        source_name: "s".into(),
        weight: 1.0,
        chunks: vec![
            scored("r1", "s", 0.9),
            scored("r2", "s", 0.8),
            scored("r3", "s", 0.7),
        ],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    // RRF scores decrease with rank: 1/61 > 1/62 > 1/63
    assert!(result[0].score > result[1].score);
    assert!(result[1].score > result[2].score);
    assert_eq!(result[0].chunk.id, "r1");
}
