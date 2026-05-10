# Test Plan: Stats Hotspot Files View (`--by-file`) -- Issue #18

**Feature:** `quorum stats --by-file [--top N]`
**Scope:** Unit tests in `src/review_log.rs`, `src/dimensions.rs`, `src/stats.rs`, `src/cli/mod.rs`; integration test in `tests/`.
**Test runner:** `cargo test --bin quorum`

---

## Module 1: Schema & Serialization (`src/review_log.rs`)

### T1.1 `review_record_with_per_file_roundtrips_through_serde`

Construct a `ReviewRecord` with a non-empty `per_file` HashMap (two entries: `"src/main.rs"` with 1 critical + 2 high, `"src/lib.rs"` with 3 medium). Serialize to JSON with `serde_json::to_string`, then deserialize back. Assert the deserialized `per_file` map has 2 keys and each `SeverityCounts` field matches the original.

**Why:** Proves the new field survives a JSON round-trip and serde attributes (`default`, `skip_serializing_if`) are correct.

### T1.2 `legacy_record_without_per_file_deserializes_with_empty_map`

Build a JSON string from an existing ReviewRecord (omitting the `per_file` key entirely). Deserialize it as `ReviewRecord`. Assert `per_file.is_empty()`.

**Why:** Backward compatibility -- existing `reviews.jsonl` rows written before this feature must not break deserialization. This is the `#[serde(default)]` contract.

### T1.3 `per_file_omitted_from_json_when_empty`

Construct a `ReviewRecord` with `per_file: HashMap::new()`. Serialize to JSON. Assert the serialized string does NOT contain the key `"per_file"`.

**Why:** Validates `skip_serializing_if = "HashMap::is_empty"` -- keeps legacy-compatible output lean.

---

## Module 2: Per-file Count Builder (`src/review_log.rs`)

### T2.1 `build_per_file_counts_maps_findings_to_severity_counts`

Create a `Vec<FileReviewResult>` with 3 entries:
- `"src/a.rs"`: findings with severities [Critical, High, High]
- `"src/b.rs"`: findings with severities [Medium, Low]
- `"src/c.rs"`: no findings (empty vec)

Call `build_per_file_counts(&results)`. Assert:
- Map has 3 keys
- `"src/a.rs"` => critical=1, high=2, medium=0, low=0, info=0
- `"src/b.rs"` => critical=0, high=0, medium=1, low=1, info=0
- `"src/c.rs"` => all zeros (total=0)

**Why:** Core mapping logic. The "zero findings" case proves that files with no findings still appear in the map (they were reviewed, just clean).

### T2.2 `build_per_file_counts_empty_input_yields_empty_map`

Call `build_per_file_counts(&[])`. Assert result is empty.

**Why:** Degenerate input guard.

### T2.3 `build_per_file_counts_uses_file_path_not_finding_fields`

Create a `FileReviewResult` where `file_path = "src/correct.rs"` and the findings have no file_path field (Finding has no such field). Assert the map key is `"src/correct.rs"`.

**Why:** Validates the plan's note that file_path comes from `FileReviewResult`, not from `Finding`.

---

## Module 3: Aggregation (`src/dimensions.rs`)

### T3.1 `group_by_file_aggregates_across_multiple_reviews`

Create 3 `ReviewRecord`s:
- Review 1: `per_file = {"src/hot.rs": {critical:1, high:0, ...}, "src/cold.rs": {high:1}}`
- Review 2: `per_file = {"src/hot.rs": {critical:0, high:2}}`
- Review 3: `per_file = {"src/hot.rs": {critical:1, high:0}, "src/new.rs": {medium:1}}`

Call `group_by_file(&records, None)`. Assert:
- `"src/hot.rs"`: review_count=3, severity_mix.critical=2, severity_mix.high=2
- `"src/cold.rs"`: review_count=1, severity_mix.high=1
- `"src/new.rs"`: review_count=1, severity_mix.medium=1

**Why:** Core aggregation -- same file across different reviews must sum severity counts and count distinct reviews.

### T3.2 `group_by_file_sorts_by_critical_then_high_then_total`

Create records producing these aggregated files:
- `"a.rs"`: critical=0, high=5, medium=0 (total=5)
- `"b.rs"`: critical=2, high=0, medium=0 (total=2)
- `"c.rs"`: critical=0, high=5, medium=3 (total=8)
- `"d.rs"`: critical=2, high=1, medium=0 (total=3)

Call `group_by_file(&records, None)`. Assert order is: `["b.rs", "d.rs", "c.rs", "a.rs"]` (critical desc, then high desc, then total desc).

Specifically:
- b.rs and d.rs both have critical=2, but d.rs has high=1 vs b.rs high=0, so d.rs before... wait, it should be: within same critical count (2), sort by high desc. d.rs has high=1, b.rs has high=0 => d.rs first. Then c.rs and a.rs both have critical=0, c.rs has high=5, a.rs has high=5 -- tie. c.rs total=8 > a.rs total=5, so c.rs first.

Corrected order: `["d.rs", "b.rs", "c.rs", "a.rs"]`.

**Why:** Sorting contract is the core value proposition -- users need the most severe hotspots at the top.

### T3.3 `group_by_file_top_n_limits_output`

Create records producing 5 distinct file hotspots. Call `group_by_file(&records, Some(2))`. Assert exactly 2 results returned, and they are the top-2 by the severity sort order.

**Why:** `--top N` truncation contract.

### T3.4 `group_by_file_top_n_none_returns_all`

Same 5 files. Call `group_by_file(&records, None)`. Assert 5 results returned.

**Why:** Explicit test that None means unlimited.

### T3.5 `group_by_file_top_n_larger_than_count_returns_all`

3 files. Call `group_by_file(&records, Some(100))`. Assert 3 results (no panic, no padding).

**Why:** Edge case -- top_n exceeds available files.

### T3.6 `group_by_file_skips_legacy_records_without_per_file`

Create 2 records: one with `per_file` data, one with `per_file: HashMap::new()` (simulating a legacy record). Call `group_by_file`. Assert only the file from the first record appears. Assert no panic or error from the empty-map record.

**Why:** Legacy safety -- old reviews.jsonl rows have no per_file.

### T3.7 `group_by_file_empty_input_yields_empty_output`

Call `group_by_file(&[], None)`. Assert empty.

**Why:** Degenerate input guard, matching the pattern in existing `group_by_repo_empty_input_yields_empty_output`.

### T3.8 `group_by_file_review_count_counts_reviews_not_findings`

Create 2 records each containing `"src/x.rs"` with varying finding counts (review 1: 3 findings, review 2: 1 finding). Assert review_count=2 (not 4).

**Why:** Validates that review_count reflects distinct review invocations, not finding count.

### T3.9 `group_by_file_last_reviewed_is_max_timestamp`

Create 3 records for the same file with timestamps T1 < T2 < T3. Assert `last_reviewed == T3`.

**Why:** Users need to know recency -- when was this hotspot last reviewed?

### T3.10 `group_by_file_last_reviewed_not_affected_by_insertion_order`

Same 3 timestamps but records inserted in order T2, T3, T1. Assert `last_reviewed == T3`.

**Why:** Implementation must use max(), not "last seen".

### T3.11 `group_by_file_low_sample_flag_below_min_sample`

Create 3 records for `"src/x.rs"` (below `MIN_SAMPLE=5`). Assert `low_sample == true`.

**Why:** Consistency with existing dimension views.

### T3.12 `group_by_file_low_sample_false_at_min_sample`

Create exactly `MIN_SAMPLE` (5) records for a single file. Assert `low_sample == false`.

**Why:** Boundary condition on the sample gate.

### T3.13 `group_by_file_single_file_single_review`

One record with one file entry. Assert: review_count=1, severity_mix matches, low_sample=true.

**Why:** Minimal non-empty case.

### T3.14 `group_by_file_all_files_same_severity_stable_sort`

Create 3 files all with identical severity counts (critical=1, high=1). Call `group_by_file`. Assert all 3 appear (no duplicates lost). The exact tiebreak order (alphabetical by path) should be verified if the implementation defines one, or assert that the output is a permutation of the input files.

**Why:** When severity is tied, the output must still be deterministic and complete.

---

## Module 4: CLI Flag Parsing (`src/cli/mod.rs`)

### T4.1 `by_file_flag_parsed`

Parse `["quorum", "stats", "--by-file"]` via clap. Assert `opts.by_file == true`.

**Why:** Basic flag registration.

### T4.2 `top_flag_parsed_with_value`

Parse `["quorum", "stats", "--by-file", "--top", "10"]`. Assert `opts.by_file == true`, `opts.top == Some(10)`.

**Why:** `--top` value parsing.

### T4.3 `top_flag_absent_yields_none`

Parse `["quorum", "stats", "--by-file"]`. Assert `opts.top.is_none()`.

**Why:** Default behavior: show all.

### T4.4 `top_flag_without_by_file_is_accepted`

Parse `["quorum", "stats", "--top", "5"]`. Should parse without error (the routing logic decides whether to use it, not clap). Assert `opts.top == Some(5)`, `opts.by_file == false`.

**Why:** Validates that `--top` is not gated by clap requires (it is semantically gated in the routing code). Alternatively, if the implementation adds a `requires = "by_file"` constraint on `--top`, this test should verify that the parse fails with a helpful error. Document whichever behavior the implementation chooses.

### T4.5 `by_file_is_mutually_exclusive_with_classic_dims`

Parse `["quorum", "stats", "--by-file", "--by-repo"]`. Expected behavior: either clap rejects it (conflicts_with), or the routing code picks one. If the plan does not enforce mutual exclusion at the clap level, this test documents that `--by-file` takes priority (or whatever the implementation decides). Assert the expected behavior.

**Why:** Users should not get confusing output from conflicting flags.

---

## Module 5: Formatter (`src/stats.rs`)

### T5.1 `format_file_hotspot_table_human_output_has_expected_columns`

Build a `Vec<FileHotspotSlice>` with 2 entries. Call `format_file_hotspot_table` in human mode. Assert the output string contains the column headers: `"File"`, `"Reviews"`, `"Crit"`, `"High"`, `"Med"`, `"Low"`, `"Last Reviewed"`.

**Why:** Column contract for human-readable output.

### T5.2 `format_file_hotspot_table_human_output_contains_file_paths`

Same data. Assert the output contains both file path strings.

**Why:** Sanity check that data rows render.

### T5.3 `format_file_hotspot_table_json_output_roundtrips`

Build a `Vec<FileHotspotSlice>`, serialize via the JSON formatter path. Parse the output as `serde_json::Value`. Assert:
- Top-level has `"mode": "by-file"` and `"slices"` array
- Each slice has fields: `file_path`, `review_count`, `last_reviewed`, `severity_mix`, `low_sample`
- `severity_mix` has fields: `critical`, `high`, `medium`, `low`, `info`

**Why:** JSON output contract for downstream consumers (CI scripts, dashboards).

### T5.4 `format_file_hotspot_table_compact_output_is_single_line_per_file`

Build 3 slices. Call the compact formatter. Assert the output has exactly 3 non-empty lines. Assert each line contains the file path.

**Why:** Compact mode contract for LLM consumption.

### T5.5 `format_file_hotspot_table_empty_slices`

Call formatter with empty slice vec. Assert output is not an error. Assert it either produces an empty string, a "no data" message, or an empty JSON array (depending on format).

**Why:** Edge case -- no hotspot data (e.g., all legacy records).

### T5.6 `format_file_hotspot_table_low_sample_annotation`

Build a slice with `low_sample=true`. In human output, assert the row contains a low-sample indicator (e.g., `"*"` suffix or `"[low sample]"` annotation, matching whatever convention the existing dimension tables use).

**Why:** Users must be warned when the sample size is below the confidence gate.

---

## Module 6: Routing (`src/main.rs`)

### T6.1 `stats_by_file_routes_to_group_by_file`

This is hard to unit-test directly in main.rs (it's a dispatch block). Validate via integration test (Module 7). At the unit level, this test verifies that `want_classic_dim` is `false` and `want_file_dim` (or equivalent) is `true` when `opts.by_file == true`.

If the routing uses a helper function, test that function. If it's inline in main(), rely on the integration test.

**Why:** Routing correctness.

### T6.2 `stats_by_file_passes_top_n_to_group_by_file`

Verify that `opts.top` is forwarded as the `top_n` parameter. Integration test (Module 7) covers this.

**Why:** Ensures `--top N` propagates.

---

## Module 7: Integration Tests (`tests/`)

### T7.1 `stats_by_file_json_output_end_to_end`

Setup: Create a temp directory. Write a `reviews.jsonl` with 3 ReviewRecords:
- Record 1: `per_file = {"src/hot.rs": {critical:2, high:1}, "src/clean.rs": {low:1}}`
- Record 2: `per_file = {"src/hot.rs": {high:3}}`
- Record 3: `per_file = {}` (legacy record, no per_file data)

Set `QUORUM_HOME` to the temp dir. Run `cargo run -- stats --by-file --json`.

Assert:
- Exit code 0
- Output parses as JSON
- `slices` array has 2 entries (`"src/hot.rs"` and `"src/clean.rs"`)
- `"src/hot.rs"` has `review_count: 2`, `severity_mix.critical: 2`, `severity_mix.high: 4`
- `"src/clean.rs"` has `review_count: 1`, `severity_mix.low: 1`
- Slices are sorted by severity (hot.rs first)
- Legacy record (3) contributed no files and caused no error

**Why:** End-to-end correctness across all layers.

### T7.2 `stats_by_file_top_n_truncates`

Same setup as T7.1 but with 4+ distinct files. Run `cargo run -- stats --by-file --top 2 --json`.

Assert:
- `slices` array has exactly 2 entries
- They are the top-2 by the severity sort order

**Why:** `--top` end-to-end.

### T7.3 `stats_by_file_compact_output`

Run `cargo run -- stats --by-file --compact`. Assert exit code 0. Assert output has one line per file.

**Why:** Compact mode end-to-end.

### T7.4 `stats_by_file_empty_review_log`

Set `QUORUM_HOME` to a temp dir with an empty `reviews.jsonl`. Run `cargo run -- stats --by-file --json`.

Assert:
- Exit code 0
- Output is valid JSON with empty slices array

**Why:** Graceful handling of empty state.

### T7.5 `stats_by_file_missing_review_log`

Set `QUORUM_HOME` to a temp dir with no `reviews.jsonl` file at all. Run `cargo run -- stats --by-file --json`.

Assert:
- Exit code 0 (or 3 if the implementation treats missing log as error -- document whichever)
- No panic

**Why:** First-run scenario where no reviews have been recorded yet.

---

## Risk Matrix

| Test | Risk Mitigated | Priority |
|------|---------------|----------|
| T1.2 | Backward compat breakage (production data loss) | P0 |
| T2.1 | Incorrect severity mapping (wrong hotspot ranking) | P0 |
| T3.1 | Cross-review aggregation errors | P0 |
| T3.2 | Wrong sort order (misleading hotspot ranking) | P0 |
| T3.6 | Legacy record crash | P0 |
| T7.1 | End-to-end integration failure | P0 |
| T1.1 | Schema correctness | P1 |
| T1.3 | JSON bloat on legacy data | P1 |
| T3.3 | top_n contract | P1 |
| T3.9 | Recency tracking | P1 |
| T4.1-4.3 | CLI contract | P1 |
| T5.3 | JSON output contract | P1 |
| T3.7 | Empty input guard | P2 |
| T3.11-12 | Low-sample gate | P2 |
| T3.14 | Sort stability | P2 |
| T5.5-5.6 | Formatter edge cases | P2 |
| T7.4-7.5 | Empty/missing state | P2 |

---

## Test Naming Convention

Follow the existing pattern in `dimensions.rs`: `snake_case` descriptive names prefixed with the function under test. Examples:
- `group_by_file_aggregates_across_multiple_reviews`
- `build_per_file_counts_maps_findings_to_severity_counts`
- `format_file_hotspot_table_json_output_roundtrips`

## Test Data Construction

Reuse and extend the existing `rec()` helper in `dimensions.rs::tests`. Add a variant `rec_with_per_file(repo, caller, per_file: HashMap<String, SeverityCounts>)` that sets `files_reviewed` to `per_file.len() as u32` for consistency. For `build_per_file_counts` tests, construct `FileReviewResult` structs directly using the existing `Finding` builder.

## Estimated Test Count

27 test functions across 7 modules. Approximately 30 minutes of implementation time given the existing test patterns to follow.
