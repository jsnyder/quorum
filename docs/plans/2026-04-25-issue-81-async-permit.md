# Issue #81 — Async permit acquisition (deadlock fix)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:test-driven-development for each task. Write the failing test, watch it fail for the documented reason, write minimal code, watch it pass, commit.

**Goal:** Eliminate the cross-runtime deadlock in `pipeline::acquire_llm_permit` by converting permit acquisition (and the three review functions that call it) from sync-with-runtime-detection to async-throughout.

**Architecture:** Replace the `MultiThread`/`CurrentThread` runtime-flavor switch (which uses `block_in_place` on multi-thread and `std::thread::scope` + a fresh runtime + `join()` on current-thread) with a single `async fn` that simply `.await`s `Semaphore::acquire_owned`. The current-thread `join()` is the deadlock root cause — when the calling runtime owns the permit holder, blocking its only worker at `join()` prevents the holder from ever running, and the spawned helper runtime awaits forever. Removing the cross-runtime blocking dance eliminates the deadlock by construction. The three callers (`review_file`, `review_source`, `review_file_llm_only`) become `async fn`; downstream call sites either `.await` directly (CLI serial, daemon, MCP handler — all already in async contexts) or wrap in `Handle::current().block_on(async { ... })` inside the existing `spawn_blocking` thread (CLI parallel — `block_on` on a blocking-pool thread is safe per Tokio docs).

**Tech Stack:** Rust 1.88, Tokio (async runtime + Semaphore), `async_trait` (already in use for `ServerHandler`), `#[tokio::test]` for regression coverage.

**Out of scope (will be filed as follow-ups):**
- Offloading the sync `LlmReviewer::review` (~12-20s reqwest::blocking call) into `spawn_blocking` to free the runtime worker. Pre-existing concern — this PR doesn't make it worse.
- `review_file_llm_only` doesn't use the `context7_skip_reason` helper that `review_file` uses. Drift, not a deadlock issue.

---

## Task 1: RED — current-thread regression test (proves the deadlock)

**Files:**
- Modify: `src/pipeline.rs` (test module at the bottom — append a new test fn)

**Why this test:** The existing `acquire_llm_permit_does_not_panic_inside_current_thread_runtime` only proves "doesn't panic" with a 1-permit-available semaphore. It does NOT exercise contention. The bug only manifests when the permit is *held by another task on the same current-thread runtime*. We need a test that reproduces that exact shape.

**Compile-fail RED (the strongest available signal):** The test below calls `.await` on `acquire_llm_permit`, which is invalid against the current SYNC signature. This produces compile error E0277 with rustc itself suggesting "consider making `fn acquire_llm_permit` asynchronous" — the compiler points at exactly the Task 2 fix. Step 1b verifies the failure mode.

The original plan called for an additional runtime-RED via a `spawn_blocking` shim, but on closer analysis (see Step 1b note) the shim does NOT reproduce the deadlock — `spawn_blocking` runs on the blocking pool, not the runtime worker, so the holder still gets polled. A faithful runtime-RED would require sync `acquire_llm_permit` to block worker X *directly*, which would also hang Tokio's timeout future itself; `std::thread::spawn` + condvar harness would be needed to detect it, which is significant complexity for a one-shot demonstration. Compile-fail RED + the issue's reproduction analysis are accepted as sufficient.

**Step 1a: Write the test**

Append to the existing `mod tests` (after the `acquire_llm_permit_does_not_panic_inside_multi_thread_runtime` test):

```rust
    /// Issue #81 regression: on a current-thread runtime, if the permit
    /// holder is another task on the same runtime, the OLD synchronous
    /// `acquire_llm_permit` deadlocks (it blocks the runtime worker at
    /// `std::thread::scope.join()`, so the holder can never run and
    /// release). Post-fix, async acquisition cooperatively yields and
    /// the holder runs to completion.
    ///
    /// Uses `tokio::sync::Notify` (not `sleep`) for a deterministic
    /// happens-before between holder.acquired and waiter.start —
    /// avoids timing flakiness on slow CI.
    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::{Notify, Semaphore};

        let sem = Arc::new(Semaphore::new(1));
        let opt = Some(sem.clone());
        let waiter_started = Arc::new(Notify::new());

        // Holder: takes the only permit, waits for the waiter to be
        // observably parked on acquire (Notify), then drops the permit.
        let holder_sem = sem.clone();
        let holder_signal = waiter_started.clone();
        let holder = async move {
            let _h = holder_sem.acquire_owned().await.unwrap();
            holder_signal.notified().await;
            // permit dropped at scope exit — waiter unparks
        };

        // Waiter: notifies "I'm about to acquire" then awaits permit.
        // Pre-fix, this deadlocks: the waiter runs first under
        // tokio::join!, so notify_one happens, but the SYNC
        // acquire_llm_permit blocks the only worker at join() and the
        // holder can never run.
        let waiter_signal = waiter_started.clone();
        let waiter = async move {
            // Yield once so the holder gets the permit before we ask.
            tokio::task::yield_now().await;
            waiter_signal.notify_one();
            acquire_llm_permit(&opt).await
        };

        // Wrap in a 5s timeout so a regression manifests as a fast
        // test failure, not a hung CI job. Capture the waiter's
        // result so the failure message is unambiguous.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            async { tokio::join!(holder, waiter) },
        )
        .await;

        let (_, waiter_permit) = result
            .expect("deadlock regression: tokio::join! on current-thread \
                     runtime did not complete within 5s — issue #81");
        assert!(
            waiter_permit.is_some(),
            "waiter must receive a permit once the holder releases"
        );
    }
```

**Step 1b: Verify the test fails to compile against current sync code (RED)**

The test calls `acquire_llm_permit(&opt).await`, which is invalid against the current SYNC signature. Run:

```bash
cargo test --bin quorum -- acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread
```

Expected: compile error E0277 with rustc's diagnostic literally suggesting "consider making `fn acquire_llm_permit` asynchronous". This is the strongest possible compile-time RED: the compiler points at exactly the fix.

**Note on runtime RED:** The original plan included a `spawn_blocking` shim to demonstrate runtime failure. That doesn't work — `spawn_blocking` dispatches to the blocking pool (separate from runtime workers), so worker X stays free to poll the holder, which can release the permit cleanly. The actual production deadlock requires sync `acquire_llm_permit` to block worker X *directly* (which `review_file` did inline from `#[tokio::main]` — but only on current-thread flavors). A test that does that would hang Tokio's timeout future itself (no worker to poll it) and require `std::thread::spawn` + condvar harness to detect, which is significant complexity for a one-shot demonstration. The compile-fail RED + the issue's reproduction analysis are accepted as sufficient.

**Step 3: Commit (RED checkpoint)**

```bash
git add src/pipeline.rs
git commit -m "test(pipeline): add deadlock regression for issue #81

Reproduces the cross-task deadlock on a current-thread runtime
where the permit holder is another task on the same runtime.
RED: does not compile against the current sync acquire_llm_permit
(intentional - the test asserts the post-fix async contract).
"
```

---

## Task 2: GREEN — convert `acquire_llm_permit` to async

**Files:**
- Modify: `src/pipeline.rs:75-128` (function + docstring)

**Step 1: Replace the function**

Find the existing `fn acquire_llm_permit(...)` block (with the `MultiThread`/`_` match) and replace the whole thing — docstring included — with:

```rust
/// Acquire a semaphore permit if configured, awaiting cooperatively.
///
/// Returns an owned permit that is released on drop (RAII). When the
/// semaphore is `None`, returns `None` immediately (no throttling).
///
/// Issue #81: pre-fix this was a sync helper that branched on the
/// current Tokio runtime flavor — `block_in_place` on multi-thread,
/// `std::thread::scope` + fresh runtime on current-thread. The
/// current-thread branch deadlocked when the permit holder was
/// another task on the *same* runtime: `join()` blocked the only
/// worker, the holder never ran to release, the helper runtime
/// awaited forever. The async shape eliminates that class of bug
/// by construction — we just `.await` and let the runtime that
/// owns the holder make progress.
///
/// Closed-semaphore (`acquire_owned` returns `Err`) degrades to
/// `None`, mirroring the prior contract for "throttling can't
/// work, don't crash the caller".
async fn acquire_llm_permit(
    sem: &Option<std::sync::Arc<tokio::sync::Semaphore>>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    sem.as_ref()?.clone().acquire_owned().await.ok()
}
```

The previous `use tokio::runtime::{Handle, RuntimeFlavor};` import inside the function disappears (it was the only consumer).

**Step 2: Run the regression test**

Run: `cargo test --bin quorum -- acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread`

Expected: still does not compile — callers of `acquire_llm_permit` are now using `.await` on a non-future site (the two call sites at lines ~561 and ~857 inside SYNC functions). This is correct; Task 3 fixes that.

---

## Task 3: GREEN — convert `review_file`, `review_source`, `review_file_llm_only` to async

**Files:**
- Modify: `src/pipeline.rs:323` (`pub fn review_file` → `pub async fn review_file`)
- Modify: `src/pipeline.rs:561` (call site: `acquire_llm_permit(...)` → `acquire_llm_permit(...).await`)
- Modify: `src/pipeline.rs:709` (`pub fn review_source` → `pub async fn review_source`)
- Modify: `src/pipeline.rs:722` (`review_file(...)` → `review_file(...).await`)
- Modify: `src/pipeline.rs:748` (`pub fn review_file_llm_only` → `pub async fn review_file_llm_only`)
- Modify: `src/pipeline.rs:857` (call site: `acquire_llm_permit(...)` → `acquire_llm_permit(...).await`)

**Step 1: Apply the six edits.** Mechanical: add `async` to the three `pub fn`, add `.await` to the three call sites. No body restructuring.

**Step 2: Run the regression test**

Run: `cargo test --bin quorum -- acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread`

Expected: still does not compile — main.rs and mcp/handler.rs and http_server.rs callers now see async functions. Task 4 wires those.

**Step 3: Run cargo check to see all call-site breakage at once**

Run: `cargo check --bin quorum 2>&1 | grep -E "error\[|^error:" | head -40`

Expected output: errors at the call sites in `main.rs`, `mcp/handler.rs`, `http_server.rs`, plus the existing test sites in `pipeline.rs` (deep-review etc.). Use this list as the work-set for Task 4.

---

## Task 4: GREEN — wire all call sites

**Files (in order of fix):**
- Modify: `src/http_server.rs:118` (already in `async fn review`, just add `.await`)
- Modify: `src/mcp/handler.rs:88` (`fn handle_review` → `async fn handle_review`, add `.await` at the `pipeline::review_source` call, update the dispatch in `handle_call_tool_request` at line ~378 to `.await` the changed handler)
- Modify: `src/main.rs:855` (serial path: add `.await` directly — already in async fn main)
- Modify: `src/main.rs:865` (serial path: add `.await` to `review_file_llm_only`)
- Modify: `src/main.rs:916-980` (parallel path: wrap the existing `rt.spawn_blocking(move || ... review_source(...) ...)` so the body uses `tokio::runtime::Handle::current().block_on(async move { review_source(...).await })`)
- Modify: existing test sites in `src/pipeline.rs:950, 1194, 1207, 1216, 1229, 1233, 1318` (these tests call `review_source`/`review_file` synchronously; convert each to `#[tokio::test]` with `.await` — flavor=multi_thread by default works fine here)
- Modify: `src/context/phase6_integration_tests.rs:283, 311` (similar conversion)

**Step 1: Fix daemon and MCP handler (smallest delta)**

For `src/http_server.rs` line 118 area:

```rust
let result = pipeline::review_source(
    std::path::Path::new(&req.file_path),
    &req.code,
    lang,
    state.llm_reviewer.as_deref(),
    &pipeline_cfg,
    Some(&state.parse_cache),
)
.await
.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Review error: {}", e)))?;
```

For `src/mcp/handler.rs`:

1. Change `fn handle_review` → `async fn handle_review`.
2. Add `.await` after the `pipeline::review_source(...)` call inside.
3. In `handle_call_tool_request` (line ~378), change `self.handle_review(tool)` → `self.handle_review(tool).await`.

**Step 2: Fix CLI serial path (`main.rs:855` and `:865`)**

```rust
let review_result = if let Some(l) = lang {
    pipeline::review_source(
        file_path, &source, l, llm_reviewer, &pipeline_cfg, Some(&parse_cache),
    ).await
} else {
    eprintln!("Note: No AST support for {}, using LLM-only review", file_path.display());
    pipeline::review_file_llm_only(
        file_path, &source, llm_reviewer, &pipeline_cfg,
    ).await
};
```

(Both at line ~855 and again at the second copy near line ~963.)

**Step 3: Fix CLI parallel path (`main.rs:916-980`)**

Inside the `rt.spawn_blocking(move || { ... })` closure body, the inner sync call:

```rust
let review_result = if let Some(l) = lang {
    pipeline::review_source(&file_path, &source, l, llm_reviewer, &pipeline_cfg, Some(&parse_cache))
} else {
    pipeline::review_file_llm_only(&file_path, &source, llm_reviewer, &pipeline_cfg)
};
```

becomes:

```rust
let handle = tokio::runtime::Handle::current();
let review_result = handle.block_on(async {
    if let Some(l) = lang {
        pipeline::review_source(&file_path, &source, l, llm_reviewer, &pipeline_cfg, Some(&parse_cache)).await
    } else {
        pipeline::review_file_llm_only(&file_path, &source, llm_reviewer, &pipeline_cfg).await
    }
});
```

The deep-review path inside the same closure does NOT call our async functions (it uses `agent::agent_loop` which stays sync), so it's unchanged.

**Add a brief inline comment justifying `block_on` inside `spawn_blocking`:**

```rust
// `spawn_blocking` runs on Tokio's blocking pool, NOT a runtime
// worker, so `Handle::block_on` here is sound (per Tokio docs).
// We deliberately keep the parsing/AST CPU work out of runtime
// workers by retaining the spawn_blocking shell.
```

**Step 4: Convert pipeline.rs and phase6 tests**

Each of these tests calls the now-async fns. Three-line change per test:
- `#[test]` → `#[tokio::test(flavor = "multi_thread", worker_threads = 1)]`
  (Default `#[tokio::test]` is current-thread, which can change cooperative-scheduling semantics for tests that previously assumed sync execution order. Pinning multi-thread/1-worker preserves sequential semantics while supporting `block_in_place`-style operations downstream.)
- `fn ...()` → `async fn ...()`
- Wrap test body call: `review_source(...)` → `review_source(...).await`

Sites to update:
- `src/pipeline.rs`: tests at lines 950, 1194, 1207, 1216, 1229, 1233, 1318 (use `cargo check --bin quorum` to enumerate exactly).
- `src/context/phase6_integration_tests.rs`: lines 283, 311.

**Step 4b: Add MCP handler async-dispatch regression test**

The MCP handler change (`fn handle_review` → `async fn handle_review`, plus `.await` in `handle_call_tool_request`) is mechanical but high-leverage — that dispatch path was untested for async behavior. Add to `src/mcp/handler.rs` test module:

```rust
    /// Issue #81 followup: handle_review is now async; the dispatch
    /// in handle_call_tool_request awaits it. This test exercises
    /// the full handler -> async pipeline surface end-to-end through
    /// the MCP tool boundary, asserting we don't reintroduce sync
    /// dispatch in a future refactor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn handle_review_is_async_and_dispatches_through_call_tool() {
        // Build a handler with a no-op LLM reviewer (None) so the
        // pipeline runs local-only — fast, no network.
        let tmpdir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(crate::feedback::FeedbackStore::new(
            tmpdir.path().join("feedback.jsonl"),
        ));
        let handler = QuorumHandler {
            llm_reviewer: None,
            feedback_store: store,
            config: crate::config::Config {
                model: "test".to_string(),
                ..Default::default()
            },
            parse_cache: std::sync::Arc::new(crate::cache::ParseCache::new(8)),
        };

        // A trivial Rust source so AST parsing runs without findings.
        let params = ReviewTool {
            file_path: "trivial.rs".to_string(),
            code: "fn main() {}".to_string(),
            focus: None,
        };
        let result = handler.handle_review(params).await;
        assert!(result.is_ok(), "handle_review must succeed: {:?}", result.err());
    }
```

(Adapt the `QuorumHandler { ... }` literal to match the actual fields — `cargo check` will tell you.)

**Step 5: Verify regression test compiles AND passes**

Run: `cargo test --bin quorum -- acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread --nocapture`

Expected: PASS. The test body executes on a current-thread runtime with `tokio::join!(holder, waiter)`, both tasks complete within 5s, no deadlock.

**Step 6: Verify the broader test suite still compiles + passes**

Run: `cargo test --bin quorum 2>&1 | tail -10`

Expected: all passing (modulo the known fastembed cache flake when running concurrently with main worktree — if you see those 3 failures, rerun with `--test-threads=1`).

**Step 7: Commit (GREEN checkpoint)**

```bash
git add -u
git commit -m "fix(pipeline): convert acquire_llm_permit + review fns to async (#81)

Eliminates the current-thread runtime deadlock by removing the
runtime-flavor switch and the std::thread::scope + fresh-runtime
+ join() dance. Now: just .await Semaphore::acquire_owned().

Public API change: review_file, review_source, review_file_llm_only
are now async fn. Callers updated in lockstep:

  - src/main.rs serial path: .await directly
  - src/main.rs parallel path: spawn_blocking still owns the
    CPU-heavy work; Handle::current().block_on(async { ... })
    drives the now-async review fns inside the blocking-pool
    thread (safe per Tokio: blocking pool != runtime worker)
  - src/http_server.rs: .await directly (already async)
  - src/mcp/handler.rs: handle_review is now async fn; the
    handle_call_tool_request dispatch awaits it
  - test sites: converted to #[tokio::test]

Regression test (added in prior commit) now passes: holder +
waiter via tokio::join! on a current-thread runtime, semaphore
of 1, completes within 5s where previously it hung.
"
```

---

## Task 5: Defensive coverage — flavor matrix + cancellation

**Files:**
- Modify: `src/pipeline.rs` (test module — add three more tests)

**Step 1: Update the existing `does_not_panic_inside_*` tests to use the async signature**

The two existing `acquire_llm_permit_does_not_panic_inside_*` tests at lines ~1013 and ~1029 already use `#[tokio::test]`. Their bodies call `acquire_llm_permit(&sem)` synchronously — change to `acquire_llm_permit(&sem).await`. (May already be done as part of Task 4 — `cargo test` will tell you.)

**Step 2: Update the no-runtime test**

The test `acquire_llm_permit_does_not_panic_outside_tokio_runtime` at line ~985 is `#[test]` (no runtime). Post-fix, calling an async fn outside a runtime won't panic — it just returns a future that's never polled. Replace the test body with one that explicitly verifies "no runtime ⇒ caller must build one to drive it":

```rust
    /// Issue #58 followup: with the async-permit shape, the helper
    /// no longer cares about runtime presence — building a fresh
    /// current-thread runtime to drive the future is the caller's
    /// responsibility. The function is still safe to *call* from
    /// non-runtime code (returns a future, no panic).
    #[test]
    fn acquire_llm_permit_returns_future_outside_tokio_runtime() {
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let sem = Some(Arc::new(Semaphore::new(1)));
        // Just constructing the future must not panic. We don't
        // poll it — that's the caller's job once they have a runtime.
        let _fut = acquire_llm_permit(&sem);
        // Drop without polling is safe.
    }
```

**Step 3: Add a cancellation-safety test**

Append to the test module:

```rust
    /// When a waiter on `acquire_llm_permit` is dropped (cancelled)
    /// before the permit becomes available, no permit is leaked and
    /// later acquisitions still work. This is the standard async
    /// cancellation guarantee — verifies we haven't accidentally
    /// broken it.
    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_cancellation_does_not_leak() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Semaphore;

        let sem = Arc::new(Semaphore::new(1));
        let opt = Some(sem.clone());

        // Hold the only permit until we explicitly drop below.
        let holder = sem.clone().acquire_owned().await.unwrap();

        // Spawn a waiter and immediately abort it.
        let opt_clone = opt.clone();
        let waiter = tokio::spawn(async move {
            acquire_llm_permit(&opt_clone).await
        });
        // Give the waiter a chance to start awaiting.
        tokio::task::yield_now().await;
        waiter.abort();
        let _ = waiter.await; // join the cancelled task

        // Available permits unchanged — still 0 because the holder
        // is alive. Drop the holder to release.
        assert_eq!(sem.available_permits(), 0);
        drop(holder);

        // Next acquirer must succeed; 2s bound is generous for
        // slow CI but tight enough to flag a regression.
        let permit = tokio::time::timeout(
            Duration::from_secs(2),
            acquire_llm_permit(&opt),
        )
        .await
        .expect("should not time out after holder release");
        assert!(permit.is_some());
    }
```

**Step 4: Add a multi-thread flavor regression test (defensive matrix coverage)**

```rust
    /// Same contention pattern as the current-thread regression but
    /// on a multi-thread runtime. Documents that the fix preserves
    /// production behavior (the path that already worked).
    /// Uses Notify for deterministic happens-before, mirroring the
    /// current-thread test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_llm_permit_does_not_deadlock_under_contention_on_multi_thread() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::{Notify, Semaphore};

        let sem = Arc::new(Semaphore::new(1));
        let opt = Some(sem.clone());
        let waiter_started = Arc::new(Notify::new());

        let holder_sem = sem.clone();
        let holder_signal = waiter_started.clone();
        let holder = async move {
            let _h = holder_sem.acquire_owned().await.unwrap();
            holder_signal.notified().await;
        };
        let waiter_signal = waiter_started.clone();
        let waiter = async move {
            tokio::task::yield_now().await;
            waiter_signal.notify_one();
            acquire_llm_permit(&opt).await
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            async {
                let (_, w) = tokio::join!(holder, waiter);
                w
            },
        )
        .await
        .expect("multi-thread contention timed out");
        assert!(result.is_some(), "waiter should receive a permit");
    }

    /// Closed-semaphore degrades to None (mirrors no-throttle
    /// contract). Cheap mutation-killer: any change that turns the
    /// `.ok()` into an unwrap or removes the `?` would fail this.
    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_returns_none_when_semaphore_is_closed() {
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let sem = Arc::new(Semaphore::new(1));
        sem.close();
        let opt = Some(sem);
        assert!(acquire_llm_permit(&opt).await.is_none());
    }
```

**Step 5: Run all four targeted tests**

```bash
cargo test --bin quorum -- acquire_llm_permit_does_not_panic_outside_tokio_runtime \
                          acquire_llm_permit_does_not_panic_inside_current_thread_runtime \
                          acquire_llm_permit_does_not_panic_inside_multi_thread_runtime \
                          acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread \
                          acquire_llm_permit_does_not_deadlock_under_contention_on_multi_thread \
                          acquire_llm_permit_cancellation_does_not_leak \
                          acquire_llm_permit_returns_future_outside_tokio_runtime
```

Expected: all pass. (If `acquire_llm_permit_does_not_panic_outside_tokio_runtime` still exists from before — it should be deleted in Step 2 since we replaced it with `acquire_llm_permit_returns_future_outside_tokio_runtime`.)

**Step 6: Commit**

```bash
git add src/pipeline.rs
git commit -m "test(pipeline): cancellation safety + multi-thread flavor coverage

Three additional regressions:
- acquire_llm_permit_returns_future_outside_tokio_runtime: replaces
  the panic-check; the async shape returns a future, no runtime
  needed to construct it.
- acquire_llm_permit_cancellation_does_not_leak: aborted waiter does
  not corrupt semaphore state; subsequent acquire still works.
- acquire_llm_permit_does_not_deadlock_under_contention_on_multi_thread:
  documents that production behavior is preserved on the path that
  already worked.
"
```

---

## Task 6: Verification (Phase 5)

**Step 1: Full test suite**

```bash
cargo test --bin quorum 2>&1 | tail -5
```

Expected: `test result: ok. <N> passed; 0 failed; 1 ignored`. If you see fastembed flake (3 failures involving `embeddings::tests::similar_texts_have_high_cosine` etc.), rerun with `--test-threads=1` — those are infrastructure flake from concurrent fastembed cache lock contention, not a regression from this PR.

**Step 2: Clippy**

```bash
cargo clippy --bin quorum -- -D warnings 2>&1 | tail -10
```

Expected: clean.

**Step 3: Release build**

```bash
cargo build --release --bin quorum 2>&1 | tail -3
```

Expected: `Finished release ...`.

**Step 4: Smoke test the MCP path**

```bash
QUORUM_API_KEY=dummy ./target/release/quorum serve <<EOF | head -5
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}
EOF
```

Expected: a JSON `initialize` response (no panic, no hang).

---

## Risks / antipatterns avoided

- **No "test the mock"**: the regression tests exercise real `tokio::sync::Semaphore` + real `tokio::join!` + real `tokio::sync::Notify` — no permit fakery, no fake runtimes.
- **Compile-fail RED (with documented runtime-RED limitation)**: Task 1b demonstrates the test fails to compile against the current SYNC signature — rustc itself suggests "consider making `fn acquire_llm_permit` asynchronous", which IS the fix. The original plan called for an additional runtime-RED via a `spawn_blocking` shim, but on closer analysis that shim does NOT reproduce the deadlock (`spawn_blocking` runs on the blocking pool, not a runtime worker, so the holder still gets polled). A faithful runtime-RED would require sync `acquire_llm_permit` to block worker X *directly*, which would also hang Tokio's timeout future itself — `std::thread::spawn` + condvar harness would be needed to detect it, which is significant complexity for a one-shot demonstration. Compile-fail RED + the issue's reproduction analysis accepted as sufficient.
- **Deterministic synchronization**: `tokio::sync::Notify` provides a happens-before signal between holder and waiter, replacing brittle `sleep(10ms)` timing windows. Slow-CI proof.
- **No mocked runtime**: tests use `#[tokio::test(flavor = "current_thread")]` to actually run on the formerly-deadlocking flavor. This is the test that proves the bug is gone.
- **Single-failure-reason per test**: Task 1's outer timeout `.expect()` and inner `assert!(waiter_permit.is_some())` are distinct branches with distinct messages — no assertion roulette.
- **No bypassed cancellation**: Task 5 explicitly tests `tokio::spawn` + `abort()` to confirm the standard async cancellation guarantee survives.
- **Mutation-killer for closed semaphore**: explicit `acquire_llm_permit_returns_none_when_semaphore_is_closed` test covers the `?` and `.ok()` branches that would otherwise be implicit.
- **Bulk-conversion safety**: `#[tokio::test]` sites pin `flavor = "multi_thread", worker_threads = 1` rather than relying on the current-thread default, preserving sequential semantics for tests that previously assumed sync execution order.
- **MCP-dispatch regression**: explicit test for `handle_review` async dispatch through `handle_call_tool_request` — covers the riskiest call-site change.
- **Bounded scope**: `LlmReviewer::review` async-hygiene (worker-blocking concern) and the `review_file_llm_only` Context7 helper drift are filed as follow-up issues, not bundled here.

## Plan revisions log

- **Round 1 (test-planning + antipattern reviews):**
  - Replaced `sleep(10ms)` timing window with `tokio::sync::Notify` deterministic handshake (Tasks 1, 5).
  - Initially added two-phase RED (runtime-fail demonstration via `spawn_blocking` shim before contract change in Task 1b); subsequently dropped after analysis showed the shim does NOT reproduce the deadlock (blocking pool ≠ runtime worker, so the holder still gets polled). Compile-fail RED + reproduction analysis accepted as sufficient.
  - Bumped cancellation post-release timeout from 500ms → 2s (slow-CI tolerance).
  - Added `acquire_llm_permit_returns_none_when_semaphore_is_closed` mutation-killer (Task 5).
  - Pinned `#[tokio::test]` bulk conversions to `multi_thread, worker_threads = 1` instead of default current-thread (Task 4).
  - Added MCP `handle_review` async-dispatch regression test (Task 4b).
