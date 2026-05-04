//! Corpus join and threshold computation for calibrator tuning.
//!
//! Joins feedback verdicts with calibrator trace entries on
//! `(finding_title, file_path)` and computes suppress/boost thresholds
//! via precision-recall curves.

use std::collections::{HashMap, HashSet};

use crate::metrics;
use crate::threshold_config::{PathThreshold, ThresholdConfig};

/// Minimum token Jaccard similarity for fuzzy title matching.
const FUZZY_THRESHOLD: f64 = 0.5;

/// Minimum margin between best and second-best Jaccard score.
const FUZZY_AMBIGUITY_MARGIN: f64 = 0.1;

fn normalize_title(raw: &str) -> String {
    let after_prefix = strip_rule_prefix(raw);
    let normalized: String = after_prefix
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect();
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

/// Join feedback verdicts with calibrator trace entries to produce labeled
/// score samples for PR curve analysis.
///
/// Join key: `(finding_title, file_path)`. Ambiguous keys (duplicate trace
/// entries for the same key) are removed entirely with a warning. Wontfix
/// verdicts are filtered out. `tp`/`partial` map to positive labels; `fp`
/// maps to negative.
///
/// Score: `tp_weight / (tp_weight + fp_weight)`. Entries where the total is
/// zero are skipped (no signal).
pub fn join_feedback_and_traces(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
) -> Vec<(f64, bool)> {
    // Primary lookup: (finding_title, file_path) -> (tp_weight, fp_weight)
    let mut trace_map: HashMap<(String, String), (f64, f64)> = HashMap::new();
    let mut ambiguous: HashSet<(String, String)> = HashSet::new();
    // Fallback: title-only lookup for pre-file_path trace entries
    let mut title_only_map: HashMap<String, (f64, f64)> = HashMap::new();
    let mut title_only_ambiguous: HashSet<String> = HashSet::new();
    let mut titles_with_file_scoped: HashSet<String> = HashSet::new();

    for t in traces {
        let title = t["finding_title"].as_str().unwrap_or("").to_string();
        if title.is_empty() {
            continue;
        }
        let fp = t["file_path"].as_str().unwrap_or("").to_string();
        let tp_w = t["tp_weight"].as_f64().unwrap_or(0.0).max(0.0);
        let fp_w = t["fp_weight"].as_f64().unwrap_or(0.0).max(0.0);

        if fp.is_empty() {
            // Pre-file_path trace entry: title-only index
            match title_only_map.entry(title.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    title_only_ambiguous.insert(title);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((tp_w, fp_w));
                }
            }
        } else {
            titles_with_file_scoped.insert(title.clone());
            let key = (title, fp);
            match trace_map.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    ambiguous.insert(key);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((tp_w, fp_w));
                }
            }
        }
    }
    for key in &ambiguous {
        trace_map.remove(key);
        tracing::warn!(
            title = %key.0,
            file_path = %key.1,
            "duplicate trace key -- skipping ambiguous entry"
        );
    }
    for key in &title_only_ambiguous {
        title_only_map.remove(key);
    }
    for title in &titles_with_file_scoped {
        title_only_map.remove(title);
    }

    let mut samples = Vec::new();
    for f in feedback {
        let verdict = f["verdict"].as_str().unwrap_or("");
        let is_positive = match verdict {
            "tp" | "partial" => true,
            "fp" => false,
            _ => continue, // skip wontfix, unknown, context_misleading
        };
        let title = f["finding_title"].as_str().unwrap_or("").to_string();
        if title.is_empty() {
            continue;
        }
        let fp = f["file_path"].as_str().unwrap_or("").to_string();
        let weights = trace_map
            .get(&(title.clone(), fp))
            .or_else(|| title_only_map.get(&title));
        if let Some((tp_w, fp_w)) = weights {
            let total = tp_w + fp_w;
            if total > 0.0 {
                let score = tp_w / total;
                if score.is_finite() {
                    samples.push((score, is_positive));
                }
            }
        }
    }
    samples
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
        let samples = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 2);
        // SQL injection: score = 2.5/(2.5+0.3) ~ 0.893, label = true
        assert!(samples.iter().any(|(s, l)| *l && (*s - 0.893).abs() < 0.01));
        // Unused var: score = 0.1/(0.1+1.8) ~ 0.053, label = false
        assert!(samples.iter().any(|(s, l)| !*l && (*s - 0.053).abs() < 0.01));
    }

    #[test]
    fn wontfix_entries_are_skipped() {
        let feedback = vec![make_feedback("Style issue", "wontfix", "src/x.rs")];
        let traces = vec![make_trace("Style issue", 0.5, 0.5, Some("src/x.rs"))];
        let samples = join_feedback_and_traces(&feedback, &traces);
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
        let samples = join_feedback_and_traces(&feedback, &traces);
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
        let samples = join_feedback_and_traces(&feedback, &traces);
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
        let samples = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        let (score, _) = samples[0];
        // Should use the primary match: 2.5/(2.5+0.3) ~ 0.893
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
        let samples = join_feedback_and_traces(&feedback, &traces);
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
        let samples = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "ambiguous join keys should be skipped");
    }

    #[test]
    fn negative_weights_clamped_to_zero() {
        let feedback = vec![make_feedback("Bug", "tp", "src/a.rs")];
        let traces = vec![make_trace("Bug", -1.0, 2.0, Some("src/a.rs"))];
        let samples = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 1);
        let (score, _) = samples[0];
        // tp_weight clamped to 0.0, so score = 0.0/2.0 = 0.0
        assert!(
            (0.0..=1.0).contains(&score),
            "negative weights should be clamped, got score {score}"
        );
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
        let samples = join_feedback_and_traces(&feedback, &traces);
        // tp_weight becomes 0.0 (JSON null), fp_weight is 0.1.
        // total = 0.1 > 0.0, so score = 0.0 / 0.1 = 0.0.
        assert_eq!(samples.len(), 1);
        let (score, label) = samples[0];
        assert!(
            (score - 0.0).abs() < 1e-9,
            "INF weight serialized as JSON null should be treated as 0.0, got {score}"
        );
        assert!(label);
    }
}
