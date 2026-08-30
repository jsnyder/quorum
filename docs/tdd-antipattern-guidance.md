# TDD Anti-Pattern Guidance for Quorum Features

## Overall Assessment

Your existing test suite is healthy: inline `#[cfg(test)]` modules, good calibrator coverage (~25 tests), behavioral prompt assertions. The main risks for these 4 features are **testing internal implementation** (#5), **snapshot abuse** (#15), and **over-mocking** (#19).

---

## Feature 1: Tighten LLM Prompt (review.rs)

### Anti-patterns to avoid

**Do NOT use insta snapshots for prompt strings.** This is Snapshot Abuse (#15). The prompt is long and changes frequently. Snapshot tests on the full prompt become rubber-stamp `--update` targets. Your existing pattern of targeted `assert!(prompt.contains(...))` / `assert!(!prompt.contains(...))` is correct.

**Do NOT test exact wording.** That couples tests to implementation (#5). Test semantic intent:

```rust
// GOOD: Tests the behavioral contract
assert!(prompt.contains("Do not report"));
assert!(!prompt.contains("naming conventions"));

// BAD: Tests exact prose (breaks on any wordsmithing)
assert_eq!(prompt, "You are a code reviewer. Focus on bugs...");
```

**Do NOT test prompt assembly mechanics.** Test what the prompt includes/excludes, not how it gets built.

### Recommended test granularity

- One test per exclusion category (formatting, naming, style) verifying the phrase is absent
- One test verifying the FP precedent section uses hard constraint language ("Do NOT report", "must not", etc.)
- One test verifying a known FP precedent appears with constraint framing, not soft guidance
- Keep tests alongside existing `test_build_prompt_*` tests in review.rs

---

## Feature 2: Wontfix Suppress Contribution (calibrator.rs)

### Anti-patterns to avoid

**Do NOT test threshold arithmetic directly.** Test the *decision outcome*, not the weight calculation internals (#5). If you later change from 50% to 40% weight, tests should only break if the decision boundary moves.

```rust
// GOOD: Tests the behavioral outcome
#[test]
fn wontfix_contributes_to_full_suppress() {
    // 2 FP (weight 2.0) + 2 wontfix (weight 1.0) = 3.0 effective, threshold 1.5
    let result = calibrate(finding, &[fp1, fp2, wontfix1, wontfix2]);
    assert!(result.suppressed);
}

// BAD: Tests the internal math
assert_eq!(wontfix_contribution, 0.5);
assert_eq!(total_fp_weight, 3.0);
```

**Do NOT duplicate setup across tests.** Your calibrator tests likely already have helper functions for building FeedbackEntry values. Reuse them. Violating this is Anti-pattern #9 (test code as second-class).

### Recommended tests (3 new, minimum)

| Test | Scenario | Expected |
|------|----------|----------|
| `wontfix_contributes_to_full_suppress` | 2 FP + 2 wontfix, combined exceeds threshold | `suppressed = true` |
| `wontfix_alone_insufficient_for_full_suppress` | 3 wontfix, no FP, below threshold | `suppressed = false`, severity demoted to INFO |
| `mixed_fp_wontfix_at_boundary` | 1 FP + 1 wontfix, right at threshold edge | Verify which side of the boundary it falls on |

### Test granularity

Stay at the `calibrate()` function level. These are unit tests of a pure(ish) function -- the right level for pyramid-style testing of business logic.

---

## Feature 3: Calibrator Tracing (calibrator_trace.rs + calibrator.rs)

### Anti-patterns to avoid

**Do NOT create assertion-free tests (#16).** It is tempting to just call `calibrate()` and check that `traces.len() > 0`. Every test must assert on trace *content*.

**Do NOT assert on every field of CalibratorTraceEntry (#5).** Assert on the fields that define the decision path, not every similarity score decimal. Use approximate matching for floats:

```rust
// GOOD: Asserts on decision-relevant fields
assert_eq!(trace.action, CalibratorAction::Suppress);
assert!(trace.fp_weight > 1.5);
assert_eq!(trace.matched_precedents.len(), 2);

// BAD: Brittle exact-match on float
assert_eq!(trace.matched_precedents[0].similarity, 0.8723456);
```

**Consider insta snapshots ONLY for the trace struct shape**, not values. If you use insta here, use `assert_json_snapshot!` with redactions for volatile fields:

```rust
insta::assert_json_snapshot!(trace, {
    ".matched_precedents[].similarity" => "[similarity]",
    ".tp_weight" => insta::rounded_redaction(2),
    ".fp_weight" => insta::rounded_redaction(2),
});
```

### Recommended test granularity

One test per calibrator path:

| Path | Verify trace contains |
|------|----------------------|
| Passthrough (no feedback) | `action = Passthrough`, empty precedents |
| Full suppress | `action = Suppress`, fp_weight > threshold, precedent list |
| Soft suppress | `action = SoftSuppress`, wontfix precedents present |
| Boost | `action = Boost`, tp_weight, boosted severity |
| Confirmed | `action = Confirmed`, tp precedents |

---

## Feature 4: Pipeline Tracing (pipeline.rs + telemetry.rs)

### Anti-patterns to avoid

**This is the highest-risk feature for anti-patterns.** Tracing output is implementation detail. Testing it wrong creates massive maintenance burden.

**Do NOT parse tracing JSON output and assert on exact structure (#5, #19).** This couples tests to the `tracing` crate's JSON format, span names, and field ordering. Any subscriber config change breaks everything.

**Do NOT test that tracing calls happen by mocking the subscriber (#19).** Over-mocking the tracing layer means you are testing that you called `tracing::info!()`, not that your system works.

### What to test instead

1. **Test the data being traced, not the trace emission.** The CalibratorTraceEntry from Feature 3 is the testable artifact. The tracing layer just serializes it -- trust the `tracing` crate to do its job.

2. **For integration tests**, use `tracing-test` or `tracing_subscriber::fmt::TestWriter` to capture output, then assert on key fields only:

```rust
// GOOD: Verify key fields exist in trace output
let output = captured_trace_output();
assert!(output.contains("calibrator_action"));
assert!(output.contains("few_shot_candidates"));

// BAD: Parse full JSON and assert on structure
let json: Value = serde_json::from_str(&output)?;
assert_eq!(json["spans"][0]["name"], "calibrate_finding");
assert_eq!(json["spans"][0]["fields"]["fp_weight"], 2.0);
```

3. **Keep integration tests minimal.** One test that runs a pipeline end-to-end with tracing enabled and verifies output is parseable JSON with expected top-level keys. That is sufficient.

### Recommended test granularity

- **Unit tests in telemetry.rs**: Test any formatting/filtering logic you add (if any)
- **One integration test**: Run pipeline with tracing subscriber, verify output is valid JSON containing expected event names
- **Do NOT unit test every `tracing::info!()` call site** -- that is testing implementation

---

## Summary: Anti-Pattern Risk Matrix

| Feature | Highest Risk Anti-Patterns | Mitigation |
|---------|---------------------------|------------|
| Prompt tightening | #15 Snapshot Abuse, #5 Testing Internals | Use contains/not-contains, avoid full-prompt snapshots |
| Wontfix suppress | #5 Testing Internals, #9 Second-class tests | Test decisions not arithmetic, reuse test helpers |
| Calibrator tracing | #16 Assertion-free, #15 Snapshot Abuse | Assert on decision fields, use redacted snapshots if any |
| Pipeline tracing | #5 Testing Internals, #19 Over-mocking | Test traced data structures, not trace emission |

## Architecture-Level Note

Quorum is a Rust library/CLI with significant business logic (calibration, prompt construction) and external boundaries (LLM API, filesystem). The **testing pyramid** is the right shape: heavy unit tests for calibration/prompt logic, integration tests for pipeline, minimal E2E for CLI. Your current 651-unit / 6-integration split matches this well.
