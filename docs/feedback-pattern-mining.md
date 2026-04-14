# Feedback Corpus Pattern Mining: AST Rule Gap Analysis

**Date:** 2026-04-13
**Corpus:** ~/.quorum/feedback.jsonl (4,463 entries)
**Method:** Contrastive rule mining — cluster TPs by structural shape, identify gaps vs existing detection

## Corpus Overview

| Verdict | Count | % |
|---------|-------|---|
| tp | 1,666 | 37.3% |
| fp | 1,193 | 26.7% |
| wontfix | 890 | 19.9% |
| partial | 714 | 16.0% |

### TPs by Language

| Extension | TP Count | % of TPs |
|-----------|----------|----------|
| .py | 463 | 27.8% |
| .rs | 380 | 22.8% |
| .ts | 373 | 22.4% |
| .yaml | 273 | 16.4% |
| .js | 115 | 6.9% |
| .sh | 48 | 2.9% |
| .sql | 6 | 0.4% |
| .tsx | 5 | 0.3% |

## Existing Detection Coverage

### Tree-sitter AST (Rust code in `src/analysis.rs`)

| Language | Patterns | Count |
|----------|----------|-------|
| Rust | unsafe block, .unwrap() (skips tests) | 2 |
| Python | eval/exec, debug=True, bind 0.0.0.0, SQL injection (f-string + .format), open() no encoding, hardcoded secrets, mutable defaults | 7 |
| TypeScript | eval, innerHTML/outerHTML XSS, empty catch, sync-in-async, hardcoded secrets, .length>=0 | 6 |
| Bash | no shebang, no set -e, eval, chmod 777, curl\|bash, hardcoded secrets | 6 |
| Dockerfile | FROM latest, no HEALTHCHECK, no USER, multiple CMD/ENTRYPOINT | 4 |
| Terraform | hardcoded secrets, provider version pins, required_providers | 3 |
| YAML | HA automation patterns, secrets, duplicate keys | 3 |

### ast-grep Rules (10 existing in `rules/`)

| Rule | Language | Feedback TPs |
|------|----------|-------------|
| `bare-except-pass` | Python | ~8 |
| `open-no-encoding` | Python | 6 |
| `resource-no-context-manager` | Python | ~5 |
| `block-on-in-async` | Rust | ~3 |
| `as-any-cast` | TypeScript | 4 |
| `bare-catch` | TypeScript | 19 |
| `sync-in-async` | TypeScript | 13 |
| `tautological-length` | TypeScript | 2 |
| `predictable-tmp` | Bash | ~5 |
| `float-zero-fallback` | YAML | ~8 |

## TP Pattern Clusters (keyword-matched, freq >= 3)

| Cluster | TPs | Already Covered? | AST-Detectable? |
|---------|-----|-----------------|-----------------|
| missing_validation | 97 | No | Weak — too varied |
| hardcoded_secret | 93 | Partially (AST code) | Supplement possible |
| unwrap_panic | 58 | Yes (AST code) | Already done |
| unused_variable | 52 | No | Moderate — lint overlap |
| encoding_issue | 51 | Yes (AST + ast-grep) | Already done |
| deprecated_api | 50 | No | Weak — needs semantic |
| yaml_issue | 38 | Partially | Some detectable |
| complexity | 36 | Yes (cyclomatic) | Already done |
| async_issue | 34 | Partially | New patterns possible |
| logging_debug | 30 | No | **Strong candidate** |
| null_undef | 21 | No | **Strong candidate** |
| regex_issue | 21 | No | Moderate |
| type_safety | 18 | Partially (as-any) | Some detectable |
| eval_exec | 14 | Yes (AST code) | Already done |
| integer_overflow | 14 | No | Weak — needs flow |
| resource_leak | 13 | Partially | **New patterns** |
| temp_file | 11 | Yes (ast-grep) | Already done |
| path_traversal | 10 | No | Weak — needs flow |
| off_by_one | 7 | No | Some detectable |
| mutable_default | 7 | Yes (AST code) | Already done |
| injection_sql | 6 | Yes (AST code) | Already done |

**852 TPs (51%) unmatched** — mostly higher-level logic bugs requiring LLM reasoning.

---

## NEW RULE CANDIDATES

Rules ordered by estimated value (TP frequency x detection precision).

### Tier 1: High Value (10+ feedback TPs, clean structural shape)

#### 1. `python/subprocess-shell-true` — subprocess with shell=True
- **Feedback TPs:** ~8 (within injection_cmd cluster)
- **Shape:** `call_expression` with `keyword_argument` `shell=True`
- **Why:** `subprocess.run(cmd, shell=True)` is command injection if cmd has user input. Not in current AST code.
- **FP guard:** Skip when cmd is a string literal (not variable).

#### 2. `javascript/console-log-artifact` — console.log left in production code
- **Feedback TPs:** 30
- **Shape:** `call_expression` matching `console.log($$$)`
- **Why:** Repeatedly flagged as debug artifact. Simple structural match.
- **FP guard:** Skip in test files, skip `console.error`/`console.warn` (those are intentional).
- **Scope:** .js files only (TypeScript projects usually have lint rules for this).

#### 3. `typescript/nullish-coalescing-preferred` — || where ?? is safer
- **Feedback TPs:** ~10 (within null_undef cluster)
- **Shape:** `binary_expression` with `||` where RHS is a default value
- **Why:** `x || defaultValue` treats `0`, `""`, `false` as missing. `??` is usually correct.
- **FP guard:** Only flag when LHS is a variable/member access and RHS is a literal.

#### 4. `python/urlopen-no-context-manager` — urlopen() outside with
- **Feedback TPs:** ~5 (within resource_leak cluster)
- **Shape:** `assignment` with `call` to `urlopen` not inside `with_statement`
- **Why:** Same pattern as existing `resource-no-context-manager` but for urllib.

#### 5. `python/subprocess-no-check` — subprocess.run without check=True
- **Feedback TPs:** ~5
- **Shape:** `call` to `subprocess.run` without `check=True` keyword
- **Why:** Silently ignores non-zero exit codes. Common oversight.
- **FP guard:** Skip when return value is assigned (caller may check manually).

### Tier 2: Medium Value (5-10 TPs or moderate precision)

#### 6. `python/re-compile-in-loop` — regex compiled every iteration
- **Feedback TPs:** ~6 (within regex_issue cluster)
- **Shape:** `call` to `re.sub`/`re.match`/`re.search`/`re.findall` inside `for_statement` or `while_statement`
- **Why:** Performance antipattern — should pre-compile with `re.compile()`.
- **FP guard:** Skip when pattern is `re.compile()` itself.

#### 7. `javascript/bind-in-add-event-listener` — .bind() prevents removeEventListener
- **Feedback TPs:** 4+
- **Shape:** `call_expression` `.addEventListener($EVENT, $HANDLER.bind($THIS))`
- **Why:** Creates new function each time — can never be removed. 4+ confirmed TPs.

#### 8. `typescript/non-null-assertion` — overuse of x!
- **Feedback TPs:** ~5 (within type_safety cluster)
- **Shape:** `non_null_expression` kind
- **Why:** `!` assertions bypass TypeScript safety. High noise risk — scope to production code.
- **FP guard:** Skip in test files.

#### 9. `rust/string-byte-slice` — &str[..n] can panic on UTF-8 boundary
- **Feedback TPs:** ~5 (within off_by_one cluster)
- **Shape:** `index_expression` on string with `range_expression`
- **Why:** `&s[..n]` panics if n is not a char boundary. Use `.chars().take(n)` instead.
- **FP guard:** Hard to distinguish str from [u8] without type info — flag as hint only.

#### 10. `yaml/jinja-loop-variable-scoping` — set x = x + 1 in for loop
- **Feedback TPs:** ~6 (within yaml_issue cluster)
- **Shape:** Jinja `{% set %}` inside `{% for %}` with compound assignment
- **Why:** Jinja2 scoping means the outer variable is not modified. Already in CLAUDE.md future work.
- **Note:** Regex-based since YAML values are opaque to tree-sitter.

### Tier 3: Low Value (< 5 TPs or requires semantic analysis)

#### 11. `python/assert-in-prod-code` — assert statement outside test files
- **Feedback TPs:** ~4
- **Shape:** `assert_statement` not in test file
- **Why:** Python -O strips asserts. Not reliable for validation.
- **FP guard:** Skip files matching `test_*.py` or `*_test.py`.

#### 12. `rust/expect-empty-message` — .expect("") with useless message
- **Feedback TPs:** ~3
- **Shape:** `call_expression` `.expect("")` or `.expect("error")`
- **Why:** Generic messages make debugging hard.

#### 13. `typescript/promise-constructor-async` — new Promise with async executor
- **Feedback TPs:** ~3
- **Shape:** `new_expression` `Promise` with `async` arrow function
- **Why:** Async executor in Promise constructor swallows errors.

#### 14. `python/broad-exception-catch` — except Exception (not just bare except)
- **Feedback TPs:** ~8 (overlap with error_handling cluster)
- **Shape:** `except_clause` with `Exception` identifier
- **Why:** Catches too broadly. Complement to existing `bare-except-pass`.
- **FP guard:** Only flag when body is trivial (pass/continue/return None).

#### 15. `bash/unquoted-variable` — $VAR without quotes in command arguments
- **Feedback TPs:** ~5
- **Shape:** `simple_expansion` not inside `string` (i.e., unquoted)
- **Why:** Word splitting and globbing on unquoted variables.
- **FP guard:** Skip inside `[[ ]]` where splitting doesn't happen.

---

## Patterns NOT Suitable for AST Rules (LLM-only)

These clusters appeared frequently in TPs but require semantic reasoning:

| Pattern | TPs | Why LLM-only |
|---------|-----|-------------|
| Missing validation (input boundary) | 97 | Requires understanding what "valid" means in context |
| Deprecated API usage | 50 | Requires knowing which API versions are current |
| Unused variables (semantic) | 52 | Linters catch simple cases; feedback is about semantic dead code |
| Race conditions | ~10 | Requires understanding concurrent execution paths |
| Path traversal | 10 | Requires tracking data flow from user input to file ops |
| Integer overflow | 14 | Requires type and range analysis |
| Cross-module inconsistency | ~100+ | Requires multi-file analysis |

---

## Implementation Priority Matrix

```
                  High Precision
                       |
    [T1: subprocess]   |  [T1: console.log]
    [T1: urlopen ctx]  |  [T2: bind-event]
    [T2: re-in-loop]   |  [T2: non-null !]
                       |
  Low TP ─────────────+──────────────── High TP
                       |
    [T3: expect ""]    |  [T1: nullish ??]
    [T3: promise async]|  [T3: broad except]
    [T3: assert prod]  |
                       |
                  Low Precision
```

**Recommended implementation order:**
1. `python/subprocess-shell-true` — highest signal, low FP risk
2. `javascript/console-log-artifact` — highest TP count, simple rule
3. `typescript/nullish-coalescing-preferred` — broad applicability
4. `python/urlopen-no-context-manager` — extends proven pattern
5. `python/subprocess-no-check` — complements #1
6. `python/re-compile-in-loop` — performance catch
7. `javascript/bind-in-add-event-listener` — JS-specific footgun
8. `rust/string-byte-slice` — Rust-specific footgun
9. `yaml/jinja-loop-variable-scoping` — addresses known gap
10. `python/broad-exception-catch` — extends bare-except rule

---

## Fixture Strategy

Each rule gets a test fixture following the Semgrep-style golden corpus:

```
rules/<lang>/<rule-id>.yml         # ast-grep rule
rules/<lang>/tests/<rule-id>/      # test directory
  positive.{ext}                    # should match (TP examples)
  negative.{ext}                    # should NOT match (FP guards)
```

TP examples from feedback.jsonl are minimized to the smallest structural shape.
FP examples are derived from same-category fp/wontfix feedback entries.
