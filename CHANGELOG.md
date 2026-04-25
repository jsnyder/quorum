# Changelog

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
