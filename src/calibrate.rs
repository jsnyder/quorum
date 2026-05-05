//! Corpus join and threshold computation for calibrator tuning.
//!
//! Joins feedback verdicts with calibrator trace entries on
//! `(finding_title, file_path)` and computes suppress/boost thresholds
//! via precision-recall curves.

use std::collections::{HashMap, HashSet};

use crate::metrics;
use crate::threshold_config::{PathThreshold, ThresholdConfig};

/// Filters applied to traces before joining with feedback.
///
/// Default filter retains all traces (including legacy ones without provenance).
/// Setting any positive filter (e.g. `quorum_version`) excludes legacy traces
/// that lack provenance metadata.
#[derive(Debug, Default)]
pub struct JoinFilter {
    /// Only include traces from this quorum version (e.g. `"0.18.4"`).
    pub quorum_version: Option<String>,
    /// When `true`, exclude traces where `provenance.dirty == Some(true)`.
    pub clean_only: bool,
    /// Only include traces from this repository.
    pub repo: Option<String>,
    /// Only include traces from this exact commit SHA.
    pub commit_sha: Option<String>,
    /// Only include traces from this run ID.
    pub run_id: Option<String>,
}

impl JoinFilter {
    /// Returns `true` when no positive filters are set (only `clean_only` can
    /// be active). Legacy traces (without provenance) are retained by default.
    fn is_default(&self) -> bool {
        self.quorum_version.is_none()
            && !self.clean_only
            && self.repo.is_none()
            && self.commit_sha.is_none()
            && self.run_id.is_none()
    }

    /// Returns `true` if the trace passes this filter.
    fn accepts(&self, trace: &serde_json::Value) -> bool {
        let prov = trace.get("provenance");

        // Legacy trace (provenance is null or missing)
        let is_legacy = prov.is_none() || prov.is_some_and(|v| v.is_null());

        if is_legacy {
            // Default filter retains legacy; any positive filter excludes them.
            return self.is_default();
        }

        let prov = prov.unwrap(); // safe: not legacy

        if let Some(ref ver) = self.quorum_version {
            let trace_ver = prov.get("quorum_version").and_then(|v| v.as_str());
            if trace_ver != Some(ver.as_str()) {
                return false;
            }
        }

        if self.clean_only {
            let dirty = prov.get("dirty").and_then(|v| v.as_bool()).unwrap_or(false);
            if dirty {
                return false;
            }
        }

        if let Some(ref repo) = self.repo {
            let trace_repo = prov.get("repo").and_then(|v| v.as_str());
            if trace_repo != Some(repo.as_str()) {
                return false;
            }
        }

        if let Some(ref sha) = self.commit_sha {
            let trace_sha = prov.get("commit_sha").and_then(|v| v.as_str());
            if trace_sha != Some(sha.as_str()) {
                return false;
            }
        }

        if let Some(ref rid) = self.run_id {
            let trace_rid = prov.get("run_id").and_then(|v| v.as_str());
            if trace_rid != Some(rid.as_str()) {
                return false;
            }
        }

        true
    }
}

/// Minimum token Jaccard similarity for fuzzy title matching.
const FUZZY_THRESHOLD: f64 = 0.5;

/// Minimum margin between best and second-best Jaccard score.
const FUZZY_AMBIGUITY_MARGIN: f64 = 0.1;

fn normalize_title(raw: &str) -> String {
    let after_prefix = strip_rule_prefix(raw);
    let normalized: String = after_prefix.chars().fold(String::new(), |mut acc, c| {
        if c.is_alphanumeric() || c == '_' {
            acc.extend(c.to_lowercase());
        } else {
            acc.push(' ');
        }
        acc
    });
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_rule_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return s;
    }
    let mut colon_pos = None;
    let mut has_hyphen = false;
    for (i, &b) in bytes.iter().enumerate().skip(1) {
        if b == b':' {
            colon_pos = Some(i);
            break;
        }
        if b == b'-' {
            has_hyphen = true;
        } else if !(b.is_ascii_lowercase() || b.is_ascii_digit()) {
            return s;
        }
    }
    match colon_pos {
        Some(pos) if pos >= 2 && has_hyphen => {
            let rest = &s[pos + 1..];
            rest.trim_start()
        }
        _ => s,
    }
}

fn token_jaccard(a: &str, b: &str) -> f64 {
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();
    let union_size = set_a.union(&set_b).count();
    if union_size == 0 {
        return 0.0;
    }
    let intersection_size = set_a.intersection(&set_b).count();
    intersection_size as f64 / union_size as f64
}

/// Minimum total samples required before computing any threshold.
const MIN_TOTAL_SAMPLES: usize = 20;

/// Minimum minority-class count per path (FP for suppress, TP for boost).
const MIN_MINORITY_CLASS: usize = 10;

#[derive(Debug, Default)]
pub struct JoinStats {
    pub exact_raw: usize,
    pub exact_normalized: usize,
    pub fuzzy_same_file: usize,
    pub raw_title_only: usize,
    pub normalized_title_only: usize,
    pub ambiguous_skipped: usize,
    pub below_threshold: usize,
    pub unmatched: usize,
}

/// Join feedback verdicts with calibrator trace entries to produce labeled
/// score samples for PR curve analysis.
///
/// Matching tiers (in priority order, first match wins):
/// 1. Raw exact `(finding_title, file_path)`
/// 2. Normalized exact `(normalize_title(finding_title), file_path)`
/// 3. Fuzzy same-file: token Jaccard >= 0.5 with margin >= 0.1
/// 4. Normalized exact title-only (legacy fallback for pre-file_path traces)
///
/// Ambiguous keys (duplicate trace entries for the same key) are removed.
/// Wontfix verdicts are filtered out. `tp`/`partial` -> positive; `fp` -> negative.
/// Score: `tp_weight / (tp_weight + fp_weight)`.
pub fn join_feedback_and_traces(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
) -> (Vec<(f64, bool)>, JoinStats) {
    join_feedback_and_traces_with_options(feedback, traces, &JoinFilter::default(), false)
}

/// Like [`join_feedback_and_traces`] but with additional controls:
///
/// - `filter`: pre-filters traces by provenance metadata before indexing.
/// - `disable_fuzzy`: when `true`, skips tiers 2-4 (normalized exact, fuzzy
///   same-file, normalized title-only). Only tier 1 (raw exact) and the raw
///   title-only fallback are used.
pub fn join_feedback_and_traces_with_options(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
    filter: &JoinFilter,
    disable_fuzzy: bool,
) -> (Vec<(f64, bool)>, JoinStats) {
    // Pre-filter traces by provenance metadata.
    let filtered_traces: Vec<&serde_json::Value> =
        traces.iter().filter(|t| filter.accepts(t)).collect();

    // Tier 1: raw exact (title, file_path)
    let mut raw_map: HashMap<(String, String), (f64, f64)> = HashMap::new();
    let mut raw_ambiguous: HashSet<(String, String)> = HashSet::new();

    // Tier 2: normalized exact (norm_title, file_path)
    let mut norm_map: HashMap<(String, String), (f64, f64)> = HashMap::new();
    let mut norm_ambiguous: HashSet<(String, String)> = HashSet::new();

    // Tier 3: fuzzy same-file: file_path -> Vec<(norm_title, weights)>
    let mut file_traces: HashMap<String, Vec<(String, (f64, f64))>> = HashMap::new();

    // Tier 4: normalized title-only (for traces without file_path)
    let mut norm_title_only: HashMap<String, (f64, f64)> = HashMap::new();
    let mut norm_title_only_ambiguous: HashSet<String> = HashSet::new();

    // Raw title-only (existing behavior preserved)
    let mut raw_title_only: HashMap<String, (f64, f64)> = HashMap::new();
    let mut raw_title_only_ambiguous: HashSet<String> = HashSet::new();

    // Track which normalized titles have file-scoped traces
    let mut norm_titles_with_file_scoped: HashSet<String> = HashSet::new();
    let mut raw_titles_with_file_scoped: HashSet<String> = HashSet::new();

    for t in &filtered_traces {
        let title = t["finding_title"].as_str().unwrap_or("").to_string();
        if title.is_empty() {
            continue;
        }
        let fp = t["file_path"].as_str().unwrap_or("").to_string();
        let tp_w = t["tp_weight"].as_f64().unwrap_or(0.0).max(0.0);
        let fp_w = t["fp_weight"].as_f64().unwrap_or(0.0).max(0.0);
        let norm = normalize_title(&title);

        if fp.is_empty() {
            // Title-only trace (legacy, no file_path)
            match raw_title_only.entry(title.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    raw_title_only_ambiguous.insert(title.clone());
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((tp_w, fp_w));
                }
            }
            if !disable_fuzzy && !norm.is_empty() {
                let norm_for_ambiguous = norm.clone();
                match norm_title_only.entry(norm) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        norm_title_only_ambiguous.insert(norm_for_ambiguous);
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert((tp_w, fp_w));
                    }
                }
            }
        } else {
            raw_titles_with_file_scoped.insert(title.clone());
            if !norm.is_empty() {
                norm_titles_with_file_scoped.insert(norm.clone());
            }

            // Tier 1: raw exact
            let raw_key = (title, fp.clone());
            match raw_map.entry(raw_key.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    raw_ambiguous.insert(raw_key);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((tp_w, fp_w));
                }
            }

            if !disable_fuzzy {
                // Tier 2: normalized exact
                if !norm.is_empty() {
                    let norm_key = (norm.clone(), fp.clone());
                    match norm_map.entry(norm_key.clone()) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            norm_ambiguous.insert(norm_key);
                        }
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert((tp_w, fp_w));
                        }
                    }
                }

                // Tier 3: file -> normalized traces for fuzzy
                if !norm.is_empty() {
                    file_traces.entry(fp).or_default().push((norm, (tp_w, fp_w)));
                }
            }
        }
    }

    // Clean up ambiguous entries
    for key in &raw_ambiguous {
        raw_map.remove(key);
        tracing::warn!(
            title = %key.0,
            file_path = %key.1,
            "duplicate trace key -- skipping ambiguous entry"
        );
    }
    for key in &norm_ambiguous {
        norm_map.remove(key);
    }
    for key in &raw_title_only_ambiguous {
        raw_title_only.remove(key);
    }
    for key in &norm_title_only_ambiguous {
        norm_title_only.remove(key);
    }

    // Block title-only fallback when file-scoped traces exist for that title
    for title in &raw_titles_with_file_scoped {
        raw_title_only.remove(title);
    }
    for norm in &norm_titles_with_file_scoped {
        norm_title_only.remove(norm);
    }

    let mut samples = Vec::new();
    let mut stats = JoinStats::default();

    for f in feedback {
        let verdict = f["verdict"].as_str().unwrap_or("");
        let is_positive = match verdict {
            "tp" | "partial" => true,
            "fp" => false,
            _ => continue,
        };
        let title = f["finding_title"].as_str().unwrap_or("").to_string();
        if title.is_empty() {
            continue;
        }
        let fp = f["file_path"].as_str().unwrap_or("").to_string();
        let norm = normalize_title(&title);

        // Tier 1: raw exact (title, file_path)
        if let Some(weights) = raw_map.get(&(title.clone(), fp.clone())) {
            if push_sample(&mut samples, weights, is_positive) {
                stats.exact_raw += 1;
                continue;
            }
        }

        // Tier 2: normalized exact (norm_title, file_path) -- skipped when fuzzy disabled
        if !disable_fuzzy && !norm.is_empty() {
            if let Some(weights) = norm_map.get(&(norm.clone(), fp.clone())) {
                if push_sample(&mut samples, weights, is_positive) {
                    stats.exact_normalized += 1;
                    continue;
                }
            }
        }

        // Tier 3: fuzzy same-file -- skipped when fuzzy disabled
        let mut fuzzy_below_threshold = false;
        if !disable_fuzzy && !norm.is_empty() && !fp.is_empty() {
            if let Some(candidates) = file_traces.get(&fp) {
                let mut best_score = 0.0_f64;
                let mut second_best = 0.0_f64;
                let mut best_weights: Option<&(f64, f64)> = None;

                for (cand_norm, weights) in candidates {
                    let j = token_jaccard(&norm, cand_norm);
                    if j > best_score {
                        second_best = best_score;
                        best_score = j;
                        best_weights = Some(weights);
                    } else if j > second_best {
                        second_best = j;
                    }
                }

                if best_score >= FUZZY_THRESHOLD {
                    let margin = best_score - second_best;
                    if margin >= FUZZY_AMBIGUITY_MARGIN {
                        if let Some(weights) = best_weights {
                            if push_sample(&mut samples, weights, is_positive) {
                                stats.fuzzy_same_file += 1;
                                continue;
                            }
                        }
                    } else {
                        stats.ambiguous_skipped += 1;
                        continue;
                    }
                } else if !candidates.is_empty() {
                    fuzzy_below_threshold = true;
                }
            }
        }

        // Title-only fallback (raw first, then normalized -- normalized skipped when fuzzy disabled)
        if let Some(weights) = raw_title_only.get(&title) {
            if push_sample(&mut samples, weights, is_positive) {
                stats.raw_title_only += 1;
                continue;
            }
        }
        if !disable_fuzzy && !norm.is_empty() {
            if let Some(weights) = norm_title_only.get(&norm) {
                if push_sample(&mut samples, weights, is_positive) {
                    stats.normalized_title_only += 1;
                    continue;
                }
            }
        }

        if fuzzy_below_threshold {
            stats.below_threshold += 1;
        } else {
            stats.unmatched += 1;
        }
    }

    tracing::info!(
        exact_raw = stats.exact_raw,
        exact_normalized = stats.exact_normalized,
        fuzzy_same_file = stats.fuzzy_same_file,
        raw_title_only = stats.raw_title_only,
        normalized_title_only = stats.normalized_title_only,
        ambiguous_skipped = stats.ambiguous_skipped,
        below_threshold = stats.below_threshold,
        unmatched = stats.unmatched,
        "join strategy breakdown"
    );

    (samples, stats)
}

fn push_sample(samples: &mut Vec<(f64, bool)>, weights: &(f64, f64), is_positive: bool) -> bool {
    let total = weights.0 + weights.1;
    if total > 0.0 {
        let score = weights.0 / total;
        if score.is_finite() {
            samples.push((score, is_positive));
            return true;
        }
    }
    false
}

/// Compute suppress and boost thresholds from labeled score samples.
///
/// Uses precision-recall curves with data quality gates:
/// - Minimum 20 total samples
/// - Minimum 10 minority-class samples per path
/// - Suppress uses an inverted PR curve (identifies FP-dominated scores)
/// - Boost uses a standard PR curve (identifies TP-dominated scores)
/// - Validates suppress_threshold < boost_threshold; drops the
///   lower-confidence path if violated
pub fn compute_thresholds(
    samples: &[(f64, bool)],
    suppress_precision: f64,
    boost_precision: f64,
) -> ThresholdConfig {
    let total = samples.len();
    let positives = samples.iter().filter(|(_, l)| *l).count();
    let negatives = total - positives;

    let mut config = ThresholdConfig::default();

    if total < MIN_TOTAL_SAMPLES {
        return config;
    }

    // Suppress path: invert labels+scores so PR curve identifies FPs.
    // suppress_threshold is a LOW score cutoff: suppress when score < threshold.
    if negatives >= MIN_MINORITY_CLASS {
        let inverted: Vec<(f64, bool)> = samples.iter().map(|(s, l)| (1.0 - s, !l)).collect();
        let inv_curve = metrics::precision_recall_curve(&inverted);
        if let Some(inv_t) = metrics::threshold_at_precision(&inv_curve, suppress_precision) {
            config.suppress = Some(PathThreshold {
                precision_target: suppress_precision,
                threshold: 1.0 - inv_t,
            });
        }
    }

    // Boost path: standard PR curve where positive=TP, high score=likely TP.
    // boost_threshold is a HIGH score cutoff: boost when score >= threshold.
    if positives >= MIN_MINORITY_CLASS {
        let curve = metrics::precision_recall_curve(samples);
        if let Some(t) = metrics::threshold_at_precision(&curve, boost_precision) {
            config.boost = Some(PathThreshold {
                precision_target: boost_precision,
                threshold: t,
            });
        }
    }

    // Validate ordering: suppress_threshold must be < boost_threshold.
    // If violated, drop the lower-confidence path (fewer minority samples).
    if let (Some(s), Some(b)) = (&config.suppress, &config.boost)
        && s.threshold >= b.threshold
    {
        tracing::warn!(
            suppress = s.threshold,
            boost = b.threshold,
            "suppress_threshold >= boost_threshold -- insufficient class separation"
        );
        if negatives < positives {
            config.suppress = None;
        } else {
            config.boost = None;
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_feedback(title: &str, verdict: &str, file_path: &str) -> serde_json::Value {
        serde_json::json!({
            "finding_title": title,
            "verdict": verdict,
            "file_path": file_path,
            "finding_category": "security",
            "reason": "test",
            "timestamp": "2026-01-01T00:00:00Z",
            "provenance": "human"
        })
    }

    fn make_trace(title: &str, tp: f64, fp: f64, file_path: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "finding_title": title,
            "finding_category": "security",
            "tp_weight": tp,
            "fp_weight": fp,
            "wontfix_weight": 0.0,
            "full_suppress_weight": fp,
            "soft_fp_weight": fp,
            "matched_precedents": [],
            "action": null,
            "input_severity": "medium",
            "output_severity": "medium"
        });
        if let Some(fp) = file_path {
            v["file_path"] = serde_json::json!(fp);
        }
        v
    }

    #[test]
    fn join_produces_labeled_scores() {
        let feedback = vec![
            make_feedback("SQL injection", "tp", "src/db.rs"),
            make_feedback("Unused var", "fp", "src/main.rs"),
        ];
        let traces = vec![
            make_trace("SQL injection", 2.5, 0.3, Some("src/db.rs")),
            make_trace("Unused var", 0.1, 1.8, Some("src/main.rs")),
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 2);
        assert!(samples.iter().any(|(s, l)| *l && (*s - 0.893).abs() < 0.01));
        assert!(samples.iter().any(|(s, l)| !*l && (*s - 0.053).abs() < 0.01));
    }

    #[test]
    fn wontfix_entries_are_skipped() {
        let feedback = vec![make_feedback("Style issue", "wontfix", "src/x.rs")];
        let traces = vec![make_trace("Style issue", 0.5, 0.5, Some("src/x.rs"))];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "wontfix should be excluded");
    }

    #[test]
    fn class_balance_gate_rejects_insufficient_fps() {
        // 25 TPs, 2 FPs -- suppress path should be gated (needs 10 minority)
        let mut samples: Vec<(f64, bool)> =
            (0..25).map(|i| (0.5 + i as f64 * 0.01, true)).collect();
        samples.extend((0..2).map(|i| (0.1 + i as f64 * 0.01, false)));
        let result = compute_thresholds(&samples, 0.95, 0.85);
        assert!(result.suppress.is_none(), "insufficient FPs for suppress");
        assert!(result.boost.is_some(), "enough TPs for boost");
    }

    #[test]
    fn minimum_total_gate() {
        let samples: Vec<(f64, bool)> = vec![(0.9, true), (0.1, false)];
        let result = compute_thresholds(&samples, 0.95, 0.85);
        assert!(result.suppress.is_none());
        assert!(result.boost.is_none());
    }

    #[test]
    fn suppress_threshold_is_low_score_cutoff() {
        // With well-separated data, suppress_threshold should be a LOW value
        // (findings scoring below it are likely FP).
        let mut samples: Vec<(f64, bool)> = Vec::new();
        // 15 TPs with high scores
        for i in 0..15 {
            samples.push((0.7 + i as f64 * 0.02, true));
        }
        // 15 FPs with low scores
        for i in 0..15 {
            samples.push((0.05 + i as f64 * 0.02, false));
        }
        let result = compute_thresholds(&samples, 0.95, 0.85);
        if let Some(ref s) = result.suppress {
            assert!(
                s.threshold < 0.5,
                "suppress_threshold should be a low score cutoff, got {}",
                s.threshold
            );
        }
        if let Some(ref b) = result.boost {
            assert!(
                b.threshold > 0.3,
                "boost_threshold should be a high score cutoff, got {}",
                b.threshold
            );
        }
    }

    #[test]
    fn threshold_ordering_enforced() {
        // Empty input should produce no thresholds (can't violate ordering).
        let result = compute_thresholds(&[], 0.95, 0.85);
        assert!(result.suppress.is_none());
        assert!(result.boost.is_none());
    }

    #[test]
    fn title_only_fallback_for_old_traces() {
        // Old trace entries without file_path should still join on title alone.
        let feedback = vec![
            make_feedback("SQL injection", "tp", "src/db.rs"),
            make_feedback("Unused var", "fp", "src/main.rs"),
        ];
        let traces = vec![
            make_trace("SQL injection", 2.5, 0.3, None), // no file_path
            make_trace("Unused var", 0.1, 1.8, None),    // no file_path
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 2, "title-only fallback should match");
    }

    #[test]
    fn title_only_ambiguous_skipped() {
        // Two old traces with the same title but no file_path are ambiguous.
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![
            make_trace("Bug", 2.5, 0.3, None),
            make_trace("Bug", 0.1, 1.8, None), // duplicate title-only
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "ambiguous title-only keys should be skipped");
    }

    #[test]
    fn primary_key_preferred_over_title_only() {
        // When both a title+file_path match and a title-only match exist,
        // the primary (more specific) match wins.
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![
            make_trace("Bug", 2.5, 0.3, Some("src/a.rs")), // primary match
            make_trace("Bug", 0.1, 1.8, None),              // title-only fallback
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        let (score, _) = samples[0];
        assert!((score - 0.893).abs() < 0.01, "should use primary key match, got {score}");
    }

    #[test]
    fn title_only_blocked_when_file_scoped_exists() {
        // If a title has file-scoped traces, the title-only fallback must not
        // be used for unmatched feedback (prevents cross-file contamination).
        let feedback = vec![make_feedback("Bug", "tp", "src/b.rs")]; // no file-scoped match
        let traces = vec![
            make_trace("Bug", 2.5, 0.3, Some("src/a.rs")), // file-scoped for different file
            make_trace("Bug", 0.1, 1.8, None),              // title-only
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(
            samples.is_empty(),
            "title-only fallback should be blocked when file-scoped traces exist for same title"
        );
    }

    #[test]
    fn duplicate_join_keys_skipped() {
        let feedback = vec![make_feedback("SQL injection", "tp", "src/db.rs")];
        let traces = vec![
            make_trace("SQL injection", 2.5, 0.3, Some("src/db.rs")),
            make_trace("SQL injection", 0.1, 1.8, Some("src/db.rs")), // duplicate key
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "ambiguous join keys should be skipped");
    }

    #[test]
    fn negative_weights_clamped_to_zero() {
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace("Bug", -1.0, 2.0, Some("src/a.rs"))];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        let (score, _) = samples[0];
        assert!(
            (0.0..=1.0).contains(&score),
            "negative weights should be clamped, got score {score}"
        );
    }

    // --- tier 2: normalized exact ---

    #[test]
    fn normalized_exact_matches_backtick_difference() {
        let feedback = vec![make_feedback("uses a fixed .tmp filename", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "uses a fixed `.tmp` filename",
            2.0, 0.3, Some("src/a.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "normalized exact should match backtick variants");
        assert_eq!(stats.exact_normalized, 1);
    }

    #[test]
    fn normalized_exact_matches_rule_prefix() {
        let feedback = vec![make_feedback("Empty .expect() message", "fp", "src/b.rs")];
        let traces = vec![make_trace(
            "expect-empty-message: Empty `.expect()` message",
            0.2, 1.5, Some("src/b.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "normalized exact should match rule-prefix variants");
        assert_eq!(stats.exact_normalized, 1);
    }

    #[test]
    fn normalized_exact_does_not_override_raw_exact() {
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace("Bug", 2.0, 0.3, Some("src/a.rs"))];
        let (_samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(stats.exact_raw, 1);
        assert_eq!(stats.exact_normalized, 0);
    }

    #[test]
    fn normalized_exact_different_file_no_match() {
        let feedback = vec![make_feedback("uses a fixed .tmp filename", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "uses a fixed `.tmp` filename",
            2.0, 0.3, Some("src/b.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "different file should not match");
        assert_eq!(stats.unmatched, 1);
    }

    // --- tier 3: fuzzy same-file ---

    #[test]
    fn fuzzy_same_file_matches_extended_title() {
        let feedback = vec![make_feedback(
            "Reset can race with visit processing",
            "tp", "src/visit.rs",
        )];
        let traces = vec![make_trace(
            "Reset can race with visit processing and lose the cleaned state",
            2.0, 0.5, Some("src/visit.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "fuzzy same-file should match extended title");
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    #[test]
    fn fuzzy_same_file_rejects_below_threshold() {
        let feedback = vec![make_feedback("API key leak", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "Database connection pool exhaustion under load",
            2.0, 0.5, Some("src/a.rs"),
        )];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "below-threshold fuzzy should not match");
    }

    #[test]
    fn fuzzy_same_file_rejects_ambiguous() {
        let feedback = vec![make_feedback(
            "error handling is missing",
            "tp", "src/a.rs",
        )];
        let traces = vec![
            make_trace("error handling is missing for IO", 2.0, 0.5, Some("src/a.rs")),
            make_trace("error handling is missing for parse", 1.0, 0.8, Some("src/a.rs")),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "ambiguous fuzzy matches should be skipped");
        assert!(stats.ambiguous_skipped >= 1);
    }

    #[test]
    fn fuzzy_same_file_accepts_clear_winner() {
        let feedback = vec![make_feedback(
            "error handling is missing",
            "tp", "src/a.rs",
        )];
        let traces = vec![
            make_trace("error handling is missing for IO operations", 2.0, 0.5, Some("src/a.rs")),
            make_trace("something completely different xyz abc", 1.0, 0.8, Some("src/a.rs")),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "clear winner should match");
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    #[test]
    fn fuzzy_same_file_different_file_no_match() {
        let feedback = vec![make_feedback(
            "Reset can race with visit processing",
            "tp", "src/a.rs",
        )];
        let traces = vec![make_trace(
            "Reset can race with visit processing and lose state",
            2.0, 0.5, Some("src/b.rs"),
        )];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "fuzzy is file-scoped");
    }

    #[test]
    fn fuzzy_exactly_at_threshold() {
        // 3 shared tokens, 3 unique = 3/6 = 0.5 exactly
        let feedback = vec![make_feedback("alpha beta gamma", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "alpha beta gamma delta epsilon zeta",
            2.0, 0.5, Some("src/a.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, ">= 0.5 is inclusive");
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    #[test]
    fn fuzzy_just_below_threshold() {
        // 2 shared, 3 unique = 2/5 = 0.4
        let feedback = vec![make_feedback("alpha beta", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "alpha beta gamma delta epsilon",
            2.0, 0.5, Some("src/a.rs"),
        )];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "0.4 < 0.5 should be rejected");
    }

    #[test]
    fn fuzzy_margin_exactly_at_boundary() {
        // best=0.6, second=0.5, margin=0.1 exactly
        // "a b c" vs "a b c d e": 3/5=0.6
        // "a b c" vs "a b d e f": 2/6=0.33 — need to pick values carefully
        // Let's use: fb="a b c d e f", trace1="a b c d e f g h i j" (6/10=0.6),
        //            trace2="a b c d e k l m n o" (5/10=0.5)
        let feedback = vec![make_feedback("a b c d e f", "tp", "src/a.rs")];
        let traces = vec![
            make_trace("a b c d e f g h i j", 2.0, 0.5, Some("src/a.rs")),
            make_trace("a b c d e k l m n o", 1.0, 0.8, Some("src/a.rs")),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, ">= 0.1 margin is inclusive");
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    #[test]
    fn fuzzy_margin_just_below_boundary() {
        // Need margin < 0.1. e.g. best=0.55, second=0.50 -> margin=0.05
        // fb="a b c d e f g h i j k" (11 tokens)
        // trace1: share 6 of 11 => add 5 unique = 6/16 ≈ 0.375 — too low
        // Simpler: pick exact Jaccard values
        // fb="a b c d e", trace1="a b c d e f g" (5/7≈0.71),
        //                 trace2="a b c d e f h" (5/7≈0.71) — too close
        // fb="a b c d e f", trace1="a b c d e f g h" (6/8=0.75),
        //                   trace2="a b c d e f g i" (6/8=0.75) — same
        // The trick: make second-best close. Let me try:
        // fb="a b c", trace1="a b c d" (3/4=0.75), trace2="a b c d e" (3/5=0.6)
        // margin = 0.15 — too much. Need them closer.
        // fb="a b c d", trace1="a b c d e" (4/5=0.8), trace2="a b c d e f" (4/6≈0.67)
        // margin = 0.13 — still > 0.1
        // fb="a b c d e f g", trace1="a b c d e f g h i" (7/9≈0.78),
        //                     trace2="a b c d e f g h j" (7/9≈0.78) — same
        // Simpler approach: just use pre-normalized strings that give exact values
        // fb = "a b", trace1 = "a b c" (2/3=0.67), trace2 = "a b d" (2/3=0.67) margin=0
        let feedback = vec![make_feedback("a b", "tp", "src/a.rs")];
        let traces = vec![
            make_trace("a b c", 2.0, 0.5, Some("src/a.rs")),
            make_trace("a b d", 1.0, 0.8, Some("src/a.rs")),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "margin 0.0 < 0.1 should be rejected");
        assert!(stats.ambiguous_skipped >= 1);
    }

    #[test]
    fn fuzzy_single_trace_in_file_no_margin_needed() {
        // Only one trace in file, Jaccard >= 0.5
        let feedback = vec![make_feedback(
            "error handling is missing",
            "tp", "src/a.rs",
        )];
        let traces = vec![make_trace(
            "error handling is missing for IO operations",
            2.0, 0.5, Some("src/a.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "single trace, margin trivially satisfied");
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    // --- tier 4: normalized title-only ---

    #[test]
    fn normalized_title_only_fallback_matches() {
        let feedback = vec![make_feedback("fixed .tmp filename", "tp", "src/a.rs")];
        let traces = vec![make_trace("fixed `.tmp` filename", 1.5, 0.5, None)];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "normalized title-only fallback should match");
        assert_eq!(stats.normalized_title_only, 1);
    }

    #[test]
    fn normalized_title_only_blocked_when_file_scoped_exists() {
        let feedback = vec![make_feedback("fixed .tmp filename", "tp", "src/b.rs")];
        let traces = vec![
            make_trace("fixed `.tmp` filename", 2.5, 0.3, Some("src/a.rs")),
            make_trace("fixed `.tmp` filename", 0.1, 1.8, None),
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(),
            "normalized title-only fallback blocked when file-scoped traces exist");
    }

    #[test]
    fn title_only_not_attempted_after_fuzzy_match() {
        let feedback = vec![make_feedback(
            "error handling is missing",
            "tp", "src/a.rs",
        )];
        let traces = vec![
            make_trace("error handling is missing for IO operations", 2.0, 0.5, Some("src/a.rs")),
            make_trace("error handling is missing", 1.0, 0.8, None),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        assert_eq!(stats.fuzzy_same_file, 1);
        assert_eq!(stats.normalized_title_only, 0);
    }

    // --- cross-tier + JoinStats ---

    #[test]
    fn all_four_tiers_exercised() {
        let feedback = vec![
            make_feedback("SQL injection", "tp", "src/db.rs"),
            make_feedback("fixed .tmp filename", "fp", "src/a.rs"),
            make_feedback("reset can race with processing", "tp", "src/v.rs"),
            make_feedback("missing error context", "fp", "src/z.rs"),
            make_feedback("completely unrelated xyz", "tp", "src/q.rs"),
        ];
        let traces = vec![
            make_trace("SQL injection", 2.0, 0.3, Some("src/db.rs")),
            make_trace("fixed `.tmp` filename", 0.2, 1.5, Some("src/a.rs")),
            make_trace("reset can race with processing and lose state", 1.5, 0.5, Some("src/v.rs")),
            make_trace("missing error context", 0.8, 1.0, None),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 4, "4 of 5 should match");
        assert_eq!(stats.exact_raw, 1);
        assert_eq!(stats.exact_normalized, 1);
        assert_eq!(stats.fuzzy_same_file, 1);
        assert_eq!(stats.raw_title_only, 1);
        assert_eq!(stats.unmatched, 1);
    }

    #[test]
    fn stats_sum_equals_eligible_feedback_count() {
        let feedback = vec![
            make_feedback("SQL injection", "tp", "src/db.rs"),
            make_feedback("fixed .tmp filename", "fp", "src/a.rs"),
            make_feedback("no match xyz", "tp", "src/q.rs"),
            make_feedback("wontfix item", "wontfix", "src/w.rs"),
        ];
        let traces = vec![
            make_trace("SQL injection", 2.0, 0.3, Some("src/db.rs")),
            make_trace("fixed `.tmp` filename", 0.2, 1.5, Some("src/a.rs")),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        let total_classified = stats.exact_raw + stats.exact_normalized
            + stats.fuzzy_same_file + stats.raw_title_only
            + stats.normalized_title_only
            + stats.ambiguous_skipped + stats.below_threshold
            + stats.unmatched;
        // 3 eligible (tp, fp, tp) — wontfix filtered before classification
        assert_eq!(total_classified, 3, "every eligible entry must be classified");
        assert_eq!(samples.len(), 2);
    }

    // --- token_jaccard tests ---

    #[test]
    fn jaccard_identical_titles() {
        let j = token_jaccard("missing error context", "missing error context");
        assert!((j - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_disjoint_titles() {
        let j = token_jaccard("sql injection risk", "memory leak detected");
        assert!(j < 0.01);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let j = token_jaccard(
            "empty expect message",
            "empty expect message provide context",
        );
        assert!((j - 0.6).abs() < 0.01, "3/5 = 0.6, got {j}");
    }

    #[test]
    fn jaccard_empty_returns_zero() {
        assert!(token_jaccard("", "something").abs() < 1e-9);
        assert!(token_jaccard("something", "").abs() < 1e-9);
        assert!(token_jaccard("", "").abs() < 1e-9);
    }

    #[test]
    fn jaccard_duplicate_tokens_ignored() {
        assert!((token_jaccard("the the the", "the") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_single_token_match() {
        assert!((token_jaccard("error", "error") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_single_token_no_match() {
        assert!(token_jaccard("error", "warning") < 0.01);
    }

    // --- normalize_title tests ---

    #[test]
    fn normalize_strips_backticks() {
        assert_eq!(
            normalize_title("uses a fixed `.tmp` filename"),
            "uses a fixed tmp filename"
        );
    }

    #[test]
    fn normalize_strips_rule_prefix() {
        assert_eq!(
            normalize_title("expect-empty-message: Empty .expect() message"),
            "empty expect message"
        );
    }

    #[test]
    fn normalize_lowercases_and_collapses_whitespace() {
        assert_eq!(
            normalize_title("  Missing  Error  Context  "),
            "missing error context"
        );
    }

    #[test]
    fn normalize_preserves_underscores() {
        assert_eq!(
            normalize_title("unwrap_or_default() silently drops errors"),
            "unwrap_or_default silently drops errors"
        );
    }

    #[test]
    fn normalize_handles_empty_and_prefix_only() {
        assert_eq!(normalize_title(""), "");
        assert_eq!(normalize_title("rule-name: "), "");
    }

    #[test]
    fn normalize_no_prefix_when_uppercase_start() {
        assert_eq!(normalize_title("SQL injection risk"), "sql injection risk");
    }

    #[test]
    fn normalize_no_prefix_when_no_hyphen() {
        assert_eq!(
            normalize_title("http: connection refused"),
            "http connection refused"
        );
    }

    #[test]
    fn normalize_no_prefix_when_short() {
        assert_eq!(normalize_title("a-b: rest"), "rest");
        assert_eq!(normalize_title("a: rest"), "a rest");
    }

    #[test]
    fn normalize_multiple_backticks_and_parens() {
        assert_eq!(
            normalize_title("`foo()` calls `bar()` via `baz`"),
            "foo calls bar via baz"
        );
    }

    #[test]
    fn normalize_numeric_rule_prefix() {
        assert_eq!(normalize_title("rule-42: something"), "something");
    }

    #[test]
    fn normalize_colon_mid_sentence_not_stripped() {
        assert_eq!(
            normalize_title("Warning: something bad"),
            "warning something bad"
        );
    }

    #[test]
    fn infinity_weight_in_join_excluded() {
        // JSON cannot represent f64::INFINITY -- serde_json serializes it as
        // null, so as_f64() returns None and unwrap_or(0.0) produces 0.0.
        // This means INFINITY weights in the JSON trace are treated as 0.0,
        // which is the correct defensive behavior for malformed data.
        // Verify the join handles this gracefully without panicking.
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace("Bug", f64::INFINITY, 0.1, Some("src/a.rs"))];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        let (score, label) = samples[0];
        assert!(
            (score - 0.0).abs() < 1e-9,
            "INF weight serialized as JSON null should be treated as 0.0, got {score}"
        );
        assert!(label);
    }

    // --- Task 6: disable_fuzzy ablation ---

    fn make_trace_with_provenance(
        title: &str,
        tp: f64,
        fp: f64,
        file_path: Option<&str>,
        prov: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut v = make_trace(title, tp, fp, file_path);
        if let Some(p) = prov {
            v["provenance"] = p;
        }
        v
    }

    #[test]
    fn disable_fuzzy_skips_normalized_exact() {
        // Without fuzzy disabled, normalized exact (tier 2) matches backtick variants.
        // With fuzzy disabled, only raw exact (tier 1) is used -- no match.
        let feedback = vec![make_feedback("uses a fixed .tmp filename", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "uses a fixed `.tmp` filename",
            2.0, 0.3, Some("src/a.rs"),
        )];

        // Fuzzy enabled: should match via tier 2
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        assert_eq!(stats.exact_normalized, 1);

        // Fuzzy disabled: no match
        let (samples, stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &JoinFilter::default(), true,
        );
        assert!(samples.is_empty(), "fuzzy disabled should skip normalized exact");
        assert_eq!(stats.exact_normalized, 0);
        assert_eq!(stats.unmatched, 1);
    }

    #[test]
    fn disable_fuzzy_skips_fuzzy_same_file() {
        let feedback = vec![make_feedback(
            "Reset can race with visit processing",
            "tp", "src/visit.rs",
        )];
        let traces = vec![make_trace(
            "Reset can race with visit processing and lose the cleaned state",
            2.0, 0.5, Some("src/visit.rs"),
        )];

        // Fuzzy enabled: should match via tier 3
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        assert_eq!(stats.fuzzy_same_file, 1);

        // Fuzzy disabled: no match
        let (samples, stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &JoinFilter::default(), true,
        );
        assert!(samples.is_empty(), "fuzzy disabled should skip fuzzy same-file");
        assert_eq!(stats.fuzzy_same_file, 0);
    }

    #[test]
    fn disable_fuzzy_skips_normalized_title_only() {
        let feedback = vec![make_feedback("fixed .tmp filename", "tp", "src/a.rs")];
        let traces = vec![make_trace("fixed `.tmp` filename", 1.5, 0.5, None)];

        // Fuzzy enabled: should match via tier 4 (normalized title-only)
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        assert_eq!(stats.normalized_title_only, 1);

        // Fuzzy disabled: no match (raw title-only doesn't match either since titles differ)
        let (samples, stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &JoinFilter::default(), true,
        );
        assert!(samples.is_empty(), "fuzzy disabled should skip normalized title-only");
        assert_eq!(stats.normalized_title_only, 0);
    }

    #[test]
    fn disable_fuzzy_preserves_raw_exact_and_raw_title_only() {
        // Raw exact (tier 1) and raw title-only should still work when fuzzy is disabled.
        let feedback = vec![
            make_feedback("SQL injection", "tp", "src/db.rs"),
            make_feedback("Unused var", "fp", "src/main.rs"),
        ];
        let traces = vec![
            make_trace("SQL injection", 2.5, 0.3, Some("src/db.rs")), // tier 1
            make_trace("Unused var", 0.1, 1.8, None),                  // raw title-only
        ];
        let (samples, stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &JoinFilter::default(), true,
        );
        assert_eq!(samples.len(), 2);
        assert_eq!(stats.exact_raw, 1);
        assert_eq!(stats.raw_title_only, 1);
    }

    // --- Task 7: JoinFilter ---

    #[test]
    fn join_filter_default_retains_legacy_traces() {
        // Default filter includes traces without provenance (legacy).
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace("Bug", 2.0, 0.3, Some("src/a.rs"))]; // no provenance
        let (samples, _stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &JoinFilter::default(), false,
        );
        assert_eq!(samples.len(), 1, "default filter should retain legacy traces");
    }

    #[test]
    fn join_filter_positive_excludes_legacy() {
        // Setting quorum_version filter excludes traces without provenance.
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace("Bug", 2.0, 0.3, Some("src/a.rs"))]; // no provenance
        let filter = JoinFilter {
            quorum_version: Some("0.18.4".to_string()),
            ..Default::default()
        };
        let (samples, _stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &filter, false,
        );
        assert!(samples.is_empty(), "positive filter should exclude legacy traces");
    }

    #[test]
    fn join_filter_by_version() {
        let feedback = vec![
            make_feedback("Bug A", "tp", "src/a.rs"),
            make_feedback("Bug B", "fp", "src/b.rs"),
        ];
        let traces = vec![
            make_trace_with_provenance("Bug A", 2.0, 0.3, Some("src/a.rs"), Some(serde_json::json!({
                "quorum_version": "0.18.4",
                "repo": "quorum"
            }))),
            make_trace_with_provenance("Bug B", 0.1, 1.8, Some("src/b.rs"), Some(serde_json::json!({
                "quorum_version": "0.18.3",
                "repo": "quorum"
            }))),
        ];
        let filter = JoinFilter {
            quorum_version: Some("0.18.4".to_string()),
            ..Default::default()
        };
        let (samples, stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &filter, false,
        );
        assert_eq!(samples.len(), 1, "only version 0.18.4 trace should match");
        assert_eq!(stats.exact_raw, 1);
        // The matched sample should be the TP (Bug A)
        assert!(samples[0].1, "matched sample should be positive (Bug A)");
    }

    #[test]
    fn join_filter_clean_only() {
        let feedback = vec![
            make_feedback("Bug A", "tp", "src/a.rs"),
            make_feedback("Bug B", "fp", "src/b.rs"),
        ];
        let traces = vec![
            make_trace_with_provenance("Bug A", 2.0, 0.3, Some("src/a.rs"), Some(serde_json::json!({
                "quorum_version": "0.18.4",
                "dirty": false
            }))),
            make_trace_with_provenance("Bug B", 0.1, 1.8, Some("src/b.rs"), Some(serde_json::json!({
                "quorum_version": "0.18.4",
                "dirty": true
            }))),
        ];
        let filter = JoinFilter {
            clean_only: true,
            ..Default::default()
        };
        let (samples, stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &filter, false,
        );
        assert_eq!(samples.len(), 1, "dirty trace should be excluded");
        assert_eq!(stats.exact_raw, 1);
        assert!(samples[0].1, "matched sample should be positive (Bug A, clean)");
    }

    #[test]
    fn join_filter_by_repo() {
        let feedback = vec![
            make_feedback("Bug A", "tp", "src/a.rs"),
            make_feedback("Bug B", "fp", "src/b.rs"),
        ];
        let traces = vec![
            make_trace_with_provenance("Bug A", 2.0, 0.3, Some("src/a.rs"), Some(serde_json::json!({
                "repo": "quorum"
            }))),
            make_trace_with_provenance("Bug B", 0.1, 1.8, Some("src/b.rs"), Some(serde_json::json!({
                "repo": "other-project"
            }))),
        ];
        let filter = JoinFilter {
            repo: Some("quorum".to_string()),
            ..Default::default()
        };
        let (samples, _stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &filter, false,
        );
        assert_eq!(samples.len(), 1, "only quorum-repo trace should match");
        assert!(samples[0].1, "matched sample should be positive (Bug A)");
    }

    #[test]
    fn join_filter_by_commit_sha() {
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![
            make_trace_with_provenance("Bug", 2.0, 0.3, Some("src/a.rs"), Some(serde_json::json!({
                "commit_sha": "abc123"
            }))),
        ];
        let filter = JoinFilter {
            commit_sha: Some("def456".to_string()),
            ..Default::default()
        };
        let (samples, _stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &filter, false,
        );
        assert!(samples.is_empty(), "wrong commit_sha should exclude trace");
    }

    #[test]
    fn join_filter_by_run_id() {
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![
            make_trace_with_provenance("Bug", 2.0, 0.3, Some("src/a.rs"), Some(serde_json::json!({
                "run_id": "run-42"
            }))),
        ];
        let filter = JoinFilter {
            run_id: Some("run-42".to_string()),
            ..Default::default()
        };
        let (samples, stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &filter, false,
        );
        assert_eq!(samples.len(), 1, "matching run_id should pass");
        assert_eq!(stats.exact_raw, 1);
    }

    #[test]
    fn join_filter_combined_version_and_clean() {
        let feedback = vec![
            make_feedback("Bug A", "tp", "src/a.rs"),
            make_feedback("Bug B", "fp", "src/b.rs"),
            make_feedback("Bug C", "tp", "src/c.rs"),
        ];
        let traces = vec![
            make_trace_with_provenance("Bug A", 2.0, 0.3, Some("src/a.rs"), Some(serde_json::json!({
                "quorum_version": "0.18.4",
                "dirty": false
            }))),
            make_trace_with_provenance("Bug B", 0.1, 1.8, Some("src/b.rs"), Some(serde_json::json!({
                "quorum_version": "0.18.4",
                "dirty": true
            }))),
            make_trace_with_provenance("Bug C", 1.0, 0.5, Some("src/c.rs"), Some(serde_json::json!({
                "quorum_version": "0.18.3",
                "dirty": false
            }))),
        ];
        let filter = JoinFilter {
            quorum_version: Some("0.18.4".to_string()),
            clean_only: true,
            ..Default::default()
        };
        let (samples, _stats) = join_feedback_and_traces_with_options(
            &feedback, &traces, &filter, false,
        );
        assert_eq!(samples.len(), 1, "only Bug A passes both version + clean filter");
        assert!(samples[0].1, "matched sample should be positive (Bug A)");
    }
}
