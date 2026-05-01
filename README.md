# quorum

Multi-source code review: local AST analysis + LLM ensemble + linter orchestration + ast-grep rules + feedback-calibrated findings.

Rust-native successor to [third-opinion](https://github.com/jsnyder/third-opinion). Single binary, 606 tests, 31MB.

## What it does

quorum reviews code using four complementary sources:

1. **Local AST analysis** (instant, free) -- tree-sitter patterns for 8 languages
2. **LLM cold read** (12-20s) -- GPT-5.4/Claude/Gemini via any OpenAI-compatible endpoint
3. **Linter orchestration** -- normalize ruff/clippy/eslint/yamllint/shellcheck/hadolint/tflint output into unified findings
4. **ast-grep rules** (instant, extensible) -- 20 bundled + user-customizable YAML pattern rules

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
| Terraform | .tf, .tfvars | secrets, wildcard IAM, open SGs, missing version pins | tflint |
| Other | * | LLM-only review (no AST) | -- |

## ast-grep Custom Rules

quorum integrates [ast-grep](https://ast-grep.github.io/) as an extensible pattern engine. 20 bundled rules ship in `rules/` covering common patterns from feedback analysis of 1,666 confirmed true positives.

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

## Context Injection (`quorum context`)

Quorum can index your own code (monorepo packages, internal SDKs, documentation directories) into a local hybrid search index and automatically splice the most relevant symbols and prose into every LLM review prompt. This is an offline, privacy-preserving alternative to external doc lookups.

```bash
# 1. Register a source (path or git URL)
quorum context init
quorum context add --name internal-sdk --kind rust --path /src/internal-sdk --weight 10

# 2. Build the index (once; refresh after large changes)
quorum context index --source internal-sdk

# 3. Verify the index is healthy
quorum context doctor

# 4. Normal reviews now inject the top-k relevant chunks
quorum review src/billing.rs   # prompt will include matching internal-sdk context
```

Under the hood the index is FTS5 + sqlite-vec with a reranker over BM25 / vector / id-match / path-match / recency signals. The rendered block is capped by token budget, deduped by qualified name, and gated per-chunk via the calibrator so `context_misleading` feedback permanently suppresses misleading hits after N confirmations. Every review records a `ContextTelemetry` entry (retrieved/injected counts, token count, rendered-prompt hash) for audit.

Configuration lives at `~/.quorum/sources.toml`; set `context.auto_inject = false` to disable injection without removing the indexes.

**Tuning `inject_budget_tokens`** (default `1500`): the planner enforces a 40%-of-budget floor on the smallest chunk it will inject, so a chunk only lands when it contributes at least `0.4 × inject_budget_tokens` tokens. This prevents tiny stubs from displacing more substantial context. If your sources index short helpers (< 600 tokens), lower the budget (try `500`–`800`) until relevant chunks clear the floor. Watch `reviews.jsonl` → `context.injected_chunk_count` to validate.

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

### Bundled Claude Code skill

A `quorum-cli` Claude Code skill ships with the repo at `skills/quorum-cli/skill.md`. It teaches Claude when to reach for which review mode, how to record feedback, and the v0.18.0+ `fp_kind` taxonomy.

```bash
mkdir -p ~/.claude/skills/quorum-cli
cp skills/quorum-cli/skill.md ~/.claude/skills/quorum-cli/
```

Re-run after `git pull` to keep it in sync with the repo's source of truth.

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
# Direct (human triage)
quorum feedback --file src/auth.rs --finding "SQL injection" --verdict tp --reason "Fixed with params"
quorum feedback --file src/auth.rs --finding "complexity 5" --verdict fp --reason "Trivial match"

# False positive with structured kind (v0.18.0+)
quorum feedback --file src/x.rs --finding "unwrap on Option" --verdict fp \
    --fp-kind pattern-overgeneralization --fp-discriminator "type-system-guaranteed Some" \
    --reason "context-specific exception"
quorum feedback --file src/x.rs --finding "fabricated function" --verdict fp \
    --fp-kind hallucination --reason "no such function in this module"
quorum feedback --file src/x.rs --finding "SQL concat" --verdict fp \
    --fp-kind compensating-control --fp-reference "src/auth.rs:42" \
    --reason "param-validating handler upstream"

# After applying a fix (1.5x calibrator weight)
quorum feedback --file src/x.rs --finding "Bug" --verdict tp --reason "Fixed in PR #200" \
    --provenance post_fix

# From another review agent (pal, third-opinion, gemini, reviewdog, ...)
quorum feedback --file src/x.rs --finding "Bug" --verdict tp --reason "confirmed" \
    --from-agent pal --agent-model gpt-5.4 --confidence 0.9

# Or via MCP feedback tool (in Claude Code), with optional fromAgent / agentModel / confidence / fpKind fields.
# Note: fpKind uses snake_case wire format (e.g. "trust_model_assumption", "out_of_scope"),
# and is *dropped* when fromAgent is set (the External path does not yet carry fpKind).
# Or programmatically via the FeedbackStore API
# Or by dropping JSONL files into ~/.quorum/inbox/ -- drained automatically on next review/stats
```

Provenance weights: `post_fix` (1.5x), `human` (1.0x), `external` (0.7x, capped at 1.4 globally), `auto_calibrate` (0.5x, soft-suppresses to INFO), `unknown` (0.3x).

**FpKind** (v0.18.0+) classifies *why* an FP was wrong, so the calibrator can decay each class on its own schedule. The CLI accepts kebab-case (`--fp-kind`); MCP uses snake_case (`fpKind`). The names are independent enums and one pair diverges: CLI shortens to `trust-model` while MCP uses the full `trust_model_assumption`.

| CLI | MCP `fpKind` | τ | Half-life | When to use |
|-----|--------------|---:|----------:|-------------|
| `hallucination` | `hallucination` | 120d | ~83d | Reviewer cited code/API that doesn't exist |
| `pattern-overgeneralization` | `pattern_overgeneralization` | 120d | ~83d | Pattern matched but context makes it benign. Add `--fp-discriminator` to teach the LLM the distinction |
| `trust-model` | `trust_model_assumption` | 40d | ~28d | Wrong threat model (internal vs. user-supplied) — decays 3× faster |
| `compensating-control` | `compensating_control` | 120d | ~83d | Real pattern, mitigated upstream. **Requires** `--fp-reference` (CLI) or `{reference: "..."}` (MCP) |
| `out-of-scope` | `out_of_scope` | 120d | ~83d | Pre-existing in diff-scoped review. Optional `--fp-tracked-in` for the follow-up link |

Untagged FPs use the default τ=120d (~83d half-life). `quorum stats` reports `fp_kind_utilization_rate` once ≥10% of recent FPs are tagged.

`fp_kind` is **dropped on the External path** — when `--from-agent` (CLI) or `fromAgent` (MCP) is set, the verdict routes through `ExternalVerdictInput`, which does not currently carry `fp_kind`. A `tracing::warn` fires at the MCP boundary so the dropped field is visible. Tag external-agent verdicts on the agent's side, not via `fpKind`.

External-agent verdicts go through a stricter trust boundary: only `tp`/`fp`/`partial` are accepted (no `wontfix`/`context_misleading`), confidence is clamped to [0,1], and the global External contribution to any finding's TP/FP weight is capped to prevent a single misbehaving agent from dominating calibration.

Feedback drives AST pattern development -- 20 ast-grep rules were mined from 1,666 confirmed true positives in the feedback store.

## Project-Level Suppression

Suppress known findings per-project via `.quorum/suppress.toml`:

```toml
[[suppress]]
pattern = "TLS certificate verification"
category = "security"
file = "src/url_resolver.py"
reason = "Intentional -- self-signed cert on local network"

[[suppress]]
pattern = "cyclomatic complexity"
reason = "Accepted for config loading patterns"
```

```bash
# Review with suppression
quorum review src/*.py

# Audit what's being hidden
quorum review src/*.py --show-suppressed
```

Matching: case-insensitive substring on title, exact on category, glob on file path. All specified fields must match (AND logic). No effect on global calibration.

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

# HTTP timeouts (v0.18.0+)
QUORUM_HTTP_TIMEOUT=300        # total LLM request timeout, seconds (default 300)
QUORUM_HTTP_READ_TIMEOUT=120   # idle/read timeout, seconds (default 120)

# base_url validation (v0.18.0+)
QUORUM_ALLOWED_BASE_URL_HOSTS=litellm.example.com   # comma-separated host allowlist
QUORUM_ALLOW_PRIVATE_BASE_URL=1                      # allow private/loopback IPs (LAN dev)
QUORUM_UNSAFE_BASE_URL=1                             # disable SSRF/scheme guards (last resort)
```

The base_url validator requires HTTPS by default, rejects credentials embedded in the URL, blocks private/loopback IPs unless explicitly opted in, and consults `QUORUM_ALLOWED_BASE_URL_HOSTS` when set. Use the host allowlist for self-hosted LiteLLM proxies on the public internet; use `QUORUM_ALLOW_PRIVATE_BASE_URL=1` for LAN/dev deployments.

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
  -> Truncate (500 lines max for LLM, annotates findings)
  -> Parallel reviewers:
    |-- Local AST patterns (instant, 8 languages)
    |-- ast-grep rules (instant, 20 bundled + user rules)
    |-- LLM cold read (GPT-5.4 + Context7 docs + suggested fixes)
    +-- Linters (ruff, clippy, eslint, yamllint, shellcheck, hadolint, tflint)
  -> Merge/dedup -> Calibrate (feedback, soft-suppress auto-only FPs)
  -> Project suppress (.quorum/suppress.toml)
  -> Auto-calibrate (o3 triage) -> Output (human/compact/JSON) -> Exit code
```

## License

MIT
