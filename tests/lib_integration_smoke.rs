//! Smoke test proving the bin/lib hybrid split (PR0) actually exposes
//! internal types to integration tests.
//!
//! Without this test, we'd ship the lib half without proof it solves the
//! problem PR1's test plan needs solved (importing internal types like
//! `CalibratorConfig`, `Finding`, `CalibratorTraceEntry` from `tests/*.rs`).
//!
//! Keep minimal: one test, no logic — just type construction to prove the
//! `pub mod` exports compile and link from an integration-test build.

use quorum::calibrator::{calibrate, CalibratorConfig};
use quorum::calibrator_trace::CalibratorTraceEntry;
use quorum::finding::{Finding, Severity, Source};

#[test]
fn lib_exports_are_reachable_from_integration_tests() {
    // CalibratorConfig must be constructible (and used below).
    let cfg = CalibratorConfig::default();
    let _ = &cfg;

    // Finding must be constructible.
    let finding = Finding {
        title: "smoke".into(),
        description: String::new(),
        severity: Severity::Low,
        category: "smoke".into(),
        source: Source::LocalAst,
        line_start: 1,
        line_end: 1,
        evidence: vec![],
        calibrator_action: None,
        similar_precedent: vec![],
        canonical_pattern: None,
        suggested_fix: None,
        based_on_excerpt: None,
    };

    // calibrate(...) must be callable with no precedents — exercises the
    // public function signature integration tests will rely on.
    let result = calibrate(vec![finding], &[], &cfg);
    assert_eq!(result.findings.len(), 1);

    // CalibratorTraceEntry must be reachable as a public type so snapshot
    // tests can deserialize traces.jsonl into it.
    let _: Option<CalibratorTraceEntry> = None;
}
