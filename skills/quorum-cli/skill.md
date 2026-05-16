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

### LLM micro-judge (v0.22.0+)

```bash
quorum review src/*.rs --judge
quorum review src/*.rs --judge --judge-model gpt-5-nano
```

Enables a secondary LLM pass that validates speculative AST rule matches. The judge confirms or suppresses findings that pattern-matched syntactically but may be benign in context. Default judge model: `gpt-5-nano` (fast, cheap). Also enabled by `QUORUM_JUDGE=1`.

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

### Dimensional drill-downs

```bash
quorum stats --by-repo                # aggregate by repository
quorum stats --by-caller              # aggregate by invocation caller (CLAUDE_CODE, tty, etc.)
quorum stats --by-file                # file hotspot ranking from feedback
quorum stats --by-file --top 10       # top N file hotspots
quorum stats --by-rule                # per-rule TP/FP/precision breakdown
quorum stats --by-rule --rule "ast-grep:python/*"  # filter rules by glob
quorum stats --rolling 50             # rolling 50-review windows
quorum stats --full                   # show all drill-downs (by-caller, rolling)
quorum stats --minimal                # hide dimensional highlights
```

### Context injection diagnostics

```bash
quorum stats --by-source              # stats grouped by injected context source
quorum stats --by-reviewed-repo       # context stats grouped by reviewed repo
quorum stats --misleading             # detect retriever errors and phantom injections
quorum stats --join-health            # reviews-to-feedback linkage diagnostic
```

### Calibrate (standalone)

```bash
quorum calibrate                      # recompute calibrator thresholds from feedback corpus
```

## Telemetry

Review telemetry is local-only and append-only at `~/.quorum/telemetry.jsonl`. Records per-review:
- Timestamp, files reviewed, finding counts by severity
- Model used, tokens in/out, duration
- No file contents, no finding text, no code snippets

Telemetry is best-effort — failures don't break reviews. Delete the file at any time with no impact.

## Recording Feedback

Feedback improves future reviews via the calibrator. Use the MCP `feedback` tool after reviewing findings.

### Which verdict to use

Only `fp` and `tp` affect the calibrator. `partial` and `wontfix` are inert metadata — they don't suppress or boost anything. `context_misleading` trains the context injector.

| Verdict | Calibrator effect | When to use |
|---------|-------------------|-------------|
| **fp** | **Suppresses** similar findings (needs 2+ matches) | Finding is wrong — not a real issue in context |
| **tp** | **Boosts** similar findings (needs 2+ matches) | Finding is real and actionable |
| **partial** | None | Real issue but severity overstated (e.g. "critical" should be "medium") |
| **wontfix** | None | Prefer `tp` (real) or `fp` (wrong) — `wontfix` is inert |
| **context_misleading** | Raises injection threshold for blamed chunks | Injected context led the LLM to a wrong finding |

### Decision tree by scenario

Work through these in order. The first match wins.

**Step 1: Is the finding wrong?**

| Scenario | Verdict | Flags | Why |
|----------|---------|-------|-----|
| LLM hallucinated (wrong line, fabricated function) | `fp` | `--fp-kind hallucination` | Trains suppression; hallucination tag tracks LLM reliability |
| Pattern matched syntactically but context makes it benign (e.g. `unwrap()` on guaranteed `Some`) | `fp` | `--fp-kind pattern-overgeneralization --fp-discriminator "why it's safe"` | Teaches the LLM when the pattern is NOT a bug |
| Wrong threat model (e.g. flagging internal API as SSRF risk) | `fp` | `--fp-kind trust-model` | Decays 3x faster because trust models evolve |
| Real pattern but mitigated upstream (e.g. input validated by caller) | `fp` | `--fp-kind compensating-control --fp-reference "src/auth.rs:42"` | Records where the mitigation lives |
| Wrong because injected context was stale/misleading | `context_misleading` | `--blamed-chunks id1,id2` | Trains the injector, not the calibrator |

**Step 2: Is the finding real and in your diff?**

| Scenario | Verdict | Flags | Why |
|----------|---------|-------|-----|
| Real bug, you fixed it | `tp` | `--in-diff true --reason "Fixed by ..."` | Strongest learning signal for the calibrator |
| Real bug, you won't fix it now | `tp` | `--in-diff true --reason "Accepted: ..."` | Still a real pattern — helps calibrator recognize similar issues |
| Real but severity overstated (e.g. "critical" should be "medium") | `partial` | `--in-diff true --reason "Severity should be medium"` | Inert for calibrator but records your assessment |

**Step 3: Is the finding real but outside your diff?**

| Scenario | Verdict | Flags | Why |
|----------|---------|-------|-----|
| Real bug, you file an issue for it | `tp` | `--in-diff false --reason "Filed as issue #123"` | Teaches the calibrator this pattern catches real bugs (0.7x weight for out-of-diff). The issue tracks the real fix. |
| Real bug, not worth filing | Skip | — | Recording feedback on unactionable findings adds noise. |
| Pre-existing, already tracked | Skip | — | Already tracked elsewhere. |

**Key principles:**
- `tp` means "I want to see MORE findings like this." Use for all real bugs — in-diff or out-of-diff.
- `fp` means "I want to see FEWER findings like this." Use only for findings that are genuinely wrong. `fp` suppresses globally across all projects, so reserve it for context-specific errors — not intentional design choices you want to keep.
- Record a verdict for every in-diff finding — each one is a learning opportunity for the calibrator.
- For real bugs outside your diff, use `tp --in-diff false` (0.7x weight). `fp --fp-kind out-of-scope` is metadata-only — excluded from the calibrator precedent pool. Add it alongside `tp --in-diff false` if you want the tracking link via `--fp-tracked-in`, or skip it.
- For pre-existing findings you won't file an issue for, skip them — recording feedback on code you can't act on adds noise.
- Always choose `tp` or `fp` — `wontfix` is inert. `partial` is inert metadata for severity disagreements only.

### Useful feedback flags

| Flag | Purpose |
|------|---------|
| `--in-diff true/false` | Explicitly mark whether the finding was inside the diff scope |
| `--blamed-chunks id1,id2` | Chunk IDs that caused misleading context (with `context_misleading` verdict) |
| `--category <cat>` | Finding category (e.g. "security", "correctness") |

### Classifying false positives with `fp_kind` (v0.18.0+)

When recording an `fp`, classify *why* it was wrong via `--fp-kind` (CLI, kebab-case) or `fpKind` (MCP, snake_case). The CLI and MCP enums are independent — one pair diverges (CLI uses `trust-model`, MCP uses `trust_model_assumption`) — and some variants carry payload fields:

| CLI flag | MCP `fpKind` | τ | Half-life | When to use |
|----------|--------------|---:|----------:|-------------|
| `hallucination` | `hallucination` | 120d | ~83d | Reviewer cited code/API that doesn't exist (wrong line, fabricated function, nonexistent import) |
| `pattern-overgeneralization` | `pattern_overgeneralization` | 120d | ~83d | Pattern matched but context makes it benign. Pass `--fp-discriminator` (or MCP nested `discriminator_hint`) to teach the LLM the distinction |
| `trust-model` | `trust_model_assumption` | 40d | ~28d | Wrong threat model — decays 3× faster because trust models evolve |
| `compensating-control` | `compensating_control` | 120d | ~83d | Real pattern, mitigated upstream. **Requires** `--fp-reference <file:line\|PR\|URL>` (CLI) or nested `{reference: "..."}` (MCP) |
| `out-of-scope` | `out_of_scope` | 120d | ~83d | Pre-existing in diff-scoped review. Metadata-only — excluded from calibrator precedent pool. Optional `--fp-tracked-in` (CLI) / `tracked_in` (MCP) records follow-up link |

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

**fp_kind is dropped on the External path** — when `--from-agent` (CLI) or `fromAgent` (MCP) is set, the verdict routes through `ExternalVerdictInput` which does not currently carry fp_kind. A `tracing::warn` fires at the MCP boundary. Only the direct/human ingestion path preserves fp_kind.

### What drives precision and recall

The calibrator learns from `tp` and `fp` verdicts only. Your feedback directly controls review quality:

- **Every `tp` you record** boosts similar findings → improves recall (quorum surfaces more real bugs)
- **Every `fp` you record** suppresses similar findings → improves precision (fewer false alarms)
- **Unrecorded findings** teach nothing — the calibrator can't learn from silence

The highest-value feedback is `tp` on real bugs you fixed (the calibrator learns "this pattern catches real issues") and `fp` with a specific `fp_kind` (the calibrator learns "this pattern fires incorrectly in this context"). Generic `fp` without `fp_kind` still helps but decays at the default rate.

## Context Injection (v0.16.0+)

Quorum can inject project-specific context (codebase docs, architecture notes, API specs) into the LLM prompt to improve review quality.

### Setup

```bash
quorum context init                   # create ~/.quorum/sources.toml
quorum context add ~/projects/mylib   # register a local source
quorum context add https://github.com/org/repo  # register a git source
quorum context index                  # extract + embed all sources
quorum context list                   # show registered sources
quorum context query "auth flow"      # semantic search across indexed chunks
quorum context refresh                # re-index drifted sources
quorum context prune                  # remove artefacts for deregistered sources
quorum context doctor                 # health checks on the context store
```

When `auto_inject = true` in `~/.quorum/sources.toml`, every `quorum review` retrieves top-k chunks, plans under a token budget (40% floor, symbols-first), and renders a fenced Markdown block into the LLM prompt.

### Context7 Framework Enrichment

Quorum auto-detects project dependencies from manifests (Cargo.toml, package.json, pyproject.toml) and queries Context7 for framework docs relevant to each file's imports. Controlled via:

```bash
quorum review src/*.rs --skip-context7    # skip framework doc enrichment
quorum review src/*.rs --live-registry    # enable download-count popularity lookups
# Also: QUORUM_CONTEXT7_LIVE_REGISTRY=1
```

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

### Review flags

| Flag | Purpose | Example |
|------|---------|---------|
| --compact | Token-efficient output (1 finding/line) | --compact |
| --parallel N | Max concurrent LLM calls (default: 4, 0=unlimited, 1=sequential) | --parallel 8 |
| --framework X | Override framework detection (e.g., home-assistant, terraform) | --framework terraform |
| --reasoning-effort | Control reasoning depth (none/low/medium/high) | --reasoning-effort=low |
| --calibration-model | Model for auto-calibration pass | --calibration-model=gpt-5.3-codex |
| --no-auto-calibrate | Skip automatic triage | |
| --judge | Enable LLM micro-judge for speculative AST rules | --judge |
| --judge-model M | Model for judge calls (default: gpt-5-nano) | --judge-model gpt-5-nano |
| --fast | Skip fastembed model (saves ~1.5 GB RAM, ~15s startup) | --fast |
| --trace | Enable structured tracing to ~/.quorum/trace.jsonl | --trace |
| --mode M | Review mode: code (default), plan, docs | --mode plan |
| --show-suppressed | Show findings suppressed by project rules | --show-suppressed |
| --skip-context7 | Skip Context7 framework doc enrichment | --skip-context7 |
| --live-registry | Enable download-count popularity lookups | --live-registry |

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

## Agentic Review + Feedback Workflow

When using quorum as part of a development workflow (e.g. `/dev:start`), follow this loop after implementation is complete:

### 1. Run diff-scoped review

Use the diff-aware review mode (see "Diff-aware review" section above):

```bash
git diff main > /tmp/changes.patch
quorum review <changed-files> --diff-file /tmp/changes.patch --compact
```

### 2. Triage every finding

Use the **Decision tree by scenario** (above) to pick the right verdict for each finding. Summary:

**In-branch** (introduced or touched by this work):
- Real bugs: fix using TDD micro-cycle (RED test reproducing finding, GREEN fix, verify), then record `tp --in-diff true`
- False positives: record `fp` with `--fp-kind` and `--reason`

**Pre-existing** (unrelated to this branch):
- Real bugs worth filing: file GitHub issue with `gh issue create` citing file:line + finding text, then record `tp --in-diff false --reason "Filed as issue #N"` (teaches calibrator the pattern is valid at 0.7x weight)
- Not worth filing: skip feedback. Keep this branch focused on its scope.

### 3. Re-run until clean

Re-run quorum on changed files until only accepted/wontfix findings remain on the changed surface.

### 4. Record feedback for every triaged finding

```bash
# True positive you fixed
quorum feedback --file src/x.rs --finding "SQL injection" --verdict tp \
    --in-diff true --reason "Fixed by parameterizing query"

# True positive, accepted but not fixed
quorum feedback --file src/x.rs --finding "Missing error handling" --verdict tp \
    --in-diff true --reason "Real issue, will address in follow-up"

# Misleading injected context caused a bad finding
quorum feedback --file src/x.rs --finding "Wrong API usage" \
    --verdict context_misleading --blamed-chunks chunk_id_1,chunk_id_2 \
    --reason "Stale docs led LLM to flag correct usage as wrong"

# False positive with classification
quorum feedback --file src/x.rs --finding "unwrap on Option" --verdict fp \
    --fp-kind pattern-overgeneralization \
    --fp-discriminator "type-system-guaranteed Some" \
    --reason "Match arm guarantees Some variant"
```

### 5. Merge CodeRabbit findings (if available)

If CodeRabbit (or another external reviewer) posted findings on the PR, evaluate those too. For external-agent verdicts:

```bash
quorum feedback --file src/x.rs --finding "Memory leak" --verdict tp \
    --from-agent coderabbit --reason "confirmed, fixed"
```

Note: `fp_kind` is dropped on the external path — it only persists on direct/human ingestion.

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
