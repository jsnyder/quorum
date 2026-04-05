# quorum

Multi-source code review: local AST analysis + LLM ensemble + linter orchestration + ast-grep rules + feedback-calibrated findings.

Rust-native successor to [third-opinion](https://github.com/jsnyder/third-opinion). Single binary, 492 tests, 31MB.

## What it does

quorum reviews code using four complementary sources:

1. **Local AST analysis** (instant, free) -- tree-sitter patterns for 7 languages
2. **LLM cold read** (12-20s) -- GPT-5.4/Claude/Gemini via any OpenAI-compatible endpoint
3. **Linter orchestration** -- normalize ruff/clippy/eslint/yamllint/shellcheck/hadolint output into unified findings
4. **ast-grep rules** (instant, extensible) -- user-customizable YAML pattern rules

Findings are merged, deduplicated, and calibrated using your feedback history. Each review automatically trains the calibrator for better future results.

## Install

### From source
```bash
cargo install --path .
```

### Pre-built binary
```bash
# macOS
curl -L https://github.com/jsnyder/quorum/releases/latest/download/quorum-darwin-arm64 -o quorum
chmod +x quorum && mv quorum /usr/local/bin/
```

## Quick Start

```bash
# Local analysis only (no API key needed, instant)
quorum review src/auth.py

# With LLM review
export QUORUM_API_KEY=sk-...
export QUORUM_BASE_URL=https://your-llm-endpoint.com/v1
quorum review src/auth.py

# With o3 calibration (recommended)
quorum review src/auth.py --calibration-model o3

# Multi-file
quorum review src/*.py

# JSON output
quorum review src/*.py --json

# Deep review (multi-turn agent loop)
quorum review src/auth.py --deep

# Change-scoped review
quorum review src/auth.py --diff-file changes.patch
```

## Modes

| Mode | Command | Speed | Use Case |
|------|---------|-------|----------|
| CLI one-shot | `quorum review file.py` | 7ms local, 15s+LLM | CI, quick checks |
| MCP server | `quorum serve` | persistent | Claude Code, AI agents |
| HTTP daemon | `quorum daemon` | cached | Editor integration, CI pipelines |
| Via daemon | `quorum review --daemon file.py` | <1ms cached | Repeated reviews |

## Supported Languages

| Language | Extensions | AST Patterns | Linter |
|----------|-----------|-------------|--------|
| Rust | .rs | complexity, unsafe, unwrap | clippy |
| Python | .py | secrets, eval, SQL injection, mutable defaults, open() encoding, bare except:pass, blocking .result() in async | ruff |
| TypeScript | .ts, .js, .mjs, .cjs | eval, innerHTML, secrets, any type, empty catch, sync-in-async, .length>=0 | eslint |
| TSX/JSX | .tsx, .jsx | same as TypeScript | eslint |
| YAML | .yaml, .yml | HA automations, secrets, duplicate keys, ESPHome, Jinja2 | yamllint |
| Bash | .sh, .bash, .zsh, .bats | eval, curl\|bash, set -e, secrets, chmod 777, shebang | shellcheck |
| Dockerfile | Dockerfile* | FROM latest, no USER, no HEALTHCHECK, secrets in ENV, ADD vs COPY, curl\|bash | hadolint |
| Other | * | LLM-only review (no AST) | -- |

## ast-grep Custom Rules

quorum integrates [ast-grep](https://ast-grep.github.io/) as an extensible pattern engine. 10 bundled rules ship in `rules/` covering common patterns from feedback analysis.

### Bundled rules

| Rule | Language | What it catches |
|------|----------|----------------|
| bare-catch | TypeScript | Empty catch blocks that swallow errors |
| sync-in-async | TypeScript | readFileSync/writeFileSync etc. in async functions |
| as-any-cast | TypeScript | `x as any` type safety bypass |
| tautological-length | TypeScript | `.length >= 0` (always true) |
| open-no-encoding | Python | `open()` without explicit encoding parameter |
| bare-except-pass | Python | `except: pass` catch-all error swallowing |
| resource-no-context-manager | Python | `open()` outside `with` statement |
| float-zero-fallback | YAML | `float(0)` masking unavailable HA sensors |
| predictable-tmp | Bash | `/tmp/$var` symlink vulnerability |
| block-on-in-async | Rust | `block_on` inside async functions |

### Adding custom rules

Drop `.yml` or `.yaml` files into `~/.quorum/rules/<language>/`:

```bash
mkdir -p ~/.quorum/rules/typescript
cat > ~/.quorum/rules/typescript/no-console-warn.yml << 'EOF'
id: no-console-warn
language: TypeScript
severity: hint
message: "console.warn() left in code"
rule:
  kind: call_expression
  pattern: console.warn($$$ARGS)
EOF
```

Rules are picked up automatically when ast-grep is in PATH. Project-local rules in `rules/` are also scanned.

## MCP Server (for Claude Code)

Add to your Claude Code MCP config:

```json
{
  "mcpServers": {
    "quorum": {
      "command": "quorum",
      "args": ["serve"],
      "env": {
        "QUORUM_BASE_URL": "https://your-endpoint.com/v1",
        "QUORUM_MODEL": "gpt-5.4"
      }
    }
  }
}
```

6 tools: `review`, `chat`, `debug`, `testgen`, `feedback`, `catalog`

## Auto-Calibration

Every LLM review automatically triages its own findings using a second model pass:

```bash
# Default: gpt-5.4 review (reasoning=low) + codex calibration
quorum review file.py

# With o3 calibration (more nuanced triage, slower)
quorum review file.py --calibration-model o3

# Disable for speed
quorum review file.py --no-auto-calibrate

# Control reasoning depth
quorum review file.py --reasoning-effort high  # deep analysis, slower
quorum review file.py --reasoning-effort none  # fastest, no reasoning
```

Verdicts (tp/fp/partial/wontfix) accumulate in `~/.quorum/feedback.jsonl`. The calibrator uses them to suppress known FPs and boost known TPs on future reviews.

## Feedback Loop

quorum improves over time through feedback. Record verdicts on findings:

```bash
# Via MCP feedback tool (in Claude Code)
# Or programmatically via the FeedbackStore API
```

Provenance weights: post_fix (1.5x), human (1.0x), auto_calibrate (0.5x).

Feedback also drives AST pattern development -- the 5 new patterns in v0.9.0 were identified by analyzing 840 confirmed true positives in the feedback store with GPT-5.4 and Gemini 3 Pro.

## Model Recommendations

| Scenario | Review Model | Calibrator | Reasoning | Speed |
|----------|-------------|-----------|-----------|-------|
| **Default (recommended)** | gpt-5.4 | gpt-5.3-codex | low | ~2s |
| Fast CI gate | gpt-5.3-codex | none | - | ~1s |
| Deep audit | gpt-5.2 | o3 | high | ~100s |
| Max coverage | claude-sonnet-4-6 | o3 | - | ~100s |
| No gpt-5.4 available | gpt-5.2 | gpt-5.3-codex | low | ~26s |

## Configuration

```bash
QUORUM_BASE_URL=https://api.openai.com/v1  # any OpenAI-compatible endpoint
QUORUM_API_KEY=sk-...                       # enables LLM review
QUORUM_MODEL=gpt-5.4                        # default review model
QUORUM_REASONING_EFFORT=low                 # default reasoning depth
QUORUM_ENSEMBLE_MODELS=gpt-5.4,claude       # for --ensemble mode
CONTEXT7_API_KEY=...                         # enables framework doc injection
```

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean (no findings or info-only) |
| 1 | Warnings (medium severity) |
| 2 | Critical (high/critical severity) |
| 3 | Tool error |

## Architecture

```
Code -> Parse (tree-sitter, cached) -> Hydrate (callee sigs, type defs)
  -> Parallel reviewers:
    |-- Local AST patterns (instant, 7 languages)
    |-- ast-grep rules (instant, user-extensible)
    |-- LLM cold read (GPT-5.4 + Context7 docs)
    +-- Linters (ruff, clippy, eslint, yamllint, shellcheck, hadolint, ast-grep)
  -> Merge/dedup -> Calibrate (feedback history) -> Auto-calibrate (o3 triage)
  -> Output (human/JSON) -> Exit code
```

## License

MIT
