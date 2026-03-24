# Test Strategy: quorum Phase 0

**Date**: 2026-03-23
**Scope**: Phase 0 -- Rust Core Library (standalone crate)
**Framework**: `cargo test` + supplementary crates
**Author**: TPIA

---

## 1. Testing Crate Dependencies

Add to `Cargo.toml` under `[dev-dependencies]`:

```toml
[dev-dependencies]
# Property-based testing
proptest = "1"

# CLI integration testing (binary invocation, exit codes, stdout/stderr capture)
assert_cmd = "2"
predicates = "3"

# Parameterized tests
test-case = "3"

# Temp directories for fixture-based tests
tempfile = "3"

# Snapshot testing for serialized output stability
insta = { version = "1", features = ["json"] }

# Tree-sitter language grammars (also needed in [dependencies] for production)
tree-sitter-rust = "0.24"
tree-sitter-python = "0.23"
tree-sitter-typescript = "0.23"
```

**Not recommended:**
- `mockall` -- we do not mock tree-sitter or AST internals. The only mock boundary is subprocess calls for linter orchestration, handled with a simple trait + test double.
- `rstest` -- `test-case` is lighter and sufficient; avoid two parameterization frameworks.

---

## 2. Test Fixture Strategy

### 2.1 Fixture Location

```
tests/
  fixtures/
    rust/
      complex_function.rs       # Deeply nested, high cyclomatic complexity
      dead_code.rs              # Unused functions, unreachable branches
      insecure_patterns.rs      # Known dangerous API calls (unsafe, raw SQL)
      clean.rs                  # No findings expected
      simple_dataflow.rs        # Unvalidated input -> sensitive sink
    python/
      complex_function.py
      dead_code.py
      insecure_patterns.py
      clean.py
      import_graph.py           # Complex import structure
    typescript/
      complex_function.ts
      dead_code.ts
      insecure_patterns.ts
      clean.ts
      react_hook.tsx            # Framework-specific patterns
    linter_output/
      ruff_output.json          # Captured real ruff JSON output
      clippy_output.json        # Captured real clippy JSON output
      eslint_output.json        # Captured real eslint JSON output
    diffs/
      simple_addition.diff      # Single function added
      refactor.diff             # Moved + renamed
      cross_file.diff           # Changes spanning multiple files
  integration/
    linter_orchestration.rs
    cli_exit_codes.rs
    end_to_end_pipeline.rs
```

### 2.2 Fixture Design Principles

- Every fixture is a **real, compilable/parseable code snippet** (not pseudocode).
- Fixtures are **small** (10-50 lines) -- enough to trigger the behavior, nothing more.
- Each fixture has a **comment header** documenting what it tests and expected findings.
- Fixtures are loaded via `include_str!()` in unit tests for zero I/O overhead.
- Integration tests use `std::fs::read_to_string()` from the `tests/fixtures/` path.

### 2.3 Example Fixture

```rust
// tests/fixtures/rust/complex_function.rs
// Expected: cyclomatic complexity >= 10, nesting depth >= 5
fn process_request(req: Request) -> Result<Response, Error> {
    if req.auth.is_some() {
        if req.auth.unwrap().is_valid() {
            match req.method {
                Method::Get => {
                    if req.path.starts_with("/api") {
                        if req.query.contains_key("admin") {
                            for item in req.items.iter() {
                                if item.is_active() {
                                    // deeply nested logic
                                    return Ok(Response::ok(item));
                                }
                            }
                        }
                    }
                }
                Method::Post => { /* ... */ }
                Method::Delete => { /* ... */ }
                _ => {}
            }
        }
    }
    Err(Error::Unauthorized)
}
```

---

## 3. Test Data Builders

### 3.1 FindingBuilder

Every module that produces or consumes `Finding` structs needs test findings. A builder prevents brittle tests that break when fields are added.

```
FindingBuilder::new()
    .severity(Severity::Critical)
    .category("security")
    .source("local-ast")
    .description("Unvalidated input passed to SQL query")
    .line_range(42, 58)
    .evidence("dataflow: req.query -> db.execute()")
    .build()
```

**Rules:**
- Sensible defaults for every field (severity=Info, source="test", line_range=1..1).
- `build()` returns `Finding`, not `Result` -- panic on invalid state in tests.
- Lives in a `test_support` module (`src/test_support.rs`, gated behind `#[cfg(test)]`), or in a shared test utility file importable by both unit and integration tests.

### 3.2 LinterOutputBuilder

For linter orchestration tests that need to construct normalized linter output:

```
LinterOutputBuilder::new("ruff")
    .finding("E501", "Line too long", 42)
    .finding("F401", "Unused import", 1)
    .exit_code(1)
    .build()
```

### 3.3 AstContextBuilder

For hydration tests that need pre-built AST context payloads:

```
AstContextBuilder::new()
    .callee_signature("fn validate(input: &str) -> bool")
    .type_definition("struct Request { auth: Option<Auth> }")
    .caller("fn handle_request() calls validate() at L15")
    .import_target("use crate::auth::validate")
    .build()
```

---

## 4. Module-by-Module Test Plan

### 4.1 Canonical Finding Format (`src/finding.rs`)

The `Finding` struct is the lingua franca of the system. Tests validate serialization, display, and the contract that all downstream modules rely on.

#### Unit Tests (`#[cfg(test)] mod tests`)

| Test Name | What It Validates |
|-----------|-------------------|
| `test_finding_serializes_to_json_with_all_fields` | All fields present in JSON output, correct key names |
| `test_finding_deserializes_from_json_roundtrip` | JSON -> Finding -> JSON produces identical output |
| `test_finding_severity_ordering` | Critical > High > Medium > Low > Info |
| `test_finding_display_human_format` | Human-readable output matches DESIGN.md format |
| `test_finding_display_json_format` | JSON output has no ANSI escape codes |
| `test_finding_with_empty_optional_fields` | Optional fields (evidence, precedent) serialize as null, not missing |
| `test_finding_source_tag_preserved` | Source field ("gpt-5.4", "local-ast", "ruff") survives roundtrip |
| `test_finding_line_range_validation` | Start <= end enforced; single-line findings have start == end |
| `test_finding_calibrator_action_values` | Only valid actions: confirm, dispute, adjust, added |

#### Property-Based Tests (proptest)

| Test Name | What It Validates |
|-----------|-------------------|
| `prop_finding_json_roundtrip` | For any valid Finding, serialize -> deserialize == identity |
| `prop_finding_severity_is_total_order` | For any two severities, exactly one of <, ==, > holds |
| `prop_finding_description_not_empty` | Generated findings always have non-empty description |

#### Snapshot Tests (insta)

| Test Name | What It Validates |
|-----------|-------------------|
| `snap_finding_json_schema` | JSON output structure matches expected schema (catches accidental field renames) |
| `snap_finding_human_display` | Human-readable output format is stable across refactors |

#### What NOT to Test

- Individual getter/setter methods on Finding
- The internal memory layout of Finding
- That serde_json works (it does)

---

### 4.2 Configuration (`src/config.rs`)

#### Unit Tests

| Test Name | What It Validates |
|-----------|-------------------|
| `test_config_loads_from_env_vars` | QUORUM_BASE_URL, QUORUM_API_KEY, QUORUM_MODEL read correctly |
| `test_config_defaults_when_env_unset` | Missing env vars produce sensible defaults |
| `test_config_base_url_default` | Default base URL is documented value |
| `test_config_model_default` | Default model is documented value |
| `test_config_api_key_required_for_llm_mode` | Missing API key returns specific error, not panic |
| `test_config_api_key_not_required_for_local_only` | Local-only analysis works without API key |
| `test_config_rejects_empty_base_url` | Empty string QUORUM_BASE_URL is treated as unset |
| `test_config_trims_whitespace` | Trailing newlines/spaces in env vars are stripped |
| `test_config_base_url_trailing_slash_normalized` | "https://example.com/" and "https://example.com" produce same result |

#### Environment Isolation

Each config test must use a helper that sets/unsets env vars in a scoped way. Since `std::env::set_var` is process-global and not thread-safe, config tests must either:

1. Run with `cargo test -- --test-threads=1` (simple but slow), OR
2. Accept a `ConfigSource` trait that abstracts env var reading (preferred -- also enables testing without touching process env)

**Recommendation**: Option 2. Define `trait ConfigSource { fn get(&self, key: &str) -> Option<String> }` with `EnvConfigSource` for production and `MapConfigSource` for tests. This is not over-abstraction -- it solves a real thread-safety problem.

#### What NOT to Test

- That `std::env::var` works
- Config file parsing (Phase 0 is env-only)

---

### 4.3 Tree-sitter Multi-Language Parsing (`src/parser.rs`)

All tests use real tree-sitter parsing. No AST mocks.

#### Unit Tests (parameterized with `#[test_case]`)

| Test Name | What It Validates |
|-----------|-------------------|
| `test_parse_valid_rust_file` | Parses without error, root node is `source_file` |
| `test_parse_valid_python_file` | Parses without error, root node is `module` |
| `test_parse_valid_typescript_file` | Parses without error, root node is `program` |
| `test_parse_detects_syntax_errors` | Intentionally broken code produces ERROR nodes in tree |
| `test_parse_empty_file` | Empty input parses successfully (not an error) |
| `test_parse_language_detection_by_extension` | `.rs` -> Rust, `.py` -> Python, `.ts` -> TypeScript, `.tsx` -> TSX |
| `test_parse_unknown_extension_returns_error` | `.xyz` file returns informative error |
| `test_parse_extracts_function_nodes_rust` | Finds all `function_item` nodes in Rust fixture |
| `test_parse_extracts_function_nodes_python` | Finds all `function_definition` nodes in Python fixture |
| `test_parse_extracts_function_nodes_typescript` | Finds all `function_declaration` and `arrow_function` nodes |
| `test_parse_preserves_line_numbers` | Node start/end positions match source line numbers |
| `test_parse_handles_utf8_identifiers` | Unicode identifiers (e.g., Japanese variable names) parse correctly |
| `test_parse_large_file_performance` | 10K-line generated file parses in < 100ms |

#### Parameterized Cross-Language Tests

Use `#[test_case]` to run the same logical test across languages:

```
#[test_case("rust", include_str!("../tests/fixtures/rust/clean.rs"); "rust_clean")]
#[test_case("python", include_str!("../tests/fixtures/python/clean.py"); "python_clean")]
#[test_case("typescript", include_str!("../tests/fixtures/typescript/clean.ts"); "ts_clean")]
fn test_parse_clean_file_produces_no_errors(lang: &str, source: &str) { ... }
```

#### What NOT to Test

- Tree-sitter's internal parsing correctness (upstream's responsibility)
- Every possible syntax construct in every language
- Performance of tree-sitter itself (only our wrapper's overhead)

---

### 4.4 AST Context Hydration (`src/hydration.rs`)

Tests that given changed lines, the hydrator correctly attaches surrounding context.

#### Unit Tests

| Test Name | What It Validates |
|-----------|-------------------|
| `test_hydrate_callee_signature_rust` | Function call in diff -> attached callee signature found |
| `test_hydrate_callee_signature_python` | Same for Python (def statement) |
| `test_hydrate_callee_signature_typescript` | Same for TypeScript (function/arrow) |
| `test_hydrate_type_definition_rust` | Custom type usage -> struct/enum definition attached |
| `test_hydrate_type_definition_typescript` | Interface/type alias attached |
| `test_hydrate_caller_blast_radius` | Changed function signature -> list of callers returned |
| `test_hydrate_import_targets_rust` | `use` statement -> resolved import path |
| `test_hydrate_import_targets_python` | `import` / `from X import Y` -> resolved |
| `test_hydrate_import_targets_typescript` | `import { X } from './Y'` -> resolved |
| `test_hydrate_no_context_for_unchanged_lines` | Lines outside diff range produce no hydration |
| `test_hydrate_multiple_calls_in_diff` | Diff with 3 function calls -> 3 callee signatures |
| `test_hydrate_recursive_call_no_infinite_loop` | Function calling itself doesn't cause infinite hydration |
| `test_hydrate_cross_function_within_file` | Helper defined later in file is still found |
| `test_hydrate_missing_definition_graceful` | Call to external crate function -> no crash, empty context |

#### Fixture Design

Each hydration test uses a fixture with a "diff" (specified as line ranges) plus the full file. The hydrator receives parsed AST + diff range and returns a `HydrationContext` struct.

```
// Fixture: function `process()` at L10-20 calls `validate()` defined at L30-35
// Diff: lines 10-20 changed
// Expected: HydrationContext contains callee_signatures: ["fn validate(input: &str) -> bool"]
```

#### What NOT to Test

- That tree-sitter node traversal works (covered by parser tests)
- Hydration for languages not in Phase 0 scope

---

### 4.5 AST Analysis -- Local Reviewer (`src/analysis.rs`)

The local reviewer produces `Finding` structs from AST analysis. Tests validate that specific code patterns produce specific findings.

#### Complexity Analysis

| Test Name | What It Validates |
|-----------|-------------------|
| `test_complexity_flags_deeply_nested_function` | 5+ nesting levels -> finding with severity >= Medium |
| `test_complexity_cyclomatic_count_rust` | Known fixture with 12 branches -> cyclomatic = 12 |
| `test_complexity_cyclomatic_count_python` | Same logic in Python |
| `test_complexity_cyclomatic_count_typescript` | Same logic in TypeScript |
| `test_complexity_ignores_simple_functions` | Linear function -> no complexity finding |
| `test_complexity_counts_match_arms_rust` | Each match arm contributes to cyclomatic count |
| `test_complexity_counts_elif_chain_python` | Each elif contributes |
| `test_complexity_threshold_configurable` | Custom threshold changes which functions are flagged |

#### Dead Code Detection

| Test Name | What It Validates |
|-----------|-------------------|
| `test_dead_code_unused_function_rust` | Function defined but never called -> finding |
| `test_dead_code_unused_function_python` | Same for Python |
| `test_dead_code_unreachable_branch` | Code after unconditional return -> finding |
| `test_dead_code_ignores_pub_functions_rust` | `pub fn` is not dead (may be called externally) |
| `test_dead_code_ignores_main` | `fn main()` and `def main()` are not dead |
| `test_dead_code_ignores_test_functions` | `#[test]` and `def test_*` are not dead |
| `test_dead_code_ignores_exported_python` | Functions in `__all__` are not dead |

#### Insecure Call Patterns

| Test Name | What It Validates |
|-----------|-------------------|
| `test_insecure_raw_sql_python` | `cursor.execute(f"SELECT {x}")` -> Critical finding |
| `test_insecure_eval_python` | `eval()` usage -> finding |
| `test_insecure_unsafe_rust` | `unsafe` block -> finding (info severity, not critical) |
| `test_insecure_unwrap_in_non_test_rust` | `.unwrap()` outside test module -> finding |
| `test_insecure_exec_typescript` | `eval()` / `new Function()` -> finding |
| `test_insecure_no_false_positive_on_safe_patterns` | Parameterized SQL, safe Rust -> no findings |

#### Simple Dataflow

| Test Name | What It Validates |
|-----------|-------------------|
| `test_dataflow_unvalidated_input_to_sink` | Request param flows to SQL without sanitization -> finding |
| `test_dataflow_validated_input_no_finding` | Input passes through validator before sink -> no finding |
| `test_dataflow_within_single_function` | Tracks flow within one function body |

#### Cross-Cutting

| Test Name | What It Validates |
|-----------|-------------------|
| `test_analysis_findings_have_correct_source_tag` | All findings tagged `source: "local-ast"` |
| `test_analysis_findings_have_line_ranges` | Every finding has non-zero line range |
| `test_analysis_clean_file_produces_no_findings` | Clean fixture -> empty findings vec |
| `test_analysis_multiple_findings_same_file` | File with multiple issues -> multiple findings returned |

#### What NOT to Test

- That the analyzer finds every possible code smell (scope to documented patterns only)
- Complex interprocedural dataflow (explicitly out of Phase 0 scope)
- Analysis of binary/generated files

---

### 4.6 Linter Orchestration (`src/linter.rs`)

This is the I/O boundary. Tests split into unit tests (normalization logic) and integration tests (real subprocess calls).

#### Unit Tests -- Output Normalization

| Test Name | What It Validates |
|-----------|-------------------|
| `test_normalize_ruff_json_output` | Real captured ruff JSON -> Vec<Finding> with correct fields |
| `test_normalize_clippy_json_output` | Real captured clippy JSON -> Vec<Finding> |
| `test_normalize_eslint_json_output` | Real captured eslint JSON -> Vec<Finding> |
| `test_normalize_ruff_severity_mapping` | ruff error -> Critical, warning -> Medium, convention -> Info |
| `test_normalize_clippy_severity_mapping` | clippy deny -> Critical, warn -> Medium, allow -> Info |
| `test_normalize_eslint_severity_mapping` | eslint 2 -> Critical, 1 -> Medium |
| `test_normalize_empty_output` | Linter finds nothing -> empty Vec<Finding> |
| `test_normalize_malformed_json_returns_error` | Garbled output -> Err, not panic |
| `test_normalize_findings_tagged_with_linter_source` | Each finding has `source: "ruff"`, `source: "clippy"`, etc. |
| `test_normalize_line_numbers_mapped_correctly` | Linter line numbers match Finding line_range |

These tests use captured JSON stored in `tests/fixtures/linter_output/`. This is the "recorded" pattern -- capture real output once, replay in tests.

#### Unit Tests -- Linter Detection

| Test Name | What It Validates |
|-----------|-------------------|
| `test_detect_ruff_from_pyproject_toml` | `pyproject.toml` with `[tool.ruff]` -> ruff detected |
| `test_detect_eslint_from_eslintrc` | `.eslintrc.json` present -> eslint detected |
| `test_detect_clippy_from_cargo_toml` | `Cargo.toml` present -> clippy detected |
| `test_detect_no_linters_in_empty_dir` | Empty directory -> empty linter list |
| `test_detect_multiple_linters` | Mixed project -> all applicable linters detected |

These tests use `tempfile::TempDir` to create minimal project structures.

#### Unit Tests -- Subprocess Abstraction

Define a trait for subprocess execution:

```
trait CommandRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput>;
}
```

Production: `RealCommandRunner` (calls `std::process::Command`).
Tests: `FakeCommandRunner` that returns pre-recorded output.

| Test Name | What It Validates |
|-----------|-------------------|
| `test_linter_run_success_returns_findings` | Fake runner returns ruff JSON -> findings produced |
| `test_linter_run_not_found_returns_tool_error` | Fake runner returns "not found" -> graceful error, not panic |
| `test_linter_run_timeout_returns_error` | Fake runner simulates timeout -> error with diagnostic message |
| `test_linter_run_nonzero_exit_with_output` | Exit code 1 + valid JSON -> findings extracted (linters exit 1 when they find issues) |
| `test_linter_run_nonzero_exit_no_output` | Exit code 2 + empty output -> tool error |

#### Integration Tests (`tests/integration/linter_orchestration.rs`)

These run real linters and are **gated behind feature flags or environment checks**:

```rust
#[test]
fn test_ruff_integration() {
    if which::which("ruff").is_err() {
        eprintln!("skipping: ruff not installed");
        return;
    }
    // ... run ruff on tests/fixtures/python/insecure_patterns.py
    // ... assert findings are produced and normalized correctly
}
```

| Test Name | Gate | What It Validates |
|-----------|------|-------------------|
| `test_ruff_real_execution` | `ruff` in PATH | Real ruff run on Python fixture -> normalized findings |
| `test_clippy_real_execution` | `cargo clippy` available | Real clippy on Rust fixture -> normalized findings |
| `test_eslint_real_execution` | `eslint` in PATH | Real eslint on TS fixture -> normalized findings |
| `test_full_linter_pipeline` | Any linter available | Detect -> run -> normalize pipeline end-to-end |

**CI configuration**: Run integration tests in a separate CI step with linters pre-installed. Mark with `#[ignore]` by default, run with `cargo test -- --ignored` in CI.

#### What NOT to Test

- That ruff/clippy/eslint themselves find specific bugs (upstream responsibility)
- Every possible linter output format variant (test the common cases + error cases)

---

### 4.7 Finding Merge + Dedup (`src/merge.rs`)

Pure logic module. Heavy unit testing.

#### Unit Tests

| Test Name | What It Validates |
|-----------|-------------------|
| `test_merge_identical_findings_deduped` | Same description + same line -> single finding |
| `test_merge_similar_findings_deduped` | "SQL injection" from ruff + "Unvalidated SQL" from local-ast -> single finding |
| `test_merge_different_findings_preserved` | Unrelated findings -> both kept |
| `test_merge_preserves_all_sources` | Deduped finding retains all source tags |
| `test_merge_picks_highest_severity` | ruff says Medium, local-ast says Critical -> Critical wins |
| `test_merge_overlapping_line_ranges` | L42-50 and L45-55 -> merged range L42-55 |
| `test_merge_adjacent_line_ranges_not_merged` | L42-50 and L52-60 -> two separate findings |
| `test_merge_empty_input` | No findings from any source -> empty result |
| `test_merge_single_source` | Only local-ast findings -> pass through unchanged |
| `test_merge_three_sources` | Findings from ruff + clippy + local-ast merge correctly |
| `test_merge_similarity_threshold` | Findings at 0.7 similarity merge; at 0.3 similarity do not |
| `test_merge_category_match_boosts_similarity` | Same category + overlapping lines -> higher similarity |
| `test_merge_preserves_evidence_from_all_sources` | Merged finding has combined evidence |
| `test_merge_ordering_by_severity_then_line` | Output sorted: Critical first, then by line number |
| `test_merge_idempotent` | merge(merge(findings)) == merge(findings) |

#### Property-Based Tests (proptest)

| Test Name | What It Validates |
|-----------|-------------------|
| `prop_merge_never_increases_finding_count` | |merged| <= |input| for any input |
| `prop_merge_preserves_all_line_ranges` | Every input line range appears in some output finding |
| `prop_merge_idempotent` | merge(merge(x)) == merge(x) |
| `prop_merge_commutative_on_sources` | merge(A, B) == merge(B, A) (order of sources doesn't matter) |

#### What NOT to Test

- The specific similarity algorithm's math (test through behavior: "these merge, these don't")
- String distance metrics in isolation (implementation detail)

---

## 5. Output & CLI Tests

### 5.1 Output Mode Detection (`src/output/mod.rs`)

Already partially implemented. Tests for `Style::detect()` and `should_disable_color()`.

| Test Name | What It Validates |
|-----------|-------------------|
| `test_style_no_color_flag_disables_color` | `Style::detect(true)` -> all fields empty |
| `test_style_no_color_env_disables_color` | `NO_COLOR=1` -> plain style |
| `test_style_term_dumb_disables_color` | `TERM=dumb` -> plain style |
| `test_style_ansi_codes_correct` | `Style::ansi()` fields match expected ANSI codes |

### 5.2 CLI Exit Codes (`tests/integration/cli_exit_codes.rs`)

Using `assert_cmd`:

| Test Name | What It Validates |
|-----------|-------------------|
| `test_version_exits_zero` | `quorum version` -> exit 0 |
| `test_review_unimplemented_exits_three` | `quorum review file.rs` -> exit 3 (tool error, current behavior) |
| `test_no_args_shows_help` | `quorum` with no subcommand -> help text on stderr |
| `test_invalid_subcommand_exits_nonzero` | `quorum foobar` -> exit 2 (clap error) |

### 5.3 JSON Output (future, once review works)

| Test Name | What It Validates |
|-----------|-------------------|
| `test_json_output_when_piped` | stdout piped -> JSON output |
| `test_json_output_with_flag` | `--json` flag -> JSON regardless of TTY |
| `test_json_output_no_ansi_codes` | JSON output contains zero ANSI escape sequences |
| `test_json_output_valid_schema` | Output parses as valid JSON with expected top-level keys |

---

## 6. What NOT to Test (and Why)

| Skip | Reason |
|------|--------|
| Finding struct field getters/setters | Trivial derived code; zero bug risk |
| Tree-sitter internal parsing correctness | Upstream library; 15+ years of production use |
| serde serialization correctness | Test our schema, not serde itself |
| clap argument parsing edge cases | Upstream responsibility; we test our specific commands |
| tokio async runtime behavior | Infrastructure, not our code |
| Linter correctness (does ruff find X?) | Upstream tools; we test normalization of their output |
| Style/color rendering on specific terminals | Environment-dependent; we test the logic, not the terminal |
| Config file parsing | Not in Phase 0 scope (env vars only) |
| LLM API calls | Phase 1 scope; no HTTP client code in Phase 0 |
| Daemon mode / caching | Phase 3 scope |
| MCP protocol handling | Phase 2 scope |
| Secret redaction | Phase 1+ scope (no LLM calls in Phase 0) |

---

## 7. Test Organization

### 7.1 Directory Structure

```
src/
  finding.rs          # #[cfg(test)] mod tests { ... }
  config.rs           # #[cfg(test)] mod tests { ... }
  parser.rs           # #[cfg(test)] mod tests { ... }
  hydration.rs        # #[cfg(test)] mod tests { ... }
  analysis.rs         # #[cfg(test)] mod tests { ... }
  linter.rs           # #[cfg(test)] mod tests { ... }
  merge.rs            # #[cfg(test)] mod tests { ... }
  output/mod.rs       # #[cfg(test)] mod tests { ... }
  test_support.rs     # FindingBuilder, LinterOutputBuilder, etc. (cfg(test))
tests/
  integration/
    linter_orchestration.rs
    cli_exit_codes.rs
  fixtures/
    rust/
    python/
    typescript/
    linter_output/
    diffs/
```

### 7.2 Naming Convention

- Unit test functions: `test_<module>_<behavior>_<scenario>`
- Integration test functions: `test_<feature>_<scenario>`
- Property tests: `prop_<module>_<invariant>`
- Snapshot tests: `snap_<module>_<output_name>`

### 7.3 Test Running Commands

```bash
# All unit tests (fast, no external dependencies)
cargo test --lib

# All integration tests (may need linters installed)
cargo test --test '*'

# Only ignored integration tests (CI with linters)
cargo test -- --ignored

# Specific module
cargo test finding::

# Update snapshots after intentional changes
cargo insta review
```

---

## 8. CI Pipeline Design

### 8.1 Fast Gate (every PR)

```yaml
- cargo test --lib                    # Unit tests only (~5s)
- cargo test --test cli_exit_codes    # CLI smoke tests (~2s)
```

Quality gate: 100% pass, no new warnings.

### 8.2 Full Gate (merge to main)

```yaml
- cargo test --lib
- cargo test --test '*'
- cargo test -- --ignored             # Real linter tests (ruff, clippy pre-installed)
```

### 8.3 Coverage

Use `cargo-llvm-cov` for coverage reporting. Target: 80% line coverage for Phase 0 modules. Do not enforce coverage on `main.rs` or CLI glue code.

---

## 9. Test Effort Estimates

| Module | Unit Tests | Integration Tests | Fixtures | Builders | Estimated Hours |
|--------|-----------|------------------|----------|----------|----------------|
| Finding | 9 + 3 prop + 2 snap | -- | -- | FindingBuilder | 3-4h |
| Config | 9 | -- | -- | MapConfigSource | 2-3h |
| Parser | 13 | -- | 9 fixture files | -- | 4-5h |
| Hydration | 14 | -- | 6 fixture files | AstContextBuilder | 6-8h |
| Analysis | 20+ | -- | 12 fixture files | -- | 8-10h |
| Linter | 15 | 4 (gated) | 3 output files | LinterOutputBuilder, FakeCommandRunner | 6-8h |
| Merge | 15 + 4 prop | -- | -- | -- | 4-5h |
| Output/CLI | 4 | 4 | -- | -- | 2-3h |
| **Total** | **~103** | **~8** | **~30 files** | **4 builders** | **35-46h** |

---

## 10. Risk-Based Prioritization

If time is constrained, implement tests in this order:

1. **Finding format** (foundational -- everything depends on it)
2. **Merge + dedup** (highest bug risk -- similarity logic is subtle)
3. **Linter output normalization** (I/O boundary -- bugs here are silent data corruption)
4. **AST analysis** (core value proposition)
5. **Parser** (tree-sitter is reliable; wrapper is thin)
6. **Config** (simple but important for UX)
7. **Hydration** (complex but Phase 0 can ship with partial hydration)
8. **CLI/Output** (lowest risk, highest visibility if broken)

---

## 11. Flaky Test Prevention

| Risk | Mitigation |
|------|-----------|
| Env var leakage between tests | `ConfigSource` trait abstracts env access; no direct `set_var` |
| Linter not installed | Gate with `which::which()` check; `#[ignore]` by default |
| File system race conditions | `tempfile::TempDir` for isolation; unique dirs per test |
| Tree-sitter version skew | Pin grammar crate versions in `Cargo.toml` |
| Snapshot test churn | Only snapshot stable public formats (JSON schema, human display) |
| Parallel test interference | No shared mutable state; each test creates its own data |

---

## 12. Success Criteria

Phase 0 test suite is complete when:

- [ ] All 7 modules have unit tests covering positive, negative, and edge cases
- [ ] Property-based tests validate serialization roundtrip and merge invariants
- [ ] Integration tests run real linters in CI (at least ruff + clippy)
- [ ] FindingBuilder is used in >= 80% of tests that create findings
- [ ] No test depends on system state (installed tools, env vars, file system)
- [ ] `cargo test --lib` completes in < 10 seconds
- [ ] Coverage >= 80% on all Phase 0 modules
- [ ] Zero `#[allow(dead_code)]` in test support modules
