---
name: quorum-cli
description: Use when reviewing code with quorum, recording feedback, or running the daemon. Provides optimal workflows for local AST analysis + LLM review with the quorum code review tool.
---

# Quorum

Multi-source code review: local AST analysis + LLM ensemble + linter orchestration + feedback-calibrated findings. Rust-native, single binary.

## Install

This skill ships with the quorum repo. To install it for use in Claude Code:

```bash
mkdir -p ~/.claude/skills/quorum-cli
cp skills/quorum-cli/skill.md ~/.claude/skills/quorum-cli/
```

The repo copy is the source of truth — re-run after `git pull` to refresh.

## Review Modes (choose by context)

| Mode | Command | Speed | Depth | When to use |
|------|---------|-------|-------|-------------|
| Local-only | `quorum review <files>` | 7ms | Pattern-matching | CI gates, quick checks, no API key |
| LLM-augmented | `QUORUM_API_KEY=... quorum review <files>` | 12-20s | Reasoning | Thorough review, pre-merge |
| Parallel | `quorum review <files> --parallel 4` | ~8s/3 files | Same as LLM | Multi-file reviews (default) |
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

Feedback improves future reviews via the calibrator. Use the MCP `feedback` tool after reviewing findings.

### Which verdict to use

Only `fp` and `tp` affect the calibrator. `partial` and `wontfix` are inert metadata — they don't suppress or boost anything.

| Verdict | Calibrator effect | When to use |
|---------|-------------------|-------------|
| **fp** | **Suppresses** similar findings (needs 2+ matches) | Finding is wrong — not a real issue in context |
| **tp** | **Boosts** similar findings (needs 2+ matches) | Finding is real and actionable |
| **partial** | None | Real issue but severity overstated (e.g. "critical" should be "medium") |
| **wontfix** | None | Avoid — use `tp` instead if the issue is real |

### Decision flowchart

1. **Is the finding wrong?** → `fp` (trains suppression globally)
2. **Is the finding real and actionable?** → `tp` (trains boosting globally)
3. **Is it real but severity overstated?** → `partial`
4. **Is it real but accepted debt / not worth fixing?** → `tp` (still a real pattern — helps calibrator recognize similar issues)
5. **Is it pre-existing, not related to your changes?** → Skip it. Don't record feedback for findings outside your diff scope.

### Classifying false positives with `fp_kind` (v0.18.0+)

When recording an `fp`, classify *why* it was wrong via `--fp-kind` (CLI, kebab-case) or `fpKind` (MCP, snake_case). The CLI and MCP enums are independent — one pair diverges (CLI uses `trust-model`, MCP uses `trust_model_assumption`) — and some variants carry payload fields:

| CLI flag | MCP `fpKind` | τ | Half-life | When to use |
|----------|--------------|---:|----------:|-------------|
| `hallucination` | `hallucination` | 120d | ~83d | Reviewer cited code/API that doesn't exist (wrong line, fabricated function, nonexistent import) |
| `pattern-overgeneralization` | `pattern_overgeneralization` | 120d | ~83d | Pattern matched but context makes it benign. Pass `--fp-discriminator` (or MCP nested `discriminator_hint`) to teach the LLM the distinction |
| `trust-model` | `trust_model_assumption` | 40d | ~28d | Wrong threat model — decays 3× faster because trust models evolve |
| `compensating-control` | `compensating_control` | 120d | ~83d | Real pattern, mitigated upstream. **Requires** `--fp-reference <file:line\|PR\|URL>` (CLI) or nested `{reference: "..."}` (MCP) |
| `out-of-scope` | `out_of_scope` | 120d | ~83d | Pre-existing in diff-scoped review. Optional `--fp-tracked-in` (CLI) / `tracked_in` (MCP) records follow-up link |

```bash
# CLI
quorum feedback --file src/x.rs --finding "unwrap on Option" --verdict fp \
    --fp-kind pattern-overgeneralization --fp-discriminator "type-system-guaranteed Some" \
    --reason "context-specific exception"

quorum feedback --file src/x.rs --finding "SQL concat" --verdict fp \
    --fp-kind compensating-control --fp-reference "src/auth.rs:42" \
    --reason "param-validating handler upstream"

# MCP feedback tool — fpKind values:
#   "hallucination"
#   "trust_model_assumption"
#   {"compensating_control": {"reference": "PR #99"}}
#   {"pattern_overgeneralization": {"discriminator_hint": "..."}}  # discriminator_hint optional
#   {"out_of_scope": {"tracked_in": "issue #200"}}                  # tracked_in optional
```

Untagged FPs use the default τ=120d (~83d half-life). `quorum stats` reports `fp_kind_utilization_rate` once ≥10% of recent FPs are tagged.

**fp_kind is dropped on the External path** — when `--from-agent` (CLI) or `fromAgent` (MCP) is set, the verdict routes through `ExternalVerdictInput` which does not currently carry fp_kind. A `tracing::warn` fires at the MCP boundary. Don't expect fp_kind to persist for external-agent verdicts.

### What NOT to do

- **Don't use `wontfix`** — it's inert. If the issue is real, use `tp`. If it's not real, use `fp`.
- **Don't record feedback for pre-existing findings** you didn't touch — it pollutes the global calibrator with noise from code you're not changing.
- **Don't record `fp` for intentional patterns** (like disabled TLS for self-signed certs) unless you want them suppressed across ALL projects.

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
| --parallel N | Max concurrent LLM calls (default: 4, 0=unlimited, 1=sequential) | --parallel 8 |
| --framework X | Override framework detection (e.g., home-assistant, terraform) | --framework terraform |
| --reasoning-effort | Control reasoning depth (none/low/medium/high) | --reasoning-effort=low |
| --calibration-model | Model for auto-calibration pass | --calibration-model=gpt-5.3-codex |
| --no-auto-calibrate | Skip automatic triage | |

### Recommended configurations

| Scenario | Config | Speed |
|----------|--------|-------|
| Default | gpt-5.4, --parallel 4 | ~8s/3 files |
| Fast CI | gpt-5.3-codex, --no-auto-calibrate | ~1s |
| Deep audit | gpt-5.2 --deep --reasoning-effort=high | ~100s |
| Sequential | --parallel 1 (debugging) | ~45s/file |
| No API key | (local only) | 7ms |

### Feedback provenance

Verdicts from different sources carry different calibration weight:
- **post_fix** (1.5x): Recorded after applying a fix — strongest signal
- **human** (1.0x): Direct user triage
- **external** (0.7x): Verdict from another review agent (pal, third-opinion, gemini, reviewdog, ...) — capped at 1.4 globally so a single agent cannot dominate
- **auto_calibrate** (0.5x): LLM triage pass
- **unknown** (0.3x): Legacy entries

### Recording External-agent verdicts (v0.17.0+)

Verdicts from other review agents flow through three paths, all going through the same trust boundary (only `tp`/`fp`/`partial` accepted; confidence clamped to [0,1]; agent name normalized):

```bash
# CLI (sync)
quorum feedback --file src/x.rs --finding "Bug" --verdict tp --reason "confirmed" \
    --from-agent pal --agent-model gpt-5.4 --confidence 0.9

# MCP feedback tool — pass fromAgent / agentModel / confidence fields

# Inbox (async batch ingestion)
# Drop JSONL files into ~/.quorum/inbox/<anything>.jsonl
# Drained automatically on next `quorum review` or `quorum stats`
# Format: {"file_path":"...","finding_title":"...","finding_category":"...","verdict":"tp","reason":"...","agent":"pal","agent_model":null,"confidence":null}
```

The tier breakdown shows up under `quorum stats` Feedback Health when any non-Human entry exists, with a per-agent sub-line for External.

## Configuration

```bash
QUORUM_BASE_URL=https://litellm.example.com  # OpenAI-compatible endpoint
QUORUM_API_KEY=sk-...                         # enables LLM review
QUORUM_MODEL=gpt-5.4                          # default model
QUORUM_ENSEMBLE_MODELS=gpt-5.4,gemini-2.5-pro # for --ensemble

# HTTP timeouts (v0.18.0+)
QUORUM_HTTP_TIMEOUT=300        # total request timeout, seconds (default 300)
QUORUM_HTTP_READ_TIMEOUT=120   # idle/read timeout, seconds (default 120)

# base_url validation (v0.18.0+)
QUORUM_ALLOWED_BASE_URL_HOSTS=litellm.example.com  # comma-separated host allowlist
QUORUM_ALLOW_PRIVATE_BASE_URL=1                     # allow private/loopback IPs
QUORUM_UNSAFE_BASE_URL=1                            # disable SSRF/scheme guards (last resort)
```

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean (no findings or info-only) |
| 1 | Warnings (medium severity) |
| 2 | Critical (high/critical severity) |
| 3 | Tool error |
