# Open-Ended LLM Prompt Reframe

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Reframe the LLM review system prompt from a ranked priority checklist to an open-ended "find all bugs" framing, improving cold-start recall on unfamiliar codebases without increasing token cost.

**Architecture:** Single-function change to `OpenAiClient::system_prompt()` in `src/llm_client.rs`. The `<review_spec>` section gets rewritten from a numbered priority list to an open-ended framing with categories as non-exhaustive examples. The `<severity_rubric>` down-classification rule 3 ("theoretically possible → omit") is softened to "flag at low with reasoning." All other sections (response format, output hygiene, untrusted data warning, historical findings policy, suggested fix policy) stay unchanged. Prompt stays >1,024 tokens to preserve OpenAI/LiteLLM prompt caching.

**Tech Stack:** Rust, cargo test

---

### Task 1: Write test asserting the prompt is open-ended

**Files:**
- Modify: `src/llm_client.rs:2872-2880` (add test near existing `system_prompt_instructs_backticked_symbol_names_in_titles`)

**Step 1: Write the failing test**

Add this test after the existing system_prompt test at line 2880:

```rust
#[test]
fn system_prompt_uses_open_ended_framing() {
    let prompt = OpenAiClient::system_prompt();
    // Must NOT contain the old ranked priority list
    assert!(
        !prompt.contains("Prioritize, in this order:"),
        "system prompt must not use ranked priority framing"
    );
    // Categories must be presented as non-exhaustive examples, not a checklist
    assert!(
        prompt.contains("non-exhaustive"),
        "system prompt must present categories as non-exhaustive"
    );
    // Must still deprioritize style
    assert!(
        prompt.contains("style") && prompt.contains("Deprioritize"),
        "system prompt must still deprioritize pure style issues"
    );
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test --bin quorum -- system_prompt_uses_open_ended_framing
```

Expected: FAIL — current prompt contains "Prioritize, in this order:"

**Step 3: Commit the failing test**

```bash
git add src/llm_client.rs
git commit -m "test: add assertion for open-ended prompt framing (RED)"
```

---

### Task 2: Write test asserting down-classification rule 3 is softened

**Files:**
- Modify: `src/llm_client.rs` (test module)

**Step 1: Write the failing test**

```rust
#[test]
fn system_prompt_does_not_instruct_omission_of_theoretical_bugs() {
    let prompt = OpenAiClient::system_prompt();
    // Must not instruct the model to omit reachable bugs
    assert!(
        !prompt.contains("or omit"),
        "system prompt must not instruct omission of any reachable bugs"
    );
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test --bin quorum -- system_prompt_does_not_instruct_omission
```

Expected: FAIL — current prompt contains "low or omit, never high"

**Step 3: Commit**

```bash
git add src/llm_client.rs
git commit -m "test: assert theoretical bugs are flagged not omitted (RED)"
```

---

### Task 3: Write test asserting prompt stays above caching threshold

**Files:**
- Modify: `src/llm_client.rs` (test module)

**Step 1: Write the failing test (this one should pass immediately — it's a guard rail)**

```rust
#[test]
fn system_prompt_exceeds_caching_threshold() {
    let prompt = OpenAiClient::system_prompt();
    // OpenAI/LiteLLM prompt caching triggers at ~1024 tokens.
    // At ~4 chars/token, 1024 tokens ≈ 4096 chars.
    assert!(
        prompt.len() >= 4096,
        "system prompt must be >=4096 chars to trigger prompt caching (~1024 tokens). \
         Current length: {} chars",
        prompt.len()
    );
}
```

**Step 2: Run test to verify it passes**

```bash
cargo test --bin quorum -- system_prompt_exceeds_caching_threshold
```

Expected: PASS (current prompt is well above 3500 chars). This is a guard rail to prevent us from accidentally shrinking the prompt below the caching threshold during the rewrite.

**Step 3: Commit**

```bash
git add src/llm_client.rs
git commit -m "test: guard rail for prompt caching threshold"
```

---

### Task 4: Rewrite the review_spec section

**Files:**
- Modify: `src/llm_client.rs:1065-1078` (the `<review_spec>` section)

**Step 1: Replace the review_spec content**

Replace the current `<review_spec>` block (lines 1068-1078) with:

```rust
"<review_spec>\n",
"Review the code thoroughly for defects. Look for bugs across all categories — security vulnerabilities, logic errors, concurrency issues, resource leaks, error-handling gaps, correctness problems, and architectural flaws. These categories are non-exhaustive; flag any genuine defect you find.\n",
"\n",
"Examples of what to look for (not a ranked checklist — search broadly):\n",
"- Data corruption, crashes, authentication bypass, credential exposure\n",
"- Injection (SQL, command, template), XSS, SSRF, path traversal, insecure crypto, secrets in source\n",
"- Wrong conditionals, off-by-one, race conditions, incorrect state transitions, silent error swallowing\n",
"- Non-atomic writes, hidden invariants, APIs that mislead callers, missing resource bounds\n",
"- Missing input validation at trust boundaries, unbounded allocation from external input\n",
"\n",
"Deprioritize pure style, naming, formatting, and documentation issues. Only report a style issue when it directly causes or hides a defect.\n",
"\n",
"Do not invent defects to fill the array. But err on the side of flagging: a real bug reported at moderate confidence is more valuable than a real bug silently omitted. When uncertain, include the finding with an appropriate confidence score rather than omitting it.\n",
"</review_spec>\n",
```

**Step 2: Run the open-ended framing test**

```bash
cargo test --bin quorum -- system_prompt_uses_open_ended_framing
```

Expected: PASS

**Step 3: Run the caching threshold guard rail**

```bash
cargo test --bin quorum -- system_prompt_exceeds_caching_threshold
```

Expected: PASS (verify we didn't shrink too much)

**Step 4: Commit**

```bash
git add src/llm_client.rs
git commit -m "feat: rewrite review_spec to open-ended bug hunting framing"
```

---

### Task 5: Soften down-classification rule 3

**Files:**
- Modify: `src/llm_client.rs:1095-1099` (down-classification rules in `<severity_rubric>`)

**Step 1: Replace rule 3**

Change:
```
"3. If the issue is 'theoretically possible but no realistic trigger exists in this codebase' → low or omit, never high.\n",
```

To:
```
"3. If the issue is theoretically possible but no realistic trigger is apparent → flag at low severity with reasoning explaining the trigger conditions, rather than omitting.\n",
```

**Step 2: Run the omission test**

```bash
cargo test --bin quorum -- system_prompt_does_not_instruct_omission
```

Expected: PASS

**Step 3: Run all system_prompt tests**

```bash
cargo test --bin quorum -- system_prompt
```

Expected: all PASS (including the existing `backticked_symbol_names` test and the caching threshold test)

**Step 4: Commit**

```bash
git add src/llm_client.rs
git commit -m "feat: soften down-classification rule 3 — flag at low, don't omit"
```

---

### Task 6: Remove the precedence rule from severity_rubric

**Files:**
- Modify: `src/llm_client.rs:1089-1093` (precedence rule in `<severity_rubric>`)

The precedence rule references "the priority list (items 1-4)" which no longer exists after the review_spec rewrite. It also adds complexity that channels the model's attention.

**Step 1: Remove the precedence rule block**

Delete these lines from the severity_rubric:
```
"Precedence rule (check first): When a finding involves missing validation, missing safety check, or missing resource bound at a trust or external-input boundary, classify it by the priority list (items 1-4) and severity rubric based on actual impact and reachable input surface. Rules 3 and 4 below do not apply to such findings. Trust/external-input boundaries include:\n",
"- Network input: timeout layering, retry policy, error-body content in user-visible output.\n",
"- Filesystem: path canonicalization, symlink handling, size caps on user-influenced content.\n",
"- Payload/response: unbounded allocation from external size, deserialization without size/shape limits.\n",
"- Auth/credential: URL parsing, credential placement, Bearer-header destination scope (SSRF surface).\n",
"\n",
```

The trust boundary examples are already covered by the new `<review_spec>` bullet list. The down-classification exception is no longer needed since rule 3 no longer says "omit".

**Step 2: Run all system_prompt tests**

```bash
cargo test --bin quorum -- system_prompt
```

Expected: all PASS

**Step 3: Run caching threshold test**

```bash
cargo test --bin quorum -- system_prompt_exceeds_caching_threshold
```

Expected: PASS (verify prompt is still large enough after removal)

**Step 4: Commit**

```bash
git add src/llm_client.rs
git commit -m "refactor: remove precedence rule that referenced deleted priority list"
```

---

### Task 7: Update the comment on system_prompt function

**Files:**
- Modify: `src/llm_client.rs:1060-1064` (comment above `concat!`)

**Step 1: Update the comment**

Replace:
```rust
        // Stable system prompt (~1200 tokens). Kept intentionally long and
        // invariant across every review so that OpenAI/LiteLLM prompt caching
        // (triggered at >=1024 tokens of identical prefix) can hit on repeat
        // invocations. Do not interpolate file-specific data here; all variable
        // content belongs in the user message, placed after stable context.
```

With:
```rust
        // Stable system prompt (~1100 tokens). Open-ended "find all bugs"
        // framing — categories are non-exhaustive examples, not a ranked
        // checklist. Kept >1024 tokens so OpenAI/LiteLLM prompt caching
        // hits on repeat invocations. Do not interpolate file-specific
        // data here; variable content belongs in the user message.
```

**Step 2: Run all tests**

```bash
cargo test --bin quorum -- system_prompt
```

Expected: all PASS

**Step 3: Commit**

```bash
git add src/llm_client.rs
git commit -m "docs: update system_prompt comment to reflect open-ended framing"
```

---

### Task 8: Full test suite verification

**Files:** None (verification only)

**Step 1: Run all unit tests**

```bash
cargo test --bin quorum
```

Expected: all tests pass (1700+ tests)

**Step 2: Run clippy**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Expected: zero warnings

**Step 3: Release build**

```bash
cargo build --release
```

Expected: compiles successfully

---

## Notes

- Test 1 asserts on `"non-exhaustive"` as the stable anchor phrase (semantic contract: categories are examples, not a checklist). If you change the review_spec wording, ensure this word survives.
- Test 2 asserts `!contains("or omit")` — broad enough to catch any variant of "omit bugs" instruction, not coupled to one exact sentence.
- Prompt caching threshold: we test for >=4096 chars (~1024 tokens at ~4 chars/token). If the prompt shrinks below this, the guard rail test will catch it.
- The `speculative issues` phrase from the old prompt is replaced with "err on the side of flagging" — this is the key behavioral change for recall improvement.
- A/B testing against synthetic fixtures (auth_service.py, cache.rs) should be done after the branch is ready, comparing recall scores before and after.
