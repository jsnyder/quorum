# Input Validation + TypeScript Fingerprinter Bugfix Rollup

**Date:** 2026-05-26
**Issues:** #149, #150, #202, #234, #346, #347, #348, #368, #369

## Scope

Two cohesive bugfix groups rolled into one PR:

1. **Input validation at CLI boundary** (6 issues) — unconstrained user inputs that bypass validation and reach business logic
2. **TypeScript fingerprinter** (3 issues) — tree-sitter traversal bugs that misclassify or drop function symbols

## Group A: Input Validation

**Design principle:** Hard error (exit 3) on invalid input. Validate at the CLI boundary via clap `value_parser` or manual validation in the opts-to-config conversion. No clamping, no silent correction.

### A1: `--parallel` rejects 0 and caps upper bound (#150)

**File:** `src/cli/mod.rs` — `ReviewOpts.parallel`
**Current:** Accepts any `usize`; `0` means unlimited per help text.
**Fix:** Add a custom `validate_parallel` function following the existing `validate_k` pattern (line 127), keeping the field as `usize`. Accepts 1..=64, rejects 0 and values above 64. Default stays at 4.

### A2: `CalibrateOpts` precision range (#368)

**File:** `src/cli/mod.rs` — `CalibrateOpts.suppress_precision`, `boost_precision`
**Current:** Accepts any `f64`.
**Fix:** Validate in a `CalibrateOpts::validate()` method called at the top of `run_calibrate`: both values must be in `0.0..=1.0`. Return `anyhow::bail!` on violation.

### A3: `ReviewOpts.reasoning_effort` (#234)

**File:** `src/cli/mod.rs` — `ReviewOpts.reasoning_effort`
**Current:** `Option<String>`, unconstrained.
**Fix:** Add a custom `validate_reasoning_effort` value_parser that accepts the known set: `none`, `minimal`, `low`, `medium`, `high`, `xhigh` (matching the existing help text). Keep the field as `Option<String>` since the value is passed as a raw string to the OpenAI API body in `llm_client.rs`. No enum conversion — just string validation at the CLI boundary.

### A4: `ContextAddOpts.kind` (#149)

**File:** `src/cli/mod.rs` — `ContextAddOpts.kind`
**Current:** `String`, documented valid values but no enforcement.
**Fix:** Add a `const VALID_KINDS: &[&str]` and a `validate_kind` value_parser. Known values: `rust`, `typescript`, `javascript`, `python`, `go`, `terraform`, `service`, `docs`.

### A5: `ContextQueryOpts` source name validation (#369)

**File:** `src/cli/mod.rs` — `ContextQueryOpts.source`
**Current:** `Option<String>`, no validation.
**Fix:** When `Some`, validate with the existing `validate_source_name` function (used by `ContextAddOpts.name`). Reject path separators, empty strings, and names exceeding 64 chars.

### A6: `Finding` deserialization rejects invalid line ranges (#202)

**File:** `src/finding.rs` — `Finding` struct, `src/pipeline.rs` — LLM response parsing
**Current:** Derives `Deserialize` directly; `is_valid()` exists but is only called in `classify_in_diff` (pipeline.rs:1491) to skip.
**Fix:** Add a `Finding::normalize_line_range()` method that repairs invalid ranges: clamp `line_start` to `max(1, line_start)` and swap start/end if inverted. Call it immediately after `parse_llm_response()` returns in `pipeline.rs`, before findings are merged, diff-classified, or grounded. This is different from CLI validation — LLM output shouldn't crash the tool, so we repair rather than reject. Only primary `line_start`/`line_end` are repaired; `cited_lines` is already guarded by `anchor_line().max(1)` from PR #402.

## Group B: TypeScript Fingerprinter

**File:** `src/context/extract/fingerprint_typescript.rs`

### B1: Arrow functions assigned to variables (#346)

**Current:** `fingerprint_all_functions` finds `arrow_function` nodes but extracts the name from the node itself (which has none for `const x = () => {}`).
**Fix:** When an `arrow_function` (or `function` expression) has no name child, walk up to check if parent is `variable_declarator` and extract the variable name from the `name` child of that declarator.
**Edge cases to test:** `const x = async () => {}`, `let x = function() {}`, `export const x = () => {}` (declarator may be wrapped in export).

### B2: Nested functions misclassified as methods (#347)

**Current:** `is_inside_class` (lines 173-179) uses `.ancestors().any()` which catches ANY ancestor being a class body — including arrow functions nested deep inside method bodies.
**Fix:** Only set `is_method=true` when the function node's **direct parent** is the class body (i.e., it's a direct member), not when it's an arbitrary descendant.
**Edge cases to test:** class field arrow functions (`field = () => {}`) vs nested arrows inside method bodies.

### B3: `count_type_nesting` returns 0 for top-level generics (#348)

**Current:** Counts nesting within child type arguments but doesn't count the top-level generic wrapper itself.
**Fix:** If the return type node itself is a generic type (has type arguments), start the count at 1 instead of 0.
**Edge cases to test:** `Promise<T>` (expect 1), `Promise<Result<T>>` (expect 2), non-generic `string` (expect 0).

## Testing Strategy

### Group A: CLI parse-path tests
- Use `Args::try_parse_from(...)` to test parse-time rejections (A1, A3, A4, A5), following the existing test pattern at `src/cli/mod.rs:1267-1546`
- For A2: unit test on `CalibrateOpts::validate()` method directly
- For A6: unit test on `Finding::normalize_line_range()` + integration test that LLM-parsed findings come out with valid ranges
- Each test confirms both the error case (invalid input rejected) and the happy path (valid input accepted)

### Group B: Tree-sitter fingerprint tests
- One test per fix using inline TypeScript snippets parsed by tree-sitter
- B1: `const processData = (input: string) => {}` → fingerprint includes "processData"; also `const x = async () => {}`
- B2: class with method containing nested arrow → nested arrow has `is_method=false`; class field arrow → `is_method=true`
- B3: `Promise<T>` return → nesting=1; `Promise<Result<T>>` → nesting=2; `string` → nesting=0
- All tests in existing test modules adjacent to the code

## Out of Scope

- File locking / concurrency (#185, #233, #331) — separate PR
- Refactoring main.rs complexity — tracked separately
- Adding new validation for fields not covered by the 6 issues
