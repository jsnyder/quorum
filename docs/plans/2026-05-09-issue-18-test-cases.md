# Test Cases: Stats --by-file (Issue #18)

> Acceptance criteria and concrete test cases for `group_by_file()`, CLI routing,
> and the dedicated formatter. Follows anti-pattern guidance: explicit timestamps,
> strong primary-sort assertions, loose tiebreaker assertions, empty-input edge cases.

---

## Acceptance Criteria

1. `quorum stats --by-file` produces a ranked list of files from the feedback store, sorted by TP count descending.
2. `--top N` limits output to the top N rows. Omitting `--top` shows all files.
3. Each row shows: file_path, tp_count, fp_count, wontfix_count, partial_count, total, last_seen.
4. `last_seen` is the maximum timestamp across all feedback entries for that file.
5. `context_misleading` verdicts are excluded from all verdict buckets (they are not TP/FP/partial/wontfix).
6. JSON output serializes `Vec<FileHotspotRow>` directly. Compact output is one-line-per-file.
7. Empty feedback produces an empty vec (no panic, no sentinel row).
8. `--by-file` is mutually compatible with `--json` and `--compact` but independent of `--by-repo`/`--by-caller`.

---

## Test Helper

**`fb(file, verdict, ts_str)` helper** -- analogous to `rec()` in dimensions.rs but for `FeedbackEntry`. Constructs a minimal FeedbackEntry with explicit `DateTime<Utc>` parsed from `ts_str` (e.g. "2026-01-15T00:00:00Z"). All other fields get safe defaults (empty strings, `Provenance::Human`, `None` for optional fields).

---

## Unit Tests: `group_by_file()`

### T01: `group_by_file_empty_input_yields_empty_output`
- Input: `&[]`
- Assert: result is empty vec.

### T02: `group_by_file_single_entry_produces_single_row`
- Input: 1 TP entry for "src/main.rs" at "2026-03-01T12:00:00Z".
- Assert: result has 1 row; tp_count=1, fp_count=0, wontfix_count=0, partial_count=0, total=1, file_path="src/main.rs", last_seen matches the explicit timestamp.

### T03: `group_by_file_aggregates_multiple_entries_for_same_file`
- Input: 4 entries for "src/lib.rs": 2 TP (ts "2026-01-10T...", "2026-03-20T..."), 1 FP (ts "2026-02-15T..."), 1 Partial (ts "2026-04-01T...").
- Assert: 1 row; tp_count=2, fp_count=1, partial_count=1, wontfix_count=0, total=4, last_seen="2026-04-01T...".

### T04: `group_by_file_separates_distinct_files`
- Input: entries for "a.rs" (2 TP) and "b.rs" (1 TP).
- Assert: result has 2 rows, distinct file_path values.

### T05: `group_by_file_wontfix_counted_separately`
- Input: 1 Wontfix entry for "src/config.rs".
- Assert: wontfix_count=1, tp_count=0, fp_count=0, partial_count=0, total=1.

### T06: `group_by_file_context_misleading_excluded_from_all_buckets`
- Input: 2 entries for "src/foo.rs": 1 TP, 1 ContextMisleading (with blamed_chunk_ids=["c1"]).
- Assert: tp_count=1, fp_count=0, wontfix_count=0, partial_count=0, total=1.
- Rationale: ContextMisleading is not a verdict on the finding quality; it blames the injected context. Counting it in any bucket would inflate totals and distort hotspot ranking.

### T07: `group_by_file_primary_sort_by_tp_count_desc`
- Input: "low.rs" with 1 TP, "high.rs" with 5 TPs, "mid.rs" with 3 TPs.
- Assert: result[0].file_path="high.rs", result[1].file_path="mid.rs", result[2].file_path="low.rs".
- Note: strong assertion on primary sort key ordering.

### T08: `group_by_file_tiebreaker_by_total_desc`
- Input: "a.rs" with 2 TP + 0 FP (total=2), "b.rs" with 2 TP + 3 FP (total=5).
- Assert: both rows have tp_count=2. The row with total=5 ("b.rs") appears first.
- Note: tiebreaker assertion -- if implementation changes tiebreaker to alphabetical, this test documents the expected behavior.

### T09: `group_by_file_top_n_limits_output`
- Input: 5 distinct files with varying TP counts.
- Call: `group_by_file(&entries, Some(2))`.
- Assert: result.len()==2; result contains the 2 files with highest TP counts.

### T10: `group_by_file_top_n_none_returns_all`
- Input: 5 distinct files.
- Call: `group_by_file(&entries, None)`.
- Assert: result.len()==5.

### T11: `group_by_file_top_n_larger_than_data_returns_all`
- Input: 3 distinct files.
- Call: `group_by_file(&entries, Some(100))`.
- Assert: result.len()==3 (no panic, no padding).

### T12: `group_by_file_top_n_zero_returns_empty`
- Input: 3 distinct files.
- Call: `group_by_file(&entries, Some(0))`.
- Assert: result is empty.

### T13: `group_by_file_last_seen_is_max_timestamp_per_file`
- Input: 3 entries for "src/x.rs" at "2026-01-01T00:00:00Z", "2026-06-15T12:00:00Z", "2026-03-10T08:00:00Z".
- Assert: last_seen equals the "2026-06-15T12:00:00Z" timestamp (not the last-inserted, but the chronologically latest).

### T14: `group_by_file_mixed_provenance_all_counted`
- Input: 2 entries for same file: 1 TP with Provenance::Human, 1 TP with Provenance::External{agent:"pal",...}.
- Assert: tp_count=2. (group_by_file does not filter by provenance.)

### T15: `group_by_file_file_paths_preserved_verbatim`
- Input: entry with file_path="/home/user/project/src/deep/nested/file.rs".
- Assert: result row has file_path exactly matching the input (no path normalization, no basename extraction).

---

## Unit Tests: CLI Parsing

### T16: `stats_by_file_flag_parsed`
- Parse: `["quorum", "stats", "--by-file"]`.
- Assert: `opts.by_file == true`, `opts.top == None`.

### T17: `stats_by_file_with_top_parsed`
- Parse: `["quorum", "stats", "--by-file", "--top", "10"]`.
- Assert: `opts.by_file == true`, `opts.top == Some(10)`.

### T18: `stats_top_without_by_file_accepted`
- Parse: `["quorum", "stats", "--top", "5"]`.
- Assert: parses without error. `opts.top == Some(5)`, `opts.by_file == false`.
- Rationale: `--top` is a general limiter; it should not require `--by-file`. If it has no effect without `--by-file`, that is a runtime no-op, not a parse error.

### T19: `stats_by_file_with_json_parsed`
- Parse: `["quorum", "stats", "--by-file", "--json"]`.
- Assert: both `opts.by_file` and `opts.json` are true.

### T20: `stats_by_file_with_compact_parsed`
- Parse: `["quorum", "stats", "--by-file", "--compact"]`.
- Assert: both `opts.by_file` and `opts.compact` are true.

---

## Unit Tests: Formatter (`format_file_hotspots`)

### T21: `format_file_hotspots_human_contains_headers`
- Input: 1 FileHotspotRow.
- Assert: output contains "File", "TPs", "FPs", "Wontfix", "Total", "Last seen" (column headers).

### T22: `format_file_hotspots_human_shows_values`
- Input: FileHotspotRow { file_path: "src/main.rs", tp_count: 5, fp_count: 2, wontfix_count: 1, partial_count: 0, total: 8, last_seen: fixed timestamp }.
- Assert: output contains "src/main.rs", "5", "2", "1", "8".

### T23: `format_file_hotspots_json_roundtrips`
- Input: Vec of 2 FileHotspotRows.
- Assert: output is valid JSON. Deserializing back to `Vec<FileHotspotRow>` produces the same data.

### T24: `format_file_hotspots_json_stable_field_names`
- Input: 1 FileHotspotRow.
- Assert: serialized JSON contains keys "file_path", "tp_count", "fp_count", "wontfix_count", "partial_count", "total", "last_seen". (Wire-format stability guard.)

### T25: `format_file_hotspots_empty_shows_no_data_message`
- Input: empty vec.
- Assert: human output contains "No file hotspot data" (or equivalent). JSON output is "[]".

### T26: `format_file_hotspots_compact_one_line_per_file`
- Input: Vec of 3 FileHotspotRows.
- Assert: output has exactly 3 non-empty lines (no headers, no separators).

### T27: `format_file_hotspots_compact_contains_file_path_and_tp_count`
- Input: FileHotspotRow with file_path="src/lib.rs", tp_count=7.
- Assert: at least one line contains both "src/lib.rs" and "7".

---

## Integration Test

### T28: `stats_by_file_integration`
- Setup: write a temporary feedback.jsonl with at least 6 entries across 3 files. Use explicit, distinct verdicts and timestamps. Include at least one file with multiple TPs and one file with only FPs.
- Run: `quorum stats --by-file --json` with HOME pointing at tempdir.
- Assert:
  - Exit code 0.
  - Output parses as valid JSON array.
  - Array length equals 3 (one row per file, not filtered out).
  - The file with the most TPs appears as the first element.
  - `tp_count` and `fp_count` for each file match expected values (non-trivial, not just >0).
  - `last_seen` for each file matches the expected max timestamp from the fixture data.

### T29: `stats_by_file_top_integration`
- Setup: same fixture as T28.
- Run: `quorum stats --by-file --top 1 --json`.
- Assert: JSON array has exactly 1 element. It is the file with the highest TP count.

### T30: `stats_by_file_empty_feedback_integration`
- Setup: empty (or missing) feedback.jsonl.
- Run: `quorum stats --by-file`.
- Assert: exit code 0, output contains "No file hotspot data" (or JSON "[]" with --json).

---

## Edge Cases Captured

| Edge case | Covered by |
|-----------|-----------|
| Empty input | T01, T25, T30 |
| Single entry | T02 |
| ContextMisleading excluded | T06 |
| Timestamp is max, not last-inserted | T13 |
| top_n=0 | T12 |
| top_n > data size | T11 |
| File paths not normalized | T15 |
| Mixed provenance counted | T14 |
| Tiebreaker sort | T08 |
| JSON wire-format stability | T24 |
