---
name: quorum-cli
description: Use when reviewing code with quorum, recording feedback, or running the daemon. Provides optimal workflows for local AST analysis + LLM review with the quorum code review tool.
---

# Quorum

Multi-source code review: local AST analysis + LLM ensemble + linter orchestration + feedback-calibrated findings. Rust-native, single binary.

## Review Modes (choose by context)

| Mode | Command | Speed | Depth | When to use |
|------|---------|-------|-------|-------------|
| Local-only | `quorum review <files>` | 7ms | Pattern-matching | CI gates, quick checks, no API key |
| LLM-augmented | `QUORUM_API_KEY=... quorum review <files>` | 12-20s | Reasoning | Thorough review, pre-merge |
| Compact | `quorum review <files> --compact` | Same | Same | LLM consumption, token-efficient |
| Ensemble | `quorum review --ensemble <files>` | 60-90s | Multi-model | Critical code, security audit |
| Via daemon | `quorum review --daemon <files>` | <1ms cached | Same as LLM | Repeated reviews, editor integration |

### Default: local + single LLM

```bash
quorum review src/auth.py src/db.py
```

Runs local AST patterns (instant) + LLM cold read (if API key set). Findings merged and calibrated against feedback history.

### Compact output (for LLM consumption)

```bash
quorum review src/*.rs --compact
```

One finding per line: `!|security|L42|SQL injection risk`. Auto-detected when `CLAUDE_CODE` env var is set. Token-efficient for agent pipelines.

### Multi-file with JSON output

```bash
quorum review src/*.rs --json | jq '.[] | select(.severity == "critical")'
```

### Daemon mode for fast repeated reviews

```bash
# Terminal 1: start daemon (keeps cache warm)
quorum daemon --port 7842

# Terminal 2: reviews use cached parsing
quorum review --daemon src/main.rs
quorum review --daemon src/lib.rs  # cache hit, instant parse
```

## What Local Analysis Finds (no LLM needed)

### Rust
- Cyclomatic complexity > threshold
- `.unwrap()` in non-test code
- `unsafe` blocks

### Python (complements ruff, does not duplicate)
- Hardcoded secrets (SECRET_KEY, PASSWORD, API_KEY with string literals)
- `debug=True` in Flask/FastAPI/uvicorn
- `host="0.0.0.0"` server binding
- f-string/`.format()` in `cursor.execute()` (SQL injection)
- Mutable default arguments (`def foo(x=[])`)
- `eval()` / `exec()`

### TypeScript
- `eval()` calls

## What LLM Analysis Adds

- Logic bugs, race conditions, state management issues
- Security vulnerabilities requiring reasoning (CSRF, SSRF, auth bypass)
- Design issues (incorrect API usage, missing error handling)
- Context-aware findings using hydrated AST context (callee signatures, type definitions, blast radius)

## Stats Dashboard

```bash
quorum stats              # human-readable: feedback health, activity, spend
quorum stats --compact    # single-line for LLM consumption
quorum stats --json       # structured JSON with all metrics
```

Shows:
- **Feedback health**: entry count, precision %, TP/FP/partial/wontfix breakdown, weekly precision trend
- **Activity (7d)**: review count, findings/review, suppression rate
- **Spend (7d)**: tokens in/out, estimated cost, tokens/finding

Reads from `~/.quorum/feedback.jsonl` and `~/.quorum/telemetry.jsonl`.

## Telemetry

Review telemetry is local-only and append-only at `~/.quorum/telemetry.jsonl`. Records per-review:
- Timestamp, files reviewed, finding counts by severity
- Model used, tokens in/out, duration
- No file contents, no finding text, no code snippets

Telemetry is best-effort — failures don't break reviews. Delete the file at any time with no impact.

## Recording Feedback

Feedback improves future reviews via the calibrator. After reviewing findings:

```bash
# Via MCP (when using quorum serve)
# Use the feedback tool with verdict: tp, fp, partial, wontfix

# Via the daemon API
curl -X POST http://127.0.0.1:7842/review -H "Content-Type: application/json" \
  -d '{"file_path":"src/auth.py","code":"..."}'
```

Verdicts:
- `tp`: true positive, real issue
- `fp`: false positive, not a real issue in context
- `partial`: real but overstated severity
- `wontfix`: real issue, not worth fixing

The calibrator needs 2+ FP verdicts on similar findings to suppress. TP verdicts with 2+ matches boost severity.

## MCP Server (for Claude Code / agents)

```bash
quorum serve   # stdio transport, 6 tools
```

Tools: `review`, `chat`, `debug`, `testgen`, `feedback`, `catalog`

Add to Claude Code settings.json:
```json
{
  "mcpServers": {
    "quorum": {
      "command": "/path/to/quorum",
      "args": ["serve"]
    }
  }
}
```

### Deep review (agent loop with tool calling)

```bash
quorum review src/auth.py --deep
```

The agent reads related files, greps for patterns, and investigates before producing findings.
Uses 3-5 LLM turns with read_file/grep/list_files tools.

### Diff-aware review (PR scoping)

```bash
git diff main > /tmp/changes.patch
quorum review src/*.rs --diff-file /tmp/changes.patch
```

Hydration context scoped to changed lines only. Same finding quality, smaller prompt.

### Model configuration

| Flag | Purpose | Example |
|------|---------|---------|
| --compact | Token-efficient output (1 finding/line) | --compact |
| --reasoning-effort | Control reasoning depth (none/low/medium/high) | --reasoning-effort=low |
| --calibration-model | Model for auto-calibration pass | --calibration-model=gpt-5.3-codex |
| --no-auto-calibrate | Skip automatic triage | |

### Recommended configurations

| Scenario | Config | Speed |
|----------|--------|-------|
| Default | gpt-5.4, reasoning_effort=low | ~2s |
| Fast CI | gpt-5.3-codex, --no-auto-calibrate | ~1s |
| Deep audit | gpt-5.2 --deep --reasoning-effort=high | ~100s |
| No API key | (local only) | 7ms |

### Feedback provenance

Verdicts from different sources carry different calibration weight:
- **post_fix** (1.5x): Recorded after applying a fix — strongest signal
- **human** (1.0x): Direct user triage
- **auto_calibrate** (0.5x): LLM triage pass
- **unknown** (0.3x): Legacy entries

## Configuration

```bash
QUORUM_BASE_URL=https://litellm.example.com  # OpenAI-compatible endpoint
QUORUM_API_KEY=sk-...                         # enables LLM review
QUORUM_MODEL=gpt-5.4                          # default model
QUORUM_ENSEMBLE_MODELS=gpt-5.4,gemini-2.5-pro # for --ensemble
```

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean (no findings or info-only) |
| 1 | Warnings (medium severity) |
| 2 | Critical (high/critical severity) |
| 3 | Tool error |
