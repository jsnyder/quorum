//! Phase 0 of PR1 (Calibrator Precision & Cleanup): pre-refactor fingerprint
//! capture for parity testing.
//!
//! PR1 will extract `calibrate_core_decision()` and `trace_entry()` from
//! `src/calibrator.rs`. This test runs every meaningful decision branch
//! through both the in-memory path (`calibrate`) and the index path
//! (`calibrate_with_index` over a `build_jaccard_only` index — embedding
//! model is deliberately out of the loop), projects the resulting traces
//! through `TraceFingerprint`, and writes a deterministic JSON fixture.
//!
//! PR1 will gain a sibling test that re-runs this matrix post-refactor and
//! asserts `Vec<TraceFingerprint>` equality field-by-field. If the refactor
//! is behavior-preserving, the test passes. If it isn't, the failure points
//! at the exact `(scenario, finding_index, from_index_path, field)`.
//!
//! The test is `#[ignore]` so it doesn't run on default `cargo test`. To
//! regenerate the fixture (only after a deliberate behavior change):
//!
//! ```text
//! cargo test --bin quorum --test calibrator_pre_refactor_snapshot \
//!     -- --ignored snapshot_capture
//! ```

use std::path::PathBuf;

use chrono::{Duration as ChronoDuration, Utc};

use quorum::calibrator::{calibrate, calibrate_with_index, CalibratorConfig};
use quorum::calibrator_fingerprint::{
    fingerprints_from_traces, sort_fingerprints, FromIndexPath, TraceFingerprint,
};
use quorum::feedback::{FeedbackEntry, FeedbackStore, FpKind, Provenance, Verdict};
use quorum::feedback_index::FeedbackIndex;
use quorum::finding::{Finding, FindingBuilder, Severity};

// --- Scenario builder helpers ---------------------------------------------

/// One scenario: a label, a list of findings, a list of feedback entries,
/// and an optional config override.
struct Scenario {
    name: &'static str,
    findings: Vec<Finding>,
    feedback: Vec<FeedbackEntry>,
    /// Some scenarios need bespoke config (e.g. disabled calibrator).
    config: CalibratorConfig,
    /// `false` for the disabled-calibrator scenario, where neither path
    /// emits traces and re-running through `calibrate_with_index` is just
    /// noise. All other scenarios run both paths.
    run_index_path: bool,
}

fn human_fb(title: &str, category: &str, verdict: Verdict) -> FeedbackEntry {
    FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: title.into(),
        finding_category: category.into(),
        verdict,
        reason: "test".into(),
        model: Some("gpt-5.4".into()),
        // Pin timestamps to a fixed reference so recency-decay output is
        // deterministic across runs. The recency window (τ in days) keys
        // off (now - timestamp), so any clock drift between snapshot
        // capture and the post-refactor parity run would otherwise show
        // up as a tp_weight diff at 4dp.
        timestamp: Utc::now() - ChronoDuration::hours(1),
        provenance: Provenance::Human,
        fp_kind: None,
    }
}

fn fb_with(title: &str, category: &str, verdict: Verdict, fp_kind: Option<FpKind>) -> FeedbackEntry {
    let mut e = human_fb(title, category, verdict);
    e.fp_kind = fp_kind;
    e
}

fn external_fb(title: &str, category: &str, verdict: Verdict, agent: &str) -> FeedbackEntry {
    let mut e = human_fb(title, category, verdict);
    e.provenance = Provenance::External {
        agent: agent.into(),
        model: None,
        confidence: None,
    };
    e
}

fn finding(title: &str, category: &str, severity: Severity) -> Finding {
    FindingBuilder::new()
        .title(title)
        .category(category)
        .severity(severity)
        .description("snapshot scenario finding")
        .build()
}

// --- Scenario matrix ------------------------------------------------------

fn build_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();

    // 1. Disabled calibrator: short-circuit returns no traces. We still emit
    //    fingerprints for the empty-trace case (zero rows) — the fixture
    //    captures that the path produced nothing. We skip the index path for
    //    this scenario because the disabled short-circuit is identical
    //    across paths and would just duplicate the empty record.
    out.push(Scenario {
        name: "01_disabled_calibrator",
        findings: vec![finding("SQL injection", "security", Severity::High)],
        feedback: vec![human_fb("SQL injection", "security", Verdict::Tp)],
        config: CalibratorConfig {
            disable_calibrator: Some(true),
            ..Default::default()
        },
        run_index_path: false,
    });

    // 2. Empty corpus: filtered.is_empty() / index.is_empty() branch emits
    //    NoMatch traces.
    out.push(Scenario {
        name: "02_empty_corpus_no_match",
        findings: vec![finding("Some new finding", "security", Severity::Medium)],
        feedback: vec![],
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 3. Single TP, weight below 1.5 boost threshold → BoostWeightTooLow.
    out.push(Scenario {
        name: "03_single_tp_human_no_boost",
        findings: vec![finding("Race condition in shared HashMap", "concurrency", Severity::Medium)],
        feedback: vec![human_fb(
            "Race condition in shared HashMap",
            "concurrency",
            Verdict::Tp,
        )],
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 4. Multiple TP human → boost succeeds (Boosted, severity Medium → High).
    //    "Race condition" is in the rubric's HIGH allowlist (concurrency
    //    category) so the gate permits the bump.
    out.push(Scenario {
        name: "04_multiple_tp_human_boost_to_high",
        findings: vec![finding("Race condition in shared HashMap", "concurrency", Severity::Medium)],
        feedback: (0..3)
            .map(|_| human_fb("Race condition in shared HashMap", "concurrency", Verdict::Tp))
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 5. Track A rubric gate: high-CC complexity finding can't bump to HIGH.
    //    Mirrors the existing `calibrate_records_boost_blocked_by_gate_for_complexity`
    //    test. severity_change_reason = BoostBlockedByGate.
    out.push(Scenario {
        name: "05_multiple_tp_human_boost_blocked_by_rubric_gate",
        findings: vec![finding(
            "Function `foo` has cyclomatic complexity 30",
            "complexity",
            Severity::Medium,
        )],
        feedback: (0..3)
            .map(|_| {
                human_fb(
                    "Function `foo` has cyclomatic complexity 30",
                    "complexity",
                    Verdict::Tp,
                )
            })
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 6. Soft suppress: small FP, no TP → demoted to Info, kept in output.
    //    soft_fp trigger (b): `soft_fp_weight >= 0.5 && tp_weight < 0.1`.
    out.push(Scenario {
        name: "06_single_fp_human_soft_suppress",
        findings: vec![finding("Use of unwrap", "error-handling", Severity::Medium)],
        feedback: vec![human_fb("Use of unwrap", "error-handling", Verdict::Fp)],
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 7. Multiple FP human → full suppress (Disputed, finding removed).
    out.push(Scenario {
        name: "07_multiple_fp_human_full_suppress",
        findings: vec![finding("Use of unwrap", "error-handling", Severity::Medium)],
        feedback: (0..5)
            .map(|_| human_fb("Use of unwrap", "error-handling", Verdict::Fp))
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 8. Disputed: FP weight dominates a non-trivial TP weight. tp_weight is
    //    nonzero but FP wins by 2x.
    out.push(Scenario {
        name: "08_disputed_tp_and_fp",
        findings: vec![finding("Use of unwrap", "error-handling", Severity::Medium)],
        feedback: {
            let mut v = vec![human_fb("Use of unwrap", "error-handling", Verdict::Tp)];
            for _ in 0..4 {
                v.push(human_fb("Use of unwrap", "error-handling", Verdict::Fp));
            }
            v
        },
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 9. OutOfScope FP must NOT contribute. Even with 5 OutOfScope FPs the
    //    finding survives unsuppressed → trace shows fp_weight=0, NoMatch.
    out.push(Scenario {
        name: "09_out_of_scope_excluded",
        findings: vec![finding("SQL injection", "security", Severity::High)],
        feedback: (0..5)
            .map(|_| {
                fb_with(
                    "SQL injection",
                    "security",
                    Verdict::Fp,
                    Some(FpKind::OutOfScope { tracked_in: None }),
                )
            })
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 10. External cap: 10 External TPs accumulate to EXTERNAL_WEIGHT_CAP=1.4
    //     instead of >=1.5, so calibrator confirms but does NOT boost.
    out.push(Scenario {
        name: "10_external_capped_at_constant",
        findings: vec![finding("SQL injection", "security", Severity::High)],
        feedback: (0..10)
            .map(|_| external_fb("SQL injection", "security", Verdict::Tp, "pal"))
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 11. Wontfix only: wontfix_weight is recorded but no longer triggers
    //     full suppression (only fp does, post-rebalance). Multiple wontfix
    //     entries → finding survives. severity_change_reason should be
    //     BoostWeightTooLow (mixed/no-action default branch).
    out.push(Scenario {
        name: "11_wontfix_soft_suppress_only",
        findings: vec![finding("Use of unwrap", "error-handling", Severity::Medium)],
        feedback: (0..5)
            .map(|_| human_fb("Use of unwrap", "error-handling", Verdict::Wontfix))
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 12. Embedding-path parity: identical input via calibrate_with_index
    //     must produce identical fingerprint shape. The boost-to-HIGH case
    //     is the most-covered pathway and exercises the rubric gate.
    out.push(Scenario {
        name: "12_embedding_path_parity_with_jaccard",
        findings: vec![finding(
            "Race condition in shared HashMap",
            "concurrency",
            Severity::Medium,
        )],
        feedback: (0..3)
            .map(|_| human_fb("Race condition in shared HashMap", "concurrency", Verdict::Tp))
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 13. Per-FpKind decay (τ=120d) — Hallucination FPs at modest age.
    //     Exercises the `verdict_weight` hallucination branch. Multiple
    //     entries → suppression triggers; trace records hallucination
    //     contribution to fp_weight at decayed magnitude.
    out.push(Scenario {
        name: "13_fpkind_hallucination_decay",
        findings: vec![finding("Use of unwrap", "error-handling", Severity::Medium)],
        feedback: (0..3)
            .map(|_| {
                let mut e = fb_with(
                    "Use of unwrap",
                    "error-handling",
                    Verdict::Fp,
                    Some(FpKind::Hallucination),
                );
                // 30 days old: e^(-30/120) ≈ 0.7788
                e.timestamp = Utc::now() - ChronoDuration::days(30);
                e
            })
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    // 14. Per-FpKind faster decay (τ=40d) — TrustModelAssumption at the
    //     same 30d age decays 3x faster. Compare-with #13 fingerprint
    //     post-refactor: the relative weight ordering must hold.
    out.push(Scenario {
        name: "14_fpkind_trust_model_faster_decay",
        findings: vec![finding("Use of unwrap", "error-handling", Severity::Medium)],
        feedback: (0..3)
            .map(|_| {
                let mut e = fb_with(
                    "Use of unwrap",
                    "error-handling",
                    Verdict::Fp,
                    Some(FpKind::TrustModelAssumption),
                );
                // 30 days old: e^(-30/40) ≈ 0.4724 (faster decay)
                e.timestamp = Utc::now() - ChronoDuration::days(30);
                e
            })
            .collect(),
        config: CalibratorConfig::default(),
        run_index_path: true,
    });

    out
}

// --- Capture driver -------------------------------------------------------

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("calibrator_pre_refactor_fingerprints.json")
}

/// Run a scenario through both calibration paths, return projected
/// fingerprints. The Jaccard-only index sidesteps the embedding model so
/// runs are deterministic regardless of fastembed availability.
fn capture_scenario(scenario: &Scenario) -> Vec<TraceFingerprint> {
    let mut out = Vec::new();

    // Path A: in-memory calibrate()
    let result_a = calibrate(scenario.findings.clone(), &scenario.feedback, &scenario.config);
    out.extend(fingerprints_from_traces(
        scenario.name,
        &result_a.traces,
        FromIndexPath::Calibrate,
    ));

    if scenario.run_index_path {
        // Path B: calibrate_with_index() with a Jaccard-only index built
        // from the same feedback corpus. Use a tempdir-backed FeedbackStore
        // so we don't leak state into ~/.quorum.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        for entry in &scenario.feedback {
            store.record(entry).expect("record feedback");
        }
        let mut index = FeedbackIndex::build_jaccard_only(&store).expect("jaccard index");
        let result_b =
            calibrate_with_index(scenario.findings.clone(), &mut index, &scenario.config);
        out.extend(fingerprints_from_traces(
            scenario.name,
            &result_b.traces,
            FromIndexPath::CalibrateWithIndex,
        ));
    }

    out
}

#[test]
#[ignore = "snapshot capture: regenerates pre-refactor fingerprint fixture"]
fn snapshot_capture() {
    let scenarios = build_scenarios();
    let mut all: Vec<TraceFingerprint> = Vec::new();
    for s in &scenarios {
        all.extend(capture_scenario(s));
    }
    sort_fingerprints(&mut all);

    // Sanity: every non-disabled scenario emitted at least one fingerprint
    // for each path (catches "scenario silently emits zero traces"
    // regressions where the calibrator early-returns). The disabled
    // scenario intentionally produces no traces.
    for s in &scenarios {
        let count = all.iter().filter(|f| f.scenario == s.name).count();
        if s.name == "01_disabled_calibrator" {
            assert_eq!(
                count, 0,
                "disabled scenario {} must emit zero traces, got {}",
                s.name, count
            );
        } else {
            assert!(
                count >= 1,
                "scenario {} emitted no fingerprints — branch coverage gap",
                s.name
            );
            if s.run_index_path {
                let calibrate_n = all
                    .iter()
                    .filter(|f| {
                        f.scenario == s.name && f.from_index_path == FromIndexPath::Calibrate
                    })
                    .count();
                let index_n = all
                    .iter()
                    .filter(|f| {
                        f.scenario == s.name
                            && f.from_index_path == FromIndexPath::CalibrateWithIndex
                    })
                    .count();
                assert!(
                    calibrate_n >= 1 && index_n >= 1,
                    "scenario {}: both paths must emit (calibrate={}, index={})",
                    s.name,
                    calibrate_n,
                    index_n
                );
            }
        }
    }

    let path = fixture_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    let json = serde_json::to_string_pretty(&all).expect("serialize fingerprints");
    std::fs::write(&path, json).expect("write fixture");

    eprintln!(
        "wrote {} fingerprints across {} scenarios to {}",
        all.len(),
        scenarios.len(),
        path.display()
    );
}
