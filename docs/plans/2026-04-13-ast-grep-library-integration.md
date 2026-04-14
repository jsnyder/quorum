# Plan: Compile ast-grep-core into quorum binary

**Date:** 2026-04-13
**Branch:** `feat/ast-grep-library-integration`
**Reviewed by:** GPT-5.4 (plan review), Gemini 3.1 Pro (rule review)

## Goal

Replace the subprocess-based ast-grep integration with compiled-in library dependencies (`ast-grep-core`, `ast-grep-config`, `ast-grep-language`). This eliminates the external dependency, improves performance, and creates a clean module boundary for future pattern migration.

## Scope

**In scope:**
- New `src/ast_grep.rs` module owning rule loading, matching, and Finding conversion
- Load bundled rules from `rules/<lang>/` AND user rules from `~/.quorum/rules/<lang>/`
- Remove `LinterKind::AstGrep` from `linter.rs`
- Wire `ast_grep.rs` into pipeline alongside analysis and linters
- ast-grep-core parses independently (no shared tree-sitter trees yet)

**Out of scope (tracked as issues):**
- Shared parse trees between analysis.rs and ast-grep-core (#9)
- Migrate patterns from analysis.rs to YAML rules (#10)
- Semgrep regression harness for TP/FP corpus (#8)

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Library vs subprocess | Library (compiled in) | Reliability: single binary always works |
| Shared parse trees | Not yet (future #9) | Risk: start simple, optimize after verifying version compat |
| User rule behavior | Additive only, no override | Simplest; use suppress.toml to disable bundled rules |
| Finding source field | Keep `Source::Linter("ast-grep")` | Backward compat with feedback corpus (~4,400 entries) |
| Crate version pinning | Exact (`=0.42.1`) | 0.x semver; minor bumps can break API |

## Dependencies

```toml
ast-grep-core = "=0.42.1"
ast-grep-config = "=0.42.1"
ast-grep-language = "=0.42.1"
```

Tree-sitter version alignment verified: quorum uses `tree-sitter 0.25`, ast-grep-core resolves to `tree-sitter 0.25.10`. All language grammars match.

## Module Design

### New: `src/ast_grep.rs`

```
load_rules(project_dir, home_dir) -> Vec<RuleConfig>
  - Scans rules/<lang>/*.yml (bundled) + ~/.quorum/rules/<lang>/*.yml (user)
  - Parses YAML via ast-grep-config
  - Skips malformed rules with warning, continues
  - Returns sorted rule list (deterministic ordering)

scan_file(source: &str, lang, rules: &[RuleConfig]) -> Vec<Finding>
  - Parses source with ast-grep-core (independent parse, not shared)
  - Runs matching rules with per-rule isolation (one bad rule doesn't block others)
  - Converts matches to Finding structs (severity, line numbers, source field)
  - Normalization contract matches current subprocess output exactly

ext_to_language(ext: &str) -> Option<AstGrepLanguage>
  - Maps file extension to ast-grep language
  - JS/JSX/MJS/CJS -> TypeScript (ast-grep uses TS grammar for JS)
  - Moved from linter.rs
```

### Pipeline Change (`src/pipeline.rs`)

```
Before:  parse -> analysis -> linters (incl. ast-grep subprocess) -> merge -> LLM
After:   parse -> analysis -> linters (external only) -> ast_grep::scan_file -> merge -> LLM
```

ast-grep findings enter the same `all_sources` merge path as before.

### Removals from `src/linter.rs`

- `LinterKind::AstGrep` enum variant
- `run_ast_grep_rules()`
- `normalize_ast_grep_output()`
- `ext_to_ast_grep_lang()`
- `which_ast_grep_available()`
- `ast_grep_has_rules()`
- ~11 associated tests (ported to ast_grep.rs)

## Error Handling

- **Malformed YAML rule:** skip rule, log warning with rule path, continue scanning remaining rules
- **Missing rules/ directory:** return empty rule set, not an error
- **Per-rule isolation:** load and validate each rule independently; one bad rule does not block others
- **Scan failure on a rule:** log warning, continue with remaining rules, return partial findings

## Testing Strategy (TDD)

**Unit tests (pure logic):**
- `ext_to_language()` mapping — port 3 existing tests
- Finding conversion — severity mapping (hint->Low, warning->Medium, error->High)
- Line number normalization (0-indexed -> 1-indexed)
- Source field set to `Source::Linter("ast-grep")`

**Integration tests (real rule loading + matching):**
- Load bundled rule, scan fixture string, verify findings
- Load user rules from tempdir
- Malformed YAML skipped with warning, valid rules still run ("one bad + one good" test)
- Unsupported file extension returns empty findings
- Empty source returns empty findings

**Parity tests (observational equivalence with subprocess):**
- Port existing 11 tests from linter.rs
- Same rule YAML + same input -> same Finding output
- Library findings enter same merge path, same source label, same severity mapping
- Run all 20 bundled rules against their test fixtures, verify match counts

**Regression:**
- All 600+ existing tests pass
- Binary size delta measured and acceptable

## Implementation Order

```
1.   Add crate deps (pinned =0.42.1), verify compiles, measure binary size
1.5  Smoke test: load bundled rules via ast-grep-config, verify JS->TS mapping works
2.   Write src/ast_grep.rs tests (RED) — parity + bad-rule-isolation + conversion
3.   Implement ext_to_language(), make tests pass
4.   Implement load_rules() — sorted, both directories, skip malformed
5.   Implement scan_file() — per-rule isolation, normalization contract
6.   Wire into pipeline.rs
7.   Parity verification: all 600+ tests pass, same findings as subprocess
8.   Remove old linter.rs code (separate commit — rollback safety)
9.   Final binary size comparison
```

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| ast-grep crate API instability (0.x) | Medium | Pin exact versions, wrap in thin adapter |
| tree-sitter version conflict | Low | Verified: both use 0.25.x |
| YAML rule format incompatibility | Medium | Smoke test in step 1.5 before writing all tests |
| JS->TS language mapping difference | Low | Explicit test in smoke test |
| Binary size increase | Low | Shared deps; measure in step 1 and 9 |
| Rollback needed | Low | Old code deleted last (step 8), separate commit |
