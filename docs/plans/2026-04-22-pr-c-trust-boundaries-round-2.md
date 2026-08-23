# PR C — Trust Boundaries, Round 2

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix three small pre-existing trust/failure-mode bugs (#71, #68, #69) under the same defense-in-depth theme as PRs #67/#70.

**Architecture:** Three independent fixes in three different files. No shared abstractions; each fix is local and follows established patterns (block_on_async runtime detection from PR #62, regex hardening from PR #67/#56, error propagation from PR #67's OpenAiClient::new). Worktree isolated, branch `fix/trust-boundaries-round-2`. RED→GREEN TDD per task.

**Tech Stack:** Rust 1.81+, tokio (runtime + Semaphore), regex 1.x, anyhow.

**Scope discipline:** Avoid `src/context_enrichment.rs`, `src/domain.rs`, and any context indexing files — issue #29 is in flight by another agent.

**Locked design choices** (from brainstorm):
- #68: option (A) — regex value class becomes escape-aware `(?:\\.|[^"\n])+`. Drop the `{6,}` floor (consistent with #61 — keyword anchor is enough).
- #71: option (B) — inline the RuntimeFlavor pattern in `acquire_llm_permit`; do NOT extract a shared helper. (Two call sites today, bound contortion not worth it.)
- #69: option (A) — hard-fail at file-level only via `?`-propagation→exit 3 + stderr; preserve `ReviewLog::load_all` semantics for malformed-line skipping.

---

## Task 1: #71 — `acquire_llm_permit` async-context panic

**Files:**
- Modify: `src/pipeline.rs` (function `acquire_llm_permit`, ~L80-95)
- Test: `src/pipeline.rs` (mod tests, near existing `acquire_llm_permit_does_not_panic_outside_tokio_runtime`)

**Step 1: Write the failing test**

Add a test that confirms calling `acquire_llm_permit` from inside an async context (current_thread runtime) does not panic:

```rust
#[tokio::test(flavor = "current_thread")]
async fn acquire_llm_permit_does_not_panic_inside_async_context() {
    // Issue #71: prior fix only handled the no-runtime case via
    // Handle::try_current().ok(). When called from INSIDE an async
    // context, handle.block_on(...) panics ("called within an
    // asynchronous execution context"). Mirror block_on_async's
    // RuntimeFlavor pattern.
    let sem = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        acquire_llm_permit(&sem)
    }));
    assert!(
        result.is_ok(),
        "acquire_llm_permit panicked inside async context: {:?}",
        result.err()
    );
    let permit = result.unwrap();
    // Inside async context we still want the permit when possible —
    // the goal is "don't panic", not "always degrade to None".
    assert!(permit.is_some(), "expected permit, got None");
}
```

**Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum pipeline::tests::acquire_llm_permit_does_not_panic_inside_async_context`
Expected: FAIL with "called within an asynchronous execution context"

**Step 3: Implement the RuntimeFlavor pattern**

Replace `acquire_llm_permit` body with the same pattern `block_on_async` uses (mirror, don't extract):

```rust
fn acquire_llm_permit(sem: &Option<std::sync::Arc<tokio::sync::Semaphore>>) -> Option<tokio::sync::OwnedSemaphorePermit> {
    use tokio::runtime::{Handle, RuntimeFlavor};
    let sem = sem.as_ref()?.clone();
    let handle = Handle::try_current().ok()?;
    match handle.runtime_flavor() {
        RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(sem.acquire_owned()).ok())
        }
        // CurrentThread or any future flavor where block_in_place is
        // disallowed: drive on a separate thread with its own runtime.
        // Throttling still works (the new runtime awaits the same
        // semaphore arc), and we don't re-enter the calling runtime.
        _ => std::thread::scope(|s| {
            s.spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()?;
                rt.block_on(sem.acquire_owned()).ok()
            })
            .join()
            .ok()
            .flatten()
        }),
    }
}
```

Update the doc-comment to reflect the new behavior.

**Step 4: Run test to verify it passes**

Run: `rtk cargo test --bin quorum pipeline::tests::acquire_llm_permit`
Expected: 3 tests pass (existing + new + no-semaphore).

**Step 5: Run full unit suite**

Run: `rtk cargo test --bin quorum`
Expected: 1321+ passed, 1 ignored, no failures.

**Step 6: Commit**

```bash
rtk git add src/pipeline.rs
rtk git commit -m "fix(pipeline): acquire_llm_permit handles async-context too (#71)"
```

---

## Task 2: #68 — redact regex doesn't handle escaped quotes

**Files:**
- Modify: `src/redact.rs` (the two quoted-secret regex patterns, ~L38-41)
- Test: `src/redact.rs` (mod tests)

**Step 1: Write the failing test**

```rust
#[test]
fn redact_quoted_secret_with_escaped_quote_in_value() {
    // Issue #68: PASSWORD="pa\"ssword" — the value class [^\n"]{6,}
    // stops at the first " and the {6,} floor fails on the 3-char
    // prefix `pa\`, so the secret leaks through.
    let cases = [
        r#"PASSWORD = "pa\"ssword""#,
        r#"API_KEY = "abc\"def""#,
    ];
    for input in cases {
        let output = redact_secrets(input);
        assert!(
            output.contains("[REDACTED]"),
            "expected redaction for {input:?}; got: {output}"
        );
        // Verify the secret content is NOT visible in the output.
        // We can't assert the literal escaped form is gone (the
        // surrounding quotes are kept) but the inner secret bytes
        // should be replaced.
        assert!(
            !output.contains("ssword") && !output.contains(r#"abc\"def"#),
            "secret bytes leaked through; got: {output}"
        );
    }
}

#[test]
fn redact_quoted_secret_with_escaped_single_quote_in_value() {
    // Mirror case for single-quoted form.
    let input = r#"TOKEN = 'it\'s-secret'"#;
    let output = redact_secrets(input);
    assert!(output.contains("[REDACTED]"));
    assert!(!output.contains("s-secret"));
}
```

**Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum redact::tests::redact_quoted_secret_with_escaped`
Expected: FAIL.

**Step 3: Update both quoted-secret regex patterns**

In `src/redact.rs`, the two patterns currently use `[^\n"]{6,}` and `[^\n']{6,}` for the value class. Replace with escape-aware classes:

- Double-quoted: `"((?:\\.|[^\n"])+)"`
- Single-quoted: `'((?:\\.|[^\n'])+)'`

Drop the `{6,}` floor — the keyword anchor is sufficient (consistent with #61 decision). The `(?:\\.|[^\n"])+` class matches escape sequences (`\"`, `\\`, `\n`, etc.) as one unit OR any non-quote/non-newline char.

**Step 4: Run test to verify it passes**

Run: `rtk cargo test --bin quorum redact::tests`
Expected: all redact tests pass (incl. the two new ones).

**Step 5: Verify no regressions across the broader suite** (regex change is broad)

Run: `rtk cargo test --bin quorum`
Expected: 1321+ passed.

**Step 6: Commit**

```bash
rtk git add src/redact.rs
rtk git commit -m "fix(redact): escape-aware value class for quoted-secret regex (#68)"
```

---

## Task 3: #69 — main.rs stats commands swallow load_all errors

**Files:**
- Modify: `src/main.rs` (stats dimensional handlers, ~L70-72 region — exact lines TBD via `grep load_all`)
- Test: `tests/cli.rs` or `tests/stats_dimensions.rs` (CLI integration test)

**Step 1: Locate the exact lines**

```bash
rtk grep -n "load_all" src/main.rs
```

Expected: two occurrences in the stats branches.

**Step 2: Write the failing test**

In `tests/stats_dimensions.rs` (or `tests/cli.rs`), add an integration test that:
- Sets `HOME` to an isolated tempdir
- Creates `~/.quorum/reviews.jsonl` as a directory (or sets perms to make it unreadable) so `load_all` fails on file open
- Runs `quorum stats --by-repo`
- Asserts: exit code != 0, stderr contains "reviews"

```rust
#[test]
fn stats_by_repo_fails_loudly_on_unreadable_log() {
    let tmp = tempfile::tempdir().unwrap();
    let quorum_dir = tmp.path().join(".quorum");
    std::fs::create_dir_all(&quorum_dir).unwrap();
    // Create reviews.jsonl as a DIRECTORY so File::open fails with
    // "Is a directory" — robust across platforms vs chmod tricks.
    std::fs::create_dir(quorum_dir.join("reviews.jsonl")).unwrap();

    let output = Command::cargo_bin("quorum")
        .unwrap()
        .arg("stats")
        .arg("--by-repo")
        .env("HOME", tmp.path())
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0), "expected nonzero exit on unreadable log");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reviews") || stderr.contains("error"),
        "expected error message in stderr; got: {stderr}"
    );
}
```

**Step 3: Run test to verify it fails**

Run: `rtk cargo test --test stats_dimensions stats_by_repo_fails_loudly_on_unreadable_log`
Expected: FAIL with exit code 0 (silent empty-stats).

**Step 4: Replace unwrap_or_default with explicit error**

In `src/main.rs`, find the two `ReviewLog::load_all(...).unwrap_or_default()` sites and replace each with:

```rust
let records = match crate::review_log::ReviewLog::load_all(&path) {
    Ok(r) => r,
    Err(e) => {
        eprintln!("error: cannot read reviews log at {}: {e}", path.display());
        return 3;
    }
};
```

(Adjust binding to whatever the surrounding code expects.)

**Step 5: Run test to verify it passes**

Run: `rtk cargo test --test stats_dimensions stats_by_repo_fails_loudly_on_unreadable_log`
Expected: PASS.

**Step 6: Run full test suite**

Run: `rtk cargo test`
Expected: All pass, 1 ignored.

**Step 7: Commit**

```bash
rtk git add src/main.rs tests/stats_dimensions.rs
rtk git commit -m "fix(main): stats commands fail loudly on unreadable reviews log (#69)"
```

---

## Phase 5 verification gates (full PR before review)

Run these before declaring complete:

```bash
rtk cargo test --bin quorum         # unit suite
rtk cargo test                       # full incl CLI integration
rtk cargo build --release            # release build smoke
rtk cargo clippy --bin quorum --tests 2>&1 | tail -30   # no NEW warnings vs main
ast-grep scan --rule rules/rust/builder-unwrap-or-default.yml src/   # no new offenders
ast-grep scan --rule rules/rust/handle-block-on-no-flavor-check.yml src/
```

## Phase 6 quorum self-review

```bash
rtk git diff main -- src/pipeline.rs src/redact.rs src/main.rs > /tmp/prc.patch
rtk cargo run --release -- review src/pipeline.rs src/redact.rs src/main.rs --diff-file /tmp/prc.patch --json > /tmp/prc-review.json
```

Triage findings:
- Any HIGH on `src/pipeline.rs:80-95`, `src/redact.rs:38-49`, or the modified `main.rs` lines → in-branch, TDD micro-cycle.
- Any HIGH elsewhere → pre-existing, file as new issue with `gh issue create`.

## Phase 7 calibrator feedback

For each finding triaged in Phase 6, record via `mcp__quorum__feedback`. Include `--reason` citing TDD test name or issue number where filed.

## Phase 8 PR

```bash
gh pr create --title "fix: trust boundaries round 2 (#68, #69, #71)" --body "..."
```

PR body references this plan + lists the verdicts recorded.
