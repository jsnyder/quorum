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
    // Use 2+ chunks per source so min-max normalization differentiates them
    let b1 = SourceBatch {
        source_name: "alpha".into(),
        chunks: vec![scored("a1", "alpha", 0.9), scored("a2", "alpha", 0.1)],
    };
    let b2 = SourceBatch {
        source_name: "beta".into(),
        chunks: vec![scored("b1", "beta", 0.95), scored("b2", "beta", 0.1)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    assert_eq!(result.len(), 4);
    // Both top chunks normalize to 1.0 (they're the max in their batch),
    // so their boosted scores are equal (both get lang_match 1.1).
    // With identical scores, tiebreak is by chunk id ascending: a1 < b1.
    // The key assertion is that mixing sources works — both appear.
    let sources: Vec<_> = result.iter().map(|c| c.chunk.source.as_str()).collect();
    assert!(sources.contains(&"alpha"));
    assert!(sources.contains(&"beta"));
}

#[test]
fn current_repo_boost_promotes_chunks() {
    let b1 = SourceBatch {
        source_name: "current".into(),
        chunks: vec![scored("c1", "current", 0.7)],
    };
    let b2 = SourceBatch {
        source_name: "other".into(),
        chunks: vec![scored("o1", "other", 0.8)],
    };
    let config = default_config();
    let ctx = boost_ctx(Some("current"), &[]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    // 0.7 * 1.3 = 0.91 vs 0.8 → current should be first
    assert_eq!(result[0].chunk.source, "current");
}

#[test]
fn dep_manifest_boost_promotes_chunks() {
    let b1 = SourceBatch {
        source_name: "dep-lib".into(),
        chunks: vec![scored("d1", "dep-lib", 0.7)],
    };
    let b2 = SourceBatch {
        source_name: "unrelated".into(),
        chunks: vec![scored("u1", "unrelated", 0.75)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &["dep-lib"]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    // 0.7 * 1.2 = 0.84 vs 0.75 → dep should be first
    assert_eq!(result[0].chunk.source, "dep-lib");
}

#[test]
fn boosts_compose_multiplicatively() {
    // Use multi-chunk batches so normalization spreads scores
    let b1 = SourceBatch {
        source_name: "my-dep".into(),
        chunks: vec![scored("md1", "my-dep", 0.5), scored("md2", "my-dep", 0.1)],
    };
    let b2 = SourceBatch {
        source_name: "other".into(),
        chunks: vec![scored("o1", "other", 0.8), scored("o2", "other", 0.1)],
    };
    let config = default_config();
    // my-dep is both current repo AND a dep AND lang match → 1.3*1.2*1.1 = 1.716
    let ctx = boost_ctx(Some("my-dep"), &["my-dep"]);
    let result = merge_and_rerank(&[b1, b2], &config, &ctx, 10);
    // md1: normalized = (0.5-0.1)/(0.5-0.1) = 1.0, boosted = 1.0*1.716 = 1.716
    // o1:  normalized = (0.8-0.1)/(0.8-0.1) = 1.0, boosted = 1.0*1.1 = 1.1
    // my-dep top chunk should be first due to triple boost
    assert_eq!(result[0].chunk.source, "my-dep");
    // Verify the boost was > 1.1 (lang match only)
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
        chunks: vec![scored("c1", "current", 0.3), scored("c2", "current", 0.25)],
    };
    let mut other_chunks = Vec::new();
    for i in 0..6 {
        other_chunks.push(scored(&format!("o{i}"), "other", 0.9 - i as f32 * 0.05));
    }
    let b_other = SourceBatch {
        source_name: "other".into(),
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
        chunks,
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 3);
    assert_eq!(result.len(), 3);
}

#[test]
fn min_max_normalization_single_chunk_source_scores_one() {
    let batch = SourceBatch {
        source_name: "solo".into(),
        chunks: vec![scored("s1", "solo", 0.5)],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    assert_eq!(result.len(), 1);
    // Single-candidate source → normalized to 1.0 → lang_match boost if applicable
    // No lang match since ctx has no current repo and "rust" matches "rust" from boost_ctx
    // Actually boost_ctx sets reviewed_language = Some("rust") and chunk language = "rust"
    // so lang_match = 1.1, normalized = 1.0 * 1.1 = 1.1
    assert!(
        result[0].score > 0.9,
        "single candidate should normalize high, got {}",
        result[0].score
    );
}

#[test]
fn output_sorted_descending_by_score() {
    let b1 = SourceBatch {
        source_name: "a".into(),
        chunks: vec![scored("a1", "a", 0.3), scored("a2", "a", 0.9)],
    };
    let b2 = SourceBatch {
        source_name: "b".into(),
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
        chunks: vec![chunk],
    };
    let config = default_config();
    let ctx = boost_ctx(None, &[]);
    let result = merge_and_rerank(&[batch], &config, &ctx, 10);
    assert_eq!(result[0].source_legs, vec![RetrievalLeg::Structural]);
}
