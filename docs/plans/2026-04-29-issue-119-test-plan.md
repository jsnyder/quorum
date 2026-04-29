# Issue #119 — SSRF + Credential Leak Hardening: Test Plan

Branch: `fix/llm-client-ssrf-cred-leak`
Targets: `OpenAiClient::new` and three POST sites in `src/llm_client.rs`.
Scope: pure helpers `validate_base_url(&str, &BaseUrlPolicy) -> Result<()>` and `sanitize_error_body(&str) -> String`, plus `BaseUrlPolicy::from_env`.

## 1. Acceptance Criteria

A1. Embedded user-info in `base_url` is **always** rejected (no env opt-out, no flag). Error message MUST NOT echo the password.
A2. With default policy, `base_url` host MUST be in the built-in allowlist (`api.openai.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`) OR present in `QUORUM_ALLOWED_BASE_URL_HOSTS` (comma-separated, case-insensitive, trimmed).
A3. With default policy, loopback / RFC1918 / link-local IP literals AND the literal name `localhost` MUST be rejected. Set `QUORUM_ALLOW_PRIVATE_BASE_URL=1` to permit them.
A4. `QUORUM_UNSAFE_BASE_URL=1` skips A2 + A3 but NOT A1 (cred-embed) and NOT scheme/parse checks.
A5. `sanitize_error_body` redacts `Bearer <token>`, `sk-...`, `sk-ant-...`, `api-key: ...`, `authorization: ...` (case-insensitive) **before** the 200-char truncation. Output length ≤ 200 chars.
A6. All three POST sites (`chat_completion`, `responses_api`, `chat_with_tools`) route their error body through `sanitize_error_body`.
A7. Existing `client_creation`, `new_rejects_url_without_scheme`, `new_rejects_non_http_scheme`, `new_accepts_http_and_https_urls` still pass with default policy (defaults to `localhost` allowed via test seam OR test uses an allowlisted host).
A8. Default-policy errors are actionable — they name the env var that would unlock the path.
A9. Per project rules: no emojis in code or messages.

## 2. Test Inventory

Existing module: tests live under `#[cfg(test)] mod tests` at the bottom of `src/llm_client.rs`. Follow that style (snake_case names, descriptive comments tying back to issue #119).

### 2a. `validate_base_url` — input parsing & scheme

| # | Name | Intent | Setup |
|---|------|--------|-------|
| 1 | `validate_rejects_unparseable_url` | Garbage string fails fast | `validate("::not a url::", &Policy::default())` |
| 2 | `validate_rejects_missing_scheme` | Bare host fails | `"api.openai.com/v1"` |
| 3 | `validate_rejects_non_http_scheme` | `file://` rejected | `"file:///etc/passwd"` |
| 4 | `validate_rejects_ftp_scheme` | `ftp://` rejected | `"ftp://api.openai.com"` (catches scheme-list drift) |
| 5 | `validate_rejects_data_scheme` | `data:` rejected | `"data:text/plain,hi"` |

### 2b. `validate_base_url` — credential embed (always-on, A1)

| # | Name | Intent | Setup |
|---|------|--------|-------|
| 6 | `validate_rejects_embedded_userinfo_user_only` | `user@` form blocked | `"https://attacker@api.openai.com/v1"` |
| 7 | `validate_rejects_embedded_userinfo_user_pass` | `user:pass@` blocked | `"https://u:p@api.openai.com"` |
| 8 | `validate_error_does_not_echo_password` | Password not in error string | construct with `"https://u:supersecret@api.openai.com"`, assert `!err.contains("supersecret")` |
| 9 | `validate_rejects_userinfo_even_with_unsafe_bypass` | A1 wins over A4 | set `unsafe_bypass=true`; userinfo URL still errors |
| 10 | `validate_rejects_percent_encoded_userinfo` | `%40` = `@` decoded; `https://u%40b:p@host` still detected | URL parser exposes `username()` non-empty |

### 2c. `validate_base_url` — host allowlist (A2)

| # | Name | Intent | Setup |
|---|------|--------|-------|
| 11 | `validate_accepts_default_allowlist_openai` | builtin host OK | `"https://api.openai.com/v1"` + default policy |
| 12 | `validate_accepts_default_allowlist_anthropic` | builtin host OK | `"https://api.anthropic.com"` |
| 13 | `validate_accepts_default_allowlist_google` | builtin host OK | `"https://generativelanguage.googleapis.com"` |
| 14 | `validate_rejects_non_allowlisted_public_host` | random public host blocked | `"https://evil.example.com"` + default policy; error names `QUORUM_ALLOWED_BASE_URL_HOSTS` |
| 15 | `validate_accepts_extra_host_via_policy` | env-extension works | policy with `extra_hosts: ["litellm.example.com"]` |
| 16 | `validate_host_match_is_case_insensitive` | `API.OpenAI.COM` matches | mixed-case host |
| 17 | `validate_host_match_is_exact_not_suffix` | suffix-attack blocked | `"https://api.openai.com.attacker.tld"` rejected (no suffix matching) |
| 18 | `validate_rejects_host_with_idn_lookalike` | Punycode `xn--pi-22a.com` not silently equal to `api.openai.com` | feed IDN host; assert reject under default policy |

### 2d. `validate_base_url` — private/loopback (A3)

| # | Name | Intent | Setup |
|---|------|--------|-------|
| 19 | `validate_rejects_localhost_name_by_default` | `localhost` is private | `"http://localhost:8000"` |
| 20 | `validate_rejects_127_0_0_1_by_default` | IPv4 loopback | `"http://127.0.0.1"` |
| 21 | `validate_rejects_127_0_0_1_alt_form` | `127.1` / `0177.0.0.1` parsed by `Url`; verify behavior is documented (reject) | `"http://127.1"` |
| 22 | `validate_rejects_rfc1918_10` | `10.0.0.0/8` | `"http://10.0.0.5"` |
| 23 | `validate_rejects_rfc1918_172_16` | `172.16.0.0/12` boundary | `"http://172.16.0.1"` and `"http://172.31.255.254"` |
| 24 | `validate_rejects_rfc1918_192_168` | `192.168.0.0/16` | `"http://192.168.1.1"` |
| 25 | `validate_rejects_link_local_169_254` | AWS metadata path | `"http://169.254.169.254/latest/meta-data/"` (the canonical SSRF target) |
| 26 | `validate_rejects_ipv6_loopback_compressed` | `::1` | `"http://[::1]"` |
| 27 | `validate_rejects_ipv6_loopback_full` | full form | `"http://[0:0:0:0:0:0:0:1]"` |
| 28 | `validate_rejects_ipv4_mapped_ipv6_loopback` | `::ffff:127.0.0.1` | `"http://[::ffff:127.0.0.1]"` (must canonicalize and check IPv4 portion) |
| 29 | `validate_rejects_ipv6_link_local` | `fe80::/10` | `"http://[fe80::1]"` |
| 30 | `validate_rejects_ipv6_unique_local` | `fc00::/7` | `"http://[fc00::1]"` |
| 31 | `validate_accepts_loopback_with_allow_private` | Ollama escape hatch | `allow_private=true`; `"http://localhost:11434"` OK |
| 32 | `validate_accepts_127_0_0_1_with_allow_private` | IPv4 loopback OK | same flag |
| 33 | `validate_accepts_public_ip_under_default_policy` | sanity: 8.8.8.8 not blocked by privacy check (would still need allowlist or extra_hosts) | use a policy with `extra_hosts=["8.8.8.8"]` to isolate |

### 2e. `validate_base_url` — `QUORUM_UNSAFE_BASE_URL` total bypass (A4)

| # | Name | Intent | Setup |
|---|------|--------|-------|
| 34 | `validate_unsafe_bypass_allows_arbitrary_public_host` | bypass works | `unsafe=true`, `"https://evil.example.com"` OK |
| 35 | `validate_unsafe_bypass_allows_loopback` | bypass works for private too | `unsafe=true`, `"http://127.0.0.1"` OK |
| 36 | `validate_unsafe_bypass_does_not_skip_scheme_check` | `file://` still fails | `unsafe=true`, `"file:///etc/passwd"` errors |
| 37 | `validate_unsafe_bypass_does_not_skip_userinfo_check` | covered by #9 — ensure ordering |

### 2f. `BaseUrlPolicy::from_env` (env-var parser)

| # | Name | Intent | Setup |
|---|------|--------|-------|
| 38 | `from_env_defaults_when_unset` | no env = strict | clear all three vars |
| 39 | `from_env_parses_extra_hosts_csv` | comma split, trim whitespace | `QUORUM_ALLOWED_BASE_URL_HOSTS=" a.com , b.com,c.com "` |
| 40 | `from_env_extra_hosts_empty_string_yields_no_extras` | empty != one empty entry | `QUORUM_ALLOWED_BASE_URL_HOSTS=""` |
| 41 | `from_env_extra_hosts_whitespace_only` | all-whitespace tokens dropped | `" , ,  "` |
| 42 | `from_env_extra_hosts_lowercased` | normalization done at parse time | `"API.Example.COM"` -> stored as `"api.example.com"` |
| 43 | `from_env_allow_private_only_accepts_one` | strict truthy | `"1"` -> true; `"0"`, `"true"`, `"yes"`, `""` -> false. Document choice. |
| 44 | `from_env_unsafe_only_accepts_one` | strict truthy parity | same matrix |
| 45 | `from_env_is_case_sensitive_for_truthy_value` | `"1"` != `"TRUE"` | document strictness |

Use a serialized env-mutex helper (the codebase already has patterns for this; if not, add a `static ENV_LOCK: Mutex<()>` local to the test module) since env mutation is process-global.

### 2g. `sanitize_error_body` (A5)

| # | Name | Intent | Setup |
|---|------|--------|-------|
| 46 | `sanitize_redacts_bearer_token` | `"Bearer sk-abc123"` -> contains `[REDACTED]`, not `sk-abc123` | |
| 47 | `sanitize_redacts_sk_token_inline_json` | `{"error":"invalid key sk-proj-abcdef..."}` redacted | |
| 48 | `sanitize_redacts_sk_ant_anthropic_token` | `sk-ant-...` redacted | distinct prefix coverage |
| 49 | `sanitize_redacts_authorization_header_dump` | `"authorization: Bearer sk-..."` (lowercase) redacted | case-insensitive match |
| 50 | `sanitize_redacts_api_key_header_dump` | `"x-api-key: abcd1234..."` redacted | header form |
| 51 | `sanitize_truncates_to_200_chars_after_redaction` | redact-then-truncate, not the other way | input: 50 chars junk + `Bearer sk-LONGSECRETXXX...` + 500 chars junk; assert no leak even when truncation cut would have spared it |
| 52 | `sanitize_passes_through_clean_error_body` | no false positives on normal errors | `"rate limit exceeded"` returns identical (≤200) string |
| 53 | `sanitize_handles_empty_string` | empty in -> empty out | |
| 54 | `sanitize_redacts_multiple_tokens_in_one_body` | both Bearer and `sk-` in same string | both redacted |
| 55 | `sanitize_does_not_redact_sk_inside_unrelated_word` | avoid redacting `"task-"` or `"sketch"` — anchor on `\bsk-[a-zA-Z0-9_\-]{8,}\b` | regression guard |

### 2h. Integration at POST sites (A6)

These verify the wiring without making real HTTP calls; use `wiremock` if already a dev-dep, otherwise spin a one-shot `tokio::net::TcpListener` that writes a 401 with a Bearer-laced body and read the resulting `anyhow::Error` string.

| # | Name | Intent |
|---|------|--------|
| 56 | `chat_completion_error_body_is_sanitized` | 401 with `Bearer sk-LEAK...` -> error string contains `[REDACTED]`, not `sk-LEAK` |
| 57 | `responses_api_error_body_is_sanitized` | same for `/responses` path |
| 58 | `chat_with_tools_error_body_is_sanitized` | same for tools path |

If `wiremock` adds dependency cost we don't want, fold these into a single `errors_at_all_three_post_sites_are_sanitized` parameterized test that invokes a test-only helper exposing the sanitize call (and keep #56-58 as a TODO documented in the plan).

### 2i. `OpenAiClient::new` integration (A7, A8)

| # | Name | Intent |
|---|------|--------|
| 59 | `new_default_rejects_evil_host` | with default policy, `"https://evil.com"` returns error mentioning `QUORUM_ALLOWED_BASE_URL_HOSTS` |
| 60 | `new_default_rejects_localhost` | regression: existing `new_accepts_http_and_https_urls` test must be **updated** — `http://localhost:8000` should now error with default env. Either update the test to set `QUORUM_ALLOW_PRIVATE_BASE_URL=1` for that line or replace it with an allowlisted host |
| 61 | `new_with_unsafe_bypass_env_accepts_arbitrary_host` | `QUORUM_UNSAFE_BASE_URL=1` set in test; `"https://evil.com"` accepted |
| 62 | `new_with_allow_private_env_accepts_localhost` | `QUORUM_ALLOW_PRIVATE_BASE_URL=1`; `"http://localhost:11434"` accepted |
| 63 | `new_userinfo_rejected_under_unsafe_bypass` | belt & suspenders — A1 not skipped by A4 at the integration boundary too |

## 3. Specifically-Asked Coverage

- **IPv6 forms**: covered by #26 (`::1`), #27 (full `0:0:0:0:0:0:0:1`), #28 (IPv4-mapped `::ffff:127.0.0.1`), #29 (link-local), #30 (unique-local). Yes — needed; `Url::host()` returns `Host::Ipv6` for these and the natural `is_loopback()` check on `Ipv6Addr` does NOT cover IPv4-mapped without explicit canonicalization. #28 is the highest-value of the three.
- **Percent-encoded user-info**: covered by #10. Necessary because `url::Url::parse` decodes `%40` (`@`) inside the userinfo segment but `username()` returns it as still-encoded text; we need to assert detection works regardless of which form attacker uses. Worth a focused test.
- **Hostname normalization (Punycode/IDN)**: covered by #18. Necessary — `url::Url` lower-cases ASCII hosts but does NOT auto-Punycode-decode. Picking a strict policy ("compare hosts as-stored, ASCII-lowercase, no IDN unification") and testing it is enough; full IDN mapping is out of scope.
- **`from_env` edge cases**: covered by #38-#45. Strict-`"1"` truthy is recommended (matches existing `QUORUM_TRACE` convention). Document in code comments.

## 4. Edge Cases Worth Adding (Not in the Original ~14)

- E1. URL with port that is an allowlisted host (`https://api.openai.com:8443/v1`) — should pass (#11 variant).
- E2. URL with `userinfo` that decodes to empty (`https://@host`) — `url` may treat as no userinfo; document behavior. Add as #6b regression.
- E3. Trailing-dot FQDN: `https://api.openai.com./v1` — many hosts treat as equivalent; we should NOT, to keep allowlist comparisons exact. Add reject test.
- E4. IPv6 with zone ID: `http://[fe80::1%25eth0]` — already private, but verify parser doesn't crash.
- E5. `sanitize_error_body` with multi-byte UTF-8 close to the 200-char boundary — make sure char-count truncation (not byte-count) is used and we don't slice mid-codepoint. The existing code uses `chars().take(200)` so this is preserved; add a test as a regression guard.
- E6. Concurrent calls to `validate_base_url` from many threads — pure-fn property guarantee; add a smoke test that spawns 32 threads and asserts no panics (cheap).

## 5. Out of Scope (Per Plan)

- Wildcard / suffix matching in `extra_hosts` (`*.example.com`) — explicitly out; #17 enforces "no suffix matching."
- Retry policy and timeout layering — tracked separately as task #18 / "issue #119 retry+timeout layering."
- CLI flag plumbing for any of the env vars — env-only configuration this round.
- DNS-time SSRF protection (resolving allowlisted host to `127.0.0.1` and re-checking the resolved IP). Documented limitation; the allowlist is a name-based defense.
- Full IDN/Unicode host equivalence rules (UTS #46). We treat the host string as opaque-after-ASCII-lowercase.
- Live integration tests that hit `api.openai.com` or LiteLLM — unit tests only.

## 6. Style Notes for Implementation

- Match the existing `#[cfg(test)] mod tests` block at bottom of `src/llm_client.rs`.
- Reference issue numbers in the test doc comments (project convention — see `new_rejects_url_without_scheme` pointing at #59, `new_preserves_configured_timeout_on_built_client` at #66).
- For env-var tests, serialize via a single test-module `Mutex` to avoid cross-test pollution.
- Pure-fn tests (`validate_*`, `sanitize_*`) need no `tokio::test`.
- No emojis. Use plain ASCII in messages and comments.
