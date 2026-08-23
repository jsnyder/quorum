# Self-Review Bugfix Batch #2 (#144–#147 + #155–#157) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Resolve 7 self-review findings discovered during the 3-way (quorum + PAL + CodeRabbit) comparison on 2026-04-30 — five in `src/llm_client.rs` and two in `src/ast_grep.rs`. Sibling to `2026-04-30-self-review-bugfix-batch.md` (#133–#139); both batches target the same v0.18.0 release window.

**Issues covered:**
- **#144** (PAL HIGH) — `sanitize_error_body` misses Authorization header / x-api-key / generic JSON token+secret fields
- **#146** (PAL MEDIUM) — retry classification too broad (drops `is_request()` catch-all)
- **#147** (Quorum HIGH) — `validate_base_url` accepts http scheme for public providers
- **#156** (CodeRabbit HIGH) — `validate_base_url` parse-error path leaks embedded credentials in error message
- **#157** (CodeRabbit MAJOR) — `send_with_retry` doesn't check `overall_deadline` before starting a new request
- **#145** (PAL HIGH) — duplicate rule IDs not detected; user rules silently shadow bundled rules
- **#155** (PAL+CodeRabbit corroborated MEDIUM/MAJOR) — `read_rule_file` missing post-read length check after `take(MAX+1)`

**Architecture:** 2 atomic PRs across 2 parallel git worktrees. PRs are independent of each other and of batch-1 (different files). Each PR follows strict RED→GREEN→REFACTOR TDD with a quorum self-review pass before merge.

**Tech Stack:** Rust 2021, regex 1, reqwest (existing dep), tokio (no new async work), tempfile + assert_matches.

**Rollout strategy:** 2 worktrees branched off the post-batch-1 main (or current main if dispatched in parallel). Implementation phase dispatches one subagent per worktree.

**Design decisions baked in:**
- **#144** — Extend the existing `LazyLock<Regex>` rather than introducing a second regex. Add fixture tests per shape so regex tweaks can't regress coverage.
- **#146** — Drop `is_request()` from the transient classification. Keep `is_timeout() || is_connect()`. Don't try to enumerate inner causes — too brittle.
- **#147** — Reject `http://` scheme outright unless `QUORUM_ALLOW_PRIVATE_BASE_URL=1` (which already implies dev/private context) OR `QUORUM_UNSAFE_BASE_URL=1` (full bypass). The bypass path keeps the existing total-bypass escape hatch consistent.
- **#156** — Use the existing `redact_userinfo_for_error` helper in the parse-error path (currently only used downstream after parse succeeds). Add a regression test that round-trips a malformed-URL-with-creds and asserts the credentials are NOT in the error string.
- **#157** — Add a deadline check at the top of the retry loop body (BEFORE `cloned.send()`), not just before the sleep. Bound: `if attempt > 0 && Instant::now() >= overall_deadline { return last_err; }`.
- **#145** — Track seen IDs with `HashSet`, skip duplicates with structured `tracing::warn!`. Don't implement "user overrides bundled" semantics — silent shadowing is the bug; explicit-override is a separate UX decision.
- **#155** — Add post-read length check after `take(MAX+1).read_to_string()`. Mirror PAL+CodeRabbit's exact suggestion (cross-tool corroborated).

---

## Worktree Map

| PR | Branch | Worktree dir | Issues | Files touched |
|----|--------|--------------|--------|---------------|
| 6 | `fix/llm-client-hardening` | `../quorum-llm-client-hardening` | #144, #146, #147, #156, #157 | `src/llm_client.rs` |
| 7 | `fix/ast-grep-rule-collision` | `../quorum-ast-grep-collision` | #145, #155 | `src/ast_grep.rs` |

---

## PR 6 — llm_client hardening (#144 + #146 + #147)

**Files:**
- Modify: `src/llm_client.rs` — `sanitize_error_body` regex (#144), `send_with_retry` transient classification (#146), `validate_base_url` scheme check (#147)
- Tests: existing `#[cfg(test)] mod tests` in `src/llm_client.rs`

### Task 6.1: RED — `sanitize_error_body` redacts header + JSON-field shapes (#144)

**Step 1: Write failing tests**

Add to existing tests module:

```rust
#[test]
fn sanitize_error_body_redacts_authorization_header_full_value() {
    let raw = r#"{"error":"unauthorized","headers":{"Authorization":"Bearer sk-foo123abc"}}"#;
    let out = sanitize_error_body(raw);
    assert!(!out.contains("sk-foo123abc"), "bearer token in Authorization header must be redacted: {out}");
    assert!(out.contains("[REDACTED]") || !out.contains("Authorization"), "header value must be scrubbed");
}

#[test]
fn sanitize_error_body_redacts_x_api_key_header() {
    let raw = r#"{"echoed":"x-api-key: 1234567890abcdef"}"#;
    let out = sanitize_error_body(raw);
    assert!(!out.contains("1234567890abcdef"), "x-api-key value must be redacted: {out}");
}

#[test]
fn sanitize_error_body_redacts_generic_token_field() {
    let raw = r#"{"error":"bad","token":"opaque-token-value-12345","other":"keep"}"#;
    let out = sanitize_error_body(raw);
    assert!(!out.contains("opaque-token-value-12345"), "generic token field must be redacted: {out}");
    assert!(out.contains("keep"), "non-secret fields must survive");
}

#[test]
fn sanitize_error_body_redacts_access_token_and_secret_fields() {
    let raw = r#"{"access_token":"at-abc-123","secret":"s3cr3t","name":"keep"}"#;
    let out = sanitize_error_body(raw);
    assert!(!out.contains("at-abc-123"));
    assert!(!out.contains("s3cr3t"));
    assert!(out.contains("keep"));
}
```

**Step 2: Run — should FAIL** with current regex catching only `bearer` / `sk-...` / `api[_-]?key`.

**Step 3: GREEN — extend regex**

Update the `LazyLock<Regex>` in `sanitize_error_body` (search for current regex pattern with `bearer\s+` literal). New pattern (verbose mode for readability):

```rust
static SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        (
            bearer\s+[A-Za-z0-9_\-\.=]+
          | authorization["']?\s*:\s*["']?[A-Za-z0-9_\-\.= ]+
          | x[-_]?api[-_]?key["']?\s*[:=]\s*["']?[A-Za-z0-9_\-\.=]+
          | (api[\s_-]?key|access[\s_-]?token|token|secret|auth)["']?\s*[:=]\s*["']?[A-Za-z0-9_\-\.=]+
          | sk-[A-Za-z0-9_\-]+
        )
        "#
    ).expect("sanitize_error_body regex")
});
```

Replace each match with `[REDACTED]`. Keep the existing 200-codepoint truncation downstream.

**Step 4: Run — all tests pass + existing 60+ sanitize tests still green.**

**Step 5: Commit** — `fix(llm_client): expand sanitize_error_body to redact headers + generic JSON secret fields (#144)`

### Task 6.2: RED — retry rejects deterministic protocol errors (#146)

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn send_with_retry_does_not_retry_request_construction_errors() {
    // A request error from URL parsing (deterministic) must NOT trigger
    // retry. Old behavior: e.is_request() returned true → 3 retries.
    // New: only timeout + connect classify as transient.
    let mut attempts = 0;
    let result = send_with_retry(|| {
        attempts += 1;
        async {
            // Force a builder/URL error — reqwest::Error::is_request() = true
            reqwest::Client::new()
                .get("not-a-url")
                .build()
                .map_err(|e| e.into())
        }
    }, /* deadline */).await;

    assert!(result.is_err());
    assert_eq!(attempts, 1, "deterministic request errors must not be retried");
}
```

(If `send_with_retry`'s signature is `Fn() -> Future<Result<Resp, anyhow::Error>>` — check the actual signature; adjust the test to inject a deterministic error via the closure.)

**Step 2: Run — FAIL** (current code retries 3x).

**Step 3: GREEN — narrow classification**

In `send_with_retry` (~line 683-689):

```rust
// Before:
let transient = e.is_timeout() || e.is_connect() || e.is_request();
// After:
let transient = e.is_timeout() || e.is_connect();
```

**Step 4: Run — pass + existing retry tests still green.**

**Step 5: Commit** — `fix(llm_client): narrow retry classification — drop is_request() catch-all (#146)`

### Task 6.3: RED — http scheme rejected for non-private bases (#147)

**Step 1: Write failing tests**

```rust
#[test]
fn validate_base_url_rejects_http_for_public_provider() {
    let policy = BaseUrlPolicy::default();
    let err = validate_base_url("http://api.openai.com/v1", &policy)
        .expect_err("http scheme to public provider must reject");
    assert!(err.contains("https"), "error must mention https requirement: {err}");
}

#[test]
fn validate_base_url_allows_http_with_allow_private_flag() {
    let policy = BaseUrlPolicy { allow_private_ips: true, ..Default::default() };
    // Local Ollama-style endpoint with http: should be permitted under the dev escape hatch
    let _ = validate_base_url("http://localhost:11434/v1", &policy)
        .expect("http+private IP must be allowed under QUORUM_ALLOW_PRIVATE_BASE_URL=1");
}

#[test]
fn validate_base_url_allows_http_with_unsafe_bypass() {
    let policy = BaseUrlPolicy { unsafe_bypass: true, ..Default::default() };
    let _ = validate_base_url("http://anything.example/v1", &policy)
        .expect("unsafe_bypass must allow http everywhere");
}
```

**Step 2: Run — FAIL** (current code accepts http:// for public providers).

**Step 3: GREEN — add scheme guard upstream of host allowlist**

In `validate_base_url` (~line 313-320), after parsing url and before host allowlist check:

```rust
if url.scheme() == "http" {
    if !policy.unsafe_bypass && !policy.allow_private_ips {
        return Err(format!(
            "base URL scheme must be https; got http://{}. \
             Set QUORUM_ALLOW_PRIVATE_BASE_URL=1 for local development \
             (Ollama, on-prem) or QUORUM_UNSAFE_BASE_URL=1 to disable all checks.",
            url.host_str().unwrap_or("?")
        ));
    }
    // http allowed only when private-IP path is intentional or full bypass
}
```

**Step 4: Run — all 3 new tests pass + ~30 existing validate_base_url tests still green.**

**Step 5: Commit** — `fix(llm_client): reject http scheme except under explicit private-IP/bypass flags (#147)`

### Task 6.4: RED — `validate_base_url` parse-error path uses redacted URL (#156)

**Step 1: Write failing test**

```rust
#[test]
fn validate_base_url_parse_error_does_not_leak_embedded_credentials() {
    let policy = BaseUrlPolicy::default();
    // Malformed URL (invalid port) WITH embedded credentials.
    // Parse fails BEFORE the embedded-credential rejection runs.
    let err = validate_base_url("https://user:secret-key@host:abc/v1", &policy)
        .expect_err("malformed URL must reject");
    let msg = format!("{err}");
    assert!(!msg.contains("user:secret-key"),
        "embedded credentials must NOT appear in parse-error message: {msg}");
    assert!(!msg.contains("secret-key"),
        "credential value must NOT appear in any form: {msg}");
}
```

**Step 2: Run — FAIL** (current code passes raw `base_url` to the parse-error message).

**Step 3: GREEN — use existing helper in parse-error path**

```rust
let display_url = redact_userinfo_for_error(base_url);
let parsed = url::Url::parse(base_url)
    .map_err(|e| anyhow::anyhow!("base_url {display_url:?} is not a valid URL: {e}"))?;
```

**Step 4: Run — pass + existing validate_base_url tests still green.**

**Step 5: Commit** — `fix(llm_client): redact embedded credentials in URL parse error path (#156)`

### Task 6.5: RED — retry loop checks deadline before launching new request (#157)

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn send_with_retry_does_not_launch_request_after_deadline() {
    // After deadline passes, no new send() call should fire.
    // Use mock that records call count and returns transient errors.
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempts_clone = attempts.clone();

    // Set a tight deadline (e.g. 50ms) and a slow first attempt that
    // exhausts it. The second attempt MUST NOT fire.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    let result = send_with_retry_with_deadline(|| {
        let n = attempts_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            if n == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err::<(), _>(/* transient */)
        }
    }, deadline).await;

    assert!(result.is_err());
    let n = attempts.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(n, 1, "no retry should have fired after deadline; got {n} attempts");
}
```

(May need to refactor `send_with_retry` to expose deadline as a parameter or use a test seam; design choice.)

**Step 2: Run — FAIL** (current code fires retry without deadline check).

**Step 3: GREEN — add deadline guard at loop top**

```rust
for attempt in 0..=MAX_RETRIES {
    if attempt > 0 && std::time::Instant::now() >= overall_deadline {
        return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("retry deadline exceeded")));
    }
    let cloned = req.try_clone().expect("...");
    let result = cloned.send().await;
    // ... existing logic
}
```

**Step 4: Run — pass + existing retry/timeout tests still green.**

**Step 5: Commit** — `fix(llm_client): check overall_deadline before launching retry attempt (#157)`

### Task 6.6: Verification + quorum review

```bash
rtk cargo test --bin quorum
rtk cargo clippy --all-targets -- -D warnings  # may need -D-skip-tests if pre-existing warnings
rtk cargo build --release
QUORUM_API_KEY=$THIRD_OPINION_API_KEY QUORUM_BASE_URL=$THIRD_OPINION_BASE_URL \
  QUORUM_ALLOWED_BASE_URL_HOSTS=litellm.5745.house \
  ./target/release/quorum review src/llm_client.rs --json > /tmp/post-pr6-review.json
```

Confirm net-zero new defects on the diff. Triage any new findings as TP/FP and record verdicts.

---

## PR 7 — ast_grep duplicate rule ID detection (#145)

**Files:**
- Modify: `src/ast_grep.rs` — `load_rules` (~lines 107-188)
- Tests: existing `#[cfg(test)] mod tests` in `src/ast_grep.rs`

### Task 7.0: RED — `read_rule_file` enforces post-read length cap (#155)

**Step 1: Write failing test**

```rust
#[test]
fn read_rule_file_rejects_file_one_byte_over_documented_cap() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oversized.yml");
    let mut f = std::fs::File::create(&path).unwrap();
    // Write exactly MAX+1 bytes
    let payload = vec![b'a'; (MAX_RULE_FILE_BYTES as usize) + 1];
    f.write_all(&payload).unwrap();

    let result = read_rule_file(&path);
    assert!(result.is_err(),
        "file MAX+1 bytes must be rejected (post-read length check)");
}
```

**Step 2: Run — FAIL** (current code returns the MAX+1 byte string silently).

**Step 3: GREEN — add post-read length check**

```rust
let mut yaml = String::new();
file.take(MAX_RULE_FILE_BYTES + 1).read_to_string(&mut yaml)?;
if yaml.len() as u64 > MAX_RULE_FILE_BYTES {
    return Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("rule file exceeds {} byte cap", MAX_RULE_FILE_BYTES),
    ));
}
Ok(yaml)
```

**Step 4: Run — pass + existing read_rule_file tests still green.**

**Step 5: Commit** — `fix(ast_grep): enforce documented post-read size cap on rule files (#155)`

### Task 7.1: RED — duplicate rule IDs are skipped with a warning

**Step 1: Write failing test**

```rust
#[test]
fn load_rules_skips_duplicate_rule_id_with_warning() {
    use tempfile::TempDir;
    use std::fs;

    let bundled = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    let lang_dir_b = bundled.path().join("python");
    let lang_dir_u = user.path().join("python");
    fs::create_dir_all(&lang_dir_b).unwrap();
    fs::create_dir_all(&lang_dir_u).unwrap();

    let rule_yaml = r#"
id: shared-rule-id
language: Python
rule:
  pattern: print($X)
"#;
    fs::write(lang_dir_b.join("bundled.yml"), rule_yaml).unwrap();
    fs::write(lang_dir_u.join("user.yml"), rule_yaml).unwrap();

    let rules = load_rules_with_dirs(&bundled.path(), &user.path()).unwrap();
    let matching: Vec<_> = rules.iter().filter(|r| r.id == "shared-rule-id").collect();
    assert_eq!(matching.len(), 1, "duplicate rule id must be deduplicated");
    // Bundled wins (loaded first) — locks the precedence rule
    // (alternatively: assert the test fails if precedence ever changes)
}
```

(If `load_rules_with_dirs` doesn't exist as a test seam, refactor `load_rules` to extract one — e.g. `load_rules_with_dirs(bundled: &Path, user: &Path) -> Vec<Rule>`. Production `load_rules` becomes a thin wrapper that resolves the canonical paths.)

**Step 2: Run — FAIL** (current code returns 2 rules).

**Step 3: GREEN — track seen IDs**

In the rule-loading loop (~line 165-185, find `rules.extend(parsed)` or equivalent):

```rust
let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
let mut deduped = Vec::new();
for parsed_rule in parsed {
    if !seen_ids.insert(parsed_rule.id.clone()) {
        tracing::warn!(
            rule_id = %parsed_rule.id,
            path = %rule_path.display(),
            "ast-grep: skipping duplicate rule id"
        );
        continue;
    }
    deduped.push(parsed_rule);
}
rules.extend(deduped);
```

(Adapt to the actual loop shape — the goal is dedup-by-id with logging.)

**Step 4: Run — test passes + existing rule-loader tests still green.**

**Step 5: Commit** — `fix(ast_grep): detect duplicate rule IDs and skip with structured warning (#145)`

### Task 7.2: Verification + quorum review

```bash
rtk cargo test --bin quorum
rtk cargo clippy --all-targets
rtk cargo build --release
QUORUM_API_KEY=$THIRD_OPINION_API_KEY QUORUM_BASE_URL=$THIRD_OPINION_BASE_URL \
  QUORUM_ALLOWED_BASE_URL_HOSTS=litellm.5745.house \
  ./target/release/quorum review src/ast_grep.rs --json > /tmp/post-pr7-review.json
```

---

## Cross-Cutting Verification (per worktree, before merge)

```bash
rtk cargo test --bin quorum 2>&1 | tail -5  # confirm full-suite green
rtk cargo build --release 2>&1 | tail -3
```

## Quorum Self-Review (Phase 6, per worktree)

Same protocol as batch-1: review the diff, triage TP/FP, record feedback verdicts.

## Feedback Recording (Phase 7, per worktree)

For every confirmed TP, FP, or partial finding, record via `quorum feedback` with appropriate `--fp-kind` if FP. Use `--from-agent pal` if the finding came from a PAL self-review during Phase 6.

## Finishing (Phase 8, per worktree)

Either fast-forward merge to main locally OR push and open a PR. Ensure CHANGELOG `[Unreleased]` reflects the fix.

---

## Coordination with Batch-1 and v0.18.0 Release

- **Batch-1** (`docs/plans/2026-04-30-self-review-bugfix-batch.md`) is in flight — covers #133-#139.
- **Batch-2** (this plan) covers #144-#147.
- Both batches target v0.18.0. When all 7 PRs merge:
  1. Update CHANGELOG `[Unreleased]` to reflect the combined work
  2. Move `[Unreleased]` content under `## [0.18.0] - <date>`
  3. Bump `Cargo.toml` version to 0.18.0
  4. Tag + push + GitHub release
- Suggested release-notes structure:
  - **Feedback (new)** — #123 Layer 1
  - **Security** — #133, #134, #135, #144, #147, #155, #156
  - **Reliability** — #137, #138, #139, #146, #157
  - **Fixed** — #136, #145
