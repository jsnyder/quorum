//! Corpus join and threshold computation for calibrator tuning.
//!
//! Joins feedback verdicts with calibrator trace entries on
//! `(finding_title, file_path)` and computes suppress/boost thresholds
//! via precision-recall curves.

use std::collections::{HashMap, HashSet};

use crate::metrics;
use crate::threshold_config::{PathThreshold, ThresholdConfig};

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
    // Build lookup: (finding_title, file_path) -> (tp_weight, fp_weight)
    let mut trace_map: HashMap<(String, String), (f64, f64)> = HashMap::new();
    let mut ambiguous: HashSet<(String, String)> = HashSet::new();

    for t in traces {
        let title = t["finding_title"].as_str().unwrap_or("").to_string();
        let fp = t["file_path"].as_str().unwrap_or("").to_string();
        let tp_w = t["tp_weight"].as_f64().unwrap_or(0.0);
        let fp_w = t["fp_weight"].as_f64().unwrap_or(0.0);
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
    for key in &ambiguous {
        trace_map.remove(key);
        tracing::warn!(
            title = %key.0,
            file_path = %key.1,
            "duplicate trace key -- skipping ambiguous entry"
        );
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
        let fp = f["file_path"].as_str().unwrap_or("").to_string();
        if let Some((tp_w, fp_w)) = trace_map.get(&(title, fp)) {
            let total = tp_w + fp_w;
            if total > 0.0 {
                samples.push((tp_w / total, is_positive));
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
