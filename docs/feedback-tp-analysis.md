# Quorum Feedback Store: TP Pattern Analysis

**Date:** 2026-04-13
**Source:** `~/.quorum/feedback.jsonl` (4,463 entries)

## Overall Verdict Distribution

| Verdict | Count | % |
|---------|-------|---|
| tp | 1,666 | 37.3% |
| fp | 1,193 | 26.7% |
| wontfix | 890 | 19.9% |
| partial | 714 | 16.0% |

## TP Finding Groups (sorted by frequency)

Groups with 3+ occurrences are listed below. Each includes an AST-detectability assessment using:
- **HIGH** = Structurally detectable via AST pattern or ast-grep rule
- **MEDIUM** = Partially detectable; needs heuristics or context beyond a single node
- **LOW** = Requires semantic understanding, cross-function analysis, or LLM reasoning

---

### 1. [68x] Empty catch block / swallowed errors
- **Languages:** .ts(35), .py(30), .js(2), .rs(1)
- **Categories:** error-handling, reliability, observability
- **Pattern:** `catch {}` with no logging, `catch { // comment only }`, silent error suppression
- **Example:** "Both file-loading paths use bare catch {} blocks with only comments"
- **AST detectability: HIGH**
  - ast-grep rule: match `catch` blocks with empty body or body containing only comments
  - Already partially detected by quorum's TS analyzer; could add ast-grep rules for all languages
  - Rule: `catch ($e) { }` or try/except with only `pass`

### 2. [47x] Secrets/credentials exposure
- **Languages:** .py(19), .ts(12), .rs(11), .sh(2), .yaml(2), .js(1)
- **Categories:** security, privacy, information-disclosure
- **Pattern:** API keys in source, empty string fallbacks for keys, credential regex compiled per-call
- **Example:** "createAxAI silently falls back to an empty API key (process.env.KEY ?? '')"
- **AST detectability: MEDIUM**
  - Some patterns are HIGH (string literals matching key patterns, `?? ''` fallbacks for env vars)
  - Others require LLM (understanding what constitutes a "credential" in context)
  - Existing quorum AST patterns cover hardcoded strings; could add `?? ''` for env var fallbacks

### 3. [35x] Dead/unreachable/unused code
- **Languages:** .py(18), .yaml(6), .rs(5), .ts(5), .sh(1)
- **Categories:** code-quality, logic, maintainability
- **Pattern:** Unused imports, dead if/else branches, fetched-but-unused variables
- **Example:** "Dead code in _get_connect_url -- both branches return identical value"
- **AST detectability: MEDIUM**
  - Unused imports: HIGH (ast-grep can match import without usage)
  - Dead branches with identical returns: MEDIUM (needs comparison of branch bodies)
  - Unused variables after fetch: LOW (needs data-flow analysis)

### 4. [26x] Bare except / except:pass
- **Languages:** .py(26)
- **Categories:** error-handling, code-quality, maintainability
- **Pattern:** `except:` without exception type, `except: pass`
- **Example:** "Broad bare except blocks suppress real failures"
- **AST detectability: HIGH**
  - ast-grep rule: match `except` handler with no exception type specified
  - Already detected by quorum's Python analyzer
  - Could add severity boost based on feedback frequency

### 5. [21x] Hardcoded secrets/credentials
- **Languages:** .py(8), .yaml(6), .rs(5), .ts(2)
- **Categories:** security, reliability, portability
- **Pattern:** Hardcoded values where env vars or config expected, placeholder values left in code
- **Example:** "Delta T 30-day average is hardcoded placeholder 18.5"
- **AST detectability: MEDIUM**
  - String literal secrets (API keys, passwords): HIGH
  - Hardcoded numeric placeholders: LOW (needs domain knowledge)
  - Many of these are "hardcoded X should be configurable" -- requires LLM reasoning

### 6. [14x] Unsafe operation (general)
- **Languages:** .rs(6), .ts(5), .yaml(1), .py(1), .sh(1)
- **Categories:** correctness, reliability, security, type-safety
- **Pattern:** Unicode-unsafe truncation, unsafe dereferences, debug mode in production
- **Example:** "Unicode-unsafe truncation can panic on non-ASCII tool output"
- **AST detectability: MEDIUM**
  - String slice `[..N]` in Rust: HIGH (ast-grep pattern for byte-index slicing)
  - Flask `debug=True`: HIGH (ast-grep: `app.run(debug=True)`)
  - Jinja2 unsafe dereference: MEDIUM (needs template context understanding)

### 7. [10x] Python open() missing encoding
- **Languages:** .py(9), .rs(1)
- **Categories:** code_quality, reliability, portability
- **Pattern:** `open(path)` without `encoding='utf-8'` parameter in text mode
- **Example:** "open(config_path) relies on the process default encoding"
- **AST detectability: HIGH**
  - ast-grep rule: match `open()` calls without `encoding` keyword argument
  - Already detected by quorum's Python AST analyzer
  - Feedback confirms this is a reliable, actionable pattern

### 8. [9x] Boundary/bounds checking
- **Languages:** .rs(5), .yaml(3), .ts(1)
- **Categories:** logic, robustness, input-validation
- **Pattern:** Out-of-bounds slice, unclamped indices, inconsistent path validation
- **Example:** "Out-of-bounds slice panic when start_line exceeds file length"
- **AST detectability: LOW**
  - Some specific sub-patterns are MEDIUM (Rust `[..N]` on strings)
  - Most require understanding of valid index ranges -- needs LLM or symbolic analysis

### 9. [7x] Tautological .length >= 0 check
- **Languages:** .ts(6), .rs(1)
- **Categories:** correctness
- **Pattern:** `array.length >= 0` which is always true (likely meant `> 0`)
- **Example:** "deduplicated.length >= 0 is always true, synthesis always runs"
- **AST detectability: HIGH**
  - ast-grep rule: match `$x.length >= 0` or `$x.length > -1`
  - Already detected by quorum's TS analyzer
  - Feedback confirms high TP rate -- good candidate for severity boost

### 10. [6x] Sync I/O in async function (readFileSync)
- **Languages:** .ts(6)
- **Categories:** concurrency
- **Pattern:** `readFileSync` / `writeFileSync` inside `async function`
- **Example:** "readFileSync blocks the event loop in async function"
- **AST detectability: HIGH**
  - ast-grep rule: match `readFileSync` or `writeFileSync` inside `async function`
  - Already detected by quorum's TS "sync-in-async" pattern
  - Could expand to cover `execSync`, `spawnSync` as well

### 11. [6x] Integer overflow
- **Languages:** .rs(5), .py(1)
- **Categories:** correctness, reliability, overflow
- **Pattern:** Unchecked arithmetic that can overflow/panic, especially with edge values
- **Example:** "threshold*2 overflows when threshold=0" / "Relative-time branch can overflow"
- **AST detectability: MEDIUM**
  - Rust: can flag `*`, `+`, `-` on integer types without `checked_*` alternatives
  - Specific pattern: multiplication in bounds calculations
  - General overflow detection requires value-range analysis -- LOW

### 12. [4x] SQL injection
- **Languages:** .py(4)
- **Categories:** security
- **Pattern:** f-string or format() interpolation into SQL queries
- **Example:** "SQL injection in login query via username interpolation"
- **AST detectability: HIGH**
  - ast-grep rule: match f-string inside `cursor.execute()` or `.query()` calls
  - Already detected by quorum's Python AST analyzer
  - High confidence TP pattern

### 13. [4x] Resource/file descriptor leak
- **Languages:** .py(3), .rs(1)
- **Categories:** resource-management
- **Pattern:** `urlopen`/connections not in `with` block, missing cleanup on exception path
- **Example:** "urlopen response not used as context manager"
- **AST detectability: MEDIUM**
  - `urlopen()` / `open()` not in `with`: HIGH (ast-grep pattern)
  - Missing cleanup on exception path: LOW (needs control-flow analysis)

### 14. [4x] Missing error handling (general)
- **Languages:** .py(2), .ts(1), .rs(1)
- **Categories:** error-handling
- **Pattern:** fs operations, JSON parsing, inference calls without try/catch
- **Example:** "Neither fs.mkdirSync nor fs.appendFileSync is wrapped in error handling"
- **AST detectability: LOW**
  - Too broad to define structurally -- any call "should" have error handling
  - Specific sub-patterns (fs.* without try/catch) could be MEDIUM

### 15. [3x] TypeScript `any` type usage
- **Languages:** .ts(3)
- **Categories:** type-safety, code_quality
- **Pattern:** `as any` casting, double `as any` to bypass type system
- **Example:** "Type safety is bypassed with double as any casting around SDK call"
- **AST detectability: HIGH**
  - ast-grep rule: match `as any` expressions
  - Already detected by quorum's TS analyzer
  - Could boost severity for `as any as any` (double cast) pattern

### 16. [3x] Unbounded query parameters
- **Languages:** .ts(3)
- **Categories:** security
- **Pattern:** Numeric parameters accepted without max bound, enabling DoS
- **Example:** "Unbounded limit query parameter can trigger expensive searches"
- **AST detectability: LOW**
  - Requires understanding that a parameter controls resource consumption
  - No structural pattern distinguishes "limit" from any other numeric param

### 17. [3x] Test expects behavior production code doesn't have
- **Languages:** .rs(3)
- **Categories:** test
- **Pattern:** Tests assert values that differ from actual production behavior
- **Example:** "Test expects a redacted Authorization header that production code never sends"
- **AST detectability: LOW**
  - Requires cross-file comparison of test assertions vs production code
  - Purely semantic analysis needed

---

## Notable 2x patterns (not yet at threshold but trending)

| Pattern | Count | Languages | AST? |
|---------|-------|-----------|------|
| Flask debug mode enabled | 2 | .py | HIGH |
| Command injection via eval/shell | 2 | .sh, .py | HIGH |
| Cyclomatic complexity (various functions) | ~30 total | .rs | HIGH (already detected) |
| Pagination without bounds | 2 | .py | MEDIUM |
| Error logging drops stack trace | 2 | .py | MEDIUM |
| Role casting without validation | 2 | .ts | MEDIUM |
| Date sorted lexicographically | 2 | .py | LOW |
| Unbounded prompt construction | 2 | .py | LOW |

---

## Summary: Best Candidates for New AST/ast-grep Rules

### Already detected but should boost severity (confirmed by feedback):
1. Empty catch blocks (68 TPs) -- highest-volume TP, boost priority
2. Bare except:pass (26 TPs) -- Python-specific, very reliable
3. open() missing encoding (10 TPs) -- confirmed actionable
4. .length >= 0 tautology (7 TPs) -- always a bug
5. readFileSync in async (6 TPs) -- reliable pattern

### New rules to add:
1. **`process.env.X ?? ''` empty-string fallback for credentials** -- .ts, HIGH detectability, captures silent auth failures (part of 47x secrets group)
2. **Rust byte-index string slicing `&s[..N]`** -- .rs, HIGH detectability, captures Unicode panics (part of 14x unsafe + 9x bounds groups)
3. **Flask `app.run(debug=True)`** -- .py, HIGH detectability, 2+ TPs
4. **`as any as any` double-cast** -- .ts, HIGH detectability, stricter variant of existing rule
5. **f-string in `cursor.execute()`** -- .py, HIGH detectability, SQL injection (4 TPs)
6. **`urlopen()` / `HTTPConnection` not in `with` block** -- .py, HIGH detectability, resource leak
7. **Integer multiplication in array/slice bounds without checked_mul** -- .rs, MEDIUM detectability

### Patterns that need LLM (not AST-automatable):
- Cross-module data flow gaps (biggest weakness per quality feedback)
- Hardcoded domain-specific placeholders (e.g., magic numbers)
- Test-vs-production behavioral mismatches
- Unbounded resource consumption parameters
- Dead code from identical branch bodies
