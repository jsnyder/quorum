# Quorum — Rust-native multi-source code review tool

## Commands

```bash
cargo build                    # compile
cargo test --bin quorum        # run unit tests (662 tests)
cargo test                     # run all tests (includes CLI integration)
cargo build --release          # release build (31MB binary)
cargo run -- version           # check version
cargo run -- review src/main.rs              # review a file
cargo run -- review src/*.rs --json          # JSON output (grouped by file)
cargo run -- stats --by-repo                 # dimensional stats by repo
cargo run -- stats --by-caller               # dimensional stats by caller
cargo run -- stats --rolling 50              # rolling 50-review windows
cargo run -- review file.yaml --deep         # multi-turn agent loop
cargo run -- review file.rs --diff-file d.patch  # change-scoped review
cargo run -- review src/*.rs --parallel 4        # parallel LLM calls (default: 4)
cargo run -- feedback --file src/main.rs --finding "SQL injection" --verdict tp --reason "Fixed"
cargo run -- serve                           # MCP server (stdio)
cargo run -- daemon --watch-dir .            # persistent daemon
```

## Environment

```bash
QUORUM_BASE_URL=https://litellm.example.com   # OpenAI-compatible endpoint
QUORUM_API_KEY=sk-...                          # enables LLM review
QUORUM_MODEL=gpt-5.4                           # default model
QUORUM_ENSEMBLE_MODELS=gpt-5.4,gemini-2.5-pro  # for --ensemble
```

## Supported Languages

| Language | Extensions | AST Analysis | Linter |
|----------|-----------|-------------|--------|
| Rust | .rs | complexity, unsafe, unwrap | clippy |
| Python | .py | secrets, eval, SQL injection, mutable defaults, open() encoding, bare except:pass | ruff |
| TypeScript | .ts, .js, .mjs, .cjs | eval, innerHTML, secrets, any type, empty catch, sync-in-async, .length>=0 | eslint |
| TSX/JSX | .tsx, .jsx | same as TypeScript | eslint |
| YAML | .yaml, .yml | HA automations, secrets, duplicate keys, ESPHome, Jinja2 | yamllint |
| Bash | .sh, .bash, .zsh, .bats | eval, curl\|bash, set -e, secrets, chmod 777, shebang | shellcheck |
| Dockerfile | Dockerfile* | FROM latest, no USER, no HEALTHCHECK, secrets in ENV, ADD vs COPY, curl\|bash | hadolint |
| Terraform | .tf, .tfvars | secrets, wildcard IAM, open SGs, missing version pins | tflint |
| Multi-lang | .rs, .py, .ts, .js, .yaml, .sh, .tf, etc. | custom YAML rules via ast-grep | ast-grep |
| Other | * | LLM-only review (no AST) | — |

### ast-grep custom rules (53 bundled)

Bundled rules live in `rules/<language>/`. Users can add custom rules to `~/.quorum/rules/<language>/` (e.g. `~/.quorum/rules/typescript/my-rule.yml`). Both directories are scanned automatically when ast-grep is in PATH.

Bundled rules by language:
- **Python** (21): assert-in-prod-code, bare-except-pass, bind-all-interfaces, blocking-call-in-async, broad-exception-catch, eval-exec-non-literal, fastapi-unbounded-pagination, flask-debug-true, insecure-file-permissions, md5-usage, mutation-during-iteration, naive-url-blacklist, non-threadsafe-singleton, open-no-encoding, re-compile-in-loop, requests-verify-false, resource-no-context-manager, sqlalchemy-raw-query, subprocess-no-check, subprocess-shell-true, urlopen-no-context-manager
- **TypeScript** (15): as-any-cast, bare-catch, cors-wildcard-origin, eval-non-literal, json-parse-as-type, non-literal-regexp, non-null-assertion, nullish-coalescing-preferred, path-traversal-join, promise-async-executor, sql-template-injection, sync-in-async, tautological-length, tls-reject-unauthorized-false, unsafe-url-concat
- **JavaScript** (3): bind-all-interfaces, bind-in-event-listener (covers add+remove), console-log-artifact
- **Rust** (5): block-on-in-async, expect-empty-message, ignored-io-result, silent-error-conversion, string-byte-slice
- **Bash** (4): predictable-tmp, toctou-lock-touch, unquoted-variable, unsafe-grep-variable
- **YAML** (3): float-zero-fallback, ha-jinja-loop-scoped-reassignment, ha-template-none-fallback
- **HCL/Terraform** (2): iam-wildcard-action, iam-wildcard-resource

Test fixtures in `rules/<language>/tests/`. Gap analysis in `docs/feedback-pattern-mining.md`.

## Constraints

- All secrets redacted before LLM calls (always-on)
- Provider-agnostic: single OpenAI-compatible client, no provider-specific code paths
- JSON output grouped by file when piped, human output when TTY
- Exit codes: 0 = clean, 1 = warnings, 2 = critical, 3 = tool error
- No emojis in code or output
- CLI design follows DESIGN.md (adapted from clig.dev principles)
- Architecture documented in docs/ARCHITECTURE.md

## Feedback

Feedback is stored at `~/.quorum/feedback.jsonl` and loaded automatically for calibration.
Record feedback via CLI (`quorum feedback`), MCP `feedback` tool, or programmatically via the FeedbackStore API.
Verdicts: tp, fp, partial, wontfix. Provenance: post_fix (1.5x), human (1.0x), auto_calibrate (0.5x).

## Context7 Framework Enrichment

`src/context_enrichment.rs::enrich_for_review_in_project` parses the project's dep manifests (Cargo.toml, package.json, pyproject.toml + requirements.txt fallback) via `src/dep_manifest.rs::parse_dependencies`, filters to deps whose name appears in the file's `import_targets`, caps at K=5 in import-occurrence order, and queries Context7 with either a curated query (`curated_query_for(name)`) or a language-aware generic (`generic_query_for_language(lang)`). Curated frameworks detected by directory layout (HA/ESPHome) flow through additively. Per-review counters (`context7_resolved`, `context7_resolve_failed`, `context7_query_failed`) land in `TelemetryEntry`. Resolve results are cached with a 24h TTL (negative results too — avoids re-hammering Context7 for private crates / typos); the clock is injectable via `CachedContextFetcher::new_with_clock` for tests.

## Context Injection (v0.16.0+)

`quorum context <init|add|list|index|refresh|query|prune|doctor>` manages a local hybrid search index (FTS5 + sqlite-vec) at `~/.quorum/sources/<name>/`. When a source has been indexed and `~/.quorum/sources.toml` has `context.auto_inject = true`, every `quorum review` builds a `ContextInjector` via `context::bootstrap::build_production_injector`, retrieves top-k chunks, plans under the token budget (40% floor, symbols-first), and renders a fenced Markdown block spliced into the LLM prompt. Per-review telemetry captures retrieved/injected counts, rendered-prompt sha256, and calibrator suppressions in `ContextTelemetry`.

Feedback verdict `context_misleading` (with `blamed_chunks`) raises per-chunk injection thresholds via `Calibrator::injection_threshold_for`; after `inject_suppress_after` confirmations the chunk is permanently sealed (`f32::INFINITY`).

## Review Telemetry (v0.13.0+)

Per-review records at `~/.quorum/reviews.jsonl` (ULID-keyed, enables exact joins to feedback). Fields: `run_id, timestamp, repo, invoked_from, model, files_reviewed, findings_by_severity, tokens_in/out/cache_read, duration_ms, flags`. Cost is computed at display time, not stored (model pricing drifts).

The `context7_resolved`, `context7_resolve_failed`, and `context7_query_failed` counters live on `TelemetryEntry` in `~/.quorum/telemetry.jsonl` (written from `src/main.rs`), not on `ReviewRecord`. They use `serde(default)` for backward-compat with pre-bump rows.

`invoked_from` auto-detected from env vars (`CLAUDE_CODE`, `CODEX_CI`, `GEMINI_CLI`, `AGENT`, else tty/pipe) or overridden with `--caller <name>`.

Dimensional views aggregate this log: `stats --by-repo`, `--by-caller`, `--rolling N`. Sample-size gate at `MIN_SAMPLE=5`. Human output uses inline semigraphics (`█·` bars, `▁▂▃▄▅▆▇█` sparklines, ↑↓→ arrows) with ASCII fallback; compact mode is glyph-free single-line.
