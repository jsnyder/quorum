//! Pure math helpers for the stats dashboard.
//!
//! Kept narrowly scoped: Wilson confidence intervals for proportions, used
//! by the headline precision trend to surface uncertainty bands. No I/O,
//! no allocations beyond return values, no logging. If a future change
//! needs more, prefer adding a new helper here over expanding an existing one.

/// Wilson score interval for a binomial proportion.
///
/// Returns `(lower, upper)` bounds at the given confidence level, both in
/// `[0.0, 1.0]`. Wilson is preferred over the normal-approximation (Wald)
/// interval for small `n` and proportions near 0 or 1, where Wald can
/// produce bounds outside the unit interval or zero-width "intervals" at
/// the extremes.
///
/// `total == 0` returns `(0.0, 1.0)` — an uninformative band, which is
/// the right answer for "we have no data" rather than panicking or
/// returning NaN.
///
/// Reference: Wilson (1927), "Probable Inference, the Law of Succession,
/// and Statistical Inference."
pub fn wilson_interval(successes: usize, total: usize, confidence: f64) -> (f64, f64) {
    // assert! (not debug_assert!) so that release builds also reject
    // impossible inputs. Silent clamping to p=1 would mask upstream data
    // corruption — off-by-one in aggregation, malformed feedback rows,
    // hand-edited JSONL — and produce plausible-looking intervals.
    // Must be checked BEFORE the total==0 early return so (successes=k>0,
    // total=0) doesn't slip through as "no data".
    assert!(
        successes <= total,
        "wilson_interval: successes ({}) must not exceed total ({})",
        successes,
        total
    );
    if total == 0 {
        return (0.0, 1.0);
    }
    let n = total as f64;
    let p = (successes as f64 / n).clamp(0.0, 1.0);
    let z = z_score(confidence);
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let half_width = (z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt()) / denom;
    let lo = (center - half_width).max(0.0);
    let hi = (center + half_width).min(1.0);
    (lo, hi)
}

/// Two-sided z-score for a given confidence level.
///
/// Hard-coded for the values we actually use — avoids pulling in a stats
/// crate for one function. Unrecognized confidence levels fall back to
/// 95% (the dashboard's documented default).
fn z_score(confidence: f64) -> f64 {
    match (confidence * 100.0).round() as u32 {
        99 => 2.576,
        95 => 1.96,
        90 => 1.645,
        _ => 1.96,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wilson_interval_with_zero_n_returns_unit_band() {
        // No data: report the entire unit interval. Don't panic, don't NaN.
        assert_eq!(wilson_interval(0, 0, 0.95), (0.0, 1.0));
    }

    #[test]
    fn wilson_interval_n_60_p_0_5_matches_published_reference() {
        // Numerical pin against a known textbook value: Wilson at n=60,
        // p=0.5, z=1.96 yields approximately (0.376, 0.624). Catches
        // sign errors and wrong z constants without re-deriving the
        // formula in test names.
        let (lo, hi) = wilson_interval(30, 60, 0.95);
        assert!((lo - 0.376).abs() < 0.005, "lower {} not near 0.376", lo);
        assert!((hi - 0.624).abs() < 0.005, "upper {} not near 0.624", hi);
    }

    #[test]
    fn wilson_interval_confidence_unknown_falls_back_to_95() {
        // Locks the documented fallback in z_score: any unrecognized
        // confidence (e.g. 0.42) uses the 95% z constant.
        let unknown = wilson_interval(30, 60, 0.42);
        let ninetyfive = wilson_interval(30, 60, 0.95);
        assert_eq!(unknown, ninetyfive);
    }

    #[test]
    fn wilson_interval_p_zero_at_small_n_lower_bound_is_zero() {
        // Edge case: 0 successes out of 5. Wald would produce a degenerate
        // (0, 0) "interval"; Wilson must give a proper upper bound > 0.
        let (lo, hi) = wilson_interval(0, 5, 0.95);
        assert_eq!(lo, 0.0, "lower bound clamps to 0");
        assert!(hi > 0.0 && hi < 1.0, "upper bound informative: {}", hi);
    }

    #[test]
    #[should_panic(expected = "successes")]
    fn wilson_interval_panics_when_total_is_zero_but_successes_is_positive() {
        // The "no data" early-return for total==0 must not also short-
        // circuit the successes<=total contract: (successes=7, total=0)
        // is impossible input, not "no data". Without the guard ahead of
        // the early return it slips through and silently returns (0,1).
        let _ = wilson_interval(7, 0, 0.95);
    }

    #[test]
    #[should_panic(expected = "successes")]
    fn wilson_interval_panics_when_successes_exceeds_total() {
        // Documents the contract: successes must not exceed total.
        // Production guard is `assert!` (not `debug_assert!`) so the
        // failure is visible in release builds too — silently clamping
        // to p=1 would mask upstream data corruption (off-by-one in
        // aggregation, malformed feedback rows, etc.) and produce
        // plausible-looking intervals on broken inputs.
        let _ = wilson_interval(7, 5, 0.95);
    }

    #[test]
    fn wilson_interval_p_one_at_small_n_upper_bound_is_one() {
        // Mirror of the above: 5/5 successes. Lower should be < 1, upper
        // clamps to 1.
        let (lo, hi) = wilson_interval(5, 5, 0.95);
        assert_eq!(hi, 1.0, "upper bound clamps to 1");
        assert!(lo > 0.0 && lo < 1.0, "lower bound informative: {}", lo);
    }
}
