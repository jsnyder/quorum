# Issue #129 — MCP spawn_blocking fix Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Stop `handle_chat`, `handle_debug`, `handle_testgen` in `src/mcp/handler.rs` from blocking the tokio executor on synchronous LLM calls. Wrap each `reviewer.review()` invocation in `tokio::task::spawn_blocking` so blocking IO moves to the dedicated blocking thread pool.

**Architecture:** Flip `QuorumHandler::llm_reviewer` from `Option<Box<dyn LlmReviewer>>` to `Option<Arc<dyn LlmReviewer>>` so a clone can be moved into a `'static` blocking task. Convert the three sync handlers to `async fn`. Inside each, clone the `Arc`, take owned `String` for prompt + model, and `spawn_blocking` the LLM call. Dispatcher gains `.await` on the three branches.

**Tech Stack:** Rust 1.85+, tokio (multi_thread runtime), reqwest::blocking (existing client; unchanged), `std::sync::{Arc, Barrier}` for the deterministic concurrency test.

**Reference design:** `docs/plans/2026-04-29-issue-129-mcp-spawn-blocking-design.md`.

---

## Task 1: RED — Deterministic concurrency regression test

**Files:**
- Modify: `src/mcp/handler.rs` (append to `mod tests`, near line ~750)

**Step 1: Add helper imports + `BarrierLlm` fake + concurrency regression test**

Append to `mod tests` block:

```rust
    use std::sync::Barrier;
    use std::time::Duration;

    /// Fake reviewer that synchronizes on a Barrier inside `review()`.
    /// Used to prove that concurrent MCP handler invocations actually run
    /// in parallel on the blocking pool — if the executor is parked on a
    /// sync `.review()` call, only one caller will ever reach `barrier.wait()`
    /// and the test will deadlock until the outer `tokio::time::timeout`.
    struct BarrierLlm {
        barrier: Arc<Barrier>,
    }

    impl LlmReviewer for BarrierLlm {
        fn review(
            &self,
            _prompt: &str,
            _model: &str,
        ) -> anyhow::Result<crate::llm_client::LlmResponse> {
            // std::sync::Barrier::wait is blocking — intentional. We need a
            // primitive that does not yield to the tokio executor; a
            // tokio::sync::Barrier would defeat the test.
            self.barrier.wait();
            Ok(crate::llm_client::LlmResponse {
                content: "ok".into(),
                usage: Default::default(),
            })
        }
    }

    fn handler_with_barrier_llm(barrier: Arc<Barrier>) -> QuorumHandler {
        QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: Some("sk-test".into()),
                model: "test-model".into(),
            },
            feedback_store: FeedbackStore::new(std::path::PathBuf::from(
                "/tmp/unused-barrier.jsonl",
            )),
            llm_reviewer: Some(Box::new(BarrierLlm { barrier })),
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn handle_chat_runs_concurrent_llm_calls_in_parallel() {
        // INVARIANT: handle_chat must not block the tokio worker.
        //
        // Bug case: with one worker, Task 1 polls, calls sync handle_chat,
        // which calls sync reviewer.review, which blocks on barrier.wait().
        // The worker is now parked. Tasks 2-N never get polled. Barrier
        // never releases. The 5-second outer timeout fires.
        //
        // Fix case: handle_chat is async and offloads .review() to the
        // blocking pool. All N tasks reach the barrier on separate threads;
        // barrier releases; all complete in microseconds.
        const N: usize = 4;
        let barrier = Arc::new(Barrier::new(N));
        let handler = Arc::new(handler_with_barrier_llm(Arc::clone(&barrier)));

        let mut joins = Vec::new();
        for _ in 0..N {
            let h = Arc::clone(&handler);
            joins.push(tokio::spawn(async move {
                // NOTE: handle_chat is currently sync — no .await. Once the
                // fix lands, this line gains .await (see Task 3).
                h.handle_chat(ChatTool {
                    code: "x".into(),
                    question: "y".into(),
                })
            }));
        }

        let all_done = async {
            for j in joins {
                j.await
                    .expect("task panicked")
                    .expect("chat handler returned err");
            }
        };

        tokio::time::timeout(Duration::from_secs(5), all_done)
            .await
            .expect("handle_chat serializes LLM calls — barrier deadlocked");
    }
```

**Step 2: Run the new test — verify it fails**

```bash
cargo test --bin quorum --test-threads=1 handle_chat_runs_concurrent_llm_calls_in_parallel -- --nocapture 2>&1 | tail -40
```

Expected: test fails after ~5s with `handle_chat serializes LLM calls — barrier deadlocked`. The test must demonstrate the bug by deadlocking.

**Step 3: Commit the failing test**

```bash
git add src/mcp/handler.rs
git commit -m "$(cat <<'EOF'
test(mcp): RED — barrier-based concurrency regression test for #129

The test spawns N=4 concurrent handle_chat invocations against a fake
LlmReviewer that synchronizes on a std::sync::Barrier. With the bug
(sync handler blocking the worker), only Task 1 reaches the barrier
and the 5s timeout fires. With the fix (spawn_blocking), all 4 reach
the barrier in parallel on the blocking pool and the test completes
near-instantly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Box → Arc field-type swap (mechanical)

**Files:**
- Modify: `src/mcp/handler.rs:26` (struct field)
- Modify: `src/mcp/handler.rs:40-44` (production constructor)
- Modify: `src/mcp/handler.rs` ~15 test constructor sites (search for `llm_reviewer:`)

**Step 1: Change struct field**

`src/mcp/handler.rs:26`:

```rust
// Before:
llm_reviewer: Option<Box<dyn LlmReviewer>>,

// After:
llm_reviewer: Option<Arc<dyn LlmReviewer>>,
```

**Step 2: Update production constructor**

`src/mcp/handler.rs:40-44`:

```rust
// Before:
let llm_reviewer: Option<Box<dyn LlmReviewer>> = if let Ok(api_key) = config.require_api_key() {
    Some(Box::new(OpenAiClient::new(&config.base_url, api_key)?))

// After:
let llm_reviewer: Option<Arc<dyn LlmReviewer>> = if let Ok(api_key) = config.require_api_key() {
    Some(Arc::new(OpenAiClient::new(&config.base_url, api_key)?))
```

**Step 3: Update all test constructors**

Search for `llm_reviewer:` in `src/mcp/handler.rs` and update every site:

- All `llm_reviewer: None` → unchanged (no type-bearing constructor)
- `llm_reviewer: Some(Box::new(FakeLlm))` (line 751) → `llm_reviewer: Some(Arc::new(FakeLlm))`
- `llm_reviewer: Some(Box::new(BarrierLlm { ... }))` (Task 1) → `llm_reviewer: Some(Arc::new(BarrierLlm { ... }))`

```bash
# Find all sites:
rtk grep -n "Box::new(.*FakeLlm\|Box::new(.*BarrierLlm" src/mcp/handler.rs
```

Replace each with `Arc::new(...)`.

**Step 4: Run all tests — verify they still pass (concurrency test still fails)**

```bash
rtk cargo test --bin quorum mcp::handler 2>&1 | tail -20
```

Expected: every existing test still passes; the new `handle_chat_runs_concurrent_llm_calls_in_parallel` still fails (we haven't implemented the fix yet). The Box→Arc swap is a no-op refactor.

**Step 5: Commit**

```bash
git add src/mcp/handler.rs
git commit -m "$(cat <<'EOF'
refactor(mcp): Box<dyn LlmReviewer> -> Arc<dyn LlmReviewer> for #129

Mechanical type swap to enable cloning the trait object into the
'static closure required by tokio::task::spawn_blocking. Behavior
unchanged. Existing tests pass; the #129 concurrency regression test
still fails (fix lands in Task 3).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: GREEN — Convert `handle_chat` to async + spawn_blocking

**Files:**
- Modify: `src/mcp/handler.rs:231-258` (handle_chat body)
- Modify: `src/mcp/handler.rs:391-395` (dispatcher branch for "chat")
- Modify: `src/mcp/handler.rs` (Task 1 test) — add `.await` to the `handle_chat` call

**Step 1: Convert `handle_chat` to async + spawn_blocking**

Replace `src/mcp/handler.rs:231-258` (entire `fn handle_chat`):

```rust
async fn handle_chat(&self, params: ChatTool) -> Result<CallToolResult, String> {
    let reviewer = Arc::clone(
        self.llm_reviewer
            .as_ref()
            .ok_or("Chat requires QUORUM_API_KEY to be set.")?,
    );

    // Build prompt (same as before — preserve existing redaction + format)
    let redacted_code = redact::redact_secrets(&params.code);
    let mut prompt = format!(
        "Answer this question about the code in `{}`:\n\n{}\n\nCode:\n",
        params.file_path.as_deref().unwrap_or("(no file)"),
        params.question,
    );
    if let Some(file_path) = &params.file_path {
        let lang = Language::from_path(std::path::Path::new(file_path));
        // ... preserve existing language-fence logic exactly as in current handle_chat
    }
    prompt.push_str(&format!("```{}\n{}\n```\n", "rust", redacted_code));
    // ^^ NOTE: the current handler at L231-258 has its own prompt-building
    //         branches; preserve verbatim, just hoist into the new async fn.

    let model = self.config.model.clone();

    let _span = tracing::info_span!("mcp.spawn_blocking", tool = "chat").entered();
    let resp = tokio::task::spawn_blocking(move || reviewer.review(&prompt, &model))
        .await
        .map_err(|e| format!("review task failed: {}", e))?
        .map_err(|e| format!("LLM error: {}", e))?;

    Ok(CallToolResult::text_content(vec![resp.content.into()]))
}
```

> **IMPORTANT:** The above shows the *shape* of the change. Read the actual current `handle_chat` body in the worktree first and preserve its exact prompt-building logic. The only changes are:
> 1. `fn` → `async fn`
> 2. `let reviewer = self.llm_reviewer.as_ref()...` → `let reviewer = Arc::clone(self.llm_reviewer.as_ref()...)`
> 3. Owned `model: String` clone before the call
> 4. The `reviewer.review(&prompt, &self.config.model).map_err(...)?` line is replaced with the `spawn_blocking` block
> 5. Add the `tracing::info_span!` on the line above

**Step 2: Update dispatcher branch for "chat"**

`src/mcp/handler.rs:391-395`:

```rust
// Before:
"chat" => {
    let tool: ChatTool = serde_json::from_value(args_value)
        .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
    self.handle_chat(tool)
}

// After:
"chat" => {
    let tool: ChatTool = serde_json::from_value(args_value)
        .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
    self.handle_chat(tool).await
}
```

**Step 3: Update Task 1 test to await `handle_chat`**

In the regression test added in Task 1, change:

```rust
joins.push(tokio::spawn(async move {
    h.handle_chat(ChatTool { code: "x".into(), question: "y".into() })
}));
```

to:

```rust
joins.push(tokio::spawn(async move {
    h.handle_chat(ChatTool { code: "x".into(), question: "y".into() }).await
}));
```

**Step 4: Run the regression test — verify it now passes**

```bash
rtk cargo test --bin quorum handle_chat_runs_concurrent_llm_calls_in_parallel 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed; 0 failed`. The barrier releases instantly because all 4 invocations land on the blocking pool.

**Step 5: Run the full handler test module — verify nothing else regressed**

```bash
rtk cargo test --bin quorum mcp::handler 2>&1 | tail -10
```

Expected: all existing handler tests still pass.

**Step 6: Commit**

```bash
git add src/mcp/handler.rs
git commit -m "$(cat <<'EOF'
fix(mcp): handle_chat — spawn_blocking for #129

Wrap the synchronous reviewer.review() call in tokio::task::spawn_blocking
so the blocking HTTP round-trip moves to the dedicated blocking thread
pool instead of parking the tokio worker. Convert the handler to async fn
and update the dispatcher branch to .await it.

Concurrency regression test now passes — 4 concurrent handle_chat calls
all reach the test barrier on independent blocking-pool threads and
release together rather than serializing on a single worker.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Convert `handle_debug` + behavioral smoke test

**Files:**
- Modify: `src/mcp/handler.rs:260-275` (handle_debug body)
- Modify: `src/mcp/handler.rs:397-401` (dispatcher branch for "debug")
- Modify: `src/mcp/handler.rs` (`mod tests`) — add behavioral smoke test

**Step 1: Add the RED behavioral smoke test**

Append to `mod tests`:

```rust
    /// Reviewer fake that returns a known sentinel string. Used to verify
    /// that the prompt actually flowed through the handler and the response
    /// surfaced to the caller — avoids the assert!(result.is_ok()) "Liar test"
    /// anti-pattern.
    struct EchoLlm {
        sentinel: &'static str,
    }

    impl LlmReviewer for EchoLlm {
        fn review(
            &self,
            _prompt: &str,
            _model: &str,
        ) -> anyhow::Result<crate::llm_client::LlmResponse> {
            Ok(crate::llm_client::LlmResponse {
                content: self.sentinel.into(),
                usage: Default::default(),
            })
        }
    }

    fn handler_with_echo_llm(sentinel: &'static str) -> QuorumHandler {
        QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: Some("sk-test".into()),
                model: "test-model".into(),
            },
            feedback_store: FeedbackStore::new(std::path::PathBuf::from(
                "/tmp/unused-echo.jsonl",
            )),
            llm_reviewer: Some(Arc::new(EchoLlm { sentinel })),
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn handle_debug_returns_llm_content() {
        const SENTINEL: &str = "debug-fake-output-2026-04-29";
        let handler = handler_with_echo_llm(SENTINEL);
        let result = handler
            .handle_debug(DebugTool {
                file_path: "f.rs".into(),
                code: "x".into(),
                error: "e".into(),
            })
            .await
            .expect("handle_debug ok");
        let json = serde_json::to_string(&result).expect("serialize result");
        assert!(
            json.contains(SENTINEL),
            "response must contain sentinel from EchoLlm; got: {}",
            json
        );
    }
```

**Step 2: Run new test — verify it fails to compile (handle_debug is still sync)**

```bash
rtk cargo test --bin quorum handle_debug_returns_llm_content 2>&1 | tail -10
```

Expected: compile error — `.await` cannot be applied to a non-`Future` (because `handle_debug` is currently sync).

**Step 3: Convert `handle_debug` to async + spawn_blocking**

Apply the exact same shape transformation to `src/mcp/handler.rs:260-275` as Task 3 applied to `handle_chat`. Preserve the existing prompt-building (redact, format!) verbatim; only swap `fn` → `async fn`, take `Arc::clone(reviewer)`, owned `model` String, replace `.review()` line with `spawn_blocking`, add `tracing::info_span!`.

**Step 4: Update dispatcher branch for "debug"**

`src/mcp/handler.rs:397-401`:

```rust
"debug" => {
    let tool: DebugTool = serde_json::from_value(args_value)
        .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
    self.handle_debug(tool).await
}
```

**Step 5: Run test — verify it now passes**

```bash
rtk cargo test --bin quorum handle_debug_returns_llm_content 2>&1 | tail -10
```

Expected: `1 passed`.

**Step 6: Commit**

```bash
git add src/mcp/handler.rs
git commit -m "$(cat <<'EOF'
fix(mcp): handle_debug — spawn_blocking for #129

Same spawn_blocking conversion as handle_chat. Adds a behavioral smoke
test that asserts the response contains the fake reviewer's sentinel
string (proves the prompt flowed through and the response surfaced),
not just is_ok().

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Convert `handle_testgen` + behavioral smoke test

**Files:**
- Modify: `src/mcp/handler.rs:277-307` (handle_testgen body)
- Modify: `src/mcp/handler.rs:403-407` (dispatcher branch for "testgen")
- Modify: `src/mcp/handler.rs` (`mod tests`) — add behavioral smoke test

**Step 1: Add the RED behavioral smoke test**

Append to `mod tests`:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn handle_testgen_returns_llm_content() {
        const SENTINEL: &str = "testgen-fake-output-2026-04-29";
        let handler = handler_with_echo_llm(SENTINEL);
        let result = handler
            .handle_testgen(TestgenTool {
                file_path: "f.rs".into(),
                code: "fn x() {}".into(),
                framework: None,
            })
            .await
            .expect("handle_testgen ok");
        let json = serde_json::to_string(&result).expect("serialize result");
        assert!(
            json.contains(SENTINEL),
            "response must contain sentinel from EchoLlm; got: {}",
            json
        );
    }
```

**Step 2: Run new test — verify compile error**

```bash
rtk cargo test --bin quorum handle_testgen_returns_llm_content 2>&1 | tail -10
```

Expected: compile error (handle_testgen still sync).

**Step 3: Convert `handle_testgen` to async + spawn_blocking**

Apply the same transformation to `src/mcp/handler.rs:277-307`. Preserve all existing logic (redact, language detection, framework_hint, format!) verbatim.

**Step 4: Update dispatcher branch for "testgen"**

`src/mcp/handler.rs:403-407`:

```rust
"testgen" => {
    let tool: TestgenTool = serde_json::from_value(args_value)
        .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
    self.handle_testgen(tool).await
}
```

**Step 5: Run test — verify pass**

```bash
rtk cargo test --bin quorum handle_testgen_returns_llm_content 2>&1 | tail -10
```

Expected: `1 passed`.

**Step 6: Commit**

```bash
git add src/mcp/handler.rs
git commit -m "$(cat <<'EOF'
fix(mcp): handle_testgen — spawn_blocking for #129

Final of three handler conversions. Same shape as handle_chat and
handle_debug. Behavioral smoke test asserts response contains the
fake reviewer's sentinel string.

Closes #129.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: CHANGELOG entry

**Files:**
- Modify: `CHANGELOG.md`

**Step 1: Add entry under [Unreleased]**

Add to `CHANGELOG.md` under the `[Unreleased]` heading. If a `### Reliability` section exists (added by #117), append; otherwise create one:

```markdown
### Reliability

- **MCP handlers no longer block the tokio executor on LLM calls** (#129). The `chat`, `debug`, and `testgen` handlers wrap their synchronous `reqwest::blocking` reviewer calls in `tokio::task::spawn_blocking`, moving the LLM round-trip to the dedicated blocking thread pool. Previously a single in-flight LLM call could stall all other MCP requests for up to the 300s `QUORUM_HTTP_TIMEOUT`.
```

**Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "$(cat <<'EOF'
docs(changelog): #129 MCP spawn_blocking Reliability entry

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Final verification

**Files:** none (run-only)

**Step 1: Full test suite**

```bash
rtk cargo test --bin quorum 2>&1 | tail -20
```

Expected: all tests pass; new tests visible (`handle_chat_runs_concurrent_llm_calls_in_parallel`, `handle_debug_returns_llm_content`, `handle_testgen_returns_llm_content`).

**Step 2: Clippy**

```bash
rtk cargo clippy --all-targets 2>&1 | tail -20
```

Expected: clean (no warnings on changed lines).

**Step 3: Release build**

```bash
rtk cargo build --release 2>&1 | tail -10
```

Expected: clean compile.

**Step 4: Confirm RED reproduces (sanity)**

Optional but recommended: temporarily revert just `Task 3 Step 3` (the `.await` add in the test) on a scratch branch, leave handler async — confirm the test fails. Then revert. This proves the test pins the invariant, not just compile shape.

```bash
git stash
# Manually edit the test in mod tests to remove .await on handle_chat — confirm fail
# Then restore:
git stash pop
```

(Skip if confident from Task 1/3 sequencing.)

---

## Done

- 5 commits (Tasks 1, 2, 3, 4, 5, 6) covering test scaffolding, refactor, three handler conversions, and changelog.
- 3 new tests: deterministic concurrency regression for `chat`, behavioral smoke tests for `debug` and `testgen`.
- Closes #129.
- No public API change.
- No CLI flag change.
- No MCP tool schema change.
- Pipeline-side blocking (#81) remains intentionally separate.

Proceed to **Phase 5 — Verification**, then **Phase 6 — Quorum + PAL self-review**, then **Phase 7 — Feedback recording**, then **Phase 8 — Code review + finish**.
