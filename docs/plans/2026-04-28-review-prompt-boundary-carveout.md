# Review Prompt: Boundary-Security Carve-Out (Issue #118)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Edit `OpenAiClient::system_prompt()` in `src/llm_client.rs` so the LLM stops demoting boundary-security findings under down-classification rules 3 and 4. Ship with Layer A (static prompt-content) + Layer B (post-LLM pipeline) regression tests. Layer C (live-fixture LLM run) is filed as #121 and is OUT OF SCOPE.

**Architecture:** Inject a single **Precedence rule** before the down-classification list (gpt-5.4 + claude-opus-4.5 + OpenAI Cookbook all converge: postpositive `EXCEPTION:` clauses are unreliable; a governing rule placed first is the right shape). Strip the inline EXCEPTION clauses from rules 3 and 4. Replace "Maintainability, naming, complexity, and defensive-programming concerns" in rule 4 with the narrower "Purely-stylistic concerns (naming, formatting, complexity-for-its-own-sake)" — and drop ALL-CAPS framing (noise, not binding). Extend priority list item 4 with resource-bounding language.

**Tech Stack:** Rust 2021 edition, `concat!()` macro for prompt assembly, `cargo test --bin quorum` for unit tests.

---

## Scope

**IN scope (this PR):**
- Edit `OpenAiClient::system_prompt()` per the design below.
- One Layer A regression test asserting precedence-rule keywords coexist.
- One Layer B regression test asserting a HIGH boundary finding survives `parse_llm_response` → `calibrator::calibrate` at HIGH.
- Update existing prompt-content tests at `src/review.rs:1144` and `src/review.rs:1492` if/only-if their assertions break against the new text.

**OUT of scope (deferred):**
- Layer C (live LLM fixture review) — issue #121.
- The boundary-security bug fixes themselves (#117 SSRF/cred-leak, #119 retry/timeout, #120 symlink/YAML-DoS) — those land in separate PRs after this prompt fix unblocks the LLM from finding them.
- Calibrator changes.
- Prompt-cache TTL or budget changes.

## Design: prompt edit

### Final prompt fragment (paste-ready)

Insert immediately BEFORE the existing `"Down-classification rules (apply in order):\n"` line:

```
Precedence rule (check first): When a finding involves missing validation, missing safety check, or missing resource bound at a trust or external-input boundary, classify it by the priority list (items 1-4) and severity rubric based on actual impact and reachable input surface. Rules 3 and 4 below do not apply to such findings. Trust/external-input boundaries include:
- Network input: timeout layering, retry policy, error-body content in user-visible output.
- Filesystem: path canonicalization, symlink handling, size caps on user-influenced content.
- Payload/response: unbounded allocation from external size, deserialization without size/shape limits.
- Auth/credential: URL parsing, credential placement, Bearer-header destination scope (SSRF surface).
```

### Simplification of rules 3 and 4

Rule 3: keep verbatim (no EXCEPTION suffix). Rule 4: replace with a narrower stylistic-only formulation:

```
3. If the issue is 'theoretically possible but no realistic trigger exists in this codebase' → low or omit, never high.
4. Purely-stylistic concerns (naming, formatting, complexity-for-its-own-sake) belong in low or info — never high — unless they directly hide a bug.
```

### Priority list item 4 extension

```
Current:
4. Architectural flaws that make bugs likely: non-atomic writes that can leave corrupt state, hidden invariants, tight coupling across trust boundaries, APIs that mislead callers about safety.

New:
4. Architectural flaws that make bugs likely: non-atomic writes that can leave corrupt state, hidden invariants, tight coupling across trust boundaries, APIs that mislead callers about safety, missing resource bounds at external-input boundaries (allocation, request count, file size).
```

### Why this shape (decision rationale)

Frontier-model critique consensus (gpt-5.4 + claude-opus-4.5):
1. Postpositive `EXCEPTION:` after a strong "never high" anchor gets compressed away by attention.
2. A precedence rule placed BEFORE the down-classification list establishes the carve-out as a primary filter, not a backtrack.
3. "Natural severity" is too implicit; explicit reference to "priority list (items 1-4) and severity rubric" forces the model to compute concretely.
4. Boundary enumeration MUST be co-located with the precedence rule (claude-opus-4.5's refinement over gpt-5.4) — without the inline list, "trust boundary" overgeneralizes.
5. OpenAI Cookbook GPT-4.1 prompting guide explicitly notes: "GPT-4.1 tends to follow the instruction closest to the end of the prompt" — confirms the structural concern.

---

## Test plan

### Layer A — ONE static-content test

Antipattern review (testing-antipatterns-expert): per-keyword tests are change-detector tautology. Snapshot tests are snapshot abuse here (devs `cargo insta accept` reflexively). Single test asserting two anchor substrings co-occur is enough.

**File:** `src/review.rs` (alongside existing prompt-content tests at lines 1144 and 1492).

**Test name:** `system_prompt_carves_out_trust_boundary_findings_via_precedence_rule`

**Assertions:**
- `prompt.contains("Precedence rule")` — regression breaks if precedence-rule scaffolding is removed.
- `prompt.contains("trust or external-input boundary")` — regression breaks if the boundary phrase is removed.
- Both must be in the same text (single `&str` returned by `system_prompt()` so this is implicit).

That's the entire Layer A test. No keyword-by-keyword sub-assertions ("symlink", "SSRF", "retry", etc.) — those are examples inside the carve-out, not the carve-out's existence.

### Layer B — ONE pipeline survival test

**File:** `src/review.rs` (a new test alongside `parse_llm_response_unknown_severity_defaults_to_medium_end_to_end`).

**Test name:** `high_boundary_finding_survives_calibrator_at_high`

**Strategy:**
1. Construct a JSON string representing one HIGH boundary-security finding (e.g. SSRF on a network call). Include enough body to look real.
2. Call `parse_llm_response(json, "test-model")` → `Vec<Finding>`.
3. Call `calibrator::calibrate(findings, &empty_feedback_store, &CalibratorConfig::default())` → `CalibrationResult`.
4. Assert `result.findings.len() == 1` and `result.findings[0].severity == Severity::High`.
5. Assert `result.suppressed == 0`.

**No HTTP/client mocking.** No new test traits or seams. `parse_llm_response` and `calibrator::calibrate` are already public; the test wires them directly.

**Limitation acknowledged:** Layer B can only test the positive direction (boundary finding survives). The negative case ("HIGH stylistic finding still gets demoted to enforce carve-out scoping") cannot be tested here — the prompt is the only place that scoping lives, and prompt-fidelity-to-LLM-behavior is exactly what Layer C (#121) covers. This limitation is intentional.

### Existing tests audit

After editing the prompt, re-run `cargo test --bin quorum` and update only the existing prompt-content tests whose assertions actually break:
- `src/review.rs:1144` `system_prompt_deprioritizes_stylistic_findings_without_hard_reject` — asserts on stylistic-deprioritization language. Likely still passes (we kept "Purely-stylistic concerns"). Audit and update keyword if needed.
- `src/review.rs:1492` `fp_precedent_policy_lives_in_system_prompt_as_hard_negative` — asserts on FP precedent policy. Should not be affected.

Don't update tests preemptively — let `cargo test` tell us what breaks.

---

## Implementation tasks (TDD order)

### Task 1: Layer A test — RED

**Files:**
- Modify: `src/review.rs` (add new test in the `mod tests` block near existing prompt-content tests at ~line 1144)

**Step 1: Write the failing test**

```rust
#[test]
fn system_prompt_carves_out_trust_boundary_findings_via_precedence_rule() {
    let prompt = crate::llm_client::OpenAiClient::system_prompt();
    assert!(
        prompt.contains("Precedence rule"),
        "system prompt missing precedence-rule scaffolding for trust-boundary carve-out"
    );
    assert!(
        prompt.contains("trust or external-input boundary"),
        "system prompt missing the trust/external-input boundary anchor phrase"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum system_prompt_carves_out_trust_boundary -- --nocapture`
Expected: FAIL — "system prompt missing precedence-rule scaffolding"

**Step 3: Commit RED**

```bash
git add src/review.rs
git commit -m "test(review): add RED test for trust-boundary precedence-rule carve-out

Asserts system_prompt() contains both 'Precedence rule' and
'trust or external-input boundary' anchor phrases. Currently fails
— prompt edit lands in the next commit.

Refs #118"
```

### Task 2: Layer A test — GREEN (the prompt edit)

**Files:**
- Modify: `src/llm_client.rs` (system_prompt body, ~lines 388 and 411-416)

**Step 1: Edit priority list item 4**

Find:
```rust
"4. Architectural flaws that make bugs likely: non-atomic writes that can leave corrupt state, hidden invariants, tight coupling across trust boundaries, APIs that mislead callers about safety.\n",
```

Replace with:
```rust
"4. Architectural flaws that make bugs likely: non-atomic writes that can leave corrupt state, hidden invariants, tight coupling across trust boundaries, APIs that mislead callers about safety, missing resource bounds at external-input boundaries (allocation, request count, file size).\n",
```

**Step 2: Insert precedence rule + simplify rules 3 & 4**

Find:
```rust
"Down-classification rules (apply in order):\n",
"1. If the trigger requires non-default configuration, an explicitly unusual input, or a code path that callers don't reach in practice → downgrade from high to medium.\n",
"2. If the impact is a panic / error rather than silent corruption or security breach → downgrade from critical to high, or from high to medium when the panic is recoverable.\n",
"3. If the issue is 'theoretically possible but no realistic trigger exists in this codebase' → low or omit, never high.\n",
"4. Maintainability, naming, complexity, and defensive-programming concerns belong in low or info — never high — unless they directly hide a bug.\n",
```

Replace with:
```rust
"Precedence rule (check first): When a finding involves missing validation, missing safety check, or missing resource bound at a trust or external-input boundary, classify it by the priority list (items 1-4) and severity rubric based on actual impact and reachable input surface. Rules 3 and 4 below do not apply to such findings. Trust/external-input boundaries include:\n",
"- Network input: timeout layering, retry policy, error-body content in user-visible output.\n",
"- Filesystem: path canonicalization, symlink handling, size caps on user-influenced content.\n",
"- Payload/response: unbounded allocation from external size, deserialization without size/shape limits.\n",
"- Auth/credential: URL parsing, credential placement, Bearer-header destination scope (SSRF surface).\n",
"\n",
"Down-classification rules (apply in order, after the precedence rule):\n",
"1. If the trigger requires non-default configuration, an explicitly unusual input, or a code path that callers don't reach in practice → downgrade from high to medium.\n",
"2. If the impact is a panic / error rather than silent corruption or security breach → downgrade from critical to high, or from high to medium when the panic is recoverable.\n",
"3. If the issue is 'theoretically possible but no realistic trigger exists in this codebase' → low or omit, never high.\n",
"4. Purely-stylistic concerns (naming, formatting, complexity-for-its-own-sake) belong in low or info — never high — unless they directly hide a bug.\n",
```

**Step 3: Run Layer A test to verify it passes**

Run: `cargo test --bin quorum system_prompt_carves_out_trust_boundary -- --nocapture`
Expected: PASS.

**Step 4: Run full prompt-content test suite**

Run: `cargo test --bin quorum system_prompt -- --nocapture`
Expected: All existing tests at lines 1144 and 1492 either pass unchanged OR fail in a way we can audit and fix in Task 3.

**Step 5: Commit GREEN**

```bash
git add src/llm_client.rs
git commit -m "feat(review): carve out trust-boundary findings from down-classification

Inject a Precedence rule before the down-classification list so the
LLM stops demoting missing safety checks at trust boundaries
(network/filesystem/payload/auth) under rule 3 ('theoretically
possible') or rule 4 ('defensive programming').

Per gpt-5.4 + claude-opus-4.5 review, postpositive EXCEPTION clauses
are unreliable — frontier models compress them away. Precedence rule
placed first establishes carve-out as primary filter.

Also simplifies rule 4 to 'Purely-stylistic concerns' (was overly
broad 'Maintainability, naming, complexity, and defensive-programming'),
and extends priority item 4 with resource-bounds language.

Closes #118"
```

### Task 3: Audit and update existing prompt-content tests

**Files:**
- Modify: `src/review.rs` (only if tests break)

**Step 1: Run existing prompt-content tests**

Run: `cargo test --bin quorum -- --nocapture 2>&1 | grep -E "FAILED|test "`

**Step 2 (conditional): Update broken assertions**

If `system_prompt_deprioritizes_stylistic_findings_without_hard_reject` fails because it asserted on the literal phrase "Maintainability, naming, complexity, and defensive-programming", update the assertion to match the new phrase "Purely-stylistic concerns" without weakening the test's intent (still asserts deprioritization happens).

If `fp_precedent_policy_lives_in_system_prompt_as_hard_negative` fails — unlikely, this asserts on the historical_findings_policy block which we didn't touch — investigate before adjusting.

**Step 3: Commit if any updates**

```bash
git add src/review.rs
git commit -m "test(review): adjust existing prompt-content assertions for new wording

Refs #118"
```

### Task 4: Layer B test — RED

**Files:**
- Modify: `src/review.rs` (add new test in the `mod tests` block, alongside `parse_llm_response_unknown_severity_defaults_to_medium_end_to_end`)

**Step 1: Write the failing test**

```rust
#[test]
fn high_boundary_finding_survives_calibrator_at_high() {
    use crate::calibrator::{self, CalibratorConfig};
    use crate::feedback::FeedbackStore;
    use crate::review::Severity;

    // Synthetic LLM response: one HIGH boundary-security finding (SSRF on
    // a network call). Mirrors what the prompt edit unblocks the LLM from
    // generating.
    let json = r#"[
        {
            "title": "User-controlled base_url enables SSRF + credential leak",
            "description": "OpenAiClient::new accepts any http(s) base_url without host allowlist. Authorization: Bearer <api_key> is sent to whatever host the URL points at — a misconfigured or attacker-influenced QUORUM_BASE_URL exfiltrates the API key.",
            "severity": "high",
            "category": "security",
            "line_start": 155,
            "line_end": 172,
            "suggested_fix": "Reject URLs with embedded credentials in OpenAiClient::new; consider host allowlist with explicit override."
        }
    ]"#;

    let findings = crate::review::parse_llm_response(json, "test-model")
        .expect("synthetic JSON should parse");
    assert_eq!(findings.len(), 1, "synthetic input has exactly one finding");
    assert_eq!(findings[0].severity, Severity::High, "input severity must be HIGH");

    // Empty feedback store — calibrator should pass the finding through unmodified.
    let empty_store = FeedbackStore::default();
    let config = CalibratorConfig::default();
    let result = calibrator::calibrate(findings, &empty_store, &config);

    assert_eq!(result.findings.len(), 1, "boundary HIGH finding must survive calibrator with empty feedback store");
    assert_eq!(
        result.findings[0].severity,
        Severity::High,
        "boundary HIGH finding must retain HIGH severity through calibrator"
    );
    assert_eq!(result.suppressed, 0, "no suppression expected");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum high_boundary_finding_survives -- --nocapture`

Expected: FAIL — most likely a compile error if `FeedbackStore::default()` or `CalibratorConfig::default()` don't exist with those exact names. **If that's the case**, look up actual constructors via `cargo doc` or `grep -n "impl Default for FeedbackStore" src/feedback.rs` and adjust. Test must FAIL FOR A REAL REASON before we believe GREEN.

**Step 3: Commit RED**

```bash
git add src/review.rs
git commit -m "test(review): add RED test for HIGH boundary finding pipeline survival

Refs #118"
```

### Task 5: Layer B test — GREEN

**Step 1: Run Layer B test**

Run: `cargo test --bin quorum high_boundary_finding_survives -- --nocapture`

**Step 2: If FAIL — debug**

Most likely: `FeedbackStore::default()` doesn't exist, or `calibrator::calibrate` signature differs. Read `src/calibrator.rs:102` and `src/feedback.rs` to find correct constructors. Fix the test (NOT the production code — production code is correct; test just needs the right constructors).

**Step 3: If PASS — done.** No production change needed because the calibrator already passes through findings with empty feedback store. Layer B test exists as a regression guard against future calibrator changes that would inadvertently start suppressing HIGH boundary findings.

**Step 4: Commit GREEN**

```bash
git add src/review.rs
git commit -m "test(review): wire Layer B test against actual calibrator API

Refs #118"
```

### Task 6: Verification gate

**Step 1: Full test suite**

Run: `cargo test --bin quorum 2>&1 | tail -20`
Expected: all tests pass, count matches baseline ± 2 new tests.

**Step 2: Clippy**

Run: `cargo clippy --bin quorum --all-features 2>&1 | tail -30`
Expected: no NEW warnings on touched lines (`src/llm_client.rs` system_prompt area, `src/review.rs` new tests).

**Step 3: Release build**

Run: `cargo build --release 2>&1 | tail -10`
Expected: clean build.

### Task 7: Quorum self-review on changed files

```bash
quorum review src/llm_client.rs src/review.rs --no-color 2>&1 | head -80
```

Triage findings:
- **In-branch bugs** — fix via TDD micro-cycle.
- **Pre-existing bugs** — file as separate GH issues; do NOT fix in this branch.

### Task 8: Validation re-review (post-merge optional)

After merge to main and rebuild, re-run quorum review on `src/llm_client.rs` and `src/ast_grep.rs`:

```bash
quorum review src/llm_client.rs --no-color
quorum review src/ast_grep.rs --no-color
```

Expected: at least 2-3 of the previously-suppressed findings now surface at MEDIUM or higher (SSRF, no-retry, symlink, YAML-DoS). If NONE surface, the prompt edit didn't take and we need to iterate — reopen #118.

This validation is informational, not a merge gate.

### Task 9: Record verdicts in feedback store

For each finding the LLM now surfaces post-merge, record `tp` with provenance `human` (or `post_fix` if fixing in a follow-up). For any findings that surface but are FPs, record `fp`. This trains the calibrator and improves future precision.

```bash
quorum feedback --file src/llm_client.rs --finding "SSRF + credential exfil via base_url" --verdict tp --reason "validated post-prompt-fix; matches PAL/gpt-5.4 finding"
```

---

## Risk register

| Risk | Mitigation |
|---|---|
| Existing prompt-content tests break in unexpected ways | Task 3 audits; expect minor wording adjustments |
| Layer B uses calibrator API names that don't exist | Task 5 debug step — read actual API and fix test, not production |
| Prompt edit doesn't actually change LLM behavior | Task 8 post-merge validation; Layer C (#121) catches this systematically |
| New prompt language causes false-positive surge on stylistic code | Acceptable risk — Layer C / 5-file panel (#115) would catch; precision trend in `quorum stats` shows drift |
| Prompt-cache miss from edit (1024+ token prefix changes) | Acceptable one-time cost; the cached prefix re-establishes on next review batch |

## Acceptance checklist

- [ ] Precedence rule injected before down-classification list in `system_prompt()`.
- [ ] Rule 4 simplified to "Purely-stylistic concerns ...".
- [ ] Priority item 4 extended with resource-bounds language.
- [ ] Layer A test `system_prompt_carves_out_trust_boundary_findings_via_precedence_rule` lands in `src/review.rs`.
- [ ] Layer B test `high_boundary_finding_survives_calibrator_at_high` lands in `src/review.rs`.
- [ ] Existing prompt-content tests (1144, 1492) updated only as needed.
- [ ] `cargo test --bin quorum` passes (baseline + 2 new tests).
- [ ] `cargo clippy --bin quorum` clean on touched code.
- [ ] Quorum self-review on changed files (Task 7) — findings triaged.
- [ ] CHANGELOG entry under "Review" or "Prompt": "Trust-boundary findings no longer suppressed by down-classification rules".
- [ ] Issue #118 closed with reference to Task 8 validation results.
