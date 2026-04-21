# Changelog

## [Unreleased] — 0.16.0 (feat/context)

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
