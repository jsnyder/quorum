# Issue #29 Test Plan — Antipattern Review

Canon: codepipes 13 + modern (snapshot abuse, AI-generated tests, ice cream cone, brittle selectors, over-mocking, missing contracts, assertion-free tests).

## 1. Antipattern findings

### Task 1 — `empty_project_returns_empty_vec`
- **Antipattern:** Assertion-Free / Liar Test (#16) and Testing Trivial Code (#4).
- **Concern:** Tests a stub that unconditionally returns `Vec::new()`. The test passes whether `parse_dependencies` reads files, panics, or is `unimplemented!()`.
- **Severity:** SHOULD-FIX.
- **Rewrite:** Skip the skeleton commit; let Task 2's first real test drive the type into existence (RED on a meaningful assertion). If the skeleton must ship for compile, do not add a "test" — add a `// implementation pending` comment.

### Task 2 — `cargo_*` cluster (`write` helper + 6 tests)
- **Antipattern:** Setup-heavy / Test Code as Second-Class (#9) and Brittle internal-structure coupling (#5).
- **Concern:** Each test inlines a raw TOML literal; the `write` helper hides nothing. Six near-identical tests differ only in TOML body. Also: `cargo_workspace_root_no_dependencies_returns_empty` asserts `is_empty()` — but a workspace root with `[workspace.dependencies]` would silently break that contract later.
- **Severity:** NICE-TO-HAVE (cluster), SHOULD-FIX (workspace test).
- **Rewrite:** Extract a `cargo_with(deps: &[(&str, &str)])` builder so test bodies show *intent* (`cargo_with(&[("tokio","1")])`). For workspace test, assert specifically that `[workspace.members]` alone produces empty AND add a sibling test for `[workspace.dependencies]` so the silent-broadening risk is locked down.

### Task 3 — `package_json_language_typescript_when_tsconfig_sibling`
- **Antipattern:** Testing Implementation Details (#5).
- **Concern:** The TS-vs-JS distinction is determined by *file presence*, which is a current implementation choice. If detection moves to reading `package.json#type` or a workspace tsconfig, this test breaks without a behavior change.
- **Severity:** SHOULD-FIX.
- **Rewrite:** Name the test `package_json_dependencies_get_typescript_language_when_project_is_typescript` and add a one-line comment that the *signal* (tsconfig) is incidental. Better: extract `detect_js_language(project_dir) -> &str` and unit-test that separately so the parser test asserts only "language matches what detector returned".

### Task 4 — `pyproject_pep621_wins_when_both_present`
- **Antipattern:** Test name doesn't match assertion (Test Naming, codepipes #9 territory).
- **Concern:** Name says "PEP 621 wins" but assertion is `!names.contains(&"django")`. A future bug that returns *neither* still passes the negative half. Also: `python` exclusion in Poetry path is tested implicitly via `!names.contains(&"python")` in `pyproject_poetry_deps_parsed` — fine — but `pyproject_pep621_deps_parsed` doesn't pin `deps.len() == 3`, so a future "include extras as deps" regression slides through.
- **Severity:** SHOULD-FIX.
- **Rewrite:** Add `assert_eq!(names, vec!["fastapi"])` (exact set + length) for the precedence test. For PEP 621, add `assert_eq!(deps.len(), 3)`.

### Task 5 — `requirements_txt_parsed_when_no_pyproject`
- **Antipattern:** Multiple-concept test ("Eager Test").
- **Concern:** Single test asserts comment-skip, blank-skip, `-r`/`-e` skip, `git+` skip, AND happy-path parsing. When it fails, you can't tell which rule broke. The `assert_eq!(names.len(), 2)` does catch over-inclusion, which is the saving grace.
- **Severity:** NICE-TO-HAVE.
- **Rewrite:** Split into `requirements_skips_comments_and_blanks`, `requirements_skips_includes_and_editable`, `requirements_skips_vcs_urls`. Each ~3 lines.

### Task 6 — `curated_query_for_known_returns_some` / `_unknown_returns_none`
- **Antipattern:** Assertion-Free in spirit (#16) — only checks Some/None.
- **Concern:** A silent edit changing `"hooks rules component lifecycle..."` to `""` still passes. The query *content* is the contract — that's what flows to Context7. (The user flagged this exact pattern.)
- **Severity:** MUST-FIX.
- **Rewrite:** `assert!(curated_query_for("react").unwrap().contains("hooks"))` and one similar semantic-marker check per curated entry. Or table-test: `for (name, marker) in [("react","hooks"),("django","ORM"),...] { assert!(curated_query_for(name).unwrap().contains(marker)); }`.

### Task 7 — `generic_query_per_language`
- **Antipattern:** Brittle exact-match string assertion (#5/#14 modern).
- **Concern:** `assert_eq!(generic_query_for_language("rust"), "common pitfalls async safety error handling")` — reordering words or adding a synonym breaks the test without changing behavior. Also duplicates the production string verbatim, which is the "Test Logic in Production Code" mirror.
- **Severity:** MUST-FIX.
- **Rewrite:** `let q = generic_query_for_language("rust"); assert!(q.contains("async") && q.contains("error"));` — assert the load-bearing keywords, not the exact prose.

### Task 8 — `build_code_aware_query_extracts_scope_for_scoped_packages`
- **Antipattern:** None — this one is well-targeted (RED first, fixes a real bug, asserts both inclusion and exclusion). Keep as-is.

### Task 9 — `enrich_for_review_empty_inputs_returns_empty`
- **Antipattern:** None significant. The Spy is minimal and only used as a no-op boundary. Acceptable seam test.

### Task 10 — `enrich_*` cluster (3 tests with `CapturingSpy`)
- **Antipattern:** Over-Mocking / Mockist Trap (#19) AND production smell from `_ = import_set;`.
- **Concern (tests):** `CapturingSpy` is duplicated *three times verbatim* across tests — copy-paste of mock infrastructure (#9). The capture-then-`assert!(captured[0].contains(...))` pattern indexes blindly: if the implementation changes ordering (e.g., curated frameworks fetched first), `captured[0]` is the wrong query and the assertion fails for the wrong reason.
- **Concern (production):** The `let _ = import_set;` line in the proposed implementation is a code smell: the variable was constructed for a reason (presumably an earlier dedupe path), then the design changed and nobody removed it. The plan acknowledging it with `// silence dead_code` is exactly the test-design feedback loop failing — the test didn't drive the variable's existence, so the test won't drive its removal either. (User flagged this explicitly.)
- **Severity:** MUST-FIX (both).
- **Rewrite (tests):** Hoist `CapturingSpy` to a `mod test_support` once. Replace `captured[0].contains(...)` with `assert!(captured.iter().any(|q| q.contains("hooks")))` so test order-independence matches behavior order-independence.
- **Rewrite (production):** Delete `import_set` entirely. If it was meant for O(1) lookup, refactor to use it; if not, it's noise. A test should fail when dead code appears, not be amended to ignore it.

### Task 11 — `enrich_caps_at_five_docs` + `enrich_telemetry_counts_correctly`
- **Antipattern:** Asserting Internal Call Counts when behavior matters (#5).
- **Concern:** `enrich_caps_at_five_docs` asserts `result.docs.len() == 5` with 10 candidates — fine. But there is no test that the *first 5 by import order* are returned. A bug that returns 5 random ones (e.g., HashMap iteration order) passes. Spy duplication continues.
- **Severity:** SHOULD-FIX.
- **Rewrite:** Make imports `dep0..dep9`, assert `result.docs.iter().map(|d| &d.library).collect::<Vec<_>>() == vec!["dep0","dep1","dep2","dep3","dep4"]` to lock import-order priority. Hoist `Spy`.

### Task 12 — `cached_fetcher_negative_result_is_cached`
- **Antipattern:** Slow Tests / Time-Dependent Tests latent (#7).
- **Concern:** Cache uses `Instant::now()` + 24h TTL. There is *no test for TTL expiry* — the negative-cache could ship with a 5-second TTL and these tests still pass. Time isn't injected, so there's no way to test it without `thread::sleep(24h)`.
- **Severity:** SHOULD-FIX.
- **Rewrite:** Inject a `Clock` trait (or `Fn() -> Instant`) into `CachedContextFetcher::new_with_clock`; default constructor uses real clock. Add `cached_fetcher_negative_result_expires_after_ttl` test that advances a `MockClock` past `RESOLVE_CACHE_TTL` and asserts the inner fetcher is called again.

### Task 13 — `review_telemetry_context7_fields_deserialize_with_defaults`
- **Antipattern:** Assertion-Free / Stub Test (#16).
- **Concern:** The test body is a comment ("adjust this test to match"). It will be committed as a no-op test that passes vacuously. This is the worst kind of false confidence.
- **Severity:** MUST-FIX.
- **Rewrite:** Either write the real test in the plan now (`let t: ReviewTelemetry = serde_json::from_str(r#"{...minimal...}"#).unwrap(); assert_eq!(t.context7_resolved, 0);`) or remove the test entirely — do not commit a TODO masquerading as a test.

### Task 14 — Pipeline integration test
- **Antipattern:** Vague test plan / Plan-Phase Liar.
- **Concern:** "Build a tempdir... inject a spy ContextFetcher... extract a helper if needed for testability". This is the agent's job to figure out — but as written, an executor could write a smoke test that instantiates the pipeline, asserts nothing meaningful, and check the box.
- **Severity:** SHOULD-FIX.
- **Rewrite:** Pin the assertion: "telemetry.context7_resolved == N where N = number of imported deps the spy resolved successfully; docs.len() == N; the spy received resolve_library calls only for deps appearing in imports". Make the contract explicit before code is written.

### Task 15 — Remove `fetch_framework_docs`
- **Antipattern:** None. Verification step (`rg`) before delete is sound.

## 2. Patterns the plan got RIGHT

- **Strict RED→GREEN cadence:** every task explicitly runs the failing test before implementing.
- **Behavioral assertions over coverage:** Task 8's both-positive-and-negative-keyword check is exemplary.
- **Real boundary mocking only:** `ContextFetcher` is a true external (HTTP) seam — mocking it is correct, not over-mocking.
- **Backward-compat awareness:** Task 13 calls out `serde(default)` for old `reviews.jsonl` records.
- **Dedupe + collision tests:** Task 11's `react`-in-both-deps-and-frameworks test catches a real class of bug.
- **`tempfile::TempDir`** for filesystem isolation — Rust tests run in parallel by default and this avoids shared-state bugs (#7).
