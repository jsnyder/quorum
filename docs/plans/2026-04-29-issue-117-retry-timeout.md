# #117 Retry + Timeout Layering Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `OpenAiClient` resilient to transient 429/5xx upstream failures and bound worst-case wall-clock per LLM call so a single slow/flaky endpoint can't pin the parallel review pool for minutes.

**Architecture:** Add a `send_with_retry` helper that wraps the existing `RequestBuilder.send().await` with an overall-deadline retry loop honoring `Retry-After` headers; add a `read_timeout` (per-read) on top of the existing `total` timeout; expose both via `QUORUM_HTTP_TIMEOUT` / `QUORUM_HTTP_READ_TIMEOUT` env vars (max_retries hardcoded at 3 for now). Three POST sites (`chat_completion`, `responses_api`, `chat_with_tools`) refactor to call the helper.

**Critical: timeouts must NOT preempt valid working requests.** Reasoning models (o1, gpt-5-thinking, claude-opus-4.5-thinking) routinely take 4-8 minutes on hard prompts. The current `total=300s` is the production-proven baseline and we KEEP it unchanged. The new `read_timeout=120s` only protects against trickle-after-first-byte (which doesn't fire on healthy "think-then-burst" responses). The new overall retry budget (600s) only bounds the *retry tail*, not the happy path.

**Tech Stack:** Rust 2021, reqwest 0.12 (already on `stream` feature), tokio, anyhow, tracing. New dev-dep: `wiremock = "0.6"` for ~4 integration smoke tests; pure helpers (`is_retriable`, `parse_retry_after`, deadline math) tested without HTTP.

---

## Threat Model and Decisions

### Decisions locked in brainstorm

1. **Configurability (B):** `QUORUM_HTTP_TIMEOUT` (total, default **300s** — unchanged from production) + `QUORUM_HTTP_READ_TIMEOUT` (per-read, default **120s**). Hardcoded `MAX_RETRIES = 3` and `OVERALL_RETRY_DEADLINE_SECS = 600`. Rationale: reasoning models routinely take 4-8min on hard prompts — the existing 300s default is already proven and a regression there would break valid calls. `read_timeout=120s` only protects against trickle-after-first-byte (does NOT fire on think-then-burst patterns because once any byte arrives the rest follows in ms). The 600s overall retry budget bounds *only the retry tail*, not single happy-path calls — those remain bounded by the existing 300s total.
2. **Test strategy (C):** Pure unit tests for `is_retriable`, `parse_retry_after`, backoff math; ~4 wiremock integration tests for retry-loop wiring (one each: 429 retry-then-200, 503 retry-then-200, 4xx not retried, deadline-exceeded aborts). Adds wiremock as dev-dep only.
3. **Retry deadline (B):** Track `started_at = Instant::now()` outside the loop; abort retries when `elapsed >= OVERALL_RETRY_DEADLINE` (default 600s, NOT the 300s total per-call timeout). Honor `Retry-After` only if `now + retry_after_dur < started_at + overall_deadline`. Rationale: a single happy-path call is bounded by `total=300s` (per-call). The retry budget only kicks in when transient failures stack — capping wall-clock at ~10min beats `4 × 300s = 20min` worst case.

### What's explicitly out of scope (for this PR)

- **Per-call telemetry on retries** — counts per attempt would land in `TelemetryEntry`, but that schema bump can come with a real consumer (#21/#33 self-improvement metrics). For now log via `tracing::warn` only.
- **Circuit breaker** — if 5 consecutive calls fail across the parallel pool, we don't trip a breaker that pauses subsequent calls. Real usage doesn't justify this yet.
- **Custom backoff jitter** — using exponential `500ms → 1s → 2s` with no jitter; thundering-herd risk is low because we cap retries at 3 and `--parallel` is small (default 4).
- **Idempotency keys** — POST is normally non-idempotent, but our requests are read-only against the LLM; retry safety is by design, not header. Adding idempotency keys is a further hardening if a provider supports it.
- **Streaming responses (SSE) — file as follow-up issue.** Current POSTs are non-streaming (`.json(&body)` without `"stream": true`); the response arrives as one JSON blob after the model finishes thinking. Implication for THIS PR: `read_timeout=120s` is "max gap between body byte chunks" — uncommon in practice for healthy upstream. If we flip to streaming later, `read_timeout` would naturally become a "token-stall detector" (fires after N seconds of no new SSE delta), and we could likely tighten it to ~30s without preempting valid responses. Streaming also gets us visible progress + partial-output UX. The `send_with_retry` helper designed here works unchanged under streaming — it only handles the connection/initial-response phase; once headers arrive successfully, the retry loop returns `Response` and the caller can stream however it wants. Filing as separate issue for later.

### Acceptance criteria (issue #117)

- [ ] `send_with_retry` helper used at all three POST sites (`chat_completion`, `responses_api`, `chat_with_tools`).
- [ ] `is_retriable` honors 429/500/502/503/504; respects `Retry-After` (seconds + HTTP-date forms).
- [ ] `read_timeout` set in addition to total `timeout`.
- [ ] Tests:
  - Unit: `is_retriable_returns_true_for_429_500_502_503_504`, `is_retriable_returns_false_for_200_400_401_403_404`, `parse_retry_after_seconds`, `parse_retry_after_http_date`, `parse_retry_after_past_date_returns_zero`, `parse_retry_after_huge_seconds`, `parse_retry_after_handles_whitespace_and_unicode_garbage`, exponential-backoff math, jitter stays within ±20%, deadline-fits-budget predicate.
  - Wiremock integration: 429-then-200 succeeds; 503-then-200 succeeds; 400 aborts (no retry, request count == 1); deadline exceeded under repeated 429 returns the last 429 error (caller's path bails); Retry-After header overrides computed backoff when it fits budget.
- [ ] CHANGELOG entry under `Reliability` section.
- [ ] No regression in 1579 existing tests.

---

## Tasks

### Task 1: Worktree + dev-dep

**Files:**
- Modify: `Cargo.toml` (dev-dep)
- Verify: existing 1579 tests pass on the new branch as baseline

**Step 1: Create worktree**

```bash
cd /Users/jsnyder/Sources/github.com/jsnyder/quorum
git worktree add .worktrees/retry-timeout -b fix/llm-client-retry-timeout
cd .worktrees/retry-timeout
```

**Step 2: Add wiremock as dev-dep**

```toml
# In [dev-dependencies] section
wiremock = "0.6"
```

**Step 3: Verify baseline**

```bash
rtk cargo test --bin quorum 2>&1 | tail -3
```

Expected: `1579 passed, 1 ignored`.

**Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore(deps): add wiremock dev-dep for #117 retry tests"
```

---

### Task 2: Pure helpers (RED → GREEN)

**Files:**
- Modify: `src/llm_client.rs` — add `is_retriable`, `parse_retry_after`, `MAX_RETRIES`, `DEFAULT_HTTP_TIMEOUT`, `DEFAULT_HTTP_READ_TIMEOUT`, `compute_backoff`, `retry_after_fits_budget` constants/fns.

**Step 1: Write failing tests (RED)**

In the `tests` module of `src/llm_client.rs`:

```rust
#[test]
fn is_retriable_returns_true_for_transient_status() {
    use reqwest::StatusCode;
    for s in [429u16, 500, 502, 503, 504] {
        assert!(
            is_retriable(StatusCode::from_u16(s).unwrap()),
            "{s} should be retriable"
        );
    }
}

#[test]
fn is_retriable_returns_false_for_success_and_permanent_4xx() {
    use reqwest::StatusCode;
    for s in [200u16, 201, 204, 400, 401, 403, 404, 422] {
        assert!(
            !is_retriable(StatusCode::from_u16(s).unwrap()),
            "{s} should NOT be retriable"
        );
    }
}

#[test]
fn parse_retry_after_seconds_form() {
    assert_eq!(parse_retry_after_value("60"), Some(Duration::from_secs(60)));
    assert_eq!(parse_retry_after_value("0"), Some(Duration::ZERO));
    assert_eq!(parse_retry_after_value(" 30 "), Some(Duration::from_secs(30)));
}

#[test]
fn parse_retry_after_http_date_form() {
    // Wider envelope to absorb VM clock jitter on CI runners. The +120s
    // future gives a [60,120] valid window, with the 60s floor rejecting
    // the "past-date returns ZERO" silent-pass case (antipattern #16: Liar).
    let future = std::time::SystemTime::now() + Duration::from_secs(120);
    let httpdate = httpdate::fmt_http_date(future);
    let dur = parse_retry_after_value(&httpdate).expect("http-date must parse");
    assert!(
        dur.as_secs() >= 60 && dur.as_secs() <= 120,
        "expected 60-120s remaining, got {dur:?}"
    );
}

#[test]
fn parse_retry_after_returns_none_for_garbage() {
    assert_eq!(parse_retry_after_value(""), None);
    assert_eq!(parse_retry_after_value("not-a-number"), None);
    assert_eq!(parse_retry_after_value("-5"), None);
}

#[test]
fn parse_retry_after_past_http_date_returns_zero() {
    // Server says "retry after a date in the past" — interpret as
    // "retry now". (Common with proxy clock skew.)
    let past = std::time::SystemTime::now() - Duration::from_secs(60);
    let httpdate = httpdate::fmt_http_date(past);
    assert_eq!(parse_retry_after_value(&httpdate), Some(Duration::ZERO));
}

#[test]
fn parse_retry_after_huge_seconds_value() {
    // 1 day in seconds. Should parse as a Duration without overflow.
    // Whether it FITS the budget is a separate concern (retry_after_fits_budget).
    assert_eq!(
        parse_retry_after_value("86400"),
        Some(Duration::from_secs(86400))
    );
}

#[test]
fn parse_retry_after_handles_whitespace_and_unicode_garbage() {
    // Header values may contain odd bytes if the server is broken. We
    // expect Option::None rather than a panic.
    assert_eq!(parse_retry_after_value("\t\n"), None);
    assert_eq!(parse_retry_after_value("60s"), None); // unit suffix not part of spec
}

#[test]
fn compute_backoff_grows_exponentially_capped() {
    assert_eq!(compute_backoff(0), Duration::from_millis(500));
    assert_eq!(compute_backoff(1), Duration::from_secs(1));
    assert_eq!(compute_backoff(2), Duration::from_secs(2));
    // Cap at 30s — even attempt 10 should not exceed cap.
    assert!(compute_backoff(10) <= Duration::from_secs(30));
}

#[test]
fn apply_jitter_with_deterministic_seeds_is_within_bounds() {
    // Using fixed `rand_unit` values instead of stochastic sampling — no
    // flake risk and proves the math directly (antipattern #14: Shallow
    // validation; #16: Liar via random sampling that happens to pass).
    let base = Duration::from_millis(1000);
    // unit=0.0 → scale = 1 + (0 - 1) * 0.2 = 0.8 → 800ms
    assert_eq!(apply_jitter(base, 0.0), Duration::from_millis(800));
    // unit=0.5 → scale = 1 + 0 * 0.2 = 1.0 → 1000ms
    assert_eq!(apply_jitter(base, 0.5), Duration::from_millis(1000));
    // unit=0.999... → scale ≈ 1 + 0.999 * 0.2 ≈ 1.1998 → ~1199ms
    let max = apply_jitter(base, 0.999);
    assert!(max >= Duration::from_millis(1190) && max <= Duration::from_millis(1200), "got {max:?}");
}

#[test]
fn jitter_unit_returns_value_in_zero_one() {
    let u = jitter_unit();
    assert!(u >= 0.0 && u < 1.0, "got {u}");
}

#[test]
fn retry_after_fits_budget_respects_remaining_time() {
    let now = std::time::Instant::now();
    let started = now - Duration::from_secs(100);
    let total = Duration::from_secs(120);
    // 30s budget left. 10s retry-after fits; 60s does not.
    assert!(retry_after_fits_budget(started, total, Duration::from_secs(10), now));
    assert!(!retry_after_fits_budget(started, total, Duration::from_secs(60), now));
}
```

**Step 2: Run tests to verify they fail**

```bash
rtk cargo test --bin quorum is_retriable parse_retry_after compute_backoff retry_after_fits_budget 2>&1 | tail -10
```

Expected: compile errors / "function not defined".

**Step 3: Implement minimal helpers**

Add the `httpdate` crate as a regular dep (already transitive via reqwest's deps; add explicit Cargo.toml line):

```toml
httpdate = "1"
```

Implementation in `src/llm_client.rs` near top of file:

```rust
const MAX_RETRIES: u32 = 3;
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 300;          // unchanged from prod baseline
const DEFAULT_HTTP_READ_TIMEOUT_SECS: u64 = 120;     // new — trickle defense
const OVERALL_RETRY_DEADLINE_SECS: u64 = 600;        // bounds retry tail wall-clock
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const BACKOFF_JITTER_FRAC: f64 = 0.2;                // ±20% when Retry-After absent

fn is_retriable(s: reqwest::StatusCode) -> bool {
    matches!(s.as_u16(), 429 | 500 | 502 | 503 | 504)
}

fn parse_retry_after_value(raw: &str) -> Option<Duration> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Seconds form
    if let Ok(secs) = s.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date form
    if let Ok(when) = httpdate::parse_http_date(s) {
        if let Ok(remaining) = when.duration_since(std::time::SystemTime::now()) {
            return Some(remaining);
        }
        // Past dates return Some(ZERO) — server says "retry now".
        return Some(Duration::ZERO);
    }
    None
}

fn compute_backoff(attempt: u32) -> Duration {
    // 500ms, 1s, 2s, 4s, ..., capped at 30s. Used as the *base* — callers
    // applying jitter wrap this; honored Retry-After bypasses this entirely.
    let ms = BACKOFF_BASE_MS.saturating_mul(1u64.saturating_shl(attempt));
    std::cmp::min(Duration::from_millis(ms), BACKOFF_CAP)
}

/// Apply ±BACKOFF_JITTER_FRAC jitter around the base backoff. Used only
/// when Retry-After is absent (server-supplied wait wins for thundering-herd
/// avoidance — clients independently jittering their own retries is enough
/// at parallel=4). `rand_seed` is `Instant::now()`-based to avoid pulling
/// in a real RNG dep.
fn apply_jitter(base: Duration, rand_unit: f64) -> Duration {
    // rand_unit in [0.0, 1.0). Map to [-frac, +frac].
    let scale = 1.0 + (rand_unit * 2.0 - 1.0) * BACKOFF_JITTER_FRAC;
    let scaled_ms = (base.as_millis() as f64 * scale.max(0.0)) as u64;
    Duration::from_millis(scaled_ms)
}

fn retry_after_fits_budget(
    started_at: std::time::Instant,
    overall_deadline: Duration,
    retry_after: Duration,
    now: std::time::Instant,
) -> bool {
    let elapsed = now.saturating_duration_since(started_at);
    let remaining = overall_deadline.saturating_sub(elapsed);
    // `<=` not `<` — if waking exactly at deadline is acceptable, don't drop
    // a valid last retry on a knife-edge timing.
    retry_after <= remaining
}
```

**Step 4: Run tests to verify they pass**

```bash
rtk cargo test --bin quorum is_retriable parse_retry_after compute_backoff retry_after_fits_budget 2>&1 | tail -5
```

Expected: all 7 tests pass.

**Step 5: Commit**

```bash
git add src/llm_client.rs Cargo.toml Cargo.lock
git commit -m "feat(llm_client): pure retry helpers — is_retriable, parse_retry_after, compute_backoff (#117)"
```

---

### Task 3: Tighten timeouts + env-var configurability

**Files:**
- Modify: `src/llm_client.rs::OpenAiClient::new` (around lines 485-490)

**Step 1: Write failing tests (RED)**

```rust
// NOTE: existing helper at src/llm_client.rs:1219 is closure-form:
//   fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F)
// (panic-safe restoration via internal Drop guard). Use the closure form,
// not a returned-guard pattern.

#[test]
fn http_timeout_defaults_to_300_120() {
    with_env(
        &[
            ("QUORUM_HTTP_TIMEOUT", None),
            ("QUORUM_HTTP_READ_TIMEOUT", None),
        ],
        || {
            let cfg = HttpTimeouts::from_env();
            assert_eq!(cfg.total, Duration::from_secs(300));
            assert_eq!(cfg.per_read, Duration::from_secs(120));
        },
    );
}

#[test]
fn http_timeout_env_override_total_and_read() {
    with_env(
        &[
            ("QUORUM_HTTP_TIMEOUT", Some("600")),
            ("QUORUM_HTTP_READ_TIMEOUT", Some("180")),
        ],
        || {
            let cfg = HttpTimeouts::from_env();
            assert_eq!(cfg.total, Duration::from_secs(600));
            assert_eq!(cfg.per_read, Duration::from_secs(180));
        },
    );
}

#[test]
fn http_timeout_env_invalid_falls_back_to_default() {
    with_env(
        &[
            ("QUORUM_HTTP_TIMEOUT", Some("not-a-number")),
            ("QUORUM_HTTP_READ_TIMEOUT", Some("")),
        ],
        || {
            let cfg = HttpTimeouts::from_env();
            assert_eq!(cfg.total, Duration::from_secs(300));
            assert_eq!(cfg.per_read, Duration::from_secs(120));
        },
    );
}

#[test]
fn http_timeout_zero_rejected_falls_back_to_default() {
    with_env(
        &[
            ("QUORUM_HTTP_TIMEOUT", Some("0")),
            ("QUORUM_HTTP_READ_TIMEOUT", Some("0")),
        ],
        || {
            let cfg = HttpTimeouts::from_env();
            // Zero would mean instant deadline — reject and use default.
            assert_eq!(cfg.total, Duration::from_secs(300));
            assert_eq!(cfg.per_read, Duration::from_secs(120));
        },
    );
}
```

**Step 2: Verify failure**

```bash
rtk cargo test --bin quorum http_timeout 2>&1 | tail -10
```

Expected: `HttpTimeouts not defined`.

**Step 3: Implement**

```rust
#[derive(Debug, Clone, Copy)]
pub(crate) struct HttpTimeouts {
    pub total: Duration,
    pub per_read: Duration,
}

impl Default for HttpTimeouts {
    fn default() -> Self {
        Self {
            total: Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS),
            per_read: Duration::from_secs(DEFAULT_HTTP_READ_TIMEOUT_SECS),
        }
    }
}

impl HttpTimeouts {
    pub(crate) fn from_env() -> Self {
        let parse = |var: &str, default_secs: u64| -> Duration {
            std::env::var(var)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|&n| n > 0)
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(default_secs))
        };
        Self {
            total: parse("QUORUM_HTTP_TIMEOUT", DEFAULT_HTTP_TIMEOUT_SECS),
            per_read: parse("QUORUM_HTTP_READ_TIMEOUT", DEFAULT_HTTP_READ_TIMEOUT_SECS),
        }
    }
}
```

Wire into `OpenAiClient::new` replacing the existing `.timeout(Duration::from_secs(300))` block:

```rust
let timeouts = HttpTimeouts::from_env();
let http = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(10))
    .read_timeout(timeouts.per_read)
    .timeout(timeouts.total)
    .build()?;
```

**Step 4: Verify pass**

```bash
rtk cargo test --bin quorum http_timeout 2>&1 | tail -5
```

**Step 5: Commit**

```bash
git add src/llm_client.rs
git commit -m "feat(llm_client): tighten HTTP timeouts to 120s/60s + env-configurable (#117)"
```

---

### Task 4: `send_with_retry` helper + wire into 3 POST sites

**Files:**
- Modify: `src/llm_client.rs::OpenAiClient` impl — add `send_with_retry` method.
- Modify: `chat_completion`, `responses_api`, `chat_with_tools` — replace `.send().await?` with `self.send_with_retry(req).await?`.

**Step 1: Write failing wiremock integration tests (RED)**

New `tests/llm_client_retry.rs` integration test or in-module wiremock tests. (Using in-module to share `OpenAiClient::new_with_policy` access.)

```rust
#[tokio::test]
async fn send_with_retry_succeeds_after_429() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server)
        .await;

    let _g = with_env(&[
        ("QUORUM_BASE_URL", Some(&server.uri())),
        ("QUORUM_API_KEY", Some("sk-test")),
        ("QUORUM_ALLOWED_BASE_URL_HOSTS", Some("127.0.0.1")),
        ("QUORUM_ALLOW_PRIVATE_BASE_URL", Some("1")),
    ]);

    let client = OpenAiClient::new().expect("must construct");
    let res = client.chat_completion("gpt-5.4", "test prompt").await;
    assert!(res.is_ok(), "expected success after retry, got {res:?}");
}

#[tokio::test]
async fn send_with_retry_succeeds_after_503() { /* ... 503-then-200 ... */ }

#[tokio::test]
async fn send_with_retry_does_not_retry_400() {
    // Single mock returns 400; expect a single attempt + bail.
    // Verify request count via server.received_requests().len() == 1.
}

#[tokio::test]
async fn send_with_retry_aborts_when_overall_deadline_exceeded() {
    // Mock always returns 429 with a Retry-After much larger than the
    // (test-shrunken) overall deadline. After exhausting retries within
    // the budget, expect the call to surface the last 429 (caller's error
    // path then bails with "API Error (429)").
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "60"))
        .mount(&server).await;

    with_env(
        &[
            ("QUORUM_BASE_URL", Some(&server.uri())),
            ("QUORUM_API_KEY", Some("sk-test")),
            ("QUORUM_ALLOWED_BASE_URL_HOSTS", Some("127.0.0.1")),
            ("QUORUM_ALLOW_PRIVATE_BASE_URL", Some("1")),
        ],
        || {},
    );
    // Recreate inside an async-friendly setup; simplest: build client outside
    // with_env, then mutate via the test-only setter so we don't pollute env.
    let mut client = OpenAiClient::new().expect("must construct");
    client.set_overall_retry_deadline_for_test(Duration::from_secs(2));

    let res = client.chat_completion("gpt-5.4", "test prompt").await;
    assert!(res.is_err(), "expected error, got {res:?}");
    // Don't assert exact wall-clock; just bound it loosely.
    // (Antipattern flagged by testing-antipatterns expert: tight timing asserts.)
}

#[tokio::test]
async fn send_with_retry_does_not_retry_after_5xx_then_4xx() {
    // Proves is_retriable is checked on EACH attempt, not cached on first.
    // 500 → retry → 400 → bail (no further retry).
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)  // wiremock 0.6: registration order with priority — pinned assumption
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server).await;

    let client = build_test_client(&server.uri());
    let res = client.chat_completion("gpt-5.4", "test prompt").await;
    assert!(res.is_err(), "400 should bail; got {res:?}");
    // Verify exactly 2 attempts: 500 + 400, no 3rd.
    let received = server.received_requests().await.expect("requests");
    assert_eq!(received.len(), 2, "expected exactly 2 attempts");
}

#[tokio::test]
async fn send_with_retry_falls_back_to_backoff_when_no_retry_after_header() {
    // 429 with NO Retry-After header → fall back to compute_backoff (jittered).
    // Just assert the call eventually succeeds (backoff fires).
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429))  // no Retry-After header
        .up_to_n_times(1)
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server).await;

    let client = build_test_client(&server.uri());
    let res = client.chat_completion("gpt-5.4", "test prompt").await;
    assert!(res.is_ok(), "expected success after backoff retry, got {res:?}");
}

#[tokio::test]
async fn send_with_retry_treats_garbage_retry_after_as_absent() {
    // 429 with malformed Retry-After ("not-a-number") → parse returns None
    // → falls back to backoff. Should NOT panic and SHOULD still retry.
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "not-a-number"))
        .up_to_n_times(1)
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server).await;

    let client = build_test_client(&server.uri());
    let res = client.chat_completion("gpt-5.4", "test prompt").await;
    assert!(res.is_ok(), "garbage Retry-After must not panic; expected success, got {res:?}");
}

#[tokio::test]
async fn send_with_retry_is_reentrant_safe_under_concurrent_callers() {
    // Quorum's value prop is `--parallel N` review pool. Smoke test that
    // 4 concurrent calls each surviving a 429 retry don't deadlock or
    // share state. Doesn't assert statistical jitter independence — just
    // that the helper is reentrant.
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    // Each caller hits this once with 429, then succeeds.
    // 4 callers × 2 calls each = 8 hits expected.
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(4)
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server).await;

    let client = std::sync::Arc::new(build_test_client(&server.uri()));
    let mut handles = vec![];
    for _ in 0..4 {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            c.chat_completion("gpt-5.4", "test").await
        }));
    }
    for h in handles {
        let r = h.await.expect("task join");
        assert!(r.is_ok(), "concurrent caller failed: {r:?}");
    }
}

#[tokio::test]
async fn send_with_retry_honors_retry_after_when_it_fits_budget() {
    // 429 with Retry-After: 1 then 200. Assert success + that elapsed
    // wall-clock is at least the header delay (loose bound, not exact).
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server).await;

    let started = std::time::Instant::now();
    let client = build_test_client(&server.uri());
    let res = client.chat_completion("gpt-5.4", "test prompt").await;
    assert!(res.is_ok(), "expected success after Retry-After-honored retry, got {res:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "expected >= ~1s elapsed (header said 1s); got {:?}",
        started.elapsed()
    );
}
```

**Step 2: Verify failure**

```bash
rtk cargo test --bin quorum send_with_retry 2>&1 | tail -10
```

**Step 3: Implement `send_with_retry`**

Add as a method on `OpenAiClient`:

```rust
async fn send_with_retry(
    &self,
    req: reqwest::RequestBuilder,
) -> anyhow::Result<reqwest::Response> {
    let started_at = std::time::Instant::now();
    // Bounds the retry *tail*. Per-call timeout is still self.timeouts.total
    // (default 300s) — single happy-path calls are unaffected by this knob.
    let overall_deadline = self.overall_retry_deadline;
    let mut last_status: Option<reqwest::StatusCode> = None;

    for attempt in 0..=MAX_RETRIES {
        let cloned = req
            .try_clone()
            .ok_or_else(|| anyhow::anyhow!("request body must be clonable for retry"))?;
        let resp = match cloned.send().await {
            Ok(r) => r,
            Err(e) => {
                // Only retry truly transient transport errors. reqwest 0.12 also
                // produces builder/redirect/decode/body errors that are NOT
                // retriable — those should bail immediately.
                let transient = e.is_timeout() || e.is_connect() || e.is_request();
                if !transient || attempt == MAX_RETRIES {
                    return Err(anyhow::anyhow!(
                        "transport error after {} attempt(s): {}",
                        attempt + 1,
                        e
                    ));
                }
                let base = compute_backoff(attempt);
                let backoff = apply_jitter(base, jitter_unit());
                if !retry_after_fits_budget(started_at, overall_deadline, backoff, std::time::Instant::now()) {
                    anyhow::bail!("transport error and retry budget exhausted: {e}");
                }
                tracing::warn!(error = %e, attempt, backoff_ms = %backoff.as_millis(), "transport error; will retry");
                tokio::time::sleep(backoff).await;
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() || !is_retriable(status) || attempt == MAX_RETRIES {
            return Ok(resp);
        }
        last_status = Some(status);

        // Retry-After (server-supplied) wins; otherwise jittered backoff.
        let retry_after_hdr = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after_value);
        let backoff = match retry_after_hdr {
            Some(d) => d,
            None => apply_jitter(compute_backoff(attempt), jitter_unit()),
        };

        if !retry_after_fits_budget(started_at, overall_deadline, backoff, std::time::Instant::now()) {
            // Return the last response so the caller's error handling fires.
            return Ok(resp);
        }
        tracing::warn!(status = %status, attempt, backoff_ms = %backoff.as_millis(), "retriable upstream; retrying");
        tokio::time::sleep(backoff).await;
    }
    // Loop falls through only if MAX_RETRIES + 1 attempts all errored
    // without returning Response (transport error path above already bails).
    Err(anyhow::anyhow!(
        "exhausted retries (last status: {:?})",
        last_status
    ))
}

/// Cheap per-call jitter source — avoids pulling `rand` for a single use site.
/// Returns a value in [0.0, 1.0). Quality matters less than independence
/// across concurrent callers; nanosecond Instant works fine for that.
fn jitter_unit() -> f64 {
    let nanos = std::time::Instant::now()
        .elapsed()
        .subsec_nanos() as u64;
    (nanos % 1_000_000) as f64 / 1_000_000.0
}
```

Wire into 3 POST sites by replacing:

```rust
let resp = self.http.post(&url)
    .header(...)
    .json(&body)
    .send()
    .await?;
```

with:

```rust
let req = self.http.post(&url)
    .header(...)
    .json(&body);
let resp = self.send_with_retry(req).await?;
```

**Also store `timeouts` + `overall_retry_deadline` on the struct** so `send_with_retry` can read them:

```rust
pub struct OpenAiClient {
    // ... existing fields
    timeouts: HttpTimeouts,
    /// Bounds the retry-loop wall-clock — separate from the per-call total
    /// timeout. Production default = OVERALL_RETRY_DEADLINE_SECS (600s);
    /// test code can shrink via `set_overall_retry_deadline_for_test`.
    overall_retry_deadline: Duration,
}
```

Populate in `new` from `HttpTimeouts::from_env()` and `Duration::from_secs(OVERALL_RETRY_DEADLINE_SECS)`.

Add a test-visible setter so the wiremock deadline test can shrink the budget without polluting the env (PAL flagged that env-driven override is the wrong shape here):

```rust
impl OpenAiClient {
    /// Test-only knob for shrinking the retry budget so deadline-exhaustion
    /// tests don't have to wait 600s. Production callers have no reason to
    /// touch this — the default is OVERALL_RETRY_DEADLINE_SECS.
    #[cfg(test)]
    pub(crate) fn set_overall_retry_deadline_for_test(&mut self, d: Duration) {
        self.overall_retry_deadline = d;
    }
}
```

**Step 4: Verify pass**

```bash
rtk cargo test --bin quorum 2>&1 | tail -5
```

Expected: 1579+ tests pass (existing) + 4 new wiremock tests pass.

**Step 5: Commit**

```bash
git add src/llm_client.rs
git commit -m "feat(llm_client): send_with_retry — overall-deadline-bounded retry on 429/5xx (#117)"
```

---

### Task 5: CHANGELOG + verification

**Files:**
- Modify: `CHANGELOG.md` — add `### Reliability` block under `[Unreleased]`.

**Step 1: Update CHANGELOG**

```markdown
### Reliability
- `OpenAiClient` now retries transient `429`/`500`/`502`/`503`/`504` responses
  up to 3 times with exponential backoff (500ms → 1s → 2s, capped at 30s, with
  ±20% jitter when the server doesn't supply `Retry-After`) and honors
  `Retry-After` headers (seconds + HTTP-date forms) when they fit the overall
  retry budget (default 600s). Per-call timeout unchanged at 300s total — the
  retry budget bounds only the retry tail, not happy-path single calls. Only
  truly transient transport errors (timeout/connect/request) are retried;
  decode/builder errors bail immediately.
- New `read_timeout` (default 120s) layered on top of the existing 300s total
  timeout — protects against trickle-after-first-byte attacks without
  affecting healthy "think-then-burst" LLM responses. Both timeouts are
  configurable via `QUORUM_HTTP_TIMEOUT` and `QUORUM_HTTP_READ_TIMEOUT` env
  vars (positive integers, seconds). Closes #117.
```

**Step 2: Full test suite**

```bash
rtk cargo test --bin quorum 2>&1 | tail -3
rtk cargo clippy --all-targets 2>&1 | tail -10
rtk cargo build --release 2>&1 | tail -3
```

**Step 3: Quorum self-review on the changed surface**

```bash
QUORUM_ALLOWED_BASE_URL_HOSTS=litellm.5745.house \
  cargo run -- review src/llm_client.rs --no-color 2>&1 | tail -50
```

Triage every finding into in-branch (fix here) vs. pre-existing (file as new issue).

**Step 4: PAL review**

```bash
# Use mcp__pal__codereview on src/llm_client.rs with focus on retry safety
# + idempotency assumptions + deadline correctness.
```

**Step 5: CodeRabbit review**

```bash
coderabbit review --agent --base main --type committed 2>&1 | tail -50
```

Iterate until clean.

**Step 6: Commit + finishing**

Record feedback verdicts via `quorum feedback` for every finding (TP / FP / wontfix). Then merge via `superpowers:finishing-a-development-branch`.

---

## Risks and mitigations

- **Risk:** `try_clone()` returns `None` for streaming bodies. **Mitigation:** all 3 POST sites use `.json(&body)` which produces a clonable `Bytes` body. The `.ok_or_else` panic-style return guards against future changes that introduce streaming bodies.
- **Risk:** wiremock binds an ephemeral port — could collide in CI under heavy parallelism. **Mitigation:** `MockServer::start()` uses random ports.
- **Risk:** `tokio::time::sleep` inside the retry loop runs in caller's runtime; if the caller is on a `current_thread` runtime under `block_on_async`, sleep should still work (issue #81 is about deadlocks via `block_on`, not async sleep). Worth a smoke test.
- **Risk:** real-world Retry-After headers can be unreasonably large (e.g., 3600s). **Mitigation:** `retry_after_fits_budget` filters those out — if it doesn't fit, we abort retries and return the last status.
- **Risk:** `jitter_unit()` uses `Instant::now()` nanos rather than a real RNG. **Mitigation:** quality matters less than independence between concurrent callers; nanosecond resolution gives enough variance at parallel=4 without pulling a `rand` dependency. If we ever bump to parallel=20+, swap for a proper RNG.

## Test helper to add (build_test_client)

```rust
#[cfg(test)]
fn build_test_client(server_uri: &str) -> OpenAiClient {
    // Snapshot env, build client, restore. Closure-form with_env handles
    // panic-safe restoration via Drop guard.
    use std::cell::Cell;
    let client_cell: Cell<Option<OpenAiClient>> = Cell::new(None);
    with_env(
        &[
            ("QUORUM_BASE_URL", Some(server_uri)),
            ("QUORUM_API_KEY", Some("sk-test")),
            ("QUORUM_ALLOWED_BASE_URL_HOSTS", Some("127.0.0.1")),
            ("QUORUM_ALLOW_PRIVATE_BASE_URL", Some("1")),
        ],
        || {
            client_cell.set(Some(OpenAiClient::new().expect("must construct")));
        },
    );
    client_cell.into_inner().expect("client built")
}
```

## Out-of-scope follow-ups to file as issues if Quorum review surfaces

- Per-attempt telemetry on retries
- Circuit breaker across the parallel pool
- Backoff jitter
