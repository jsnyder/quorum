//! Precision-recall curve computation for calibrator threshold tuning.

/// Compute a precision-recall curve from labeled scores.
///
/// Input: `(score, is_positive)` pairs. Higher scores should indicate
/// more likely positive (TP-like). Returns `(precision, recall, threshold)`
/// triples sorted by descending threshold.
pub fn precision_recall_curve(samples: &[(f64, bool)]) -> Vec<(f64, f64, f64)> {
    if samples.is_empty() {
        return vec![];
    }

    let mut sorted: Vec<(f64, bool)> = samples
        .iter()
        .filter(|(s, _)| s.is_finite())
        .copied()
        .collect();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let total_positives = sorted.iter().filter(|(_, p)| *p).count() as f64;
    if total_positives == 0.0 {
        return vec![];
    }

    let mut curve = Vec::new();
    let mut tp: f64 = 0.0;
    let mut fp: f64 = 0.0;
    let mut i = 0;

    while i < sorted.len() {
        let threshold = sorted[i].0;
        // Consume all samples at this score (handle ties)
        while i < sorted.len() && (sorted[i].0 - threshold).abs() < 1e-12 {
            if sorted[i].1 {
                tp += 1.0;
            } else {
                fp += 1.0;
            }
            i += 1;
        }
        let precision = tp / (tp + fp);
        let recall = tp / total_positives;
        curve.push((precision, recall, threshold));
    }

    curve
}

/// Find the lowest threshold that achieves at least `min_precision`.
/// Returns `None` if no threshold meets the target or the curve is empty.
///
/// The curve is sorted by descending threshold, so the *last* qualifying
/// entry has the lowest threshold (and therefore highest recall).
pub fn threshold_at_precision(curve: &[(f64, f64, f64)], min_precision: f64) -> Option<f64> {
    // Curve is sorted by descending threshold. rfind traverses from the end,
    // returning the last qualifying entry (lowest threshold, highest recall).
    curve
        .iter()
        .rfind(|(p, _, _)| *p >= min_precision)
        .map(|(_, _, t)| *t)
}

/// Find the threshold that maximizes F1 score.
/// Returns `None` if the curve is empty.
pub fn f1_optimal_threshold(curve: &[(f64, f64, f64)]) -> Option<f64> {
    curve
        .iter()
        .filter(|(p, r, _)| *p + *r > 0.0)
        .max_by(|(p1, r1, _), (p2, r2, _)| {
            let f1_a = 2.0 * p1 * r1 / (p1 + r1);
            let f1_b = 2.0 * p2 * r2 / (p2 + r2);
            f1_a.partial_cmp(&f1_b).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, _, t)| *t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_curve_trivial_four_samples() {
        // Scores: 0.9(TP), 0.7(FP), 0.5(TP), 0.3(FP)
        let samples = vec![
            (0.9, true),
            (0.7, false),
            (0.5, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        // At threshold 0.9: TP=1, FP=0, FN=1 -> P=1.0, R=0.5
        // At threshold 0.7: TP=1, FP=1, FN=1 -> P=0.5, R=0.5
        // At threshold 0.5: TP=2, FP=1, FN=0 -> P=0.667, R=1.0
        // At threshold 0.3: TP=2, FP=2, FN=0 -> P=0.5, R=1.0
        assert_eq!(curve.len(), 4);
        let (p, r, t) = curve[0];
        assert!((p - 1.0).abs() < 1e-9);
        assert!((r - 0.5).abs() < 1e-9);
        assert!((t - 0.9).abs() < 1e-9);
    }

    #[test]
    fn pr_curve_tied_scores_produces_one_point_per_distinct_score() {
        let samples = vec![
            (0.8, true),
            (0.8, false),
            (0.5, true),
        ];
        let curve = precision_recall_curve(&samples);
        assert_eq!(curve.len(), 2, "tied scores should collapse to one point");
    }

    #[test]
    fn pr_curve_empty_input() {
        let curve = precision_recall_curve(&[]);
        assert!(curve.is_empty());
    }

    #[test]
    fn pr_curve_all_positive() {
        let samples = vec![(0.9, true), (0.5, true)];
        let curve = precision_recall_curve(&samples);
        // Every threshold yields precision=1.0
        for (p, _, _) in &curve {
            assert!((p - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn pr_curve_all_negative_returns_empty() {
        let samples = vec![(0.9, false), (0.5, false)];
        let curve = precision_recall_curve(&samples);
        assert!(curve.is_empty(), "no positives -> empty curve");
    }

    #[test]
    fn pr_curve_filters_nan_scores() {
        let samples = vec![
            (f64::NAN, true),
            (0.9, true),
            (0.5, false),
        ];
        let curve = precision_recall_curve(&samples);
        // NaN entry should be filtered; remaining 2 samples yield curve
        assert!(!curve.is_empty());
        for (_, _, t) in &curve {
            assert!(!t.is_nan(), "no NaN thresholds in output");
        }
    }

    #[test]
    fn threshold_at_precision_finds_lowest_meeting_target() {
        let samples = vec![
            (0.9, true),
            (0.7, false),
            (0.5, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        // Only threshold 0.9 achieves P>=0.95
        let t = threshold_at_precision(&curve, 0.95);
        assert_eq!(t, Some(0.9));
    }

    #[test]
    fn threshold_at_precision_returns_none_when_unachievable() {
        // All FP -- no threshold achieves any precision on positives
        let samples = vec![(0.9, false), (0.5, false)];
        let curve = precision_recall_curve(&samples);
        let t = threshold_at_precision(&curve, 0.5);
        assert_eq!(t, None);
    }

    #[test]
    fn threshold_at_precision_picks_lowest_for_max_recall() {
        // Multiple thresholds achieve target -- pick lowest (highest recall)
        let samples = vec![
            (0.9, true),
            (0.8, true),
            (0.7, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        // At 0.9: P=1.0, at 0.8: P=1.0, at 0.7: P=1.0, at 0.3: P=0.75
        let t = threshold_at_precision(&curve, 0.95);
        assert!((t.unwrap() - 0.7).abs() < 1e-9, "should pick lowest threshold achieving P>=0.95");
    }

    #[test]
    fn f1_optimal_threshold_picks_best_f1() {
        let samples = vec![
            (0.9, true),
            (0.7, false),
            (0.5, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        let t = f1_optimal_threshold(&curve);
        // At 0.9: F1=2*(1.0*0.5)/(1.0+0.5)=0.667
        // At 0.5: F1=2*(0.667*1.0)/(0.667+1.0)=0.800
        assert!((t.unwrap() - 0.5).abs() < 1e-9, "threshold 0.5 has best F1");
    }
}
