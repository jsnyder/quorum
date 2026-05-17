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

/// Compute the area under the precision-recall curve (trapezoidal).
///
/// Prepends the conventional (P=1.0, R=0.0) baseline so the area from
/// zero recall to the first observed recall is captured.
pub fn pr_auc(samples: &[(f64, bool)]) -> f64 {
    let curve = precision_recall_curve(samples);
    if curve.is_empty() {
        return 0.0;
    }
    let mut auc = 0.0;
    let mut prev_r = 0.0;
    let mut prev_p = 1.0;
    for &(p, r, _) in &curve {
        auc += (r - prev_r) * (prev_p + p) / 2.0;
        prev_r = r;
        prev_p = p;
    }
    auc
}

/// Stepwise Average Precision (no interpolation).
///
/// Sorts by score descending, sums P(k) at each rank where recall increases,
/// divides by total positives. Higher score = more likely positive.
///
/// Returns 0.0 for empty input or when there are no positives.
pub fn average_precision_stepwise(samples: &[(f64, bool)]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    let mut sorted: Vec<(f64, bool)> = samples
        .iter()
        .filter(|(s, _)| s.is_finite())
        .copied()
        .collect();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let total_positives = sorted.iter().filter(|(_, p)| *p).count();
    if total_positives == 0 {
        return 0.0;
    }

    let mut tp: f64 = 0.0;
    let mut fp: f64 = 0.0;
    let mut sum_precision = 0.0;

    for &(_, is_positive) in &sorted {
        if is_positive {
            tp += 1.0;
            let precision = tp / (tp + fp);
            sum_precision += precision;
        } else {
            fp += 1.0;
        }
    }

    sum_precision / total_positives as f64
}

/// Compute FP recall at a given TP recall constraint.
///
/// Input: `(score, is_fp)` pairs where higher score = more likely false positive.
/// Sweeps threshold from high to low: FPs caught increase FP recall,
/// non-FPs (TPs) caught count as suppressions. Stops when TP suppression
/// exceeds the budget `(1 - min_tp_recall) * total_tp`.
///
/// Returns `fp_caught / total_fp`. Returns 0.0 for edge cases: empty input,
/// no FPs, no TPs, or all tied scores.
pub fn fp_recall_at_tp_recall(predictions: &[(f64, bool)], min_tp_recall: f64) -> f64 {
    if predictions.is_empty() {
        return 0.0;
    }

    let mut sorted: Vec<(f64, bool)> = predictions
        .iter()
        .filter(|(s, _)| s.is_finite())
        .copied()
        .collect();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let total_fp = sorted.iter().filter(|(_, is_fp)| *is_fp).count();
    let total_non_fp = sorted.iter().filter(|(_, is_fp)| !*is_fp).count();

    if total_fp == 0 || total_non_fp == 0 {
        return 0.0;
    }

    // If all scores are tied, we can't make any distinction
    if sorted.first().map(|f| f.0) == sorted.last().map(|l| l.0) {
        return 0.0;
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let max_tp_suppressed = ((1.0 - min_tp_recall) * total_non_fp as f64).floor() as usize;
    let mut fp_caught: usize = 0;
    let mut tp_suppressed: usize = 0;

    for &(_, is_fp) in &sorted {
        if is_fp {
            fp_caught += 1;
        } else {
            if tp_suppressed >= max_tp_suppressed {
                // This TP would push us over budget, stop
                break;
            }
            tp_suppressed += 1;
        }
    }

    fp_caught as f64 / total_fp as f64
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
        let samples = vec![(0.9, true), (0.7, false), (0.5, true), (0.3, false)];
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
        let samples = vec![(0.8, true), (0.8, false), (0.5, true)];
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
        let samples = vec![(f64::NAN, true), (0.9, true), (0.5, false)];
        let curve = precision_recall_curve(&samples);
        // NaN entry should be filtered; remaining 2 samples yield curve
        assert!(!curve.is_empty());
        for (_, _, t) in &curve {
            assert!(!t.is_nan(), "no NaN thresholds in output");
        }
    }

    #[test]
    fn threshold_at_precision_finds_lowest_meeting_target() {
        let samples = vec![(0.9, true), (0.7, false), (0.5, true), (0.3, false)];
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
        let samples = vec![(0.9, true), (0.8, true), (0.7, true), (0.3, false)];
        let curve = precision_recall_curve(&samples);
        // At 0.9: P=1.0, at 0.8: P=1.0, at 0.7: P=1.0, at 0.3: P=0.75
        let t = threshold_at_precision(&curve, 0.95);
        assert!(
            (t.unwrap() - 0.7).abs() < 1e-9,
            "should pick lowest threshold achieving P>=0.95"
        );
    }

    #[test]
    fn pr_auc_basic() {
        let samples = vec![(0.9, true), (0.7, false), (0.5, true), (0.3, false)];
        let auc = pr_auc(&samples);
        assert!(
            auc > 0.0 && auc <= 1.0,
            "PR-AUC should be in (0,1], got {auc}"
        );
    }

    #[test]
    fn pr_auc_perfect_separation() {
        let samples = vec![(0.9, true), (0.8, true), (0.2, false), (0.1, false)];
        let auc = pr_auc(&samples);
        assert!(
            auc > 0.9,
            "perfect separation should yield high PR-AUC, got {auc}"
        );
    }

    #[test]
    fn pr_auc_empty_returns_zero() {
        assert!((pr_auc(&[]) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn f1_optimal_threshold_picks_best_f1() {
        let samples = vec![(0.9, true), (0.7, false), (0.5, true), (0.3, false)];
        let curve = precision_recall_curve(&samples);
        let t = f1_optimal_threshold(&curve);
        // At 0.9: F1=2*(1.0*0.5)/(1.0+0.5)=0.667
        // At 0.5: F1=2*(0.667*1.0)/(0.667+1.0)=0.800
        assert!((t.unwrap() - 0.5).abs() < 1e-9, "threshold 0.5 has best F1");
    }

    // --- Tests for average_precision_stepwise ---

    #[test]
    fn ap_stepwise_basic_case() {
        // Scores: 0.9(TP), 0.8(FP), 0.7(TP), 0.6(FP), 0.5(TP)
        // Rank 1: TP -> P=1/1=1.0, recall increases -> sum += 1.0
        // Rank 2: FP -> no recall increase
        // Rank 3: TP -> P=2/3=0.667, recall increases -> sum += 0.667
        // Rank 4: FP -> no recall increase
        // Rank 5: TP -> P=3/5=0.6, recall increases -> sum += 0.6
        // AP = (1.0 + 2/3 + 3/5) / 3 = (1.0 + 0.6667 + 0.6) / 3 = 2.2667/3 = 0.7556
        let samples = vec![
            (0.9, true),
            (0.8, false),
            (0.7, true),
            (0.6, false),
            (0.5, true),
        ];
        let ap = average_precision_stepwise(&samples);
        let expected = (1.0 + 2.0 / 3.0 + 3.0 / 5.0) / 3.0;
        assert!(
            (ap - expected).abs() < 1e-9,
            "expected {expected}, got {ap}"
        );
    }

    #[test]
    fn ap_stepwise_perfect_separation() {
        // All positives ranked above all negatives -> AP = 1.0
        let samples = vec![
            (0.9, true),
            (0.8, true),
            (0.7, true),
            (0.3, false),
            (0.2, false),
        ];
        let ap = average_precision_stepwise(&samples);
        assert!(
            (ap - 1.0).abs() < 1e-9,
            "perfect separation should give AP=1.0, got {ap}"
        );
    }

    #[test]
    fn ap_stepwise_worst_case() {
        // All negatives ranked first, then all positives
        // Scores: 0.9(FP), 0.8(FP), 0.7(TP)
        // Rank 1: FP -> no recall increase
        // Rank 2: FP -> no recall increase
        // Rank 3: TP -> P=1/3, recall increases -> sum += 1/3
        // AP = (1/3) / 1 = 1/3
        let samples = vec![(0.9, false), (0.8, false), (0.7, true)];
        let ap = average_precision_stepwise(&samples);
        assert!(
            (ap - 1.0 / 3.0).abs() < 1e-9,
            "worst case should give AP=1/3, got {ap}"
        );
    }

    #[test]
    fn ap_stepwise_empty() {
        let ap = average_precision_stepwise(&[]);
        assert!((ap - 0.0).abs() < 1e-9);
    }

    #[test]
    fn ap_stepwise_no_positives() {
        let samples = vec![(0.9, false), (0.5, false)];
        let ap = average_precision_stepwise(&samples);
        assert!((ap - 0.0).abs() < 1e-9);
    }

    // --- Tests for fp_recall_at_tp_recall ---

    #[test]
    fn fp_recall_perfect_separation() {
        // FPs scored higher than TPs -> at min_tp_recall=0.95,
        // we can catch all FPs without suppressing any TPs
        let predictions = vec![
            (0.9, true),  // is_fp=true
            (0.8, true),  // is_fp=true
            (0.3, false), // is_fp=false (i.e., TP)
            (0.2, false), // is_fp=false (i.e., TP)
        ];
        let recall = fp_recall_at_tp_recall(&predictions, 0.95);
        assert!(
            (recall - 1.0).abs() < 1e-9,
            "perfect FP separation should give recall=1.0, got {recall}"
        );
    }

    #[test]
    fn fp_recall_no_separation() {
        // All items have the same score -> can't distinguish, return 0.0
        let predictions = vec![(0.5, true), (0.5, false), (0.5, true), (0.5, false)];
        let recall = fp_recall_at_tp_recall(&predictions, 0.95);
        assert!(
            (recall - 0.0).abs() < 1e-9,
            "no separation should give recall=0.0, got {recall}"
        );
    }

    #[test]
    fn fp_recall_empty() {
        let recall = fp_recall_at_tp_recall(&[], 0.95);
        assert!((recall - 0.0).abs() < 1e-9);
    }

    #[test]
    fn fp_recall_no_fps() {
        let predictions = vec![(0.9, false), (0.5, false)];
        let recall = fp_recall_at_tp_recall(&predictions, 0.95);
        assert!((recall - 0.0).abs() < 1e-9);
    }

    #[test]
    fn fp_recall_no_tps() {
        let predictions = vec![(0.9, true), (0.5, true)];
        let recall = fp_recall_at_tp_recall(&predictions, 0.95);
        assert!((recall - 0.0).abs() < 1e-9);
    }
}
