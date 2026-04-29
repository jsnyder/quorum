# Issue #119: SSRF + cred-exfil hardening on `OpenAiClient`

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Harden `src/llm_client.rs::OpenAiClient::new` against SSRF + credential exfiltration via `base_url`, and scrub bearer tokens / API keys from error-bail messages on all three POST sites.

**Architecture:** Two new pure helpers in `src/llm_client.rs`:
1. `validate_base_url(url: &str, policy: &BaseUrlPolicy) -> anyhow::Result<()>` — secure-by-default URL validation
2. `sanitize_error_body(raw: &str) -> String` — scrub Bearer/sk-/api-key patterns, truncate

**Tech Stack:** Rust, `url` crate (already a dep), `regex` (already a dep), `std::net::IpAddr`.

---

## Threat model

`QUORUM_BASE_URL` is user-configured shell env. A misconfiguration or shell-eval injection causes `OpenAiClient::new(base_url, api_key)` to send `Authorization: Bearer <api_key>` to any host the URL points at. Concrete attacks:

- `QUORUM_BASE_URL=https://attacker.example.com/v1` — typo or DNS-controlled domain → API key leaks.
- `QUORUM_BASE_URL=https://user:password@proxy.example.com/v1` — embedded creds flow on every request, surface in logs.
- `QUORUM_BASE_URL=http://169.254.169.254/v1` — IMDS metadata exfil on cloud instances.
- `QUORUM_BASE_URL=http://10.0.0.1/v1` — typo lands API key on internal infrastructure.

Plus: error-bail sites at lines 233-236, 287-290, 358-361 echo response body into `anyhow::bail!` messages, which propagate to terminal/daemon-logs/telemetry. Some OAI-compatible gateways echo back request headers (Bearer token!) or body (prompt+source code) on validation errors.

## Defense layers (per user spec, secure-by-design + actionable fail-fast)

| Layer | Default | Override |
|---|---|---|
| Reject embedded `user:pass@` in URL | always on, no opt-out | — |
| Allowlist enforcement (default: `api.openai.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`) | on | `QUORUM_ALLOWED_BASE_URL_HOSTS=foo,bar` (additive — extends defaults) |
| Reject private/loopback/link-local IPs | on | `QUORUM_ALLOW_PRIVATE_BASE_URL=1` |
| Bypass all configurable validation | — | `QUORUM_UNSAFE_BASE_URL=1` |
| Sanitize error body (scrub Bearer/sk-/api-key) | always on | — |

**Error messages must be actionable** — every rejection includes the env var to set or the bypass flag.

---

## Task 1: Plan + test names committed (this doc).

Done by writing this file.

---

## Task 2: `BaseUrlPolicy` + `validate_base_url` helper

**Files:** `src/llm_client.rs` (above `impl OpenAiClient`).

```rust
/// Built-in allowlist of public OAI-compatible hosts. Users on other
/// providers (LiteLLM, Ollama, Azure OpenAI, on-prem gateways) extend
/// via QUORUM_ALLOWED_BASE_URL_HOSTS or bypass via QUORUM_UNSAFE_BASE_URL.
const DEFAULT_ALLOWED_BASE_URL_HOSTS: &[&str] = &[
    "api.openai.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
];

/// Policy controlling base_url validation. Built from env via `from_env`;
/// strictest defaults from `Default::default()`.
#[derive(Debug, Default, Clone)]
pub struct BaseUrlPolicy {
    /// Hosts to allow IN ADDITION to DEFAULT_ALLOWED_BASE_URL_HOSTS.
    /// Exact-match — does NOT support wildcards or subdomains (would
    /// expand attack surface for typo-based DNS hijacking).
    pub additional_allowed_hosts: Vec<String>,
    /// If true, allow private/loopback/link-local/unspecified IPs.
    pub allow_private_ips: bool,
    /// If true, skip allowlist + IP checks (still enforces embedded-creds).
    pub unsafe_bypass: bool,
}

impl BaseUrlPolicy {
    /// Build from env vars. Empty/missing = default behavior.
    pub fn from_env() -> Self {
        let additional_allowed_hosts = std::env::var("QUORUM_ALLOWED_BASE_URL_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let allow_private_ips = matches_truthy(&std::env::var("QUORUM_ALLOW_PRIVATE_BASE_URL").unwrap_or_default());
        let unsafe_bypass = matches_truthy(&std::env::var("QUORUM_UNSAFE_BASE_URL").unwrap_or_default());
        Self { additional_allowed_hosts, allow_private_ips, unsafe_bypass }
    }
}

fn matches_truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// Validate a base_url against the policy. Returns a fail-fast error
/// with an actionable message on rejection.
pub fn validate_base_url(base_url: &str, policy: &BaseUrlPolicy) -> anyhow::Result<()> {
    let parsed = url::Url::parse(base_url)
        .map_err(|e| anyhow::anyhow!("base_url {base_url:?} is not a valid URL: {e}"))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!(
            "base_url {base_url:?} must use http or https scheme, got {:?}",
            parsed.scheme()
        );
    }

    // Always-on: embedded credentials. No opt-out — no legitimate use case.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!(
            "base_url must not contain embedded credentials (user:password@host). \
             Pass the API key via QUORUM_API_KEY instead."
        );
    }

    // Configurable layers — bypass skips both.
    if policy.unsafe_bypass {
        return Ok(());
    }

    let host = parsed.host().ok_or_else(|| anyhow::anyhow!("base_url has no host"))?;

    // Per-branch logic: private/loopback hosts gate on `allow_private_ips`
    // and bypass the allowlist when permitted (running Ollama on localhost
    // shouldn't ALSO require adding "localhost" to QUORUM_ALLOWED_BASE_URL_HOSTS).
    // Public hosts still go through the allowlist.
    match host {
        url::Host::Ipv4(ip) => {
            if ipv4_is_local_or_special(ip) {
                if !policy.allow_private_ips {
                    anyhow::bail!(actionable_error_for_private_ip(&base_url, &ip.to_string()));
                }
                // allow_private_ips opted in: skip allowlist for private IPs.
            } else if !host_in_allowlist(&ip.to_string(), &policy.additional_allowed_hosts) {
                anyhow::bail!(actionable_error_for_unknown_host(&base_url, &ip.to_string(), policy));
            }
        }
        url::Host::Ipv6(ip) => {
            // IPv4-mapped IPv6 (::ffff:127.0.0.1) → apply IPv4 rules so
            // loopback isn't bypassed by IPv6 form.
            let is_local = if let Some(v4) = ipv6_to_ipv4_mapped(ip) {
                ipv4_is_local_or_special(v4)
            } else {
                ipv6_is_local_or_special(ip)
            };
            if is_local {
                if !policy.allow_private_ips {
                    anyhow::bail!(actionable_error_for_private_ip(&base_url, &ip.to_string()));
                }
            } else if !host_in_allowlist(&ip.to_string(), &policy.additional_allowed_hosts) {
                anyhow::bail!(actionable_error_for_unknown_host(&base_url, &ip.to_string(), policy));
            }
        }
        url::Host::Domain(d) => {
            let dn = d.to_ascii_lowercase();
            if is_localhost_name(&dn) {
                if !policy.allow_private_ips {
                    anyhow::bail!(actionable_error_for_private_ip(&base_url, &dn));
                }
                // allow_private_ips opted in: skip allowlist for localhost-family.
            } else if !host_in_allowlist(&dn, &policy.additional_allowed_hosts) {
                anyhow::bail!(actionable_error_for_unknown_host(&base_url, &dn, policy));
            }
        }
    }

    Ok(())
}

fn host_in_allowlist(host: &str, additional: &[String]) -> bool {
    let h = host.to_ascii_lowercase();
    DEFAULT_ALLOWED_BASE_URL_HOSTS.iter().any(|a| *a == h)
        || additional.iter().any(|a| a == &h)
}

fn is_localhost_name(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host == "ip6-localhost"
}

fn ipv4_is_local_or_special(ip: std::net::Ipv4Addr) -> bool {
    ip.is_loopback()        // 127.0.0.0/8
        || ip.is_private()  // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local() // 169.254/16
        || ip.is_unspecified() // 0.0.0.0
        || ip.is_broadcast()   // 255.255.255.255
}

fn ipv6_is_local_or_special(ip: std::net::Ipv6Addr) -> bool {
    ip.is_loopback() // ::1
        || ip.is_unspecified() // ::
        || is_ipv6_unique_local(&ip)         // fc00::/7
        || is_ipv6_link_local(&ip)           // fe80::/10
}

// Manual range checks (these std methods are unstable on some Rust versions).
fn is_ipv6_unique_local(ip: &std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}
fn is_ipv6_link_local(ip: &std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn actionable_error_for_private_ip(url: &str, ip_or_name: &str) -> String {
    format!(
        "base_url {url:?} resolves to a private/loopback/link-local address ({ip_or_name}). \
         To allow this for Ollama / on-prem LLMs, set:\n  \
         export QUORUM_ALLOW_PRIVATE_BASE_URL=1"
    )
}

fn actionable_error_for_unknown_host(url: &str, host: &str, policy: &BaseUrlPolicy) -> String {
    let mut all: Vec<String> = DEFAULT_ALLOWED_BASE_URL_HOSTS.iter().map(|s| s.to_string()).collect();
    all.extend(policy.additional_allowed_hosts.iter().cloned());
    let allowed = if all.is_empty() { "(none)".to_string() } else { all.join(", ") };
    format!(
        "base_url {url:?} host {host:?} is not on the allowlist.\n\n\
         Allowed hosts: {allowed}\n\n\
         To allow this host, set:\n  \
         export QUORUM_ALLOWED_BASE_URL_HOSTS={host}\n\
         (Comma-separate multiple hosts; additive to the built-in defaults.)\n\n\
         To bypass validation entirely (development/testing only):\n  \
         export QUORUM_UNSAFE_BASE_URL=1"
    )
}
```

**Tests in `src/llm_client.rs` test module:**

```rust
#[test]
fn validate_base_url_rejects_embedded_credentials() { ... }

#[test]
fn validate_base_url_rejects_embedded_credentials_even_with_unsafe_bypass() {
    // Always-on guard, no opt-out.
}

#[test]
fn validate_base_url_accepts_default_allowed_hosts() {
    // api.openai.com, api.anthropic.com, generativelanguage.googleapis.com all pass.
}

#[test]
fn validate_base_url_rejects_unknown_host_with_actionable_error() {
    // assert error message contains QUORUM_ALLOWED_BASE_URL_HOSTS, the host, and the bypass var.
}

#[test]
fn validate_base_url_accepts_host_added_via_policy() { ... }

#[test]
fn validate_base_url_rejects_loopback_ipv4() { ... }

#[test]
fn validate_base_url_rejects_rfc1918_ipv4() {
    // 10.0.0.1, 172.16.0.1, 192.168.1.1
}

#[test]
fn validate_base_url_rejects_link_local_ipv4() {
    // 169.254.169.254 — IMDS / cloud metadata exfil.
}

#[test]
fn validate_base_url_rejects_loopback_ipv6() { ... }

#[test]
fn validate_base_url_rejects_unique_local_ipv6() {
    // fc00::/7
}

#[test]
fn validate_base_url_rejects_localhost_name_as_loopback() {
    // The DNS name "localhost" is rejected as loopback.
}

#[test]
fn validate_base_url_unsafe_bypass_skips_allowlist_and_ip_check() {
    // unsafe_bypass = true; arbitrary public IP literal passes.
}

#[test]
fn validate_base_url_allow_private_ips_lets_ollama_through() {
    // allow_private_ips = true + host "localhost" in additional list → 127.0.0.1 passes.
}

#[test]
fn validate_base_url_rejects_non_http_scheme() { ... }
```

---

## Task 3: Wire `validate_base_url` into `OpenAiClient::new`

**Files:** `src/llm_client.rs` lines 155-172.

Replace existing scheme check + URL parse with a `validate_base_url(base_url, &BaseUrlPolicy::from_env())?` call. Preserve the trim/store logic.

**Test:** `OpenAiClient::new` returns Err on each rejection case (covered by the helper tests; one integration-style test that calls `OpenAiClient::new` directly with bad URL).

---

## Task 4: `sanitize_error_body` helper + apply at 3 bail sites

**Files:** `src/llm_client.rs`.

```rust
/// Scrub bearer tokens / api-key shapes from an error body before it
/// flows into terminal output or daemon logs. Some OAI-compatible
/// gateways echo back request headers (Authorization: Bearer ...) and
/// request body (prompt + source code) on validation errors.
fn sanitize_error_body(raw: &str) -> String {
    // std::sync::LazyLock is the modern (Rust 1.80+) replacement for
    // once_cell::sync::Lazy — no extra dep needed.
    use std::sync::LazyLock;
    static SECRET_PAT: LazyLock<regex::Regex> = LazyLock::new(|| {
        // bearer charset includes `.` and `=` for JWT (header.payload.signature)
        // and base64 padding; api-key field separator allows space, underscore,
        // and hyphen so `api key:`, `api_key=`, `api-key:` all match.
        regex::Regex::new(
            r#"(?i)(bearer\s+[A-Za-z0-9_\-\.=]+|sk-[A-Za-z0-9_\-]+|api[\s_-]?key["']?\s*[:=]\s*["']?[A-Za-z0-9_\-]+)"#
        ).expect("static regex")
    });
    let scrubbed = SECRET_PAT.replace_all(raw, "[REDACTED]");
    scrubbed.chars().take(200).collect()
}
```

Apply at each of the three `bail!` sites:
- Line 233-236 (`chat_completion`)
- Line 287-290 (`responses_api`)
- Line 358-361 (`chat_with_tools`)

Replace `let truncated: String = body.chars().take(200).collect();` with `let truncated = sanitize_error_body(&body);`.

**Tests:**

```rust
#[test]
fn sanitize_error_body_scrubs_bearer_token() {
    let raw = "401: invalid bearer abcXYZ123_456";
    let s = sanitize_error_body(raw);
    assert!(!s.contains("abcXYZ123_456"), "got: {s}");
    assert!(s.contains("[REDACTED]"));
}

#[test]
fn sanitize_error_body_scrubs_sk_prefix_keys() {
    for k in ["sk-abc123", "sk-proj-xyz789", "sk-live-foo", "sk-test-bar"] {
        let raw = format!("error: invalid key {k}");
        let s = sanitize_error_body(&raw);
        assert!(!s.contains(k), "got: {s} for input {raw}");
    }
}

#[test]
fn sanitize_error_body_scrubs_api_key_field() {
    let raw = r#"{"error":"bad request","api_key":"my-secret-value"}"#;
    let s = sanitize_error_body(raw);
    assert!(!s.contains("my-secret-value"), "got: {s}");
}

#[test]
fn sanitize_error_body_truncates_to_200_chars() {
    let raw = "x".repeat(500);
    let s = sanitize_error_body(&raw);
    assert!(s.chars().count() <= 200);
}

#[test]
fn sanitize_error_body_preserves_safe_content() {
    let raw = "rate limit exceeded, retry in 30s";
    let s = sanitize_error_body(raw);
    assert_eq!(s, raw);
}
```

---

## Task 5: CHANGELOG entry

**Files:** `CHANGELOG.md`.

Add under `[Unreleased]` -> Security:

```markdown
- **OpenAiClient base_url hardened against SSRF + credential exfiltration (#119).** Pre-fix `OpenAiClient::new` accepted any caller-controlled http(s) URL and sent `Authorization: Bearer <api_key>` to it on every request. Concrete attacks: typo'd or shell-injected `QUORUM_BASE_URL` exfiltrated the key to attacker-chosen hosts; `https://user:pass@host/v1` flowed embedded creds on every call; `http://169.254.169.254/v1` exfiltrated to cloud-IMDS endpoints. Fix: `validate_base_url` enforces three layers — always-on rejection of embedded credentials (no opt-out), default-on allowlist of public OAI-compatible hosts (`api.openai.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`) extensible via `QUORUM_ALLOWED_BASE_URL_HOSTS`, and default-on rejection of loopback/RFC1918/link-local IP literals + `localhost` name with `QUORUM_ALLOW_PRIVATE_BASE_URL=1` opt-in for Ollama / on-prem use. Total bypass available via `QUORUM_UNSAFE_BASE_URL=1` for development. Errors are actionable — every rejection points at the exact env var to set.

- **Error-body sanitization on LLM POST failures (#119).** Pre-fix `chat_completion`, `responses_api`, and `chat_with_tools` echoed the first 200 chars of the response body into `anyhow::bail!` messages, which propagate to terminal output, daemon logs, and telemetry. Some OAI-compatible gateways echo back request headers (Bearer token) or body (prompt + source code) on validation errors. Fix: `sanitize_error_body` scrubs `bearer\s+...`, `sk-...`, and `api[_-]?key=...` patterns via regex before truncation; applied at all three POST error sites.
```

---

## Phase 3 reconciled test set (post-antipattern review 2026-04-29)

Antipattern reviewer flagged: error-message contract should be a separate test; IPv4/IPv6 boundary tests required for hand-rolled bitmasks; truncation semantics must be `== 200` not `<= 200` (else no-op `""` passes); provider-derived key shapes (`sk-svcacct-`, `sk-org-`, `sk-ant-api03-`) should be tested even though the catch-all regex covers them; env tests must serialize via mutex.

Test-planner flagged: existing `new_accepts_http_and_https_urls` (line ~791) uses `http://localhost:8000` and will fail under default policy — must update.

Final test set:

**`validate_base_url` (16 tests):**
1. `rejects_embedded_credentials`
2. `rejects_embedded_credentials_even_with_unsafe_bypass` (no opt-out)
3. `rejects_embedded_credentials_even_with_allow_private_ips` (no opt-out)
4. `accepts_default_allowed_hosts` (parameterized over 3 defaults)
5. `rejects_unknown_host`
6. `unknown_host_error_message_is_actionable` (asserts on env var names)
7. `accepts_host_added_via_policy`
8. `rejects_loopback_ipv4` (127.0.0.1)
9. `rejects_rfc1918_ipv4` (10.0.0.1, 172.16.0.1, 192.168.1.1) + boundaries (9.255.255.255, 11.0.0.0, 172.15.255.255, 172.32.0.0 must NOT trigger private-IP path; allowlist still rejects)
10. `rejects_link_local_ipv4` (169.254.169.254 IMDS)
11. `rejects_loopback_ipv6` (`::1`, `0:0:0:0:0:0:0:1`, `::ffff:127.0.0.1` IPv4-mapped)
12. `rejects_unique_local_ipv6` (`fc00::`, plus boundary `fbff::` must NOT trigger)
13. `rejects_link_local_ipv6` (`fe80::`, plus boundaries `fe7f::` and `fec0::` must NOT trigger)
14. `rejects_localhost_name_as_loopback`
15. `unsafe_bypass_skips_allowlist_and_ip_check`
16. `allow_private_ips_lets_localhost_through_when_allowlisted`

**`from_env` (3 tests, serialized via Mutex):**
17. `from_env_parses_csv_allowlist_with_trim_and_lowercase`
18. `from_env_strict_truthy` (1, true, yes, on, "0", "false", "")
19. `from_env_empty_yields_default`

**`sanitize_error_body` (7 tests):**
20. `scrubs_bearer_token`
21. `scrubs_openai_key_shapes` (sk-proj-, sk-live-, sk-test-, sk-svcacct-, sk-org-)
22. `scrubs_anthropic_key_shape` (sk-ant-api03-)
23. `scrubs_api_key_json_field`
24. `truncates_to_exactly_200_codepoints_when_input_longer` (== 200, with 500-char input)
25. `multi_byte_utf8_truncation_uses_codepoints_not_bytes` (200 emoji = 200 codepoints, ~800 bytes — pin the spec)
26. `preserves_safe_content_unchanged` (rate-limit message round-trips)

**`OpenAiClient::new` integration (3 tests):**
27. `new_rejects_embedded_credentials`
28. `new_default_allowlist_accepts_openai_host`
29. **UPDATE existing** `new_accepts_http_and_https_urls`: change to use allowlisted host or scope `QUORUM_ALLOW_PRIVATE_BASE_URL=1` for the test.

**Documented test gap** (per antipattern reviewer concern about prompt-echo):
30. `sanitize_error_body_does_not_address_prompt_echo_filed_as_followup` — single test asserting `sanitize_error_body("function add_user(password: 'hunter2')")` returns the passwordstring unchanged (because the scrub only targets bearer/sk- patterns), with comment linking to a follow-up issue. Locks the gap into the test suite as documentation.

Total: 30 tests, 1 existing test updated.

## Verification gates

- `cargo test --bin quorum validate_base_url` — all green
- `cargo test --bin quorum sanitize_error_body` — all green
- `cargo test --bin quorum` — full suite green
- `cargo clippy --bin quorum --tests` — no new warnings on touched lines
- `cargo build --release` — clean

## Out of scope

- Retry/timeout work (#117) — separate issue, separate branch.
- CLI `--unsafe-base-url` flag — env var sufficient for v1; flag can be added later if useful for one-off dev work.
- Wildcard / subdomain matching in allowlist — exact-match only for v1; wildcards expand attack surface.
- Sanitizing prompt-content echo (separate from credential scrubbing) — defer; current cap of 200 chars bounds blast radius.
