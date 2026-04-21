//! Rerank formula: blend BM25 + vector similarity, apply identifier/language
//! boosts, and multiply by a recency decay factor.
//!
//! Pure function — no I/O. Normalization is min-max per call (within the
//! candidate set).

use chrono::{DateTime, Utc};

/// Per-candidate inputs to the rerank scoring function.
#[derive(Debug, Clone)]
pub struct RerankInput {
    pub chunk_id: String,
    /// Raw BM25 score (higher = better). Zero if this candidate only came
    /// from the vector search.
    pub bm25_raw: f32,
    /// Raw vector similarity: `1.0 - distance` (higher = better). Zero if
    /// this candidate only came from BM25.
    pub vec_raw: f32,
    /// Whether `chunk.qualified_name` matched one of the query identifiers
    /// exactly (case-sensitive).
    pub id_exact_match: bool,
    /// Whether the chunk's `language` equals the reviewed file's language.
    pub language_match: bool,
    /// When the chunk's source was last indexed.
    pub indexed_at: DateTime<Utc>,
}

/// Final score plus its components for telemetry/debugging.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreBreakdown {
    pub bm25_norm: f32,
    pub vec_norm: f32,
    pub id_boost: f32,
    pub path_boost: f32,
    pub recency_mul: f32,
    pub score: f32,
}

/// Rerank configuration. Defaults come from `ContextConfig` downstream.
#[derive(Debug, Clone)]
pub struct RerankConfig {
    pub recency_halflife_days: u32,
    pub recency_floor: f32,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            recency_halflife_days: 90,
            recency_floor: 0.25,
        }
    }
}

const BM25_WEIGHT: f32 = 0.6;
const VEC_WEIGHT: f32 = 0.4;
const ID_BOOST: f32 = 1.0;
const PATH_BOOST: f32 = 0.5;

/// Coerce non-finite raw scores (NaN, +/-inf) to 0.0 before normalization.
fn sanitize(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// Min-max normalize `values`. Ties-at-zero → 0.0 for all. Ties-above-zero →
/// 1.0 for all. Non-finite inputs are treated as 0.0.
fn normalize(values: &[f32]) -> Vec<f32> {
    if values.is_empty() {
        return Vec::new();
    }
    let clean: Vec<f32> = values.iter().map(|v| sanitize(*v)).collect();
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &v in &clean {
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    if (max - min).abs() < f32::EPSILON {
        let fill = if max > 0.0 { 1.0 } else { 0.0 };
        return vec![fill; clean.len()];
    }
    let range = max - min;
    clean.iter().map(|v| (v - min) / range).collect()
}

/// Recency decay: `max(exp(-ln2 * age_days / halflife), floor)`.
/// Negative ages (clock skew) clamp to 0 (→ decay = 1.0).
fn recency_multiplier(
    now: DateTime<Utc>,
    indexed_at: DateTime<Utc>,
    config: &RerankConfig,
) -> f32 {
    let age_days = (now - indexed_at).num_days().max(0) as f32;
    let halflife = config.recency_halflife_days.max(1) as f32;
    let decay = (-std::f32::consts::LN_2 * age_days / halflife).exp();
    let value = if decay.is_finite() { decay } else { config.recency_floor };
    value.max(config.recency_floor)
}

/// Compute the final blended+boosted score for a list of candidates relative
/// to a query time `now`. Returns inputs paired with their final breakdown,
/// order preserved.
pub(crate) fn rerank(
    inputs: &[RerankInput],
    now: DateTime<Utc>,
    config: &RerankConfig,
) -> Vec<(String, ScoreBreakdown)> {
    if inputs.is_empty() {
        return Vec::new();
    }
    // Clamp recency_floor to [0, 1]; a floor > 1.0 would amplify old items
    // above the no-decay baseline.
    let config = RerankConfig {
        recency_halflife_days: config.recency_halflife_days,
        recency_floor: config.recency_floor.clamp(0.0, 1.0),
    };
    let bm25_raw: Vec<f32> = inputs.iter().map(|i| i.bm25_raw).collect();
    let vec_raw: Vec<f32> = inputs.iter().map(|i| i.vec_raw).collect();
    let bm25_norm = normalize(&bm25_raw);
    let vec_norm = normalize(&vec_raw);

    inputs
        .iter()
        .enumerate()
        .map(|(i, input)| {
            let bn = bm25_norm[i];
            let vn = vec_norm[i];
            let id_boost = if input.id_exact_match { ID_BOOST } else { 0.0 };
            let path_boost = if input.language_match { PATH_BOOST } else { 0.0 };
            let blended = BM25_WEIGHT * bn + VEC_WEIGHT * vn;
            let recency_mul = recency_multiplier(now, input.indexed_at, &config);
            let score = (blended + id_boost + path_boost) * recency_mul;
            (
                input.chunk_id.clone(),
                ScoreBreakdown {
                    bm25_norm: bn,
                    vec_norm: vn,
                    id_boost,
                    path_boost,
                    recency_mul,
                    score,
                },
            )
        })
        .collect()
}
