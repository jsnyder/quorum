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
# CLI (new in v0.9.5)
quorum feedback --file src/auth.rs --finding "SQL injection" --verdict tp --reason "Fixed with params"
quorum feedback --file src/auth.rs --finding "complexity 5" --verdict fp --reason "Trivial match"

# Or via MCP feedback tool (in Claude Code)
# Or programmatically via the FeedbackStore API
```

Provenance weights: post_fix (1.5x), human (1.0x), auto_calibrate (0.5x, soft-suppresses to INFO).

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
