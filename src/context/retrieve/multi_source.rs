use std::collections::HashSet;

use super::retriever::ScoredChunk;
use crate::context::config::MultiSourceConfig;

#[derive(Debug, Clone)]
pub struct SourceBatch {
    pub source_name: String,
    pub chunks: Vec<ScoredChunk>,
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
    pub normalized_score: f32,
    pub boosted_score: f32,
    pub is_current_repo: bool,
}

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

        // Filter NaN scores before normalization — they'd corrupt min/max
        // and sort ordering.
        let clean_chunks: Vec<&ScoredChunk> = batch
            .chunks
            .iter()
            .filter(|sc| sc.score.is_finite())
            .collect();
        if clean_chunks.is_empty() {
            continue;
        }

        let (min_score, max_score) = min_max_scores_refs(&clean_chunks);
        let range = max_score - min_score;

        for sc in &clean_chunks {
            let normalized = if range < f32::EPSILON {
                1.0
            } else {
                (sc.score - min_score) / range
            };

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
                normalized_score: normalized,
                boosted_score: normalized * boost,
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

fn min_max_scores_refs(chunks: &[&ScoredChunk]) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for c in chunks {
        if c.score < min {
            min = c.score;
        }
        if c.score > max {
            max = c.score;
        }
    }
    if !min.is_finite() {
        min = 0.0;
    }
    if !max.is_finite() {
        max = 0.0;
    }
    (min, max)
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
