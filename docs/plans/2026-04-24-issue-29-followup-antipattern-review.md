# Anti-Pattern Review: issue-29 Followup Test Plan (3 HIGH bugs)

Plan file (`docs/plans/2026-04-24-issue-29-followup-test-plan.md`) was not on disk
at review time. Review is grounded in the bug descriptions plus the production
code at `src/context_enrichment.rs` (lines 55–100, 163, 197) and
`src/dep_manifest.rs` (lines 70–169). The original-sin precedent (clean-form
fixtures vs. hydrated `"Symbol: use ..."` strings) sets the bar.

## Bug 1 — `parse_python_import("os, sys, json")` returns only `["os"]`

### MUST-FIX 1.1 — Original-sin re-run risk (convenience-shaped data)
- **Affects:** any direct unit test of `parse_python_import` only.
- **Why here:** the function is `fn` (private) at `context_enrichment.rs:94`.
  Production never calls it with `"os, sys, json"` — it calls it with
  `"sys: import os, sys, json"` from `normalize_import_to_dep_names`
  (line 73, after `strip_prefix("import ")` removes only the verb, not the
  symbol prefix). A test like
  `assert_eq!(parse_python_import("os, sys, json"), vec!["os","sys","json"])`
  could pass while production still emits one name. This is *exactly* the
  `normalize_import_to_dep_names` regression repeated.
- **Fix:** the canonical assertion MUST go through `normalize_import_to_dep_names`
  with the hydrated form `"sys: import os, sys, json"` (and the `as`-aliased
  form `"p: import os.path as p, sys"`). A direct-helper test is fine as a
  second layer, but it cannot be the only one.

### MUST-FIX 1.2 — Branch coverage for the `extract_quoted_source` short-circuit
- **Affects:** Bug-1 tests that only exercise bare `import os, sys`.
- **Why here:** `normalize_import_to_dep_names` line 70 routes any `import`
  statement carrying quotes through `normalize_ts_package` instead of
  `parse_python_import`. A Python multi-import never has quotes, but a fix that
  e.g. splits on `,` *before* the quote check would silently break
  `"x: import 'reflect-metadata'"`. Add at least one TS side-effect-import
  assertion to the same test module so the routing branch is pinned.

### NICE-TO-HAVE 1.3 — Dotted + aliased combinations
- Add `"join: from os.path import join, dirname"` and
  `"p: import os.path as p, sys.path as sp"` to lock down both the `from` and
  `import` parser shapes the fix touches.

## Bug 2 — `parse_dependencies` picks empty pyproject over requirements.txt

### MUST-FIX 2.1 — Mock theatre / vacuous condition
- **Affects:** any test that stubs `parse_pyproject` to return `vec![]`.
- **Why here:** the bug *only* exists because `parse_pyproject` returns `Vec<Dependency>`
  with no signal distinguishing "no PEP 621 section, no Poetry section" from
  "explicit empty array" (which test `pyproject_empty_pep621_array_wins_over_poetry`
  at line 389 deliberately *protects*). A mock returning `vec![]` cannot tell
  these cases apart, so the test would either falsely pass or break the
  empty-array invariant. Tests MUST use real `pyproject.toml` content via
  `tempfile::TempDir` + `write()` (matching the established pattern at
  `dep_manifest.rs:176-187`).
- **Fix:** at minimum write three real fixtures into `tempdir + requirements.txt`
  combos: (a) zero-byte pyproject, (b) syntactically invalid pyproject (toml
  parse failure path at line 77), (c) pyproject with only `[build-system]` and
  no `[project]` / `[tool.poetry]`. Each must assert that the requirements.txt
  deps surface.

### MUST-FIX 2.2 — Don't break the explicit-empty-array contract
- **Affects:** the fix itself, but tests must pin the boundary.
- **Why here:** lines 87–109 implement a deliberate "PEP 621 present-but-empty
  wins over Poetry". A naive Bug-2 fix ("if pyproject empty, use requirements")
  would silently regress that contract: a project with `dependencies = []` AND
  a stray `requirements.txt` would suddenly resurrect requirements deps.
  The plan MUST include a test:
  `pyproject = "[project]\ndependencies = []\n"` + `requirements.txt = "django\n"`
  → expect `[]`, NOT `["django"]`.

### NICE-TO-HAVE 2.3 — Assert *which* file was read, not just the result
- Use distinct dep names per file (`"fastapi"` only in pyproject,
  `"django"` only in requirements). Asserting `names == ["django"]` is
  stronger than `!names.is_empty()` (which a liar test could satisfy by
  surfacing both).

## Bug 3 — `parse_requirements_txt` drops `mypkg @ git+https://...`

### MUST-FIX 3.1 — Liar-test risk on the URL filter
- **Affects:** any test that asserts only on positive cases
  (`names.contains(&"mypkg")`).
- **Why here:** the existing `requirements_txt_skips_vcs_urls` (line 363)
  *currently passes* by dropping the entire line when `line.contains("://")`
  triggers (line 140). A naive fix that just removes the `://` check would
  silently regress that test. The new test MUST be the
  PEP 508 direct-reference form `"mypkg @ git+https://github.com/x/y.git"` —
  same shape already covered for pyproject at `pyproject_pep621_skips_pep508_direct_url_refs`
  (line 408). The fixture for Bug 3 needs the *same shape* in
  requirements.txt and assert both: `mypkg` survives AND a bare
  `git+https://...` (no `name @ `) is still skipped.

### MUST-FIX 3.2 — Apply Bug-1's "production format" rule here too
- The fix lives in a private parser, but the test SHOULD call
  `parse_dependencies(dir.path())` (the public surface used by
  `context_enrichment.rs:197`), not reach into `parse_requirements_txt`.
  The existing test module already does this — keep that pattern. A test that
  pokes the private function bypasses the `pyp.exists()` / fallback wiring at
  lines 161–167 that Bug 2 is fixing in parallel.

### NICE-TO-HAVE 3.3 — Extras + version specifier on the same direct ref
- Add `"mypkg[extra1] @ git+https://..."` to verify
  `strip_python_dep_spec` (line 61) still trims `[extra1]` when the line is
  no longer pre-filtered by the `://` check. Single-line coverage of both
  branches the fix touches.

## Cross-cutting

### MUST-FIX X.1 — Each new test must fail against `main` before the fix
- Stated obvious, but the original-sin test at
  `context_enrichment.rs:648-663` *passed* on broken code. Plan must
  explicitly call out: run each new test against the unpatched code and
  capture the failure message in the PR description (or at least the commit
  body). No "assertion-free / passes-on-anything" tests sneak through.

### NICE-TO-HAVE X.2 — Don't couple to `tracing::warn!` text
- `parse_pyproject` and friends emit `tracing::warn!` on parse failures
  (lines 19, 45, 78). Tests SHOULD NOT assert log output — that couples to
  implementation. Assert observable behaviour (empty `Vec`, fallback engaged).

### NICE-TO-HAVE X.3 — Name tests after the production scenario, not the helper
- `requirements_txt_falls_back_when_pyproject_has_no_deps` is more durable
  than `parse_dependencies_branch_coverage`. The first survives a refactor
  that inlines the parsers; the second doesn't.

## Summary

| Item | Verdict |
|---|---|
| 1.1 Hydrated-form input for parse_python_import | MUST-FIX |
| 1.2 Quoted-source branch coverage | MUST-FIX |
| 1.3 Dotted + aliased shapes | NICE-TO-HAVE |
| 2.1 Real pyproject fixtures, no mocks | MUST-FIX |
| 2.2 Don't regress explicit-empty-array contract | MUST-FIX |
| 2.3 Distinct dep names per file | NICE-TO-HAVE |
| 3.1 PEP 508 `name @ url` shape | MUST-FIX |
| 3.2 Test through `parse_dependencies` public API | MUST-FIX |
| 3.3 Extras on direct ref | NICE-TO-HAVE |
| X.1 Each test fails on `main` first | MUST-FIX |
| X.2 No coupling to tracing output | NICE-TO-HAVE |
| X.3 Behaviour-named tests | NICE-TO-HAVE |
