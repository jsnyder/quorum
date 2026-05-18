//! Corpus join and threshold computation for calibrator tuning.
//!
//! Joins feedback verdicts with calibrator trace entries on
//! `(finding_title, file_path)` and computes suppress/boost thresholds
//! via precision-recall curves.

use std::collections::{HashMap, HashSet};

use crate::calibrator_model::{CalibratorModel, ModelMeta, ScoreWeights};
use crate::file_util::{normalize_file_path_deep, path_suffix_eq};
use crate::metrics;
use crate::threshold_config::{PathThreshold, ThresholdConfig};

/// Raw feature vector for a single joined sample, used for weight learning.
#[derive(Debug, Clone)]
pub struct SampleFeatures {
    pub precedent_score: f64,
    pub word_lor: f64,
    pub family_fp_inv: f64,
    pub language_fp_inv: f64,
}

impl SampleFeatures {
    pub fn score(&self, weights: &ScoreWeights) -> f64 {
        weights.score * self.precedent_score
            + weights.word_lor * self.word_lor
            + weights.family_fp_inv * self.family_fp_inv
            + weights.language_fp_inv * self.language_fp_inv
    }
}

/// Expanded feature vector for logistic calibrator (22 dimensions).
/// Field order matches `to_vec()` and `feature_names()`.
#[derive(Debug, Clone)]
pub struct ExpandedFeatures {
    // Precedent decomposition (6)
    pub log1p_tp_weight: f64,
    pub log1p_fp_weight: f64,
    pub precedent_count: f64,
    pub max_similarity: f64,
    pub mean_similarity: f64,
    pub has_no_precedents: f64,
    // Weight accumulators (3)
    pub log1p_soft_fp_weight: f64,
    pub log1p_full_suppress_weight: f64,
    pub log1p_wontfix_weight: f64,
    // Smoothed priors (3)
    pub category_fp_rate: f64,
    pub severity_fp_rate: f64,
    pub model_fp_rate: f64,
    // Text statistics (3)
    pub max_word_lor: f64,
    pub min_word_lor: f64,
    pub count_negative_lor_tokens: f64,
    // Structural features (7)
    pub is_test_file: f64,
    pub source_is_ast: f64,
    pub finding_count_same_file: f64,
    pub file_fp_rate: f64,
    pub finding_span_lines: f64,
    pub is_mock_or_fixture: f64,
    pub is_generated_or_vendor: f64,
}

impl ExpandedFeatures {
    /// Convert to ordered `Vec<f64>` matching `feature_names()` order.
    pub fn to_vec(&self) -> Vec<f64> {
        vec![
            self.log1p_tp_weight,
            self.log1p_fp_weight,
            self.precedent_count,
            self.max_similarity,
            self.mean_similarity,
            self.has_no_precedents,
            self.log1p_soft_fp_weight,
            self.log1p_full_suppress_weight,
            self.log1p_wontfix_weight,
            self.category_fp_rate,
            self.severity_fp_rate,
            self.model_fp_rate,
            self.max_word_lor,
            self.min_word_lor,
            self.count_negative_lor_tokens,
            self.is_test_file,
            self.source_is_ast,
            self.finding_count_same_file,
            self.file_fp_rate,
            self.finding_span_lines,
            self.is_mock_or_fixture,
            self.is_generated_or_vendor,
        ]
    }

    /// Feature names matching `to_vec()` order.
    pub fn feature_names() -> Vec<&'static str> {
        vec![
            "log1p_tp_weight",
            "log1p_fp_weight",
            "precedent_count",
            "max_similarity",
            "mean_similarity",
            "has_no_precedents",
            "log1p_soft_fp_weight",
            "log1p_full_suppress_weight",
            "log1p_wontfix_weight",
            "category_fp_rate",
            "severity_fp_rate",
            "model_fp_rate",
            "max_word_lor",
            "min_word_lor",
            "count_negative_lor_tokens",
            "is_test_file",
            "source_is_ast",
            "finding_count_same_file",
            "file_fp_rate",
            "finding_span_lines",
            "is_mock_or_fixture",
            "is_generated_or_vendor",
        ]
    }

    /// All zeros (useful for tests).
    pub fn zeros() -> Self {
        Self {
            log1p_tp_weight: 0.0,
            log1p_fp_weight: 0.0,
            precedent_count: 0.0,
            max_similarity: 0.0,
            mean_similarity: 0.0,
            has_no_precedents: 0.0,
            log1p_soft_fp_weight: 0.0,
            log1p_full_suppress_weight: 0.0,
            log1p_wontfix_weight: 0.0,
            category_fp_rate: 0.0,
            severity_fp_rate: 0.0,
            model_fp_rate: 0.0,
            max_word_lor: 0.0,
            min_word_lor: 0.0,
            count_negative_lor_tokens: 0.0,
            is_test_file: 0.0,
            source_is_ast: 0.0,
            finding_count_same_file: 0.0,
            file_fp_rate: 0.0,
            finding_span_lines: 0.0,
            is_mock_or_fixture: 0.0,
            is_generated_or_vendor: 0.0,
        }
    }
}

/// Univariate feature screening: returns indices of features with
/// stepwise AP >= baseline_ap + 0.02 (the minimum lift threshold).
///
/// Each feature is evaluated independently as a predictor of the positive class.
/// `baseline_ap` is typically the class prevalence (proportion of positives).
pub fn univariate_screen(samples: &[(ExpandedFeatures, bool)], baseline_ap: f64) -> Vec<usize> {
    let threshold = baseline_ap + 0.02;
    let n_features = ExpandedFeatures::feature_names().len(); // 22

    // Pre-compute feature matrix to avoid repeated to_vec() allocations (N*22 -> N).
    let matrix: Vec<Vec<f64>> = samples.iter().map(|(f, _)| f.to_vec()).collect();
    let labels: Vec<bool> = samples.iter().map(|(_, l)| *l).collect();
    let mut selected = Vec::new();

    for feat_idx in 0..n_features {
        let univariate: Vec<(f64, bool)> = matrix
            .iter()
            .zip(labels.iter())
            .map(|(row, &label)| (row[feat_idx], label))
            .collect();
        let ap = crate::metrics::average_precision_stepwise(&univariate);
        if ap >= threshold {
            selected.push(feat_idx);
        }
    }

    selected
}

/// Compute per-feature univariate AP scores for diagnostics.
/// Returns (feature_index, ap_score) pairs sorted by AP descending.
pub fn feature_importance_scores(samples: &[(ExpandedFeatures, bool)]) -> Vec<(usize, f64)> {
    let n_features = ExpandedFeatures::feature_names().len();

    // Pre-compute feature matrix to avoid repeated to_vec() allocations.
    let matrix: Vec<Vec<f64>> = samples.iter().map(|(f, _)| f.to_vec()).collect();
    let labels: Vec<bool> = samples.iter().map(|(_, l)| *l).collect();

    let mut scores: Vec<(usize, f64)> = (0..n_features)
        .map(|feat_idx| {
            let univariate: Vec<(f64, bool)> = matrix
                .iter()
                .zip(labels.iter())
                .map(|(row, &label)| (row[feat_idx], label))
                .collect();
            (
                feat_idx,
                crate::metrics::average_precision_stepwise(&univariate),
            )
        })
        .collect();
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scores
}

/// Filters applied to traces before joining with feedback.
///
/// Default filter retains all traces (including legacy ones without provenance).
/// Setting any positive filter (e.g. `quorum_version`) excludes legacy traces
/// that lack provenance metadata.
#[derive(Debug, Default)]
pub struct JoinFilter {
    /// Only include traces from this quorum version (e.g. `"0.18.4"`).
    pub quorum_version: Option<String>,
    /// When `true`, only include traces with `provenance.dirty == Some(false)`.
    /// Traces with unknown or missing dirty state are excluded.
    pub clean_only: bool,
    /// Only include traces from this repository.
    pub repo: Option<String>,
    /// Only include traces from this exact commit SHA.
    pub commit_sha: Option<String>,
    /// Only include traces from this run ID.
    pub run_id: Option<String>,
}

impl JoinFilter {
    /// Returns `true` when all filters are at their default values. Legacy
    /// traces (without provenance) are retained only when this returns true.
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
            let dirty = prov.get("dirty").and_then(|v| v.as_bool());
            if dirty != Some(false) {
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
    pub path_normalized: usize,
    pub suffix_matched: usize,
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
#[allow(clippy::type_complexity)]
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

    // Tier 2.5: deep-normalized path (norm_title, deep_norm_path) — #307
    let mut deep_path_map: HashMap<(String, String), (f64, f64)> = HashMap::new();
    let mut deep_path_ambiguous: HashSet<(String, String)> = HashSet::new();

    // Tier 3: fuzzy same-file: file_path -> Vec<(norm_title, weights)>
    let mut file_traces: HashMap<String, Vec<(String, (f64, f64))>> = HashMap::new();

    // Tier 3.5: suffix index — filename -> Vec<(deep_norm_path, norm_title, weights)> — #307
    let mut suffix_index: HashMap<String, Vec<(String, String, (f64, f64))>> = HashMap::new();

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
        let deep_fp = normalize_file_path_deep(&fp);

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

                // Tier 2.5: deep-normalized path (#307)
                if !norm.is_empty() && !deep_fp.is_empty() {
                    let deep_key = (norm.clone(), deep_fp.clone());
                    match deep_path_map.entry(deep_key.clone()) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            deep_path_ambiguous.insert(deep_key);
                        }
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert((tp_w, fp_w));
                        }
                    }
                }

                // Tier 3: file -> normalized traces for fuzzy
                if !norm.is_empty() {
                    file_traces
                        .entry(fp)
                        .or_default()
                        .push((norm.clone(), (tp_w, fp_w)));
                }

                // Tier 3.5: suffix index (#307)
                if !norm.is_empty() && !deep_fp.is_empty() {
                    let filename = deep_fp.rsplit('/').next().unwrap_or("").to_string();
                    if !filename.is_empty() {
                        suffix_index.entry(filename).or_default().push((
                            deep_fp,
                            norm,
                            (tp_w, fp_w),
                        ));
                    }
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
    for key in &deep_path_ambiguous {
        deep_path_map.remove(key);
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
        let deep_fp = normalize_file_path_deep(&fp);

        // Tier 1: raw exact (title, file_path)
        if let Some(weights) = raw_map.get(&(title.clone(), fp.clone()))
            && push_sample(&mut samples, weights, is_positive)
        {
            stats.exact_raw += 1;
            continue;
        }

        // Tier 2: normalized exact (norm_title, file_path) -- skipped when fuzzy disabled
        if !disable_fuzzy
            && !norm.is_empty()
            && let Some(weights) = norm_map.get(&(norm.clone(), fp.clone()))
            && push_sample(&mut samples, weights, is_positive)
        {
            stats.exact_normalized += 1;
            continue;
        }

        // Tier 2.5: deep-normalized path (norm_title, deep_norm_path) -- #307
        if !disable_fuzzy
            && !norm.is_empty()
            && !deep_fp.is_empty()
            && let Some(weights) = deep_path_map.get(&(norm.clone(), deep_fp.clone()))
            && push_sample(&mut samples, weights, is_positive)
        {
            stats.path_normalized += 1;
            continue;
        }

        // Tier 3: fuzzy same-file -- skipped when fuzzy disabled
        let mut fuzzy_below_threshold = false;
        if !disable_fuzzy
            && !norm.is_empty()
            && !fp.is_empty()
            && let Some(candidates) = file_traces.get(&fp)
        {
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
                    if let Some(weights) = best_weights
                        && push_sample(&mut samples, weights, is_positive)
                    {
                        stats.fuzzy_same_file += 1;
                        continue;
                    }
                } else {
                    stats.ambiguous_skipped += 1;
                    continue;
                }
            } else if !candidates.is_empty() {
                fuzzy_below_threshold = true;
            }
        }

        // Tier 3.5: suffix match -- catches absolute vs relative paths (#307)
        if !disable_fuzzy && !norm.is_empty() && !deep_fp.is_empty() {
            let filename = deep_fp.rsplit('/').next().unwrap_or("");
            if let Some(candidates) = suffix_index.get(filename) {
                let matches: Vec<&(f64, f64)> = candidates
                    .iter()
                    .filter(|(cand_path, cand_norm, _)| {
                        *cand_norm == norm && path_suffix_eq(cand_path, &deep_fp)
                    })
                    .map(|(_, _, w)| w)
                    .collect();
                if matches.len() == 1 && push_sample(&mut samples, matches[0], is_positive) {
                    stats.suffix_matched += 1;
                    continue;
                }
            }
        }

        // Title-only fallback (raw first, then normalized -- normalized skipped when fuzzy disabled)
        if let Some(weights) = raw_title_only.get(&title)
            && push_sample(&mut samples, weights, is_positive)
        {
            stats.raw_title_only += 1;
            continue;
        }
        if !disable_fuzzy
            && !norm.is_empty()
            && let Some(weights) = norm_title_only.get(&norm)
            && push_sample(&mut samples, weights, is_positive)
        {
            stats.normalized_title_only += 1;
            continue;
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
        path_normalized = stats.path_normalized,
        suffix_matched = stats.suffix_matched,
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

/// Stats from a `backfill_file_paths` run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackfillStats {
    /// Traces that already had file_path (skipped).
    pub already_present: usize,
    /// Backfilled via unambiguous feedback title match.
    pub feedback_exact: usize,
    /// Backfilled via unambiguous normalized feedback title match.
    pub feedback_normalized: usize,
    /// Backfilled via unambiguous matched_precedents file_path.
    pub precedent_inferred: usize,
    /// Ambiguous (2+ candidate files) — left as null.
    pub ambiguous: usize,
    /// No signal found — left as null.
    pub no_match: usize,
    /// Total traces modified.
    pub total_backfilled: usize,
}

/// Enrich legacy traces that lack `file_path` by cross-referencing the
/// feedback corpus.
pub fn backfill_file_paths(
    traces: &mut [serde_json::Value],
    feedback: &[serde_json::Value],
) -> BackfillStats {
    let mut exact_map: HashMap<String, HashSet<String>> = HashMap::new();
    let mut norm_map: HashMap<String, HashSet<String>> = HashMap::new();

    for f in feedback {
        let title = f["finding_title"].as_str().unwrap_or("");
        let fp = f["file_path"].as_str().unwrap_or("");
        if title.is_empty() || fp.is_empty() {
            continue;
        }
        exact_map
            .entry(title.to_string())
            .or_default()
            .insert(fp.to_string());
        let norm = normalize_title(title);
        if !norm.is_empty() {
            norm_map.entry(norm).or_default().insert(fp.to_string());
        }
    }

    let mut stats = BackfillStats::default();

    for trace in traces.iter_mut() {
        let existing = trace
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !existing.is_empty() {
            stats.already_present += 1;
            continue;
        }

        let title = trace["finding_title"].as_str().unwrap_or("").to_string();
        if title.is_empty() {
            stats.no_match += 1;
            continue;
        }

        // Tier 1: exact title match
        if let Some(files) = exact_map.get(&title)
            && files.len() == 1
        {
            let fp = files.iter().next().unwrap().clone();
            trace["file_path"] = serde_json::Value::String(fp);
            stats.feedback_exact += 1;
            stats.total_backfilled += 1;
            continue;
        }

        // Tier 2: normalized title match
        let norm = normalize_title(&title);
        if !norm.is_empty()
            && let Some(files) = norm_map.get(&norm)
            && files.len() == 1
        {
            let fp = files.iter().next().unwrap().clone();
            trace["file_path"] = serde_json::Value::String(fp);
            stats.feedback_normalized += 1;
            stats.total_backfilled += 1;
            continue;
        }

        // Tier 3: precedent inference
        if let Some(precs) = trace.get("matched_precedents").and_then(|v| v.as_array()) {
            let prec_files: HashSet<&str> = precs
                .iter()
                .filter_map(|p| p["file_path"].as_str())
                .filter(|s| !s.is_empty())
                .collect();
            if prec_files.len() == 1 {
                let fp = prec_files.into_iter().next().unwrap().to_string();
                trace["file_path"] = serde_json::Value::String(fp);
                stats.precedent_inferred += 1;
                stats.total_backfilled += 1;
                continue;
            }
        }

        // Determine whether it was ambiguous or truly no match
        let had_exact = exact_map.get(&title).is_some_and(|f| f.len() > 1);
        let had_norm = !norm.is_empty() && norm_map.get(&norm).is_some_and(|f| f.len() > 1);
        if had_exact || had_norm {
            stats.ambiguous += 1;
        } else {
            stats.no_match += 1;
        }
    }

    stats
}

/// Minimum support (total TP+FP) for a word to be included in the word_lor vocabulary.
const WORD_MIN_SUPPORT: usize = 5;

/// Minimum support (total entries) for a family to get its own FP rate.
const FAMILY_MIN_SUPPORT: usize = 3;

/// Minimum support (total entries) for a language to get its own FP rate.
const LANG_MIN_SUPPORT: usize = 5;

/// Compute a `CalibratorModel` from feedback entries.
///
/// Builds lookup tables for word log-odds, family FP rates, and language FP
/// rates from the feedback corpus. Wontfix verdicts are treated as negative
/// (alongside FP) for the purpose of FP rate computation.
/// Build lookup tables from the feedback corpus.
///
/// `wontfix` is treated as negative (alongside `fp`) because it signals
/// findings that are valid but not worth fixing — useful FP-rate signal for
/// suppression. This differs from threshold fitting which excludes wontfix
/// to keep the PR-curve clean.
pub fn compute_calibrator_model(feedback: &[serde_json::Value]) -> Option<CalibratorModel> {
    let mut word_tp: HashMap<String, usize> = HashMap::new();
    let mut word_fp: HashMap<String, usize> = HashMap::new();
    let mut family_tp: HashMap<String, usize> = HashMap::new();
    let mut family_fp: HashMap<String, usize> = HashMap::new();
    let mut lang_tp: HashMap<String, usize> = HashMap::new();
    let mut lang_fp: HashMap<String, usize> = HashMap::new();
    let mut total_tp: usize = 0;
    let mut total_fp: usize = 0;

    for entry in feedback {
        let verdict = entry["verdict"].as_str().unwrap_or("");
        let is_positive = match verdict {
            "tp" | "partial" => true,
            "fp" | "wontfix" => false,
            _ => continue,
        };
        let title = entry["finding_title"].as_str().unwrap_or("");
        if title.is_empty() {
            continue;
        }
        let file_path = entry["file_path"].as_str().unwrap_or("");

        if is_positive {
            total_tp += 1;
        } else {
            total_fp += 1;
        }

        // Word counts
        let words = tokenize_title(title);
        for w in &words {
            if is_positive {
                *word_tp.entry(w.clone()).or_default() += 1;
            } else {
                *word_fp.entry(w.clone()).or_default() += 1;
            }
        }

        // Family counts
        let family = CalibratorModel::title_family(title);
        if !family.is_empty() {
            if is_positive {
                *family_tp.entry(family.clone()).or_default() += 1;
            } else {
                *family_fp.entry(family).or_default() += 1;
            }
        }

        // Language counts
        if !file_path.is_empty() {
            let lang = CalibratorModel::file_ext_language(file_path).to_string();
            if is_positive {
                *lang_tp.entry(lang.clone()).or_default() += 1;
            } else {
                *lang_fp.entry(lang).or_default() += 1;
            }
        }
    }

    let total = total_tp + total_fp;
    if total == 0 {
        return None;
    }

    let global_fp_rate = total_fp as f64 / total as f64;

    // Word log-odds only make sense once both classes have support.
    let eps = 0.5; // Laplace smoothing
    let mut word_lor_map: HashMap<String, f64> = HashMap::new();
    if total_tp > 0 && total_fp > 0 {
        let all_words: HashSet<&String> = word_tp.keys().chain(word_fp.keys()).collect();
        for w in all_words {
            let tp_count = word_tp.get(w).copied().unwrap_or(0);
            let fp_count = word_fp.get(w).copied().unwrap_or(0);
            let support = tp_count + fp_count;
            if support < WORD_MIN_SUPPORT {
                continue;
            }
            let tp_rate = (tp_count as f64 + eps) / (total_tp as f64 + eps);
            let fp_rate = (fp_count as f64 + eps) / (total_fp as f64 + eps);
            let lor = (fp_rate / tp_rate).ln();
            if lor.is_finite() {
                word_lor_map.insert(w.clone(), lor);
            }
        }
    }

    // Family FP rates
    let mut family_fp_rate_map: HashMap<String, f64> = HashMap::new();
    let all_families: HashSet<&String> = family_tp.keys().chain(family_fp.keys()).collect();
    for fam in all_families {
        let tp = family_tp.get(fam).copied().unwrap_or(0);
        let fp = family_fp.get(fam).copied().unwrap_or(0);
        let support = tp + fp;
        if support < FAMILY_MIN_SUPPORT {
            continue;
        }
        family_fp_rate_map.insert(fam.clone(), fp as f64 / support as f64);
    }

    // Language FP rates
    let mut lang_fp_rate_map: HashMap<String, f64> = HashMap::new();
    let all_langs: HashSet<&String> = lang_tp.keys().chain(lang_fp.keys()).collect();
    for lang in all_langs {
        let tp = lang_tp.get(lang).copied().unwrap_or(0);
        let fp = lang_fp.get(lang).copied().unwrap_or(0);
        let support = tp + fp;
        if support < LANG_MIN_SUPPORT {
            continue;
        }
        lang_fp_rate_map.insert(lang.clone(), fp as f64 / support as f64);
    }

    Some(CalibratorModel {
        meta: ModelMeta {
            computed_at: chrono::Utc::now().to_rfc3339(),
            feedback_count: total,
            global_fp_rate,
            learned_weights: None,
        },
        weights: ScoreWeights {
            score: 0.5,
            word_lor: 1.5,
            family_fp_inv: 1.0,
            language_fp_inv: 0.5,
        },
        logistic_model: None,
        word_lor: word_lor_map,
        family_fp_rate: family_fp_rate_map,
        language_fp_rate: lang_fp_rate_map,
        category_fp_rate_map: None,
        severity_fp_rate: None,
        model_fp_rate: None,
        file_fp_rate: None,
        file_finding_counts: None,
    })
}

/// Re-score join samples using composite scores from a model.
///
/// Thin wrapper over [`extract_join_features`] that applies the model's
/// weights to produce final composite scores.
pub fn rescore_samples_with_model(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
    model: &CalibratorModel,
    filter: &JoinFilter,
    disable_fuzzy: bool,
) -> Vec<(f64, bool)> {
    extract_join_features(feedback, traces, model, filter, disable_fuzzy)
        .into_iter()
        .filter_map(|(feat, label)| {
            let s = feat.score(&model.weights);
            s.is_finite().then_some((s, label))
        })
        .collect()
}

/// Extract raw feature vectors for each joined sample.
///
/// Walks the same multi-tier join as the threshold computation, but returns
/// per-sample feature vectors instead of composite scores. Used by
/// [`learn_weights`] for grid search and by [`rescore_samples_with_model`].
pub fn extract_join_features(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
    model: &CalibratorModel,
    filter: &JoinFilter,
    disable_fuzzy: bool,
) -> Vec<(SampleFeatures, bool)> {
    let filtered_traces: Vec<&serde_json::Value> =
        traces.iter().filter(|t| filter.accepts(t)).collect();

    // Build the same index structures as join_feedback_and_traces_with_options
    let mut raw_map: HashMap<(String, String), TraceInfo> = HashMap::new();
    let mut raw_ambiguous: HashSet<(String, String)> = HashSet::new();
    let mut norm_map: HashMap<(String, String), TraceInfo> = HashMap::new();
    let mut norm_ambiguous: HashSet<(String, String)> = HashSet::new();
    let mut deep_path_map: HashMap<(String, String), TraceInfo> = HashMap::new();
    let mut deep_path_ambiguous: HashSet<(String, String)> = HashSet::new();
    let mut file_traces: HashMap<String, Vec<(String, TraceInfo)>> = HashMap::new();
    let mut suffix_index: HashMap<String, Vec<(String, String, TraceInfo)>> = HashMap::new();
    let mut norm_title_only: HashMap<String, TraceInfo> = HashMap::new();
    let mut norm_title_only_ambiguous: HashSet<String> = HashSet::new();
    let mut raw_title_only: HashMap<String, TraceInfo> = HashMap::new();
    let mut raw_title_only_ambiguous: HashSet<String> = HashSet::new();
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
        let deep_fp = normalize_file_path_deep(&fp);
        let info = TraceInfo {
            tp_weight: tp_w,
            fp_weight: fp_w,
        };

        if fp.is_empty() {
            match raw_title_only.entry(title.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    raw_title_only_ambiguous.insert(title.clone());
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(info.clone());
                }
            }
            if !disable_fuzzy && !norm.is_empty() {
                let norm_for_ambiguous = norm.clone();
                match norm_title_only.entry(norm) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        norm_title_only_ambiguous.insert(norm_for_ambiguous);
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(info);
                    }
                }
            }
        } else {
            raw_titles_with_file_scoped.insert(title.clone());
            if !norm.is_empty() {
                norm_titles_with_file_scoped.insert(norm.clone());
            }
            let raw_key = (title, fp.clone());
            match raw_map.entry(raw_key.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    raw_ambiguous.insert(raw_key);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(info.clone());
                }
            }
            if !disable_fuzzy && !norm.is_empty() {
                let norm_key = (norm.clone(), fp.clone());
                match norm_map.entry(norm_key.clone()) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        norm_ambiguous.insert(norm_key);
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(info.clone());
                    }
                }
                if !deep_fp.is_empty() {
                    let deep_key = (norm.clone(), deep_fp.clone());
                    match deep_path_map.entry(deep_key.clone()) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            deep_path_ambiguous.insert(deep_key);
                        }
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(info.clone());
                        }
                    }
                    let filename = deep_fp.rsplit('/').next().unwrap_or("").to_string();
                    if !filename.is_empty() {
                        suffix_index.entry(filename).or_default().push((
                            deep_fp,
                            norm.clone(),
                            info.clone(),
                        ));
                    }
                }
                file_traces.entry(fp).or_default().push((norm, info));
            }
        }
    }

    for key in &raw_ambiguous {
        raw_map.remove(key);
    }
    for key in &norm_ambiguous {
        norm_map.remove(key);
    }
    for key in &deep_path_ambiguous {
        deep_path_map.remove(key);
    }
    for key in &raw_title_only_ambiguous {
        raw_title_only.remove(key);
    }
    for key in &norm_title_only_ambiguous {
        norm_title_only.remove(key);
    }
    for title in &raw_titles_with_file_scoped {
        raw_title_only.remove(title);
    }
    for norm in &norm_titles_with_file_scoped {
        norm_title_only.remove(norm);
    }

    let mut samples = Vec::new();

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
        let deep_fp = normalize_file_path_deep(&fp);

        let matched = raw_map
            .get(&(title.clone(), fp.clone()))
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() {
                    norm_map.get(&(norm.clone(), fp.clone()))
                } else {
                    None
                }
            })
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() && !deep_fp.is_empty() {
                    deep_path_map.get(&(norm.clone(), deep_fp.clone()))
                } else {
                    None
                }
            })
            .or_else(|| {
                if !disable_fuzzy
                    && !norm.is_empty()
                    && !fp.is_empty()
                    && let Some(candidates) = file_traces.get(&fp)
                {
                    let mut best_score = 0.0_f64;
                    let mut second_best = 0.0_f64;
                    let mut best_info: Option<&TraceInfo> = None;
                    for (cand_norm, info) in candidates {
                        let j = token_jaccard(&norm, cand_norm);
                        if j > best_score {
                            second_best = best_score;
                            best_score = j;
                            best_info = Some(info);
                        } else if j > second_best {
                            second_best = j;
                        }
                    }
                    if best_score >= FUZZY_THRESHOLD
                        && (best_score - second_best) >= FUZZY_AMBIGUITY_MARGIN
                    {
                        return best_info;
                    }
                }
                None
            })
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() && !deep_fp.is_empty() {
                    let filename = deep_fp.rsplit('/').next().unwrap_or("");
                    if let Some(candidates) = suffix_index.get(filename) {
                        let matches: Vec<&TraceInfo> = candidates
                            .iter()
                            .filter(|(cand_path, cand_norm, _)| {
                                *cand_norm == norm && path_suffix_eq(cand_path, &deep_fp)
                            })
                            .map(|(_, _, info)| info)
                            .collect();
                        if matches.len() == 1 {
                            return Some(matches[0]);
                        }
                    }
                }
                None
            })
            .or_else(|| raw_title_only.get(&title))
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() {
                    norm_title_only.get(&norm)
                } else {
                    None
                }
            });

        if let Some(info) = matched {
            let total = info.tp_weight + info.fp_weight;
            if total > 0.0 {
                let precedent_score = info.tp_weight / total;
                if precedent_score.is_finite() {
                    let lang = CalibratorModel::file_ext_language(&fp);
                    let family = CalibratorModel::title_family(&title);
                    let family_fp = model
                        .family_fp_rate
                        .get(&family)
                        .copied()
                        .unwrap_or(model.meta.global_fp_rate);
                    let lang_fp = model
                        .language_fp_rate
                        .get(lang)
                        .copied()
                        .unwrap_or(model.meta.global_fp_rate);
                    samples.push((
                        SampleFeatures {
                            precedent_score,
                            word_lor: model.word_lor_score(&title),
                            family_fp_inv: 1.0 - family_fp,
                            language_fp_inv: 1.0 - lang_fp,
                        },
                        is_positive,
                    ));
                }
            }
        }
    }

    samples
}

const MIN_SAMPLES_FOR_LEARNING: usize = 50;
const WEIGHT_STABILITY_TOLERANCE: f64 = 0.20;

/// Result of weight learning via grid search.
#[derive(Debug, Clone)]
pub struct LearnedWeights {
    pub weights: ScoreWeights,
    pub pr_auc: f64,
    pub baseline_auc: f64,
    pub stable: bool,
    pub fold_aucs: Vec<f64>,
}

/// Learn composite scoring weights from the joined feature corpus via grid
/// search with k-fold cross-validation.
///
/// Returns `None` when fewer than [`MIN_SAMPLES_FOR_LEARNING`] samples are
/// available (falls back to hardcoded weights).
pub fn learn_weights(
    features: &[(SampleFeatures, bool)],
    k_folds: usize,
) -> Option<LearnedWeights> {
    if features.len() < MIN_SAMPLES_FOR_LEARNING {
        return None;
    }

    // Baseline: PR-AUC of trivial classifier (constant score, all samples tied)
    let baseline_scored: Vec<(f64, bool)> = features.iter().map(|(_, l)| (0.0, *l)).collect();
    let baseline_auc = metrics::pr_auc(&baseline_scored);

    let score_grid: &[f64] = &[0.0, 0.25, 0.5, 1.0, 1.5, 2.0];
    let word_lor_grid: &[f64] = &[0.0, 0.5, 1.0, 1.5, 2.0, 3.0];
    let family_grid: &[f64] = &[0.0, 0.25, 0.5, 1.0, 1.5, 2.0];
    let lang_grid: &[f64] = &[0.0, 0.25, 0.5, 1.0, 1.5];

    let full_best = grid_search_best(features, score_grid, word_lor_grid, family_grid, lang_grid);

    let k = k_folds.min(features.len());
    if k < 2 {
        return Some(LearnedWeights {
            weights: full_best.0,
            pr_auc: full_best.1,
            baseline_auc,
            stable: true,
            fold_aucs: vec![full_best.1],
        });
    }

    let indices = deterministic_permutation(features.len());
    let fold_size = features.len() / k;
    let mut fold_weights: Vec<ScoreWeights> = Vec::with_capacity(k);
    let mut fold_aucs: Vec<f64> = Vec::with_capacity(k);

    for i in 0..k {
        let start = i * fold_size;
        let end = if i == k - 1 {
            features.len()
        } else {
            start + fold_size
        };

        let train: Vec<(SampleFeatures, bool)> = indices
            .iter()
            .enumerate()
            .filter(|(idx, _)| *idx < start || *idx >= end)
            .map(|(_, &orig)| features[orig].clone())
            .collect();
        let val: Vec<(SampleFeatures, bool)> = indices[start..end]
            .iter()
            .map(|&orig| features[orig].clone())
            .collect();

        let (best_w, _) =
            grid_search_best(&train, score_grid, word_lor_grid, family_grid, lang_grid);

        let val_scored: Vec<(f64, bool)> =
            val.iter().map(|(f, l)| (f.score(&best_w), *l)).collect();
        let val_auc = metrics::pr_auc(&val_scored);

        fold_weights.push(best_w);
        fold_aucs.push(val_auc);
    }

    let stable = weights_stable(&fold_weights, WEIGHT_STABILITY_TOLERANCE);

    Some(LearnedWeights {
        weights: full_best.0,
        pr_auc: full_best.1,
        baseline_auc,
        stable,
        fold_aucs,
    })
}

fn grid_search_best(
    features: &[(SampleFeatures, bool)],
    score_grid: &[f64],
    word_lor_grid: &[f64],
    family_grid: &[f64],
    lang_grid: &[f64],
) -> (ScoreWeights, f64) {
    let mut best_auc = f64::NEG_INFINITY;
    let mut best_weights = ScoreWeights {
        score: 0.5,
        word_lor: 1.5,
        family_fp_inv: 1.0,
        language_fp_inv: 0.5,
    };

    for &s in score_grid {
        for &w in word_lor_grid {
            for &f in family_grid {
                for &l in lang_grid {
                    if s == 0.0 && w == 0.0 && f == 0.0 && l == 0.0 {
                        continue;
                    }
                    let weights = ScoreWeights {
                        score: s,
                        word_lor: w,
                        family_fp_inv: f,
                        language_fp_inv: l,
                    };
                    let scored: Vec<(f64, bool)> = features
                        .iter()
                        .map(|(feat, label)| (feat.score(&weights), *label))
                        .collect();
                    let auc = metrics::pr_auc(&scored);
                    if auc > best_auc {
                        best_auc = auc;
                        best_weights = weights;
                    }
                }
            }
        }
    }

    (best_weights, best_auc)
}

fn deterministic_permutation(n: usize) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        let hash = (i as u64)
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (hash % (i as u64 + 1)) as usize;
        indices.swap(i, j);
    }
    indices
}

fn weights_stable(folds: &[ScoreWeights], tolerance: f64) -> bool {
    if folds.len() < 2 {
        return true;
    }
    for extract in [
        |w: &ScoreWeights| w.score,
        |w: &ScoreWeights| w.word_lor,
        |w: &ScoreWeights| w.family_fp_inv,
        |w: &ScoreWeights| w.language_fp_inv,
    ] {
        let vals: Vec<f64> = folds.iter().map(extract).collect();
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        if mean.abs() < 1e-9 {
            if vals.iter().any(|v| v.abs() > tolerance) {
                return false;
            }
        } else {
            for v in &vals {
                if ((v - mean) / mean).abs() > tolerance {
                    return false;
                }
            }
        }
    }
    true
}

#[derive(Clone)]
struct TraceInfo {
    tp_weight: f64,
    fp_weight: f64,
}

pub fn tokenize_title(title: &str) -> Vec<String> {
    let lower = title.to_lowercase();
    crate::calibrator_model::WORD_RE
        .find_iter(&lower)
        .map(|m| m.as_str().to_string())
        .filter(|w| w.len() >= 2)
        .collect()
}

// ---------------------------------------------------------------------------
// Fold-local feature extraction (logistic calibrator infrastructure)
// ---------------------------------------------------------------------------

/// Intermediate representation of a joined sample before feature extraction.
/// Contains all metadata needed to compute expanded features.
#[derive(Debug, Clone)]
pub struct JoinedSample {
    pub title: String,
    pub category: String,
    pub severity: String,
    pub model: String,
    pub tp_weight: f64,
    pub fp_weight: f64,
    pub soft_fp_weight: f64,
    pub full_suppress_weight: f64,
    pub wontfix_weight: f64,
    pub precedent_count: usize,
    pub max_similarity: f64,
    pub mean_similarity: f64,
    pub is_fp: bool,
    pub family: String,
    pub file_path: String,
    pub source_is_ast: bool,
    pub finding_span_lines: u32,
}

/// Per-fold computed statistics for target-encoded features.
/// Prevents leakage by computing rates only from the training partition.
#[derive(Debug, Clone)]
pub struct FoldLocalStats {
    pub category_fp_rates: HashMap<String, f64>,
    pub severity_fp_rates: HashMap<String, f64>,
    pub model_fp_rates: HashMap<String, f64>,
    pub word_lor: HashMap<String, f64>,
    pub global_fp_rate: f64,
    pub file_fp_rates: HashMap<String, f64>,
    pub file_finding_counts: HashMap<String, usize>,
}

/// Smoothing parameter for beta-smoothed empirical rates.
const BETA_ALPHA: f64 = 5.0;

/// Beta-smoothed empirical rate: (count + alpha * prior) / (total + alpha)
pub fn beta_smoothed_rate(fp_count: usize, total: usize, global_rate: f64) -> f64 {
    (fp_count as f64 + BETA_ALPHA * global_rate) / (total as f64 + BETA_ALPHA)
}

/// Compute `FoldLocalStats` from a slice of `JoinedSample` references (training partition).
///
/// Builds per-category, per-severity, and per-model FP rates using beta smoothing,
/// plus word log-odds ratios with Laplace smoothing and minimum support filtering.
pub fn compute_fold_local_stats(samples: &[&JoinedSample]) -> FoldLocalStats {
    let total = samples.len();
    let fp_total = samples.iter().filter(|s| s.is_fp).count();
    let tp_total = total.saturating_sub(fp_total);
    let global_fp_rate = if total > 0 {
        fp_total as f64 / total as f64
    } else {
        0.0
    };

    // Per-category FP rates
    let mut cat_fp: HashMap<String, usize> = HashMap::new();
    let mut cat_total: HashMap<String, usize> = HashMap::new();
    // Per-severity FP rates
    let mut sev_fp: HashMap<String, usize> = HashMap::new();
    let mut sev_total: HashMap<String, usize> = HashMap::new();
    // Per-model FP rates
    let mut model_fp: HashMap<String, usize> = HashMap::new();
    let mut model_total: HashMap<String, usize> = HashMap::new();
    // Word counts for LOR
    let mut word_fp_count: HashMap<String, usize> = HashMap::new();
    let mut word_tp_count: HashMap<String, usize> = HashMap::new();
    // Per-file FP rates and finding counts
    let mut file_fp: HashMap<String, usize> = HashMap::new();
    let mut file_total: HashMap<String, usize> = HashMap::new();

    for s in samples {
        // Category
        *cat_total.entry(s.category.clone()).or_default() += 1;
        if s.is_fp {
            *cat_fp.entry(s.category.clone()).or_default() += 1;
        }
        // Severity
        *sev_total.entry(s.severity.clone()).or_default() += 1;
        if s.is_fp {
            *sev_fp.entry(s.severity.clone()).or_default() += 1;
        }
        // Model
        *model_total.entry(s.model.clone()).or_default() += 1;
        if s.is_fp {
            *model_fp.entry(s.model.clone()).or_default() += 1;
        }
        // File
        if !s.file_path.is_empty() {
            *file_total.entry(s.file_path.clone()).or_default() += 1;
            if s.is_fp {
                *file_fp.entry(s.file_path.clone()).or_default() += 1;
            }
        }
        // Words
        let words = tokenize_title(&s.title);
        for w in &words {
            if s.is_fp {
                *word_fp_count.entry(w.clone()).or_default() += 1;
            } else {
                *word_tp_count.entry(w.clone()).or_default() += 1;
            }
        }
    }

    // Beta-smoothed category FP rates
    let category_fp_rates: HashMap<String, f64> = cat_total
        .iter()
        .map(|(cat, &tot)| {
            let fp_c = cat_fp.get(cat).copied().unwrap_or(0);
            (cat.clone(), beta_smoothed_rate(fp_c, tot, global_fp_rate))
        })
        .collect();

    // Beta-smoothed severity FP rates
    let severity_fp_rates: HashMap<String, f64> = sev_total
        .iter()
        .map(|(sev, &tot)| {
            let fp_c = sev_fp.get(sev).copied().unwrap_or(0);
            (sev.clone(), beta_smoothed_rate(fp_c, tot, global_fp_rate))
        })
        .collect();

    // Beta-smoothed model FP rates
    let model_fp_rates: HashMap<String, f64> = model_total
        .iter()
        .map(|(m, &tot)| {
            let fp_c = model_fp.get(m).copied().unwrap_or(0);
            (m.clone(), beta_smoothed_rate(fp_c, tot, global_fp_rate))
        })
        .collect();

    // Word log-odds ratios with Laplace smoothing
    let mut word_lor_map: HashMap<String, f64> = HashMap::new();
    if tp_total > 0 && fp_total > 0 {
        let all_words: HashSet<&String> =
            word_fp_count.keys().chain(word_tp_count.keys()).collect();
        let eps = 0.5_f64; // Laplace smoothing
        for w in all_words {
            let fp_c = word_fp_count.get(w).copied().unwrap_or(0);
            let tp_c = word_tp_count.get(w).copied().unwrap_or(0);
            let support = fp_c + tp_c;
            if support < WORD_MIN_SUPPORT {
                continue;
            }
            let fp_rate = (fp_c as f64 + eps) / (fp_total as f64 + eps);
            let tp_rate = (tp_c as f64 + eps) / (tp_total as f64 + eps);
            let lor = (fp_rate / tp_rate).ln();
            if lor.is_finite() {
                word_lor_map.insert(w.clone(), lor);
            }
        }
    }

    // Beta-smoothed file FP rates
    let file_fp_rates: HashMap<String, f64> = file_total
        .iter()
        .map(|(f, &tot)| {
            let fp_c = file_fp.get(f).copied().unwrap_or(0);
            (f.clone(), beta_smoothed_rate(fp_c, tot, global_fp_rate))
        })
        .collect();

    let file_finding_counts: HashMap<String, usize> = file_total;

    FoldLocalStats {
        category_fp_rates,
        severity_fp_rates,
        model_fp_rates,
        word_lor: word_lor_map,
        global_fp_rate,
        file_fp_rates,
        file_finding_counts,
    }
}

/// Populate the rate-map fields on a [`CalibratorModel`] from full-corpus
/// [`FoldLocalStats`].  File paths are deep-normalized so the keys match
/// review-time lookups regardless of leading `./` or `../` prefixes.
pub fn store_rate_maps_in_model(
    model: &mut crate::calibrator_model::CalibratorModel,
    stats: &FoldLocalStats,
) {
    model.category_fp_rate_map = Some(stats.category_fp_rates.clone());
    model.severity_fp_rate = Some(stats.severity_fp_rates.clone());
    model.model_fp_rate = Some(stats.model_fp_rates.clone());

    let file_fp: std::collections::HashMap<String, f64> = stats
        .file_fp_rates
        .iter()
        .map(|(k, &v)| (crate::file_util::normalize_file_path_deep(k), v))
        .collect();
    model.file_fp_rate = Some(file_fp);

    let file_counts: std::collections::HashMap<String, usize> = stats
        .file_finding_counts
        .iter()
        .map(|(k, &v)| (crate::file_util::normalize_file_path_deep(k), v))
        .collect();
    model.file_finding_counts = Some(file_counts);
}

/// Extract expanded features for a single sample using fold-local stats.
///
/// Returns the feature vector and the label (`true` = FP for logistic regression).
pub fn extract_single_expanded(
    s: &JoinedSample,
    stats: &FoldLocalStats,
) -> (ExpandedFeatures, bool) {
    let words = tokenize_title(&s.title);

    let max_word_lor = words
        .iter()
        .filter_map(|w| stats.word_lor.get(w))
        .copied()
        .reduce(f64::max)
        .unwrap_or(0.0);

    let min_word_lor = words
        .iter()
        .filter_map(|w| stats.word_lor.get(w))
        .copied()
        .reduce(f64::min)
        .unwrap_or(0.0);

    let count_negative_lor_tokens = words
        .iter()
        .filter_map(|w| stats.word_lor.get(w))
        .filter(|&&lor| lor < -0.5)
        .count() as f64;

    let features = ExpandedFeatures {
        log1p_tp_weight: s.tp_weight.ln_1p(),
        log1p_fp_weight: s.fp_weight.ln_1p(),
        precedent_count: (s.precedent_count as f64).min(10.0),
        max_similarity: s.max_similarity,
        mean_similarity: s.mean_similarity,
        has_no_precedents: if s.precedent_count == 0 { 1.0 } else { 0.0 },
        log1p_soft_fp_weight: s.soft_fp_weight.ln_1p(),
        log1p_full_suppress_weight: s.full_suppress_weight.ln_1p(),
        log1p_wontfix_weight: s.wontfix_weight.ln_1p(),
        category_fp_rate: stats
            .category_fp_rates
            .get(&s.category)
            .copied()
            .unwrap_or(stats.global_fp_rate),
        severity_fp_rate: stats
            .severity_fp_rates
            .get(&s.severity)
            .copied()
            .unwrap_or(stats.global_fp_rate),
        model_fp_rate: stats
            .model_fp_rates
            .get(&s.model)
            .copied()
            .unwrap_or(stats.global_fp_rate),
        max_word_lor,
        min_word_lor,
        count_negative_lor_tokens,
        is_test_file: if is_test_file_path(&s.file_path) {
            1.0
        } else {
            0.0
        },
        source_is_ast: if s.source_is_ast { 1.0 } else { 0.0 },
        finding_count_same_file: if s.file_path.is_empty() {
            0.0
        } else {
            (stats
                .file_finding_counts
                .get(&s.file_path)
                .copied()
                .unwrap_or(1) as f64)
                .ln_1p()
        },
        file_fp_rate: if s.file_path.is_empty() {
            stats.global_fp_rate
        } else {
            stats
                .file_fp_rates
                .get(&s.file_path)
                .copied()
                .unwrap_or(stats.global_fp_rate)
        },
        finding_span_lines: (s.finding_span_lines as f64).ln_1p(),
        is_mock_or_fixture: if is_mock_or_fixture_path(&s.file_path) {
            1.0
        } else {
            0.0
        },
        is_generated_or_vendor: if is_generated_or_vendor_path(&s.file_path) {
            1.0
        } else {
            0.0
        },
    };

    (features, s.is_fp)
}

/// Returns `true` if the file path looks like a test file.
pub fn is_test_file_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let lower = path.to_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    for part in &parts {
        if *part == "tests" || *part == "test" || *part == "spec" || *part == "__tests__" {
            return true;
        }
    }
    let filename = parts.last().unwrap_or(&"");
    filename.starts_with("test_")
        || filename.ends_with("_test.rs")
        || filename.ends_with("_test.py")
        || filename.ends_with("_test.ts")
        || filename.ends_with("_test.js")
        || filename.ends_with(".test.ts")
        || filename.ends_with(".test.js")
        || filename.ends_with(".test.tsx")
        || filename.ends_with(".test.jsx")
        || filename.ends_with(".spec.ts")
        || filename.ends_with(".spec.js")
        || filename.ends_with("_spec.rb")
}

/// Returns `true` if the file path looks like a mock, stub, or fixture file.
pub fn is_mock_or_fixture_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let lower = path.to_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    for part in &parts {
        if *part == "mocks"
            || *part == "mock"
            || *part == "stubs"
            || *part == "fixtures"
            || *part == "fixture"
            || *part == "__mocks__"
            || *part == "__fixtures__"
        {
            return true;
        }
    }
    let filename = parts.last().unwrap_or(&"");
    filename.contains("mock")
        || filename.contains("stub")
        || filename.contains("fixture")
        || filename.contains("dummy")
        || filename.contains("fake")
}

/// Returns `true` if the file path looks like generated, vendored, or build output.
pub fn is_generated_or_vendor_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let lower = path.to_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    for part in &parts {
        if *part == "vendor"
            || *part == "vendored"
            || *part == "third_party"
            || *part == "third-party"
            || *part == "node_modules"
            || *part == "dist"
            || *part == "build"
            || *part == "generated"
            || *part == "gen"
            || *part == "autogen"
            || *part == "proto"
            || *part == "target"
        {
            return true;
        }
    }
    let filename = parts.last().unwrap_or(&"");
    filename.ends_with(".generated.rs")
        || filename.ends_with(".generated.ts")
        || filename.ends_with(".generated.go")
        || filename.ends_with(".pb.go")
        || filename.ends_with(".pb.rs")
        || filename.ends_with(".min.js")
        || filename.ends_with(".min.css")
        || filename.starts_with("generated_")
        || filename.starts_with("autogen_")
        || *filename == "package-lock.json"
        || *filename == "yarn.lock"
        || *filename == "pnpm-lock.yaml"
        || *filename == "cargo.lock"
}

// ---------------------------------------------------------------------------
// Expanded trace info for extract_joined_samples
// ---------------------------------------------------------------------------

/// Richer variant of `TraceInfo` carrying all metadata fields needed to
/// populate a `JoinedSample`.
#[derive(Clone)]
struct TraceInfoExpanded {
    tp_weight: f64,
    fp_weight: f64,
    soft_fp_weight: f64,
    full_suppress_weight: f64,
    wontfix_weight: f64,
    precedent_count: usize,
    max_similarity: f64,
    mean_similarity: f64,
    category: String,
    severity: String,
    model: String,
    finding_span_lines: u32,
}

/// Extract `JoinedSample` records by walking the same multi-tier join as
/// `extract_join_features`, but returning richer metadata instead of composite
/// features. Used by the logistic calibrator's cross-validation pipeline.
pub fn extract_joined_samples(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
    _model: &CalibratorModel,
    filter: &JoinFilter,
    disable_fuzzy: bool,
) -> Vec<JoinedSample> {
    let filtered_traces: Vec<&serde_json::Value> =
        traces.iter().filter(|t| filter.accepts(t)).collect();

    // Build the same index structures as extract_join_features, using TraceInfoExpanded.
    let mut raw_map: HashMap<(String, String), TraceInfoExpanded> = HashMap::new();
    let mut raw_ambiguous: HashSet<(String, String)> = HashSet::new();
    let mut norm_map: HashMap<(String, String), TraceInfoExpanded> = HashMap::new();
    let mut norm_ambiguous: HashSet<(String, String)> = HashSet::new();
    let mut deep_path_map: HashMap<(String, String), TraceInfoExpanded> = HashMap::new();
    let mut deep_path_ambiguous: HashSet<(String, String)> = HashSet::new();
    let mut file_traces: HashMap<String, Vec<(String, TraceInfoExpanded)>> = HashMap::new();
    let mut suffix_index: HashMap<String, Vec<(String, String, TraceInfoExpanded)>> =
        HashMap::new();
    let mut norm_title_only: HashMap<String, TraceInfoExpanded> = HashMap::new();
    let mut norm_title_only_ambiguous: HashSet<String> = HashSet::new();
    let mut raw_title_only: HashMap<String, TraceInfoExpanded> = HashMap::new();
    let mut raw_title_only_ambiguous: HashSet<String> = HashSet::new();
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
        let soft_fp_w = t["soft_fp_weight"].as_f64().unwrap_or(0.0).max(0.0);
        let full_suppress_w = t["full_suppress_weight"].as_f64().unwrap_or(fp_w).max(0.0);
        let wontfix_w = t["wontfix_weight"].as_f64().unwrap_or(0.0).max(0.0);
        let precedent_count = t["precedent_count"]
            .as_u64()
            .map(|v| v as usize)
            .unwrap_or_else(|| if tp_w > 0.0 || fp_w > 0.0 { 1 } else { 0 });
        let max_sim = t["max_similarity"].as_f64().unwrap_or(0.0);
        let mean_sim = t["mean_similarity"].as_f64().unwrap_or(0.0);
        let category = t["finding_category"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let severity = t["input_severity"].as_str().unwrap_or("medium").to_string();
        let model_name = t["provenance"]["review_model"]
            .as_str()
            .or_else(|| t["model"].as_str())
            .unwrap_or("unknown")
            .to_string();
        let span_lines = t["finding_span_lines"].as_u64().unwrap_or(1) as u32;

        let norm = normalize_title(&title);
        let deep_fp = normalize_file_path_deep(&fp);
        let info = TraceInfoExpanded {
            tp_weight: tp_w,
            fp_weight: fp_w,
            soft_fp_weight: soft_fp_w,
            full_suppress_weight: full_suppress_w,
            wontfix_weight: wontfix_w,
            precedent_count,
            max_similarity: max_sim,
            mean_similarity: mean_sim,
            category,
            severity,
            model: model_name,
            finding_span_lines: span_lines,
        };

        if fp.is_empty() {
            match raw_title_only.entry(title.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    raw_title_only_ambiguous.insert(title.clone());
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(info.clone());
                }
            }
            if !disable_fuzzy && !norm.is_empty() {
                let norm_for_ambiguous = norm.clone();
                match norm_title_only.entry(norm) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        norm_title_only_ambiguous.insert(norm_for_ambiguous);
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(info);
                    }
                }
            }
        } else {
            raw_titles_with_file_scoped.insert(title.clone());
            if !norm.is_empty() {
                norm_titles_with_file_scoped.insert(norm.clone());
            }
            let raw_key = (title, fp.clone());
            match raw_map.entry(raw_key.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    raw_ambiguous.insert(raw_key);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(info.clone());
                }
            }
            if !disable_fuzzy && !norm.is_empty() {
                let norm_key = (norm.clone(), fp.clone());
                match norm_map.entry(norm_key.clone()) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        norm_ambiguous.insert(norm_key);
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(info.clone());
                    }
                }
                if !deep_fp.is_empty() {
                    let deep_key = (norm.clone(), deep_fp.clone());
                    match deep_path_map.entry(deep_key.clone()) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            deep_path_ambiguous.insert(deep_key);
                        }
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(info.clone());
                        }
                    }
                    let filename = deep_fp.rsplit('/').next().unwrap_or("").to_string();
                    if !filename.is_empty() {
                        suffix_index.entry(filename).or_default().push((
                            deep_fp,
                            norm.clone(),
                            info.clone(),
                        ));
                    }
                }
                file_traces.entry(fp).or_default().push((norm, info));
            }
        }
    }

    for key in &raw_ambiguous {
        raw_map.remove(key);
    }
    for key in &norm_ambiguous {
        norm_map.remove(key);
    }
    for key in &deep_path_ambiguous {
        deep_path_map.remove(key);
    }
    for key in &raw_title_only_ambiguous {
        raw_title_only.remove(key);
    }
    for key in &norm_title_only_ambiguous {
        norm_title_only.remove(key);
    }
    for title in &raw_titles_with_file_scoped {
        raw_title_only.remove(title);
    }
    for norm in &norm_titles_with_file_scoped {
        norm_title_only.remove(norm);
    }

    let mut samples = Vec::new();

    for f in feedback {
        let verdict = f["verdict"].as_str().unwrap_or("");
        let is_fp = match verdict {
            "tp" | "partial" => false,
            "fp" => true,
            _ => continue,
        };
        let title = f["finding_title"].as_str().unwrap_or("").to_string();
        if title.is_empty() {
            continue;
        }
        let fp = f["file_path"].as_str().unwrap_or("").to_string();
        let norm = normalize_title(&title);
        let deep_fp = normalize_file_path_deep(&fp);

        let matched = raw_map
            .get(&(title.clone(), fp.clone()))
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() {
                    norm_map.get(&(norm.clone(), fp.clone()))
                } else {
                    None
                }
            })
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() && !deep_fp.is_empty() {
                    deep_path_map.get(&(norm.clone(), deep_fp.clone()))
                } else {
                    None
                }
            })
            .or_else(|| {
                if !disable_fuzzy
                    && !norm.is_empty()
                    && !fp.is_empty()
                    && let Some(candidates) = file_traces.get(&fp)
                {
                    let mut best_score = 0.0_f64;
                    let mut second_best = 0.0_f64;
                    let mut best_info: Option<&TraceInfoExpanded> = None;
                    for (cand_norm, info) in candidates {
                        let j = token_jaccard(&norm, cand_norm);
                        if j > best_score {
                            second_best = best_score;
                            best_score = j;
                            best_info = Some(info);
                        } else if j > second_best {
                            second_best = j;
                        }
                    }
                    if best_score >= FUZZY_THRESHOLD
                        && (best_score - second_best) >= FUZZY_AMBIGUITY_MARGIN
                    {
                        return best_info;
                    }
                }
                None
            })
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() && !deep_fp.is_empty() {
                    let filename = deep_fp.rsplit('/').next().unwrap_or("");
                    if let Some(candidates) = suffix_index.get(filename) {
                        let matches: Vec<&TraceInfoExpanded> = candidates
                            .iter()
                            .filter(|(cand_path, cand_norm, _)| {
                                *cand_norm == norm && path_suffix_eq(cand_path, &deep_fp)
                            })
                            .map(|(_, _, info)| info)
                            .collect();
                        if matches.len() == 1 {
                            return Some(matches[0]);
                        }
                    }
                }
                None
            })
            .or_else(|| raw_title_only.get(&title))
            .or_else(|| {
                if !disable_fuzzy && !norm.is_empty() {
                    norm_title_only.get(&norm)
                } else {
                    None
                }
            });

        if let Some(info) = matched {
            let family = CalibratorModel::title_family(&title);
            let source_is_ast = info.model == "unknown" || info.category == "ast-pattern";
            samples.push(JoinedSample {
                title,
                category: info.category.clone(),
                severity: info.severity.clone(),
                model: info.model.clone(),
                tp_weight: info.tp_weight,
                fp_weight: info.fp_weight,
                soft_fp_weight: info.soft_fp_weight,
                full_suppress_weight: info.full_suppress_weight,
                wontfix_weight: info.wontfix_weight,
                precedent_count: info.precedent_count,
                max_similarity: info.max_similarity,
                mean_similarity: info.mean_similarity,
                is_fp,
                family,
                file_path: fp.clone(),
                source_is_ast,
                finding_span_lines: info.finding_span_lines,
            });
        }
    }

    samples
}

// ---------------------------------------------------------------------------
// Logistic calibrator: cross-validated training pipeline
// ---------------------------------------------------------------------------

/// Minimum total samples required for logistic model training.
const MIN_SAMPLES_FOR_LOGISTIC: usize = 200;

/// Minimum count of each class (FP and TP) required.
const MIN_CLASS_COUNT: usize = 30;

/// L2 regularization strengths to search over.
const LAMBDA_GRID: &[f64] = &[0.01, 0.1, 1.0, 10.0];

/// Maximum iterations for logistic regression fitting.
const MAX_LOGISTIC_ITER: usize = 500;

/// Result of logistic calibrator training.
#[derive(Debug, Clone)]
pub struct LogisticResult {
    pub selected_features: Vec<usize>,
    pub selected_feature_names: Vec<String>,
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    pub feature_means: Vec<f64>,
    pub feature_stddevs: Vec<f64>,
    pub suppress_threshold: f64,
    pub boost_threshold: f64,
    pub ap_score: f64,
    pub fp_recall_at_99_tp_recall: f64,
    pub baseline_ap: f64,
    pub n_samples: usize,
    pub n_fp: usize,
}

/// Assigns each sample to a fold based on its family (group).
/// Families are assigned to folds round-robin in order of first appearance.
fn group_k_fold(families: &[&str], k: usize) -> Vec<usize> {
    let mut family_to_fold: HashMap<&str, usize> = HashMap::new();
    let mut next_fold = 0usize;
    families
        .iter()
        .map(|f| {
            *family_to_fold.entry(f).or_insert_with(|| {
                let fold = next_fold % k;
                next_fold += 1;
                fold
            })
        })
        .collect()
}

/// Train a logistic model via cross-validation with per-fold feature screening.
///
/// Returns `None` when:
/// - Insufficient samples (< 200)
/// - Class imbalance (min class < 30)
/// - Fewer than 2 features pass consensus selection
/// - Model fails to beat baseline AP by >= 0.02
pub fn learn_logistic(samples: &[JoinedSample], k_folds: usize) -> Option<LogisticResult> {
    let n = samples.len();

    // Safety gate: minimum sample count
    if n < MIN_SAMPLES_FOR_LOGISTIC {
        return None;
    }

    let n_fp = samples.iter().filter(|s| s.is_fp).count();
    let n_tp = n - n_fp;

    // Safety gate: minimum class count
    if n_fp < MIN_CLASS_COUNT || n_tp < MIN_CLASS_COUNT {
        return None;
    }

    // Baseline AP: FP prevalence (trivial classifier)
    let baseline_ap = n_fp as f64 / n as f64;

    // GroupKFold assignment by family
    let families: Vec<&str> = samples.iter().map(|s| s.family.as_str()).collect();
    let fold_assignments = group_k_fold(&families, k_folds);

    // Per-fold tracking
    let mut all_oof_predictions: Vec<(f64, bool)> = vec![(0.0, false); n];
    let mut oof_filled = vec![false; n];
    let mut fold_selected_features: Vec<Vec<usize>> = Vec::with_capacity(k_folds);
    let mut fold_best_lambdas: Vec<f64> = Vec::with_capacity(k_folds);

    for fold_idx in 0..k_folds {
        // Split into train/val by fold assignment
        let train_indices: Vec<usize> = (0..n)
            .filter(|&i| fold_assignments[i] != fold_idx)
            .collect();
        let val_indices: Vec<usize> = (0..n)
            .filter(|&i| fold_assignments[i] == fold_idx)
            .collect();

        if val_indices.is_empty() || train_indices.is_empty() {
            continue;
        }

        // Compute FoldLocalStats from training samples
        let train_refs: Vec<&JoinedSample> = train_indices.iter().map(|&i| &samples[i]).collect();
        let stats = compute_fold_local_stats(&train_refs);

        // Extract expanded features for train and val
        let train_expanded: Vec<(ExpandedFeatures, bool)> = train_indices
            .iter()
            .map(|&i| extract_single_expanded(&samples[i], &stats))
            .collect();
        let val_expanded: Vec<(ExpandedFeatures, bool)> = val_indices
            .iter()
            .map(|&i| extract_single_expanded(&samples[i], &stats))
            .collect();

        // Univariate screen on train features
        let selected = univariate_screen(&train_expanded, baseline_ap);

        // If < 2 features selected, this fold cannot proceed
        if selected.len() < 2 {
            return None;
        }

        fold_selected_features.push(selected.clone());

        // Build feature matrices (only selected columns)
        let train_x: Vec<Vec<f64>> = train_expanded
            .iter()
            .map(|(f, _)| {
                let full = f.to_vec();
                selected.iter().map(|&idx| full[idx]).collect()
            })
            .collect();
        let train_y: Vec<bool> = train_expanded.iter().map(|(_, label)| *label).collect();

        let val_x: Vec<Vec<f64>> = val_expanded
            .iter()
            .map(|(f, _)| {
                let full = f.to_vec();
                selected.iter().map(|&idx| full[idx]).collect()
            })
            .collect();

        // Lambda grid search
        let mut best_lambda = LAMBDA_GRID[0];
        let mut best_ap = f64::NEG_INFINITY;

        for &lambda in LAMBDA_GRID {
            let model = crate::logistic::fit(&train_x, &train_y, lambda, MAX_LOGISTIC_ITER);
            let val_preds = model.predict(&val_x);
            let val_scored: Vec<(f64, bool)> = val_preds
                .into_iter()
                .zip(val_expanded.iter().map(|(_, label)| *label))
                .collect();
            let ap = crate::metrics::average_precision_stepwise(&val_scored);
            if ap > best_ap {
                best_ap = ap;
                best_lambda = lambda;
            }
        }

        fold_best_lambdas.push(best_lambda);

        // Refit with best lambda, produce OOF predictions for val indices
        let best_model = crate::logistic::fit(&train_x, &train_y, best_lambda, MAX_LOGISTIC_ITER);
        let oof_preds = best_model.predict(&val_x);

        for (local_idx, &global_idx) in val_indices.iter().enumerate() {
            all_oof_predictions[global_idx] = (oof_preds[local_idx], val_expanded[local_idx].1);
            oof_filled[global_idx] = true;
        }
    }

    // Consensus feature selection: feature must be selected in >= ceil(k_folds/2 + 1) folds
    let consensus_threshold = k_folds / 2 + 1;
    let n_features = ExpandedFeatures::feature_names().len();
    let mut feature_vote_count = vec![0usize; n_features];
    for fold_features in &fold_selected_features {
        for &feat_idx in fold_features {
            feature_vote_count[feat_idx] += 1;
        }
    }
    let consensus_features: Vec<usize> = (0..n_features)
        .filter(|&i| feature_vote_count[i] >= consensus_threshold)
        .collect();

    if consensus_features.len() < 2 {
        return None;
    }

    // Aggregated OOF metrics (only use filled slots)
    let filled_predictions: Vec<(f64, bool)> = all_oof_predictions
        .iter()
        .zip(oof_filled.iter())
        .filter(|(_, filled)| **filled)
        .map(|(pred, _)| *pred)
        .collect();

    let ap_score = crate::metrics::average_precision_stepwise(&filled_predictions);
    let fp_recall = crate::metrics::fp_recall_at_tp_recall(&filled_predictions, 0.99);

    // If AP does not beat baseline by at least 0.02, return None
    if ap_score <= baseline_ap + 0.02 {
        return None;
    }

    // Production model: retrain on 100% of data with consensus features
    let all_refs: Vec<&JoinedSample> = samples.iter().collect();
    let all_stats = compute_fold_local_stats(&all_refs);
    let all_expanded: Vec<(ExpandedFeatures, bool)> = samples
        .iter()
        .map(|s| extract_single_expanded(s, &all_stats))
        .collect();

    let prod_x: Vec<Vec<f64>> = all_expanded
        .iter()
        .map(|(f, _)| {
            let full = f.to_vec();
            consensus_features.iter().map(|&idx| full[idx]).collect()
        })
        .collect();
    let prod_y: Vec<bool> = all_expanded.iter().map(|(_, label)| *label).collect();

    // Use median of per-fold CV-selected lambdas for production refit
    let prod_lambda = {
        let mut sorted_lambdas = fold_best_lambdas.clone();
        sorted_lambdas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted_lambdas[sorted_lambdas.len() / 2]
    };
    let prod_model = crate::logistic::fit(&prod_x, &prod_y, prod_lambda, MAX_LOGISTIC_ITER);

    // Threshold selection from OOF predictions (not in-sample) to avoid
    // optimistic safety estimates. The production model provides deployed
    // coefficients; thresholds come from held-out data.
    let mut oof_tp_predictions: Vec<f64> = filled_predictions
        .iter()
        .filter(|(_, is_fp)| !*is_fp)
        .map(|(pred, _)| *pred)
        .collect();
    let mut oof_fp_predictions: Vec<f64> = filled_predictions
        .iter()
        .filter(|(_, is_fp)| *is_fp)
        .map(|(pred, _)| *pred)
        .collect();

    if oof_tp_predictions.is_empty() || oof_fp_predictions.is_empty() {
        return None;
    }

    // Suppress threshold: sort TP OOF predictions descending, pick at ceil(n_tp * 0.01)
    // This ensures 99% of TPs have OOF prediction below the threshold (safe from suppression)
    oof_tp_predictions.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let suppress_idx = ((oof_tp_predictions.len() as f64 * 0.01).ceil() as usize)
        .saturating_sub(1)
        .min(oof_tp_predictions.len() - 1);
    let suppress_threshold = oof_tp_predictions[suppress_idx];

    // Boost threshold: sort FP OOF predictions ascending, pick at ceil(n_fp * 0.05)
    // This ensures 95% of FPs have OOF prediction above the threshold
    oof_fp_predictions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let boost_idx = ((oof_fp_predictions.len() as f64 * 0.05).ceil() as usize)
        .saturating_sub(1)
        .min(oof_fp_predictions.len() - 1);
    let boost_threshold = oof_fp_predictions[boost_idx];

    let feature_names = ExpandedFeatures::feature_names();
    let selected_feature_names: Vec<String> = consensus_features
        .iter()
        .map(|&i| feature_names[i].to_string())
        .collect();

    Some(LogisticResult {
        selected_features: consensus_features,
        selected_feature_names,
        coefficients: prod_model.coefficients,
        intercept: prod_model.intercept,
        feature_means: prod_model.feature_means,
        feature_stddevs: prod_model.feature_stddevs,
        suppress_threshold,
        boost_threshold,
        ap_score,
        fp_recall_at_99_tp_recall: fp_recall,
        baseline_ap,
        n_samples: n,
        n_fp,
    })
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
        assert!(
            samples
                .iter()
                .any(|(s, l)| !*l && (*s - 0.053).abs() < 0.01)
        );
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
        assert!(
            samples.is_empty(),
            "ambiguous title-only keys should be skipped"
        );
    }

    #[test]
    fn primary_key_preferred_over_title_only() {
        // When both a title+file_path match and a title-only match exist,
        // the primary (more specific) match wins.
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![
            make_trace("Bug", 2.5, 0.3, Some("src/a.rs")), // primary match
            make_trace("Bug", 0.1, 1.8, None),             // title-only fallback
        ];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        let (score, _) = samples[0];
        assert!(
            (score - 0.893).abs() < 0.01,
            "should use primary key match, got {score}"
        );
    }

    #[test]
    fn title_only_blocked_when_file_scoped_exists() {
        // If a title has file-scoped traces, the title-only fallback must not
        // be used for unmatched feedback (prevents cross-file contamination).
        let feedback = vec![make_feedback("Bug", "tp", "src/b.rs")]; // no file-scoped match
        let traces = vec![
            make_trace("Bug", 2.5, 0.3, Some("src/a.rs")), // file-scoped for different file
            make_trace("Bug", 0.1, 1.8, None),             // title-only
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
        let feedback = vec![make_feedback(
            "uses a fixed .tmp filename",
            "tp",
            "src/a.rs",
        )];
        let traces = vec![make_trace(
            "uses a fixed `.tmp` filename",
            2.0,
            0.3,
            Some("src/a.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            samples.len(),
            1,
            "normalized exact should match backtick variants"
        );
        assert_eq!(stats.exact_normalized, 1);
    }

    #[test]
    fn normalized_exact_matches_rule_prefix() {
        let feedback = vec![make_feedback("Empty .expect() message", "fp", "src/b.rs")];
        let traces = vec![make_trace(
            "expect-empty-message: Empty `.expect()` message",
            0.2,
            1.5,
            Some("src/b.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            samples.len(),
            1,
            "normalized exact should match rule-prefix variants"
        );
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
        let feedback = vec![make_feedback(
            "uses a fixed .tmp filename",
            "tp",
            "src/a.rs",
        )];
        let traces = vec![make_trace(
            "uses a fixed `.tmp` filename",
            2.0,
            0.3,
            Some("src/b.rs"),
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
            "tp",
            "src/visit.rs",
        )];
        let traces = vec![make_trace(
            "Reset can race with visit processing and lose the cleaned state",
            2.0,
            0.5,
            Some("src/visit.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            samples.len(),
            1,
            "fuzzy same-file should match extended title"
        );
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    #[test]
    fn fuzzy_same_file_rejects_below_threshold() {
        let feedback = vec![make_feedback("API key leak", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "Database connection pool exhaustion under load",
            2.0,
            0.5,
            Some("src/a.rs"),
        )];
        let (samples, _stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "below-threshold fuzzy should not match");
    }

    #[test]
    fn fuzzy_same_file_rejects_ambiguous() {
        let feedback = vec![make_feedback("error handling is missing", "tp", "src/a.rs")];
        let traces = vec![
            make_trace(
                "error handling is missing for IO",
                2.0,
                0.5,
                Some("src/a.rs"),
            ),
            make_trace(
                "error handling is missing for parse",
                1.0,
                0.8,
                Some("src/a.rs"),
            ),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert!(
            samples.is_empty(),
            "ambiguous fuzzy matches should be skipped"
        );
        assert!(stats.ambiguous_skipped >= 1);
    }

    #[test]
    fn fuzzy_same_file_accepts_clear_winner() {
        let feedback = vec![make_feedback("error handling is missing", "tp", "src/a.rs")];
        let traces = vec![
            make_trace(
                "error handling is missing for IO operations",
                2.0,
                0.5,
                Some("src/a.rs"),
            ),
            make_trace(
                "something completely different xyz abc",
                1.0,
                0.8,
                Some("src/a.rs"),
            ),
        ];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "clear winner should match");
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    #[test]
    fn fuzzy_same_file_different_file_no_match() {
        let feedback = vec![make_feedback(
            "Reset can race with visit processing",
            "tp",
            "src/a.rs",
        )];
        let traces = vec![make_trace(
            "Reset can race with visit processing and lose state",
            2.0,
            0.5,
            Some("src/b.rs"),
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
            2.0,
            0.5,
            Some("src/a.rs"),
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
            2.0,
            0.5,
            Some("src/a.rs"),
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
        let feedback = vec![make_feedback("error handling is missing", "tp", "src/a.rs")];
        let traces = vec![make_trace(
            "error handling is missing for IO operations",
            2.0,
            0.5,
            Some("src/a.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1, "single trace, margin trivially satisfied");
        assert_eq!(stats.fuzzy_same_file, 1);
    }

    // --- tier 2.5: deep-normalized path (#307) ---

    #[test]
    fn deep_path_matches_dotdot_prefix() {
        let feedback = vec![make_feedback(
            "SQL injection risk",
            "tp",
            "../../../samples/rust/patterns.rs",
        )];
        let traces = vec![make_trace(
            "SQL injection risk",
            2.0,
            0.5,
            Some("./samples/rust/patterns.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            samples.len(),
            1,
            "deep path norm should match ../../../ to ./"
        );
        assert_eq!(stats.path_normalized, 1);
    }

    #[test]
    fn deep_path_matches_mixed_dotdot() {
        let feedback = vec![make_feedback(
            "Hardcoded secret",
            "fp",
            "../quorum-ast-rules/rules/python/tests/bare-except.py",
        )];
        let traces = vec![make_trace(
            "Hardcoded secret",
            0.0,
            1.0,
            Some("../../quorum-ast-rules/rules/python/tests/bare-except.py"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        assert_eq!(stats.path_normalized, 1);
    }

    // --- tier 3.5: suffix matching (#307) ---

    #[test]
    fn suffix_matches_absolute_vs_relative() {
        let feedback = vec![make_feedback(
            "Missing error context",
            "tp",
            "/Users/jsnyder/Sources/github.com/jsnyder/quorum/src/main.rs",
        )];
        let traces = vec![make_trace(
            "Missing error context",
            2.0,
            0.5,
            Some("src/main.rs"),
        )];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            samples.len(),
            1,
            "suffix match should recover absolute path"
        );
        assert_eq!(stats.suffix_matched, 1);
    }

    #[test]
    fn suffix_rejects_filename_only_match() {
        let feedback = vec![make_feedback("Bug", "tp", "/some/path/main.rs")];
        let traces = vec![make_trace("Bug", 1.0, 0.5, Some("main.rs"))];
        let (_samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            stats.suffix_matched, 0,
            "filename-only should not suffix match"
        );
    }

    #[test]
    fn suffix_rejects_ambiguous_matches() {
        let feedback = vec![make_feedback(
            "Unused variable",
            "tp",
            "/Users/jsnyder/Sources/repo/src/main.rs",
        )];
        let traces = vec![
            make_trace("Unused variable", 2.0, 0.0, Some("project-a/src/main.rs")),
            make_trace("Unused variable", 0.0, 2.0, Some("project-b/src/main.rs")),
        ];
        let (_samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            stats.suffix_matched, 0,
            "ambiguous suffix matches (both share src/main.rs suffix) should be skipped"
        );
    }

    // --- tier 4: normalized title-only ---

    #[test]
    fn normalized_title_only_fallback_matches() {
        let feedback = vec![make_feedback("fixed .tmp filename", "tp", "src/a.rs")];
        let traces = vec![make_trace("fixed `.tmp` filename", 1.5, 0.5, None)];
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(
            samples.len(),
            1,
            "normalized title-only fallback should match"
        );
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
        assert!(
            samples.is_empty(),
            "normalized title-only fallback blocked when file-scoped traces exist"
        );
    }

    #[test]
    fn title_only_not_attempted_after_fuzzy_match() {
        let feedback = vec![make_feedback("error handling is missing", "tp", "src/a.rs")];
        let traces = vec![
            make_trace(
                "error handling is missing for IO operations",
                2.0,
                0.5,
                Some("src/a.rs"),
            ),
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
            make_trace(
                "reset can race with processing and lose state",
                1.5,
                0.5,
                Some("src/v.rs"),
            ),
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
        let total_classified = stats.exact_raw
            + stats.exact_normalized
            + stats.fuzzy_same_file
            + stats.raw_title_only
            + stats.normalized_title_only
            + stats.ambiguous_skipped
            + stats.below_threshold
            + stats.unmatched;
        // 3 eligible (tp, fp, tp) — wontfix filtered before classification
        assert_eq!(
            total_classified, 3,
            "every eligible entry must be classified"
        );
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
        let feedback = vec![make_feedback(
            "uses a fixed .tmp filename",
            "tp",
            "src/a.rs",
        )];
        let traces = vec![make_trace(
            "uses a fixed `.tmp` filename",
            2.0,
            0.3,
            Some("src/a.rs"),
        )];

        // Fuzzy enabled: should match via tier 2
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        assert_eq!(stats.exact_normalized, 1);

        // Fuzzy disabled: no match
        let (samples, stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &JoinFilter::default(), true);
        assert!(
            samples.is_empty(),
            "fuzzy disabled should skip normalized exact"
        );
        assert_eq!(stats.exact_normalized, 0);
        assert_eq!(stats.unmatched, 1);
    }

    #[test]
    fn disable_fuzzy_skips_fuzzy_same_file() {
        let feedback = vec![make_feedback(
            "Reset can race with visit processing",
            "tp",
            "src/visit.rs",
        )];
        let traces = vec![make_trace(
            "Reset can race with visit processing and lose the cleaned state",
            2.0,
            0.5,
            Some("src/visit.rs"),
        )];

        // Fuzzy enabled: should match via tier 3
        let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        assert_eq!(stats.fuzzy_same_file, 1);

        // Fuzzy disabled: no match
        let (samples, stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &JoinFilter::default(), true);
        assert!(
            samples.is_empty(),
            "fuzzy disabled should skip fuzzy same-file"
        );
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
        let (samples, stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &JoinFilter::default(), true);
        assert!(
            samples.is_empty(),
            "fuzzy disabled should skip normalized title-only"
        );
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
            make_trace("Unused var", 0.1, 1.8, None),                 // raw title-only
        ];
        let (samples, stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &JoinFilter::default(), true);
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
            &feedback,
            &traces,
            &JoinFilter::default(),
            false,
        );
        assert_eq!(
            samples.len(),
            1,
            "default filter should retain legacy traces"
        );
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
        let (samples, _stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
        assert!(
            samples.is_empty(),
            "positive filter should exclude legacy traces"
        );
    }

    #[test]
    fn join_filter_by_version() {
        let feedback = vec![
            make_feedback("Bug A", "tp", "src/a.rs"),
            make_feedback("Bug B", "fp", "src/b.rs"),
        ];
        let traces = vec![
            make_trace_with_provenance(
                "Bug A",
                2.0,
                0.3,
                Some("src/a.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.4",
                    "repo": "quorum"
                })),
            ),
            make_trace_with_provenance(
                "Bug B",
                0.1,
                1.8,
                Some("src/b.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.3",
                    "repo": "quorum"
                })),
            ),
        ];
        let filter = JoinFilter {
            quorum_version: Some("0.18.4".to_string()),
            ..Default::default()
        };
        let (samples, stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
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
            make_trace_with_provenance(
                "Bug A",
                2.0,
                0.3,
                Some("src/a.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.4",
                    "dirty": false
                })),
            ),
            make_trace_with_provenance(
                "Bug B",
                0.1,
                1.8,
                Some("src/b.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.4",
                    "dirty": true
                })),
            ),
        ];
        let filter = JoinFilter {
            clean_only: true,
            ..Default::default()
        };
        let (samples, stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
        assert_eq!(samples.len(), 1, "dirty trace should be excluded");
        assert_eq!(stats.exact_raw, 1);
        assert!(
            samples[0].1,
            "matched sample should be positive (Bug A, clean)"
        );
    }

    #[test]
    fn join_filter_by_repo() {
        let feedback = vec![
            make_feedback("Bug A", "tp", "src/a.rs"),
            make_feedback("Bug B", "fp", "src/b.rs"),
        ];
        let traces = vec![
            make_trace_with_provenance(
                "Bug A",
                2.0,
                0.3,
                Some("src/a.rs"),
                Some(serde_json::json!({
                    "repo": "quorum"
                })),
            ),
            make_trace_with_provenance(
                "Bug B",
                0.1,
                1.8,
                Some("src/b.rs"),
                Some(serde_json::json!({
                    "repo": "other-project"
                })),
            ),
        ];
        let filter = JoinFilter {
            repo: Some("quorum".to_string()),
            ..Default::default()
        };
        let (samples, _stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
        assert_eq!(samples.len(), 1, "only quorum-repo trace should match");
        assert!(samples[0].1, "matched sample should be positive (Bug A)");
    }

    #[test]
    fn join_filter_by_commit_sha() {
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace_with_provenance(
            "Bug",
            2.0,
            0.3,
            Some("src/a.rs"),
            Some(serde_json::json!({
                "commit_sha": "abc123"
            })),
        )];
        let filter = JoinFilter {
            commit_sha: Some("def456".to_string()),
            ..Default::default()
        };
        let (samples, _stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
        assert!(samples.is_empty(), "wrong commit_sha should exclude trace");
    }

    #[test]
    fn join_filter_by_run_id() {
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace_with_provenance(
            "Bug",
            2.0,
            0.3,
            Some("src/a.rs"),
            Some(serde_json::json!({
                "run_id": "run-42"
            })),
        )];
        let filter = JoinFilter {
            run_id: Some("run-42".to_string()),
            ..Default::default()
        };
        let (samples, stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
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
            make_trace_with_provenance(
                "Bug A",
                2.0,
                0.3,
                Some("src/a.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.4",
                    "dirty": false
                })),
            ),
            make_trace_with_provenance(
                "Bug B",
                0.1,
                1.8,
                Some("src/b.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.4",
                    "dirty": true
                })),
            ),
            make_trace_with_provenance(
                "Bug C",
                1.0,
                0.5,
                Some("src/c.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.3",
                    "dirty": false
                })),
            ),
        ];
        let filter = JoinFilter {
            quorum_version: Some("0.18.4".to_string()),
            clean_only: true,
            ..Default::default()
        };
        let (samples, _stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
        assert_eq!(
            samples.len(),
            1,
            "only Bug A passes both version + clean filter"
        );
        assert!(samples[0].1, "matched sample should be positive (Bug A)");
    }

    #[test]
    fn join_filter_clean_only_excludes_unknown_dirty() {
        let feedback = vec![
            make_feedback("Bug A", "tp", "src/a.rs"),
            make_feedback("Bug B", "fp", "src/b.rs"),
        ];
        let traces = vec![
            make_trace_with_provenance(
                "Bug A",
                2.0,
                0.3,
                Some("src/a.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.4",
                    "dirty": false
                })),
            ),
            make_trace_with_provenance(
                "Bug B",
                0.1,
                1.8,
                Some("src/b.rs"),
                Some(serde_json::json!({
                    "quorum_version": "0.18.4"
                })),
            ),
        ];
        let filter = JoinFilter {
            clean_only: true,
            ..Default::default()
        };
        let (samples, _stats) =
            join_feedback_and_traces_with_options(&feedback, &traces, &filter, false);
        assert_eq!(
            samples.len(),
            1,
            "trace with unknown dirty should be excluded by clean_only"
        );
    }

    #[test]
    fn backfill_stamps_file_path_from_unambiguous_feedback() {
        let feedback = vec![
            serde_json::json!({
                "finding_title": "SQL injection risk",
                "file_path": "src/db.rs",
                "verdict": "tp"
            }),
            serde_json::json!({
                "finding_title": "SQL injection risk",
                "file_path": "src/db.rs",
                "verdict": "fp"
            }),
        ];
        let mut traces = vec![serde_json::json!({
            "finding_title": "SQL injection risk",
            "finding_category": "security",
            "tp_weight": 2.0,
            "fp_weight": 0.5
        })];
        let stats = backfill_file_paths(&mut traces, &feedback);
        assert_eq!(traces[0]["file_path"].as_str(), Some("src/db.rs"));
        assert_eq!(stats.feedback_exact, 1);
        assert_eq!(stats.total_backfilled, 1);
    }

    #[test]
    fn backfill_skips_traces_with_existing_file_path() {
        let feedback = vec![serde_json::json!({
            "finding_title": "SQL injection",
            "file_path": "src/db.rs",
            "verdict": "tp"
        })];
        let mut traces = vec![serde_json::json!({
            "finding_title": "SQL injection",
            "finding_category": "security",
            "tp_weight": 1.0,
            "fp_weight": 0.0,
            "file_path": "src/other.rs"
        })];
        let stats = backfill_file_paths(&mut traces, &feedback);
        assert_eq!(traces[0]["file_path"].as_str(), Some("src/other.rs"));
        assert_eq!(stats.already_present, 1);
        assert_eq!(stats.total_backfilled, 0);
    }

    #[test]
    fn backfill_leaves_ambiguous_as_null() {
        let feedback = vec![
            serde_json::json!({
                "finding_title": "Use of unwrap()",
                "file_path": "src/a.rs",
                "verdict": "tp"
            }),
            serde_json::json!({
                "finding_title": "Use of unwrap()",
                "file_path": "src/b.rs",
                "verdict": "fp"
            }),
        ];
        let mut traces = vec![serde_json::json!({
            "finding_title": "Use of unwrap()",
            "finding_category": "correctness",
            "tp_weight": 1.0,
            "fp_weight": 0.0
        })];
        let stats = backfill_file_paths(&mut traces, &feedback);
        // Should NOT have file_path set
        let fp = traces[0].get("file_path");
        assert!(
            fp.is_none() || fp.unwrap().is_null() || fp.unwrap().as_str() == Some(""),
            "ambiguous title should not be stamped, got: {:?}",
            fp
        );
        assert_eq!(stats.ambiguous, 1);
        assert_eq!(stats.total_backfilled, 0);
    }

    #[test]
    fn backfill_uses_precedent_when_feedback_ambiguous() {
        let feedback = vec![
            serde_json::json!({
                "finding_title": "Use of unwrap()",
                "file_path": "src/a.rs",
                "verdict": "tp"
            }),
            serde_json::json!({
                "finding_title": "Use of unwrap()",
                "file_path": "src/b.rs",
                "verdict": "fp"
            }),
        ];
        let mut traces = vec![serde_json::json!({
            "finding_title": "Use of unwrap()",
            "finding_category": "correctness",
            "tp_weight": 1.0,
            "fp_weight": 0.0,
            "matched_precedents": [
                {"finding_title": "unwrap risk", "file_path": "src/a.rs", "verdict": "tp"},
                {"finding_title": "unwrap again", "file_path": "src/a.rs", "verdict": "tp"}
            ]
        })];
        let stats = backfill_file_paths(&mut traces, &feedback);
        assert_eq!(traces[0]["file_path"].as_str(), Some("src/a.rs"));
        assert_eq!(stats.precedent_inferred, 1);
        assert_eq!(stats.total_backfilled, 1);
    }

    #[test]
    fn backfill_handles_empty_inputs() {
        let mut empty_traces: Vec<serde_json::Value> = vec![];
        let empty_feedback: Vec<serde_json::Value> = vec![];
        let stats = backfill_file_paths(&mut empty_traces, &empty_feedback);
        assert_eq!(stats, BackfillStats::default());
    }

    #[test]
    fn backfill_skips_trace_with_missing_title() {
        let feedback = vec![serde_json::json!({
            "finding_title": "A",
            "file_path": "f.rs",
            "verdict": "tp"
        })];
        let mut traces = vec![serde_json::json!({
            "tp_weight": 1.0,
            "fp_weight": 0.0
        })];
        let stats = backfill_file_paths(&mut traces, &feedback);
        assert_eq!(stats.no_match, 1);
        assert_eq!(stats.total_backfilled, 0);
    }

    #[test]
    fn backfill_falls_through_to_normalized_title() {
        // Feedback has rule-prefixed title, trace has bare title
        let feedback = vec![serde_json::json!({
            "finding_title": "bare-except-pass: Using bare except: pass",
            "file_path": "src/handler.py",
            "verdict": "tp"
        })];
        let mut traces = vec![serde_json::json!({
            "finding_title": "Using bare except: pass",
            "finding_category": "correctness",
            "tp_weight": 1.0,
            "fp_weight": 0.0
        })];
        let stats = backfill_file_paths(&mut traces, &feedback);
        assert_eq!(traces[0]["file_path"].as_str(), Some("src/handler.py"));
        assert_eq!(stats.feedback_normalized, 1);
        assert_eq!(stats.total_backfilled, 1);
    }

    #[test]
    fn backfill_reports_correct_stats_and_mutations_on_mixed_corpus() {
        let feedback = vec![
            serde_json::json!({"finding_title": "A", "file_path": "f1.rs", "verdict": "tp"}),
            serde_json::json!({"finding_title": "B", "file_path": "f2.rs", "verdict": "tp"}),
            serde_json::json!({"finding_title": "C", "file_path": "f3.rs", "verdict": "tp"}),
            serde_json::json!({"finding_title": "C", "file_path": "f4.rs", "verdict": "fp"}),
        ];
        let mut traces = vec![
            // 0: Already has file_path -> skip
            serde_json::json!({"finding_title": "A", "tp_weight": 1.0, "fp_weight": 0.0, "file_path": "f1.rs"}),
            // 1: Exact match -> backfill to f2.rs
            serde_json::json!({"finding_title": "B", "tp_weight": 1.0, "fp_weight": 0.0}),
            // 2: Ambiguous (C -> f3.rs AND f4.rs)
            serde_json::json!({"finding_title": "C", "tp_weight": 1.0, "fp_weight": 0.0}),
            // 3: No match at all
            serde_json::json!({"finding_title": "D", "tp_weight": 1.0, "fp_weight": 0.0}),
        ];
        let stats = backfill_file_paths(&mut traces, &feedback);

        // Verify stats
        assert_eq!(stats.already_present, 1);
        assert_eq!(stats.feedback_exact, 1);
        assert_eq!(stats.ambiguous, 1);
        assert_eq!(stats.no_match, 1);
        assert_eq!(stats.total_backfilled, 1);

        // Verify actual mutations
        assert_eq!(traces[0]["file_path"].as_str(), Some("f1.rs"), "unchanged");
        assert_eq!(traces[1]["file_path"].as_str(), Some("f2.rs"), "backfilled");
        assert!(
            traces[2].get("file_path").is_none()
                || traces[2]["file_path"].is_null()
                || traces[2]["file_path"].as_str() == Some(""),
            "ambiguous: should not be stamped"
        );
        assert!(
            traces[3].get("file_path").is_none()
                || traces[3]["file_path"].is_null()
                || traces[3]["file_path"].as_str() == Some(""),
            "no match: should not be stamped"
        );
    }

    // --- compute_calibrator_model tests ---

    fn make_model_feedback(title: &str, verdict: &str, file_path: &str) -> serde_json::Value {
        serde_json::json!({
            "finding_title": title,
            "verdict": verdict,
            "file_path": file_path,
            "finding_category": "test",
            "reason": "test",
            "timestamp": "2026-01-01T00:00:00Z",
            "provenance": "human"
        })
    }

    #[test]
    fn compute_model_from_feedback() {
        let feedback = vec![
            // 3 entries for same family -> known family
            make_model_feedback("Function `a` has complexity 10", "fp", "test.rs"),
            make_model_feedback("Function `b` has complexity 20", "fp", "test.rs"),
            make_model_feedback("Function `c` has complexity 5", "tp", "test.rs"),
            // 2 entries for another family -> below threshold, novel
            make_model_feedback("Missing error handling", "tp", "lib.rs"),
            make_model_feedback("Missing error handling", "tp", "main.rs"),
        ];
        let model = compute_calibrator_model(&feedback).unwrap();
        // Known family: "function `` has complexity N"
        assert!(
            model
                .family_fp_rate
                .contains_key("function `` has complexity N"),
            "family with 3+ entries should be in model, got keys: {:?}",
            model.family_fp_rate.keys().collect::<Vec<_>>()
        );
        let rate = model.family_fp_rate["function `` has complexity N"];
        assert!(
            (rate - 0.6667).abs() < 0.01,
            "2 FP / 3 total = 0.667, got {rate}"
        );
        // Novel family not in map (only 2 entries, below FAMILY_MIN_SUPPORT=3)
        assert!(
            !model.family_fp_rate.contains_key("missing error handling"),
            "family with < 3 entries should not be in model"
        );
        // Global FP rate: 2 FP out of 5 total = 0.4
        assert!(
            (model.meta.global_fp_rate - 0.4).abs() < 0.01,
            "global FP rate should be 0.4, got {}",
            model.meta.global_fp_rate
        );
    }

    #[test]
    fn compute_model_wontfix_treated_as_negative() {
        let feedback = vec![
            make_model_feedback("Bug A", "tp", "a.rs"),
            make_model_feedback("Bug A", "tp", "b.rs"),
            make_model_feedback("Bug A", "wontfix", "c.rs"),
            make_model_feedback("Bug A", "wontfix", "d.rs"),
            make_model_feedback("Bug A", "wontfix", "e.rs"),
        ];
        let model = compute_calibrator_model(&feedback).unwrap();
        // 3 wontfix + 2 tp = 5 total, 3 negative -> global_fp_rate = 0.6
        assert!(
            (model.meta.global_fp_rate - 0.6).abs() < 0.01,
            "wontfix should count as negative, got {}",
            model.meta.global_fp_rate
        );
    }

    #[test]
    fn compute_model_empty_feedback_returns_none() {
        let feedback: Vec<serde_json::Value> = vec![];
        assert!(compute_calibrator_model(&feedback).is_none());
    }

    #[test]
    fn compute_model_word_lor_signs() {
        // Create enough feedback to pass min_support for words
        let mut feedback = Vec::new();
        for i in 0..10 {
            feedback.push(make_model_feedback(
                "hardcoded secret in config",
                "fp",
                &format!("f{i}.py"),
            ));
        }
        for i in 0..10 {
            feedback.push(make_model_feedback(
                "SQL injection via user input",
                "tp",
                &format!("g{i}.py"),
            ));
        }
        let model = compute_calibrator_model(&feedback).unwrap();
        // "hardcoded" should have positive lor (FP-leaning)
        if let Some(&lor) = model.word_lor.get("hardcoded") {
            assert!(
                lor > 0.0,
                "hardcoded should be FP-leaning (positive lor), got {lor}"
            );
        }
        // "sql" should have negative lor (TP-leaning)
        if let Some(&lor) = model.word_lor.get("sql") {
            assert!(
                lor < 0.0,
                "sql should be TP-leaning (negative lor), got {lor}"
            );
        }
    }

    #[test]
    fn compute_model_language_fp_rates() {
        let mut feedback = Vec::new();
        // 6 rust findings: 4 FP, 2 TP -> fp_rate = 0.667
        for i in 0..4 {
            feedback.push(make_model_feedback("Bug", "fp", &format!("f{i}.rs")));
        }
        for i in 0..2 {
            feedback.push(make_model_feedback("Bug", "tp", &format!("g{i}.rs")));
        }
        // 5 python findings: 1 FP, 4 TP -> fp_rate = 0.2
        feedback.push(make_model_feedback("Bug", "fp", "a.py"));
        for i in 0..4 {
            feedback.push(make_model_feedback("Bug", "tp", &format!("h{i}.py")));
        }
        let model = compute_calibrator_model(&feedback).unwrap();
        assert!(
            model.language_fp_rate.contains_key("rust"),
            "rust should have fp rate"
        );
        assert!(
            (model.language_fp_rate["rust"] - 0.6667).abs() < 0.01,
            "rust fp rate should be ~0.667, got {}",
            model.language_fp_rate["rust"]
        );
        assert!(
            (model.language_fp_rate["python"] - 0.2).abs() < 0.01,
            "python fp rate should be 0.2, got {}",
            model.language_fp_rate["python"]
        );
    }

    // --- learn_weights tests ---

    #[test]
    fn sample_features_score_matches_manual() {
        let f = SampleFeatures {
            precedent_score: 0.8,
            word_lor: -0.5,
            family_fp_inv: 0.7,
            language_fp_inv: 0.6,
        };
        let w = ScoreWeights {
            score: 0.5,
            word_lor: 1.5,
            family_fp_inv: 1.0,
            language_fp_inv: 0.5,
        };
        let expected = 0.5 * 0.8 + 1.5 * (-0.5) + 1.0 * 0.7 + 0.5 * 0.6;
        assert!((f.score(&w) - expected).abs() < 1e-9);
    }

    #[test]
    fn learn_weights_returns_none_below_threshold() {
        let features: Vec<(SampleFeatures, bool)> = (0..49)
            .map(|i| {
                (
                    SampleFeatures {
                        precedent_score: i as f64 / 49.0,
                        word_lor: 0.0,
                        family_fp_inv: 0.5,
                        language_fp_inv: 0.5,
                    },
                    i % 2 == 0,
                )
            })
            .collect();
        assert!(learn_weights(&features, 5).is_none());
    }

    #[test]
    fn learn_weights_finds_separable_weights() {
        // TP samples have high precedent_score, FP samples have low.
        // The grid search should assign positive weight to `score`.
        let mut features = Vec::new();
        for i in 0..100 {
            let is_tp = i >= 50;
            features.push((
                SampleFeatures {
                    precedent_score: if is_tp { 0.9 } else { 0.1 },
                    word_lor: 0.0,
                    family_fp_inv: 0.5,
                    language_fp_inv: 0.5,
                },
                is_tp,
            ));
        }
        let result = learn_weights(&features, 5).unwrap();
        assert!(
            result.weights.score > 0.0,
            "score weight should be positive for separable data"
        );
        assert!(
            result.pr_auc > 0.8,
            "PR-AUC should be high for separable data"
        );
    }

    #[test]
    fn learn_weights_stable_on_uniform_data() {
        // All features perfectly separate classes → all folds should agree.
        let mut features = Vec::new();
        for i in 0..200 {
            let is_tp = i >= 100;
            features.push((
                SampleFeatures {
                    precedent_score: if is_tp { 0.9 } else { 0.1 },
                    word_lor: if is_tp { 0.5 } else { -0.5 },
                    family_fp_inv: 0.5,
                    language_fp_inv: 0.5,
                },
                is_tp,
            ));
        }
        let result = learn_weights(&features, 5).unwrap();
        assert!(result.stable, "weights should be stable on uniform data");
    }

    #[test]
    fn deterministic_permutation_is_valid() {
        let perm = super::deterministic_permutation(100);
        assert_eq!(perm.len(), 100);
        let mut sorted = perm.clone();
        sorted.sort();
        assert_eq!(sorted, (0..100).collect::<Vec<_>>());
        assert_ne!(perm, (0..100).collect::<Vec<_>>(), "should be shuffled");
    }

    #[test]
    fn weights_stable_identical_folds() {
        let w = ScoreWeights {
            score: 0.5,
            word_lor: 1.5,
            family_fp_inv: 1.0,
            language_fp_inv: 0.5,
        };
        assert!(super::weights_stable(&[w.clone(), w.clone(), w], 0.20));
    }

    #[test]
    fn weights_unstable_divergent_folds() {
        let w1 = ScoreWeights {
            score: 0.5,
            word_lor: 1.5,
            family_fp_inv: 1.0,
            language_fp_inv: 0.5,
        };
        let w2 = ScoreWeights {
            score: 2.0,
            word_lor: 0.0,
            family_fp_inv: 0.0,
            language_fp_inv: 1.5,
        };
        assert!(!super::weights_stable(&[w1, w2], 0.20));
    }

    #[test]
    fn expanded_features_to_vec_correct_order() {
        let f = ExpandedFeatures {
            log1p_tp_weight: 1.0,
            log1p_fp_weight: 0.5,
            precedent_count: 3.0,
            max_similarity: 0.9,
            mean_similarity: 0.7,
            has_no_precedents: 0.0,
            log1p_soft_fp_weight: 0.3,
            log1p_full_suppress_weight: 0.1,
            log1p_wontfix_weight: 0.0,
            category_fp_rate: 0.25,
            severity_fp_rate: 0.18,
            model_fp_rate: 0.22,
            max_word_lor: 2.1,
            min_word_lor: -1.5,
            count_negative_lor_tokens: 3.0,
            is_test_file: 0.0,
            source_is_ast: 0.0,
            finding_count_same_file: 0.0,
            file_fp_rate: 0.0,
            finding_span_lines: 0.0,
            is_mock_or_fixture: 0.0,
            is_generated_or_vendor: 0.0,
        };
        let v = f.to_vec();
        assert_eq!(v.len(), 22);
        assert!((v[0] - 1.0).abs() < 1e-9); // log1p_tp_weight
        assert!((v[1] - 0.5).abs() < 1e-9); // log1p_fp_weight
        assert!((v[5] - 0.0).abs() < 1e-9); // has_no_precedents
        assert!((v[14] - 3.0).abs() < 1e-9); // count_negative_lor_tokens
    }

    #[test]
    fn expanded_features_names_match_vec_order() {
        let names = ExpandedFeatures::feature_names();
        assert_eq!(names.len(), 22);
        assert_eq!(names[0], "log1p_tp_weight");
        assert_eq!(names[5], "has_no_precedents");
        assert_eq!(names[9], "category_fp_rate");
        assert_eq!(names[14], "count_negative_lor_tokens");
        assert_eq!(names[15], "is_test_file");
        assert_eq!(names[16], "source_is_ast");
        assert_eq!(names[17], "finding_count_same_file");
        assert_eq!(names[18], "file_fp_rate");
        assert_eq!(names[19], "finding_span_lines");
        assert_eq!(names[20], "is_mock_or_fixture");
        assert_eq!(names[21], "is_generated_or_vendor");
    }

    #[test]
    fn expanded_features_zeros() {
        let f = ExpandedFeatures::zeros();
        let v = f.to_vec();
        assert!(v.iter().all(|&x| x == 0.0));
    }

    // --- fold-local feature extraction ---

    #[test]
    fn beta_smoothed_rate_basic() {
        // 3 FP out of 10 total, global_rate=0.18, alpha=5
        let rate = beta_smoothed_rate(3, 10, 0.18);
        // (3 + 5*0.18) / (10 + 5) = 3.9/15 = 0.26
        assert!((rate - 0.26).abs() < 0.01);
    }

    #[test]
    fn beta_smoothed_rate_zero_observations() {
        // No observations -> converges to global rate
        let rate = beta_smoothed_rate(0, 0, 0.18);
        // (0 + 5*0.18) / (0 + 5) = 0.9/5 = 0.18
        assert!((rate - 0.18).abs() < 0.01);
    }

    #[test]
    fn compute_fold_local_stats_basic() {
        let samples: Vec<JoinedSample> = (0..100)
            .map(|i| JoinedSample {
                title: if i < 20 {
                    "bad unwrap pattern".to_string()
                } else {
                    "good error handling".to_string()
                },
                category: "correctness".to_string(),
                severity: "medium".to_string(),
                model: "gpt-5.4".to_string(),
                tp_weight: if i < 20 { 0.1 } else { 2.0 },
                fp_weight: if i < 20 { 2.0 } else { 0.1 },
                soft_fp_weight: 0.0,
                full_suppress_weight: 0.0,
                wontfix_weight: 0.0,
                precedent_count: 1,
                max_similarity: 0.5,
                mean_similarity: 0.5,
                is_fp: i < 20,
                family: format!("family_{}", i % 10),
                file_path: "src/some_file.rs".to_string(),
                source_is_ast: false,
                finding_span_lines: 3,
            })
            .collect();
        let refs: Vec<&JoinedSample> = samples.iter().collect();
        let stats = compute_fold_local_stats(&refs);
        assert!(
            (stats.global_fp_rate - 0.20).abs() < 0.01,
            "global FP rate should be 0.20, got {}",
            stats.global_fp_rate
        );
        // "correctness" has 20/100 FP -> beta_smoothed(20, 100, 0.20) = (20+1)/(100+5) approx 0.20
        assert!(stats.category_fp_rates.contains_key("correctness"));
        let cat_rate = stats.category_fp_rates["correctness"];
        assert!(
            (cat_rate - 0.20).abs() < 0.02,
            "category FP rate should be close to 0.20, got {cat_rate}"
        );
    }

    #[test]
    fn extract_single_expanded_all_finite() {
        let s = JoinedSample {
            title: "potential SQL injection".to_string(),
            category: "security".to_string(),
            severity: "high".to_string(),
            model: "gpt-5.4".to_string(),
            tp_weight: 1.5,
            fp_weight: 0.5,
            soft_fp_weight: 0.3,
            full_suppress_weight: 0.5,
            wontfix_weight: 0.0,
            precedent_count: 3,
            max_similarity: 0.85,
            mean_similarity: 0.72,
            is_fp: false,
            family: "sql_injection".to_string(),
            file_path: "src/some_file.rs".to_string(),
            source_is_ast: false,
            finding_span_lines: 3,
        };
        let stats = FoldLocalStats {
            category_fp_rates: HashMap::from([("security".to_string(), 0.15)]),
            severity_fp_rates: HashMap::from([("high".to_string(), 0.12)]),
            model_fp_rates: HashMap::from([("gpt-5.4".to_string(), 0.20)]),
            word_lor: HashMap::from([("sql".to_string(), -0.8), ("injection".to_string(), -1.2)]),
            global_fp_rate: 0.18,
            file_fp_rates: HashMap::new(),
            file_finding_counts: HashMap::new(),
        };
        let (features, is_fp) = extract_single_expanded(&s, &stats);
        assert!(!is_fp);
        let v = features.to_vec();
        assert_eq!(v.len(), 22);
        assert!(v.iter().all(|x| x.is_finite()));
        // Check specific values
        assert!((features.log1p_tp_weight - 1.5_f64.ln_1p()).abs() < 1e-9);
        assert!((features.precedent_count - 3.0).abs() < 1e-9);
        assert!((features.category_fp_rate - 0.15).abs() < 1e-9);
        assert!((features.min_word_lor - (-1.2)).abs() < 1e-9);
        assert!((features.count_negative_lor_tokens - 2.0).abs() < 1e-9);
    }

    #[test]
    fn univariate_screen_selects_discriminative_features() {
        // 100 samples, 20% FP (baseline AP = 0.20)
        let samples: Vec<(ExpandedFeatures, bool)> = (0..100)
            .map(|i| {
                let is_fp = i < 20;
                let mut f = ExpandedFeatures::zeros();
                // Feature 0 (log1p_tp_weight): perfect separation
                f.log1p_tp_weight = if is_fp { 0.9 } else { 0.1 };
                // Feature 1 (log1p_fp_weight): anti-correlated with label order
                // Assign linearly increasing scores so positives (first 20) get LOW
                // scores and negatives (last 80) get HIGH scores. This means ranking
                // by descending score puts negatives first -> AP should be near baseline.
                f.log1p_fp_weight = i as f64 / 100.0;
                // Feature 2 (precedent_count): moderate separation
                f.precedent_count = if is_fp { 0.7 } else { 0.3 };
                (f, is_fp)
            })
            .collect();

        let selected = univariate_screen(&samples, 0.20);
        // Feature 0 should be selected (perfect -> AP=1.0, lift=0.80)
        assert!(selected.contains(&0), "Perfect feature should be selected");
        // Feature 1 should NOT be selected (random -> AP~0.20, lift~0)
        assert!(
            !selected.contains(&1),
            "Random feature should not be selected"
        );
        // Feature 2 should be selected (moderate separation)
        assert!(selected.contains(&2), "Moderate feature should be selected");
    }

    #[test]
    fn univariate_screen_empty_returns_empty() {
        let samples: Vec<(ExpandedFeatures, bool)> = vec![];
        let selected = univariate_screen(&samples, 0.20);
        assert!(selected.is_empty());
    }

    #[test]
    fn feature_importance_scores_sorted_descending() {
        let samples: Vec<(ExpandedFeatures, bool)> = (0..50)
            .map(|i| {
                let is_fp = i < 10;
                let mut f = ExpandedFeatures::zeros();
                f.log1p_tp_weight = if is_fp { 1.0 } else { 0.0 };
                (f, is_fp)
            })
            .collect();
        let scores = feature_importance_scores(&samples);
        assert_eq!(scores.len(), 22);
        // Should be sorted descending by AP
        for w in scores.windows(2) {
            assert!(w[0].1 >= w[1].1);
        }
        // Feature 0 should be first (highest AP)
        assert_eq!(scores[0].0, 0);
    }

    // -----------------------------------------------------------------------
    // extract_joined_samples tests
    // -----------------------------------------------------------------------

    fn make_test_model() -> CalibratorModel {
        CalibratorModel {
            meta: ModelMeta {
                computed_at: "2026-05-16T00:00:00Z".to_string(),
                feedback_count: 10,
                global_fp_rate: 0.3,
                learned_weights: None,
            },
            weights: ScoreWeights {
                score: 0.5,
                word_lor: 1.5,
                family_fp_inv: 1.0,
                language_fp_inv: 0.5,
            },
            logistic_model: None,
            word_lor: HashMap::new(),
            family_fp_rate: HashMap::new(),
            language_fp_rate: HashMap::new(),
            category_fp_rate_map: None,
            severity_fp_rate: None,
            model_fp_rate: None,
            file_fp_rate: None,
            file_finding_counts: None,
        }
    }

    #[test]
    fn extract_joined_samples_basic() {
        let trace = serde_json::json!({
            "finding_title": "SQL injection risk",
            "file_path": "src/db.rs",
            "tp_weight": 1.5,
            "fp_weight": 0.5,
            "soft_fp_weight": 0.3,
            "wontfix_weight": 0.0,
            "finding_category": "security",
            "input_severity": "high",
            "model": "gpt-5.4",
            "precedent_count": 2,
            "max_similarity": 0.85,
            "mean_similarity": 0.72,
        });
        let feedback_entry = serde_json::json!({
            "finding_title": "SQL injection risk",
            "file_path": "src/db.rs",
            "verdict": "tp",
        });
        let traces = vec![trace];
        let feedback = vec![feedback_entry];
        let model = make_test_model();
        let filter = JoinFilter::default();

        let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.title, "SQL injection risk");
        assert_eq!(s.category, "security");
        assert_eq!(s.severity, "high");
        assert_eq!(s.model, "gpt-5.4");
        assert!(!s.is_fp);
        assert!((s.tp_weight - 1.5).abs() < 1e-9);
        assert!((s.fp_weight - 0.5).abs() < 1e-9);
        assert!((s.soft_fp_weight - 0.3).abs() < 1e-9);
        assert_eq!(s.precedent_count, 2);
        assert!((s.max_similarity - 0.85).abs() < 1e-9);
        assert!((s.mean_similarity - 0.72).abs() < 1e-9);
    }

    #[test]
    fn extract_joined_samples_fp_label() {
        let trace = serde_json::json!({
            "finding_title": "Unused import",
            "file_path": "src/lib.rs",
            "tp_weight": 0.1,
            "fp_weight": 2.0,
        });
        let feedback_entry = serde_json::json!({
            "finding_title": "Unused import",
            "file_path": "src/lib.rs",
            "verdict": "fp",
        });
        let traces = vec![trace];
        let feedback = vec![feedback_entry];
        let model = make_test_model();
        let filter = JoinFilter::default();

        let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
        assert_eq!(samples.len(), 1);
        assert!(samples[0].is_fp);
        // Verify defaults for missing fields
        assert_eq!(samples[0].category, "unknown");
        assert_eq!(samples[0].severity, "medium");
        assert_eq!(samples[0].model, "unknown");
    }

    #[test]
    fn extract_joined_samples_skips_wontfix() {
        let trace = serde_json::json!({
            "finding_title": "Style issue",
            "file_path": "src/main.rs",
            "tp_weight": 0.5,
            "fp_weight": 0.5,
        });
        let feedback_entry = serde_json::json!({
            "finding_title": "Style issue",
            "file_path": "src/main.rs",
            "verdict": "wontfix",
        });
        let traces = vec![trace];
        let feedback = vec![feedback_entry];
        let model = make_test_model();
        let filter = JoinFilter::default();

        let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
        assert!(samples.is_empty());
    }

    #[test]
    fn extract_joined_samples_partial_counts_as_tp() {
        let trace = serde_json::json!({
            "finding_title": "Buffer overflow",
            "file_path": "src/buf.rs",
            "tp_weight": 1.0,
            "fp_weight": 0.2,
            "finding_category": "memory-safety",
            "input_severity": "critical",
            "model": "gemini-2.5-pro",
            "precedent_count": 5,
            "max_similarity": 0.95,
            "mean_similarity": 0.80,
        });
        let feedback_entry = serde_json::json!({
            "finding_title": "Buffer overflow",
            "file_path": "src/buf.rs",
            "verdict": "partial",
        });
        let traces = vec![trace];
        let feedback = vec![feedback_entry];
        let model = make_test_model();
        let filter = JoinFilter::default();

        let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
        assert_eq!(samples.len(), 1);
        assert!(!samples[0].is_fp); // partial = not FP
        assert_eq!(samples[0].category, "memory-safety");
        assert_eq!(samples[0].severity, "critical");
    }

    #[test]
    fn extract_joined_samples_precedent_count_inferred() {
        // When precedent_count is absent but weights > 0, infer 1
        let trace = serde_json::json!({
            "finding_title": "Inferred precedent",
            "file_path": "src/a.rs",
            "tp_weight": 1.0,
            "fp_weight": 0.0,
        });
        let feedback_entry = serde_json::json!({
            "finding_title": "Inferred precedent",
            "file_path": "src/a.rs",
            "verdict": "tp",
        });
        let traces = vec![trace];
        let feedback = vec![feedback_entry];
        let model = make_test_model();
        let filter = JoinFilter::default();

        let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].precedent_count, 1);
    }

    #[test]
    fn extract_joined_samples_unmatched_feedback_skipped() {
        let trace = serde_json::json!({
            "finding_title": "SQL injection",
            "file_path": "src/db.rs",
            "tp_weight": 1.0,
            "fp_weight": 0.5,
        });
        let feedback_entry = serde_json::json!({
            "finding_title": "Completely different finding",
            "file_path": "src/other.rs",
            "verdict": "tp",
        });
        let traces = vec![trace];
        let feedback = vec![feedback_entry];
        let model = make_test_model();
        let filter = JoinFilter::default();

        let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
        assert!(samples.is_empty());
    }

    #[test]
    fn extract_joined_samples_family_populated() {
        let trace = serde_json::json!({
            "finding_title": "bare-except-pass: dangerous pattern",
            "file_path": "src/main.py",
            "tp_weight": 1.0,
            "fp_weight": 0.0,
        });
        let feedback_entry = serde_json::json!({
            "finding_title": "bare-except-pass: dangerous pattern",
            "file_path": "src/main.py",
            "verdict": "tp",
        });
        let traces = vec![trace];
        let feedback = vec![feedback_entry];
        let model = make_test_model();
        let filter = JoinFilter::default();

        let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].family, "dangerous pattern");
    }

    // --- learn_logistic tests ---

    #[test]
    fn learn_logistic_returns_none_below_min_samples() {
        let samples: Vec<JoinedSample> = (0..50)
            .map(|i| JoinedSample {
                title: format!("finding {}", i),
                category: "correctness".to_string(),
                severity: "medium".to_string(),
                model: "gpt-5.4".to_string(),
                tp_weight: 1.0,
                fp_weight: 0.5,
                soft_fp_weight: 0.0,
                full_suppress_weight: 0.5,
                wontfix_weight: 0.0,
                precedent_count: 1,
                max_similarity: 0.5,
                mean_similarity: 0.5,
                is_fp: i < 10,
                family: format!("family_{}", i % 5),
                file_path: "src/some_file.rs".to_string(),
                source_is_ast: false,
                finding_span_lines: 3,
            })
            .collect();
        assert!(learn_logistic(&samples, 5).is_none());
    }

    #[test]
    fn learn_logistic_returns_none_class_imbalance() {
        // 200 samples but only 5 FP (below MIN_CLASS_COUNT=30)
        let samples: Vec<JoinedSample> = (0..200)
            .map(|i| JoinedSample {
                title: format!("finding {}", i),
                category: "correctness".to_string(),
                severity: "medium".to_string(),
                model: "gpt-5.4".to_string(),
                tp_weight: 1.0,
                fp_weight: 0.5,
                soft_fp_weight: 0.0,
                full_suppress_weight: 0.5,
                wontfix_weight: 0.0,
                precedent_count: 1,
                max_similarity: 0.5,
                mean_similarity: 0.5,
                is_fp: i < 5, // only 5 FP
                family: format!("family_{}", i % 20),
                file_path: "src/some_file.rs".to_string(),
                source_is_ast: false,
                finding_span_lines: 3,
            })
            .collect();
        assert!(learn_logistic(&samples, 5).is_none());
    }

    #[test]
    fn learn_logistic_on_separable_synthetic_data() {
        // 300 samples with clear signal in tp_weight/fp_weight
        let samples: Vec<JoinedSample> = (0..300)
            .map(|i| {
                let is_fp = i < 60; // 20% FP
                JoinedSample {
                    title: if is_fp {
                        "bad pattern unwrap".to_string()
                    } else {
                        "good error handling code".to_string()
                    },
                    category: if is_fp {
                        "style".to_string()
                    } else {
                        "correctness".to_string()
                    },
                    severity: "medium".to_string(),
                    model: "gpt-5.4".to_string(),
                    tp_weight: if is_fp { 0.1 } else { 2.5 },
                    fp_weight: if is_fp { 2.5 } else { 0.1 },
                    soft_fp_weight: if is_fp { 1.5 } else { 0.0 },
                    full_suppress_weight: if is_fp { 2.5 } else { 0.1 },
                    wontfix_weight: 0.0,
                    precedent_count: if is_fp { 0 } else { 4 },
                    max_similarity: if is_fp { 0.3 } else { 0.85 },
                    mean_similarity: if is_fp { 0.2 } else { 0.75 },
                    is_fp,
                    family: format!("family_{}", i % 30),
                    file_path: "src/some_file.rs".to_string(),
                    source_is_ast: false,
                    finding_span_lines: 3,
                }
            })
            .collect();

        let result = learn_logistic(&samples, 5);
        assert!(result.is_some(), "Should succeed on well-separated data");
        let r = result.unwrap();
        assert!(
            r.ap_score > r.baseline_ap + 0.02,
            "AP {} should beat baseline {} + 0.02",
            r.ap_score,
            r.baseline_ap
        );
        assert!(
            r.selected_feature_names.len() >= 2,
            "Should select at least 2 features"
        );
        assert_eq!(r.n_samples, 300);
        assert_eq!(r.n_fp, 60);
        assert!(
            r.suppress_threshold < r.boost_threshold,
            "suppress {} should be < boost {} for well-separated data (suppress = 99th pctl TP P(FP), boost = 5th pctl FP P(FP))",
            r.suppress_threshold,
            r.boost_threshold
        );
        assert!(r.coefficients.len() == r.selected_features.len());
        assert!(r.feature_means.len() == r.selected_features.len());
    }

    #[test]
    fn group_k_fold_same_family_same_fold() {
        let families = vec!["a", "a", "b", "b", "c", "c", "a", "b"];
        let folds = group_k_fold(&families, 3);
        // All "a" should be in the same fold
        let a_folds: Vec<usize> = families
            .iter()
            .zip(folds.iter())
            .filter(|(f, _)| **f == "a")
            .map(|(_, fold)| *fold)
            .collect();
        assert!(a_folds.iter().all(|f| *f == a_folds[0]));
        // All "b" should be in the same fold
        let b_folds: Vec<usize> = families
            .iter()
            .zip(folds.iter())
            .filter(|(f, _)| **f == "b")
            .map(|(_, fold)| *fold)
            .collect();
        assert!(b_folds.iter().all(|f| *f == b_folds[0]));
    }

    #[test]
    fn tokenize_title_drops_digits_and_lowercases() {
        let tokens = tokenize_title("Buffer overflow in parse123 at L42");
        assert!(tokens.contains(&"buffer".to_string()));
        assert!(tokens.contains(&"overflow".to_string()));
        assert!(tokens.contains(&"parse".to_string()));
        assert!(tokens.contains(&"at".to_string()));
        assert!(
            !tokens
                .iter()
                .any(|t| t == "123" || t == "42" || t == "parse123")
        );
        assert!(!tokens.iter().any(|t| t.len() < 2));
    }

    #[test]
    fn tokenize_title_keeps_underscores() {
        let tokens = tokenize_title("buffer_overflow detected in my_func");
        assert!(tokens.contains(&"buffer_overflow".to_string()));
        assert!(tokens.contains(&"detected".to_string()));
        assert!(tokens.contains(&"my_func".to_string()));
    }

    #[test]
    fn tokenize_title_matches_word_re_regex() {
        use crate::calibrator_model::WORD_RE;
        let title = "SQL injection in `process_data` at line 42";
        let tokens = tokenize_title(title);
        let lower = title.to_lowercase();
        let regex_tokens: Vec<String> = WORD_RE
            .find_iter(&lower)
            .map(|m| m.as_str().to_string())
            .filter(|w| w.len() >= 2)
            .collect();
        assert_eq!(tokens, regex_tokens);
    }

    #[test]
    fn store_rate_maps_populates_model() {
        let samples = [
            JoinedSample {
                title: "SQL injection".to_string(),
                category: "security".to_string(),
                severity: "critical".to_string(),
                model: "gpt-5.4".to_string(),
                tp_weight: 0.0,
                fp_weight: 1.0,
                soft_fp_weight: 0.0,
                full_suppress_weight: 1.0,
                wontfix_weight: 0.0,
                precedent_count: 1,
                max_similarity: 0.9,
                mean_similarity: 0.9,
                is_fp: true,
                family: "sql".to_string(),
                file_path: "./src/db.rs".to_string(),
                source_is_ast: false,
                finding_span_lines: 5,
            },
            JoinedSample {
                title: "Buffer overflow".to_string(),
                category: "correctness".to_string(),
                severity: "warning".to_string(),
                model: "gpt-5.4".to_string(),
                tp_weight: 1.0,
                fp_weight: 0.0,
                soft_fp_weight: 0.0,
                full_suppress_weight: 0.0,
                wontfix_weight: 0.0,
                precedent_count: 1,
                max_similarity: 0.8,
                mean_similarity: 0.8,
                is_fp: false,
                family: "memory".to_string(),
                file_path: "./src/db.rs".to_string(),
                source_is_ast: true,
                finding_span_lines: 10,
            },
        ];
        let refs: Vec<&JoinedSample> = samples.iter().collect();
        let stats = compute_fold_local_stats(&refs);

        let mut model = make_test_model();
        store_rate_maps_in_model(&mut model, &stats);

        // Maps populated
        assert!(model.category_fp_rate_map.is_some());
        assert!(model.severity_fp_rate.is_some());
        assert!(model.model_fp_rate.is_some());
        assert!(model.file_fp_rate.is_some());
        assert!(model.file_finding_counts.is_some());

        // Category rates are correct (beta-smoothed)
        let cat = model.category_fp_rate_map.unwrap();
        assert!(cat.contains_key("security"));
        assert!(cat.contains_key("correctness"));

        // File path normalized: ./src/db.rs -> src/db.rs
        let file_fp = model.file_fp_rate.unwrap();
        assert!(
            file_fp.contains_key("src/db.rs"),
            "file path should be normalized"
        );
        assert!(
            !file_fp.contains_key("./src/db.rs"),
            "raw path should not be key"
        );

        // File finding counts normalized too
        let counts = model.file_finding_counts.unwrap();
        assert_eq!(counts.get("src/db.rs"), Some(&2));
    }
}
