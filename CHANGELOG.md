# Changelog

## [Unreleased]

_No unreleased changes._

## [0.18.1] - 2026-04-30

### Security

- **`validate_base_url` http-rejection now scoped to actual host classification (#167).** The v0.18.0 #147 fix used `if scheme == "http" && !policy.allow_private_ips`, which incorrectly assumed reaching that check with `allow_private_ips=true` implied a private host. In fact public allowlisted hosts (`api.openai.com`, `api.anthropic.com`, etc.) also reached the check, so setting `QUORUM_ALLOW_PRIVATE_BASE_URL=1` for an Ollama deployment inadvertently permitted plaintext `http://` to public destinations — leaking the API key in cleartext on every request. Fix: track `host_is_private` during the existing host-classification arms (`Ipv4`, `Ipv6`, `Domain`); the scheme check now keys on `!host_is_private` instead of `!policy.allow_private_ips`. The flag now correctly only gates the *private-host* rejection, not the HTTPS requirement for public hosts. Surfaced by quorum's own self-review on v0.18.0 ~2 hours after #147 shipped — the post-#118 prompt carve-out elevated this to HIGH instead of demoting under the old "defensive-programming" rule (self-improvement loop validation). Two regression tests: `validate_base_url_rejects_http_for_public_host_even_with_allow_private_ips` (the bug, now closed); `validate_base_url_allows_http_for_private_ip_with_allow_private_ips` (regression-lock for the legitimate Ollama / on-prem case).

## [0.18.0] - 2026-04-30

### Feedback

- **FpKind enum + per-kind calibrator behavior (#123 Layer 1).** `Verdict::Fp` entries can now be tagged with one of five kinds — `Hallucination` (default), `CompensatingControl { reference }`, `TrustModelAssumption`, `PatternOvergeneralization { discriminator_hint }`, `OutOfScope { tracked_in }` — letting the calibrator apply different decay/scope rules per FP type. `TrustModelAssumption` FPs decay 3× faster (40d half-life vs. 120d default) so cross-model corroboration TPs can naturally outweigh stale trust-model judgments. `OutOfScope` FPs are excluded from the precedent pool entirely (they represent real defects tracked elsewhere, not suppression signal). `PatternOvergeneralization` FPs surface their `discriminator_hint` in the few-shot prompt so the LLM can re-flag the pattern in legitimate cases. The 2302 existing `~/.quorum/feedback.jsonl` entries deserialize as `fp_kind: None` (Hallucination semantics) — zero migration required. CLI: `quorum feedback --fp-kind {hallucination|trust-model|compensating-control|pattern-overgeneralization|out-of-scope}` plus the corresponding `--fp-reference` / `--fp-discriminator` / `--fp-tracked-in` suffix flags. MCP: parallel `fpKind` field on the `feedback` tool with JSON-typed variant payloads. New `fp_kind_utilization_rate` field on `TelemetryEntry` records the fraction of FP entries that carry a `fp_kind` discriminator — informs Layer 3 prioritization (low utilization → LLM-driven backfill becomes critical; high utilization → Layer 1 alone may suffice). Layer 2 (auto-stale on cross-model TP corroboration, `feedback doctor` reference validation) and Layer 3 (LLM-driven reclassification of existing entries) are tracked separately.

### Reliability

- **MCP handlers no longer block the tokio executor on LLM calls (#129).** Pre-fix `handle_chat`, `handle_debug`, and `handle_testgen` in `src/mcp/handler.rs` were synchronous and called `reqwest::blocking`-backed `LlmReviewer::review()` inline inside the async dispatcher. A single in-flight chat/debug/testgen tool call parked the tokio worker for the full LLM round-trip — up to the 300s `QUORUM_HTTP_TIMEOUT` from #117 — stalling all other MCP requests. Fix: convert each handler to `async fn`, flip `QuorumHandler::llm_reviewer` from `Box<dyn LlmReviewer>` to `Arc<dyn LlmReviewer>`, wrap the `.review()` call in `tokio::task::spawn_blocking` so the blocking HTTP round-trip moves to the dedicated blocking thread pool. Concurrency regression test uses `std::sync::Barrier` + `mpsc::recv_timeout` for a deterministic deadlock-or-instant outcome (no timing thresholds). Architectural follow-ups filed as #131 (migrate `LlmReviewer` trait to `async fn review` — eliminates `spawn_blocking` workaround and closes #81 as a side effect) and #132 (DRY the three handler bodies into a shared helper). PAL pre-implementation review caught a Send/Sync subtlety and recommended the deterministic Barrier primitive over an earlier throughput-based test.

- **`send_with_retry` narrowed transient classification (#146).** Pre-fix the helper treated every `reqwest::Error::is_request() == true` as transient, retrying up to 3× on deterministic protocol errors (URL builder failures, decode errors) that were never going to succeed on retry. Real-world impact: a 4xx-with-decode-failure became a 3-retry stall before the eventual error surfaced to the caller. New `is_transient_transport_error` helper retries only `is_timeout()` + `is_connect()`. RED test used wiremock + `.json::<Value>()` on garbage to synthesize a real `is_decode()` / `is_request()` reqwest::Error.

- **`send_with_retry` checks `overall_deadline` at top of retry loop (#157).** Pre-fix the deadline was checked only before the inter-attempt sleep, not before launching a new request. A long first attempt (e.g. 150ms response delay against a 100ms deadline) could fire 3 more requests after the deadline. New top-of-loop guard `if attempt > 0 && Instant::now() >= overall_deadline { return last_err; }` — `attempt > 0` no-ops the first iteration so the budget bounds the retry tail, not the initial call.

- **`telemetry::load_all` streams line-by-line with structured parse errors (#138, #139).** Pre-fix `load_all` read the entire file into a `String` (pathological for >100MB telemetry stores — Quorum's grows ~1MB per 100 reviews) and silently dropped malformed JSONL rows by counting them in `LoadStats.skipped`. New `load_all_with_stats` streams via `BufReader::lines`, returning a structured `ParseError { line, content, kind }` so daemon-mode operators can surface specific failures via `tracing::warn!`. Also bounds per-line + total error allocation, applies the size cap to JSONL payload (not raw buffer), accepts a JSONL line of exactly `MAX_JSONL_LINE_BYTES` at EOF, and rejects invalid UTF-8 instead of lossy-rewriting it. Self-review caught 4 iterative hardenings during implementation.

- **Pipeline `--diff-file` matching uses repo-relative path equality (#137).** Pre-fix `diff_path_matches_review` used `path.ends_with(other_path)` on string-typed paths, which incorrectly matched `src/foo.rs` against any path ending in `foo.rs` (e.g. `unrelated/dir/foo.rs`). Combined with multi-file reviews this caused diff filters to bleed across modules. Fix: hoist `diff_path` canonicalization out of the per-file filter loop (do it once) and compare via component equality rather than string suffix.

- **OpenAiClient retries transient upstream failures + bounded-tail wall-clock (#117).** Pre-fix `chat_completion`, `responses_api`, and `chat_with_tools` each called `.send().await?` with a single attempt and a coarse 300s total timeout (no per-read timeout). A single transient `429`/`5xx` from OpenAI/LiteLLM aborted entire batch reviews; with `--parallel 4` this multiplied — N concurrent calls with N independent failure odds. Cross-model PAL corroboration on 2026-04-28 (claude-opus-4.5 MEDIUM, gpt-5.4 MEDIUM on retry; gpt-5.4 unique on the timeout layering). Fix: new `send_with_retry` helper retries `429`/`500`/`502`/`503`/`504` up to 3 times with exponential backoff (500ms→1s→2s, capped at 30s, with ±20% jitter when no `Retry-After` header is supplied) and honors server-supplied `Retry-After` (seconds + HTTP-date forms) when the wait fits the remaining budget. Bounded by a separate `OVERALL_RETRY_DEADLINE_SECS=600s` budget so the retry tail can't stack into 20-minute worst cases — the per-call 300s timeout still covers single happy-path requests, including reasoning models that take 4-8 minutes. Only truly transient transport errors (`is_timeout`/`is_connect`/`is_request`) are retried; decode/builder errors bail immediately. New `read_timeout` (default 120s) layered on top of the existing 300s total — protects against trickle-after-first-byte without affecting healthy "think-then-burst" responses. Both timeouts configurable via `QUORUM_HTTP_TIMEOUT` / `QUORUM_HTTP_READ_TIMEOUT` env vars. PAL/gpt-5.4 pre-implementation review flagged 4 critical + 3 important issues (separate retry-budget vs per-call timeout, off-by-one in `retry_after_fits_budget`, transport-error classification too broad, env-driven test override) — all addressed before TDD started. Antipatterns-expert review caught the wall-clock-flake risk in the HTTP-date test (widened to a 60-120s envelope) and pushed to deterministic seeds for the jitter test instead of stochastic sampling.

### Security

- **OpenAiClient base_url hardened against SSRF + credential exfiltration (#119).** Pre-fix `OpenAiClient::new` accepted any caller-controlled http(s) URL and sent `Authorization: Bearer <api_key>` to it on every request. Concrete attacks: typo'd or shell-injected `QUORUM_BASE_URL` exfiltrated the key to attacker-chosen hosts; `https://user:pass@host/v1` flowed embedded creds on every call; `http://169.254.169.254/v1` exfiltrated to cloud-IMDS endpoints. Cross-model corroboration on 2026-04-28 (PAL/claude-opus-4.5 + PAL/gpt-5.4 both flagged HIGH/MEDIUM). Fix: `validate_base_url` enforces three layers — always-on rejection of embedded credentials (no opt-out, applies even with bypass), default-on allowlist of public OAI-compatible hosts (`api.openai.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`) extensible additively via `QUORUM_ALLOWED_BASE_URL_HOSTS`, and default-on rejection of loopback/RFC1918/link-local IP literals + `localhost` DNS-name family with `QUORUM_ALLOW_PRIVATE_BASE_URL=1` opt-in for Ollama / on-prem. The opt-in alone is sufficient for private hosts (no second env-var dance). Total bypass available via `QUORUM_UNSAFE_BASE_URL=1` for development. Errors are actionable — every rejection points at the exact env var to set. Defense is layered, not toggle-based: embedded-credential rejection is upstream of the bypass branch. Hand-rolled IPv6 bitmasks for `fc00::/7` and `fe80::/10` validated by boundary tests (`fbff::`, `fe7f::`, `fec0::`); IPv4-mapped IPv6 (`::ffff:127.0.0.1`) extracted and run through IPv4 rules. **Known limitation:** validation is path-bound; DNS-resolved private IPs aren't blocked (filed as #126 — requires a custom reqwest resolver).

- **MCP tool input structs reject unknown JSON fields (#134).** All 6 MCP tool input structs (`ReviewTool`, `FeedbackTool`, `ChatTool`, `DebugTool`, `TestgenTool`, `CatalogTool`) gain `#[serde(deny_unknown_fields)]`. Pre-fix a typo or extra field in a client request was silently ignored — `{"file": "x.rs", "filee": "..."}` parsed as if `filee` didn't exist, hiding caller mistakes and weakening the wire-protocol contract. Now a malformed call surfaces a clear deserialization error.

- **CLI `quorum context add --name` validated at parse time (#135).** `--name` is now constrained via clap `value_parser` regex `^[a-zA-Z0-9_-]+$` so path-traversal characters (`/`, `..`, NUL, etc.) are rejected before any filesystem work happens. Defense-in-depth `assert_safe_relative_path` in `run_add` catches any post-parse mutation. Pre-fix `--name '../../etc/passwd'` would have rooted a context source at the traversed path under `~/.quorum/sources/`.

- **CLI `quorum context query --k` capped at 1..=100 (#136).** Pre-fix `--k 0` silently returned an empty result set (mathematically valid but actionable misconfiguration), and `--k 1000000` allocated unbounded result buffers. Now constrained via clap `value_parser!(usize).range(1..=100)`.

- **Feedback inbox TOCTOU window narrowed on non-Unix (#133).** Follow-up to the inbox-hardening entry below. On non-Unix platforms `read_inbox_file` had no `O_NOFOLLOW`-equivalent and relied on the pre-claim `symlink_metadata` check alone, leaving a TOCTOU window between classify and read. New code re-stat-validates the file *after* opening on non-Unix so the race is bounded by the syscall pair — same defense pattern as ast-grep `read_rule_file`.

- **`sanitize_error_body` redacts JSON token / secret / access_token fields (#144).** Extension to the existing `LazyLock<Regex>` in `sanitize_error_body` covering generic `"token": "..."`, `"secret": "..."`, and `"access_token": "..."` JSON shapes that gateways may echo back on validation errors. The bare `Authorization:` header echo and `x-api-key: <value>` shapes were already covered by the existing `bearer\s+...` and `api[_-]?key...` patterns; corresponding RED tests kept as regression locks against future regex refactors.

- **`validate_base_url` rejects plaintext `http://` for public providers (#147).** Pre-fix a base URL of `http://api.openai.com/v1` (or any host on the public allowlist) would silently send `Authorization: Bearer <api_key>` and the entire request body — including prompts that contain user source code — over the wire in cleartext. The allowlist + IP-block layers from #119 protected against SSRF and DNS tricks but not scheme-level mistakes. New check fires after host classification (so private-IP rejections keep their actionable wording) and refuses `http` unless `QUORUM_ALLOW_PRIVATE_BASE_URL=1` is set (Ollama / on-prem already implicitly accepting plaintext on a LAN) or `QUORUM_UNSAFE_BASE_URL=1` (full bypass).

- **`validate_base_url` parse-error path redacts embedded credentials (#156).** Pre-fix a malformed URL like `https://user:secret@host:abc/v1` (invalid port → parse fails) would echo the embedded `user:secret` in the parse-error string, propagating credentials to terminal output, daemon logs, and telemetry — even though the legitimate-parse path was already redacting via `redact_userinfo_for_error`. Fix: introduce `redact_userinfo_for_error` (string-level walk, since `url::Url` is unavailable post parse-fail) and apply it in the error-message construction.

- **Error-body sanitization on LLM POST failures + bounded read (#119).** Pre-fix `chat_completion`, `responses_api`, and `chat_with_tools` echoed up to 200 chars of the response body into `anyhow::bail!` messages, propagating to terminal output, daemon logs, and telemetry. Some OAI-compatible gateways echo back request headers (Bearer token, including JWTs) or body (prompt + source code) on validation errors; a multi-megabyte error page would also OOM the process before the 200-char truncation kicked in. Fix: `read_capped_error_body` reads via `Response::chunk()` with a 64 KiB hard cap and surfaces transport errors via `tracing::warn!` (no silent discard). `sanitize_error_body` then scrubs bearer tokens (now including JWT `header.payload.signature` form via expanded charset), OpenAI key shapes (`sk-proj-`, `sk-svcacct-`, `sk-org-`, `sk-live-`, `sk-test-`), Anthropic shapes (`sk-ant-api03-`), and `api_key` / `api-key` / `api key` field echoes (case-insensitive, space/underscore/hyphen separators). Truncates to 200 codepoints (multi-byte UTF-8 honored — emoji-padding regression test pins the codepoints-not-bytes contract). Quorum self-review of the original implementation caught two real defects in-branch (JWT charset, unbounded `resp.text()`) plus a UX bug (`allow_private_ips` requiring a second env-var); all fixed before merge.

### Fixed

- **External-feedback inbox hardened against symlink-redirect, FIFO-hang, and unbounded-allocation attacks.** Pre-fix `FeedbackStore::drain_inbox` filtered candidates with `!p.is_dir()` (which follows symlinks) and read each claimed file with `std::fs::read_to_string` (which is unbounded and follows symlinks). Concrete attacks: `~/.quorum/inbox/evil.jsonl -> /etc/passwd` was renamed into `processing/` and read; a FIFO at the same path blocked the drain loop indefinitely (kills daemon mode); a symlink to `/dev/zero` or a 10 GiB file OOMed the process. Surfaced during the post-#118/#120 5-file panel review on 2026-04-29 — the same defect class as the just-shipped #120 fix on `src/ast_grep.rs`, now in the feedback ingestion path. Fix: layered guards mirroring #120 — `classify_inbox_entry` rejects symlinks/FIFOs/sockets/oversized files at iteration time via `symlink_metadata` (rejection happens BEFORE the claim-rename, so rejected files stay in `inbox/` for operator inspection — never silently flow into `processing/` or `processed/`); `read_inbox_file` opens with `O_NOFOLLOW | O_NONBLOCK` as defense-in-depth against TOCTOU between classify and read, validates regular-file via fstat on the handle, caps at 1 MiB, and reads via `.take(MAX+1)` to defend against inodes that lie about size. Quorum self-review of the initial implementation caught a non-Unix `File::open` arm that weakened the security model relative to Unix; collapsed into the inline-`#[cfg(unix)]` pattern matching `read_rule_file` in `src/ast_grep.rs`.

- **User rule loader hardened against symlink-follow + unbounded YAML (#120).** Pre-fix `load_rules` used `is_dir()` and `read_to_string()` which both follow symlinks, and had no size cap on the YAML read. Concrete attacks: a symlink at `~/.quorum/rules/python -> /etc/ssh/` exfiltrated arbitrary file contents; a multi-MB YAML in the user-rules tree could exhaust memory or hang on `/dev/zero`. Cross-model PAL corroboration on 2026-04-28 (gpt-5.4 HIGH, claude-opus-4.5 MEDIUM) overturned a 2026-04-14 trust-model FP that had previously suppressed this class. Fix: three layered guards — `symlink_metadata` gate on the rules-root itself (closes the case where `~/.quorum/rules` is the symlink), `symlink_metadata` gate on per-language directories, and a `read_rule_file` helper that opens with `O_NOFOLLOW` + validates via the opened handle (eliminates the TOCTOU window where an attacker could swap a checked regular file for a symlink between `symlink_metadata` and `read_to_string`). Plus a 1 MiB `MAX_RULE_FILE_BYTES` size cap validated on the opened handle, with a defensive `.take(MAX+1)` read bound that holds even if the inode size lies (proc/sysfs/network FS). Largest bundled rule today is ~1.6 KiB so the cap has 600× headroom. Codex review of the implementation plan flagged the TOCTOU and the unguarded top-level rules-root; both folded into the fix before TDD started.

- **ast-grep duplicate rule IDs no longer silently shadow (#145).** Pre-fix the `load_rules` loader could load multiple rules with the same `id` from bundled+user directories, causing `scan_file` to double-fire on every match (one finding per duplicate rule) and leaving the user-vs-bundled override semantics ambiguous. New code tracks a `HashSet<(SupportLang, String)>` of seen `(language, id)` pairs as rules are loaded; on collision the second copy is dropped and a structured `tracing::warn!` surfaces the shadow attempt. Bundled is scanned before user, so a bundled id wins. The dedup key is `(language, id)` rather than bare id because bundled rules legitimately reuse ids across grammars (e.g. `bind-all-interfaces` in both Python and JavaScript) — a bare-id key would erroneously drop the second-language copy.

- **#155 (read_rule_file post-read length check) closed as not reproducible.** Reported by PAL+CodeRabbit cross-corroboration during the 3-way self-review on 2026-04-30. RED test passed without code change: the metadata pre-check at `src/ast_grep.rs:59` (`if meta.len() > MAX_RULE_FILE_BYTES`) already rejects oversized files before the read, and the `take(MAX+1)` cap holds even if metadata lies (proc / sysfs / network FS). Recorded as `compensating_control` FP from both external agents (`--fp-reference src/ast_grep.rs:59`) so the calibrator surfaces the metadata gate as precedent on similar findings.

- **Review prompt structurally suppressed boundary-security findings (#118).** Pre-fix the system prompt's down-classification rules 3 ("theoretically possible but no realistic trigger") and 4 ("Maintainability, naming, complexity, and defensive-programming concerns belong in low or info") silently demoted real missing-safety-check-at-trust-boundary findings (no retry on transient failures, unbounded allocation from external input, symlink follow, SSRF + credential exfil) to LOW where the default review threshold dropped them. Confirmed structurally: same-model PAL/gpt-5.4 vs Quorum/gpt-5.4 on `src/llm_client.rs` and `src/ast_grep.rs` produced ZERO overlap; PAL surfaced 8+ TPs that Quorum's prompt rules suppressed. Fix: injected a "Precedence rule" before the down-classification list that exempts findings about missing validation, missing safety checks, or missing resource bounds at trust/external-input boundaries (network input, filesystem, response/payload, auth/credential) from rules 3 and 4 — those findings now classify on actual impact and reachable input surface per the priority list. Postpositive `EXCEPTION:` clauses were rejected per gpt-5.4 + claude-opus-4.5 critique (frontier models compress them away under the surrounding "never high" anchor) and per OpenAI Cookbook GPT-4.1 prompting guidance ("instruction closest to the end" wins). Also narrowed rule 4 to "Purely-stylistic concerns (naming, formatting, complexity-for-its-own-sake)" since the previous "Maintainability ... defensive-programming" framing conflated style with missing invariants. Priority item 4 extended with resource-bounds language at external-input boundaries (allocation, request count, file size).

### Tests

- New `drain_inbox_*` hardening tests: `drain_inbox_skips_symlinked_inbox_file` (symlink to outside file rejected pre-rename, file remains in `inbox/`), `drain_inbox_rejects_oversized_file` (2 MiB > 1 MiB cap), `drain_inbox_rejects_non_regular_file` (Unix socket via `UnixListener::bind`), `drain_inbox_rejects_fifo_file` (FIFO via `libc::mkfifo` — headline threat against daemon-mode drain loop), `drain_inbox_accepts_file_at_size_cap` + `drain_inbox_rejects_file_one_byte_over_cap` (off-by-one boundary defense), `drain_inbox_happy_path_unaffected_by_nofollow_helper` (regression guard distinct from existing `drain_inbox_valid_file_appends_and_moves`). testing-antipatterns-expert flagged the original substring assertions as Anti-Pattern #5 (implementation coupling); upgraded to assert on the structured `"rejected: ..."` message prefix (stable contract) rather than free-form error text.

- New: `system_prompt_carves_out_trust_boundary_findings_via_precedence_rule` (Layer A — single static-content assertion that both `Precedence rule` and `trust or external-input boundary` anchor phrases co-occur in `system_prompt()`; per-keyword tests rejected as change-detector tautology). `high_boundary_finding_survives_calibrator_at_high` (Layer B regression guard — synthetic HIGH SSRF finding round-trips through `parse_llm_response` + `calibrator::calibrate` at HIGH severity with empty feedback; guards against future calibrator changes that would inadvertently re-suppress the class). Layer C (live LLM fixture review) deferred to issue #121 as a separate placeholder.

## [0.17.4] - 2026-04-25

### Fixed

- **`pipeline::acquire_llm_permit` cross-runtime deadlock (#81).** Pre-fix the helper synchronously branched on the calling Tokio runtime flavor: `block_in_place` on multi-thread, `std::thread::scope` + a fresh current-thread runtime + `join()` on current-thread. The current-thread branch deadlocked when the permit holder was another task on the *same* runtime — `join()` blocked the only worker, the holder never ran to release, and the spawned helper runtime awaited forever. Production hit only the multi-thread path (`#[tokio::main]` defaults), so the bug surfaced primarily in `#[tokio::test]` and embedders. Post-fix `acquire_llm_permit` is `async fn`: `sem.as_ref()?.clone().acquire_owned().await.ok()`. No runtime detection, no thread spawning, no blocking. Awaiting cooperatively yields to the runtime that owns the holder; deadlock vanishes by construction.

### Changed (public API)

- `pipeline::review_file`, `review_source`, `review_file_llm_only` are now `pub async fn`. Sync embedders must drive the future via a runtime (e.g., `Runtime::new()?.block_on(review_source(...))`). All in-tree call sites updated: CLI serial path `.await`s directly; CLI parallel path keeps the `spawn_blocking` shell so CPU-heavy parsing stays off runtime workers, with `Handle::current().block_on(async { ... })` inside the closure to bridge sync-context blocking-pool threads into the async pipeline; MCP `handle_review` is now `async fn`; HTTP daemon `.await`s.

### Tests

- New: `acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread` (regression for #81 — actively exercises the formerly-deadlocking flavor with `tokio::sync::Notify` deterministic handshake), `..._on_multi_thread` (defensive matrix coverage), `..._cancellation_does_not_leak`, `..._returns_none_when_semaphore_is_closed` (mutation-killer for `?` and `.ok()`), `..._returns_some_when_permit_available_on_current_thread`, `..._on_multi_thread`, `..._returns_future_outside_tokio_runtime`. Bulk-converted ~15 `#[test]` sites to `#[tokio::test(flavor = "multi_thread", worker_threads = 1)]` to preserve sequential semantics.

## [0.17.3] - 2026-04-25

### Fixed

- **MCP `handle_review` wrote feedback to a different file than it read (#93).** Pre-fix, the handler loaded precedents from `self.feedback_store` (the path injected at construction) but built `PipelineConfig.feedback_store` from `dirs_path()/feedback.jsonl` regardless. Tests (and any alternate prod constructor) silently split reads from one DB and pipeline-side writes (post-fix verdicts, auto-calibrate recordings) to a different file. Added `pub fn path(&self) -> &Path` to `FeedbackStore` and extracted `pub(crate) build_pipeline_config_for_review(&self, params: &ReviewTool)` from `handle_review`'s inline assembly, so the helper is unit-testable independently of running a full review.
- **`drain_inbox` silently swallowed `read_dir` iteration errors (#103).** The listing site used `filter_map(|e| e.ok())`, dropping every per-entry I/O / permission error. Combined with claim-then-ingest, a single permission-denied file could strand all subsequent ingestion of that file forever with no observability hook. Extracted `pub(crate) drain_inbox_entries<I>(impl Iterator<Item = io::Result<PathBuf>>) -> (Vec<PathBuf>, Vec<DrainError>)` so production callers fold per-entry Errs into `report.errors`. Helper takes `Iterator<io::Result<PathBuf>>` (not `DirEntry`, which has a private constructor) so tests can inject synthetic `Err`. Size-warning site at the bottom of `drain_inbox` deliberately keeps `filter_map(.ok())` — best-effort cosmetic counter, documented in a code comment justifying the asymmetry.
- **MCP `ReviewTool.focus` field was a documented no-op (#104).** Schema declared `focus: Option<String>` but `handle_review` dropped it on the floor. Threaded through: added `focus` to `ReviewRequest` and `PipelineConfig`; `build_pipeline_config_for_review` copies `params.focus.clone()`; both `ReviewRequest` construction sites in `pipeline.rs` propagate `pipeline_config.focus.clone()`; `build_review_prompt` renders a `<focus_areas>` sandbox-tag block AFTER `</untrusted_code>` (cache-prefix stable) via `defang_sandbox_tags`. Empty / whitespace-only focus is byte-identical to None (mirrors the `context_block` pattern).

## [0.17.2] - 2026-04-25

### Fixed

- **`quorum review` accepted zero files (#89).** `ReviewOpts.files` had no `required` constraint, so `quorum review` (no args) parsed successfully and the handler short-circuited with `eprintln!("Error: ..."); return 3` — a usage error masquerading as a tool error. Lifted the rule into clap via `#[arg(required = true, num_args = 1..)]` so users get the standard "required arguments were not provided" message + `--help` hint with the conventional exit-2 status. Removed the now-dead handler-level guard.
- **`run_context` swallowed non-`BrokenPipe` stdout write errors (#84).** Previous `let _ = handle.write_all(...)` / `let _ = handle.flush()` silenced `BrokenPipe` (correct, the user closed `| head`) but also `EIO` / `ENOSPC`, so `quorum context list > /full-disk` exited 0 with no output delivered. Extracted `cli_io::write_cmd_output(out, err, cmd) -> i32`: BrokenPipe → silent exit 0; any other error → `error: failed to write output: {e}` on stderr + exit 1. Warnings reach stderr unconditionally (CodeRabbit caught a regression where the helper hid them on the error path). Doctor exit-code propagation (#73) preserved.
- **`--source` + `--all` regression guards (#79).** The conflict was already enforced by `#[arg(conflicts_with = ...)]` on `ContextIndexOpts` / `ContextRefreshOpts`; added 6 regression tests via `Args::try_parse_from` + `ErrorKind::ArgumentConflict` (2 conflict + 4 positive controls covering both `index` and `refresh`) so a future drop of the annotation is caught immediately.

### Dependencies / Crate Metadata

- Declared `rust-version = "1.88"` in `[package]`. The codebase already uses `if let` chains (stabilized in Rust 1.88) in `src/context/cli.rs`, `src/context/extract/markdown.rs`, and now `src/cli_io.rs`. Edition 2024 only requires 1.85, so the actual minimum was undeclared.

## [0.17.1] - 2026-04-25

### Fixed

- **CLI `--verdict` parser mismatch (#90).** Dropped the clap `PossibleValuesParser` so case + whitespace variants like `--verdict TP` or `--verdict ' tp '` normalize through `parse_verdict` (matching its long-documented contract). Previously they were rejected before normalization.
- **MCP `FeedbackTool.verdict` trust boundary (#94).** Replaced the `String` field with a `FeedbackVerdict` enum (`#[serde(rename_all = "snake_case")]`). The MCP JSON-Schema now enumerates the five valid wire strings (`tp`, `fp`, `partial`, `wontfix`, `context_misleading`) instead of advertising "string (any)" — schema-driven clients can now discover the constraint.
- **`rename_or_tolerate_race` ENOENT misclassification (#101).** Only treat `NotFound` from `std::fs::rename` as a benign "another process already claimed this" signal when the source path is confirmed absent. If the source is still present, the `NotFound` came from a missing destination parent dir or similar — propagate so misconfiguration surfaces.
- **`load_all` silent-skip observability (#92).** Added `pub(crate) load_all_with_stats() -> (entries, LoadStats { kept, skipped })` and a `tracing::warn!` event when any line fails to parse. Public `load_all` signature unchanged. Read path now also takes a shared advisory lock to pair with the writer-side exclusive lock — readers can no longer observe a partial mid-append line and silently count it as malformed.
- **Concurrent `record()` corruption defense (#91).** Append now takes an advisory exclusive lock via `fs2::FileExt::lock_exclusive`. POSIX `O_APPEND` atomicity covers single-syscall writes today, but `write_all` can multi-syscall under partial-write conditions and a future buffering refactor could break per-line atomicity. Defense-in-depth at minimal cost.

### Dependencies

- Added `fs2 = "0.4"` (portable POSIX flock + Windows LockFileEx — small, well-known crate; no transitive bloat).

## [0.17.0] - 2026-04-24

### Added
- External-agent feedback ingestion (issue #32). Verdicts from other review agents (pal, third-opinion, gemini, reviewdog, ...) now flow through three paths, all funneling through `FeedbackStore::record_external`:
  - `~/.quorum/inbox/*.jsonl` drained at the top of every `review`/`stats` invocation via claim-then-ingest (atomic rename to `inbox/processing/` before parse, archive to `inbox/processed/` on success)
  - `quorum feedback --from-agent <name> [--agent-model <m>] [--confidence 0..1] [--category <c>]`
  - MCP `feedback` tool with `fromAgent` / `agentModel` / `confidence` fields
- New `Provenance::External` variant with calibrator weight 0.7x. Trust boundary: External may only record `tp` / `fp` / `partial` — `wontfix` and `context_misleading` are rejected at the chokepoint. Confidence is clamped to [0,1] (NaN-safe), agent name is normalized (trim+lowercase).
- Tier breakdown by Provenance shows up under `quorum stats` Feedback Health when any non-Human entry exists, with a per-agent sub-line for External.
- Context7 dependency-based enrichment beyond curated frameworks (issue #29). Parses Cargo.toml, package.json, pyproject.toml + requirements.txt; filters by import_targets; caps at K=5; queries Context7 with curated-or-language-aware queries. 24h TTL cache, negative results too.

### Fixed
- Calibrator: cap External-provenance contribution at `EXTERNAL_WEIGHT_CAP = 1.4` (issue #97). Single misbehaving agent can no longer flood TP/FP verdicts and dominate calibration. Cap is global across agents, applied symmetrically in both calibrate code paths via the new `accumulate_capped` helper.
- `FeedbackStore::record` now creates the feedback file's parent directory before opening (issue #100). Direct callers (tests, daemon, future entry points) no longer hit ENOENT on fresh installs or alternate `QUORUM_HOME`.
- `dep_manifest`: PEP 621 array branch now dedupes; package.json deduplication corrected; complete Poetry sections parsed (PR #86).
- Trust-boundary cleanup across MCP feedback handler, MCP review pipeline, and CLI verdict parsing (issues #59, #61, #65, #66, #67, #68, #69, #71, #72, #73).
- Multiple sandbox-tag and prompt-injection defenses across review surfaces.

## [0.16.0] - 2026-04-22 (feat/context)

### Added
- `quorum context` subcommand: local/offline alternative to Context7 for injecting project-specific symbols and docs into LLM review prompts
  - `init` / `add` / `list` / `index` / `refresh` / `query` / `prune` / `doctor` subcommands
  - Hybrid retrieval: FTS5 BM25 + sqlite-vec cosine, reranked by id/path/recency signals
  - `render` pipeline emits a fenced Markdown block (symbols first, then prose), stable prompt hash for telemetry
  - Per-source on-disk layout at `~/.quorum/sources/<name>/{chunks.jsonl,index.db,state.json}`
  - `doctor` runs 7 structural checks and reports fixable vs non-fixable failures
- Context injector wired into the review pipeline: `quorum review` loads `~/.quorum/sources.toml` automatically and injects the rendered block when `auto_inject = true`
- `context_misleading` feedback verdict + `blamed_chunks` routing: per-chunk injection thresholds raise with each confirmation and seal at N (default 3)
- Review telemetry record gains a `ContextTelemetry` block (retrieved/injected counts, token count, threshold, duration, calibrator suppression count, rendered-prompt sha256)

### Fixed
- `context query` in a fresh process failed with `no such module: vec0` because sqlite-vec's auto-extension hook was only registered inside `IndexBuilder`. `ensure_vec_loaded()` is now called from `run_query` and `db_chunk_count` before the raw `Connection::open*`
- Calibrator gate in the context injector enforces `max(inject_min_score, calibrator_threshold)` to match the documented contract

## [0.3.0] - 2026-03-25

### Added
- TypeScript local analysis: hardcoded secrets, innerHTML/document.write XSS, console.log, any type, non-null assertion
- Context7 integration: auto-fetches framework docs (React, Django, FastAPI, etc.) for LLM prompt enrichment
- Configurable calibration model (`--calibration-model o3`)
- 3 new Python patterns: mutate-while-iterate, exception disclosure, blocking .result() in async
- Secret patterns from detect-secrets: AWS STS, Slack, Stripe, Twilio
- Model comparison benchmark across 7 models

### Fixed
- Secret redaction no longer destroys variable references (`api_key = os.getenv(...)`)
- Context7 project root detection (walks up to find pyproject.toml/package.json)
- Context7 handles plain text responses (not just JSON)

## [0.2.0] - 2026-03-24

### Added
- Auto-calibration: second LLM pass triages findings automatically
- Python local patterns: hardcoded secrets, debug=True, host=0.0.0.0, f-string SQL, mutable defaults
- Test code filtering: .unwrap() in #[cfg(test)] modules suppressed
- Robust JSON parsing: invalid escapes, truncated responses, wrapped objects
- Calibrator with feedback RAG: suppresses FPs, boosts TPs
- HTTP daemon with warm cache + file watcher
- MCP server cache integration
- CLI --daemon mode
- Per-source analytics
- Domain detection (React, Next.js, Django, FastAPI, Flask, Express, Vue, Fastify)

### Fixed
- LLM response parsing: max_tokens bumped to 16384, finish_reason truncation check
- Hydration: overlap-based blast radius, TypeScript import parsing

## [0.1.0] - 2026-03-24

### Added
- Core: canonical Finding format, Config, tree-sitter parser (Rust, Python, TypeScript, TSX)
- Analysis: cyclomatic complexity, insecure patterns (eval, exec, unsafe, unwrap)
- Pipeline: hydration -> LLM review -> local analysis -> merge/dedup -> calibrate -> output
- MCP server: 6 tools (review, chat, debug, testgen, feedback, catalog)
- LLM client: OpenAI-compatible HTTP client with block_in_place
- Output: human format (ANSI), JSON format, exit codes (0/1/2/3)
- Secret redaction: 7 regex patterns, always-on
- Feedback storage: JSONL append, query by verdict
- Parse cache: LRU with SHA-256 content hash
- Daemon mode: file watcher + warm cache
- Linter orchestration: detect/run/normalize ruff, clippy, eslint
