//! Precedence resolution for duplicate `qualified_name` across sources.
//!
//! Tiebreak chain: weight desc -> indexed_at desc -> source asc -> chunk.id asc.
//! The chunk.id tail-breaker ensures determinism when the same source emits
//! two chunks with the same qualified_name (e.g. shadowed definitions).

use std::collections::{BTreeMap, HashMap};

use crate::context::retrieve::ScoredChunk;

/// Source priority (from `SourcesConfig.sources[].weight`). Higher = preferred.
#[derive(Debug, Clone, Default)]
pub struct SourceWeights(HashMap<String, i32>);

impl SourceWeights {
    pub fn new(weights: impl IntoIterator<Item = (String, i32)>) -> Self {
        Self(weights.into_iter().collect())
    }

    pub fn get(&self, source: &str) -> i32 {
        self.0.get(source).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Record of precedence decisions for a single rendering pass.
#[derive(Debug, Clone, Default)]
pub struct PrecedenceLog {
    entries: Vec<PrecedenceEntry>,
}

#[derive(Debug, Clone)]
pub struct PrecedenceEntry {
    pub qualified_name: String,
    pub winner_source: String,
    pub loser_source: String,
    pub reason: String,
}

impl PrecedenceLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[PrecedenceEntry] {
        &self.entries
    }

    pub fn record_winner(
        &mut self,
        qualified_name: impl Into<String>,
        winner: impl Into<String>,
        loser: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.entries.push(PrecedenceEntry {
            qualified_name: qualified_name.into(),
            winner_source: winner.into(),
            loser_source: loser.into(),
            reason: reason.into(),
        });
    }
}

/// Resolve precedence: group input by `qualified_name`, pick a winner per
/// group. Chunks without a qualified_name pass through untouched.
///
/// Tiebreak chain (first non-tie wins):
///   1. `weights.get(source)` desc
///   2. `chunk.metadata.indexed_at` desc
///   3. `chunk.source` asc (alphabetical)
///   4. `chunk.id` asc (stable deterministic fallback)
///
/// Returns `(kept_chunks, log)` with kept_chunks preserving original input order.
pub fn resolve_precedence(
    chunks: Vec<ScoredChunk>,
    weights: &SourceWeights,
) -> (Vec<ScoredChunk>, PrecedenceLog) {
    let mut log = PrecedenceLog::new();

    // First pass: group indices by qualified_name. Skip None (always kept).
    // BTreeMap keeps group iteration deterministic so the PrecedenceLog entry
    // order matches sorted qualified_name regardless of hasher state.
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, c) in chunks.iter().enumerate() {
        if let Some(qname) = c.chunk.qualified_name.as_ref() {
            groups.entry(qname.clone()).or_default().push(idx);
        }
    }

    // For each competition group, pick the winner index and record losers.
    let mut loser_indices: std::collections::HashSet<usize> =
        std::collections::HashSet::new();

    for (qname, indices) in &groups {
        if indices.len() < 2 {
            continue;
        }
        // Pick winner via the tiebreak chain.
        let winner_idx = *indices
            .iter()
            .min_by(|&&a, &&b| compare_chunks(&chunks[a], &chunks[b], weights))
            .expect("group is non-empty");

        let winner = &chunks[winner_idx];
        for &idx in indices {
            if idx == winner_idx {
                continue;
            }
            let loser = &chunks[idx];
            let reason = reason_for(winner, loser, weights);
            log.record_winner(
                qname.clone(),
                winner.chunk.source.clone(),
                loser.chunk.source.clone(),
                reason,
            );
            loser_indices.insert(idx);
        }
    }

    // Second pass: preserve input order, filter out losers.
    let kept: Vec<ScoredChunk> = chunks
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if loser_indices.contains(&i) {
                None
            } else {
                Some(c)
            }
        })
        .collect();

    (kept, log)
}

/// `std::cmp::Ordering` comparator: `a` "less" means `a` ranks before `b`
/// (i.e. `a` is the preferred winner). Using `min_by` with this yields the
/// winner.
fn compare_chunks(
    a: &ScoredChunk,
    b: &ScoredChunk,
    weights: &SourceWeights,
) -> std::cmp::Ordering {
    let wa = weights.get(&a.chunk.source);
    let wb = weights.get(&b.chunk.source);
    // Weight desc: higher weight ranks earlier.
    match wb.cmp(&wa) {
        std::cmp::Ordering::Equal => {}
        non_eq => return non_eq,
    }
    // indexed_at desc: newer ranks earlier.
    match b.chunk.metadata.indexed_at.cmp(&a.chunk.metadata.indexed_at) {
        std::cmp::Ordering::Equal => {}
        non_eq => return non_eq,
    }
    // source asc.
    match a.chunk.source.cmp(&b.chunk.source) {
        std::cmp::Ordering::Equal => {}
        non_eq => return non_eq,
    }
    // chunk.id asc — deterministic fallback.
    a.chunk.id.cmp(&b.chunk.id)
}

fn reason_for(winner: &ScoredChunk, loser: &ScoredChunk, weights: &SourceWeights) -> String {
    let ww = weights.get(&winner.chunk.source);
    let lw = weights.get(&loser.chunk.source);
    if ww != lw {
        return format!("weight {ww} > {lw}");
    }
    let wi = winner.chunk.metadata.indexed_at;
    let li = loser.chunk.metadata.indexed_at;
    if wi != li {
        return format!(
            "indexed {} > {}",
            wi.format("%Y-%m-%dT%H:%M:%SZ"),
            li.format("%Y-%m-%dT%H:%M:%SZ")
        );
    }
    if winner.chunk.source != loser.chunk.source {
        return format!(
            "source '{}' < '{}' (alphabetical)",
            winner.chunk.source, loser.chunk.source
        );
    }
    format!(
        "chunk_id '{}' < '{}' (alphabetical)",
        winner.chunk.id, loser.chunk.id
    )
}
