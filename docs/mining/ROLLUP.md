# Pattern Mining Roll-up — 2026-04-20

Model: gemini-2.5-pro | corpus: human-verified feedback since 2026-01-01
Cost: ~$0.81 (6 languages, 153K tokens)

## Candidate ast-grep rules (ranked by TP coverage × precision)

| Lang | Rule ID | Cluster | TPs | Prec | Cov | Score |
|---|---|---|---|---|---|---|
| python | `blocking-call-in-async` | Blocking Call in Async Function | 8 | 0.95 | 8 | 7.60 |
| javascript | `bind-in-remove-event-listener` | Incorrect listener removal with .bind() | 6 | 0.95 | 6 | 5.70 |
| yaml | `ha-jinja-loop-scoped-reassignment` | Jinja Loop Variable Scoping Bug | 6 | 0.95 | 6 | 5.70 |
| typescript | `ts-json-parse-as-type` | Unvalidated JSON Parse with Type Assertion | 8 | 0.95 | 5 | 4.75 |
| python | `fastapi-unconstrained-pagination` | Unconstrained Numeric Parameters in FastAPI | 6 | 0.85 | 5 | 4.25 |
| rust | `silent-error-conversion-on-fallible-op` | Silent Error Conversion on Fallible Operations | 4 | 0.95 | 4 | 3.80 |
| python | `db-connection-no-context-manager` | Database Connection Leak | 5 | 0.9 | 4 | 3.60 |
| typescript | `ts-unsafe-url-concat` | Unsafe URL Concatenation | 4 | 0.85 | 4 | 3.40 |
| yaml | `ha-input-text-potential-overflow` | input_text 255 Character Limit Overflow | 4 | 0.8 | 4 | 3.20 |
| rust | `ignored-io-result` | Ignoring Result of I/O Operations | 3 | 1.0 | 3 | 3.00 |
| python | `bare-except-with-logic` | Bare `except` with Logic | 3 | 0.9 | 3 | 2.70 |
| python | `flask-debug-true` | Flask Debug Mode Enabled | 3 | 1.0 | 2 | 2.00 |
| python | `f-string-in-db-execute` | SQL Injection via f-string | 2 | 1.0 | 2 | 2.00 |
| yaml | `ha-unsafe-dict-index-before-default` | Unsafe Dictionary/Attribute Access | 5 | 1.0 | 2 | 2.00 |
| bash | `unsafe-grep-variable` | Unsafe grep with variable pattern | 2 | 0.95 | 2 | 1.90 |
| bash | `toctou-lock-touch` | Non-atomic file-based locking (TOCTOU) | 2 | 0.9 | 2 | 1.80 |
| yaml | `ha-hardcoded-secret` | Hardcoded Secrets | 2 | 0.9 | 2 | 1.80 |
| bash | `non-portable-grep-p` | Non-portable grep -P | 1 | 1.0 | 1 | 1.00 |
| python | `flask-unsafe-form-access` | Unsafe Dictionary Access on Request Object | 2 | 1.0 | 1 | 1.00 |
| python | `numpy-load-allow-pickle-true` | Insecure `numpy.load` | 1 | 1.0 | 1 | 1.00 |
| python | `use-sys-exit` | Using `exit()` in Scripts | 1 | 1.0 | 1 | 1.00 |
| rust | `unwrap-on-duration-since` | Unwrap on Fallible Time Calculation | 1 | 1.0 | 1 | 1.00 |
| typescript | `ts-weak-crypto-random` | Weak Cryptography | 1 | 1.0 | 1 | 1.00 |
| yaml | `ha-unhandled-as-datetime-none` | as_datetime() without None check | 1 | 1.0 | 1 | 1.00 |
| typescript | `ts-test-fixed-wait` | Fixed-duration Wait in Test | 1 | 0.98 | 1 | 0.98 |
| bash | `manual-json-echo` | Manual JSON construction with echo | 1 | 0.95 | 1 | 0.95 |
| typescript | `ts-regex-word-merge` | Incorrect Regex-based Tokenization | 1 | 0.95 | 1 | 0.95 |
| bash | `silent-error-suppression-loop` | Silent error suppression in loop | 1 | 0.9 | 1 | 0.90 |
| typescript | `ts-split-without-filter` | split() without Filtering Empty Results | 1 | 0.7 | 1 | 0.70 |

## Clusters already covered by existing rules

- **bash** Predictable temporary file (2 TPs) → `predictable-tmp.yml`
- **javascript** Falsy `0` incorrectly handled by logical OR (1 TPs) → `nullish-coalescing-preferred.yml`
- **python** Assert for Runtime Check (2 TPs) → `assert-in-prod-code.yml`
- **python** Non-Thread-Safe Singleton (5 TPs) → `non-threadsafe-singleton.yml`
- **python** open() without encoding (4 TPs) → `open-no-encoding.yml`
- **python** Insecure MD5 Usage (2 TPs) → `md5-usage.yml`
- **python** Mutation During Iteration (1 TPs) → `mutation-during-iteration.yml`
- **python** Broad Exception Catch (0 TPs) → `broad-exception-catch.yml`
- **rust** Unsafe UTF-8 String Slicing (6 TPs) → `string-byte-slice.yml`
- **typescript** Silent Catch Block (15 TPs) → `bare-catch.yml`
- **typescript** Synchronous I/O in Async Function (4 TPs) → `sync-in-async.yml`
- **typescript** Tautological `length` Check (1 TPs) → `tautological-length.yml`
- **typescript** `console.log` Debug Artifact (2 TPs) → `console-log-artifact.yml`
- **yaml** float(0) Default Masks Sensor Unavailability (6 TPs) → `float-zero-fallback.yml`

## LLM-only clusters (no viable ast-grep signature)

- **bash** Missing resource cleanup on exit (1 TPs) — Detecting a leaked resource requires understanding its intended lifecycle. A syntactic rule to check for file creation (
- **bash** Miscellaneous semantic issues (5 TPs) — These issues require semantic understanding beyond syntactic patterns:
- **Unused variable**: Requires full scope and us
- **javascript** Unhandled async errors causing state corruption (3 TPs) — The bug is not the absence of `try/catch` itself, but the semantic consequence of an unhandled rejection on application 
- **javascript** High-context logic bugs (3 TPs) — These bugs are highly dependent on context not available in the AST. The `.trim()` bug requires knowing that whitespace 
- **python** Server-Side Request Forgery (SSRF) (4 TPs) — Detecting SSRF requires taint analysis to track data flow from an untrusted source (e.g., a web request parameter) to a 
- **python** Semantic Logic Bugs (15 TPs) — These findings relate to incorrect business logic, algorithmic flaws, or misinterpretation of data semantics (e.g., trea
- **python** Unused Code (Imports, Variables, Parameters) (8 TPs) — Detecting unused code requires scope analysis and tracking variable usage throughout a file or project. While this is a 
- **rust** Panics from Unchecked Zero/Edge Values (5 TPs) — This class of bugs involves logical errors related to numeric edge cases (e.g., division by zero, subtraction causing un
- **rust** Missing or Inadequate Input Validation (4 TPs) — This pattern concerns the absence of validation code at system boundaries (e.g., CLI arguments, config file parsing). Sy
- **rust** Concurrency and Race Conditions (3 TPs) — Concurrency bugs are defined by the interaction of multiple threads or processes over time. Their signature is not conta
- **typescript** Semantic Logic and Data Flow Errors (20 TPs) — These bugs are not in the syntax but in the program's logic, intent, or interaction between components. Detecting them r
- **typescript** Missing Input Validation (10 TPs) — This requires identifying a data source as 'untrusted' (e.g., a request body, query parameter) and tracing its flow to a
- **yaml** Parallel Read-Modify-Write Race Condition (4 TPs) — Detecting this requires identifying three components: 1) `mode: parallel`, 2) a service call that writes to an entity, a
- **yaml** Plausible but Incorrect `float` Default (3 TPs) — This is a semantic issue. The problem is not the use of `float(default)` itself, but that the chosen default value (e.g.
- **yaml** Unconditional Service Call Before Guard (2 TPs) — This pattern involves a service call that uses a template variable (e.g., `item: "{{ task }}"`) followed by a separate t
