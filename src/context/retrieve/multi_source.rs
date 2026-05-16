use std::collections::HashSet;

use super::retriever::ScoredChunk;
use crate::context::config::MultiSourceConfig;

/// Reciprocal Rank Fusion constant. Higher values compress rank differences.
/// k=60 is the standard value from the original RRF paper (Cormack et al. 2009).
const RRF_K: f32 = 60.0;

#[derive(Debug, Clone)]
pub struct SourceBatch {
    pub source_name: String,
    pub chunks: Vec<ScoredChunk>,
    /// Source weight multiplier (from sources.toml). Default 1.0.
    pub weight: f32,
}

#[derive(Debug, Clone)]
pub struct BoostContext {
    pub current_repo_source: Option<String>,
    pub dep_manifest_sources: HashSet<String>,
    pub reviewed_language: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MultiSourceCandidate {
    pub chunk: ScoredChunk,
    pub source_name: String,
    pub rrf_score: f32,
    pub boosted_score: f32,
    pub is_current_repo: bool,
}

/// Merge candidates from multiple sources using Reciprocal Rank Fusion.
///
/// Each chunk's base score is `weight / (RRF_K + rank)` where rank is its
/// 1-indexed position within the source batch (pre-sorted by retriever score).
/// Multiplicative boosts (current-repo, dep-manifest, language-match) are
/// applied on top. Diversity constraints enforce per-source caps and
/// current-repo reserved slots.
pub fn merge_and_rerank(
    batches: &[SourceBatch],
    config: &MultiSourceConfig,
    ctx: &BoostContext,
    top_k: u32,
) -> Vec<ScoredChunk> {
    if batches.is_empty() {
        return Vec::new();
    }

    let mut candidates: Vec<MultiSourceCandidate> = Vec::new();

    for batch in batches {
        let is_current = ctx
            .current_repo_source
            .as_ref()
            .is_some_and(|cr| cr == &batch.source_name);

        let clean_chunks: Vec<&ScoredChunk> = batch
            .chunks
            .iter()
            .filter(|sc| sc.score.is_finite())
            .collect();
        if clean_chunks.is_empty() {
            continue;
        }

        for (rank_0, sc) in clean_chunks.iter().enumerate() {
            let rrf_score = batch.weight / (RRF_K + (rank_0 as f32) + 1.0);

            let mut boost = 1.0f32;
            if is_current {
                boost *= config.current_repo_boost;
            }
            if ctx.dep_manifest_sources.contains(&batch.source_name) {
                boost *= config.dep_manifest_boost;
            }
            let lang_matches = matches!(
                (&sc.chunk.metadata.language, &ctx.reviewed_language),
                (Some(a), Some(b)) if a == b
            );
            if lang_matches {
                boost *= config.lang_match_boost;
            }

            candidates.push(MultiSourceCandidate {
                chunk: (*sc).clone(),
                source_name: batch.source_name.clone(),
                rrf_score,
                boosted_score: rrf_score * boost,
                is_current_repo: is_current,
            });
        }
    }

    candidates.sort_by(|a, b| {
        b.boosted_score
            .partial_cmp(&a.boosted_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk.chunk.id.cmp(&b.chunk.chunk.id))
    });

    apply_diversity_constraints(candidates, config, ctx, top_k as usize)
}

fn apply_diversity_constraints(
    sorted_candidates: Vec<MultiSourceCandidate>,
    config: &MultiSourceConfig,
    ctx: &BoostContext,
    top_k: usize,
) -> Vec<ScoredChunk> {
    let per_source_cap = config.per_source_cap as usize;
    let reserved = config.current_repo_reserved as usize;

    // First pass: fill reserved slots for current repo
    let mut reserved_chunks: Vec<ScoredChunk> = Vec::new();
    let mut reserved_ids: HashSet<String> = HashSet::new();

    if ctx.current_repo_source.is_some() {
        for c in &sorted_candidates {
            if reserved_chunks.len() >= reserved {
                break;
            }
            if c.is_current_repo {
                let mut out = c.chunk.clone();
                out.score = c.boosted_score;
                reserved_chunks.push(out);
                reserved_ids.insert(c.chunk.chunk.id.clone());
            }
        }
    }

    // Second pass: fill remaining slots from all candidates respecting per_source_cap
    let remaining = top_k.saturating_sub(reserved_chunks.len());
    let mut result: Vec<ScoredChunk> = Vec::new();
    let mut source_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    // Count reserved chunks toward current repo's non-cap count
    // (current repo is exempt from per_source_cap)

    for c in &sorted_candidates {
        if result.len() >= remaining {
            break;
        }
        if reserved_ids.contains(&c.chunk.chunk.id) {
            continue;
        }

        let source = &c.source_name;
        let is_current = c.is_current_repo;

        if !is_current {
            let count = source_counts.get(source).copied().unwrap_or(0);
            if count >= per_source_cap {
                continue;
            }
        }

        *source_counts.entry(source.clone()).or_insert(0) += 1;
        let mut out = c.chunk.clone();
        out.score = c.boosted_score;
        result.push(out);
    }

    // Merge reserved + remaining, re-sort by score
    reserved_chunks.extend(result);
    reserved_chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk.id.cmp(&b.chunk.id))
    });
    reserved_chunks.truncate(top_k);
    reserved_chunks
}
