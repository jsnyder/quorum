# Issue #129 — MCP handler blocking-LLM-in-async fix (design)

**Date:** 2026-04-29
**Issue:** [#129 — mcp/handler.rs: blocking LLM calls run on async request handlers — stalls executor](https://github.com/jsnyder/quorum/issues/129)
**Scope:** Narrow — MCP handler only. Pipeline-side blocking (#81) is intentionally separate.

## Problem

`QuorumHandler::handle_chat`, `handle_debug`, and `handle_testgen` in `src/mcp/handler.rs` are currently `fn` (sync) but called from an async dispatcher. Each invokes `reviewer.review(prompt, model)` which is `reqwest::blocking`, parking the entire tokio worker for the full LLM round-trip — up to the 300s `QUORUM_HTTP_TIMEOUT` from #117.

A single in-flight chat/debug/testgen MCP request stalls the executor; under `worker_threads = 1` (test default) or low-worker production runtimes, the server becomes single-flight for the full LLM duration.

## Out of scope

- **#81** — `pipeline.rs` blocking semaphore `.join()` on current-thread runtimes. Same defect class, separate file, separate PR.
- **`handle_review`** — already async via `pipeline::review_source`.
- **`handle_feedback` / `handle_catalog`** — non-LLM, no blocking concern.
- **`async fn review` migration of the `LlmReviewer` trait.** Bigger refactor; tactical `spawn_blocking` is sufficient.
- **MCP request size caps (#130) and per-agent auth (#128).** Adjacent issues, separate PRs.
- **Blocking-pool starvation under sustained burst.** Cluster with #130.

## Architecture

### Field type

`QuorumHandler::llm_reviewer` flips from `Option<Box<dyn LlmReviewer>>` to `Option<Arc<dyn LlmReviewer>>`. Reason: `tokio::task::spawn_blocking` requires `F: FnOnce() -> R + Send + 'static`. Capturing `&self.llm_reviewer` violates `'static`. Cloning an `Arc` is a single atomic increment; the trait already declares `Send + Sync` (`src/pipeline.rs:101`).

The trait object's implicit `+ 'static` lifetime is satisfied automatically (no explicit lifetime annotation in the field type).

### Handler shape

Each of the three handlers converts `fn → async fn`:

```rust
async fn handle_chat(&self, params: ChatTool) -> Result<CallToolResult, String> {
    let reviewer = Arc::clone(self.llm_reviewer.as_ref()
        .ok_or("Chat requires QUORUM_API_KEY to be set.")?);
    let prompt: String = /* built from params + redact */;
    let model: String = self.config.model.clone();

    let _span = tracing::info_span!("mcp.spawn_blocking", tool = "chat").entered();
    let resp = tokio::task::spawn_blocking(move || reviewer.review(&prompt, &model))
        .await
        .map_err(|e| format!("review task failed: {}", e))?
        .map_err(|e| format!("LLM error: {}", e))?;

    Ok(CallToolResult::text_content(vec![resp.content.into()]))
}
```

`handle_debug` and `handle_testgen` follow the same shape with their respective prompt construction.

### Dispatcher

`handle_call_tool_request` in `src/mcp/handler.rs:367` currently does `result = self.handle_chat(tool)` (no `.await`). Three branches change to `self.handle_chat(tool).await` (and same for debug + testgen). Other branches (feedback, catalog, review) are already correctly matched to their handler's async-ness.

## Error handling

| Surface | Today | Post-fix |
|---|---|---|
| `JoinError` (panic / cancel) | Panic unwinds worker | `format!("review task failed: {}", e)` returned to MCP caller |
| `anyhow::Error` from `review` | `format!("LLM error: {}", e)` | Unchanged |
| MCP-side request cancellation | Sync handler runs to completion; result dropped | `JoinHandle` dropped; OS thread continues; LLM call completes server-side. Best-effort. |

**Timeouts.** No new MCP-boundary timeout. The inner `OpenAiClient` already enforces `total=300s`, `read=120s`, and `overall_retry_deadline=600s` (#117). An outer `tokio::time::timeout` would risk cancelling successful late responses and double up with retry logic.

**Cancellation cost.** `spawn_blocking` does not cancel the underlying OS thread when the `JoinHandle` is dropped. An MCP-side cancellation today already cannot reclaim in-flight reqwest::blocking work; this is unchanged behavior, documented explicitly.

## Test plan

### Existing tests (~15)

Mechanical `Box::new` → `Arc::new` swap on every `QuorumHandler` constructor. The single `Some(Box::new(FakeLlm))` at `src/mcp/handler.rs:751` flips to `Some(Arc::new(FakeLlm))`. No assertion changes.

### New: deterministic concurrency regression test

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn handle_chat_runs_concurrent_llm_calls_in_parallel() {
    use std::sync::Barrier;
    struct BarrierLlm { barrier: Arc<Barrier> }
    impl LlmReviewer for BarrierLlm {
        fn review(&self, _p: &str, _m: &str) -> anyhow::Result<LlmResponse> {
            // Blocks until N callers arrive. With the bug, only 1 caller
            // reaches this point because the executor is parked on the
            // first sync .review() call. The outer timeout then fires.
            self.barrier.wait();
            Ok(LlmResponse { content: "ok".into(), usage: Default::default() })
        }
    }
    const N: usize = 4;
    let reviewer = Arc::new(BarrierLlm { barrier: Arc::new(Barrier::new(N)) });
    let handler = Arc::new(/* QuorumHandler with this reviewer */);
    let mut joins = Vec::new();
    for _ in 0..N {
        let h = Arc::clone(&handler);
        joins.push(tokio::spawn(async move {
            h.handle_chat(ChatTool { code: "x".into(), question: "y".into() }).await
        }));
    }
    let all_done = async {
        for j in joins { j.await.unwrap().expect("chat handler ok"); }
    };
    tokio::time::timeout(Duration::from_secs(5), all_done)
        .await
        .expect("handle_chat serializes LLM calls — barrier deadlocked");
}
```

**Why barrier vs. throughput.** Wall-clock thresholds, even with 2.5× slack, have remaining flakiness modes on shared CI runners. A barrier converts the bug into a deterministic deadlock; the outer `tokio::time::timeout(5s)` converts that into a test failure with a clean assertion message. CI noise is irrelevant — the test either deadlocks (bug) or completes in microseconds (fix).

### New: behavioral smoke tests for `handle_debug` + `handle_testgen`

Each handler gets one `#[tokio::test]` against an `EchoLlm` fake that returns a known string. Test asserts the response content **contains** the fake's output — proves the prompt flowed through and the response surfaced (avoids the "Liar test" anti-pattern of `assert!(result.is_ok())`).

### Decision against integration tests

`wiremock` would add real network round-trips and isn't needed — the executor-blocking property is unit-testable with the `BarrierLlm` fake. The existing `OpenAiClient` retry/timeout behavior was already covered by #117's wiremock suite.

## Risks

| Risk | Mitigation |
|---|---|
| `Arc<dyn LlmReviewer>` ripple beyond MCP | Field is private to `QuorumHandler`. Pipeline takes `&dyn LlmReviewer` and is unchanged. |
| Lost MCP-side request cancellation cost | Unchanged from current behavior. Documented above. |
| Blocking-pool starvation under burst | Out of scope; cluster with #130. |
| Hidden borrow in `spawn_blocking` closure | We move owned `String` (prompt, model) and `Arc` clone — verified via PAL design review. |

## Definition of done

- [ ] `cargo test --bin quorum` passes (existing + 3 new tests).
- [ ] `cargo clippy` clean.
- [ ] `cargo build --release` clean.
- [ ] New concurrency regression test fails on the unfixed branch (proves it tests the bug).
- [ ] Quorum self-review + PAL review surface no new HIGH/CRITICAL on the diff.
- [ ] Behavioral smoke tests for `handle_debug` and `handle_testgen` assert response content shape, not just `Ok`.

## Files touched

- `src/mcp/handler.rs` — field type flip, 3 handler conversions, 3 dispatcher `.await`s, ~15 mechanical test-constructor updates, 3 new tests.

No public API change. No CLI flag change. No MCP tool schema change.

## Reviews

- **testing-antipatterns-expert**: no-go on initial tick-counter test (Anti-Pattern #7 flakiness, partial Q6 risk). Resolved by adopting throughput-based test, then deterministic barrier-based test per PAL feedback.
- **pal:thinkdeep (gpt-5.4)**: solid plan. Two tighten-ups adopted: deterministic concurrency primitive (Barrier replaces throughput) and tracing spans around blocking sections.

## Follow-up issues filed

- **#131** — Migrate `LlmReviewer` trait to `async fn review`. Eliminates `spawn_blocking` workaround entirely; closes #81 as a side effect. Wide blast radius across `pipeline.rs`, `mcp/handler.rs`, `llm_client.rs`. Tracked separately.
- **#132** — DRY MCP LLM-handler bodies (chat/debug/testgen) into a shared `spawn_blocking` helper. Deferred per PAL recommendation ("extract only after the first handler proves clean"). Likely subsumed by #131.

## Acceptance criteria (Phase 3)

| # | Criterion | Pinned by test |
|---|---|---|
| AC1 | A blocking `LlmReviewer::review()` invocation in `handle_chat`/`handle_debug`/`handle_testgen` no longer parks the tokio worker; concurrent MCP requests progress in parallel via the blocking pool. | T1 |
| AC2 | `handle_chat` returns the LLM response content unchanged through the new `async`/`spawn_blocking` path. | T1 (implicit Ok) + existing FakeLlm tests |
| AC3 | `handle_debug` returns the LLM response content unchanged. | T2 |
| AC4 | `handle_testgen` returns the LLM response content unchanged. | T3 |
| AC5 | No public API change, no CLI flag change, no MCP tool schema change. | Compile + existing handler suite |
| AC6 | All pre-existing handler tests pass after `Box<dyn>` → `Arc<dyn>` swap. | Existing ~15 constructor-touching tests |
| AC7 | `cargo test --bin quorum`, `cargo clippy --all-targets`, `cargo build --release` clean. | DoD checklist |
| AC8 | `JoinError` from a panicking blocking task is surfaced as `"review task failed: {e}"`. | Not asserted (gap G1) |

| Test | Asserts | AC |
|---|---|---|
| T1 `handle_chat_runs_concurrent_llm_calls_in_parallel` | N=4 concurrent `handle_chat().await` calls all reach `Barrier::wait()` and complete within 5s under `worker_threads=1`. | AC1, AC2 |
| T2 `handle_debug_returns_llm_content` | Serialized `CallToolResult` contains EchoLlm sentinel `debug-fake-output-2026-04-29`. | AC3, AC5 |
| T3 `handle_testgen_returns_llm_content` | Serialized `CallToolResult` contains EchoLlm sentinel `testgen-fake-output-2026-04-29`. | AC4, AC5 |

**Accepted coverage gaps:** G1 JoinError-on-panic path (low value vs. cost; behavior documented). G2 blocking-pool starvation (clustered with #130). G3 MCP-side request cancellation reclaiming in-flight `reqwest::blocking` work (unchanged behavior; documented). G4 async-trait migration (#131). G5 pipeline-side blocking (#81). G6 wiremock real-network coverage of the new path (`OpenAiClient` already covered by #117 wiremock suite).

**Phase 3 reviewers:** test-planning-implementation produced the AC table; testing-antipatterns-expert returned **GO** with two non-blocking nits (use `tempfile::TempDir` over `/tmp/unused-*.jsonl` — deferred for consistency with existing test convention; promote Task 7 Step 4 from optional to required — Task 1 Step 2 already covers this RED check).
