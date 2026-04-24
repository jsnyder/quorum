# External Feedback Ingestion Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Let quorum ingest TP/FP verdicts from other review agents (pal, third-opinion, gemini, etc.) as a fourth provenance tier weighted at 0.7x in the calibrator.

**Architecture:** Add `Provenance::External { agent, model, confidence }` variant. Three ingestion surfaces — inbox jsonl drop, `quorum feedback --from-agent`, and an extended MCP `feedback` tool — all funnel through a single `FeedbackStore::record_external` constructor. Inbox drain runs in `main.rs` before pipeline/stats so the pipeline module stays IO-pure.

**Tech Stack:** Rust 1.94, serde, chrono, clap v4, ulid, anyhow, tracing, tempfile (tests). No new dependencies — `ulid` crate is already in Cargo.toml.

**Design doc:** `docs/plans/2026-04-24-external-feedback-ingestion-design.md` (read first for rationale and policy thresholds).

**Branch/worktree:** `feat/external-feedback-ingest` (created by /dev:start Phase 2).

**Test discipline (critical):** Every task is RED → GREEN → REFACTOR. Write the failing test first, watch it fail for the *specific reason we expect*, then implement. Never weaken a test to make it pass. Routes to `rust-expert` for borrow/lifetime/async issues; to `testing-antipatterns-expert` if a test smells off (mock overuse, tautology, coverage cosplay).

**Existing test style:** inline `#[cfg(test)] mod tests` at bottom of module. See `src/feedback.rs::tests` and `src/calibrator.rs::tests` for canonical patterns. Use `tempfile::TempDir` for any filesystem state.

**Phase 3 review results (test-planning + antipattern agents):** 13 patches applied below covering NaN/Inf in `clamp_confidence`, Task 3 consolidation, Task 5 rename+seam-extraction, Task 7 behavior-test-not-library-test, cross-path equivalence, struct-over-string assertions, ContextMisleading rejection, `QUORUM_HOME` hermetic env, proptests for clamp + agent-normalization, schema forward-compat, stats zero-graceful, Task 1 build-exhaustiveness note.

**Scope deviation from issue #32 success criteria:** Issue says "calibrator applies 0.7x weight (configurable) to external verdicts." v1 hardcodes 0.7x — configurability is a v1.1 follow-up. Call this out explicitly in the PR description so the checkbox isn't silently unchecked.

**Confirmed via code inspection (2026-04-24):**
- `ulid = { version = "1", features = ["serde"] }`, `assert_cmd = "2"`, `predicates = "3"`, `tempfile = "3"` — all already in Cargo.toml, no additions needed.
- `cli::FeedbackOpts` lives at `src/cli/mod.rs:360` — fields: `file, finding, verdict, reason, model, blamed_chunks, json`. **There is no `--provenance` flag today.** Plan does NOT introduce one — `--from-agent` stands alone as the External trigger.
- `mcp::tools::FeedbackTool` uses `file_path` (not `file`). Test fields match exactly.
- `src/mcp/handler.rs` has NO shared `test_handler()` helper — existing tests construct `McpHandler` directly with `feedback_store: FeedbackStore::new(dir.path().join("fb.jsonl"))`. See lines 370/394/422/453 for reference.
- `src/analytics.rs::compute_stats` aggregates by `entry.model` (per-source), NOT by provenance tier. Task 9 ADDS a new `compute_tier_stats` function in parallel — it does NOT refactor `compute_stats`.

---

## Task 0: Baseline verification (prerequisite check)

**Files:** none (read-only check)

**Why this task exists:** TDD relies on failing for the *right* reason. If the baseline has a compile error or a flaky test, Task 1's "failing test" diagnosis becomes noise.

**Step 1: Confirm deps are already present**

```bash
rg '^(ulid|assert_cmd|predicates|tempfile)' Cargo.toml
```
Expected: 4 lines match. If any are missing, add them before proceeding.

**Step 2: Establish green baseline**

```bash
cargo test --bin quorum 2>&1 | tail -20
```
Expected: `test result: ok.` at the end. If anything fails, STOP and investigate — don't proceed to Task 1.

**Step 3: Confirm no in-flight work**

```bash
rtk git status
```
Expected: clean working tree (`nothing to commit`). If dirty, decide whether to stash.

**Step 4: No commit.** This task only verifies.

---

## Task 1: Add `External` variant to `Provenance` enum (schema only, no weight yet)

**Files:**
- Modify: `src/feedback.rs` (enum `Provenance` at top of file)
- Test: `src/feedback.rs` (inline `#[cfg(test)] mod tests`)

**Why start here:** The enum variant is the keystone. Every downstream test depends on it existing. Weight change comes in Task 2 so we can independently verify the schema compiles and round-trips.

**Step 1: Write the failing roundtrip test**

Add to `src/feedback.rs` tests module:

```rust
#[test]
fn external_variant_roundtrips_through_jsonl() {
    let (store, _dir) = test_store();
    let entry = FeedbackEntry {
        file_path: "src/auth.rs".into(),
        finding_title: "SQL injection".into(),
        finding_category: "security".into(),
        verdict: Verdict::Tp,
        reason: "Confirmed".into(),
        model: None,
        timestamp: Utc::now(),
        provenance: Provenance::External {
            agent: "pal".into(),
            model: Some("gemini-3-pro-preview".into()),
            confidence: Some(0.9),
        },
    };
    store.record(&entry).unwrap();
    let all = store.load_all().unwrap();
    assert_eq!(all.len(), 1);
    match &all[0].provenance {
        Provenance::External { agent, model, confidence } => {
            assert_eq!(agent, "pal");
            assert_eq!(model.as_deref(), Some("gemini-3-pro-preview"));
            assert_eq!(*confidence, Some(0.9));
        }
        other => panic!("expected External, got {:?}", other),
    }
}

#[test]
fn external_serializes_with_external_tag() {
    let p = Provenance::External {
        agent: "pal".into(),
        model: Some("gpt-5.4".into()),
        confidence: None,
    };
    let v = serde_json::to_value(&p).unwrap();
    // Externally tagged: {"external": {...}}
    assert!(v.get("external").is_some(), "expected 'external' tag, got {v}");
    let inner = v.get("external").unwrap();
    assert_eq!(inner.get("agent").and_then(|x| x.as_str()), Some("pal"));
    // `confidence: None` may serialize as `null` OR be absent (if serde adds
    // skip_serializing_if later). Both are valid wire forms — accept either.
    assert!(inner.get("confidence").map_or(true, |c| c.is_null()),
        "confidence must be null or absent, got {:?}", inner.get("confidence"));
}

#[test]
fn external_deserializes_when_confidence_key_absent() {
    // Contract: agents may omit the confidence key entirely. Must round-trip
    // to Provenance::External { confidence: None, .. }.
    let json = r#"{"external":{"agent":"pal","model":"gpt-5.4"}}"#;
    let p: Provenance = serde_json::from_str(json).unwrap();
    match p {
        Provenance::External { agent, model, confidence } => {
            assert_eq!(agent, "pal");
            assert_eq!(model.as_deref(), Some("gpt-5.4"));
            assert_eq!(confidence, None);
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn unknown_provenance_variant_deserializes_as_unknown() {
    // Forward-compat: a future quorum may add a `Provenance::Foo` variant.
    // An older quorum seeing such rows must NOT hard-fail load_all — it must
    // fall back to Unknown so the store remains readable. `#[serde(other)]`
    // on a catch-all variant is the standard pattern for this.
    // NOTE: this test requires `#[serde(other)]` on Provenance::Unknown. If the
    // first run fails with "unknown variant `future_variant`", add that attribute.
    let json = r#"{"file_path":"x.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":{"future_variant":{"agent":"x"}}}"#;
    let entry: FeedbackEntry = serde_json::from_str(json)
        .expect("forward-compat: unknown provenance variant must deserialize");
    assert_eq!(entry.provenance, Provenance::Unknown);
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum feedback::tests::external_ -- --nocapture
```
Expected: compile error — `no variant named External on Provenance`.

**Step 3: Add the variant**

Edit `src/feedback.rs` enum:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Human,
    PostFix,
    AutoCalibrate(String),
    External {
        agent: String,
        model: Option<String>,
        confidence: Option<f32>,
    },
    #[serde(other)]
    Unknown,
}
```

Notes:
- Drop `Eq` from the derive list — `Option<f32>` is not `Eq`. The existing derive was `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]`. Downstream callers use `PartialEq` via `assert_eq!`, not `Eq` trait bounds, so this is safe. Grep to confirm: `rg 'Provenance:.*Eq'` should only hit the derive itself.
- `#[serde(other)]` on `Unknown` is the forward-compat fallback for the `unknown_provenance_variant_deserializes_as_unknown` test. Without it, future variants added by newer quorum versions would cause `load_all` to skip rows silently.

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum feedback::tests::external_ -- --nocapture
```
Expected: both PASS.

**Step 5: Verify no existing test broke AND the full crate still builds**

```bash
cargo test --bin quorum feedback::tests
cargo build --bin quorum 2>&1 | rg -i 'error|non-exhaustive' | head
```
Expected: first command all PASS (the existing `provenance_serializes_correctly` still works — unit variants unchanged). Second command should produce no matches — `verdict_weight` in calibrator.rs has an exhaustive match, so adding the `External` variant would break the build until Task 2 lands. If the build fails here, proceed directly to Task 2 in the same commit (don't commit a broken build).

**Step 6: Commit**

```bash
git add src/feedback.rs
git commit -m "feat(feedback): add External provenance variant (issue #32)"
```

---

## Task 2: Calibrator weight for External = 0.7, preserve Unknown = 0.3

**Files:**
- Modify: `src/calibrator.rs` (function `verdict_weight` ~line 45-61)
- Test: `src/calibrator.rs` (inline tests)

**Step 1: Write failing weight tests**

Add to `src/calibrator.rs::tests`:

```rust
#[test]
fn external_provenance_weights_0_7() {
    let entry = FeedbackEntry {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: "c".into(),
        verdict: Verdict::Tp,
        reason: "r".into(),
        model: None,
        timestamp: Utc::now(),  // recency ≈ 1.0
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: None,
        },
    };
    let w = verdict_weight(&entry);
    // recency for age=0 is 1.0; allow tiny float drift
    assert!((w - 0.7).abs() < 0.01, "expected ~0.7, got {w}");
}

#[test]
fn external_weight_independent_of_confidence_in_v1() {
    // confidence is stored but IGNORED by calibrator in v1.
    // Table-driven so one failure doesn't mask the others.
    let mk = |conf: Option<f32>| FeedbackEntry {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: "c".into(),
        verdict: Verdict::Tp,
        reason: "r".into(),
        model: None,
        timestamp: Utc::now(),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: conf,
        },
    };
    let cases: &[(&str, Option<f32>)] = &[
        ("None", None),
        ("low", Some(0.1)),
        ("high", Some(0.99)),
        ("zero", Some(0.0)),
        ("one", Some(1.0)),
    ];
    for (label, conf) in cases {
        let w = verdict_weight(&mk(*conf));
        assert!(
            (w - 0.7).abs() < 1e-6,
            "confidence={label}: expected 0.7, got {w}"
        );
    }
}

#[test]
fn unknown_weight_remains_0_3_regression_guard() {
    // Guard against accidentally changing Unknown while editing match arms
    let entry = FeedbackEntry {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: "c".into(),
        verdict: Verdict::Tp,
        reason: "r".into(),
        model: None,
        timestamp: Utc::now(),
        provenance: crate::feedback::Provenance::Unknown,
    };
    let w = verdict_weight(&entry);
    assert!((w - 0.3).abs() < 0.01, "Unknown must stay at 0.3, got {w}");
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum calibrator::tests::external_provenance_weights_0_7 \
           calibrator::tests::unknown_weight_remains_0_3_regression_guard \
           calibrator::tests::external_weight_independent_of_confidence_in_v1
```
Expected: compile failure — `Provenance::External` is now an unmatched arm in the match at calibrator.rs:46.

**Step 3: Add the match arm**

Edit `src/calibrator.rs::verdict_weight` (line ~46):

```rust
let provenance_weight = match &entry.provenance {
    crate::feedback::Provenance::PostFix => 1.5,
    crate::feedback::Provenance::Human => 1.0,
    crate::feedback::Provenance::External { .. } => 0.7,
    crate::feedback::Provenance::AutoCalibrate(_) => 0.5,
    crate::feedback::Provenance::Unknown => 0.3,
};
```

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum calibrator::tests::external_provenance_weights_0_7 \
           calibrator::tests::unknown_weight_remains_0_3_regression_guard \
           calibrator::tests::external_weight_independent_of_confidence_in_v1
```
Expected: all PASS.

**Step 5: Run full calibrator suite to catch regressions**

```bash
cargo test --bin quorum calibrator::tests
```
Expected: all PASS. If any existing test fails, STOP and investigate — the match ordering change should be behavior-preserving for all non-External variants.

**Step 6: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): weight External provenance at 0.7x (issue #32)"
```

---

## Task 3: External bypasses `use_auto_feedback` filter + uncapped in `other_*_weight` bucket

**Files:**
- Modify: `src/calibrator.rs` (no code change — verify existing filter logic passes External through; we're only adding tests that pin the behavior)
- Test: `src/calibrator.rs` (inline tests)

**Why this task exists:** The filter at calibrator.rs:75 and :337 specifically excludes `AutoCalibrate`. The cap at :133 and :370 specifically buckets `AutoCalibrate` into `auto_*_weight.min(1.0)`. `External` inherits the "fall through as other" behavior by *default* because those code paths only branch on `AutoCalibrate`. We pin this with tests so a future edit that adds External to those branches breaks CI.

**Step 1: Write failing pinning tests**

Before writing, grep for existing finding-construction helpers:

```bash
rg -n 'fn (sample|mk|build)_finding|fn (sample|mk|build)_fb' src/calibrator.rs | head
```

Reuse the closest existing helper (or add a local one following its shape). Don't create parallel helpers by accident.

Add to `src/calibrator.rs::tests`:

```rust
// Helper — build an External FP FeedbackEntry with a given age.
fn external_fp(age_days: i64) -> FeedbackEntry {
    FeedbackEntry {
        file_path: "src/auth.rs".into(),
        finding_title: "SQL injection".into(),
        finding_category: "security".into(),
        verdict: Verdict::Fp,
        reason: "r".into(),
        model: None,
        timestamp: Utc::now() - chrono::Duration::days(age_days),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: None,
        },
    }
}

#[test]
fn external_not_filtered_when_use_auto_feedback_false() {
    // External must survive the use_auto_feedback=false filter that targets AutoCalibrate.
    let findings = vec![sample_finding("SQL injection", Severity::High)];
    let feedback = vec![external_fp(0)];
    let config = CalibratorConfig {
        use_auto_feedback: false,
        ..CalibratorConfig::default()
    };
    let result = calibrate(findings, &feedback, &config);
    let trace = result.traces.last().expect("expected a calibrator trace");
    assert!(!trace.matched_precedents.is_empty(),
        "External verdict must survive use_auto_feedback=false");
}

#[test]
fn external_fp_accumulation_thresholds() {
    // Table-driven: one test covers n=1,2,3 with a clear per-row failure message.
    // Subsumes the earlier four-way split which duplicated setup and hid accumulator bugs.
    use Severity::*;
    #[derive(Debug, PartialEq)]
    enum Outcome { Kept, Soft, Full }

    let cases: &[(usize, Outcome)] = &[
        (1, Outcome::Kept),  // 1 × 0.7 = 0.7: below soft (1.0) and full (1.5) thresholds
        (2, Outcome::Soft),  // 2 × 0.7 = 1.4: soft-suppress (>=1.0), not full (<1.5)
        (3, Outcome::Full),  // 3 × 0.7 = 2.1: full-suppress (>=1.5)
    ];

    for (n, expected) in cases {
        let findings = vec![sample_finding("SQL injection", High)];
        let feedback: Vec<_> = (0..*n as i64).map(external_fp).collect();
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        let outcome = match (result.suppressed, result.findings.first().map(|f| &f.severity)) {
            (1, _) => Outcome::Full,
            (0, Some(Severity::Info)) => Outcome::Soft,
            (0, Some(_)) => Outcome::Kept,
            _ => panic!("unexpected result for n={n}: {result:?}"),
        };
        assert_eq!(outcome, *expected, "n={n}: expected {expected:?}, got {outcome:?}");
    }
}

#[test]
fn external_accumulator_uncapped_verified_via_trace() {
    // Stronger pin than outcome-based tests: inspect the calibrator trace to
    // confirm the actual weight accumulated exceeds 1.0 (the AutoCalibrate cap).
    // If a future edit accidentally routes External into auto_fp_weight.min(1.0),
    // this test fails even if the end-outcome happens to still be "full-suppress"
    // for unrelated reasons.
    let findings = vec![sample_finding("X", Severity::High)];
    let feedback: Vec<_> = (0..3).map(external_fp).collect();
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    let trace = result.traces.last().expect("expected a trace");
    // 3 fresh External FPs → fp_weight should be ≈ 3 × 0.7 = 2.1 (modulo small
    // recency decay at age=0,1,2 days). If External were capped at 1.0, this
    // would be ≤ 1.0.
    assert!(trace.fp_weight > 1.0,
        "expected uncapped fp_weight > 1.0 (got {}) — External must not be capped like AutoCalibrate",
        trace.fp_weight);
}
```

Note: `sample_finding` — grep as shown above. If it doesn't exist, add a minimal local helper that constructs a `Finding` with the given title + severity. Do NOT create a parallel helper if an existing one (e.g. `build_finding`, `mk_finding`) is already in scope.

**Step 2: Run tests — expect them to PASS immediately**

```bash
cargo test --bin quorum calibrator::tests::external_not_filtered_when_use_auto_feedback_false \
           calibrator::tests::external_fp_accumulation_thresholds \
           calibrator::tests::external_accumulator_uncapped_verified_via_trace
```

**Expected outcome: all 3 PASS on first run.** That's the point of pinning tests — the filter/cap sites correctly pass External through by default (they only branch on `AutoCalibrate`). These tests don't drive any code change; they *fence off* a future accidental change that adds External to the filter or cap arms.

**If any FAIL:** STOP. It means either (a) an earlier task introduced a real gap (route to `rust-expert` to diagnose), or (b) a test assumption is wrong (e.g., `sample_finding` produces a finding that doesn't match the feedback title closely enough for the calibrator's similarity threshold — grep and verify).

**Step 3: Commit pinning tests**

```bash
git add src/calibrator.rs
git commit -m "test(calibrator): pin External filter+cap behavior (issue #32)"
```

---

## Task 4: `ExternalVerdictInput` DTO + `record_external` constructor

**Files:**
- Modify: `src/feedback.rs` (add DTO + method)
- Test: `src/feedback.rs` (inline tests)

**Step 1: Write failing DTO tests**

```rust
#[test]
fn record_external_writes_external_provenance() {
    let (store, _dir) = test_store();
    let input = ExternalVerdictInput {
        file_path: "src/a.rs".into(),
        finding_title: "Bug".into(),
        finding_category: Some("security".into()),
        verdict: Verdict::Tp,
        reason: "confirmed".into(),
        agent: "pal".into(),
        agent_model: Some("gemini-3-pro-preview".into()),
        confidence: Some(0.85),
    };
    store.record_external(input).unwrap();
    let all = store.load_all().unwrap();
    assert_eq!(all.len(), 1);
    match &all[0].provenance {
        Provenance::External { agent, model, confidence } => {
            assert_eq!(agent, "pal");
            assert_eq!(model.as_deref(), Some("gemini-3-pro-preview"));
            assert_eq!(*confidence, Some(0.85));
        }
        o => panic!("expected External, got {o:?}"),
    }
    assert!(all[0].model.is_none(), "entry.model should be None (reviewer model, not agent model)");
}

#[test]
fn record_external_normalizes_agent_name() {
    let (store, _dir) = test_store();
    store.record_external(ExternalVerdictInput {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: None,
        verdict: Verdict::Tp,
        reason: "r".into(),
        agent: "  PaL  ".into(),  // mixed case + whitespace
        agent_model: None,
        confidence: None,
    }).unwrap();
    let all = store.load_all().unwrap();
    match &all[0].provenance {
        Provenance::External { agent, .. } => assert_eq!(agent, "pal"),
        o => panic!("{o:?}"),
    }
}

#[test]
fn record_external_rejects_empty_agent() {
    let (store, _dir) = test_store();
    let err = store.record_external(ExternalVerdictInput {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: None,
        verdict: Verdict::Tp,
        reason: "r".into(),
        agent: "   ".into(),  // whitespace-only
        agent_model: None,
        confidence: None,
    }).expect_err("empty agent must be rejected");
    assert!(err.to_string().to_lowercase().contains("agent"),
        "error message should mention agent: {err}");
}

#[test]
fn record_external_defaults_missing_category_to_unknown() {
    let (store, _dir) = test_store();
    store.record_external(ExternalVerdictInput {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: None,
        verdict: Verdict::Tp,
        reason: "r".into(),
        agent: "pal".into(),
        agent_model: None,
        confidence: None,
    }).unwrap();
    let all = store.load_all().unwrap();
    assert_eq!(all[0].finding_category, "unknown");
}

// Unit-test the pure clamp function directly — no filesystem needed.
#[test]
fn clamp_confidence_maps_values() {
    assert_eq!(clamp_confidence(None), None);
    assert_eq!(clamp_confidence(Some(0.42)), Some(0.42));
    assert_eq!(clamp_confidence(Some(1.5)), Some(1.0));
    assert_eq!(clamp_confidence(Some(-0.2)), Some(0.0));
    assert_eq!(clamp_confidence(Some(0.0)), Some(0.0));
    assert_eq!(clamp_confidence(Some(1.0)), Some(1.0));
}

#[test]
fn clamp_confidence_rejects_nan_inf() {
    // f32::clamp(0.0, 1.0) is NOT NaN-safe — it returns NaN for NaN input.
    // clamp_confidence must detect and reject non-finite values explicitly.
    assert_eq!(clamp_confidence(Some(f32::NAN)), None, "NaN must become None");
    assert_eq!(clamp_confidence(Some(f32::INFINITY)), None, "+inf must become None");
    assert_eq!(clamp_confidence(Some(f32::NEG_INFINITY)), None, "-inf must become None");
}

// One integration test that the record path calls clamp_confidence.
#[test]
fn record_external_applies_clamp_confidence() {
    let (store, _dir) = test_store();
    store.record_external(ExternalVerdictInput {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: None,
        verdict: Verdict::Tp,
        reason: "r".into(),
        agent: "pal".into(),
        agent_model: None,
        confidence: Some(1.5),
    }).unwrap();
    let all = store.load_all().unwrap();
    match &all[0].provenance {
        Provenance::External { confidence, .. } => {
            assert_eq!(*confidence, Some(1.0), "1.5 must clamp to 1.0");
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn record_external_rejects_context_misleading_verdict() {
    // ContextMisleading requires blamed_chunk_ids the reviewer identified.
    // An external agent can't credibly produce those (it doesn't see our
    // injected context), so the verdict has no meaningful semantics here.
    // Reject at the ingest boundary to prevent polluting the calibrator.
    let (store, _dir) = test_store();
    let err = store.record_external(ExternalVerdictInput {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: None,
        verdict: Verdict::ContextMisleading { blamed_chunk_ids: vec!["c1".into()] },
        reason: "r".into(),
        agent: "pal".into(),
        agent_model: None,
        confidence: None,
    }).expect_err("ContextMisleading must be rejected for External provenance");
    assert!(
        err.to_string().to_lowercase().contains("context"),
        "error message must mention context_misleading: {err}"
    );
}
```

**Proptest (add as separate test module; proptest = "1" is already in Cargo.toml):**

```rust
#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn clamp_always_finite_and_in_unit_interval(c in any::<f32>()) {
            match clamp_confidence(Some(c)) {
                Some(out) => {
                    prop_assert!(out.is_finite(), "clamp output must be finite, got {out}");
                    prop_assert!((0.0..=1.0).contains(&out), "out={out} not in [0,1]");
                }
                None => prop_assert!(!c.is_finite(), "None only allowed for non-finite input, got {c}"),
            }
        }

        #[test]
        fn normalize_agent_is_idempotent(s in "\\PC{0,64}") {
            let once = normalize_agent(&s);
            let twice = once.as_ref().map(|x| normalize_agent(x)).and_then(|r| r.ok());
            if let Ok(first) = &once {
                prop_assert_eq!(Some(first.clone()), twice.flatten().ok(),
                    "normalize(normalize(s)) must equal normalize(s)");
            }
        }

        #[test]
        fn normalize_agent_empty_iff_trim_empty(s in "\\PC{0,64}") {
            let normalized = normalize_agent(&s);
            prop_assert_eq!(normalized.is_err(), s.trim().is_empty(),
                "err iff trim empty for input {s:?}");
        }
    }
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum feedback::tests::record_external_
```
Expected: compile error — `ExternalVerdictInput` and `record_external` undefined.

**Step 3: Implement DTO + method**

Add to `src/feedback.rs` (public items, above `impl FeedbackStore`):

```rust
/// Input for recording a verdict from an external review agent.
///
/// Use `FeedbackStore::record_external` instead of constructing a `FeedbackEntry`
/// directly — it handles agent-name normalization, confidence clamping, and
/// timestamp assignment.
#[derive(Debug, Clone)]
pub struct ExternalVerdictInput {
    pub file_path: String,
    pub finding_title: String,
    pub finding_category: Option<String>,
    pub verdict: Verdict,
    pub reason: String,
    pub agent: String,
    pub agent_model: Option<String>,
    pub confidence: Option<f32>,
}
```

Add helper functions (pub(crate) so proptests can use them):

```rust
/// Clamp confidence to [0,1], mapping NaN/±Inf to None.
/// f32::clamp is NOT NaN-safe — this wraps it with an is_finite gate.
pub(crate) fn clamp_confidence(c: Option<f32>) -> Option<f32> {
    c.filter(|x| x.is_finite()).map(|x| x.clamp(0.0, 1.0))
}

/// Normalize an agent name: trim + lowercase. Returns Err for empty-after-trim.
pub(crate) fn normalize_agent(raw: &str) -> anyhow::Result<String> {
    let t = raw.trim();
    if t.is_empty() {
        anyhow::bail!("agent name cannot be empty after normalization");
    }
    Ok(t.to_lowercase())
}
```

Add to `impl FeedbackStore`:

```rust
/// Record a verdict from an external review agent (pal, third-opinion, etc.).
/// Normalizes agent name, NaN-safe confidence clamp, rejects ContextMisleading
/// verdicts (external agents cannot credibly produce blamed_chunk_ids).
/// See issue #32.
pub fn record_external(&self, input: ExternalVerdictInput) -> anyhow::Result<()> {
    if matches!(input.verdict, Verdict::ContextMisleading { .. }) {
        anyhow::bail!(
            "context_misleading verdicts are not accepted from External agents \
             (they cannot identify blamed chunks in our injected context)"
        );
    }
    let agent = normalize_agent(&input.agent)?;
    let confidence = clamp_confidence(input.confidence);
    let category = input.finding_category
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let entry = FeedbackEntry {
        file_path: input.file_path,
        finding_title: input.finding_title,
        finding_category: category,
        verdict: input.verdict,
        reason: input.reason,
        model: None,
        timestamp: Utc::now(),
        provenance: Provenance::External {
            agent,
            model: input.agent_model,
            confidence,
        },
    };
    self.record(&entry)
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum feedback::tests::record_external_
```
Expected: all 5 PASS.

**Step 5: Commit**

```bash
git add src/feedback.rs
git commit -m "feat(feedback): add ExternalVerdictInput + record_external (issue #32)"
```

---

## Task 5: `drain_inbox` — fast-path + happy path + malformed tolerance

**Files:**
- Modify: `src/feedback.rs` (add `drain_inbox` as an impl method or free function on `FeedbackStore`)
- Test: `src/feedback.rs` (inline tests)

**Step 1: Write failing tests**

```rust
#[test]
fn drain_inbox_missing_dir_returns_zero_work() {
    // Inbox dir doesn't exist at all (first-run scenario). Must NOT error.
    let dir = tempfile::TempDir::new().unwrap();
    let inbox = dir.path().join("nonexistent-inbox");
    let processed = dir.path().join("processed");
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.drained_files, 0);
    assert_eq!(report.entries, 0);
    assert!(report.errors.is_empty());
    assert!(!processed.exists(), "processed/ must not be created when inbox is absent");
}

#[test]
fn drain_inbox_empty_returns_zero_work() {
    let dir = tempfile::TempDir::new().unwrap();
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.drained_files, 0);
    assert_eq!(report.entries, 0);
    assert!(report.errors.is_empty());
    assert_eq!(report.processed_bytes, 0);
    // processed/ should NOT be created if there was no work
    assert!(!processed.exists(), "processed/ should not be created when inbox is empty");
}

#[test]
fn drain_inbox_valid_file_appends_and_moves() {
    let dir = tempfile::TempDir::new().unwrap();
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    let line = serde_json::to_string(&serde_json::json!({
        "file_path": "src/a.rs",
        "finding_title": "Bug",
        "finding_category": "security",
        "verdict": "tp",
        "reason": "confirmed",
        "agent": "pal",
        "agent_model": "gemini-3-pro-preview",
        "confidence": 0.9
    })).unwrap();
    let inbox_file = inbox.join("pal-run-1.jsonl");
    std::fs::write(&inbox_file, format!("{line}\n")).unwrap();

    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.drained_files, 1);
    assert_eq!(report.entries, 1);
    assert!(report.errors.is_empty());

    // Feedback store contains the entry
    let all = store.load_all().unwrap();
    assert_eq!(all.len(), 1);
    assert!(matches!(all[0].provenance, Provenance::External { .. }));

    // Original inbox file is gone
    assert!(!inbox_file.exists(), "inbox file should be moved after drain");

    // A file exists in processed/ with ulid suffix
    let processed_files: Vec<_> = std::fs::read_dir(&processed).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(processed_files.len(), 1);
    let name = processed_files[0].file_name().into_string().unwrap();
    assert!(name.starts_with("pal-run-1.jsonl."), "expected ulid-suffixed name, got {name}");
    assert!(name.ends_with(".jsonl"));
}

#[test]
fn drain_inbox_malformed_line_skipped_rest_drained() {
    let dir = tempfile::TempDir::new().unwrap();
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    let good = serde_json::to_string(&serde_json::json!({
        "file_path": "src/a.rs",
        "finding_title": "Bug",
        "finding_category": "security",
        "verdict": "tp",
        "reason": "r",
        "agent": "pal",
        "agent_model": null,
        "confidence": null
    })).unwrap();
    let bad = "{not json";
    std::fs::write(inbox.join("mix.jsonl"), format!("{good}\n{bad}\n{good}\n")).unwrap();

    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.drained_files, 1);
    assert_eq!(report.entries, 2, "2 good + 1 bad = 2 appended");
    assert_eq!(report.errors.len(), 1);

    let all = store.load_all().unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn drain_inbox_is_idempotent_on_empty_second_call() {
    // After a successful drain, the inbox is empty; a second drain must
    // produce zero work. Honest name for what this actually tests.
    let dir = tempfile::TempDir::new().unwrap();
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    let line = r#"{"file_path":"a.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
    std::fs::write(inbox.join("a.jsonl"), format!("{line}\n")).unwrap();

    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let r1 = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(r1.drained_files, 1);
    let r2 = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(r2.drained_files, 0, "second drain is a no-op, not an error");
}

#[test]
fn rename_or_tolerate_race_swallows_nonexistent_source() {
    // Directly tests the ENOENT-tolerance contract by calling the extracted
    // seam with a source path that doesn't exist. Proves the multi-process
    // race arm of drain_inbox without requiring actual concurrency.
    let dir = tempfile::TempDir::new().unwrap();
    let missing = dir.path().join("not-there.jsonl");
    let dst = dir.path().join("processed").join("moved.jsonl");
    let renamed = rename_or_tolerate_race(&missing, &dst).unwrap();
    assert!(!renamed, "missing source must return Ok(false), not Err");
    assert!(!dst.exists(), "destination must not be created");
}

#[test]
fn drain_inbox_rejects_uppercase_verdict_string() {
    // Verdict must round-trip through #[serde(rename_all="snake_case")].
    // "TP" is not valid; the line lands in errors, other lines still drain.
    let dir = tempfile::TempDir::new().unwrap();
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    let bad = r#"{"file_path":"a.rs","finding_title":"t","finding_category":"c","verdict":"TP","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
    let good = r#"{"file_path":"b.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
    std::fs::write(inbox.join("mix.jsonl"), format!("{bad}\n{good}\n")).unwrap();

    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.drained_files, 1);
    assert_eq!(report.entries, 1, "only the valid line was appended");
    assert_eq!(report.errors.len(), 1, "uppercase TP must land in errors");
    assert!(report.errors[0].message.to_lowercase().contains("tp")
         || report.errors[0].message.to_lowercase().contains("verdict"),
        "error must mention the bad verdict: {}", report.errors[0].message);
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum feedback::tests::drain_inbox
```
Expected: compile error — `drain_inbox`, `DrainReport`, `DrainError` undefined.

**Step 3: Implement drain**

Add to `src/feedback.rs`:

```rust
#[derive(Debug, Clone)]
pub struct DrainReport {
    pub drained_files: usize,
    pub entries: usize,
    pub errors: Vec<DrainError>,
    pub processed_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct DrainError {
    pub file: PathBuf,
    pub line: usize,       // 0 = file-level error (e.g. read failure)
    pub message: String,
}

impl FeedbackStore {
    /// Drain all *.jsonl files from `inbox_dir` into this store as External verdicts.
    /// On success, each file is moved (atomic rename) to `processed_dir/<name>.<ulid>.jsonl`.
    /// Malformed lines are skipped and logged to the returned `DrainReport.errors`.
    /// ENOENT on rename is treated as a successful no-op (lock-free multi-process race).
    pub fn drain_inbox(
        &self,
        inbox_dir: &std::path::Path,
        processed_dir: &std::path::Path,
    ) -> anyhow::Result<DrainReport> {
        use std::io::ErrorKind;
        let mut report = DrainReport {
            drained_files: 0, entries: 0, errors: vec![], processed_bytes: 0
        };

        // Fast path: ENOENT → zero work, idiomatic pattern (no double-read_dir,
        // no TOCTOU). An empty dir yields an empty iterator which is also a no-op.
        let read = match std::fs::read_dir(inbox_dir) {
            Ok(r) => r,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(e.into()),
        };

        let mut files: Vec<PathBuf> = read
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .filter(|p| !p.is_dir())  // skip the processed/ subdir
            .collect();
        files.sort();  // deterministic order for tests

        if files.is_empty() {
            return Ok(report);
        }

        std::fs::create_dir_all(processed_dir)?;

        for file in files {
            let content = match std::fs::read_to_string(&file) {
                Ok(c) => c,
                Err(e) => {
                    report.errors.push(DrainError {
                        file: file.clone(), line: 0,
                        message: format!("read failed: {e}")
                    });
                    continue;
                }
            };
            for (idx, line) in content.lines().enumerate() {
                if line.trim().is_empty() { continue; }
                match serde_json::from_str::<ExternalVerdictInputWire>(line) {
                    Ok(wire) => {
                        let input: ExternalVerdictInput = wire.into();
                        if let Err(e) = self.record_external(input) {
                            report.errors.push(DrainError {
                                file: file.clone(), line: idx + 1,
                                message: format!("record failed: {e}"),
                            });
                        } else {
                            report.entries += 1;
                        }
                    }
                    Err(e) => {
                        report.errors.push(DrainError {
                            file: file.clone(), line: idx + 1,
                            message: format!("parse failed: {e}"),
                        });
                    }
                }
            }
            // Move to processed/ via extracted seam (unit-testable ENOENT tolerance)
            let fname = file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown.jsonl");
            let ulid = ulid::Ulid::new().to_string();
            let target = processed_dir.join(format!("{fname}.{ulid}.jsonl"));
            match rename_or_tolerate_race(&file, &target) {
                Ok(true) => {
                    if let Ok(meta) = std::fs::metadata(&target) {
                        report.processed_bytes += meta.len();
                    }
                    report.drained_files += 1;
                }
                Ok(false) => {
                    // ENOENT — another process beat us to it. Not an error.
                }
                Err(e) => {
                    report.errors.push(DrainError {
                        file: file.clone(), line: 0,
                        message: format!("rename to processed failed: {e}"),
                    });
                }
            }
        }

        // Size-warning threshold
        const WARN_BYTES: u64 = 50 * 1024 * 1024;
        if let Ok(entries) = std::fs::read_dir(processed_dir) {
            let total: u64 = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum();
            if total > WARN_BYTES {
                tracing::warn!(
                    processed_dir = %processed_dir.display(),
                    total_mb = total / 1024 / 1024,
                    "quorum inbox processed/ is large; consider manual cleanup"
                );
            }
        }

        Ok(report)
    }
}

/// Rename `src` to `dst`. Returns `Ok(true)` on success, `Ok(false)` if the
/// source disappeared between enumeration and rename (benign multi-process race).
/// Any other IO error propagates. Extracted for direct unit testing.
pub(crate) fn rename_or_tolerate_race(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<bool> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

// Wire format — what agents write to inbox jsonl (mirrors ExternalVerdictInput).
#[derive(Debug, Deserialize)]
struct ExternalVerdictInputWire {
    file_path: String,
    finding_title: String,
    finding_category: Option<String>,
    verdict: Verdict,
    reason: String,
    agent: String,
    agent_model: Option<String>,
    confidence: Option<f32>,
}

impl From<ExternalVerdictInputWire> for ExternalVerdictInput {
    fn from(w: ExternalVerdictInputWire) -> Self {
        Self {
            file_path: w.file_path,
            finding_title: w.finding_title,
            finding_category: w.finding_category,
            verdict: w.verdict,
            reason: w.reason,
            agent: w.agent,
            agent_model: w.agent_model,
            confidence: w.confidence,
        }
    }
}
```

Note: `ulid` crate already in Cargo.toml — verify with `rg '^ulid' Cargo.toml`. If not present, add `ulid = "1"`.

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum feedback::tests::drain_inbox
```
Expected: all 4 PASS.

If a borrow/lifetime issue appears: delegate to `rust-expert`.

**Step 5: Commit**

```bash
git add src/feedback.rs Cargo.toml Cargo.lock
git commit -m "feat(feedback): add drain_inbox with fast-path + atomic-move (issue #32)"
```

---

## Task 6: Wire drain into `main.rs` before review/stats

**Files:**
- Modify: `src/main.rs` (at the top of review + stats dispatch arms)
- Test: CLI integration test in `tests/` (or skip and verify manually — see Step 5)

**Step 1: Locate the dispatch sites**

```bash
rg -n 'Command::Review|Command::Stats' src/main.rs | head -6
```

**Step 2: Write a CLI integration test**

Create or extend `tests/cli_inbox_drain.rs`:

```rust
use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn review_drains_inbox_before_loading_feedback() {
    let home = TempDir::new().unwrap();
    let quorum_home = home.path().join(".quorum");
    let inbox = quorum_home.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let line = r#"{"file_path":"x.rs","finding_title":"Bug","finding_category":"security","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
    std::fs::write(inbox.join("drop.jsonl"), format!("{line}\n")).unwrap();

    // Run stats (cheap, exercises the same drain hook)
    Command::cargo_bin("quorum").unwrap()
        .env("HOME", home.path())
        .args(["stats"])
        .assert()
        .success();

    // Inbox should be empty, processed/ should have one file
    let remaining: Vec<_> = std::fs::read_dir(&inbox).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(remaining.iter().filter(|e| e.path().extension().map(|x| x=="jsonl").unwrap_or(false)).count(), 0);
    let processed = quorum_home.join("inbox").join("processed");
    assert!(processed.exists());
    let moved: Vec<_> = std::fs::read_dir(&processed).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(moved.len(), 1);

    // feedback.jsonl should contain the entry
    let fb = std::fs::read_to_string(quorum_home.join("feedback.jsonl")).unwrap();
    assert!(fb.contains("\"external\""), "feedback should contain external-tagged entry");
}
```

**Step 3: Run test to verify it fails**

```bash
cargo test --test cli_inbox_drain
```
Expected: FAIL — no drain hook yet.

**Step 4: Implement the hook + add `QUORUM_HOME` env override**

First, add the env-var escape hatch so integration tests are hermetic on every platform (macOS caches `$HOME` in some crates). Grep for the existing dirs helper:

```bash
rg -n 'fn quorum_dir|fn dirs_path|home_dir' src/
```

Whatever the existing function is (let's call it `quorum_dir()` — rename to match actual), prepend an env check:

```rust
pub fn quorum_dir() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("QUORUM_HOME") {
        return Some(PathBuf::from(override_path));
    }
    // ...existing logic unchanged
}
```

Then add the drain function in `src/main.rs`:

```rust
fn drain_agent_inbox() {
    let Some(home) = crate::dirs::quorum_dir() else { return; };
    let inbox = home.join("inbox");
    let processed = inbox.join("processed");
    let feedback_path = home.join("feedback.jsonl");
    let store = crate::feedback::FeedbackStore::new(feedback_path);
    match store.drain_inbox(&inbox, &processed) {
        Ok(r) if r.drained_files > 0 => {
            tracing::info!(
                files = r.drained_files, entries = r.entries, errors = r.errors.len(),
                "drained external feedback inbox"
            );
            for e in &r.errors {
                tracing::warn!(file = %e.file.display(), line = e.line, msg = %e.message, "inbox drain error");
            }
        }
        Ok(_) => {}  // empty or no-op
        Err(e) => tracing::warn!(error = %e, "inbox drain failed"),
    }
}
```

Call `drain_agent_inbox()` at the top of the `Command::Review` and `Command::Stats` arms. (Skip for `Command::Feedback` — user is explicitly recording a verdict; don't mix filesystem races.)

**Update the test to use `QUORUM_HOME` instead of `HOME`:**

```rust
Command::cargo_bin("quorum").unwrap()
    .env("QUORUM_HOME", quorum_home.as_os_str())
    .args(["stats"])
    .assert()
    .success();
```

**Step 5: Run test to verify it passes**

```bash
cargo test --test cli_inbox_drain
```
Expected: PASS.

**Step 6: Commit**

```bash
git add src/main.rs tests/cli_inbox_drain.rs
git commit -m "feat(cli): drain agent inbox before review/stats (issue #32)"
```

---

## Task 7: CLI `--from-agent` flag

**Files:**
- Modify: `src/cli.rs` (`FeedbackOpts` struct)
- Modify: `src/main.rs` (`run_feedback` function)
- Test: `tests/cli_feedback_agent.rs` (new integration test file)

**Step 1: Write failing CLI tests**

Create `tests/cli_feedback_agent.rs`:

```rust
use assert_cmd::Command;
use tempfile::TempDir;

fn run_feedback(home: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    // Use QUORUM_HOME for hermetic isolation — see Task 6 for rationale.
    let qhome = home.join(".quorum");
    std::fs::create_dir_all(&qhome).unwrap();
    Command::cargo_bin("quorum").unwrap()
        .env("QUORUM_HOME", qhome.as_os_str())
        .args(["feedback"])
        .args(args)
        .assert()
}

#[test]
fn from_agent_writes_external_provenance() {
    let home = TempDir::new().unwrap();
    run_feedback(home.path(), &[
        "--file", "src/a.rs",
        "--finding", "SQL injection",
        "--verdict", "tp",
        "--reason", "confirmed",
        "--from-agent", "pal",
        "--agent-model", "gemini-3-pro-preview",
        "--confidence", "0.9",
    ]).success();

    let fb = std::fs::read_to_string(home.path().join(".quorum/feedback.jsonl")).unwrap();
    assert!(fb.contains("\"external\""), "feedback must contain external-tagged entry: {fb}");
    assert!(fb.contains("\"pal\""));
    assert!(fb.contains("\"gemini-3-pro-preview\""));
}

#[test]
fn agent_model_alone_does_not_write_external_entry() {
    // Behavior test (not library test): --agent-model without --from-agent
    // must NOT produce an External entry in feedback.jsonl. We don't care
    // HOW clap enforces this (requires vs conflicts_with vs custom validator)
    // — only that the contract holds. Asserting clap's stderr string would
    // couple us to clap's wording across versions.
    let home = TempDir::new().unwrap();
    let fb_path = home.path().join(".quorum/feedback.jsonl");
    let _ = run_feedback(home.path(), &[
        "--file", "a.rs",
        "--finding", "X",
        "--verdict", "tp",
        "--reason", "r",
        "--agent-model", "gpt-5.4",
    ]);  // pass or fail; we assert on side-effects, not exit
    if fb_path.exists() {
        let fb = std::fs::read_to_string(&fb_path).unwrap();
        assert!(!fb.contains("\"external\""),
            "agent-model alone must NOT produce External entry: {fb}");
    }
}

#[test]
fn feedback_without_from_agent_still_writes_human() {
    let home = TempDir::new().unwrap();
    run_feedback(home.path(), &[
        "--file", "a.rs",
        "--finding", "X",
        "--verdict", "tp",
        "--reason", "r",
    ]).success();
    let fb = std::fs::read_to_string(home.path().join(".quorum/feedback.jsonl")).unwrap();
    assert!(fb.contains("\"provenance\":\"human\""), "default path must be Human: {fb}");
}
```

Dependencies: add `predicates = "3"` and `assert_cmd = "2"` to `[dev-dependencies]` if not present. Check with `rg 'assert_cmd|predicates' Cargo.toml`.

**Step 2: Run tests to verify they fail**

```bash
cargo test --test cli_feedback_agent
```
Expected: FAIL — `--from-agent` is not a recognized flag yet.

**Step 3: Extend `FeedbackOpts`**

In `src/cli/mod.rs` around line 360 (`pub struct FeedbackOpts`), add:

```rust
/// Record the verdict as coming from an external review agent (pal, third-opinion, etc.).
/// Triggers External provenance instead of the default Human path.
#[arg(long)]
pub from_agent: Option<String>,

/// Optional: the LLM model the external agent used (only meaningful with --from-agent).
#[arg(long, requires = "from_agent")]
pub agent_model: Option<String>,

/// Optional: agent-reported confidence, clamped to [0,1]. Ignored by calibrator in v1.
#[arg(long, requires = "from_agent")]
pub confidence: Option<f32>,
```

**No `conflicts_with` needed** — `FeedbackOpts` has no `--provenance` flag today. `--from-agent` stands alone as the External trigger. If we later add `--provenance`, mutual exclusion goes in at that time.

**Step 4: Update `run_feedback` in `src/main.rs`**

Find the `run_feedback` handler (grep: `rg 'fn run_feedback' src/main.rs`). Branch on `opts.from_agent`:

```rust
if let Some(agent) = opts.from_agent {
    let input = crate::feedback::ExternalVerdictInput {
        file_path: opts.file,
        finding_title: opts.finding,
        finding_category: opts.category,
        verdict: parse_verdict(&opts.verdict)?,
        reason: opts.reason,
        agent,
        agent_model: opts.agent_model,
        confidence: opts.confidence,
    };
    store.record_external(input)?;
    println!("Recorded external verdict from agent.");
    return 0;
}
// ...existing Human path
```

**Step 5: Run tests to verify they pass**

```bash
cargo test --test cli_feedback_agent
```
Expected: all 3 PASS.

**Step 6: Commit**

```bash
git add src/cli.rs src/main.rs tests/cli_feedback_agent.rs Cargo.toml Cargo.lock
git commit -m "feat(cli): add --from-agent flag to feedback subcommand (issue #32)"
```

---

## Task 8: MCP `feedback` tool `from_agent` param

**Files:**
- Modify: `src/mcp/tools.rs` (`FeedbackTool` struct — add optional fields)
- Modify: `src/mcp/handler.rs` (`handle_feedback` — branch on `from_agent.is_some()`)
- Test: `src/mcp/handler.rs` inline tests (existing `feedback_tool_deserializes_input` pattern at tools.rs:140)

**Step 1: Write failing handler tests**

**Note:** There is NO `test_handler()` helper in `src/mcp/handler.rs`. Existing tests construct `McpHandler` directly with a tempdir-backed `FeedbackStore`. See lines 370, 394, 422, 453 for reference. Use the same pattern here — a small inline helper at the top of the new tests keeps boilerplate down.

Add to `src/mcp/handler.rs::tests`:

```rust
fn handler_with_tempdir() -> (McpHandler, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("fb.jsonl");
    // Minimal handler construction — match the existing pattern at handler.rs:422
    let handler = McpHandler {
        feedback_store: FeedbackStore::new(path),
        // fill remaining fields following existing test construction; copy
        // verbatim from the nearest existing test (handler.rs:420-460).
    };
    (handler, dir)
}

#[test]
fn mcp_from_agent_writes_external_provenance() {
    let (handler, _dir) = handler_with_tempdir();
    // NOTE: FeedbackTool uses `file_path` not `file` — see src/mcp/tools.rs:30
    let params = FeedbackTool {
        file_path: "src/a.rs".into(),
        finding: "SQL injection".into(),
        verdict: "tp".into(),
        reason: "confirmed".into(),
        model: None,
        blamed_chunks: None,
        from_agent: Some("pal".into()),
        agent_model: Some("gemini-3-pro-preview".into()),
        confidence: Some(0.9),
    };
    handler.handle_feedback(params).unwrap();
    let all = handler.feedback_store.load_all().unwrap();
    assert_eq!(all.len(), 1);
    assert!(matches!(all[0].provenance, crate::feedback::Provenance::External { .. }));
}

#[test]
fn mcp_feedback_without_from_agent_still_writes_human() {
    let (handler, _dir) = handler_with_tempdir();
    let params = FeedbackTool {
        file_path: "src/a.rs".into(),
        finding: "Bug".into(),
        verdict: "tp".into(),
        reason: "r".into(),
        model: None,
        blamed_chunks: None,
        from_agent: None,
        agent_model: None,
        confidence: None,
    };
    handler.handle_feedback(params).unwrap();
    let all = handler.feedback_store.load_all().unwrap();
    assert_eq!(all[0].provenance, crate::feedback::Provenance::Human);
}
```

**Prep step before writing the tests:** read the nearest existing MCP handler test (e.g. lines 420-460 of `src/mcp/handler.rs`) to copy the exact McpHandler struct-construction fields. The shape differs from what the plan can infer — read the code, don't guess.

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum mcp::handler::tests::mcp_
```
Expected: compile error — fields `from_agent`/`agent_model`/`confidence` don't exist.

**Step 3: Extend `FeedbackTool` schema**

In `src/mcp/tools.rs`, find the `FeedbackTool` struct (~line 23). Add:

```rust
#[schemars(description = "Optional: record as an external agent's verdict instead of human")]
pub from_agent: Option<String>,
#[schemars(description = "Optional: external agent's LLM model")]
pub agent_model: Option<String>,
#[schemars(description = "Optional: agent-reported confidence in [0,1]; ignored by calibrator in v1")]
pub confidence: Option<f32>,
```

**Step 4: Update `handle_feedback` in `src/mcp/handler.rs`**

Around line 91, branch:

```rust
fn handle_feedback(&self, params: FeedbackTool) -> Result<CallToolResult, String> {
    if let Some(agent) = params.from_agent {
        let input = crate::feedback::ExternalVerdictInput {
            file_path: params.file,
            finding_title: params.finding,
            finding_category: params.category,
            verdict: parse_verdict(&params.verdict).map_err(|e| e.to_string())?,
            reason: params.reason,
            agent,
            agent_model: params.agent_model,
            confidence: params.confidence,
        };
        self.feedback_store.record_external(input).map_err(|e| e.to_string())?;
        let count = self.feedback_store.count().unwrap_or(0);
        return Ok(CallToolResult::text(format!(
            "Recorded external verdict. Total entries: {count}"
        )));
    }
    // ...existing Human path unchanged
}
```

**Step 5: Run tests to verify they pass**

```bash
cargo test --bin quorum mcp::handler::tests::mcp_
```
Expected: both PASS.

**Step 6: Commit**

```bash
git add src/mcp/tools.rs src/mcp/handler.rs
git commit -m "feat(mcp): accept from_agent in feedback tool (issue #32)"
```

---

## Task 8.5: Cross-path equivalence — inbox + CLI + MCP produce identical entries

**Files:**
- Test: `tests/cross_path_equivalence.rs` (new integration test file)

**Why this task exists:** Three ingestion surfaces all funnel through `record_external`. If one of them forgets to normalize (e.g. CLI trims but inbox passes raw), External entries diverge silently. Prevention: an explicit test that drives identical inputs through all three paths and asserts resulting `FeedbackEntry`s are byte-identical except for `timestamp`.

**Step 1: Write the failing test**

```rust
use assert_cmd::Command;
use tempfile::TempDir;
use serde_json::Value;

fn entry_without_timestamp(e: &Value) -> Value {
    let mut e = e.clone();
    if let Some(obj) = e.as_object_mut() {
        obj.remove("timestamp");
    }
    e
}

#[test]
fn three_ingestion_paths_produce_equivalent_entries() {
    // Same logical input through each of the three surfaces.
    // All three must land identical External entries (sans timestamp).
    let payload_verdict = "tp";
    let payload_agent = "pal";
    let payload_model = "gemini-3-pro-preview";
    let payload_conf = "0.9";

    // -------- Path A: inbox drain --------
    let home_a = TempDir::new().unwrap();
    let qhome_a = home_a.path().join(".quorum");
    std::fs::create_dir_all(qhome_a.join("inbox")).unwrap();
    let line = format!(
        r#"{{"file_path":"src/a.rs","finding_title":"Bug","finding_category":"security","verdict":"{payload_verdict}","reason":"r","agent":"{payload_agent}","agent_model":"{payload_model}","confidence":{payload_conf}}}"#
    );
    std::fs::write(qhome_a.join("inbox").join("drop.jsonl"), format!("{line}\n")).unwrap();
    Command::cargo_bin("quorum").unwrap()
        .env("QUORUM_HOME", &qhome_a)
        .args(["stats"])
        .assert().success();
    let entry_a: Value = serde_json::from_str(
        &std::fs::read_to_string(qhome_a.join("feedback.jsonl")).unwrap().lines().next().unwrap()
    ).unwrap();

    // -------- Path B: CLI --from-agent --------
    let home_b = TempDir::new().unwrap();
    let qhome_b = home_b.path().join(".quorum");
    std::fs::create_dir_all(&qhome_b).unwrap();
    Command::cargo_bin("quorum").unwrap()
        .env("QUORUM_HOME", &qhome_b)
        .args([
            "feedback",
            "--file", "src/a.rs",
            "--finding", "Bug",
            "--verdict", payload_verdict,
            "--reason", "r",
            "--from-agent", payload_agent,
            "--agent-model", payload_model,
            "--confidence", payload_conf,
        ])
        .assert().success();
    let entry_b: Value = serde_json::from_str(
        &std::fs::read_to_string(qhome_b.join("feedback.jsonl")).unwrap().lines().next().unwrap()
    ).unwrap();

    // -------- Path C: MCP --------
    // Invoke via the MCP handler directly (already unit-tested via handler tests);
    // for this integration check we can reuse the same construction.
    // Skip if wiring an in-process MCP server is heavyweight — this test is
    // focused on CLI vs inbox equivalence; MCP equivalence is already covered
    // by the unit test in Task 8 that asserts Provenance::External shape.

    let a = entry_without_timestamp(&entry_a);
    let b = entry_without_timestamp(&entry_b);
    assert_eq!(
        a, b,
        "inbox and CLI paths produced divergent entries:\n  inbox: {a:#}\n  CLI  : {b:#}"
    );

    // Pin the finding_category default behavior: inbox supplies it, CLI doesn't
    // (FeedbackOpts has no --category field today). Both should end up with
    // "security" since inbox provides it explicitly; CLI inherits from... wait,
    // CLI has no category arg. Document this gap: CLI falls back to "unknown".
    // If that's intended, this test should pass only with an explicit --category
    // flag added, OR we accept the divergence as intentional.
    //
    // ACTION: if this test fails because of finding_category mismatch, the
    // correct fix depends on product intent:
    //   (a) add --category flag to FeedbackOpts (small scope increase)
    //   (b) rewrite the test to normalize finding_category before comparison
    //       and document CLI as "category-less; gets 'unknown'"
    // Prefer (a) — it closes the gap permanently.
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test --test cross_path_equivalence
```
Expected: FAIL. The mismatch will most likely be on `finding_category` (inbox has "security", CLI has "unknown") — confirming the action item in the test comment.

**Step 3: Resolve the divergence**

Add a `--category` flag to `FeedbackOpts`:

```rust
/// Finding category (e.g. "security", "correctness"). Defaults to "unknown".
#[arg(long)]
pub category: Option<String>,
```

Thread through `run_feedback` into both the Human and External paths.

**Step 4: Run test to verify it passes**

```bash
cargo test --test cross_path_equivalence
```
Expected: PASS.

**Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs tests/cross_path_equivalence.rs
git commit -m "feat(cli): add --category flag + pin cross-path equivalence (issue #32)"
```

---

## Task 9: Stats — add tier-level aggregation alongside existing per-source stats

**Important scope note from code inspection:** `src/analytics.rs::compute_stats` aggregates by `entry.model.as_deref()` (the *reviewer* model, e.g. `"gpt-5.4"`), NOT by provenance tier. The existing `SourceStats` has fields `tp, fp, partial, wontfix` — it's per-source TP/FP, not per-tier.

This task ADDS a new parallel function `compute_tier_stats` that groups by provenance tier (Human / PostFix / AutoCalibrate / External / Unknown) and returns `SourceStats` per tier. For External, the "source" key is the agent name, so `compute_tier_stats` returns a `TierSummary` struct that has a nested per-agent breakdown only for the External tier.

**Files:**
- Modify: `src/analytics.rs` (add new types + `compute_tier_stats` + formatter)
- Modify: `src/main.rs::run_stats` (or its helper — grep to locate) to print the new summary block
- Test: `src/analytics.rs` inline tests

**Step 1: Write failing tier-aggregator tests**

Add to `src/analytics.rs::tests` (reuse the existing `fn entry(...)` helper at line 143 — note it hardcodes `provenance: Provenance::Unknown`, so write a new `fn entry_with(provenance, verdict) -> FeedbackEntry` variant rather than mutating the existing one):

```rust
fn entry_with(provenance: crate::feedback::Provenance, verdict: Verdict) -> FeedbackEntry {
    FeedbackEntry {
        file_path: "a.rs".into(),
        finding_title: "t".into(),
        finding_category: "c".into(),
        verdict,
        reason: "r".into(),
        model: Some("gpt-5.4".into()),
        timestamp: chrono::Utc::now(),
        provenance,
    }
}

#[test]
fn tier_stats_group_by_provenance() {
    use crate::feedback::Provenance;
    let fb = vec![
        entry_with(Provenance::Human, Verdict::Tp),
        entry_with(Provenance::Human, Verdict::Fp),
        entry_with(Provenance::PostFix, Verdict::Tp),
        entry_with(Provenance::External { agent: "pal".into(), model: None, confidence: None }, Verdict::Tp),
        entry_with(Provenance::External { agent: "pal".into(), model: None, confidence: None }, Verdict::Fp),
        entry_with(Provenance::External { agent: "third-opinion".into(), model: None, confidence: None }, Verdict::Tp),
    ];
    let summary = compute_tier_stats(&fb);
    assert_eq!(summary.human.total(), 2);
    assert_eq!(summary.human.tp, 1);
    assert_eq!(summary.human.fp, 1);
    assert_eq!(summary.post_fix.total(), 1);
    assert_eq!(summary.external.total.total(), 3);
    assert_eq!(summary.external.total.tp, 2);
    assert_eq!(summary.external.total.fp, 1);
    // Per-agent breakdown, sorted desc by count
    assert_eq!(summary.external.per_agent[0].0, "pal");
    assert_eq!(summary.external.per_agent[0].1.total(), 2);
    assert_eq!(summary.external.per_agent[1].0, "third-opinion");
    assert_eq!(summary.external.per_agent[1].1.total(), 1);
}

#[test]
fn tier_stats_format_shows_external_and_top_agents_stable() {
    // Struct-level assertions for the data (stable contract);
    // one stable regex for the sub-line format (brittle if we ever localize labels).
    use crate::feedback::Provenance;
    let fb = vec![
        entry_with(Provenance::External { agent: "pal".into(), model: None, confidence: None }, Verdict::Tp),
        entry_with(Provenance::External { agent: "pal".into(), model: None, confidence: None }, Verdict::Tp),
        entry_with(Provenance::External { agent: "third-opinion".into(), model: None, confidence: None }, Verdict::Fp),
    ];
    let summary = compute_tier_stats(&fb);

    // --- Data contract (stable) ---
    assert_eq!(summary.external.total.total(), 3);
    assert_eq!(summary.external.per_agent.len(), 2);
    assert_eq!(summary.external.per_agent[0].0, "pal");
    assert_eq!(summary.external.per_agent[1].0, "third-opinion");

    // --- Format contract (minimal, single regex for the sub-line) ---
    let report = format_tier_report(&summary);
    let re = regex::Regex::new(r"top agents:\s+pal\s*\(\d+\).*third-opinion\s*\(\d+\)").unwrap();
    assert!(re.is_match(&report),
        "sub-line format must list agents with counts: {report}");
}

#[test]
fn format_tier_report_handles_zero_external_entries() {
    // Zero externals → no "top agents:" sub-line (not "top agents: " empty).
    use crate::feedback::Provenance;
    let fb = vec![entry_with(Provenance::Human, Verdict::Tp)];
    let summary = compute_tier_stats(&fb);
    assert_eq!(summary.external.total.total(), 0);
    assert!(summary.external.per_agent.is_empty());

    let report = format_tier_report(&summary);
    assert!(!report.contains("top agents:"),
        "must not emit empty 'top agents:' when no external entries: {report}");
    // External row itself is still present, just with zero counts.
    assert!(report.contains("External"),
        "External row should still appear (with 0 total): {report}");
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum analytics::tests::tier_stats_
```
Expected: compile error — `compute_tier_stats`, `format_tier_report`, `TierSummary` undefined.

**Step 3: Implement the new API**

Add to `src/analytics.rs` (below the existing `compute_stats` block, NOT replacing it):

```rust
/// Tier-level aggregation of feedback entries by `Provenance`.
/// Parallel to (and does not replace) `compute_stats`, which aggregates by reviewer model.
#[derive(Debug, Clone, Default)]
pub struct TierSummary {
    pub human: SourceStats,
    pub post_fix: SourceStats,
    pub auto_calibrate: SourceStats,
    pub external: ExternalTierStats,
    pub unknown: SourceStats,
}

#[derive(Debug, Clone, Default)]
pub struct ExternalTierStats {
    pub total: SourceStats,
    /// Per-agent breakdown, sorted desc by total count.
    pub per_agent: Vec<(String, SourceStats)>,
}

pub fn compute_tier_stats(entries: &[FeedbackEntry]) -> TierSummary {
    use crate::feedback::Provenance;
    let mut summary = TierSummary::default();
    let mut per_agent: std::collections::HashMap<String, SourceStats> = Default::default();

    let bump = |s: &mut SourceStats, v: &Verdict| match v {
        Verdict::Tp => s.tp += 1,
        Verdict::Fp => s.fp += 1,
        Verdict::Partial => s.partial += 1,
        Verdict::Wontfix => s.wontfix += 1,
        Verdict::ContextMisleading { .. } => {}  // excluded — retrieval signal, not finding verdict
    };

    for entry in entries {
        match &entry.provenance {
            Provenance::Human => bump(&mut summary.human, &entry.verdict),
            Provenance::PostFix => bump(&mut summary.post_fix, &entry.verdict),
            Provenance::AutoCalibrate(_) => bump(&mut summary.auto_calibrate, &entry.verdict),
            Provenance::External { agent, .. } => {
                bump(&mut summary.external.total, &entry.verdict);
                bump(per_agent.entry(agent.clone()).or_default(), &entry.verdict);
            }
            Provenance::Unknown => bump(&mut summary.unknown, &entry.verdict),
        }
    }

    let mut agents: Vec<_> = per_agent.into_iter().collect();
    agents.sort_by(|a, b| b.1.total().cmp(&a.1.total()));
    summary.external.per_agent = agents;
    summary
}

pub fn format_tier_report(summary: &TierSummary) -> String {
    let mut lines = Vec::new();
    lines.push("Feedback by provenance tier:".into());
    lines.push("-".repeat(65));
    let rows: [(&str, &SourceStats); 4] = [
        ("Human      ", &summary.human),
        ("PostFix    ", &summary.post_fix),
        ("External   ", &summary.external.total),
        ("AutoCalib  ", &summary.auto_calibrate),
    ];
    for (label, s) in rows {
        lines.push(format!(
            "{label}: {:>5} total  (tp {:>3}  fp {:>3}  partial {:>2}  wontfix {:>2})  {:>5.0}% prec",
            s.total(), s.tp, s.fp, s.partial, s.wontfix, s.precision() * 100.0
        ));
    }
    if !summary.external.per_agent.is_empty() {
        let top: Vec<String> = summary.external.per_agent.iter().take(3)
            .map(|(name, s)| format!("{name} ({})", s.total()))
            .collect();
        lines.push(format!("    top agents: {}", top.join(", ")));
    }
    if summary.unknown.total() > 0 {
        let s = &summary.unknown;
        lines.push(format!(
            "Unknown    : {:>5} total  (legacy rows with no provenance field)",
            s.total()
        ));
    }
    lines.join("\n")
}
```

**Step 4: Wire into `run_stats`**

```bash
rg -n 'format_stats_report' src/main.rs
```
Find the print site and add a second block right after the existing per-source report:

```rust
let tier_summary = crate::analytics::compute_tier_stats(&feedback);
println!("\n{}", crate::analytics::format_tier_report(&tier_summary));
```

**Step 5: Run tests to verify they pass**

```bash
cargo test --bin quorum analytics::tests::tier_stats_
```
Expected: both PASS.

**Step 6: Commit**

```bash
git add src/analytics.rs src/main.rs
git commit -m "feat(stats): add tier-level summary with External + top agents (issue #32)"
```

---

## Task 10: Verification + docs + MEMORY update

**Step 1: Full test suite**

```bash
cargo test --bin quorum
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```
Expected: all green.

**Step 2: Update `CLAUDE.md` Feedback section**

Add line after the existing Verdicts/Provenance list:

```markdown
External provenance (External { agent, model, confidence }) is weighted 0.7x by the calibrator.
Drop JSONL files into `~/.quorum/inbox/` to ingest verdicts from other review agents (pal, third-opinion, etc.).
Drained files are archived to `~/.quorum/inbox/processed/` — manually clean up when a size warning surfaces.
```

**Step 3: Update MEMORY.md**

Add a line under "Future Work":
```markdown
- [Issue #32](issue_32_external_feedback.md) — External feedback ingestion shipped; watch calibration precision over 30 days to decide if 0.7x needs tuning.
```

Create the memory file:
```markdown
---
name: Issue #32 External feedback ingestion
description: Shipped ingestion of verdicts from pal/third-opinion/etc. at 0.7x calibrator weight
type: project
---

External verdicts from other review agents (pal, third-opinion, gemini, reviewdog) now flow into quorum's feedback store at 0.7x calibrator weight.

**Why:** Cross-agent precedent accelerates calibration without human triage. The 0.7x sits between Human (1.0x) and AutoCalibrate (0.5x) — a different model weighing in avoids the self-verification failure mode that sank AutoCalibrate in v0.11.0.

**How to apply:** When running non-quorum review tools on code quorum has already reviewed, record verdicts via `quorum feedback --from-agent <name>`, the MCP `feedback` tool with `from_agent`, or by dropping ExternalVerdictInput JSONL into `~/.quorum/inbox/`. Watch the External tier in `quorum stats` and revisit the 0.7x weight after 30 days of real data.
```

**Step 4: Commit**

```bash
git add CLAUDE.md ~/.claude/projects/-Users-jsnyder-Sources-github-com-jsnyder-quorum/memory/
git commit -m "docs(feedback): document External provenance ingestion (issue #32)"
```

---

## Execution notes

- **Routing:** For any Rust type/borrow/async issue during implementation, delegate to `rust-expert` rather than flailing. For test strategy doubts, consult `testing-antipatterns-expert`.
- **Commit discipline:** One commit per task. Don't bundle.
- **Never weaken a test:** If a test fails unexpectedly, diagnose. Don't remove assertions or relax thresholds to make CI green.
- **Quorum self-review:** At the end (Phase 6 of /dev:start), run `quorum review` on all changed files. Triage findings into in-branch (fix) vs pre-existing (file issue).
- **Self-ingestion dogfood:** Phase 7 — record verdicts via the new `--from-agent quorum` path to exercise the External ingestion end-to-end.
