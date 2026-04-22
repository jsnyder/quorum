# How Quorum Works

Quorum reviews code by combining three kinds of analysis: structural pattern matching against the syntax tree, judgment calls from a large language model, and output from conventional linters. It merges these into a single list of findings, then sharpens the list using a store of past human verdicts.

## The Review Pipeline

A review proceeds in six stages. Each stage refines the signal from the last.

```
                        source file
                            |
                      1. PARSE (tree-sitter)
                            |
                   +--------+--------+
                   |        |        |
              2. AST    3. LLM   4. LINTERS
              patterns   review   (external)
                   |        |        |
                   +--------+--------+
                            |
                      5. MERGE + DEDUP
                            |
                      6. CALIBRATE
                            |
                       findings out
```

### 1. Parse

Tree-sitter parses the source file into a concrete syntax tree. Quorum detects the language from the file extension and selects the appropriate grammar.

Supported grammars: Rust, Python, TypeScript (also used for JavaScript), YAML, Bash, Dockerfile. Files with unrecognized extensions skip AST analysis and go straight to the LLM.

In daemon mode, parsed trees are cached in a 256-entry LRU keyed by content hash. A changed file invalidates its cache entry automatically.

### 2. AST Analysis

Two passes run over the syntax tree:

**Complexity analysis** walks every function node, counts decision points (if, match, for, while, &&, ||), and flags functions that exceed a threshold.

**Insecure pattern detection** walks every node looking for language-specific anti-patterns. Each language has its own scanner:

| Language | Patterns |
|----------|----------|
| Rust | `unsafe` blocks, `.unwrap()` outside tests |
| Python | `eval`/`exec`, SQL injection via f-strings, `debug=True`, hardcoded secrets, mutable defaults, bare `except: pass`, `open()` without encoding, blocking `.result()` in async, mutating collections while iterating |
| TypeScript | `eval`, `innerHTML`, `document.write`, hardcoded secrets, `any` type, non-null assertions, empty catch blocks, sync APIs in async functions, tautological `.length >= 0` |
| YAML | Home Assistant automation patterns, hardcoded secrets, duplicate keys, ESPHome, Jinja2 |
| Bash | `eval`, `curl\|bash`, missing `set -e`, hardcoded secrets, `chmod 777`, missing shebang |
| Dockerfile | `FROM :latest`, missing `USER`, missing `HEALTHCHECK`, secrets in `ENV`, `ADD` vs `COPY` |

Every finding carries `source: LocalAst` so downstream stages can distinguish it from LLM or linter output.

### 3. LLM Review

If an API key is configured, quorum sends the code to a language model for a cold read. The prompt is built in layers:

```
 "Review the following {language} code from {path}..."
 ┌─────────────────────────────────────────────┐
 │  Hydration context (if parsed)              │
 │  - Signatures of called functions           │
 │  - Type definitions used in the code        │
 │  - Functions that call into this code       │
 ├─────────────────────────────────────────────┤
 │  Framework docs (if Context7 available)     │
 ├─────────────────────────────────────────────┤
 │  Historical review findings                 │
 │  - Up to 3 past human verdicts on similar   │
 │    code (mix of TP and FP examples)         │
 │  - "Do NOT limit your review to these"      │
 ├─────────────────────────────────────────────┤
 │  ## Code                                    │
 │  ```{language}                              │
 │  {redacted source}                          │
 │  ```                                        │
 └─────────────────────────────────────────────┘
```

**Hydration** gives the model what a human reviewer gets from their IDE: the full signatures of functions being called, the definitions of types being used, and a list of callers that would break if this code changes. This is context completion, not priming — the model decides what matters.

**Secret redaction** strips API keys, passwords, and tokens from the code before it leaves the machine. Seven regex patterns run against the source, the hydration context, and any auto-calibration output. Redaction is always on.

**Few-shot feedback injection** queries the feedback store for past findings on similar code. It selects up to three precedents — a mix of true positives and false positives — and injects them into the prompt. True positives teach the model what the team cares about. False positives teach it what not to flag. An explicit instruction prevents the model from anchoring on the examples.

The model returns a JSON array of findings, each with title, description, severity, category, and line range. A parser recovers from markdown fences, control characters, and malformed JSON that reasoning models sometimes emit.

### 4. Linters

Quorum detects and runs external linters by scanning the project for configuration files:

| Linter | Detection | Invocation |
|--------|-----------|------------|
| ruff | `ruff.toml` or `[tool.ruff]` in pyproject.toml | `ruff check --output-format=json` |
| clippy | `Cargo.toml` present | `cargo clippy --message-format=json` |
| eslint | `.eslintrc.*` or `eslint.config.*` | `eslint --format=json` |
| yamllint | `.yamllint*` present | `yamllint -f parsable` |
| shellcheck | `.sh` files in project root | `shellcheck --format=json1` |
| hadolint | `.hadolint.*` or `Dockerfile` present | `hadolint --format tty` |
| ast-grep | `rules/<lang>/*.yml` files present | `ast-grep scan --json=compact -r <rule>` |

Each linter's output is normalized into the same `Finding` struct, tagged with `source: Linter("name")`. Linter exit code 1 (findings exist) is normal; exit code 2+ with empty output is an error.

**ast-grep** deserves special mention. It runs user-extensible YAML rules that match structural patterns in the syntax tree. Ten rules ship with quorum. Users add their own by dropping `.yml` files into `~/.quorum/rules/<language>/`. Both the project-local `rules/` directory and the user-global directory are scanned.

### 5. Merge and Deduplicate

Findings from all sources are flattened into a single list, then deduplicated. For each candidate finding, quorum checks whether a sufficiently similar finding already exists in the merged list. Similarity is computed from title word overlap and line range overlap.

When two findings match:
- The higher severity wins.
- The line range widens to cover both.
- Evidence lists are combined (without duplicates).
- The finding that arrived first keeps its source tag.

The merged list is sorted by severity (descending), then by line number.

### 6. Calibrate

The calibrator adjusts findings using a store of past human verdicts. The store lives at `~/.quorum/feedback.jsonl` and grows over time as users record whether findings were true positives, false positives, or not worth fixing.

For each merged finding, the calibrator retrieves the most similar past verdicts. Two retrieval methods are available:

**Semantic retrieval** (preferred) embeds finding titles with BGE small (bge-small-en-v1.5) and searches by cosine similarity. The threshold is 0.75 — high enough to avoid false associations, since BGE embeddings cluster tightly in positive space.

**Jaccard fallback** computes word overlap between finding titles. The threshold is 0.5. This path activates when the embedding model is unavailable.

Each retrieved verdict carries a weight:

| Provenance | Weight | Rationale |
|------------|--------|-----------|
| post_fix (user fixed the issue) | 1.5x | Strongest signal — the user acted on it |
| human (manual verdict) | 1.0x | Direct judgment |
| auto_calibrate (legacy LLM triage) | 0.5x, capped at 1.0 total | Historical entries from a removed second-pass triage feature; weight clamp prevents them from dominating live calibration |

Weights decay exponentially with age. The half-life is 83 days — long enough that a three-month-old verdict still carries 47% of its original weight, because code patterns change slowly.

**Suppression**: If false-positive weight exceeds 1.5 and is more than double the true-positive weight, the finding is removed. This requires human corroboration; auto-calibrate entries alone cannot suppress a finding.

**Boosting**: If true-positive weight exceeds 1.5 and is more than double the false-positive weight, the finding's severity is promoted one level (e.g., medium to high). The finding is annotated with the precedents that informed the decision.

## The Feedback Loop

Quorum improves through use. The feedback store serves three purposes:

1. **Pre-generation**: The best precedents are injected into the LLM prompt as few-shot examples, preventing false positives before they are generated.

2. **Post-generation**: The calibrator suppresses known false positives and boosts known true positives based on similarity to past verdicts.

3. **Pattern mining**: Aggregate analysis of confirmed true positives reveals structural patterns that can be implemented as AST rules — instant, free, and deterministic. The five AST patterns added in v0.9.0 were discovered this way.

```
  user reviews findings
        |
        v
  records verdict (TP/FP/partial/wontfix)
        |
        v
  feedback store grows
        |
   +----+----+
   |    |    |
   v    v    v
 few-  cali-  AST
 shot  brator pattern
 in    adjusts mining
 prompt sev.
```

## Deployment Modes

**CLI** (`quorum review file.py`): Parse, analyze, review, output, exit. Local-only analysis takes 7ms. With an LLM, 15-20 seconds. Exit codes reflect the highest severity found: 0 for clean, 1 for warnings, 2 for critical issues, 3 for tool errors.

**MCP server** (`quorum serve`): A persistent process that communicates over stdio using JSON-RPC. Exposes six tools — review, chat, debug, testgen, feedback, catalog — for use by Claude Code or other AI agents.

**Daemon** (`quorum daemon --watch-dir .`): Watches the filesystem, maintains a warm parse cache, and serves reviews over HTTP. Subsequent reviews of unchanged files return instantly from cache.

## Finding Anatomy

Every finding, regardless of source, carries the same structure:

```
Finding {
    title:              "Empty catch block silently swallows errors"
    description:        "An empty catch block hides failures..."
    severity:           Medium          // Info | Low | Medium | High | Critical
    category:           "reliability"
    source:             LocalAst        // or Llm("gpt-5.4") or Linter("eslint")
    line_start:         14
    line_end:           16
    evidence:           ["catch (e) { }"]
    calibrator_action:  Confirmed       // or Disputed, Adjusted, Added, None
    similar_precedent:  ["TP: Empty catch block... (sim=0.92)"]
}
```

The `source` tag lets users and downstream tools distinguish between instant local analysis, model judgment, and linter output. The `calibrator_action` and `similar_precedent` fields show how feedback influenced the finding.

## Output

When stdout is a terminal, quorum renders findings with colored severity badges, file paths, line ranges, and descriptions. When piped, it emits JSON grouped by file:

```json
[
  {
    "file": "src/auth.py",
    "findings": [ ... ]
  }
]
```

This makes quorum composable with `jq`, CI systems, and feedback recording scripts.
