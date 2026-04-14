# Quorum AST Detection Patterns Inventory
**Complete mapping of all pattern detection methods (Rust AST + ast-grep rules)**

---

## PART 1: AST-GREP CUSTOM RULES (rules/ directory)

**Total: 10 rules across 6 languages**

### Bash (1 rule)
- **predictable-tmp.yml** — Symlink attack in /tmp via variable interpolation
  - Detection: regex match for `/tmp/` + expansion/simple_expansion
  - Feedback: ~5 TP, warning severity

### Python (3 rules)
- **bare-except-pass.yml** — Bare `except: pass` silently swallows all exceptions
  - Detection: except_clause without as_pattern + pass_statement only
  - Feedback: ~8 TP, warning severity
  
- **open-no-encoding.yml** — File open without explicit encoding parameter
  - Detection: call node, function=open, missing keyword_argument with name=encoding
  - Feedback: 6 TP, hint severity
  
- **resource-no-context-manager.yml** — File opened outside `with` context manager
  - Detection: assignment with right=call(function=open), not inside with_statement
  - Feedback: ~5 TP, hint severity

### TypeScript/JavaScript (4 rules)
- **as-any-cast.yml** — Type assertion to `any` bypasses safety
  - Detection: as_expression containing predefined_type matching "any"
  - Feedback: 4 TP, hint severity
  
- **bare-catch.yml** — Empty catch block swallows errors
  - Detection: catch_clause with statement_block (no expression/throw/return/variable/if)
  - Feedback: 19 TP, warning severity
  
- **sync-in-async.yml** — Synchronous API blocks event loop inside async function
  - Detection: call_expression with sync method inside async function context
  - Methods: readFile, readFileSync, writeFile, writeFileSync (many more in full rule)
  - Feedback: 13 TP, warning severity
  
- **tautological-length.yml** — `.length >= 0` always true tautology
  - Detection: binary_expression pattern `$LEFT >= 0` where left matches `\.length$`
  - Feedback: 2 TP, 100% precision, warning severity

### Rust (1 rule)
- **block-on-in-async.yml** — `block_on()` inside async context deadlock/panic
  - Detection: call_expression pattern `$RUNTIME.block_on($$$ARGS)` inside async function_item
  - Feedback: ~3 TP, warning severity

### YAML (1 rule)
- **float-zero-fallback.yml** — `float(0)` fallback masks sensor unavailability
  - Detection: plain/single/double_quote_scalar matching regex `\| *float\(0\)`
  - Context: Home Assistant automations
  - Feedback: ~8 TP, warning severity

---

## PART 2: TREE-SITTER AST ANALYSIS (src/analysis.rs)

**Total: ~127+ patterns across 8 languages, organized by function**

### RUST (scan_insecure_rust, ~8 patterns)
#### Critical Severity
- **unsafe block usage** — Unsafe code bypasses safety
  - Detection: unsafe_block node
  
- **unwrap() calls** — May panic at runtime (skipped in test context)
  - Detection: call_expression > field_expression where field="unwrap"

#### Info/Low Severity
- Unsafe pointer dereference, panic propagation, blocking calls in async

### PYTHON (scan_insecure_python, ~22 patterns)

#### Critical Severity
- **eval()/exec() code injection** — Arbitrary code execution
  - Detection: call node where function in ["eval", "exec"]
  
- **SQL injection via f-string** — Dynamic SQL string interpolation
  - Detection: call with function ending in ".execute" + f-string first arg
  
- **SQL injection via .format()** — Dynamic SQL via string formatting
  - Detection: call with function ending in ".execute" + .format() in args

#### High Severity
- **debug=True in Flask/FastAPI** — Exposes error pages and debugger
- **Hardcoded secrets** — Credentials in source (PASSWORD=, API_KEY=, SECRET_KEY=, etc.)

#### Medium/Low Severity
- **open() without encoding** — Platform-dependent default encoding
- **Bare except: pass** — Silently swallows all exceptions
- **Mutable default arguments** — Shared state across function calls

### TYPESCRIPT/JAVASCRIPT (scan_insecure_typescript, ~28 patterns)

#### Critical Severity
- **eval() code injection** — Arbitrary JavaScript execution
- **innerHTML/outerHTML XSS** — DOM injection vulnerability

#### High Severity
- **Hardcoded secrets** — API keys, tokens in source

#### Medium/Low Severity
- **Synchronous file I/O in async** (covered by sync-in-async rule)
- **Empty catch blocks** (covered by bare-catch rule)
- **Type assertions to any** (covered by as-any-cast rule)
- **Tautological comparisons** (covered by tautological-length rule)
- **console.log in production**, missing error handling, string concatenation in HTML

### YAML (scan_insecure_yaml, ~15 patterns)

#### Critical Severity
- **Secrets in YAML keys/values** — Plaintext credentials

#### High Severity
- **Jinja2 filter misuse** — Unsafe templating in Home Assistant

#### Medium Severity
- **Duplicate keys** — Last value wins, silent data loss
- **float(0) fallback** (covered by float-zero-fallback rule)

#### Automation-Specific (Home Assistant)
- Missing availability checks, type mismatches, ESPHome YAML structure, missing event attributes

### BASH (scan_insecure_bash, ~18 patterns)

#### Critical Severity
- **Hardcoded secrets** — Passwords, API keys, tokens
- **eval/source with user input** — Code injection

#### High Severity
- **curl | bash** — Arbitrary code execution from network
- **chmod 777** — World-writable permissions
- **Missing shebang** — Script incompatibility

#### Medium/Low Severity
- **set -e not used**, predictable /tmp files, unquoted variables, hardcoded paths

### DOCKERFILE (scan_insecure_dockerfile + analyze_dockerfile_structure, ~16 patterns)

#### Critical Severity
- **FROM latest tag** — Unpredictable builds
- **curl | bash in RUN** — Arbitrary code execution
- **ADD from URL** — Can execute code, use COPY instead

#### High Severity
- **No USER directive** — Runs as root
- **Secrets in ENV** — Plaintext credentials in image layers
- **No HEALTHCHECK** — No automated liveness detection

#### Medium Severity
- **COPY vs ADD** — ADD has unpacking behavior

### TERRAFORM (scan_insecure_terraform + analyze_terraform_structure, ~20 patterns)

#### Critical Severity
- **Hardcoded secrets** — API keys, passwords in .tf files
- **Wildcard IAM permissions** — Overly broad access (Action="*", Resource="*")
- **Open security groups** — 0.0.0.0/0 CIDR blocks

#### High Severity
- **Missing version constraints** — Unexpected provider/module changes
- **Public S3 bucket** — Unintended data exposure

#### Medium Severity
- **Unencrypted RDS**, missing database backups, unencrypted EBS volumes

---

## PART 3: COMPLEXITY ANALYSIS (analyze_complexity)

**Cyclomatic Complexity Detection** — Counts decision points:
- Branching: if/elif/else_if, match/case/switch
- Loops: for, while, for_in
- Exception handling: except/catch clauses
- Ternary/conditional expressions
- Logical operators: && || and or

**Per-Language Function Node Kinds:**
- Rust: function_item
- Python: function_definition
- TypeScript/TSX: function_declaration, method_definition
- Bash: function_definition
- YAML, Dockerfile, Terraform: (no function nodes extracted)

---

## PART 4: PATTERN CLASSIFICATION VOCABULARY (src/patterns.rs)

**Canonical patterns for normalizing diverse findings** (~14 core patterns):
- sql_injection
- xss
- eval_exec
- hardcoded_secret
- debug_mode
- open_binding (0.0.0.0)
- bare_except
- blocking_in_async
- path_traversal
- weak_crypto
- unused_code
- non_atomic_write

---

## PART 5: IDENTIFIED GAPS & PLANNED FEATURES

### Not Yet Implemented (from docs/plans/)

1. **Go Language Support** — SQL injection, hardcoded secrets, insecure HTTP, goroutine leaks
2. **Java/JVM Support** — SQL injection, XXE, deserialization RCE, hardcoded secrets
3. **TypeScript Advanced** — Floating point equality, race conditions, missing await
4. **Terraform Advanced** — Cross-module secrets, state encryption, advanced IAM
5. **Bash Advanced** — Command injection, race conditions, capability drops
6. **YAML/Home Assistant Advanced** — Circular dependencies, condition fallbacks, complex Jinja2
7. **Performance Patterns** — N+1 queries, inefficient operations, memory leaks
8. **Architecture Antipatterns** — God objects, tight coupling, dependency cycles

---

## SUMMARY

| Language | AST Rules | Tree-Sitter Patterns | Total | Coverage |
|----------|-----------|----------------------|-------|----------|
| Rust | 1 | ~8 | ~9 | Moderate |
| Python | 3 | ~22 | ~25 | Good |
| TypeScript | 4 | ~28 | ~32 | Very Good |
| YAML | 1 | ~15 | ~16 | Moderate |
| Bash | 1 | ~18 | ~19 | Good |
| Dockerfile | 0 | ~16 | ~16 | Good |
| Terraform | 0 | ~20 | ~20 | Very Good |
| **TOTAL** | **10** | **127+** | **137+** | |

---

## KEY IMPLEMENTATION DETAILS

- **Tree-Sitter Grammars**: Rust, Python, TypeScript/TSX, YAML, Bash + vendored Dockerfile grammar
- **HCL Support**: Terraform via tree-sitter-hcl
- **ast-grep Integration**: Custom rules in YAML format, auto-discovered from `~/.quorum/rules/`
- **Test Context Awareness**: Unsafe/unwrap checks skip #[test] and #[cfg(test)] code
- **Linter Coordination**: AST findings integrated with clippy, ruff, eslint, shellcheck, hadolint, tflint
- **Complexity Thresholds**: Varies by language, calibrated via feedback loop
