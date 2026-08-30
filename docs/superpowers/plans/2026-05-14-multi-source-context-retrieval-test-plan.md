# Test Plan: Multi-Source Context Retrieval

**Spec:** `docs/superpowers/specs/2026-05-14-multi-source-context-retrieval-design.md`
**Date:** 2026-05-14
**Status:** Draft — ready for TDD implementation agent

---

## Acceptance Criteria

Observable behaviors that prove the feature works end-to-end:

- [ ] When `[context.multi_source] enabled = true` (default), the retriever queries all valid sources and merges results into a single globally ranked top-k set
- [ ] When `[context.multi_source] enabled = false`, the retriever falls back to single-source `find_map` behavior identical to pre-feature behavior
- [ ] Chunks from the current repo receive a configurable boost (default 1.3x) on their normalized score
- [ ] Chunks from declared dependencies (matched by name or `provides` aliases) receive a configurable boost (default 1.2x)
- [ ] Chunks from same-language sources receive a configurable boost (default 1.1x)
- [ ] Boosts compose multiplicatively (current-repo + dep + lang = 1.3 * 1.2 * 1.1 = 1.716x)
- [ ] Per-source scores are min-max normalized before cross-source comparison
- [ ] A single-candidate source normalizes to 1.0
- [ ] At least `current_repo_reserved` slots (default 2) in the final top-k are reserved for current-repo chunks
- [ ] No single non-current source contributes more than `per_source_cap` chunks (default 3) to the final top-k
- [ ] Reserved slots only draw from chunks surviving `inject_min_score` -- no backfilling below threshold
- [ ] At most `max_sources_queried` (default 10) sources are queried per review
- [ ] Sources are queried in priority order: current-repo first, then by weight descending, then config order
- [ ] `include_for` / `exclude_for` filters control source eligibility per project; `exclude_for` wins when both match
- [ ] `provides` aliases on `[[source]]` allow dependency matching for sources with divergent names
- [ ] Old `sources.toml` files without new fields parse without error (backward compat)
- [ ] New telemetry fields (`sources_queried`, `sources_contributing`, `per_source_contributions`, `dep_boost_applied`, `current_repo_reserved_available`, `current_repo_reserved_filled`) are populated on every review
- [ ] Legacy `reviews.jsonl` records without new telemetry fields deserialize with serde defaults
- [ ] When `parse_dependencies()` fails, the review proceeds without the dep boost (degraded, not errored)
- [ ] When `canonicalize()` fails for a source path, `is_current_repo` is not set for that source
- [ ] When all sources fail to open, the retriever returns empty injection with `retriever_errored = true`
- [ ] The `ContextInjector` and its inject pipeline (NaN filter, floor gate, calibrator gate, precedence, plan, render) remain unchanged -- multi-source is entirely within the retriever closure

---

## Test Cases by Module

### 1. `src/context/retrieve/multi_source.rs` (NEW)

Core merge-and-rerank logic. All tests use in-memory `ScoredChunk` vectors (no SQLite needed). Follow the `scored()` helper pattern from `injector.rs` tests.

#### Helper functions needed

```rust
// Extend the existing scored() helper to accept source name
fn scored_from(id: &str, source: &str, score: f32) -> ScoredChunk { ... }
fn scored_from_lang(id: &str, source: &str, score: f32, lang: &str) -> ScoredChunk { ... }
```

#### Score Normalization

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `normalize_single_source_preserves_relative_order` | Min-max normalization within one source does not reorder candidates | Candidates with scores [0.3, 0.5, 0.9] normalize to [0.0, 0.333, 1.0] |
| `normalize_single_candidate_yields_one` | A source returning exactly one candidate normalizes its score to 1.0 | `normalized == 1.0` |
| `normalize_identical_scores_yields_one` | All candidates from a source have the same score | All normalize to 1.0 (0/0 edge case handled) |
| `normalize_two_sources_makes_scores_comparable` | Two sources with disjoint score ranges produce comparable normalized scores | Source A [0.1, 0.5] and Source B [0.8, 0.9] are both mapped to [0.0, 1.0] |
| `normalize_zero_range_source_all_ones` | Source where min == max (all identical scores) | All candidates normalize to 1.0 |

#### Boost Application

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `current_repo_boost_applied` | Chunks from the current repo receive the current-repo boost | Score multiplied by 1.3 (or configured value) |
| `dep_manifest_boost_applied` | Chunks from a declared dependency receive the dep boost | Score multiplied by 1.2 (or configured value) |
| `lang_match_boost_applied` | Chunks from a same-language source receive the lang boost | Score multiplied by 1.1 (or configured value) |
| `boosts_compose_multiplicatively` | A chunk receiving all three boosts gets the product | 1.3 * 1.2 * 1.1 = 1.716x on the normalized score |
| `source_weight_applied_as_multiplier` | Source weight from sources.toml modulates the final score | Weight=2 doubles the boosted normalized score |
| `no_boost_for_non_matching_source` | Chunk from unrelated source receives no boost multipliers | Score = normalized * source_weight only |
| `custom_boost_values_from_config` | Override default boosts via `MultiSourceConfig` | Custom values (e.g., 1.5, 1.4, 1.2) applied instead of defaults |

#### Diversity Constraints

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `current_repo_reserved_slots_filled` | When current-repo chunks exist, reserved slots are filled first | First N slots in final top-k contain current-repo chunks (N=`current_repo_reserved`) |
| `current_repo_reserved_not_backfilled_below_threshold` | Reserved slots only accept chunks above `inject_min_score` | If only 1 current-repo chunk survives min_score with 2 reserved, only 1 slot filled |
| `per_source_cap_limits_non_current_sources` | Non-current sources cannot exceed `per_source_cap` in final top-k | Source with 10 high-scoring chunks capped at 3 (default) |
| `current_repo_exempt_from_per_source_cap` | Current-repo is not subject to the per_source_cap | Current repo can contribute more than `per_source_cap` chunks |
| `reserved_slots_zero_disables_reservation` | `current_repo_reserved = 0` means no guaranteed slots | Pure score-based ranking with only per_source_cap |
| `diversity_plus_top_k_truncation` | After diversity constraints, final list is truncated to `inject_max_chunks` | Result length <= inject_max_chunks |
| `all_from_one_source_capped_remainder_from_others` | One dominant source is capped, remaining slots go to next-best | 3 from source A (cap), remaining from source B sorted by score |

#### Merge Ordering

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `final_order_is_boosted_normalized_score_descending` | After all boosts and diversity, results sorted by final score desc | `result[i].final_score >= result[i+1].final_score` for all i |
| `tie_breaking_deterministic` | Two chunks with identical final scores produce stable ordering | Repeated calls produce the same order (tie-break by chunk id or source name) |

#### Edge Cases

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `empty_candidates_returns_empty` | All sources return zero candidates | Empty Vec returned, no panic |
| `single_source_single_chunk` | Only one source with one chunk | That chunk returned with normalized score 1.0 |
| `all_chunks_below_min_score` | Every candidate is below `inject_min_score` before normalization | Empty result (min_score applied to source-local scores per spec) |
| `nan_scores_in_candidates_filtered` | Some candidates have NaN scores from upstream | NaN chunks excluded before normalization |
| `inf_scores_handled_gracefully` | A candidate has `f32::INFINITY` score | Does not panic; infinity sorts to top but is capped by diversity |
| `zero_sources_returns_empty` | Zero valid sources passed to merge_and_rerank | Empty result, no panic |
| `max_sources_queried_respected` | More sources available than `max_sources_queried` allows | Only first N sources (by priority) are queried |

---

### 2. `src/context/config.rs`

Config parsing for new fields. Follow the existing `from_str` / TOML-literal pattern.

#### New Fields on RawSource

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `source_with_provides_parses` | `provides = ["ha-core", "home-assistant-core"]` on a source entry | `SourceEntry.provides` contains both aliases |
| `source_with_include_for_parses` | `include_for = ["quorum-self"]` restricts injection scope | `SourceEntry.include_for` == `["quorum-self"]` |
| `source_with_exclude_for_parses` | `exclude_for = ["blockcraft"]` blocks injection for that project | `SourceEntry.exclude_for` == `["blockcraft"]` |
| `source_without_new_fields_still_parses` | Old-style `[[source]]` with only name/kind/path/weight | Parses without error; new fields default to empty vecs |
| `deny_unknown_fields_removed_from_raw_source` | Adding an unknown field to `[[source]]` no longer errors | TOML with `some_future_field = true` on source parses (previously would fail) |
| `deny_unknown_fields_kept_on_raw_config` | Unknown top-level field still rejected | TOML with `[bogus_section]` fails to parse |
| `deny_unknown_fields_kept_on_raw_context` | Unknown field in `[context]` still rejected | TOML with `context.bogus = 1` fails to parse |

#### New `[context.multi_source]` Sub-table

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `multi_source_defaults_when_absent` | No `[context.multi_source]` in TOML | `MultiSourceConfig` defaults: enabled=true, max_sources_queried=10, per_source_cap=3, current_repo_reserved=2, boosts=1.3/1.2/1.1 |
| `multi_source_all_fields_parse` | Complete `[context.multi_source]` block with all overrides | All fields populated with the specified values |
| `multi_source_partial_override` | Only `per_source_cap = 5` specified | per_source_cap=5, all other fields at defaults |
| `multi_source_enabled_false_disables` | `enabled = false` in multi_source block | `MultiSourceConfig.enabled == false` |
| `multi_source_boost_validation_rejects_zero` | `current_repo_boost = 0.0` | Config error: boost must be positive |
| `multi_source_boost_validation_rejects_negative` | `dep_manifest_boost = -1.0` | Config error: boost must be positive |
| `multi_source_max_sources_zero_rejected` | `max_sources_queried = 0` | Config error: must be > 0 |
| `old_config_without_multi_source_parses` | Full valid old-style sources.toml | Parses cleanly; multi_source at defaults |

#### SourceEntry Enrichment

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `source_entry_exposes_provides` | After config parse, SourceEntry has `provides: Vec<String>` | Accessible for dep matching |
| `source_entry_exposes_include_exclude` | After parse, include_for/exclude_for available | Accessible for bootstrap filtering |

---

### 3. `src/context/bootstrap.rs`

Integration tests for the multi-source wiring. Follow the existing pattern of building fixture indexes with `IndexBuilder` + `HashEmbedder`.

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `multi_source_injector_queries_both_sources` | Two fixture sources indexed; review retrieves chunks from both | `telemetry.sources_queried >= 2`, `telemetry.injected_sources` contains both source names |
| `multi_source_disabled_falls_back_to_single` | `[context.multi_source] enabled = false` with two indexed sources | Only the first valid source is queried (single-source behavior) |
| `include_for_filters_eligible_sources` | Source A has `include_for = ["project-x"]`, reviewing project-y | Source A excluded from the eligible source list |
| `exclude_for_filters_eligible_sources` | Source B has `exclude_for = ["project-y"]`, reviewing project-y | Source B excluded |
| `exclude_for_wins_over_include_for` | Source has both `include_for = ["x"]` and `exclude_for = ["x"]` | Source excluded (exclude wins) |
| `current_repo_detection_path_source` | Reviewing a file inside an indexed path-source | That source flagged as `is_current_repo`, boost applied |
| `current_repo_detection_git_source_never_matches` | All sources are git-backed | No source flagged as current repo |
| `dep_manifest_matched_to_sources` | Project has Cargo.toml with `serde` dep, source named "serde" exists | "serde" source receives dep boost |
| `dep_manifest_provides_alias_matched` | Source declares `provides = ["ha-core"]`, Cargo.toml has `ha-core` dep | Source matched via alias |
| `dep_manifest_failure_proceeds_without_boost` | Project dir has unreadable Cargo.toml | Review proceeds; dep_boost_applied = 0 in telemetry |
| `canonicalize_failure_skips_current_repo_flag` | Source path is a non-existent directory | Source not flagged as current repo; review proceeds |
| `max_sources_queried_caps_source_count` | 15 sources indexed, max_sources_queried = 3 | Only 3 sources queried (telemetry.sources_queried == 3) |
| `source_query_order_current_first_then_weight` | Sources with different weights; one is current repo | Current repo queried first, then weight descending |
| `all_sources_fail_returns_empty_with_error_flag` | All indexed sources have corrupt DBs | `telemetry.retriever_errored == true`, rendered == None |
| `partial_source_failure_degrades_gracefully` | One source DB corrupt, one healthy | Healthy source contributes; corrupt one skipped with warning |
| `telemetry_populated_for_multi_source_review` | Two sources, successful review | All new telemetry fields populated: sources_queried, sources_contributing, per_source_contributions, dep_boost_applied, current_repo_reserved_available, current_repo_reserved_filled |

**Edge cases to consider in bootstrap tests:**
- Symlinked source paths and canonicalization
- Source with empty index (0 chunks) -- should not count as "contributing"
- Source ordering stability when weights are equal

---

### 4. `src/dep_manifest.rs`

New `match_sources_to_deps` helper function. Follow the existing `TempDir` + `write()` pattern.

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `match_exact_name_hits` | Source name "serde" matches dep name "serde" | "serde" in the returned HashSet |
| `match_provides_alias_hits` | Source provides `["ha-core"]`, dep name is "ha-core" | Source name in the returned HashSet |
| `match_multiple_aliases_any_hit` | Source provides `["a", "b", "c"]`, dep name is "b" | Source name in returned set |
| `no_match_returns_empty` | Source named "foo", deps are ["bar", "baz"] | Empty HashSet |
| `match_is_case_sensitive` | Source named "Serde", dep named "serde" | No match (names are case-sensitive per Rust/npm conventions) |
| `cargo_underscore_normalization_matches` | Source named "serde_json", Cargo.toml has "serde-json" | Match (parse_cargo already normalizes hyphens to underscores) |
| `empty_deps_returns_empty` | No manifest present, empty deps vec | Empty HashSet |
| `empty_sources_returns_empty` | Sources list empty, deps present | Empty HashSet |
| `multiple_sources_matched` | Three sources, two are deps | Both dep-matching sources in the returned set |
| `provides_does_not_override_name_match` | Source named "x" with provides=["y"], dep is "x" | Source matched by name (provides is additive) |

---

### 5. `src/review_log.rs`

Telemetry backward compatibility. Follow the existing `serde_json::from_str` pattern for legacy records.

#### New Fields on ContextTelemetry

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `new_multi_source_telemetry_fields_round_trip` | Serialize and deserialize a ContextTelemetry with all new fields populated | All values preserved: sources_queried, sources_contributing, per_source_contributions (BTreeMap), dep_boost_applied, current_repo_reserved_available, current_repo_reserved_filled |
| `legacy_context_telemetry_deserializes_with_defaults` | JSON string missing all new fields | New fields default: sources_queried=0, sources_contributing=0, per_source_contributions=empty, dep_boost_applied=0, current_repo_reserved_available=0, current_repo_reserved_filled=0 |
| `per_source_contributions_serializes_as_map` | BTreeMap with {"mini-rust": 2, "serde": 1} | JSON contains `"per_source_contributions":{"mini-rust":2,"serde":1}` |
| `per_source_contributions_empty_map_defaults` | Missing from JSON | Defaults to empty BTreeMap |
| `new_telemetry_fields_populated_in_review_record` | ReviewRecord with populated new ContextTelemetry written to log | Round-trips through ReviewLog.record() / load_all() |
| `mixed_old_new_records_in_log` | Log file with one legacy record and one new record | Both deserialize; legacy has zero defaults, new has populated values |

#### Backward Compat Edge Cases

| Test Name | What it tests | Expected Outcome |
|-----------|--------------|-----------------|
| `completely_empty_context_object_deserializes` | `"context": {}` in JSON | All fields at Default values |
| `unknown_future_field_in_context_ignored` | `"context": {"sources_queried": 5, "future_field": true}` | Deserializes without error (serde default + deny_unknown_fields NOT on ContextTelemetry) |
| `new_fields_skip_serialized_when_default` | ContextTelemetry with sources_queried=0 and empty per_source_contributions | Ideally omitted from JSON to save disk (if using skip_serializing_if); if not, test that the zero values round-trip |

---

## Implementation Notes for TDD Agent

### General Patterns (from existing codebase)

1. **Test module structure**: All tests go in `#[cfg(test)] mod tests { ... }` at the bottom of the source file.

2. **Helper construction**: Use small helper fns (`scored()`, `mk_chunk()`, `ctx_with_min_score()`) to build fixtures. Minimize boilerplate per test.

3. **Temp directories**: Use `tempfile::tempdir()` for any filesystem state. Never write to real `~/.quorum/`.

4. **Assert style**: Use `assert_eq!` with explanatory format strings. For floating point, use `(actual - expected).abs() < epsilon`.

5. **No unwrap in assertions**: Prefer `assert!(result.is_some(), "...")` over `result.unwrap()` for test clarity.

6. **Naming**: `snake_case`, descriptive, starting with the behavior under test. Examples from codebase: `returns_none_when_sources_toml_missing`, `gate_applies_config_min_score_before_calibrator`.

### New Type Signatures Expected

```rust
// src/context/retrieve/multi_source.rs
pub struct MultiSourceConfig {
    pub enabled: bool,
    pub max_sources_queried: u32,
    pub per_source_cap: u32,
    pub current_repo_reserved: u32,
    pub current_repo_boost: f32,
    pub dep_manifest_boost: f32,
    pub lang_match_boost: f32,
}

pub struct SourceCandidate {
    pub source_name: String,
    pub chunks: Vec<ScoredChunk>,
    pub is_current_repo: bool,
    pub is_dependency: bool,
    pub kind: SourceKind,
    pub weight: f32,
}

pub fn merge_and_rerank(
    candidates: Vec<SourceCandidate>,
    config: &MultiSourceConfig,
    reviewed_file_lang: Option<&str>,
    inject_min_score: f32,
    inject_max_chunks: u32,
) -> Vec<ScoredChunk>;

// src/dep_manifest.rs
pub fn match_sources_to_deps(
    sources: &[SourceEntry],
    deps: &[Dependency],
) -> HashSet<String>;

// src/context/config.rs (new fields on SourceEntry)
pub struct SourceEntry {
    // ... existing fields ...
    pub provides: Vec<String>,
    pub include_for: Vec<String>,
    pub exclude_for: Vec<String>,
}
```

### Dependency Order for Implementation

```
1. src/context/config.rs         -- parse new fields first (no deps)
2. src/review_log.rs             -- add telemetry fields (no deps)
3. src/dep_manifest.rs           -- match_sources_to_deps (deps: config.rs)
4. src/context/retrieve/multi_source.rs  -- merge_and_rerank (deps: config.rs, retrieve types)
5. src/context/bootstrap.rs      -- wire it all together (deps: all above)
```

### Test Count Estimate

| Module | Unit Tests | Integration Tests | Total |
|--------|-----------|-------------------|-------|
| multi_source.rs | 21 | 0 | 21 |
| config.rs | 15 | 0 | 15 |
| bootstrap.rs | 0 | 16 | 16 |
| dep_manifest.rs | 10 | 0 | 10 |
| review_log.rs | 9 | 0 | 9 |
| **Total** | **55** | **16** | **71** |

### Risk Areas Requiring Extra Attention

1. **Score normalization with degenerate inputs**: Zero range, single candidate, all-NaN, all-identical. The `0/0` division must be handled.
2. **Diversity constraint interaction with reserved slots**: The reservation + cap logic has subtle ordering requirements. Test the reservation fill before the general top-k selection.
3. **Backward compatibility**: Both config parsing (old TOML without new fields) and telemetry deserialization (old JSON without new fields) must remain seamless.
4. **Current-repo detection**: Path canonicalization can fail on symlinks, deleted directories, or permission issues. Each failure mode should degrade gracefully.
5. **`exclude_for` wins**: The spec says exclude wins when both match. This is a single `if` branch but easy to get wrong with set operations.
