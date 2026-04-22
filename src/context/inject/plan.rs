//! Injection planner: decide which scored chunks to render into the review
//! prompt under a token budget, with adaptive threshold + symbol-first
//! priority and budget spillover from symbol hits to prose candidates.

use crate::context::config::ContextConfig;
use crate::context::retrieve::ScoredChunk;

/// Output of the planner.
#[derive(Debug, Clone)]
pub struct InjectionPlan {
    /// Chunks chosen for injection, in emit order (symbols before prose).
    pub injected: Vec<ScoredChunk>,
    /// Tokens used by the injected set (sum over rendered chunks).
    pub token_count: usize,
    /// Number of input candidates whose score fell below the effective
    /// threshold (symbols below `tau`, prose below
    /// `effective_prose_threshold`).
    pub below_threshold_count: usize,
    /// The threshold actually applied to prose (lowered when symbols starve).
    pub effective_prose_threshold: f32,
    /// Whether adaptive lowering kicked in.
    pub adaptive_threshold_applied: bool,
}

/// Token counter — callers inject a simple estimator for MVP
/// (e.g., `|s| s.split_whitespace().count()`).
pub type TokenCounter = dyn Fn(&str) -> usize;

/// Build an [`InjectionPlan`] from two ranked streams (symbols and prose).
///
/// The planner is pure: no I/O, no allocation outside the output.
#[must_use]
pub fn plan_injection(
    symbol_hits: Vec<ScoredChunk>,
    prose_candidates: Vec<ScoredChunk>,
    config: &ContextConfig,
    token_counter: &TokenCounter,
) -> InjectionPlan {
    let tau = config.inject_min_score;
    let max_chunks = config.inject_max_chunks as usize;
    let budget = config.inject_budget_tokens as usize;

    // Step 2: filter each stream by tau.
    let symbols_passing: Vec<ScoredChunk> = symbol_hits
        .iter()
        .filter(|c| c.score >= tau)
        .cloned()
        .collect();
    let mut prose_passing: Vec<ScoredChunk> = prose_candidates
        .iter()
        .filter(|c| c.score >= tau)
        .cloned()
        .collect();

    // Step 3: adaptive threshold — only lowered when BOTH streams are empty
    // at the primary threshold.
    let mut effective_prose_threshold = tau;
    let mut adaptive_threshold_applied = false;
    if symbols_passing.is_empty() && prose_passing.is_empty() && !prose_candidates.is_empty() {
        let lowered = (tau - 0.10).max(0.0);
        if lowered < tau {
            effective_prose_threshold = lowered;
            adaptive_threshold_applied = true;
            prose_passing = prose_candidates
                .iter()
                .filter(|c| c.score >= effective_prose_threshold)
                .cloned()
                .collect();
        }
    }

    // Step 4: merge streams with symbol priority.
    let mut accepted_candidates: Vec<ScoredChunk> =
        Vec::with_capacity(symbols_passing.len() + prose_passing.len());
    accepted_candidates.extend(symbols_passing);
    accepted_candidates.extend(prose_passing);

    // Step 5: budget clip — never split a chunk; skip oversized chunks and
    // keep walking so a smaller later chunk can still fit.
    let mut injected: Vec<ScoredChunk> = Vec::new();
    let mut token_count: usize = 0;
    for c in accepted_candidates {
        if injected.len() >= max_chunks {
            break;
        }
        let cost = token_counter(&c.chunk.content);
        if token_count + cost > budget {
            continue;
        }
        token_count += cost;
        injected.push(c);
    }

    // Step 6: below_threshold_count over both input lists using the
    // appropriate threshold for each stream.
    let below_symbols = symbol_hits.iter().filter(|c| c.score < tau).count();
    let below_prose = prose_candidates
        .iter()
        .filter(|c| c.score < effective_prose_threshold)
        .count();
    let below_threshold_count = below_symbols + below_prose;

    InjectionPlan {
        injected,
        token_count,
        below_threshold_count,
        effective_prose_threshold,
        adaptive_threshold_applied,
    }
}
