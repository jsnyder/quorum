# Precision Improvements & Calibrator Tracing Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve review precision from 55% to 65%+ by tightening LLM prompts and wontfix suppression, and add structured tracing to enable data-driven calibrator tuning.

**Architecture:** Four sequential tasks: (1) tighten the LLM review prompt to reject stylistic findings, (2) let wontfix feedback contribute to full suppression at 50% weight, (3) add a CalibratorTraceEntry struct that captures per-finding decision data, (4) wire `tracing` crate for structured JSON logging of calibrator and few-shot decisions. Each task follows RED-GREEN TDD.

**Tech Stack:** Rust, cargo test, tracing + tracing-subscriber + tracing-appender crates, serde, tempfile

---

## Test Design Guidelines (from antipatterns analysis)

- **No snapshot tests for prompt strings** -- use targeted `contains`/`!contains` assertions. Snapshots over-couple to wording and break on any tweak.
- **Test behavior, not implementation** -- calibrator tests should assert on `calibrator_action` and `severity` outcomes, not internal weight variables.
- **Use the existing `FindingBuilder` and `fb()` helpers** -- don't create new test factories. Reuse `calibrator.rs:337-348` helper.
- **Keep tracing tests focused on struct population** -- don't test log output strings. Test that `CalibrationResult.traces` contains the right data.
- **No flaky timing tests** -- tracing tests verify structure, not timing.
- **One behavior per test** -- each test name describes the scenario and expected outcome.

---

### Task 1: Tighten LLM Review Prompt

**Files:**
- Modify: `src/review.rs:64-66` (prompt intro)
- Modify: `src/review.rs:105-108` (FP precedent instructions)
- Test: `src/review.rs` (inline tests)

**Step 1: Write failing test for exclusion of stylistic findings**

Add to `src/review.rs` tests module:

```rust
#[test]
fn build_prompt_excludes_stylistic_findings() {
    let req = ReviewRequest {
        file_path: "src/auth.rs".into(),
        language: "rust".into(),
        code: "fn login() {}".into(),
        hydration_context: None,
        framework_docs: None,
        feedback_precedents: None,
        truncation_notice: None,
    };
    let prompt = build_review_prompt(&req);
    assert!(prompt.contains("bugs"));
    assert!(prompt.contains("security"));
    assert!(!prompt.contains("code quality problems"));
    assert!(prompt.contains("Do NOT flag"));
    assert!(prompt.contains("stylistic"));
}
```

**Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum build_prompt_excludes_stylistic_findings`
Expected: FAIL -- prompt currently contains "code quality problems" and lacks "Do NOT flag"

**Step 3: Update prompt intro text**

In `src/review.rs:64-66`, change the prompt format string:

```rust
let mut prompt = format!(
    "Review the following {} code from `{}` for bugs, security vulnerabilities, \
     logic errors, and architectural flaws.\n\
     Do NOT flag stylistic preferences, naming conventions, formatting issues, \
     or missing documentation.\n\n",
    req.language, req.file_path
);
```

**Step 4: Run test to verify it passes**

Run: `rtk cargo test --bin quorum build_prompt_excludes_stylistic_findings`
Expected: PASS

**Step 5: Write failing test for hardened FP precedent instructions**

```rust
#[test]
fn build_prompt_fp_precedents_are_hard_negative() {
    let req = ReviewRequest {
        file_path: "src/auth.rs".into(),
        language: "rust".into(),
        code: "fn login() {}".into(),
        hydration_context: None,
        framework_docs: None,
        feedback_precedents: Some(vec![
            "[FALSE POSITIVE] states() without check: HA safe form".into(),
        ]),
        truncation_notice: None,
    };
    let prompt = build_review_prompt(&req);
    assert!(prompt.contains("MUST NOT flag"));
    assert!(prompt.contains("FALSE POSITIVE precedent"));
}
```

**Step 6: Run test to verify it fails**

Run: `rtk cargo test --bin quorum build_prompt_fp_precedents_are_hard_negative`
Expected: FAIL -- current text says "Use them to understand project-specific patterns"

**Step 7: Harden the precedent instruction text**

In `src/review.rs`, the `feedback_precedents` block (around line 105), change the instruction text:

```rust
prompt.push_str("## Historical Review Findings\n");
prompt.push_str("The following are human-verified findings from past reviews of similar code. ");
prompt.push_str("CRITICAL: If the code matches a FALSE POSITIVE precedent, you MUST NOT flag it. ");
prompt.push_str("TRUE POSITIVE precedents show real issues -- look for similar patterns. ");
prompt.push_str("Do NOT limit your review to only these topics.\n\n");
```

**Step 8: Run test to verify it passes**

Run: `rtk cargo test --bin quorum build_prompt_fp_precedents_are_hard_negative`
Expected: PASS

**Step 9: Run existing prompt tests to verify no regressions**

Run: `rtk cargo test --bin quorum build_prompt`
Expected: All prompt tests pass (update `build_prompt_includes_code_and_path` if it asserts on old wording)

**Step 10: Fix any broken existing tests**

The test `build_prompt_includes_code_and_path` (line 332) likely asserts `prompt.contains("bugs")` which still holds. But `build_prompt_includes_feedback_precedents` (line 497) may assert on old text "Use them to understand". Update that assertion to match new wording.

**Step 11: Commit**

```bash
git add src/review.rs
git commit -m "feat: tighten LLM prompt to reject stylistic findings and harden FP precedent instructions"
```

---

### Task 2: Wontfix Contributes to Full Suppression

**Files:**
- Modify: `src/calibrator.rs:130-145` (calibrate fn suppress logic)
- Modify: `src/calibrator.rs:255-265` (calibrate_with_index fn suppress logic)
- Test: `src/calibrator.rs` (inline tests)

**Step 1: Write failing test -- wontfix contributes to full suppress with FP**

```rust
#[test]
fn wontfix_contributes_to_full_suppress_with_fp() {
    // FP weight alone = 1.0 (below 1.5 threshold)
    // Wontfix at 50% adds 0.5 -> total = 1.5 (hits threshold)
    let finding = FindingBuilder::new()
        .title("Missing explicit mode")
        .category("quality")
        .severity(Severity::Medium)
        .build();

    let feedback = vec![
        fb("Missing explicit mode", "quality", Verdict::Fp),
        fb("Missing explicit mode defaults", "quality", Verdict::Wontfix),
        fb("No explicit mode set", "quality", Verdict::Wontfix),
    ];

    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);
    // With wontfix contributing at 50%, combined weight should suppress
    assert_eq!(result.suppressed, 1);
    assert!(result.findings.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum wontfix_contributes_to_full_suppress_with_fp`
Expected: FAIL -- currently wontfix doesn't contribute to full suppression

**Step 3: Write failing test -- wontfix alone still insufficient for full suppress**

```rust
#[test]
fn wontfix_alone_insufficient_for_full_suppress() {
    // Even with high wontfix weight, without FP, no full suppression
    let finding = FindingBuilder::new()
        .title("No explicit mode")
        .category("quality")
        .severity(Severity::Medium)
        .build();

    let feedback = vec![
        fb("No explicit mode", "quality", Verdict::Wontfix),
        fb("No explicit mode set", "quality", Verdict::Wontfix),
        fb("Missing explicit mode", "quality", Verdict::Wontfix),
        fb("Automation has no mode", "quality", Verdict::Wontfix),
    ];

    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);
    // Wontfix alone should soft-suppress (demote to Info) but not fully suppress
    assert_eq!(result.suppressed, 0);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, Severity::Info);
}
```

**Step 4: Run test to verify it fails (or passes if behavior already correct)**

Run: `rtk cargo test --bin quorum wontfix_alone_insufficient_for_full_suppress`
Expected: May pass already -- wontfix alone already only soft-suppresses. If it passes, good -- this is a guard rail test.

**Step 5: Implement wontfix contribution to full suppression**

In `src/calibrator.rs`, modify the full suppress check in BOTH `calibrate` and `calibrate_with_index` functions. Change from:

```rust
// Full suppress: only strict FP weight (no wontfix contribution)
if fp_weight >= 1.5 && fp_weight > tp_weight * 2.0 {
```

To:

```rust
// Full suppress: FP weight + wontfix at 50% contribution
let full_suppress_weight = fp_weight + (wontfix_weight * 0.5);
if full_suppress_weight >= 1.5 && fp_weight > 0.0 && full_suppress_weight > tp_weight * 2.0 {
```

The `fp_weight > 0.0` guard ensures wontfix alone cannot trigger full suppression -- there must be at least some FP signal.

**Step 6: Run new tests to verify they pass**

Run: `rtk cargo test --bin quorum wontfix_contributes_to_full_suppress`
Run: `rtk cargo test --bin quorum wontfix_alone_insufficient`
Expected: Both PASS

**Step 7: Run full calibrator test suite for regressions**

Run: `rtk cargo test --bin quorum calibrat`
Expected: All ~25 existing tests pass. The test `wontfix_only_soft_suppresses_not_full` (line 901) should still pass because it has no FP entries.

**Step 8: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat: wontfix contributes to full suppression at 50% weight (requires FP corroboration)"
```

---

### Task 3: Calibrator Trace Entries

**Files:**
- Create: `src/calibrator_trace.rs`
- Modify: `src/calibrator.rs:9-13` (CalibrationResult struct)
- Modify: `src/calibrator.rs` (both calibrate fns to populate traces)
- Modify: `src/main.rs` or `src/lib.rs` (add `mod calibrator_trace;`)
- Test: `src/calibrator_trace.rs` (struct tests) + `src/calibrator.rs` (trace population tests)

**Step 1: Write failing test for trace struct serialization**

Create `src/calibrator_trace.rs`:

```rust
//! Calibrator decision tracing: structured records of per-finding calibration decisions.

use serde::Serialize;
use crate::finding::{CalibratorAction, Severity};
use crate::feedback::Verdict;

#[derive(Debug, Clone, Serialize)]
pub struct PrecedentTrace {
    pub finding_title: String,
    pub verdict: Verdict,
    pub similarity: f64,
    pub weight: f64,
    pub provenance: String,
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalibratorTraceEntry {
    pub finding_title: String,
    pub finding_category: String,
    pub tp_weight: f64,
    pub fp_weight: f64,
    pub wontfix_weight: f64,
    pub full_suppress_weight: f64,
    pub soft_fp_weight: f64,
    pub matched_precedents: Vec<PrecedentTrace>,
    pub action: Option<CalibratorAction>,
    pub input_severity: Severity,
    pub output_severity: Severity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_entry_serializes_to_json() {
        let trace = CalibratorTraceEntry {
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            tp_weight: 2.5,
            fp_weight: 0.3,
            wontfix_weight: 0.0,
            full_suppress_weight: 0.3,
            soft_fp_weight: 0.3,
            matched_precedents: vec![PrecedentTrace {
                finding_title: "SQL injection via f-string".into(),
                verdict: Verdict::Tp,
                similarity: 0.92,
                weight: 1.5,
                provenance: "human".into(),
                file_path: "src/db.py".into(),
            }],
            action: Some(CalibratorAction::Confirmed),
            input_severity: Severity::Medium,
            output_severity: Severity::High,
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains("\"tp_weight\":2.5"));
        assert!(json.contains("\"similarity\":0.92"));
    }

    #[test]
    fn trace_entry_with_no_precedents() {
        let trace = CalibratorTraceEntry {
            finding_title: "Unused variable".into(),
            finding_category: "quality".into(),
            tp_weight: 0.0,
            fp_weight: 0.0,
            wontfix_weight: 0.0,
            full_suppress_weight: 0.0,
            soft_fp_weight: 0.0,
            matched_precedents: vec![],
            action: None,
            input_severity: Severity::Low,
            output_severity: Severity::Low,
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains("\"matched_precedents\":[]"));
        assert!(json.contains("\"action\":null"));
    }
}
```

**Step 2: Register the module**

Add `pub mod calibrator_trace;` to `src/main.rs` (or lib.rs, wherever modules are declared). Find the existing `mod calibrator;` line and add adjacent to it.

**Step 3: Run test to verify it passes (struct-only, no logic yet)**

Run: `rtk cargo test --bin quorum calibrator_trace`
Expected: PASS -- these just test serialization

**Step 4: Write failing test -- calibrate populates traces for suppressed finding**

Add to `src/calibrator.rs` tests:

```rust
#[test]
fn calibrate_populates_trace_for_suppressed_finding() {
    let finding = FindingBuilder::new()
        .title("Unused import")
        .category("style")
        .severity(Severity::Low)
        .build();

    let feedback = vec![
        fb("Unused import", "style", Verdict::Fp),
        fb("Unused import os", "style", Verdict::Fp),
    ];

    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);
    assert_eq!(result.suppressed, 1);
    assert_eq!(result.traces.len(), 1);

    let trace = &result.traces[0];
    assert_eq!(trace.finding_title, "Unused import");
    assert!(trace.fp_weight > 0.0);
    assert_eq!(trace.action, Some(CalibratorAction::Disputed));
    assert!(!trace.matched_precedents.is_empty());
}
```

**Step 5: Run test to verify it fails**

Run: `rtk cargo test --bin quorum calibrate_populates_trace_for_suppressed`
Expected: FAIL -- `CalibrationResult` has no `traces` field yet

**Step 6: Write failing test -- trace for boosted finding**

```rust
#[test]
fn calibrate_populates_trace_for_boosted_finding() {
    let finding = FindingBuilder::new()
        .title("SQL injection")
        .category("security")
        .severity(Severity::Medium)
        .build();

    let feedback = vec![
        fb("SQL injection", "security", Verdict::Tp),
        fb("SQL injection in query", "security", Verdict::Tp),
    ];

    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);
    assert_eq!(result.traces.len(), 1);

    let trace = &result.traces[0];
    assert_eq!(trace.action, Some(CalibratorAction::Confirmed));
    assert!(trace.tp_weight > 0.0);
    assert_eq!(trace.input_severity, Severity::Medium);
    assert_eq!(trace.output_severity, Severity::High);
}
```

**Step 7: Write failing test -- trace for passthrough finding (no precedents)**

```rust
#[test]
fn calibrate_populates_trace_for_passthrough() {
    let finding = FindingBuilder::new()
        .title("Race condition")
        .category("concurrency")
        .build();

    let feedback = vec![
        fb("Unused import", "style", Verdict::Fp),
    ];

    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.traces.len(), 1);

    let trace = &result.traces[0];
    assert_eq!(trace.finding_title, "Race condition");
    assert_eq!(trace.tp_weight, 0.0);
    assert_eq!(trace.fp_weight, 0.0);
    assert!(trace.matched_precedents.is_empty());
    assert_eq!(trace.action, None);
}
```

**Step 8: Add traces field to CalibrationResult**

In `src/calibrator.rs:9-13`, modify `CalibrationResult`:

```rust
#[derive(Debug, Clone)]
pub struct CalibrationResult {
    pub findings: Vec<Finding>,
    pub suppressed: usize,
    pub boosted: usize,
    pub traces: Vec<crate::calibrator_trace::CalibratorTraceEntry>,
}
```

**Step 9: Implement trace population in `calibrate` function**

In `src/calibrator.rs`, inside the `calibrate` function:

1. Add `let mut traces = Vec::new();` after the other `let mut` declarations.
2. At the start of each finding loop iteration, save `let input_severity = finding.severity.clone();`.
3. For the passthrough case (no similar entries): push a trace with zero weights.
4. Before each `output.push(finding)` and before the `continue` in suppression: build and push a `CalibratorTraceEntry` with the computed weights, matched precedents, action, and severity change.
5. In the `CalibrationResult` return, add `traces`.

**Step 10: Implement trace population in `calibrate_with_index` function**

Same pattern as Step 9 but for the index-based path. The precedent traces include similarity scores from `SimilarEntry.similarity`.

**Step 11: Fix all CalibrationResult construction sites**

Search for `CalibrationResult {` in the codebase -- anywhere it's constructed needs the `traces` field. This includes the early return when feedback is empty.

**Step 12: Run all trace tests**

Run: `rtk cargo test --bin quorum calibrate_populates_trace`
Expected: All 3 new trace tests PASS

**Step 13: Run full calibrator test suite**

Run: `rtk cargo test --bin quorum calibrat`
Expected: All tests pass (existing tests just need `traces` field added to any assertions on CalibrationResult if they destructure it, but most test via `result.findings`/`result.suppressed` which are unchanged)

**Step 14: Commit**

```bash
git add src/calibrator_trace.rs src/calibrator.rs src/main.rs
git commit -m "feat: add CalibratorTraceEntry for per-finding calibration decision tracing"
```

---

### Task 4: Structured Tracing with `tracing` Crate

**Files:**
- Modify: `Cargo.toml` (add tracing dependencies)
- Create: `src/trace_subscriber.rs` (JSON subscriber setup)
- Modify: `src/pipeline.rs:99-163` (instrument few-shot retrieval)
- Modify: `src/pipeline.rs:166-389` (instrument calibrator call and write trace file)
- Modify: `src/main.rs` (init subscriber, add `--trace` flag)
- Test: `src/trace_subscriber.rs` (subscriber init tests)

**Step 1: Add tracing dependencies to Cargo.toml**

```toml
# Structured tracing
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
tracing-appender = "0.2"
```

**Step 2: Create trace subscriber module with test**

Create `src/trace_subscriber.rs`:

```rust
//! Optional JSON trace subscriber for calibrator decision logging.
//! Activated by --trace flag or QUORUM_TRACE=1 env var.
//! Writes to ~/.quorum/trace.jsonl.

use std::path::PathBuf;

/// Initialize the tracing subscriber if tracing is enabled.
/// Returns the guard that must be held for the lifetime of the program.
pub fn init_trace_subscriber(trace_path: Option<PathBuf>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let path = trace_path?;

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .json()
        .with_writer(non_blocking)
        .with_target(false)
        .with_level(true)
        .init();

    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_with_none_returns_none() {
        // Don't actually init subscriber in tests -- just verify API
        let result = init_trace_subscriber(None);
        assert!(result.is_none());
    }

    #[test]
    fn trace_path_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("trace.jsonl");
        // We can't actually init the global subscriber in a test (singleton),
        // so just verify the parent dir creation logic
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        assert!(path.parent().unwrap().exists());
    }
}
```

**Step 3: Register module and run tests**

Add `mod trace_subscriber;` to `src/main.rs`.

Run: `rtk cargo test --bin quorum trace_subscriber`
Expected: PASS

**Step 4: Add tracing instrumentation to few-shot retrieval**

In `src/pipeline.rs`, at the end of `query_feedback_precedents` (around line 155), add before the return:

```rust
tracing::debug!(
    query = %query,
    candidates_found = candidates.len(),
    selected_count = selected.len(),
    selected_precedents = ?selected.iter().map(|s| {
        format!("{} ({:?}, sim={:.2})", s.entry.finding_title, s.entry.verdict, s.similarity)
    }).collect::<Vec<_>>(),
    "Few-shot precedent retrieval"
);
```

**Step 5: Add tracing instrumentation to calibration in review_file**

In `src/pipeline.rs`, after calibration completes in `review_file` (where `CalibrationResult` is returned), add:

```rust
for trace in &cal_result.traces {
    tracing::info!(
        finding = %trace.finding_title,
        category = %trace.finding_category,
        tp_weight = trace.tp_weight,
        fp_weight = trace.fp_weight,
        wontfix_weight = trace.wontfix_weight,
        full_suppress_weight = trace.full_suppress_weight,
        action = ?trace.action,
        input_severity = ?trace.input_severity,
        output_severity = ?trace.output_severity,
        precedent_count = trace.matched_precedents.len(),
        "Calibrator decision"
    );
}
```

**Step 6: Write trace file alongside telemetry**

In `src/pipeline.rs`, after `final_findings` is finalized and before the `Ok(FileReviewResult {...})` return, write the traces to a JSONL file:

```rust
if !cal_result.traces.is_empty() {
    if let Some(store_path) = &pipeline_config.feedback_store {
        let trace_path = store_path.with_file_name("calibrator_traces.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
        {
            use std::io::Write;
            for trace in &cal_result.traces {
                if let Ok(json) = serde_json::to_string(trace) {
                    let _ = writeln!(file, "{}", json);
                }
            }
        }
    }
}
```

**Step 7: Add --trace CLI flag**

In `src/main.rs`, add a `--trace` flag to the review subcommand. When set, initialize the trace subscriber with `~/.quorum/trace.jsonl`.

**Step 8: Run full test suite**

Run: `rtk cargo test --bin quorum`
Expected: All tests pass

**Step 9: Build and verify**

Run: `rtk cargo build`
Expected: Clean compile with tracing wired in

**Step 10: Manual smoke test**

```bash
cargo run -- review src/calibrator.rs --trace 2>/dev/null
cat ~/.quorum/calibrator_traces.jsonl | head -5
```

Expected: JSONL output with per-finding trace entries showing weights and actions.

**Step 11: Commit**

```bash
git add Cargo.toml src/trace_subscriber.rs src/pipeline.rs src/main.rs
git commit -m "feat: add structured tracing for calibrator decisions and few-shot retrieval"
```

---

### Post-Implementation: Version Bump

**Step 1:** Bump version in `Cargo.toml` to 0.11.0 (minor version -- new features)
**Step 2:** Update test count in `CLAUDE.md` if changed
**Step 3:** Build release: `rtk cargo build --release`
**Step 4:** Install and verify: `cargo install --path . && quorum version`
**Step 5:** Commit version bump

---

## Verification Checklist

- [ ] `rtk cargo test --bin quorum` -- all tests pass
- [ ] `rtk cargo test` -- all tests including integration pass
- [ ] `rtk cargo clippy` -- no warnings
- [ ] Prompt no longer contains "code quality problems"
- [ ] FP precedent instruction contains "MUST NOT flag"
- [ ] Wontfix + FP can trigger full suppression
- [ ] Wontfix alone cannot trigger full suppression
- [ ] CalibrationResult has traces field populated
- [ ] `--trace` flag writes to `~/.quorum/trace.jsonl`
- [ ] Calibrator trace JSONL contains tp_weight, fp_weight, action, precedents
