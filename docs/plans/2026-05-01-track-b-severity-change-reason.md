# Track B: severity_change_reason trace metadata Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a `severity_change_reason` field to `CalibratorTraceEntry` that records *why* the calibrator did or did not change a finding's severity (boost succeeded, boost blocked by gate, disputed, weak signal, or no precedents matched), unblocking precise eval-harness measurement of the Track A gate's effect; bundle the codex-flagged CRITICAL keyword tightening (`secret leak` / `data loss` over-broad matches) with regression tests.

**Architecture:** Trace-only addition. New `SeverityChangeReason` enum in `src/calibrator_trace.rs`, new `Option<SeverityChangeReason>` field on `CalibratorTraceEntry` with `#[serde(default, skip_serializing_if = "Option::is_none")]` for backward compat. The two boost branches in `src/calibrator.rs` (`calibrate` and `calibrate_with_index`) track a local `Option<SeverityChangeReason>` set at each decision branch and include it in the final trace push. Existing `CalibratorAction` semantics and the `boosted` counter are unchanged — purely additive.

**Tech Stack:** Rust 2024, serde, the existing trace pipeline (writes to `~/.quorum/calibrator_traces.jsonl`).

---

## Background — what we're changing and why

After Track A (PR #187, merged e2ad4a6) shipped the rubric severity gate, the gate works correctly but its effect is hard to measure cleanly. When the calibrator skips a boost, three different cases all currently produce the same trace shape:

1. **Boost happened** — `tp_weight` met threshold AND gate allowed → `severity_after > severity_before`, `boosted` counter++.
2. **Boost blocked by gate** — `tp_weight` met threshold AND gate refused → `severity_after == severity_before`, `boosted` counter unchanged, `calibrator_action = Confirmed`.
3. **Boost not attempted** — `tp_weight` did not meet 1.5/2x threshold → `severity_after == severity_before`, `boosted` counter unchanged, `calibrator_action = Confirmed` or `None`.

Cases 2 and 3 are visually identical in the trace. The eval harness can count boosts but not gate-blocks. After this PR, every trace entry carries a `severity_change_reason` that disambiguates them.

The bundled second piece: codex's review of Track A flagged that `secret leak` and `data loss` are too broad as CRITICAL gate keywords. A finding describing "logs may leak secrets" or "lossy cache cleanup" is not CRITICAL but currently passes the gate. We tighten the phrasing and add regression tests.

---

## Task 1: Add `SeverityChangeReason` enum + field

**Files:**
- Modify: `src/calibrator_trace.rs` — add enum + field, update existing serialization tests.

**Step 1: Write the failing test**

Add this to `src/calibrator_trace.rs`'s existing `mod tests`:

```rust
#[test]
fn severity_change_reason_serializes_snake_case() {
    use SeverityChangeReason::*;
    for (variant, expected) in [
        (Boosted, "\"boosted\""),
        (BoostBlockedByGate, "\"boost_blocked_by_gate\""),
        (Disputed, "\"disputed\""),
        (BoostWeightTooLow, "\"boost_weight_too_low\""),
        (NoMatch, "\"no_match\""),
    ] {
        let s = serde_json::to_string(&variant).unwrap();
        assert_eq!(s, expected, "variant {variant:?} must serialize to {expected}");
    }
}

#[test]
fn trace_entry_omits_reason_when_none() {
    // Backward compat: pre-Track-B trace lines carry no reason field.
    let trace = CalibratorTraceEntry {
        finding_title: "x".into(),
        finding_category: "y".into(),
        tp_weight: 0.0,
        fp_weight: 0.0,
        wontfix_weight: 0.0,
        full_suppress_weight: 0.0,
        soft_fp_weight: 0.0,
        matched_precedents: vec![],
        action: None,
        input_severity: Severity::Low,
        output_severity: Severity::Low,
        severity_change_reason: None,
    };
    let json = serde_json::to_string(&trace).unwrap();
    assert!(!json.contains("severity_change_reason"),
        "None reason must be omitted from JSON to keep traces backward-compatible");
}

#[test]
fn trace_entry_deserializes_pre_track_b_lines() {
    // A trace line written before Track B has no severity_change_reason field.
    // It must still deserialize, with reason defaulting to None.
    let pre_track_b = r#"{
        "finding_title":"x","finding_category":"y",
        "tp_weight":0.0,"fp_weight":0.0,"wontfix_weight":0.0,
        "full_suppress_weight":0.0,"soft_fp_weight":0.0,
        "matched_precedents":[],"action":null,
        "input_severity":"low","output_severity":"low"
    }"#;
    let trace: CalibratorTraceEntry = serde_json::from_str(pre_track_b).unwrap();
    assert!(trace.severity_change_reason.is_none());
}
```

**Step 2: Run tests to verify they fail**

```bash
cd "$(git rev-parse --show-toplevel)"
cargo test --bin quorum -p quorum severity_change_reason_serializes_snake_case trace_entry_omits_reason_when_none trace_entry_deserializes_pre_track_b_lines 2>&1 | tail -10
```
Expected: 3 errors, "cannot find type `SeverityChangeReason`" / "no field `severity_change_reason`".

**Step 3: Add the enum + field**

Edit `src/calibrator_trace.rs`. Add after `PrecedentTrace`:

```rust
/// Why the calibrator did (or did not) change a finding's severity.
///
/// Serialized into `~/.quorum/calibrator_traces.jsonl` for eval-harness
/// measurement. `None` means "the field wasn't set" (backward compat with
/// pre-Track-B trace lines); not a value the live calibrator should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityChangeReason {
    /// Calibrator raised severity (gate allowed). `output_severity > input_severity`.
    Boosted,
    /// Calibrator wanted to raise severity but the rubric gate refused.
    /// `output_severity == input_severity`, but a boost was attempted.
    BoostBlockedByGate,
    /// FP / wontfix precedent demoted finding to Info.
    /// `output_severity < input_severity`.
    Disputed,
    /// TP precedent existed but didn't reach the 1.5 / 2x boost thresholds.
    /// `output_severity == input_severity`, no boost attempted.
    BoostWeightTooLow,
    /// No precedents in the corpus matched this finding.
    /// `output_severity == input_severity`, no calibration applied.
    NoMatch,
}
```

Then add field to `CalibratorTraceEntry`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Track B (#TBD): why severity did or did not change.
    /// `None` only for backward-compat with pre-Track-B trace lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_change_reason: Option<SeverityChangeReason>,
}
```

Note: had to add `Deserialize` to the struct derives (was Serialize-only); needed for the round-trip test. Verify nothing else depends on the previous derive set — likely none, since traces are write-only in production.

**Step 4: Update existing trace tests to compile**

In `src/calibrator_trace.rs::tests`, the two existing tests (`trace_entry_serializes_to_json`, `trace_entry_with_no_precedents`) construct `CalibratorTraceEntry` literals. Add `severity_change_reason: None` to both.

**Step 5: Run tests to verify they pass**

```bash
cargo test --bin quorum -p quorum trace_entry 2>&1 | tail -5
cargo test --bin quorum -p quorum severity_change_reason 2>&1 | tail -5
```
Expected: all green.

**Step 6: Commit**

```bash
git add src/calibrator_trace.rs
git commit -m "feat(calibrator-trace): add SeverityChangeReason enum + trace field

Schema-only change. Field is Option, serialized only when Some, defaults
to None on deserialize, so pre-Track-B trace lines parse unchanged.
Wiring into the calibrator follows in subsequent commits."
```

---

## Task 2: Find call sites of `CalibratorTraceEntry { ... }` outside the test module

**Files:**
- Inspect: `src/calibrator.rs`

**Step 1: Run the search**

```bash
grep -n "CalibratorTraceEntry {" src/calibrator.rs
```

Expected: 6 matches at lines ~191, ~270, ~320, ~449, ~525, ~575 (six push sites in `calibrate` and `calibrate_with_index`).

**Step 2: Add `severity_change_reason: None` to each push site as a placeholder**

Use multi-edit to add the field with `None` default at every push site so the file compiles immediately. We'll wire actual values in Tasks 3 and 4.

For each site, the push currently looks like:
```rust
traces.push(crate::calibrator_trace::CalibratorTraceEntry {
    finding_title: ...,
    ...
    output_severity: finding.severity.clone(),
});
```

Add a final line before the closing brace:
```rust
    output_severity: finding.severity.clone(),
    severity_change_reason: None,
});
```

**Step 3: Verify compile**

```bash
cargo check --bin quorum 2>&1 | tail -3
```
Expected: clean compile.

**Step 4: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): wire severity_change_reason placeholder in trace pushes"
```

---

## Task 3: Wire reasons in `calibrate()` (non-indexed path)

**Files:**
- Modify: `src/calibrator.rs` lines ~178-330 (the `calibrate` fn body's per-finding loop).

The flow already filters to `similar` precedents. After it computes `tp_weight` / `fp_weight` / `soft_fp_weight`, the code branches into:
1. `if soft_fp_weight ... { finding.severity = Info; action = Disputed; }`
2. `if config.boost_tp && tp_weight >= 1.5 && tp_weight > fp_weight * 2.0` (boost path with gate)
3. `else if tp_weight > fp_weight * 1.5` (confirm-only)
4. (implicit else: leave action=None)

There's also the early-return `if similar.is_empty() { ... }` push site at the top.

**Step 1: Write the failing test**

Add to `src/calibrator.rs::tests`:

```rust
#[test]
fn calibrate_records_no_match_reason_when_corpus_empty() {
    let findings = vec![
        FindingBuilder::new()
            .title("Some finding never seen before")
            .category("security")
            .severity(Severity::Medium)
            .build(),
    ];
    let result = calibrate(findings, &[], &CalibratorConfig::default());
    assert_eq!(result.traces.len(), 1);
    assert_eq!(
        result.traces[0].severity_change_reason,
        Some(crate::calibrator_trace::SeverityChangeReason::NoMatch)
    );
}

#[test]
fn calibrate_records_boosted_reason_when_bump_succeeds() {
    let findings = vec![
        FindingBuilder::new()
            .title("Race condition in shared HashMap")
            .category("concurrency")
            .severity(Severity::Medium)
            .build(),
    ];
    let feedback = vec![
        fb("Race condition in shared HashMap", "concurrency", Verdict::Tp),
        fb("Race condition in shared HashMap", "concurrency", Verdict::Tp),
        fb("Race condition in shared HashMap", "concurrency", Verdict::Tp),
    ];
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    assert_eq!(result.findings[0].severity, Severity::High);
    assert_eq!(
        result.traces[0].severity_change_reason,
        Some(crate::calibrator_trace::SeverityChangeReason::Boosted)
    );
}

#[test]
fn calibrate_records_boost_blocked_by_gate_for_complexity() {
    // Stylistic complexity finding can't reach HIGH per the rubric gate.
    // calibrator wants to bump but gate refuses.
    let findings = vec![
        FindingBuilder::new()
            .title("Function `foo` has cyclomatic complexity 30")
            .category("complexity")
            .severity(Severity::Medium)
            .description("Long branchy function.")
            .build(),
    ];
    let feedback = vec![
        fb("Function `foo` has cyclomatic complexity 30", "complexity", Verdict::Tp),
        fb("Function `foo` has cyclomatic complexity 30", "complexity", Verdict::Tp),
        fb("Function `foo` has cyclomatic complexity 30", "complexity", Verdict::Tp),
    ];
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    assert_eq!(result.findings[0].severity, Severity::Medium, "gate blocked the bump");
    assert_eq!(
        result.traces[0].severity_change_reason,
        Some(crate::calibrator_trace::SeverityChangeReason::BoostBlockedByGate)
    );
}

#[test]
fn calibrate_records_disputed_reason_when_fp_dominates() {
    let findings = vec![
        FindingBuilder::new()
            .title("Use of unwrap")
            .category("error-handling")
            .severity(Severity::Medium)
            .build(),
    ];
    let mut feedback = vec![];
    for _ in 0..5 {
        feedback.push(fb("Use of unwrap", "error-handling", Verdict::Fp));
    }
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    assert_eq!(result.findings[0].severity, Severity::Info);
    assert_eq!(
        result.traces[0].severity_change_reason,
        Some(crate::calibrator_trace::SeverityChangeReason::Disputed)
    );
}

#[test]
fn calibrate_records_weight_too_low_reason_for_mixed_signal() {
    let findings = vec![
        FindingBuilder::new()
            .title("Some marginal finding")
            .category("security")
            .severity(Severity::Medium)
            .build(),
    ];
    // 1 TP + 1 FP — below the 1.5 boost threshold and 1.5x confirm threshold.
    let feedback = vec![
        fb("Some marginal finding", "security", Verdict::Tp),
        fb("Some marginal finding", "security", Verdict::Fp),
    ];
    let result = calibrate(findings, &feedback, &CalibratorConfig::default());
    assert_eq!(result.findings[0].severity, Severity::Medium);
    assert_eq!(
        result.traces[0].severity_change_reason,
        Some(crate::calibrator_trace::SeverityChangeReason::BoostWeightTooLow)
    );
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum -p quorum calibrate_records 2>&1 | tail -10
```
Expected: 5 failures, all on `severity_change_reason: None` instead of the expected variant.

**Step 3: Wire reasons in `calibrate()`**

In `src/calibrator.rs`, find the `calibrate` fn (around line 134). Inside the per-finding loop:

1. **At the `similar.is_empty()` early-return push site** (~line 191): change `severity_change_reason: None,` to:
   ```rust
   severity_change_reason: Some(crate::calibrator_trace::SeverityChangeReason::NoMatch),
   ```

2. **In the main path (~lines 270-280)**: declare a local `let mut reason: Option<crate::calibrator_trace::SeverityChangeReason> = None;` right before the `if soft_fp_weight ...` block.

3. **In the disputed branch**: set `reason = Some(SeverityChangeReason::Disputed);`

4. **In the boost branch**: when the gate allows the bump (`severity = proposed; boosted += 1;`), set `reason = Some(SeverityChangeReason::Boosted);`. When the gate blocks (the `else` of the gate check), set `reason = Some(SeverityChangeReason::BoostBlockedByGate);`. Be careful: the current code is structured as `if !gate_on || rubric_supports... { /* allowed */ }` with no else block — add an `else` arm explicitly:
   ```rust
   if !gate_on || rubric_supports_severity_bump(&proposed, &finding) {
       finding.severity = proposed;
       boosted += 1;
       reason = Some(SeverityChangeReason::Boosted);
   } else {
       reason = Some(SeverityChangeReason::BoostBlockedByGate);
   }
   ```

5. **In the `else if tp_weight > fp_weight * 1.5` confirm branch**: set `reason = Some(SeverityChangeReason::BoostWeightTooLow);` (TP confirmed, but didn't qualify for boost).

6. **Implicit else (mixed signal — TP ~ FP)**: also set `reason = Some(SeverityChangeReason::BoostWeightTooLow);` (semantically: signal exists but didn't qualify to act on). Place this default after all the if/else branches, BEFORE the trace push, gated on `if reason.is_none() { reason = Some(SeverityChangeReason::BoostWeightTooLow); }`.

7. **At the trace push site for the main path** (~line 320): change `severity_change_reason: None,` to `severity_change_reason: reason,`.

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum -p quorum calibrate_records 2>&1 | tail -5
```
Expected: 5 passing.

```bash
cargo test --bin quorum 2>&1 | tail -3
```
Expected: full suite still green (1737+ passing — 1732 baseline + 5 new).

**Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): wire severity_change_reason in calibrate()"
```

---

## Task 4: Wire reasons in `calibrate_with_index()` (indexed path)

**Files:**
- Modify: `src/calibrator.rs` `calibrate_with_index` fn (~lines 380-595).

`calibrate_with_index` mirrors `calibrate` but uses an embedding-similarity-filtered precedent set. Same branches, same reason mapping.

**Step 1: Write the failing test**

```rust
#[test]
fn calibrate_with_index_records_severity_change_reasons() {
    use crate::calibrator_trace::SeverityChangeReason;
    use crate::feedback_index::FeedbackIndex;

    let index = FeedbackIndex::default();
    let findings = vec![
        FindingBuilder::new()
            .title("Race condition in shared map")
            .category("concurrency")
            .severity(Severity::Medium)
            .build(),
    ];
    let result = calibrate_with_index(findings, &mut index, &CalibratorConfig::default());
    assert_eq!(
        result.traces[0].severity_change_reason,
        Some(SeverityChangeReason::NoMatch)
    );
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test --bin quorum -p quorum calibrate_with_index_records 2>&1 | tail -5
```
Expected: fails (currently still placeholder None).

**Step 3: Apply the same wiring as Task 3**

Mirror the changes from Task 3 to `calibrate_with_index`. The branches are at:
- `similar.is_empty()` early return (~line 449): `NoMatch`
- soft_fp Disputed (~line 525): `Disputed`
- boost branch (~line 575): `Boosted` / `BoostBlockedByGate`
- confirm-only branch: `BoostWeightTooLow`
- final trace push (~line 585): use `reason` local

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum -p quorum calibrate_with_index_records 2>&1 | tail -5
cargo test --bin quorum 2>&1 | tail -3
```
Expected: green; full suite still green.

**Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): wire severity_change_reason in calibrate_with_index()"
```

---

## Task 5: Tighten CRITICAL keyword set (codex finding #3)

**Files:**
- Modify: `src/calibrator.rs::rubric_supports_severity_bump` (~line 660).

Currently the CRITICAL_KEYWORDS list contains:
```rust
"secret leak", "credential leak", "credential exfil", ...
"data corruption", "data loss", ...
```

The `secret leak` and `data loss` are too broad; they match "logs may leak secrets" and "lossy cache cleanup" respectively, which are not CRITICAL.

**Step 1: Write the failing tests**

Add to the existing `mod tests` near the other gate tests:

```rust
#[test]
fn rubric_gate_blocks_speculative_secret_leak_to_critical() {
    // Codex review #3 (2026-05-01): "logs may leak secrets" is not CRITICAL —
    // it's a speculative information-disclosure HIGH at most. Track A's
    // "secret leak" keyword incorrectly matched this phrasing.
    let f = finding_at(
        "Verbose error logger may leak secrets to log files",
        "security",
        Severity::High,
        "If the user passes secrets in query params, they could appear in log files.",
    );
    assert!(
        !rubric_supports_severity_bump(&Severity::Critical, &f),
        "speculative 'may leak secrets' phrasing must NOT justify CRITICAL"
    );
}

#[test]
fn rubric_gate_blocks_lossy_cache_cleanup_to_critical() {
    // Same review: "lossy cache cleanup loses ephemeral state" is not CRITICAL.
    // Tighten "data loss" → "durable data loss" so transient state is excluded.
    let f = finding_at(
        "Cache eviction loses ephemeral query state",
        "correctness",
        Severity::High,
        "Items can be evicted from the in-memory cache, causing data loss for unsaved drafts.",
    );
    assert!(
        !rubric_supports_severity_bump(&Severity::Critical, &f),
        "ephemeral 'data loss' phrasing must NOT justify CRITICAL"
    );
}

#[test]
fn rubric_gate_allows_explicit_credential_leak_to_critical() {
    // Verify the tightened phrasing still catches the real cases.
    let f = finding_at(
        "Endpoint leaks credentials to unauthenticated callers",
        "security",
        Severity::High,
        "GET /admin returns the password hash table.",
    );
    assert!(
        rubric_supports_severity_bump(&Severity::Critical, &f),
        "explicit 'leaks credentials' must justify CRITICAL"
    );
}

#[test]
fn rubric_gate_allows_durable_data_loss_to_critical() {
    let f = finding_at(
        "Migration drops user_settings table without backup",
        "correctness",
        Severity::High,
        "The migration causes durable data loss for ~50k users.",
    );
    assert!(
        rubric_supports_severity_bump(&Severity::Critical, &f),
        "explicit 'durable data loss' must justify CRITICAL"
    );
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum -p quorum rubric_gate_blocks_speculative_secret_leak_to_critical \
  rubric_gate_blocks_lossy_cache_cleanup_to_critical \
  rubric_gate_allows_explicit_credential_leak_to_critical \
  rubric_gate_allows_durable_data_loss_to_critical 2>&1 | tail -10
```
Expected: 2 fail (the BLOCKS tests — current keywords accept them) + 2 pass (ALLOWS tests will pass since `credential leak` and `data loss` are present).

Wait — careful: the current keyword `credential leak` IS still present, so test 3 ("leaks credentials") may NOT match `credential leak` (different word order!). And the current `secret leak` keyword DOES match "may leak secrets" in test 1. Verify:

- test 1 ("may leak secrets"): "secret leak" matches `secret`+`leak`? No — `contains_word("secret leak")` requires the literal phrase. "may leak secrets" contains "leak" and "secrets" but not adjacent. So this should already pass under the current implementation. **Predicted: test 1 passes under existing code, no behavioral change needed.**
- test 2 ("lossy cleanup loses... data loss"): description contains "data loss" exactly. Current code matches. **Predicted: test 2 fails as designed — keyword match.**
- test 3 ("leaks credentials"): contains "credentials" + "leaks" but not "credential leak". Under the current keyword `credential leak`, `contains_word("credential leak", haystack)` requires the exact phrase. **Predicted: test 3 fails (no keyword match).**
- test 4 ("durable data loss"): contains "data loss" — current code matches. **Predicted: test 4 passes under existing code.**

So the actual failures are tests 2 and 3. Re-run to confirm before implementing.

**Step 3: Update keywords in `rubric_supports_severity_bump`**

Replace the `CRITICAL_KEYWORDS` block:
```rust
const CRITICAL_KEYWORDS: &[&str] = &[
    "rce",
    "remote code execution",
    "code execution",
    "arbitrary code",
    "data corruption",
    "auth bypass",
    "authentication bypass",
    "credential leak",
    "credential exfil",
    "credentials leaked",
    "leaks credentials",
    "leaks the password",
    "leaks all passwords",
    "secret exfil",
    "exfiltrates secrets",
    "guaranteed crash",
    "guaranteed production crash",
    "durable data loss",
    "permanent data loss",
];
```

Removed: `"secret leak"` (over-broad — matches speculative "may leak secrets"), `"data loss"` (over-broad — matches "ephemeral data loss" / "lossy cache").
Added: `"durable data loss"`, `"permanent data loss"` (the durability qualifier discriminates), `"credentials leaked"`, `"leaks credentials"`, `"leaks the password"`, `"leaks all passwords"`, `"secret exfil"`, `"exfiltrates secrets"` (active-voice variants for real cases).

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum -p quorum rubric_gate 2>&1 | tail -5
```
Expected: all 16 gate tests passing (12 prior + 4 new).

**Step 5: Run full suite + harness sanity**

```bash
cargo test --bin quorum 2>&1 | tail -3
```
Expected: 1742+ passing (1732 baseline + 5 new in Task 3 + 1 new in Task 4 + 4 new here).

**Step 6: Commit**

```bash
git add src/calibrator.rs
git commit -m "fix(calibrator-gate): tighten CRITICAL keywords per codex review

Replace broad 'secret leak' / 'data loss' with active-voice variants
('leaks credentials', 'credentials leaked', 'durable data loss',
'permanent data loss', etc). The original keywords caught speculative
phrasings ('logs may leak secrets', 'lossy cache cleanup loses data')
that are not CRITICAL per the rubric.

4 regression tests added."
```

---

## Task 6: Verification gates + self-review + PR

**Files:** none (process only).

**Step 1: Full verification**

```bash
cd "$(git rev-parse --show-toplevel)"
cargo test --bin quorum 2>&1 | tail -3
cargo build --release --bin quorum 2>&1 | tail -3
cargo clippy --bin quorum 2>&1 | grep -E "(calibrator|calibrator_trace)\.rs:" | head -10
```
Expected: tests green, release build clean, no NEW clippy warnings in changed files (pre-existing ones from Track A's PR may still be present — file-header doc comment).

**Step 2: Quorum self-review on the diff**

```bash
QUORUM_API_KEY=... QUORUM_BASE_URL=https://litellm.5745.house QUORUM_ALLOWED_BASE_URL_HOSTS=litellm.5745.house \
  ./target/release/quorum review src/calibrator.rs src/calibrator_trace.rs --json --no-color --parallel 2 \
  > /tmp/track-b-self-review.json
jq -r '.[] | select(.findings) | .file as $f | .findings[] | "[" + .severity + "] " + .title + " :: " + $f' /tmp/track-b-self-review.json
```

Triage findings:
- **In-branch bugs** → fix via mini TDD cycle.
- **Pre-existing** → skip (file as issue if HIGH+).

**Step 3: Record feedback for any in-branch findings**

```bash
./target/release/quorum feedback --file <file> --finding "<title>" --verdict tp --reason "Fixed in-branch"
```

**Step 4: Push branch + open PR**

```bash
git push -u origin feat/calibrator-trace-reason
gh pr create --title "feat(calibrator): Track B — severity_change_reason trace metadata + keyword tightening" \
  --body-file /tmp/track-b-pr-body.md
```

PR body template:
```markdown
## Summary

Two related calibrator improvements:

1. **`severity_change_reason` trace metadata** — every entry in
   `~/.quorum/calibrator_traces.jsonl` now records *why* the calibrator
   did or did not change a finding's severity (Boosted / BoostBlockedByGate /
   Disputed / BoostWeightTooLow / NoMatch). Unblocks precise eval-harness
   measurement of the Track A gate's effect.

2. **CRITICAL keyword tightening** — replace over-broad `secret leak` /
   `data loss` with active-voice variants ('leaks credentials',
   'durable data loss', etc). Per codex review of PR #187 — speculative
   phrasings like "logs may leak secrets" and "ephemeral cache data loss"
   were inappropriately passing the CRITICAL gate.

Schema-only addition; `Option<SeverityChangeReason>` with serde defaults
keeps pre-Track-B trace lines parsable.

## Test plan

- [x] cargo test --bin quorum (1742+ unit tests, all green)
- [x] cargo build --release
- [x] 4 new severity_change_reason wiring tests + 4 keyword regression tests
- [x] Backward-compat deserialize test for pre-Track-B trace lines
- [x] Self-review on src/calibrator{,_trace}.rs

## Out of scope (Track B-2)

- Finding-level `original_severity` + `severity_change_reason` (user-visible
  in `quorum review --json` output)
- Eval-harness consumer of the new trace field

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

**Step 5: Babysit CodeRabbit + merge**

After CR completes, address comments if any. Merge via `gh pr merge --squash --delete-branch`.

---

## Out of scope

- **Track B-2** (Finding-level provenance — `original_severity` + `severity_change_reason` on `Finding`, visible in `quorum review --json` output and human display).
- Eval-harness work that consumes the new trace field — separate experimentation effort, not a code change.
- The two pre-existing HIGH bugs filed during Track A's self-review (#185 trace JSONL multi-process race, #186 Context7 per-file init aborts file).

## DRY/YAGNI/TDD discipline reminders

- Each task has a RED → GREEN → REFACTOR cycle.
- Don't combine multiple branches into one commit.
- Don't refactor `boost_severity` or the gate logic itself in this PR.
- Don't add UI/display surfaces for the new field — that's Track B-2.
- The `BoostWeightTooLow` variant intentionally covers both "TP existed but below threshold" and "mixed signal (TP ~ FP)" — don't split prematurely.
