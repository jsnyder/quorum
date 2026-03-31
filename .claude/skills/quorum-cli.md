---
name: quorum-cli
description: Use when reviewing code with quorum, recording feedback, or running the daemon. Provides optimal workflows for local AST analysis + LLM review with the quorum code review tool.
---

# Quorum CLI

Multi-source code review: local AST analysis + LLM ensemble + linter orchestration + feedback-calibrated findings. Rust-native, single binary.

## Review Modes (choose by context)

| Mode | Command | Speed | Depth | When to use |
|------|---------|-------|-------|-------------|
| Local-only | `quorum review <files>` | 7ms | Pattern-matching | CI gates, quick checks, no API key |
| LLM-augmented | `QUORUM_API_KEY=... quorum review <files>` | 12-20s | Reasoning | Thorough review, pre-merge |
| Ensemble | `quorum review --ensemble <files>` | 60-90s | Multi-model | Critical code, security audit |
| Via daemon | `quorum review --daemon <files>` | <1ms cached | Same as LLM | Repeated reviews, editor integration |

### Default: local + single LLM

```bash
quorum review src/auth.py src/db.py
```

Runs local AST patterns (instant) + LLM cold read (if API key set). Findings merged and calibrated against feedback history.

### Multi-file with JSON output

```bash
quorum review src/*.rs --json | jq '.[].findings[] | select(.severity == "critical")'
```

JSON output is grouped by file: `[{file, findings: [...]}]`

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
- `eval()`, `document.write()`, `innerHTML` XSS
- Hardcoded secrets, `any` type, non-null assertions
- `console.log`/`console.debug` debug artifacts

### YAML / Home Assistant
- Duplicate keys (silent data loss)
- Hardcoded secrets (skips `!secret`, `!include`, `!env_var`)
- HA automation: missing `id`, missing `mode`, deprecated singular `trigger`/`action`/`condition`, empty triggers/actions
- `entity_id` without domain prefix, `service` without domain
- URL with embedded credentials, `0.0.0.0` server binding
- ESPHome: OTA without password, API without encryption
- Jinja2: `states()` without availability check, deprecated dot-notation access

## What LLM Analysis Adds

- Logic bugs, race conditions, state management issues
- Security vulnerabilities requiring reasoning (CSRF, SSRF, auth bypass)
- Design issues (incorrect API usage, missing error handling)
- Context-aware findings using hydrated AST context (callee signatures, type definitions, blast radius)
- HA-specific: strapping pin usage, sensor fallback logic, template rendering issues

## Recording Feedback

Feedback improves future reviews via the calibrator with vector similarity matching.

```bash
# Via MCP (when using quorum serve)
# Use the feedback tool with verdict: tp, fp, partial, wontfix

# Programmatically (append JSONL)
echo '{"file_path":"config.yaml","finding_title":"...","finding_category":"security","verdict":"fp","reason":"Intentional design","model":"gpt-5.4","timestamp":"...","provenance":"human"}' >> ~/.quorum/feedback.jsonl
```

Verdicts:
- `tp`: true positive, real issue
- `fp`: false positive, not a real issue in context
- `partial`: real but overstated severity
- `wontfix`: real issue, not worth fixing

Provenance weights: `post_fix` (1.5x) > `human` (1.0x) > `auto_calibrate` (0.5x) > `unknown` (0.3x)

## MCP Server (for Claude Code / agents)

```bash
quorum serve   # stdio transport, 6 tools
```

Tools: `review`, `chat`, `debug`, `testgen`, `feedback`, `catalog`

### Deep review (agent loop with tool calling)

```bash
quorum review src/auth.py --deep
```

Uses 3-5 LLM turns with read_file/grep/list_files tools for deeper investigation.

### Diff-aware review (PR scoping)

```bash
git diff main > /tmp/changes.patch
quorum review src/*.rs --diff-file /tmp/changes.patch
```

### Model configuration

| Flag | Purpose | Example |
|------|---------|---------|
| --reasoning-effort | Control reasoning depth | --reasoning-effort=low |
| --calibration-model | Model for auto-calibration | --calibration-model=gpt-5.3-codex |
| --no-auto-calibrate | Skip automatic triage | |
| --ensemble | Multi-model review | |
| --provenance | Show finding sources | |

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
