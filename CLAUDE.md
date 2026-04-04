# Quorum

Multi-source code review tool: LLM ensemble + local AST analysis + linter orchestration + feedback-augmented calibration.

Rust-native, single-binary distribution. Successor to third-opinion (TypeScript).

## Commands

```bash
cargo build                    # compile
cargo test --bin quorum        # run unit tests (485 tests)
cargo test                     # run all tests (includes CLI integration)
cargo build --release          # release build (29MB binary)
cargo run -- version           # check version
cargo run -- review src/main.rs              # review a file
cargo run -- review src/*.rs --json          # JSON output (grouped by file)
cargo run -- review file.yaml --deep         # multi-turn agent loop
cargo run -- review file.rs --diff-file d.patch  # change-scoped review
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
| Multi-lang | .rs, .py, .ts, .js, .yaml, .sh, etc. | custom YAML rules via ast-grep | ast-grep |
| Other | * | LLM-only review (no AST) | — |

### ast-grep custom rules

Bundled rules live in `rules/<language>/`. Users can add custom rules to `~/.quorum/rules/<language>/` (e.g. `~/.quorum/rules/typescript/my-rule.yml`). Both directories are scanned automatically when ast-grep is in PATH.

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
Record feedback via MCP `feedback` tool or programmatically via the FeedbackStore API.
Verdicts: tp, fp, partial, wontfix. Provenance: post_fix (1.5x), human (1.0x), auto_calibrate (0.5x).
