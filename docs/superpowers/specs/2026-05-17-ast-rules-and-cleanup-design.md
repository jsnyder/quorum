# AST Rules & Cleanup Design

**Date:** 2026-05-17
**Issues:** #380, #381, #382
**Branch:** `feat/ast-rules-and-cleanup`

## Scope

Two work streams in one PR:

1. **Cleanup** — Remove `#![allow(dead_code)]` blanket, fix all surfaced items, adopt `FindingBuilder` across ~46 construction sites in `analysis.rs`, gate test-only accessors behind `#[cfg(test)]`.

2. **AST Rules** — Three new ast-grep rules:
   - `discarded-fallible-result.yml` (Rust) — broadens `ignored-io-result` to catch `let _ =` on any known-fallible method (#380)
   - `console-log-non-test.yml` (TypeScript) — flags `console.log/debug/warn` outside test contexts (#381)
   - `unwrap-after-infallible.yml` (Rust) — suppression pattern marking safe `.unwrap()` calls (#382)

## Design Decisions

### Dead Code Removal

- Remove crate-level `#![allow(dead_code)]` from `src/main.rs:1`
- Strategy per surfaced item: truly dead → delete; test-only → `#[cfg(test)]`; intentional future-use → prefix `_`
- `conn()`/`conn_mut()` in `src/context/index/builder.rs` → `#[cfg(test)]`

### FindingBuilder Adoption

- `FindingBuilder` already exists at `src/finding.rs:216`
- Mechanical replacement of all `Finding { field: value, ... }` with builder chain
- No behavioral change — same fields, same values
- Reduces boilerplate and makes future Finding field additions less painful

### Rule: discarded-fallible-result (Rust)

- Additive to existing `ignored-io-result.yml` (kept for backward compat)
- Matches: `let _ = $EXPR` where EXPR calls `.lock()`, `.send()`, `.flush()`, `.close()`, `.write()`, `.read()`, `.connect()`, `.bind()`
- Also matches bare expression statements for these methods
- `precision: high`, severity: `warning`

### Rule: console-log-non-test (TypeScript)

- Pattern: `console.log($$$)`, `console.debug($$$)`, `console.warn($$$)`
- Exclusions: inside `describe()`, `it()`, `test()` blocks; catch clauses
- Note: ast-grep cannot filter by file path — path exclusion handled at quorum integration layer
- `precision: speculative`, severity: `hint`

### Rule: unwrap-after-infallible (Rust)

- Suppression rule, not detection — provides counter-evidence for unwrap findings
- Patterns: `Some($X).unwrap()`, `Ok($X).unwrap()`
- Integration: calibrator uses presence of this match as FP signal
- `precision: high`, metadata: `judge: suppress_if_matched`

## Testing

- Each rule gets fixtures in `rules/<lang>/tests/` (positive + negative cases)
- FindingBuilder adoption verified by existing 1363-test suite (no behavioral change)
- Dead code verified by `cargo check` producing zero warnings
- Integration verified by `cargo test --bin quorum`

## Non-Goals

- No cross-file analysis
- No changes to calibrator logic (suppression rule is data-only)
- No changes to LLM prompt
