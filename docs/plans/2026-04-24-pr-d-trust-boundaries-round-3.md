# PR D — Trust Boundaries, Round 3

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix two pre-existing bugs (#72, #73) surfaced by PR C's quorum self-review, under the same trust-boundaries theme as PRs #67/#70/#74.

**Architecture:** Two independent local fixes in two different files. Worktree isolated, branch `fix/trust-boundaries-round-3`. RED→GREEN TDD per task.

**Tech Stack:** Rust 1.81+, regex 1.x, anyhow.

**Scope discipline:** Avoid `src/context_enrichment.rs`, `src/domain.rs`, and any context indexing files — issue #29 is in flight by another agent. Do NOT touch the JSON/table/compact rendering paths in `src/context/cli.rs::render_doctor_*` (those are presentation-layer; we change only the internal data flow that drives them).

**Locked design choices** (from brainstorm + cross-model consensus):
- **#72**: drop bare `auth` from the secret-keyword alternation. `auth_token`/`auth_secret`/`auth_key`/`auth_password` are still redacted via the existing `token`/`secret`/`api[_-]?key`/`password` keywords, so credential coverage is unchanged. Loss: bare `auth = "x"` (rare; would need to be its own assignment with no credential-shaped suffix). Net effect: eliminates the `auth_<noun>` false-positive class (`auth_log_path`, `auth_db_url`, `auth_endpoint`, `auth_provider`).
- **#73**: extend `CmdOutput` (in `src/context/cli.rs`) with `pub doctor_failed: Option<bool>`. `run_doctor` populates it from the existing `any_fail` computation (already at L1885-1887, currently discarded after rendering). `main.rs` reads the typed field instead of substring-parsing `out.stdout`. Remove `doctor_reports_fail` (dead code after the structured path is in place).

---

## Task 1: #72 — drop bare `auth` from redact keyword class

**Files:**
- Modify: `src/redact.rs:44,46` (the two quoted-secret regex patterns)
- Test: `src/redact.rs` (mod tests)

**Step 1: Write the failing tests**

Add to `mod tests` in `src/redact.rs`:

```rust
#[test]
fn redact_does_not_match_auth_log_path() {
    // Issue #72: bare `auth` keyword combined with `(?:[_-][A-Za-z0-9]+)*`
    // matches benign config keys like `auth_log_path = "/var/log/auth.log"`,
    // redacting a path that isn't a credential.
    let input = r#"auth_log_path = "/var/log/auth.log""#;
    let output = redact_secrets(input);
    assert_eq!(
        input, output,
        "auth_log_path is a path, not a credential; should not be redacted. got: {output}"
    );
}

#[test]
fn redact_does_not_match_auth_db_url() {
    // URL value, not a credential. The URL-password regex separately
    // redacts the password embedded in the URL (covered by another test).
    let input = r#"auth_db_url = "postgres://app@db.local:5432/auth""#;
    let output = redact_secrets(input);
    assert_eq!(
        input, output,
        "auth_db_url with no embedded password should not be redacted. got: {output}"
    );
}

#[test]
fn redact_does_not_match_auth_endpoint_or_provider() {
    // Two cases in one test — both should pass through untouched.
    for input in [
        r#"auth_endpoint = "https://login.example.com/oauth/token""#,
        r#"auth_provider = "okta""#,
    ] {
        let output = redact_secrets(input);
        assert_eq!(
            input, output,
            "auth_endpoint / auth_provider are not credentials; got: {output}"
        );
    }
}

#[test]
fn redact_still_matches_auth_token_via_token_keyword() {
    // Positive control: `auth_token = "..."` is a credential and MUST
    // still be redacted. After dropping bare `auth`, the match flows
    // through the `token` keyword (which does `(?:[_-][A-Za-z0-9]+)*`
    // backwards via the `auth_` prefix? — no: `token` only matches
    // forward. So actually the way `auth_token` matches under the new
    // regex is via... let me re-read.)
    //
    // ACTUALLY: dropping bare `auth` means `auth_token` ONLY matches if
    // we add `_token` as a suffix to one of the remaining keywords. The
    // existing alternation has no leading-context match — it anchors at
    // the boundary char `(^|[^A-Za-z0-9])` and then matches the keyword
    // forward. So `auth_token = "x"` would NOT match `token` because
    // the boundary char before `token` is `_` which is NOT in
    // `[^A-Za-z0-9]` — wait, `_` is not alphanumeric so it IS in the
    // class. Actually `[^A-Za-z0-9]` means "anything except A-Z, a-z,
    // 0-9", so underscore matches. So `auth_token` triggers: boundary
    // = `_`, keyword = `token`, suffix = none, `\s*[=:]\s*` matches
    // ` = `. ✓ Redacts.
    //
    // Same logic applies to `auth_secret`, `auth_key`, `auth_password`.
    let input = r#"auth_token = "abc123secret""#;
    let output = redact_secrets(input);
    assert!(
        output.contains("[REDACTED]"),
        "auth_token IS a credential and must still be redacted via the `token` keyword. got: {output}"
    );
    assert!(
        !output.contains("abc123secret"),
        "secret value leaked. got: {output}"
    );
}

#[test]
fn redact_still_matches_bare_token_password_secret_key() {
    // Positive control: the four other keywords must still match
    // bare assignments after the regex change.
    for (input, expected_keyword) in [
        (r#"PASSWORD = "hunter2""#, "password"),
        (r#"SECRET = "shh""#, "secret"),
        (r#"TOKEN = "tok123""#, "token"),
        (r#"API_KEY = "ak_456""#, "api_key"),
        (r#"PASSWD = "pw1""#, "passwd"),
    ] {
        let output = redact_secrets(input);
        assert!(
            output.contains("[REDACTED]"),
            "{expected_keyword} keyword must still redact: input={input:?}, got={output}"
        );
    }
}
```

**Step 2: Run tests to verify RED (and confirm positive controls already pass)**

Run: `rtk cargo test --bin quorum redact::tests::redact_does_not_match_auth redact::tests::redact_still_matches -- --nocapture`

Expected:
- 3 RED tests FAIL (`auth_log_path`, `auth_db_url`, `auth_endpoint_or_provider`) — current regex redacts them.
- `redact_still_matches_auth_token_via_token_keyword` PASSES even before the fix (it currently matches via the `auth` keyword; after the fix it matches via `token`). This is the trickiest one — verify it stays GREEN through the change.
- `redact_still_matches_bare_token_password_secret_key` PASSES (regression guard for the four untouched keywords).

DO NOT proceed if RED-state isn't as expected.

**Step 3: Drop bare `auth` from both regex patterns**

In `src/redact.rs:44,46`, change the keyword alternation:

Before:
```rust
(?:api[_-]?key|password|secret|token|passwd|auth)
```

After:
```rust
(?:api[_-]?key|password|secret|token|passwd)
```

(Same change in both the double-quoted and single-quoted regex.)

Update the docstring comment block above (around L17-43) to note that bare `auth` was dropped per #72: `auth_token`/`auth_secret`/`auth_key`/`auth_password` are still covered via boundary-char + the credential-shaped keyword.

**Step 4: Run tests to verify GREEN**

Run: `rtk cargo test --bin quorum redact::tests`

Expected: all redact tests pass (the 4 new ones now GREEN, plus the existing ~22 unchanged).

**Step 5: Run full unit suite (regex change is broad — verify no regressions)**

Run: `rtk cargo test --bin quorum`

Expected: 1335+ passed (1331 from PR C baseline + 4 new). Embedding tests may flake on fastembed cache lock; re-run serially with `-- --test-threads=1` to confirm.

**Step 6: Commit**

```bash
rtk git add src/redact.rs
rtk git commit -m "fix(redact): drop bare 'auth' keyword to eliminate auth_<noun> over-redaction (#72)"
```

---

## Task 2: #73 — typed doctor exit signal via `CmdOutput.doctor_failed`

**Files:**
- Modify: `src/context/cli.rs` (struct `CmdOutput` ~L508-516; `run_doctor` ~L1894-1900)
- Modify: `src/main.rs` (run_context ~L351; remove `doctor_reports_fail` ~L364-374)
- Test: `tests/context_cli.rs` or `src/context/cli.rs` mod tests (unit-level test for typed field) + new `tests/doctor_exit_code.rs` (CLI integration test for end-to-end exit code)

**Step 1: Locate exact lines**

```bash
rtk grep -n "struct CmdOutput\|fn run_doctor\|let any_fail\|fn doctor_reports_fail\|is_doctor &&" src/context/cli.rs src/main.rs
```

Expected:
- `src/context/cli.rs:511` — `pub struct CmdOutput {`
- `src/context/cli.rs:1771` — `fn run_doctor<D: ContextDeps>(args: &DoctorArgs, deps: &D) -> Result<CmdOutput>`
- `src/context/cli.rs:1885` — `let any_fail = checks.iter().any(|c| matches!(c.status, CheckStatus::Fail { .. }));`
- `src/context/cli.rs:1894` — `Ok(CmdOutput { stdout, created_paths: created, removed_paths: Vec::new(), warnings, })`
- `src/main.rs:351` — `if is_doctor && doctor_reports_fail(&out.stdout) {`
- `src/main.rs:365-374` — `fn doctor_reports_fail(stdout: &str) -> bool { ... }`

**Step 2: Write the failing unit test (typed field at the cli.rs layer)**

In `src/context/cli.rs`, add to the existing test module (find via `rtk grep -n "^#\[cfg(test)\]\|^mod tests" src/context/cli.rs`):

```rust
#[test]
fn run_doctor_sets_doctor_failed_field_on_check_failure() {
    // Issue #73: doctor exit status was previously inferred by re-parsing
    // the rendered stdout (looking for `"ok": false`, `overall: fail`, or
    // `fail\t`). That coupled exit code to presentation strings. This
    // test pins the typed-signal contract: when any check fails, the
    // CmdOutput carries `Some(true)`. When no checks fail, `Some(false)`.
    // Other commands leave the field at `None` (Default).
    use crate::context::cli::test_support::DoctorTestDeps;  // see Step 3 if helper missing

    let deps = DoctorTestDeps::with_failing_check();
    let args = DoctorArgs { format: DoctorFormat::Json, repair: false };
    let out = run_doctor(&args, &deps).unwrap();
    assert_eq!(out.doctor_failed, Some(true));

    let deps = DoctorTestDeps::all_passing();
    let out = run_doctor(&args, &deps).unwrap();
    assert_eq!(out.doctor_failed, Some(false));
}

#[test]
fn cmd_output_default_doctor_failed_is_none() {
    // Other commands (init/add/list/index/refresh/query/prune) don't
    // populate doctor_failed — pin the Default behavior so we can rely
    // on `out.doctor_failed.unwrap_or(false)` at the call site.
    let out = CmdOutput::default();
    assert_eq!(out.doctor_failed, None);
}
```

**Note on test deps:** if `DoctorTestDeps` (or equivalent fixture) does not exist, look for the existing test fixture in `src/context/cli.rs` — there is likely already a `MockDeps` or `TestDeps` struct used by other doctor tests. Use `rtk grep -n "ContextDeps for\|impl.*ContextDeps" src/context/cli.rs src/context/` to find it. If no fixture exists that can produce a failing check, prefer adapting an existing one over writing a new mock from scratch — this keeps the test discipline tight and avoids over-mocking (testing-antipatterns).

**Step 3: Write the failing CLI integration test**

Create `tests/doctor_exit_code.rs`:

```rust
//! Issue #73: `quorum context doctor` exit code must be derived from a typed
//! signal (CmdOutput.doctor_failed), not by re-parsing rendered stdout.
//! These tests pin the contract that:
//!   (a) failing checks produce exit 1
//!   (b) all-passing checks produce exit 0
//!   (c) cosmetic copy edits to doctor output do not flip the exit code
//!       (verified indirectly: we no longer match `"ok": false` etc., so
//!       changing those strings can't affect the test outcome)

use assert_cmd::Command;

/// Build a HOME directory with NO `.quorum/sources.toml` — `check_sources_toml`
/// returns `CheckStatus::Fail` for "missing config", which is the simplest
/// reproducer for "doctor reports any failing check".
fn home_with_no_sources_toml() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".quorum")).unwrap();
    tmp
}

#[test]
fn doctor_exits_1_when_checks_fail() {
    let tmp = home_with_no_sources_toml();
    let output = Command::cargo_bin("quorum").unwrap()
        .arg("context").arg("doctor")
        .env("HOME", tmp.path())
        .output().unwrap();
    assert_eq!(output.status.code(), Some(1),
        "expected exit 1 when checks fail; got: {:?}, stderr: {}",
        output.status.code(), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn doctor_exits_0_when_all_checks_pass() {
    // The simplest "all passing" state is an empty-but-valid sources.toml
    // (no sources => no per-source checks => only the toml + orphan-dirs
    // checks run, both of which can pass).
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".quorum/sources")).unwrap();
    std::fs::write(
        tmp.path().join(".quorum/sources.toml"),
        "[context]\nauto_inject = false\n",
    ).unwrap();
    let output = Command::cargo_bin("quorum").unwrap()
        .arg("context").arg("doctor")
        .env("HOME", tmp.path())
        .output().unwrap();
    assert_eq!(output.status.code(), Some(0),
        "expected exit 0 when all checks pass; got: {:?}, stderr: {}",
        output.status.code(), String::from_utf8_lossy(&output.stderr));
}
```

**Verify the "all passing" assumption first:** before running RED, manually trace `run_doctor`'s checks for an empty-sources-toml HOME. The two always-run checks are `check_sources_toml` and `check_orphan_dirs`. If either could fail in this state (e.g., orphan-dirs detection finds the empty `.quorum/sources/` dir as orphan), the second test will fail GREEN and the test plan needs adjustment. Read `check_orphan_dirs` in `src/context/cli.rs` first to confirm. If the fixture isn't quite right, EITHER: (a) adjust the HOME setup until both checks pass, OR (b) drop test #2 and rely only on #1 + the unit-level `Some(false)` assertion in Step 2.

**Step 4: Run tests to verify RED**

Run: `rtk cargo test --test doctor_exit_code` and `rtk cargo test --bin quorum -- run_doctor_sets_doctor_failed cmd_output_default`.

Expected:
- `cmd_output_default_doctor_failed_is_none` FAILS to compile — the field doesn't exist yet.
- `run_doctor_sets_doctor_failed_field_on_check_failure` FAILS to compile — same reason.
- CLI integration tests: `doctor_exits_1_when_checks_fail` may currently PASS (the existing substring matcher catches it). `doctor_exits_0_when_all_checks_pass` should also currently PASS. Both are regression guards — they should stay GREEN through the refactor.

DO NOT proceed if compile errors aren't the predicted ones (missing field).

**Step 5: Add the typed field to `CmdOutput`**

In `src/context/cli.rs:511-516`, extend the struct:

```rust
#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    /// Human-readable summary the CLI layer prints on stdout.
    pub stdout: String,
    /// Paths the command created (for test assertions + `--dry-run` UX).
    pub created_paths: Vec<PathBuf>,
    /// Paths the command deleted (or would have, under `--dry-run`).
    /// Populated by `prune`; empty for all other commands.
    pub removed_paths: Vec<PathBuf>,
    /// Non-fatal warnings (e.g. "already initialized").
    pub warnings: Vec<String>,
    /// Doctor-only: `Some(true)` if any check failed, `Some(false)` if all
    /// passed, `None` for non-doctor commands. Drives the CLI exit code
    /// without re-parsing rendered stdout (issue #73).
    pub doctor_failed: Option<bool>,
}
```

The `Default` derive makes `doctor_failed` default to `None` automatically — no other call site needs to change.

**Step 6: Populate `doctor_failed` in `run_doctor`**

In `src/context/cli.rs`, find the `Ok(CmdOutput { ... })` at the END of `run_doctor` (around L1894 — there may be more than one; locate the one inside `run_doctor` specifically):

Before:
```rust
Ok(CmdOutput {
    stdout,
    created_paths: created,
    removed_paths: Vec::new(),
    warnings,
})
```

After:
```rust
Ok(CmdOutput {
    stdout,
    created_paths: created,
    removed_paths: Vec::new(),
    warnings,
    doctor_failed: Some(any_fail),
})
```

(`any_fail` is already computed at L1885-1887 — no new logic needed, just thread the value into the output.)

**Step 7: Switch `main.rs` to read the typed field**

In `src/main.rs:351`, replace:

```rust
if is_doctor && doctor_reports_fail(&out.stdout) {
    return 1;
}
```

with:

```rust
if out.doctor_failed.unwrap_or(false) {
    return 1;
}
```

Note: `is_doctor` is no longer needed at this site because `doctor_failed` is `None` for non-doctor commands and `unwrap_or(false)` handles that. But `is_doctor` may be used elsewhere — check via `rtk grep -n "is_doctor" src/main.rs` and only remove the `let is_doctor = ...;` binding if it has no other uses.

**Step 8: Remove `doctor_reports_fail` (dead code)**

Delete the function `fn doctor_reports_fail(stdout: &str) -> bool { ... }` at `src/main.rs:365-374`. Run `rtk cargo build --tests --bin quorum` to confirm no other site depends on it.

**Step 9: Run all the affected tests**

Run in order:
```bash
rtk cargo test --bin quorum -- run_doctor_sets_doctor_failed cmd_output_default
rtk cargo test --test doctor_exit_code
rtk cargo test --bin quorum    # full unit suite
rtk cargo test                  # full incl integration
```

Expected: all GREEN. Embedding tests may flake on fastembed cache lock; re-run serially.

**Step 10: Commit**

```bash
rtk git add src/context/cli.rs src/main.rs tests/doctor_exit_code.rs
rtk git commit -m "fix(main): doctor exit code from typed CmdOutput.doctor_failed (#73)"
```

---

## Phase 5 verification gates (full PR before review)

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
rtk git diff main -- src/redact.rs src/context/cli.rs src/main.rs tests/doctor_exit_code.rs > /tmp/prd.patch
rtk cargo run --release -- review src/redact.rs src/context/cli.rs src/main.rs tests/doctor_exit_code.rs --diff-file /tmp/prd.patch --json > /tmp/prd-review.json
```

Triage findings:
- Any HIGH on the modified surfaces (redact regex L44/L46, CmdOutput struct L511-518, run_doctor L1885-1900, main.rs L351 region) → in-branch, TDD micro-cycle.
- Any HIGH elsewhere → pre-existing, file as new issue with `gh issue create`.

## Phase 7 calibrator feedback

For each finding triaged in Phase 6, record via `mcp__quorum__feedback`. Include `--reason` citing TDD test name or issue number where filed. **Important for this PR specifically:** record `tp` + `--provenance post_fix` for #72 and #73 themselves (the original PR-C self-review findings) — the calibrator should learn that those flagged TPs led to fixes (1.5x weight).

## Phase 8 PR

```bash
gh pr create --title "fix: trust boundaries round 3 (#72, #73)" --body "..."
```

PR body references this plan + lists the verdicts recorded. Close #72 and #73 via PR description ("Closes #72, #73").
