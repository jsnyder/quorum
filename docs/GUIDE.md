# Quorum User Guide

## Installation

### From source (requires Rust 1.85+)

```bash
git clone https://github.com/jsnyder/quorum.git
cd quorum
cargo install --path .
```

### Verify

```bash
quorum version
# quorum 0.9.2
```

## Your First Review

Quorum works without any configuration. Point it at a file:

```bash
quorum review src/auth.py
```

It parses the file, runs AST pattern checks, and prints findings:

```
~ Review: src/auth.py

  ! Use of `eval()` is a code injection risk  [security] L14
    `eval()` executes arbitrary code. Avoid using it with untrusted input.

  ! Hardcoded secret in `SECRET_KEY`  [security] L5
    Secrets should be loaded from environment variables or a secrets manager,
    not hardcoded in source.

  ~ Catch-all `except: pass` silently swallows errors  [reliability] L19-20
    Catching all exceptions with `pass` hides bugs. Log the error or catch
    a specific exception type.

  - `open()` without explicit `encoding` parameter  [reliability] L11
    Without `encoding=`, open() uses the system default which varies by
    platform. Specify `encoding='utf-8'` for portable behavior.

  4 findings (2 critical, 1 warning, 1 info)
```

Severity markers: `!` critical/high, `~` medium, `-` low/info, `=` clean.

This local-only mode takes about 7 milliseconds. No API key, no network calls.

## Adding LLM Review

Set an API key to enable model-powered review alongside local analysis:

```bash
export QUORUM_BASE_URL=https://your-llm-endpoint.com/v1
export QUORUM_API_KEY=sk-...
quorum review src/auth.py
```

The LLM finds issues that patterns cannot:

```
  ! Insecure password hashing using MD5  [security] L7-8
    MD5 is not suitable for password hashing because it is fast and
    vulnerable to brute-force attacks. Use bcrypt, scrypt, or Argon2.

  ~ Undefined function call to decode  [bug] L18
    The function decode is used but never imported or defined in this
    module. This will raise a NameError at runtime.
```

Local patterns catch structural issues (eval, secrets, bare except). The LLM catches semantic issues (wrong hash algorithm, undefined references). Both run in parallel and their findings are merged.

## Reviewing Multiple Files

```bash
# All Python files in a directory
quorum review src/*.py

# All Rust files
quorum review src/*.rs

# Specific files
quorum review src/auth.py src/db.py src/api.py
```

## JSON Output

When piped or passed `--json`, quorum emits structured JSON grouped by file:

```bash
quorum review src/auth.py --json
```

```json
[
  {
    "file": "src/auth.py",
    "findings": [
      {
        "title": "Use of `eval()` is a code injection risk",
        "severity": "critical",
        "category": "security",
        "source": "local-ast",
        "line_start": 14,
        "calibrator_action": "confirmed",
        "similar_precedent": ["TP: eval in command builder (sim=0.87)"]
      }
    ]
  }
]
```

The `source` field tells you where each finding came from:
- `local-ast` -- tree-sitter pattern match (instant, deterministic)
- `{"llm": "gpt-5.4"}` -- language model judgment
- `{"linter": "ruff"}` -- external linter output

This makes quorum composable with `jq`:

```bash
# Count findings by severity
quorum review src/*.py --json | jq '[.[].findings[]] | group_by(.severity) | map({(.[0].severity): length}) | add'

# List only critical findings
quorum review src/*.py --json | jq '[.[].findings[] | select(.severity == "critical")] | .[].title'
```

## Supported Languages

| Language | Extensions | Local Patterns | External Linter |
|----------|-----------|---------------|-----------------|
| Rust | .rs | complexity, unsafe, unwrap | clippy |
| Python | .py | eval, secrets, SQL injection, mutable defaults, open() encoding, bare except | ruff |
| TypeScript | .ts .js .mjs .cjs | eval, innerHTML, secrets, any type, empty catch, sync-in-async | eslint |
| TSX/JSX | .tsx .jsx | same as TypeScript | eslint |
| YAML | .yaml .yml | HA automations, secrets, duplicate keys, Jinja2 | yamllint |
| Bash | .sh .bash .zsh | eval, curl\|bash, set -e, secrets, chmod 777 | shellcheck |
| Dockerfile | Dockerfile* | FROM latest, no USER, no HEALTHCHECK | hadolint |
| Other | * | LLM-only (no local patterns) | -- |

For files with no recognized extension, quorum skips AST analysis and sends the code directly to the LLM (if configured).

## Change-Scoped Review

Review only the parts of a file that changed:

```bash
git diff > changes.patch
quorum review src/auth.py --diff-file changes.patch
```

The hydration context is scoped to changed lines, and the LLM focuses on the delta.

## Deep Review (Agent Loop)

For thorough analysis, use multi-turn agent review:

```bash
quorum review src/auth.py --deep
```

The agent can read additional files, search the codebase, and follow call chains before producing findings. Slower but more thorough for complex code.

## Configuration

### Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `QUORUM_BASE_URL` | OpenAI-compatible endpoint | -- (local-only mode) |
| `QUORUM_API_KEY` | API key for LLM review | -- (local-only mode) |
| `QUORUM_MODEL` | Review model | gpt-5.4 |
| `QUORUM_REASONING_EFFORT` | Reasoning depth: none, low, medium, high | low |
| `QUORUM_ENSEMBLE_MODELS` | Comma-separated models for --ensemble | -- |
| `CONTEXT7_API_KEY` | Framework doc injection via Context7 | -- |

### Model Recommendations

| Scenario | Model | Calibrator | Speed |
|----------|-------|-----------|-------|
| Default | gpt-5.4 | auto | ~2s |
| Fast CI gate | gpt-5.3-codex | none | ~1s |
| Deep audit | gpt-5.2 | o3 | ~100s |
| Max coverage | --ensemble | o3 | ~100s |

### Calibration Model

Override the auto-calibration model:

```bash
# Use o3 for more nuanced triage
quorum review src/auth.py --calibration-model o3

# Disable auto-calibration for speed
quorum review src/auth.py --no-auto-calibrate
```

## Custom ast-grep Rules

Quorum runs [ast-grep](https://ast-grep.github.io/) rules from two locations:
- `rules/<language>/` in the project directory (ships with 10 bundled rules)
- `~/.quorum/rules/<language>/` for user-global rules

### Example: Adding a Custom Rule

Create a rule to flag `console.warn()` in TypeScript:

```bash
mkdir -p ~/.quorum/rules/typescript

cat > ~/.quorum/rules/typescript/no-console-warn.yml << 'EOF'
id: no-console-warn
language: TypeScript
severity: hint
message: "console.warn() left in code — use a proper logger"
rule:
  kind: call_expression
  pattern: console.warn($$$ARGS)
EOF
```

The rule is picked up automatically on the next review. No restart needed.

### Bundled Rules

| Rule | Language | What It Catches |
|------|----------|----------------|
| bare-catch | TypeScript | Empty catch blocks that swallow errors |
| sync-in-async | TypeScript | readFileSync etc. in async functions |
| as-any-cast | TypeScript | `x as any` type safety bypass |
| tautological-length | TypeScript | `.length >= 0` (always true) |
| open-no-encoding | Python | `open()` without encoding parameter |
| bare-except-pass | Python | `except: pass` catch-all swallowing |
| resource-no-context-manager | Python | `open()` outside `with` statement |
| float-zero-fallback | YAML | `float(0)` masking unavailable HA sensors |
| predictable-tmp | Bash | `/tmp/$var` symlink vulnerability |
| block-on-in-async | Rust | `block_on` inside async functions |

## The Feedback System

Quorum improves through use. When you record whether findings were correct, future reviews get better.

### Recording Feedback

Via the MCP server (in Claude Code):

```
Use the quorum feedback tool:
  file: src/auth.py
  finding: "Use of eval() is a code injection risk"
  verdict: tp
  reason: "Real issue, refactored to use json.loads"
```

Verdicts: `tp` (true positive), `fp` (false positive), `partial` (partly right), `wontfix` (real but not worth fixing).

### How Feedback Helps

1. **Pre-generation**: Past findings are injected into the LLM prompt as few-shot examples, teaching it what to flag and what to skip.

2. **Post-generation**: The calibrator suppresses known false positives and boosts confirmed true positives based on similarity to past verdicts.

3. **Pattern mining**: Analyzing confirmed true positives reveals structural patterns that become new AST rules.

### Feedback Store

Stored at `~/.quorum/feedback.jsonl`. Grows automatically through auto-calibration and manual verdicts.

## MCP Server (Claude Code Integration)

Add quorum to your Claude Code MCP configuration:

```json
{
  "mcpServers": {
    "quorum": {
      "command": "quorum",
      "args": ["serve"],
      "env": {
        "QUORUM_BASE_URL": "https://your-endpoint.com/v1",
        "QUORUM_API_KEY": "sk-..."
      }
    }
  }
}
```

Six tools are exposed:

| Tool | Purpose |
|------|---------|
| review | Review code for bugs, security, quality |
| chat | Ask questions about code |
| debug | Analyze error messages with code context |
| testgen | Generate tests for code |
| feedback | Record TP/FP verdicts on findings |
| catalog | Query the feedback store |

## Daemon Mode

For editor integration or CI pipelines that review many files:

```bash
# Start the daemon
quorum daemon --watch-dir /path/to/project

# Reviews are cached — unchanged files return instantly
quorum review src/auth.py --daemon
```

The daemon maintains a warm parse cache (256 entries, LRU) and invalidates on file change.

## Exit Codes

| Code | Meaning | Use In CI |
|------|---------|-----------|
| 0 | Clean (no findings or info-only) | Pass |
| 1 | Warnings (medium severity) | Warn |
| 2 | Critical (high/critical severity) | Fail |
| 3 | Tool error | Retry |

### CI Example

```bash
# Fail the build on critical findings
quorum review src/*.py
if [ $? -eq 2 ]; then
  echo "Critical issues found — blocking merge"
  exit 1
fi
```

## Troubleshooting

**No findings on a file with obvious issues**: Check that the file extension is recognized. Use `quorum review file.py --json` and check the `source` field — if only LLM findings appear, the AST parser may not support the language.

**"command not found" for a linter**: Quorum detects linters from config files but requires them in PATH. Install ruff/clippy/eslint as needed.

**Feedback not affecting results**: Run with stderr visible (`quorum review file.py 2>&1`) and look for "Loaded N feedback entries". If N is 0, check that `~/.quorum/feedback.jsonl` exists and is valid JSON-per-line.

**Too many complexity findings**: Complexity threshold defaults to 5. Functions with many branches will flag. These are informational — focus on security and correctness findings first.
