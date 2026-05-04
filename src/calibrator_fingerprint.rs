//! Stable projection over `CalibratorTraceEntry` for refactor parity testing.
//!
//! PR1 of the Calibrator Precision & Cleanup plan refactors `calibrator.rs`
//! (extracts `calibrate_core_decision()` and `trace_entry()` factory). To
//! prove behavioral parity post-refactor, we snapshot pre-refactor traces
//! through this projection — capturing every meaningful decision the
//! calibrator makes while excluding fields that drift legitimately across
//! runs (timestamps, ULIDs, raw precedent text, embedding-similarity scores
//! beyond 4 decimal places).
//!
//! Weights are stringified with `{:.4}` so f64 round-tripping through serde
//! is byte-stable and so NaN / Inf can't make `PartialEq` lie.
//!
//! See `tests/calibrator_pre_refactor_snapshot.rs` for the scenario matrix
//! that drives `tests/fixtures/calibrator_pre_refactor_fingerprints.json`.

use serde::{Deserialize, Serialize};

use crate::calibrator_trace::{CalibratorTraceEntry, SeverityChangeReason};
use crate::finding::{CalibratorAction, Severity};

/// Which calibrate path produced a trace.
///
/// Both paths must agree on every meaningful decision; the parity test
/// compares fingerprints across paths to catch path-divergence bugs (the
/// kind that motivated the existing
/// `calibrate_paths_agree_on_out_of_scope_exclusion` regression test).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FromIndexPath {
    /// `calibrate()` — in-memory feedback slice, Jaccard similarity.
    Calibrate,
    /// `calibrate_with_index()` — `FeedbackIndex` lookup. Tests use
    /// `FeedbackIndex::build_jaccard_only` to keep the embedding model
    /// out of the deterministic loop.
    CalibrateWithIndex,
}

/// Stable, comparison-friendly projection of one `CalibratorTraceEntry`.
///
/// Field choices:
/// - `scenario` + `finding_index` form a stable identity for the row in the
///   pretty-printed fixture so a diff points at the exact failing case.
/// - `tp_weight` / `fp_weight` are formatted to 4 decimal places. `wontfix`
///   is included because it's still emitted (even though it no longer
///   contributes to suppression) — losing it would mask a regression where
///   wontfix accidentally re-enters the math.
/// - `matched_precedent_count` replaces the full `Vec<PrecedentTrace>`:
///   precedent text drifts with feedback corpus state, but a count change
///   is still a behavioral change worth catching.
/// - `severity_change_reason` is included because Track B (#189) is the
///   exact behavioral surface PR1 must not break.
/// - Path-divergence bugs are first-class: `from_index_path` lets the
///   parity assertion separate "calibrate() drifted" from "embedding path
///   drifted".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceFingerprint {
    pub scenario: String,
    pub finding_index: usize,
    pub from_index_path: FromIndexPath,

    pub finding_title: String,
    pub finding_category: String,

    pub input_severity: Severity,
    pub output_severity: Severity,
    pub action: Option<CalibratorAction>,
    pub severity_change_reason: Option<SeverityChangeReason>,

    /// `format!("{:.4}", tp_weight)` — string avoids f64 NaN / Inf serde edge
    /// cases and locks the resolution at 4dp so embedding-score jitter past
    /// the 5th decimal doesn't cause spurious diffs.
    pub tp_weight: String,
    pub fp_weight: String,
    pub wontfix_weight: String,
    pub full_suppress_weight: String,
    pub soft_fp_weight: String,

    pub matched_precedent_count: usize,
}

/// Project a single trace entry into a fingerprint.
#[must_use]
pub fn fingerprint_from(
    scenario: &str,
    finding_index: usize,
    entry: &CalibratorTraceEntry,
    from_index_path: FromIndexPath,
) -> TraceFingerprint {
    TraceFingerprint {
        scenario: scenario.to_string(),
        finding_index,
        from_index_path,
        finding_title: entry.finding_title.clone(),
        finding_category: entry.finding_category.clone(),
        input_severity: entry.input_severity.clone(),
        output_severity: entry.output_severity.clone(),
        action: entry.action.clone(),
        severity_change_reason: entry.severity_change_reason,
        tp_weight: format!("{:.4}", entry.tp_weight),
        fp_weight: format!("{:.4}", entry.fp_weight),
        wontfix_weight: format!("{:.4}", entry.wontfix_weight),
        full_suppress_weight: format!("{:.4}", entry.full_suppress_weight),
        soft_fp_weight: format!("{:.4}", entry.soft_fp_weight),
        matched_precedent_count: entry.matched_precedents.len(),
    }
}

/// Project a slice of trace entries (one calibrate call's output) into
/// fingerprints. `finding_index` is the position in the input findings vec —
/// callers must pass the original input length so we can detect the
/// suppress-path (where a finding gets a trace but no output finding).
///
/// In practice the calibrator emits one trace per *input* finding (suppress
/// path emits a trace then `continue`s without pushing to output), so the
/// 1:1 mapping holds.
#[must_use]
pub fn fingerprints_from_traces(
    scenario: &str,
    traces: &[CalibratorTraceEntry],
    from_index_path: FromIndexPath,
) -> Vec<TraceFingerprint> {
    traces
        .iter()
        .enumerate()
        .map(|(i, t)| fingerprint_from(scenario, i, t, from_index_path))
        .collect()
}

/// Sort fingerprints into a deterministic order for fixture serialization.
/// Required because `Vec<TraceFingerprint>` equality is order-sensitive but
/// the calibrator's iteration order over feedback / similar precedents is
/// not part of the contract we want to lock.
pub fn sort_fingerprints(fps: &mut [TraceFingerprint]) {
    fps.sort_by(|a, b| {
        (
            a.scenario.as_str(),
            a.from_index_path,
            a.finding_index,
            a.finding_title.as_str(),
        )
            .cmp(&(
                b.scenario.as_str(),
                b.from_index_path,
                b.finding_index,
                b.finding_title.as_str(),
            ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibrator_trace::PrecedentTrace;
    use crate::feedback::Verdict;

    fn sample_entry() -> CalibratorTraceEntry {
        CalibratorTraceEntry {
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            tp_weight: 1.234_567_8,
            fp_weight: 0.0,
            wontfix_weight: 0.0,
            full_suppress_weight: 0.0,
            soft_fp_weight: 0.0,
            matched_precedents: vec![PrecedentTrace {
                finding_title: "p1".into(),
                verdict: Verdict::Tp,
                similarity: 0.91,
                weight: 1.0,
                provenance: "human".into(),
                file_path: "x.rs".into(),
            }],
            action: Some(CalibratorAction::Confirmed),
            input_severity: Severity::Medium,
            output_severity: Severity::High,
            severity_change_reason: Some(SeverityChangeReason::Boosted),
            file_path: None,
        }
    }

    #[test]
    fn weights_truncated_to_four_decimals() {
        let fp = fingerprint_from("s", 0, &sample_entry(), FromIndexPath::Calibrate);
        assert_eq!(fp.tp_weight, "1.2346");
    }

    #[test]
    fn precedent_count_is_extracted() {
        let fp = fingerprint_from("s", 0, &sample_entry(), FromIndexPath::Calibrate);
        assert_eq!(fp.matched_precedent_count, 1);
    }

    #[test]
    fn fingerprint_roundtrips_through_json() {
        let fp = fingerprint_from("s", 0, &sample_entry(), FromIndexPath::CalibrateWithIndex);
        let json = serde_json::to_string(&fp).unwrap();
        let back: TraceFingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(fp, back);
    }

    #[test]
    fn sort_orders_by_scenario_then_path_then_index() {
        let e = sample_entry();
        let mut fps = vec![
            fingerprint_from("b", 0, &e, FromIndexPath::Calibrate),
            fingerprint_from("a", 1, &e, FromIndexPath::CalibrateWithIndex),
            fingerprint_from("a", 0, &e, FromIndexPath::Calibrate),
            fingerprint_from("a", 0, &e, FromIndexPath::CalibrateWithIndex),
        ];
        sort_fingerprints(&mut fps);
        assert_eq!(fps[0].scenario, "a");
        assert_eq!(fps[0].from_index_path, FromIndexPath::Calibrate);
        assert_eq!(fps[0].finding_index, 0);
        assert_eq!(fps[1].from_index_path, FromIndexPath::CalibrateWithIndex);
        assert_eq!(fps[1].finding_index, 0);
        assert_eq!(fps[2].finding_index, 1);
        assert_eq!(fps[3].scenario, "b");
    }
}
