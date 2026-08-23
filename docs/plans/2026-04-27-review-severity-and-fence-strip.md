# Review.rs Severity Fallback + Fence-Strip Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix the corroborated severity-fallback bug in `src/review.rs` surfaced by the 3-way tool comparison meta-review (2026-04-26): silent severity downgrade for unknown LLM severity strings.

**Scope revision (2026-04-27 evening):** Originally bundled Bug 2 (fence-strip backtick deletion in `extract_json_array`). During TDD, three RED tests for Bug 2 all passed on the buggy code — Rust's `trim_end_matches` stops at first non-match, and real LLM payloads always have `]`/`}`/newline between JSON content and the closing fence, so the claimed corruption doesn't manifest. The original quorum self-review TP was theoretical; recorded an FP correction in the feedback store (entry #2212) citing this trace. Bug 2 dropped from the PR. See discussion thread on 2026-04-27 in `dev:start` execution.

**Architecture:** Two localized changes in `src/review.rs`. Bug 1 swaps the catch-all match arm in `LlmFinding::into_finding` to default to `Severity::Medium` and emit a `tracing::warn!` for schema-drift forensics. Bug 2 swaps `trim_start_matches`/`trim_end_matches` for one-shot `strip_prefix`/`strip_suffix` in `extract_json_array`'s fence-stripping block — the bracket-finding loop that follows is untouched. Strict TDD: every behavior change ships with a failing test that turns green.

**Tech Stack:** Rust (project workspace), `tracing` crate (already a project dep), `cargo test --bin quorum` for the unit tests.

---

## Source

- Comparison artifact: `docs/comparisons/2026-04-26-review-rs-three-way.md`
- Feedback store entries (already recorded): severity-downgrade as External TP from both `pal` and `third-opinion`; fence-strip as Human TP recorded by quorum self-review
- Gemini 3.1 Pro design review (2026-04-27 evening) caught one regression in the original draft (would have dropped the bracket-finding fallback) — corrected before plan was written
- Acceptance criteria for Tier 1 explicitly defined in the meta-review thread

## Out of scope

- Issue #112 (hydration sandbox) — deferred pending PoC
- Issue #113 (line-bounds clamp) — separate PR
- Issue #114 (calibrator corroborated_by) — separate work stream
- Issue #115 (5-file methodology panel) — separate work stream
- Adding `original_severity_string` to the `Finding` struct — YAGNI; trace.jsonl covers forensics

---

## Task 1: Bug 1 RED — failing test for unknown severity → Medium

**Files:**
- Modify: `src/review.rs` (find existing `mod tests` block and add new test next to `llm_finding_unknown_severity_defaults_to_info` at line ~441)

**Step 1: Add the failing test**

Add this test to the same `mod tests` block where `llm_finding_unknown_severity_defaults_to_info` lives:

```rust
#[test]
fn llm_finding_unknown_severity_defaults_to_medium() {
    // Cross-tool corroborated TP from third-opinion + pal in 3-way comparison
    // 2026-04-26 (docs/comparisons/2026-04-26-review-rs-three-way.md).
    // Unknown severity strings (schema drift, prompt-injected output) must
    // default to Medium rather than silently degrade to Info.
    let lf = LlmFinding {
        title: "T".into(),
        description: "D".into(),
        severity: "blocker".into(),
        category: "c".into(),
        line_start: 1,
        line_end: 1,
        suggested_fix: None,
    };
    assert_eq!(lf.into_finding("m").severity, Severity::Medium);
}

#[test]
fn llm_finding_empty_severity_defaults_to_medium() {
    let lf = LlmFinding {
        title: "T".into(),
        description: "D".into(),
        severity: "".into(),
        category: "c".into(),
        line_start: 1,
        line_end: 1,
        suggested_fix: None,
    };
    assert_eq!(lf.into_finding("m").severity, Severity::Medium);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum llm_finding_unknown_severity_defaults_to_medium llm_finding_empty_severity_defaults_to_medium`

Expected: both tests FAIL with `assertion `left == right` failed: left: Info, right: Medium`.

**Step 3: Commit the RED tests**

```bash
git add src/review.rs
git commit -m "test(review): RED — unknown severity should default to Medium

Adds two failing tests asserting Severity::Medium for unknown ('blocker')
and empty severity strings. Cross-tool corroborated TP from
third-opinion + pal in 3-way comparison 2026-04-26.

Refs: docs/comparisons/2026-04-26-review-rs-three-way.md"
```

---

## Task 2: Bug 1 GREEN — flip catch-all to Medium + tracing::warn

**Files:**
- Modify: `src/review.rs:51-52` (the catch-all match arm in `into_finding`)

**Step 1: Verify `tracing` is already imported**

Run: `grep -n "^use tracing" src/review.rs` — there should already be a `use tracing::*` or similar. If not, run `grep -n "tracing::" src/review.rs | head -3` to find an existing call as a model.

If no `use` line for tracing exists at the top of `src/review.rs`, the implementation step below uses the fully-qualified `tracing::warn!` macro — no import needed.

**Step 2: Replace the catch-all arm**

Change the match block in `into_finding` (currently at line 46-53) from:

```rust
let severity = match self.severity.to_lowercase().as_str() {
    "critical" => Severity::Critical,
    "high" | "error" => Severity::High,
    "medium" | "warning" | "warn" => Severity::Medium,
    "low" | "note" => Severity::Low,
    "info" | "suggestion" | "hint" => Severity::Info,
    _ => Severity::Info,
};
```

To:

```rust
let severity = match self.severity.to_lowercase().as_str() {
    "critical" => Severity::Critical,
    "high" | "error" => Severity::High,
    "medium" | "warning" | "warn" => Severity::Medium,
    "low" | "note" => Severity::Low,
    "info" | "suggestion" | "hint" => Severity::Info,
    other => {
        tracing::warn!(
            target: "review.severity_drift",
            model = %model_name,
            raw_severity = %other,
            "unknown severity in LLM response; defaulting to Medium"
        );
        Severity::Medium
    }
};
```

Notes:
- The `other` binding captures the lowercased string (after `.to_lowercase()`), which is what we want recorded.
- `target: "review.severity_drift"` lets users filter trace events: `RUST_LOG=review.severity_drift=warn`.
- Project standard logs to `~/.quorum/trace.jsonl` automatically with `--trace` or `QUORUM_TRACE=1` (per CLAUDE.md).

**Step 3: Run the new tests to verify GREEN**

Run: `cargo test --bin quorum llm_finding_unknown_severity_defaults_to_medium llm_finding_empty_severity_defaults_to_medium`

Expected: both PASS.

**Step 4: Commit the GREEN fix**

```bash
git add src/review.rs
git commit -m "fix(review): default unknown LLM severity to Medium with tracing::warn

Schema drift (unknown severity strings, prompt-injected output) was
silently downgraded to Severity::Info, hiding meaningful findings.
Now defaults to Medium and emits a structured trace event so drift
is observable.

Cross-tool corroborated TP from third-opinion + pal in
3-way comparison 2026-04-26.

Refs: docs/comparisons/2026-04-26-review-rs-three-way.md"
```

**Note on test coverage:** the `tracing::warn!` emission is deliberately
not tested. The `target:` string and structured field names are
observability surface, not behavioral contract — coupling tests to
them would be a testing-implementation-details antipattern. `tracing`
is third-party and well-tested upstream. (Decision recorded after
testing-antipatterns-expert review on 2026-04-27.)

---

## Task 3: Bug 1 REFACTOR — flip the existing test asserting Info

**Files:**
- Modify: `src/review.rs` (the existing `llm_finding_unknown_severity_defaults_to_info` test, currently around line 441)

**Step 1: Update the existing test**

Replace the existing test block with a renamed version that reflects the new behavior:

```rust
#[test]
fn llm_finding_unknown_severity_falls_through_to_default() {
    // Behavior change 2026-04-27: unknown severity strings used to default
    // to Severity::Info, which silently hid schema drift. We now default
    // to Severity::Medium with a tracing::warn — see Task 2 of
    // docs/plans/2026-04-27-review-severity-and-fence-strip.md and the
    // corroborated TP in the 3-way comparison artifact.
    //
    // This test is the historical "tests the fallback mechanism" coverage,
    // updated to reflect the new fallback target.
    let lf = LlmFinding {
        title: "T".into(),
        description: "D".into(),
        severity: "banana".into(),
        category: "c".into(),
        line_start: 1,
        line_end: 1,
        suggested_fix: None,
    };
    assert_eq!(lf.into_finding("m").severity, Severity::Medium);
}
```

Note: `llm_finding_case_insensitive_severity` (the test directly below it) is unchanged — it asserts `"HIGH"` → `Severity::High`, which is still correct.

**Step 2: Run all severity tests**

Run: `cargo test --bin quorum llm_finding`

Expected: all 4 tests pass (`llm_finding_unknown_severity_falls_through_to_default`, `llm_finding_unknown_severity_defaults_to_medium`, `llm_finding_empty_severity_defaults_to_medium`, `llm_finding_case_insensitive_severity`).

**Step 3: Commit the test cleanup**

```bash
git add src/review.rs
git commit -m "test(review): rename + flip severity-fallback test for new behavior

Renames llm_finding_unknown_severity_defaults_to_info to
llm_finding_unknown_severity_falls_through_to_default and updates the
expectation to Severity::Medium. Preserves the historical intent
(testing the fallback mechanism) while reflecting the new target."
```

---

## Task 4: Bug 2 RED — failing test for fence-strip preserving trailing backticks

**Files:**
- Modify: `src/review.rs` (existing `mod tests` block, near other `extract_json_array`/`parse_llm_response` tests)

**Step 1: Find an anchor test**

Run: `grep -n "fn extract_json_array_\|fn parse_llm_response_" src/review.rs | head -5` to find the cluster of fence/parse tests. New tests go in the same module.

**Step 2: Add the failing tests**

Add these two tests to the `mod tests` block (place them next to the existing `extract_json_array_*` tests):

```rust
#[test]
fn extract_json_array_preserves_trailing_backticks_in_string_value() {
    // Human TP from quorum self-review in 3-way comparison 2026-04-26.
    // The greedy trim_end_matches("```") used to strip ALL trailing backtick
    // runs, corrupting JSON whose final string value legitimately ends with
    // backticks (e.g. a suggested_fix containing a code sample).
    let payload = "```json\n[{\"title\":\"x\",\"suggested_fix\":\"use ```json blocks```\"}]\n```";
    let extracted = extract_json_array(payload);
    // The extracted slice must still parse as JSON — the trailing ``` inside
    // the string value must survive untouched.
    let parsed: serde_json::Value = serde_json::from_str(&extracted)
        .expect("extracted slice should still be valid JSON");
    let suggested_fix = parsed[0]["suggested_fix"].as_str().unwrap();
    // Full equality (not ends_with) — kills off-by-one mutants where the
    // implementation strips one fewer/more backtick than expected.
    assert_eq!(suggested_fix, "use ```json blocks```");
}

#[test]
fn extract_json_array_preserves_backticks_in_bare_fence() {
    // Same bug, bare ``` fence (no language hint).
    let payload = "```\n[{\"title\":\"x\",\"suggested_fix\":\"trailing ```\"}]\n```";
    let extracted = extract_json_array(payload);
    let parsed: serde_json::Value = serde_json::from_str(&extracted)
        .expect("extracted slice should still be valid JSON");
    let suggested_fix = parsed[0]["suggested_fix"].as_str().unwrap();
    assert_eq!(suggested_fix, "trailing ```");
}

#[test]
fn extract_json_array_handles_unclosed_fence() {
    // Locks in the `strip_suffix(...).unwrap_or(rest)` fallback in Task 5.
    // Without unwrap_or (e.g. a future refactor to `strip_suffix(...)?`),
    // a payload with an opening fence but no closing fence would return
    // the wrong thing or panic. This test prevents that regression.
    let payload = "```json\n[{\"title\":\"x\"}]";  // no trailing ```
    let parsed: serde_json::Value = serde_json::from_str(&extract_json_array(payload))
        .expect("unclosed-fence payload must still parse");
    assert_eq!(parsed[0]["title"], "x");
}
```

**Step 3: Run tests to verify they fail**

Run: `cargo test --bin quorum extract_json_array_preserves extract_json_array_handles_unclosed_fence`

Expected:
- `extract_json_array_preserves_*` (both): FAIL with `serde_json::from_str` parse error (greedy trim corrupted the JSON), OR `assert_eq!` fails if parse somehow succeeded.
- `extract_json_array_handles_unclosed_fence`: PASS even before the fix — the existing greedy `trim_end_matches("```")` on a payload with no `\`\`\`` to trim is a no-op, so the bracket-finding fallback already handles this case. This test is a regression guard for the GREEN refactor (Task 5).

**Step 4: Commit the RED tests**

```bash
git add src/review.rs
git commit -m "test(review): RED — fence-strip must preserve trailing backticks in JSON values

Adds two failing tests for extract_json_array: trailing backticks
inside a JSON string value (e.g. a suggested_fix containing a code
sample ending with \`\`\`) must survive fence stripping. Currently
trim_end_matches(\"\`\`\`\") strips them greedily and corrupts the JSON.

Quorum self-review TP in 3-way comparison 2026-04-26.

Refs: docs/comparisons/2026-04-26-review-rs-three-way.md"
```

---

## Task 5: Bug 2 GREEN — replace greedy trim with one-shot strip_prefix/strip_suffix

**Files:**
- Modify: `src/review.rs:361-370` (the fence-stripping block at the top of `extract_json_array`)

**Step 1: Replace the fence-strip block**

Change the function head from:

```rust
fn extract_json_array(text: &str) -> String {
    // Strip markdown code fences if present
    let text = text.trim();
    let text = if text.starts_with("```json") {
        text.trim_start_matches("```json").trim_end_matches("```").trim()
    } else if text.starts_with("```") {
        text.trim_start_matches("```").trim_end_matches("```").trim()
    } else {
        text
    };
    // ... bracket-finding logic unchanged ...
```

To:

```rust
fn extract_json_array(text: &str) -> String {
    // Strip markdown code fences if present.
    // Use one-shot strip_prefix / strip_suffix so we don't greedily delete
    // backtick runs at the start or end of the payload — that used to
    // corrupt JSON values whose string content legitimately ends with
    // backticks (e.g. a suggested_fix containing a fenced code sample).
    let text = text.trim();
    let text = if let Some(rest) = text.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = text.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        text
    };
    // ... bracket-finding logic unchanged ...
```

**Critical:** the bracket-finding loop that follows this block (currently lines 372 onward) must remain unchanged. Only the inner `let text = if ... else ...` is rewritten. Verify by running `grep -n "outermost JSON array using bracket depth" src/review.rs` after the edit — that comment must still be present, immediately following the rewritten block.

**Step 2: Run the new tests to verify GREEN**

Run: `cargo test --bin quorum extract_json_array_preserves`

Expected: both PASS.

**Step 3: Run all extract_json_array + parse_llm_response tests for regressions**

Run: `cargo test --bin quorum extract_json_array parse_llm_response`

Expected: full test cluster passes — both new tests AND every existing test (no regressions in the fence-stripping or bracket-finding paths).

**Step 4: Commit the GREEN fix**

```bash
git add src/review.rs
git commit -m "fix(review): preserve trailing backticks in JSON values via one-shot strip

extract_json_array used trim_end_matches(\"\`\`\`\") to strip closing
fences, which greedily removes ALL trailing backtick runs. JSON
string values that legitimately end with backticks (suggested_fix
containing code samples) were corrupted before parsing.

Replaced with one-shot strip_prefix / strip_suffix on both ends.
Bracket-finding fallback below the fence-strip block is untouched.

Quorum self-review TP in 3-way comparison 2026-04-26.

Refs: docs/comparisons/2026-04-26-review-rs-three-way.md"
```

---

## Task 6: End-to-end regression test through `parse_llm_response`

Both individual unit tests are now green. Add one integration-style test that exercises both fixes through the public-facing `parse_llm_response` entry point. Without this, a future refactor that bypasses `into_finding` or `extract_json_array` (e.g. a new parse strategy that re-implements either) could regress silently — the unit tests would still pass.

**Files:**
- Modify: `src/review.rs` (existing `mod tests` block, near the other `parse_llm_response_*` tests)

**Step 1: Add the end-to-end test**

```rust
#[test]
fn parse_llm_response_handles_unknown_severity_and_trailing_backticks() {
    // End-to-end regression for both bugs from the 3-way comparison
    // 2026-04-26: (1) unknown severity defaults to Medium (not Info),
    // (2) trailing backticks in JSON string values survive fence-strip.
    // This guards against a future refactor that bypasses into_finding
    // or extract_json_array.
    let payload = "```json\n[{\
        \"title\":\"finding\",\
        \"description\":\"d\",\
        \"severity\":\"blocker\",\
        \"category\":\"correctness\",\
        \"line_start\":1,\
        \"line_end\":1,\
        \"suggested_fix\":\"use ```json blocks```\"\
    }]\n```";

    let findings = parse_llm_response(payload, "gpt-test")
        .expect("payload should parse end-to-end");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Medium);
    assert_eq!(
        findings[0].suggested_fix.as_deref(),
        Some("use ```json blocks```")
    );
}
```

**Step 2: Run the test**

Run: `cargo test --bin quorum parse_llm_response_handles_unknown_severity_and_trailing_backticks`

Expected: PASS — both fixes are already green at this point, so this test should pass first try. If it fails, the failure points to a defect in how the unit-level fixes compose end-to-end.

**Step 3: Commit**

```bash
git add src/review.rs
git commit -m "test(review): end-to-end regression for severity + fence-strip fixes

Adds parse_llm_response integration test exercising both bugs in a
single payload: unknown severity 'blocker' and a suggested_fix string
ending with literal backticks. Guards against a future refactor that
bypasses into_finding or extract_json_array.

Refs: docs/plans/2026-04-27-review-severity-and-fence-strip.md"
```

---

## Task 7: Verification gates

**Files:** none — running existing project gates.

**Step 1: Full test suite**

Run: `cargo test --bin quorum`

Expected: all unit tests pass (~660+ depending on current count). Capture the final summary line as evidence.

**Step 2: Full integration tests**

Run: `cargo test`

Expected: full suite passes including CLI integration tests.

**Step 3: Clippy**

Run: `cargo clippy --bin quorum -- -D warnings`

Expected: no warnings or errors. If clippy complains about the new `tracing::warn!` invocation or anything else introduced in Tasks 2/5, fix the lint and amend the relevant commit (or add a follow-up commit if amending would muddy the TDD history).

**Step 4: Release build**

Run: `cargo build --release`

Expected: clean release build, ~31MB binary. This catches any debug-only issues that don't surface in `cargo test`.

**Step 5: Capture verification evidence**

Append the test/clippy/build summary lines to a temporary scratch note (or paste directly into the PR body during Phase 8). Per superpowers:verification-before-completion, never claim success without the output.

**Step 6: No commit needed for this task** — it's a verification gate, not a code change.

---

## Task 8: Quorum self-review on the diff

**Files:** none — running an analysis tool.

**Step 1: Run quorum on the changed file**

Run: `quorum review src/review.rs --no-color`

Expected: zero NEW critical/high findings introduced by this branch. Pre-existing findings (e.g. cyclomatic complexity on `build_review_prompt` / `sanitize_json_escapes`) will still surface — those are already calibrator-suppressed and tracked elsewhere.

**Step 2: Triage each new finding**

For every finding NOT already in the calibrator's suppression list:
- **In-branch bug** (introduced or directly touched by this work): return to Task N+1 — RED test reproducing the finding → GREEN fix → commit. Stay in the branch.
- **Pre-existing bug** (orthogonal to this work): file as a new GitHub issue with `gh issue create`, citing `src/review.rs:<line>` and the quorum finding text. Do NOT fix in this branch.

**Step 3: Re-run quorum until clean**

Loop Step 1 until the changed surface returns no new findings.

---

## Task 9: Record feedback verdicts

**Files:** none — feedback CLI calls.

For every quorum finding from Task 7 (whether confirmed, fixed, or rejected), record a verdict so the calibrator learns. Run these in parallel where independent:

```bash
# Example shapes — actual finding titles will vary.
# Confirmed bug we fixed in this branch:
quorum feedback --file src/review.rs --finding "<title>" --verdict tp \
  --reason "Fixed in this PR. <one-line explanation>"

# Pre-existing bug filed as separate issue:
quorum feedback --file src/review.rs --finding "<title>" --verdict tp \
  --reason "Real but pre-existing; filed as #<issue>. Not in scope for this PR."

# False positive:
quorum feedback --file src/review.rs --finding "<title>" --verdict fp \
  --reason "<why it's not a real issue>"
```

If quorum returns zero new findings (clean), no feedback to record for this branch.

---

## Task 10: Independent code review

**Files:** none — review of the diff.

Invoke `superpowers:requesting-code-review` on the branch. Provide the reviewer with:
- Link to this plan
- The two corroborated TPs from the comparison artifact
- The Gemini 3.1 Pro design review (in the conversation history)
- Quorum's self-review verdicts from Task 7

Address any blocking feedback with focused commits before opening the PR.

---

## Task 11: Open PR

**Files:** none — PR creation.

**Step 1: Push the branch**

```bash
git push -u origin <branch-name>
```

**Step 2: Open the PR with full context**

```bash
gh pr create --title "fix(review): severity unknown defaults to Medium + fence-strip preserves backticks" \
  --body-file <prepared-body.md>
```

The PR body must include:
- One-line summary per bug
- Source/evidence: link to `docs/comparisons/2026-04-26-review-rs-three-way.md`, mention the two corroborated TPs, link to this plan
- Verification evidence captured in Task 6 (test counts, clippy, release build)
- Quorum self-review verdicts from Task 7
- Test plan checklist for the reviewer

**Step 3: Verify CI is green** before requesting human review.

---

## Stop conditions

- **Plan rejected** by user → halt, do not create worktree.
- **Test cannot be made green** after 3 honest attempts → stop, consult user. Never weaken or skip the test.
- **Quorum surfaces architectural issues** that would expand scope → stop, return to brainstorming with user. No silent scope creep.

---

## Definition of done

- [ ] Both bugs have RED-then-GREEN tests committed in TDD order
- [ ] Existing line-441 test renamed and flipped (Task 3)
- [ ] End-to-end regression test through `parse_llm_response` (Task 6)
- [ ] `cargo test`, `cargo clippy --bin quorum -- -D warnings`, `cargo build --release` all clean
- [ ] Quorum self-review on `src/review.rs` returns no new in-branch findings (pre-existing findings filed as separate issues)
- [ ] Feedback verdicts recorded for every quorum finding triaged in Task 8
- [ ] Independent code review pass complete
- [ ] PR opened with verification evidence + verdict log + plan link in body
