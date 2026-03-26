# quorum

Multi-source code review: local AST analysis + LLM ensemble + feedback-calibrated findings.

Rust-native successor to [third-opinion](https://github.com/jsnyder/third-opinion). Single binary, 310 tests, 11MB.

## What it does

quorum reviews code using three complementary sources:

1. **Local AST analysis** (instant, free) -- tree-sitter patterns for Rust, Python, TypeScript
2. **LLM cold read** (12-20s) -- GPT-5.4/Claude/Gemini via any OpenAI-compatible endpoint
3. **Linter orchestration** -- normalize ruff/clippy/eslint output into unified findings

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
```

## Modes

| Mode | Command | Speed | Use Case |
|------|---------|-------|----------|
| CLI one-shot | `quorum review file.py` | 7ms local, 15s+LLM | CI, quick checks |
| MCP server | `quorum serve` | persistent | Claude Code, AI agents |
| HTTP daemon | `quorum daemon` | cached | Editor integration, CI pipelines |
| Via daemon | `quorum review --daemon file.py` | <1ms cached | Repeated reviews |

## Local Patterns (no LLM needed)

### Rust
- Cyclomatic complexity
- `.unwrap()` in non-test code
- `unsafe` blocks

### Python (complements ruff)
- Hardcoded secrets (SECRET_KEY, PASSWORD, API_KEY)
- `debug=True` / `host="0.0.0.0"` in Flask/FastAPI
- f-string SQL injection in `.execute()`
- Mutable default arguments
- `eval()` / `exec()`
- Mutating collection while iterating
- Exception details in API responses
- Blocking `.result()` in async functions

### TypeScript (complements ESLint)
- Hardcoded secrets
- `innerHTML` / `document.write` XSS
- `console.log` debug artifacts
- `any` type annotations
- Non-null assertion `!` operator
- `eval()`

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
    |-- Local AST patterns (instant)
    |-- LLM cold read (GPT-5.4 + Context7 docs)
    +-- Linters (ruff, clippy, eslint)
  -> Merge/dedup -> Calibrate (feedback history) -> Auto-calibrate (o3 triage)
  -> Output (human/JSON) -> Exit code
```

## License

MIT
