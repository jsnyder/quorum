# Agent Loop Test Plan Review: Anti-Pattern Analysis

**Date**: 2026-03-25
**Scope**: Proposed test plan for multi-turn agent loop
**Reviewer**: TPIA

---

## Summary

- **Anti-patterns found:** 4 of 13 are relevant risks
- **Critical issues:** 1 (missing state machine tests)
- **Test suite health projection:** Good, with gaps noted below

---

## Findings

### [Critical] Anti-Pattern #5: Testing Internal Implementation — RISK in planned tests

**Evidence:**
Tests 9-10 (spinner_writes_to_stderr, spinner_disabled_when_not_tty) test output mechanics
rather than observable behavior. The spinner is a presentation concern. If you later swap
eprint! for indicatif or a progress callback, these tests break for no behavioral reason.

Tests 11-12 (async_context7_fetcher, async_context7_timeout) are already well-tested in
`context_enrichment.rs` based on the existing codebase patterns. Adding a parallel set
in the agent module duplicates coverage.

**Impact:**
Refactoring the progress UI will break tests that provide no agent-loop-specific value.

**Remediation:**
- Drop tests 9-10 from the agent loop test file. If spinner behavior matters, test it in
  `output/mod.rs` where `Style::detect()` already lives.
- Drop tests 11-12 unless the agent loop has its own Context7 integration distinct from
  the pipeline. If so, test the agent-loop-specific integration point only.

---

### [High] Anti-Pattern #4: Testing Wrong Functionality — Missing critical state transitions

**Evidence:**
The proposed plan tests boundaries (max_iterations, max_tool_calls, max_bytes) but misses
the core state machine transitions:

1. **Message history accumulation**: After a tool call round, do tool results appear
   correctly as `role: "tool"` messages with matching `tool_call_id`? Malformed message
   history is the #1 cause of agent loop failures in practice.

2. **Partial findings on early termination**: When max_iterations is hit mid-loop, does
   the agent attempt to parse whatever partial content exists, or does it return an error?
   Your plan says "returns what it has" but has no test proving this.

3. **Tool call argument parsing**: The LLM returns `arguments` as a JSON string. What
   happens when it returns malformed JSON arguments? This is different from
   "tool_error_continues" — that tests tool execution failure, not argument parsing failure.

4. **Cumulative byte counting**: max_bytes_read applies across iterations. Test that
   reading 60KB in round 1 and requesting 50KB in round 2 triggers the limit (total 110KB > 100KB).

5. **Mixed tool calls + content**: Some models return both `tool_calls` and `content` in
   the same message. The loop must handle this edge case.

**Impact:**
The highest-risk behaviors (message history correctness, partial results) are untested.

**Remediation:**
Add these tests:
```
agent_loop_message_history_correct — verify tool results have correct role/id pairing
agent_loop_partial_findings_on_limit — max_iterations hit, best-effort parse
agent_loop_malformed_tool_arguments — LLM sends bad JSON in arguments field
agent_loop_cumulative_byte_limit — bytes accumulate across iterations
agent_loop_mixed_content_and_tools — model returns both in one response
```

---

### [Medium] Anti-Pattern #3: Wrong Proportions — Over-indexing on boundary tests

**Evidence:**
Of 8 agent tests, 3 are boundary/limit tests (max_iterations, max_tool_calls, max_bytes).
That is 37% of tests dedicated to configuration limits, vs. 2 tests for the actual
multi-turn conversation flow (tests 2-3).

The agent loop is fundamentally a **state machine** (send -> parse -> execute -> accumulate -> decide).
The interesting bugs are in state transitions, not in counter comparisons.

**Impact:**
Confidence in the happy path is lower than confidence in edge cases. Boundary tests
are important but should not outnumber core flow tests.

**Remediation:**
Rebalance: 5-6 core flow tests, 3 boundary tests, 2-3 error handling tests.
The boundary tests are trivially correct if the counter logic is right; the
flow tests catch real integration bugs.

---

### [Medium] Anti-Pattern #9: Treating Test Code as Second-Class — Fake reviewer duplication

**Evidence:**
The codebase already has three separate `FakeDirectReviewer` / `FakeLlmReviewer`
implementations across `agent.rs`, `pipeline.rs`, and `auto_calibrate.rs`. Each is
slightly different. Your plan will add a fourth: a multi-turn fake that returns
different responses per call.

Current fakes (from grep):
- `src/agent.rs:82` — FakeDirectReviewer(String), returns same response always
- `src/pipeline.rs:375` — FakeLlmReviewer with `with_findings()`, `empty()`, `failing()`
- `src/auto_calibrate.rs:173` — FakeLlm
- `src/mcp/handler.rs:495` — FakeLlm

**Impact:**
When `LlmReviewer` trait changes, 4+ test doubles must be updated. Inconsistent
behaviors across fakes create subtle test bugs.

**Remediation:**
Before writing agent loop tests, extract a shared `FakeReviewer` into a test support
module (e.g., `src/test_support.rs` as already planned in TEST_STRATEGY.md section 3.1).
The multi-turn fake needs a `VecDeque<String>` of responses, returning them in sequence.
This single fake replaces all current variants:

```rust
// src/test_support.rs
pub struct FakeReviewer {
    responses: Mutex<VecDeque<Result<String, String>>>,
    captured_prompts: Mutex<Vec<String>>,
}
impl FakeReviewer {
    pub fn sequence(responses: Vec<&str>) -> Self { ... }
    pub fn always(response: &str) -> Self { ... }
    pub fn failing(msg: &str) -> Self { ... }
    pub fn captured_prompts(&self) -> Vec<String> { ... }
}
```

---

## Anti-Patterns NOT Found (good signs)

- **#1 (Unit without integration)**: The plan correctly mixes unit-level agent tests
  with the tool registry's tempdir-based filesystem tests.
- **#2 (Integration without unit)**: Agent loop tests use fakes for the LLM, keeping
  them fast and deterministic. Good.
- **#7 (Flaky tests)**: No timing dependencies, no real network calls, tempdir isolation.
  The existing patterns in tools.rs are solid.
- **#8 (Manual tests)**: CI is already configured per TEST_STRATEGY.md section 8.
- **#12 (Reinventing framework utilities)**: Using tempfile, anyhow, serde_json
  appropriately. No custom wait/retry utilities.

---

## Missing Tests (not anti-patterns, just gaps)

### Security boundary in agent context

The agent loop gives the LLM access to `ToolRegistry`. Test that:
- Agent cannot read files outside repo root even across multiple turns
- Agent's total byte consumption is tracked correctly across tool calls
- Tool results are not passed through `redact::redact_secrets` (or ARE, depending on design intent — this needs a decision)

### LLM response format evolution

The `chat_with_tools` method in `llm_client.rs` parses `tool_calls` from the OpenAI
response format. Test the agent loop's handling of:
- Empty tool_calls array (should treat as FinalContent)
- tool_calls with no arguments field
- Responses API format differences (if codex models will use agent mode)

### Convergence behavior

Add a test that the agent actually converges: after reading files and investigating,
it produces *better* findings than a single-pass review. This is an integration test
with real (fixture) files where the agent's tool usage demonstrably improves the review.

---

## Recommended Test List (final, prioritized)

### Core flow (implement first)
1. `agent_loop_no_tool_calls` — direct findings, no tool use
2. `agent_loop_single_tool_round` — one round of tool calls, then findings
3. `agent_loop_multi_turn` — 2+ rounds before final findings
4. `agent_loop_message_history_format` — verify tool results have correct role/tool_call_id
5. `agent_loop_cumulative_byte_tracking` — bytes accumulate across rounds

### Boundary enforcement
6. `agent_loop_max_iterations_stops` — loop terminates, returns best-effort findings
7. `agent_loop_max_tool_calls_stops` — stops mid-iteration if call count exceeded
8. `agent_loop_max_bytes_stops` — cumulative read limit enforced

### Error handling
9. `agent_loop_tool_execution_error` — bad path/args, loop continues
10. `agent_loop_malformed_tool_arguments` — LLM sends invalid JSON args
11. `agent_loop_empty_findings_valid` — clean code, empty array result

### Do NOT implement (dropped from original plan)
- spinner_writes_to_stderr — test in output module if needed
- spinner_disabled_when_not_tty — test in output module if needed
- async_context7_fetcher — already covered in context_enrichment
- async_context7_timeout — already covered in context_enrichment

---

## Prerequisites

Before writing these tests:
1. Extract shared `FakeReviewer` into `src/test_support.rs`
2. Design the multi-turn fake to return a sequence of `LlmTurnResult` values
   (not just strings), since `chat_with_tools` returns `LlmTurnResult::ToolCalls`
   or `LlmTurnResult::FinalContent`
3. Decide: does the agent loop use `LlmReviewer::review()` (sync, single prompt)
   or `OpenAiClient::chat_with_tools()` (async, multi-turn)? The current agent.rs
   uses `review()` for single-pass. The multi-turn loop needs `chat_with_tools()`,
   which means either a new trait or a concrete dependency on OpenAiClient.
