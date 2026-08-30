# Testing Anti-Patterns Review: Multi-Source Context Retrieval

## Summary
- **Anti-patterns found:** 6 of 20 applicable
- **Critical issues:** 2
- **Test suite health:** Good (existing patterns are strong; spec gaps are the concern)
- **Recommended testing shape:** Pyramid — this is algorithm-heavy scoring/ranking logic where unit tests provide the most value

## Findings

### [Critical] Anti-Pattern #16 — Assertion-Free / Weak Assertion Risk in Integration Test

**Evidence:**
The spec's integration test description reads: "multi-source bootstrap with 2+ fixture sources, verify chunks from both appear in injection." The existing integration test in `bootstrap.rs:237-303` (`returns_some_when_one_source_has_index`) demonstrates this risk in practice — it asserts only `out.telemetry.auto_inject_enabled` and `out.telemetry.injector_available`, which are trivially true booleans. It does not assert on the *content* of the injection, chunk scores, source origins, or chunk counts. The test would pass even if the retriever returned garbage or nothing at all, because `InjectionOutcome.rendered` is never checked.

**Impact:**
The planned multi-source integration test, if it follows this pattern, will verify only that "something came back" rather than "chunks from multiple sources were merged correctly." A merge_and_rerank bug that silently drops all non-current-repo chunks would pass.

**Remediation:**
- The integration test must assert: (1) `injected_sources` contains names from at least 2 distinct sources, (2) `injected_chunk_count >= 2`, (3) `rendered.is_some()`, (4) the rendered block contains text from both fixture sources. The existing `returns_some_when_one_source_has_index` should also be strengthened — it is the template the new test will follow.

---

### [Critical] Anti-Pattern #4 — Testing Wrong Functionality / Missing Coverage Areas

**Evidence:**
The spec's Testing Strategy section lists 5 bullet points. Several critical behavioral areas receive no mention:

1. **Source prioritization order:** The spec defines "current-repo first, then by weight descending, then by config order" for source query ordering plus `max_sources_queried` cap. No test is planned for this ordering or the cap.
2. **include_for / exclude_for filtering:** These are new config fields with interaction semantics (`exclude_for` wins when both match). No unit tests are planned.
3. **Current-repo detection:** Path canonicalization, `starts_with` matching, canonicalization failure handling, and the "git sources never get current-repo flag" rule are all untested in the plan.
4. **Boost composition:** The spec defines that boosts multiply (`1.3 * 1.2 * 1.1 = 1.716x`). No test verifies the multiplicative composition vs. additive.
5. **Error handling:** "All sources fail -> return empty injection" and "parse_dependencies fails -> proceed without dep boost" are described in the spec but not in the testing strategy.
6. **Single-source fallback:** `[context.multi_source] enabled = false` is specified to restore `find_map` behavior. Not mentioned in testing.

**Impact:**
The most complex and error-prone parts of the feature — scoring math, filter interactions, error degradation — have no planned tests. The planned tests cover the mechanical parts (config parsing, normalization) but miss the behavioral contract.

**Remediation:**
Add unit tests for each of these areas. Specifically:
- `merge_and_rerank` with mixed current-repo and non-current chunks, verifying final order
- `include_for`/`exclude_for` interaction matrix (include only, exclude only, both match, neither)
- Current-repo detection with canonical paths, symlinks, git-backed sources
- Boost composition with a hand-calculated expected score
- All error paths: all sources fail, dep parsing fails, canonicalization fails
- Fallback: `enabled = false` returns same behavior as single-source

---

### [High] Anti-Pattern #5 — Testing Internal Implementation (Inspector Risk)

**Evidence:**
The spec's "unit tests for merge_and_rerank: score normalization" bullet suggests testing the normalization formula directly. The min-max normalization `(score - source_min) / (source_max - source_min)` is an implementation detail that could change to z-score, percentile rank, or any other normalization without changing the behavioral contract (which is: "scores from different sources are comparable and the best chunks float to the top").

The existing codebase shows good discipline here — `injector.rs` tests assert on telemetry outcomes (suppressed_by_floor, suppressed_by_calibrator) rather than internal filtering steps. But the spec's wording invites implementation-coupled tests.

**Impact:**
If normalization tests assert on exact normalized score values, any algorithm improvement (e.g., switching to RRF or z-score) would break tests without changing behavior.

**Remediation:**
Test normalization *indirectly* through behavioral assertions:
- Given chunks from Source A (scores 0.5, 0.8) and Source B (scores 0.3, 0.9), assert that the final ranking reflects the relative ordering after boosts, not the exact normalized values.
- Test edge cases behaviorally: single-candidate source still appears in results, all-identical-score source doesn't produce NaN (the `source_max - source_min = 0` case).
- If direct normalization tests exist, use property-based assertions: "normalized scores are in [0, 1]", "relative ordering within a source is preserved."

---

### [High] Anti-Pattern #19 — Over-Mocking Risk in merge_and_rerank Tests

**Evidence:**
The `merge_and_rerank()` function takes candidates from multiple sources. Testing it will require constructing `ScoredChunk` values with specific scores, source names, chunk kinds, and metadata. The existing `scored()` helper in `injector.rs:427-441` creates chunks with hardcoded boilerplate:

```rust
fn scored(id: &str, score: f32) -> ScoredChunk {
    ScoredChunk {
        chunk: mk_chunk(id),
        score,
        components: ScoreBreakdown { ... },
        source_legs: vec![],
    }
}
```

The `mk_chunk()` helper creates chunks with `content: "fn foo() { bar() }".repeat(40)` — a single repeated string for all chunks. If `merge_and_rerank` ever incorporates content-aware dedup or token counting, all tests would silently pass with identical content but fail on real data.

**Impact:**
Builder helpers that paper over too much create a blind spot. Chunk `source` is hardcoded to "src" in `mk_chunk`, which means no multi-source test built with the existing helper can verify source-level diversity or per-source caps without overriding the source field manually every time.

**Remediation:**
- Create a new `ScoredChunkBuilder` (or extend the existing helpers) with explicit `source`, `kind`, and `language` parameters since these are the fields `merge_and_rerank` branches on.
- Use distinct, realistic content in test chunks rather than identical repeated strings.
- Keep the builder in a shared test utility module, not duplicated per test file. The codebase already has `src/context/inject/plan_tests.rs:10` with `mock_chunk` — consolidate.

---

### [Medium] Anti-Pattern #3 — Right Testing Shape: Config Tests Are Under-Leveraged

**Evidence:**
The spec says `deny_unknown_fields` will be removed from `RawSource` to allow `include_for`, `exclude_for`, and `provides`. The existing `config_tests.rs:267-277` test `rejects_unknown_source_field` with a typo field. This test will start failing when `deny_unknown_fields` is removed, which is correct but the spec doesn't mention updating it. More importantly, removing `deny_unknown_fields` means *any* typo in source config will silently be accepted, which is a regression in user safety.

The spec adds 3 new fields on `RawSource` and 7 new fields on `MultiSourceConfig`. Config parsing is a high-risk area for this codebase (evidenced by the 20 existing config tests). The testing strategy mentions "new fields parse correctly, old configs without new fields still parse" but does not mention:
- Typo detection after `deny_unknown_fields` removal
- Validation ranges for numeric fields (per_source_cap, current_repo_reserved, boost values)
- Interaction between `include_for` and `exclude_for` at the config level (should `exclude_for: ["self"]` be rejected if `self` is not a valid source name? or is it lazy-evaluated?)

**Impact:**
Config errors that were previously caught at parse time will silently pass. Users will get unexpected behavior from typos like `incldue_for`.

**Remediation:**
- Add a test that verifies the new fields parse correctly with valid values
- Add tests for invalid/missing values on the new fields (e.g., `per_source_cap = 0`, negative boosts, NaN/Inf boosts)
- Document what happens to `rejects_unknown_source_field` after `deny_unknown_fields` removal — either accept the regression explicitly or add a custom validator for the known-field set
- Test backward compat: existing sources.toml without new fields still parses with correct defaults

---

### [Medium] Anti-Pattern #9 — Test Code as Second-Class: Test Helper Fragmentation

**Evidence:**
Test chunk/score builders are duplicated across files:
- `src/context/inject/injector.rs` has `mk_chunk()` and `scored()`
- `src/context/inject/plan_tests.rs` has `mock_chunk()` and `scored_chunk()`
- `src/context/retrieve/rerank_tests.rs`, `precedence_tests.rs`, `retriever_tests.rs` likely have their own variants

The multi-source feature will need chunk builders that support multiple sources, languages, and chunk kinds. Building these as one-off helpers in the new test file will add a 4th or 5th copy.

**Impact:**
When `ScoredChunk` or `Chunk` gains a new field (as this feature adds to telemetry), every copy of the builder needs updating. Missed copies produce silent compilation errors or worse — default values that mask test failures.

**Remediation:**
- Before implementing multi-source tests, extract a `test_support::chunks` module (the codebase already has `src/test_support/` based on the `fakes::FakeReviewer` reference) with parameterized builders for `Chunk`, `ScoredChunk`, and `InjectionRequest`.
- Use the builder pattern: `ChunkBuilder::new("id").source("auth-service").kind(Symbol).score(0.8).build()`

---

## Missing Test Coverage Areas (Not Anti-Patterns, but Gaps)

1. **Diversity constraints interaction:** The spec defines `current_repo_reserved` (2 slots) AND `per_source_cap` (3 max from non-current). Test what happens when these constraints conflict: e.g., current repo has only 1 chunk above min_score but reserved = 2. Does the empty reserved slot get backfilled from non-current sources?

2. **Weight = 0 or negative weights:** The spec says `source.weight` is a multiplicative boost. Weight = 0 would zero out all scores from that source. Negative would invert ordering. Are these valid?

3. **Normalization edge case: all candidates from one source have identical scores.** `source_max - source_min = 0` produces division by zero. The spec says "single candidate -> normalized = 1.0" but doesn't address "multiple candidates, all same score."

4. **Telemetry backward compatibility deserialization:** The spec says "all `#[serde(default)]`" but needs a test that old JSON records without the new fields deserialize correctly (the pattern exists elsewhere in the codebase for `LegCounts`).

5. **`match_sources_to_deps` with `provides` aliases:** The spec shows `provides = ["home-assistant-core", "ha-core"]` but no test is planned for partial match, case sensitivity, or hyphen/underscore normalization (which the Cargo parser already does for crate names).

---

## Recommendations (Priority Order)

1. **Add behavioral tests for merge_and_rerank scoring contract.** Test input-output pairs with hand-calculated expected orderings, not intermediate score values. Cover: mixed sources, boost composition, diversity caps, reserved slots, all-fail, and single-source fallback. This is the core value of the feature.

2. **Strengthen the integration test template.** Fix the existing `returns_some_when_one_source_has_index` to assert on rendered content and telemetry counts before cloning it for multi-source. Otherwise both tests will be Liars.

3. **Add include_for/exclude_for filter tests** as a separate unit test group. These are new user-facing config semantics with documented precedence rules. Cover the 2x2 matrix (include only, exclude only, both, neither) plus the "exclude wins" interaction.

4. **Extract shared test chunk builders** into `test_support::chunks` before writing new tests. This prevents copy #4 of `mk_chunk` and makes multi-source test data construction readable.

5. **Add normalization edge case tests:** identical scores, single candidate per source, NaN scores in input, zero-weight source. Use property assertions ("output in [0,1]", "ordering preserved") not exact values.

6. **Plan the `deny_unknown_fields` removal carefully.** Either add a custom field validator to maintain typo detection, or explicitly accept the regression and update `rejects_unknown_source_field` to document the new behavior.

7. **Add current-repo detection unit tests** as a standalone function testable without bootstrap. Cover: symlinks, non-existent paths, git-backed sources excluded, nested project within source.
