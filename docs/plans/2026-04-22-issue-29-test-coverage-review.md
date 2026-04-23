# Issue #29 — Test Coverage Review

**Reviewer:** Test Planning & Implementation Agent
**Date:** 2026-04-22
**Inputs:** `2026-04-22-issue-29-context7-deps-design.md`, `2026-04-22-issue-29-context7-deps.md`,
            `src/context_enrichment.rs`, `src/domain.rs`, `src/telemetry.rs`

---

## 1. Acceptance criteria for issue #29

A reviewer should be able to walk through this list and verify each behavior before
calling the issue closed.

1. **Rust enrichment exists.** Given a project with `Cargo.toml` containing `tokio = "1"`
   and a Rust file that imports `tokio::sync`, a `quorum review` invocation MUST cause
   the LLM prompt to contain a Context7 docs section labeled with `tokio` and the
   `rust` generic query (`async safety error handling`).
2. **Hyphen normalization round-trips.** `Cargo.toml` declaring `serde-json = "1"`
   combined with `use serde_json::Value;` MUST produce a Context7 doc for
   `serde_json` (the import-side spelling).
3. **Curated wins over generic.** A JS project with `react` in both `package.json`
   and `import 'react'` MUST emit exactly one curated `react` doc with the curated
   query (`hooks rules component lifecycle...`), not the generic JS query.
4. **Imports filter is enforced.** A manifest with 50 deps where the file imports
   only one of them MUST trigger Context7 calls for at most that one dep (plus any
   directory-detected curated frameworks).
5. **K=5 cap is enforced.** A file importing 10 manifest-matched deps MUST yield
   at most 5 Context7 docs from the dep path, in import-occurrence order.
6. **HA/ESPHome path still works.** A directory with `configuration.yaml` (no
   manifest) MUST still produce a curated `home-assistant` Context7 doc.
7. **Telemetry is wired.** Every review MUST produce a `reviews.jsonl` row whose
   `context7_resolved + context7_resolve_failed` equals the number of distinct dep
   names attempted, and `context7_query_failed` reflects post-resolve failures.
8. **Backward-compatible JSONL.** An old `reviews.jsonl` row missing the three new
   fields MUST still deserialize and the missing fields MUST default to `0`.
9. **Negative cache holds.** Two consecutive reviews of the same file with a dep
   that fails to resolve MUST result in exactly one inner `resolve_library` call
   to Context7 (within the 24h TTL).
10. **Scoped npm packages preserve scope.** `@nestjs/core` in `package.json` matched
    against `import { x } from '@nestjs/core'` MUST query Context7 with the keyword
    `nestjs`, not `core`.
11. **No-op safety.** A project with no manifests and no curated framework match
    MUST make zero Context7 calls and produce an empty docs section.
12. **Exit code unchanged.** Adding the new path MUST NOT change exit codes for
    any previously-passing test scenario (`cargo test` stays green at branch start
    and at branch end).

---

## 2. Test coverage gap analysis

The 16-task plan covers happy paths thoroughly, but several gaps matter.

### 2.1 Telemetry struct identity (MUST-FIX)

The plan (Task 13) targets `pub struct ReviewTelemetry` in `src/review.rs`. The
codebase actually has `pub struct TelemetryEntry` in `src/telemetry.rs`. Either
the plan is referencing a stale name or there is a struct I did not find. Before
TDD begins, this needs to be reconciled — otherwise Task 13 cannot land. The
backward-compat test in §3 is written against `TelemetryEntry`; rename if needed.

### 2.2 `normalize_import_to_dep_names` edge cases (SHOULD-FIX)

The plan implements this private helper in Task 10 with no dedicated unit tests.
It is the single point that bridges import strings to dep names — bugs here
silently break the whole feature. Untested cases:

- **Bare `use tokio;`** — extractor likely yields `"tokio"` with no `::`. The split
  on `[. / :]` returns `"tokio"` head, which is correct, but should be asserted.
- **Grouped use `use tokio::{sync, time};`** — depends on what the upstream
  extractor emits. If it emits `"tokio::{sync, time}"` as a single string, the
  current head-split returns `"tokio::{sync, time}"` (fails). If it emits two
  entries `"tokio::sync"` and `"tokio::time"`, the dedup in `import_matched`
  protects us. We need a test pinning the contract.
- **Local module paths** — `crate::foo`, `super::foo`, `self::foo` MUST NOT match
  any dep named `crate`, `super`, `self`. Current code would match a hypothetical
  `crate` dep (impossible in practice but worth a guard).
- **Empty string** and **leading `::` (`::std::ptr`)** — `split` would yield `""`
  as head, which silently matches a bogus empty dep. Add filter.
- **Scoped pkg without slash (`@foo`)** — current code returns `vec![imp.into()]`,
  which is fine but should be asserted.

### 2.3 Error paths on Context7 calls (SHOULD-FIX)

Plan tests use only `Spy` fetchers that return `Some/None`. There are no tests for:

- **HTTP 500 from Context7** — handled by the fetcher trait returning `None`?
  Confirm and assert that this increments `context7_query_failed`, not
  `context7_resolve_failed`.
- **Malformed JSON from Context7** — same path; should be treated as query fail.
- **Network timeout** — also `None`.

These are mostly covered by `enrich_telemetry_counts_correctly`, but the test
mixes resolve-fail and query-fail in ways that don't isolate the path. One
focused test per error class would prevent silent regression.

### 2.4 Boundary cases on K (MUST-FIX)

Task 11 covers K=10→5 and K=0 (empty inputs returns empty). It does NOT cover:

- **Exactly K=5 import-matched deps** — should produce exactly 5, not truncate.
- **K=4** (below the cap) — should produce 4.
- **K=6** (one over) — should drop the LAST in import order, not a random one.

The "drop the last" guarantee underpins the AC "in import-occurrence order."

### 2.5 Language mismatch (SHOULD-FIX)

If a Python project somehow has `tokio` in its imports (impossible in practice
but possible in a polyglot repo: `Cargo.toml` + `package.json` + `pyproject.toml`
all in one dir), what happens? The plan would happily fetch `tokio` docs for a
Python file. A test asserting that import-to-dep matching considers neither
language nor file extension is fine — but the behavior should be pinned with a
comment so future readers don't introduce a "filter by language" change that
breaks the polyglot case.

### 2.6 Curated AND generic for the same dep (NICE-TO-HAVE)

Currently impossible by construction (curated is checked first), but if someone
adds `react` to both the curated map and the deps with imports, the
`enrich_dedupes_curated_framework_already_in_deps` test covers the dedup. This
is fine; no extra test needed.

### 2.7 `requirements.txt` extras + version operators (SHOULD-FIX)

Task 5 tests cover `fastapi>=0.100`, comments, `-r`, `-e`, `git+`. They miss:

- **Extras in requirements.txt:** `pydantic[email]>=2` — `strip_python_dep_spec`
  handles it (per the pyproject test), but no test asserts it for requirements.txt.
- **Environment markers:** `requests; python_version >= "3.8"` — the `;` is in
  the strip set, so it works, but no test pins it.
- **CRLF line endings** — Windows-authored requirements.txt; a test would catch
  any future `lines()` swap.

### 2.8 Cargo edge cases (SHOULD-FIX)

Plan misses:

- **Duplicate dep across `[dependencies]` and `[dev-dependencies]`** (e.g.
  `tokio` in both). Should appear once or twice? The current implementation
  would push twice. Either dedup in `parse_cargo` or assert "appears at least
  once" — pin the choice.
- **Renamed deps** (`foo = { package = "real-crate", version = "1" }`) — code
  uses the key `foo`, not the package name. This is the right behavior (matches
  the import) but should be asserted.

### 2.9 Pipeline integration (SHOULD-FIX)

Task 14 hand-waves the integration test ("or extract a helper if needed"). The
risk is that the pipeline never actually calls `enrich_for_review` in the JSON-
output path, or in the `--parallel` path, or in the daemon path. Three explicit
end-to-end assertions:

- TTY review writes counters to `reviews.jsonl`.
- `--json` review writes counters to `reviews.jsonl`.
- Two reviews back-to-back show resolve cache hit (zero calls on the second).

### 2.10 Backward compatibility of `reviews.jsonl` (MUST-FIX)

Task 13's second test is left as a TODO ("adjust to actual minimal shape"). This
is the riskiest behavioral change in the PR — old rows must keep deserializing
or every `stats` invocation breaks for users who have history. Concrete failing
test in §3.

---

## 3. Concrete additional test cases

Below are the smallest set of tests that close the gaps marked MUST-FIX and the
highest-value SHOULD-FIX items. Drop into the appropriate `#[cfg(test)] mod tests`.

### 3.1 `normalize_import_to_dep_names` direct tests
*(add to `src/context_enrichment.rs` test mod; requires making the helper
`pub(crate)` for testing or moving the assertions into `enrich_for_review` end-
to-end tests).*

```rust
#[test]
fn normalize_bare_use_returns_root() {
    assert_eq!(normalize_import_to_dep_names("tokio"), vec!["tokio"]);
}

#[test]
fn normalize_local_paths_do_not_match_real_deps() {
    // crate::foo / super::foo / self::foo must yield "crate"/"super"/"self"
    // verbatim — they will simply not appear in any real Cargo.toml, so the
    // import-set lookup misses. Pin this so a future "filter locals" change
    // doesn't accidentally start matching a "crate" or "super" dep.
    assert_eq!(normalize_import_to_dep_names("crate::foo"), vec!["crate"]);
    assert_eq!(normalize_import_to_dep_names("super::foo"), vec!["super"]);
    assert_eq!(normalize_import_to_dep_names("self::foo"), vec!["self"]);
}

#[test]
fn normalize_leading_colon_does_not_yield_empty() {
    let out = normalize_import_to_dep_names("::std::ptr");
    assert!(out.iter().all(|s| !s.is_empty()),
        "leading :: must not produce empty head: {out:?}");
}

#[test]
fn normalize_scoped_pkg_without_subpath() {
    assert_eq!(normalize_import_to_dep_names("@foo"), vec!["@foo"]);
}

#[test]
fn normalize_scoped_pkg_with_deep_path() {
    // @nestjs/common/decorators must collapse to @nestjs/common
    assert_eq!(
        normalize_import_to_dep_names("@nestjs/common/decorators"),
        vec!["@nestjs/common"]
    );
}
```

### 3.2 K-boundary tests
*(add to `src/context_enrichment.rs` test mod, alongside `enrich_caps_at_five_docs`).*

```rust
#[test]
fn enrich_exactly_five_matched_returns_five() {
    use crate::dep_manifest::Dependency;
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, n: &str) -> Option<String> { Some(n.into()) }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { Some("d".into()) }
    }
    let deps: Vec<_> = (0..5).map(|i| Dependency {
        name: format!("dep{i}"), language: "rust".into(),
    }).collect();
    let imports: Vec<_> = (0..5).map(|i| format!("dep{i}::x")).collect();
    let result = enrich_for_review(&deps, &[], &imports, &Spy);
    assert_eq!(result.docs.len(), 5);
}

#[test]
fn enrich_six_matched_drops_the_last_in_import_order() {
    use crate::dep_manifest::Dependency;
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, n: &str) -> Option<String> { Some(n.into()) }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { Some("d".into()) }
    }
    let deps: Vec<_> = (0..6).map(|i| Dependency {
        name: format!("dep{i}"), language: "rust".into(),
    }).collect();
    // import order: 0,1,2,3,4,5 -> dep5 must be the one dropped
    let imports: Vec<_> = (0..6).map(|i| format!("dep{i}::x")).collect();
    let result = enrich_for_review(&deps, &[], &imports, &Spy);
    let libs: Vec<_> = result.docs.iter().map(|d| d.library.as_str()).collect();
    assert_eq!(libs.len(), 5);
    assert!(!libs.contains(&"dep5"), "dep5 should be dropped, got {libs:?}");
}
```

### 3.3 Telemetry backward compatibility (the test the plan stubbed)
*(add to wherever `TelemetryEntry`/`ReviewTelemetry` lives — likely
`src/telemetry.rs` test mod).*

```rust
#[test]
fn telemetry_entry_old_jsonl_row_deserializes_with_zero_context7_fields() {
    // Synthesized from a real reviews.jsonl row schema BEFORE the new fields
    // were added. If this fails, every existing user's `quorum stats` breaks.
    let old = r#"{
        "run_id": "01HXYZ",
        "timestamp": "2026-01-01T00:00:00Z",
        "repo": "x",
        "invoked_from": "tty",
        "model": "gpt-5.4",
        "files_reviewed": 1,
        "findings_by_severity": {},
        "tokens_in": 0,
        "tokens_out": 0,
        "tokens_cache_read": 0,
        "duration_ms": 0,
        "flags": []
    }"#;
    let entry: TelemetryEntry = serde_json::from_str(old)
        .expect("old JSONL rows must deserialize after schema bump");
    assert_eq!(entry.context7_resolved, 0);
    assert_eq!(entry.context7_resolve_failed, 0);
    assert_eq!(entry.context7_query_failed, 0);
}
```

*(NOTE: confirm the actual `TelemetryEntry` field set in `src/telemetry.rs`
before committing — adjust the JSON shape to match. The test is load-bearing;
do not skip the field-by-field reconcile.)*

### 3.4 Cargo duplicate-dep behavior pinned
*(add to `src/dep_manifest.rs` test mod).*

```rust
#[test]
fn cargo_dep_in_both_dependencies_and_dev_dependencies_appears_at_least_once() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dependencies]
tokio = "1"

[dev-dependencies]
tokio = "1"
"#);
    let deps = parse_dependencies(dir.path());
    let count = deps.iter().filter(|d| d.name == "tokio").count();
    // Pin whichever behavior is chosen in implementation. If we dedup -> 1.
    // If we allow dups (downstream HashSet handles it) -> 2. Update assert
    // to match the implemented decision.
    assert!(count >= 1, "tokio missing entirely: {deps:?}");
}

#[test]
fn cargo_renamed_dep_uses_key_not_package() {
    // foo is the import-side name; "real-crate" is what's on crates.io.
    // We must surface "foo" so the import filter matches `use foo::...`.
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dependencies]
foo = { package = "real-crate", version = "1" }
"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "foo"),
        "renamed dep must surface key, not package: {deps:?}");
    assert!(!deps.iter().any(|d| d.name == "real_crate"),
        "must not surface package name: {deps:?}");
}
```

### 3.5 Negative-cache TTL respected
*(add to `src/context_enrichment.rs` test mod, next to `cached_fetcher_negative_result_is_cached`).*

```rust
#[test]
fn cached_fetcher_negative_cache_respects_ttl() {
    // To make this testable in <24h, expose a constructor that takes a custom
    // TTL: CachedContextFetcher::with_ttl(&inner, Duration::from_millis(50))
    use std::sync::Mutex;
    use std::time::Duration;
    struct CountingSpy { calls: Mutex<u32> }
    impl ContextFetcher for CountingSpy {
        fn resolve_library(&self, _: &str) -> Option<String> {
            *self.calls.lock().unwrap() += 1;
            None
        }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { None }
    }
    let inner = CountingSpy { calls: Mutex::new(0) };
    let cached = CachedContextFetcher::with_ttl(&inner, Duration::from_millis(50));
    let _ = cached.resolve_library("missing");
    std::thread::sleep(Duration::from_millis(75));
    let _ = cached.resolve_library("missing");
    assert_eq!(*inner.calls.lock().unwrap(), 2,
        "expired entry must trigger fresh inner call");
}
```

### 3.6 Pipeline integration: end-to-end counters
*(add to `tests/context7_integration.rs`; replaces the hand-wave in Task 14).*

```rust
#[test]
fn pipeline_review_writes_context7_counters_to_reviews_jsonl() {
    // Build a tempdir with Cargo.toml + src/main.rs that imports tokio.
    // Inject a spy fetcher that resolves tokio and returns a doc.
    // Run the pipeline entry that flushes telemetry.
    // Read ~/.quorum/reviews.jsonl (or QUORUM_HOME-overridden path) and
    // assert the latest row has context7_resolved >= 1.
    //
    // This is the only integration test that proves wiring; without it,
    // unit tests for enrich_for_review are happy but the pipeline could
    // silently drop the metrics.
}
```

### 3.7 Polyglot repo language-mismatch pinning
*(add to `src/context_enrichment.rs` test mod).*

```rust
#[test]
fn enrich_does_not_filter_by_dep_language_vs_import_origin() {
    // Pinning current behavior: deps from any manifest are eligible if
    // their name matches an import head. Adding a "must match language"
    // filter would silently break polyglot repos.
    use crate::dep_manifest::Dependency;
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, n: &str) -> Option<String> { Some(n.into()) }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { Some("d".into()) }
    }
    let deps = vec![
        Dependency { name: "tokio".into(), language: "rust".into() },
        Dependency { name: "tokio".into(), language: "python".into() }, // hypothetical
    ];
    let imports = vec!["tokio::sync".into()];
    let result = enrich_for_review(&deps, &[], &imports, &Spy);
    assert!(result.docs.iter().any(|d| d.library == "tokio"));
}
```

---

## Summary of priorities

| Severity     | Gap                                                      |
|--------------|----------------------------------------------------------|
| MUST-FIX     | `TelemetryEntry` vs `ReviewTelemetry` name reconcile (§2.1) |
| MUST-FIX     | K boundary tests at exactly 5 and 6 (§2.4)               |
| MUST-FIX     | Backward-compat JSONL deserialization test (§2.10)       |
| SHOULD-FIX   | `normalize_import_to_dep_names` direct tests (§2.2)      |
| SHOULD-FIX   | Pipeline end-to-end counter integration test (§2.9)      |
| SHOULD-FIX   | Cargo duplicate + renamed dep behavior pinned (§2.8)     |
| SHOULD-FIX   | Negative-cache TTL test with injectable duration (§3.5)  |
| SHOULD-FIX   | requirements.txt extras + env markers (§2.7)             |
| NICE-TO-HAVE | Polyglot language-mismatch pinning (§2.5)                |
| NICE-TO-HAVE | Explicit HTTP-500/malformed-JSON error path tests (§2.3) |

Total new tests proposed: **12**, all written in the same style as the plan's
existing tests. They close the riskiest behavioral gaps — particularly the
backward-compat one, where silence at PR time means broken `quorum stats` for
every user with prior history.
