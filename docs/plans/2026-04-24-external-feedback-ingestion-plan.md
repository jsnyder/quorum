# External Feedback Ingestion Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Let quorum ingest TP/FP verdicts from other review agents (pal, third-opinion, gemini, etc.) as a fourth provenance tier weighted at 0.7x in the calibrator.

**Architecture:** Add `Provenance::External { agent, model, confidence }` variant. Three ingestion surfaces — inbox jsonl drop, `quorum feedback --from-agent`, and an extended MCP `feedback` tool — all funnel through a single `FeedbackStore::record_external` constructor. Inbox drain runs in `main.rs` before pipeline/stats so the pipeline module stays IO-pure.

**Tech Stack:** Rust 1.94, serde, chrono, clap v4, ulid, anyhow, tracing, tempfile (tests). No new dependencies — `ulid` crate is already in Cargo.toml.

**Design doc:** `docs/plans/2026-04-24-external-feedback-ingestion-design.md` (read first for rationale and policy thresholds).

**Branch/worktree:** `feat/external-feedback-ingest` (created by /dev:start Phase 2).

**Test discipline (critical):** Every task is RED → GREEN → REFACTOR. Write the failing test first, watch it fail for the *specific reason we expect*, then implement. Never weaken a test to make it pass. Routes to `rust-expert` for borrow/lifetime/async issues; to `testing-antipatterns-expert` if a test smells off (mock overuse, tautology, coverage cosplay).

**Existing test style:** inline `#[cfg(test)] mod tests` at bottom of module. See `src/feedback.rs::tests` and `src/calibrator.rs::tests` for canonical patterns. Use `tempfile::TempDir` for any filesystem state.

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
    assert!(inner.get("confidence").map_or(false, |c| c.is_null()));
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
    Unknown,
}
```

Note: drop `Eq` from derive list if it was there — `Option<f32>` is not `Eq`. Check `src/feedback.rs:27` — the existing derive was `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]`. Removing `Eq` breaks downstream `assert_eq!(entry.provenance, Provenance::Human)` calls only if they rely on `Eq` trait bounds (they use `PartialEq`, so fine). Grep to confirm: `rg 'Provenance:.*Eq'` should only hit the derive itself.

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum feedback::tests::external_ -- --nocapture
```
Expected: both PASS.

**Step 5: Verify no existing test broke**

```bash
cargo test --bin quorum feedback::tests
```
Expected: all PASS (the existing `provenance_serializes_correctly` still works — unit variants unchanged).

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
    // confidence is stored but IGNORED by calibrator in v1
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
    let w_none = verdict_weight(&mk(None));
    let w_low  = verdict_weight(&mk(Some(0.1)));
    let w_high = verdict_weight(&mk(Some(0.99)));
    assert!((w_none - w_low).abs() < 1e-6);
    assert!((w_none - w_high).abs() < 1e-6);
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

Add to `src/calibrator.rs::tests`:

```rust
#[test]
fn external_not_filtered_when_use_auto_feedback_false() {
    let findings = vec![sample_finding("SQL injection", Severity::High)];
    let feedback = vec![FeedbackEntry {
        file_path: "src/auth.rs".into(),
        finding_title: "SQL injection".into(),
        finding_category: "security".into(),
        verdict: Verdict::Fp,
        reason: "not actually user input".into(),
        model: None,
        timestamp: Utc::now(),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: None,
        },
    }];
    let config = CalibratorConfig {
        use_auto_feedback: false,  // would filter AutoCalibrate; External must survive
        ..CalibratorConfig::default()
    };
    let result = calibrate(findings, &feedback, &config);
    // External is seen (not filtered) so the finding either stays or gets soft-severity
    // but NOT "no precedent found" (which would happen if it were filtered out)
    let trace = result.traces.last().expect("expected a calibrator trace");
    assert!(!trace.matched_precedents.is_empty(),
        "External verdict must survive use_auto_feedback=false");
}

#[test]
fn two_external_fps_soft_suppress_not_full() {
    // 2 × 0.7 = 1.4 → soft (>=1.0) but NOT full (<1.5) with tp_weight=0
    let findings = vec![sample_finding("SQL injection", Severity::High)];
    let fb = |ts_offset_days: i64| FeedbackEntry {
        file_path: "src/auth.rs".into(),
        finding_title: "SQL injection".into(),
        finding_category: "security".into(),
        verdict: Verdict::Fp,
        reason: "r".into(),
        model: None,
        timestamp: Utc::now() - chrono::Duration::days(ts_offset_days),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: None,
        },
    };
    let feedback = vec![fb(0), fb(1)];
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    // Finding should still be present but downgraded to Info
    assert_eq!(result.suppressed, 0, "2 externals should NOT full-suppress");
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, Severity::Info,
        "2 external FPs should soft-suppress to Info");
}

#[test]
fn three_external_fps_full_suppress_allowed() {
    // 3 × 0.7 = 2.1 → full suppress (>=1.5) with tp_weight=0
    let findings = vec![sample_finding("SQL injection", Severity::High)];
    let fb = |ts_offset_days: i64| FeedbackEntry {
        file_path: "src/auth.rs".into(),
        finding_title: "SQL injection".into(),
        finding_category: "security".into(),
        verdict: Verdict::Fp,
        reason: "r".into(),
        model: None,
        timestamp: Utc::now() - chrono::Duration::days(ts_offset_days),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: None,
        },
    };
    let feedback = vec![fb(0), fb(1), fb(2)];
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    assert_eq!(result.suppressed, 1, "3 external FPs should full-suppress");
    assert_eq!(result.findings.len(), 0);
}

#[test]
fn external_not_capped_like_auto_calibrate() {
    // AutoCalibrate FP weight is capped at 1.0 via auto_fp_weight.min(1.0).
    // External falls into other_fp_weight which is UNCAPPED.
    // Sanity: if External were capped, 3 externals would cap at 1.0 and NOT full-suppress.
    // This is the inverse of the previous test — co-locate so future regressions surface here.
    let findings = vec![sample_finding("X", Severity::High)];
    let fb_ext = FeedbackEntry {
        file_path: "a.rs".into(),
        finding_title: "X".into(),
        finding_category: "security".into(),
        verdict: Verdict::Fp,
        reason: "r".into(),
        model: None,
        timestamp: Utc::now(),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: None,
        },
    };
    let feedback = vec![fb_ext.clone(), fb_ext.clone(), fb_ext];
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    // Confirm uncapped: 3 × 0.7 = 2.1 ≥ 1.5 → full suppress
    assert_eq!(result.suppressed, 1,
        "External must accumulate uncapped (sum=2.1), not cap at 1.0");
}
```

Note: if `sample_finding` helper doesn't exist in the test module, add it by grepping existing helpers and reusing/adapting. Likely candidates: look for `fn build_finding`, `fn mk_finding`, or similar in `src/calibrator.rs::tests`. If none exist, add one.

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum calibrator::tests::two_external_fps_soft_suppress_not_full \
           calibrator::tests::three_external_fps_full_suppress_allowed \
           calibrator::tests::external_not_filtered_when_use_auto_feedback_false \
           calibrator::tests::external_not_capped_like_auto_calibrate
```
Expected: one or more FAIL. Why: the filter/cap sites correctly pass External through by default, so these tests may actually PASS — which is the point of pinning. If any FAIL, it reveals a real gap.

**If all 4 pass immediately:** good — commit the tests as pinning-only, no code change needed for this task. If any fails, route to `rust-expert` subagent to diagnose why External was unexpectedly filtered or capped.

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

#[test]
fn record_external_clamps_confidence_to_unit_interval() {
    let (store, _dir) = test_store();
    let test_cases = [(Some(1.5), Some(1.0)), (Some(-0.2), Some(0.0)), (Some(0.42), Some(0.42))];
    for (input, expected) in test_cases {
        let dir = tempfile::TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("f.jsonl"));
        store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "pal".into(),
            agent_model: None,
            confidence: input,
        }).unwrap();
        let all = store.load_all().unwrap();
        match &all[0].provenance {
            Provenance::External { confidence, .. } => {
                match (confidence, expected) {
                    (Some(c), Some(e)) => assert!((c - e).abs() < 1e-6, "input={input:?} expected={expected:?} got={c}"),
                    (None, None) => (),
                    other => panic!("mismatch: {other:?}"),
                }
            }
            o => panic!("{o:?}"),
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

Add to `impl FeedbackStore`:

```rust
/// Record a verdict from an external review agent (pal, third-opinion, etc.).
/// Normalizes agent name (lowercase+trim), clamps confidence to [0,1], and
/// sets `provenance = Provenance::External{..}`. See issue #32.
pub fn record_external(&self, input: ExternalVerdictInput) -> anyhow::Result<()> {
    let agent = input.agent.trim().to_lowercase();
    if agent.is_empty() {
        anyhow::bail!("agent name cannot be empty after normalization");
    }
    let confidence = input.confidence.map(|c| c.clamp(0.0, 1.0));
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
fn drain_inbox_enoent_race_is_not_an_error() {
    // Simulate a two-process race: we stat-and-enumerate a file that no longer
    // exists when we try to rename. ENOENT on rename must be silently tolerated
    // (another process beat us to it). Emulate by listing a phantom filename
    // via a custom scenario: create + delete before rename.
    // Simplest emulation: create a file, then delete it before calling drain
    // in-process is racy. Instead, test the error-path contract by inserting
    // a bogus file, deleting mid-drain is hard to guarantee portably, so
    // assert a weaker property: drain is idempotent — running it twice on
    // the same-inbox-now-empty state yields zero work without panicking.
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

        // Fast path
        if !inbox_dir.exists() {
            return Ok(report);
        }
        let mut entries_iter = std::fs::read_dir(inbox_dir)?;
        if entries_iter.next().is_none() {
            return Ok(report);
        }
        // Re-open the iterator (we consumed one entry above just to peek)
        let read = std::fs::read_dir(inbox_dir)?;

        let mut files: Vec<PathBuf> = read
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
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
            // Move to processed/
            let fname = file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown.jsonl");
            let ulid = ulid::Ulid::new().to_string();
            let target = processed_dir.join(format!("{fname}.{ulid}.jsonl"));
            match std::fs::rename(&file, &target) {
                Ok(_) => {
                    if let Ok(meta) = std::fs::metadata(&target) {
                        report.processed_bytes += meta.len();
                    }
                    report.drained_files += 1;
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    // race with another process — fine
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

**Step 4: Implement the hook**

In `src/main.rs`, after argument parsing, before dispatching to `run_review` / `run_stats`:

```rust
// Drain agent-contributed verdicts before loading feedback store.
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

Grep for the exact helper name — use `crate::dirs::quorum_dir()` or whatever the existing helper is for `~/.quorum`. Confirm with `rg 'fn quorum_dir|fn dirs_path' src/`.

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
    Command::cargo_bin("quorum").unwrap()
        .env("HOME", home)
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
fn from_agent_conflicts_with_provenance_flag() {
    let home = TempDir::new().unwrap();
    run_feedback(home.path(), &[
        "--file", "a.rs",
        "--finding", "X",
        "--verdict", "tp",
        "--reason", "r",
        "--from-agent", "pal",
        "--provenance", "human",
    ])
    .failure()
    .stderr(predicates::str::contains("cannot be used with"));
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

In `src/cli.rs`, find `struct FeedbackOpts` (grep: `rg 'struct FeedbackOpts' src/`). Add:

```rust
/// Record the verdict as coming from an external review agent.
#[arg(long, conflicts_with = "provenance")]
pub from_agent: Option<String>,

/// Optional: the LLM model the external agent used.
#[arg(long, requires = "from_agent")]
pub agent_model: Option<String>,

/// Optional: agent-reported confidence, clamped to [0,1]. Ignored by calibrator in v1.
#[arg(long, requires = "from_agent")]
pub confidence: Option<f32>,
```

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

Extend `src/mcp/handler.rs::tests` (grep for existing `fn handle_feedback` tests):

```rust
#[test]
fn mcp_from_agent_writes_external_provenance() {
    let (handler, _dir) = test_handler();  // assume existing helper; if not, follow existing test pattern
    let params = FeedbackTool {
        file: "src/a.rs".into(),
        finding: "SQL injection".into(),
        verdict: "tp".into(),
        reason: "confirmed".into(),
        category: None,
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
    let (handler, _dir) = test_handler();
    let params = FeedbackTool {
        file: "src/a.rs".into(),
        finding: "Bug".into(),
        verdict: "tp".into(),
        reason: "r".into(),
        category: None,
        from_agent: None,
        agent_model: None,
        confidence: None,
    };
    handler.handle_feedback(params).unwrap();
    let all = handler.feedback_store.load_all().unwrap();
    assert_eq!(all[0].provenance, crate::feedback::Provenance::Human);
}
```

If `test_handler` helper doesn't exist, create it (following the existing MCP test patterns).

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

## Task 9: Stats — External tier row + top agents

**Files:**
- Modify: `src/analytics.rs` (feedback-tier aggregation)
- Modify: `src/main.rs::run_stats` or its helper (output formatting)
- Test: `src/analytics.rs` inline tests

**Step 1: Locate the existing tier aggregator**

```bash
rg -n 'AutoCalibrate|PostFix|Human' src/analytics.rs | head -20
```

Identify the struct/function that counts entries per provenance tier and the print path.

**Step 2: Write failing aggregator test**

Add to `src/analytics.rs::tests`:

```rust
#[test]
fn tier_summary_counts_external_and_top_agents() {
    let fb = vec![
        mk_entry(Provenance::External { agent: "pal".into(), model: None, confidence: None }, Verdict::Tp),
        mk_entry(Provenance::External { agent: "pal".into(), model: None, confidence: None }, Verdict::Fp),
        mk_entry(Provenance::External { agent: "third-opinion".into(), model: None, confidence: None }, Verdict::Tp),
        mk_entry(Provenance::Human, Verdict::Tp),
    ];
    let summary = build_tier_summary(&fb);  // or whatever existing helper is named
    assert_eq!(summary.external.total, 3);
    assert_eq!(summary.external.tp, 2);
    assert_eq!(summary.external.fp, 1);
    assert_eq!(summary.external.top_agents[0].0, "pal");
    assert_eq!(summary.external.top_agents[0].1, 2);
    assert_eq!(summary.human.total, 1);
}
```

Fill in `mk_entry` and `build_tier_summary` per existing helpers — grep for similar patterns in `src/analytics.rs::tests`.

**Step 3: Run test to verify it fails**

```bash
cargo test --bin quorum analytics::tests::tier_summary_counts_external_and_top_agents
```
Expected: FAIL — External tier not aggregated yet.

**Step 4: Extend the aggregator**

Add an `external: TierCounts` field (or whatever the existing struct is) with `tp/fp/partial/wontfix/total` and a `top_agents: Vec<(String, usize)>` field (sorted desc by count, capped at 3). Extend the match on provenance to populate it.

Sample code skeleton (adapt to actual aggregator shape):

```rust
#[derive(Default)]
pub struct TierCounts {
    pub total: usize, pub tp: usize, pub fp: usize, pub partial: usize, pub wontfix: usize,
}

#[derive(Default)]
pub struct ExternalTierCounts {
    pub counts: TierCounts,
    pub top_agents: Vec<(String, usize)>,
}

// in build_tier_summary:
let mut agent_counts: std::collections::HashMap<String, usize> = Default::default();
for entry in feedback {
    match &entry.provenance {
        Provenance::External { agent, .. } => {
            *agent_counts.entry(agent.clone()).or_insert(0) += 1;
            summary.external.counts.total += 1;
            bump_verdict(&mut summary.external.counts, &entry.verdict);
        }
        // ...existing arms
    }
}
let mut agents: Vec<_> = agent_counts.into_iter().collect();
agents.sort_by(|a, b| b.1.cmp(&a.1));
summary.external.top_agents = agents.into_iter().take(3).collect();
```

**Step 5: Update stats print path**

Find the feedback-tier print block in `src/main.rs::run_stats` (or helper). Add an "External" row after `PostFix` with count + verdict breakdown + a sub-line `    top agents: pal (142), third-opinion (43), gemini (22)` when `top_agents` is non-empty.

**Step 6: Run test to verify it passes**

```bash
cargo test --bin quorum analytics::tests::tier_summary_counts_external_and_top_agents
```
Expected: PASS.

**Step 7: Commit**

```bash
git add src/analytics.rs src/main.rs
git commit -m "feat(stats): surface External tier + top agents (issue #32)"
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
