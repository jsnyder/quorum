# Architectural Review: SQL Optimization for `quorum stats`

**Date:** 2026-05-14
**Reviewer:** Oracle (Claude Opus 4.6)
**Scope:** Planned SQL push-down optimizations for the post-JSONL-migration SQLite-backed stats pipeline

---

## 1. Additional SQL Optimizations You Are Missing

### 1a. `load_since()` on TelemetryStore Loads Everything

The current `load_since()` at `src/telemetry.rs:209` is:

```rust
pub fn load_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<TelemetryEntry>> {
    Ok(self.load_all_with_stats()?.0
        .into_iter()
        .filter(|e| e.ts >= since)
        .collect())
}
```

This loads ALL telemetry from SQLite, deserializes every row (including JSON columns), then filters in Rust. You identified this as optimization #2 but the current implementation has not yet been pushed to SQL. The fix is straightforward:

```sql
SELECT * FROM telemetry WHERE ts >= ?1 ORDER BY ts ASC
```

This is your highest-ROI change because `compute_report()` calls `load_since(since_7d)` and most of the telemetry table is older than 7 days.

### 1b. Dedicated `load_all_finding_ids()` for Linkage Stats

`analytics::linkage_stats()` builds a `HashSet<&str>` of all `finding_id`s from all reviews, then iterates feedback entries checking membership. Currently this requires `review_log.load_all()` which deserializes every review row including JSON context columns, just to extract finding_ids.

Add a dedicated method:

```sql
SELECT DISTINCT finding_id FROM review_finding_ids
```

This avoids deserializing every review row when all you need is the finding_ids set. Once feedback moves to SQLite, the entire linkage computation becomes a single SQL JOIN.

### 1c. `compute_report()` Loads All Reviews for Dashboard Highlights Only

At `src/stats.rs:148`, `review_log.load_all()` loads every review into memory solely to compute:
- Top 3 repos by count
- Top 3 callers by count
- Last 200 reviews for rolling windows

A `load_recent(200)` covers rolling windows. GROUP BY push-down covers repo/caller highlights. The full `load_all()` becomes unnecessary for the default dashboard.

### 1d. Model Distribution Is a Clean SQL Push-Down

At `src/stats.rs:135-147`, the most-frequent-model computation iterates all recent telemetry in Rust:

```sql
SELECT model, COUNT(*) AS cnt
FROM telemetry WHERE ts >= ?1
GROUP BY model ORDER BY cnt DESC LIMIT 1
```

---

## 2. GROUP BY Strategy: Option (b) Is Correct

**Recommendation: SQL GROUP BY for aggregate counts, Rust for derived metrics and sparklines.**

### Why Not (a): Full SQL

The `DimensionSlice` struct has 15 fields. Pushing everything to SQL means:

1. **Sparklines** require ordered within-group records and bucket-based sub-aggregation. Expressible with `NTILE` window functions but ugly and brittle in SQLite.
2. **`findings_per_kloc`** needs conditional NULL-handling logic on `lines_added`/`lines_removed`.
3. **Suppression rate** requires `json_each()` correlated subqueries on `suppressed_by_rule`.

Each alone is manageable, but 15 metrics composed into one SELECT creates a monster query that is hard to test, debug, and maintain -- for negligible performance gain at your scale.

### Why (b) Works

Push to SQL:
```sql
SELECT
    COALESCE(repo, '(no repo)') AS repo_key,
    COUNT(*) AS n_reviews,
    SUM(critical) AS critical, SUM(high) AS high,
    SUM(medium) AS medium, SUM(low) AS low, SUM(info) AS info,
    SUM(critical + high + medium + low + info) AS n_findings,
    SUM(files_reviewed) AS files_reviewed,
    SUM(COALESCE(lines_added, 0) + COALESCE(lines_removed, 0)) AS lines_touched,
    SUM(CASE WHEN lines_added IS NOT NULL OR lines_removed IS NOT NULL THEN 1 ELSE 0 END) AS has_lines_count,
    SUM(tokens_in) AS tokens_in, SUM(tokens_out) AS tokens_out,
    SUM(tokens_cache_read) AS tokens_cache_read,
    SUM(duration_ms) AS duration_total_ms,
    SUM((SELECT COALESCE(SUM(je.value), 0) FROM json_each(suppressed_by_rule) je)) AS suppressed
FROM reviews
GROUP BY repo_key
ORDER BY n_reviews DESC
```

Compute in Rust: `findings_per_file`, `findings_per_kloc`, `suppression_rate`, `cache_hit_rate`, `avg_duration_ms`, sparklines.

### Sparkline Tension

Sparklines need ordered per-group records. Two-query approach:

1. GROUP BY query for aggregates (above)
2. Filtered SELECT for sparkline data on groups that passed MIN_SAMPLE:

```sql
SELECT repo, files_reviewed, critical+high+medium+low+info AS n_findings
FROM reviews WHERE repo IN (?1, ?2, ?3)
ORDER BY timestamp ASC
```

For the default dashboard (3 repos), this is cheap.

---

## 3. Concerns and Correctness Risks

### 3a. ISO 8601 Text Comparison (Worth Verifying)

SQLite string comparison is lexicographic, which works for ISO 8601 **if and only if all timestamps use the same suffix format**. `2026-05-14T10:30:00+00:00` sorts AFTER `2026-05-14T10:30:00Z` because `+` (0x2B) > `Z` (0x5A) is false -- actually `+` is 0x2B and `Z` is 0x5A, so `+` sorts BEFORE `Z`. The real issue: `+00:00` expands the string length, making `2026-05-14T10:30:00+00:00` sort differently than `2026-05-14T10:30:00Z`.

I checked: your insert path uses `entry.timestamp.to_rfc3339()` which for `DateTime<Utc>` always produces `Z` suffix. Migrated JSONL data also passes through chrono deserialization. **This is safe** as long as no external tooling writes timestamps in `+00:00` format. Low risk but worth documenting.

### 3b. `COALESCE(lines_added, 0)` Preserves Semantics

The current Rust `aggregate()` only counts `lines_touched` when at least one of `lines_added`/`lines_removed` is `Some`. The `has_lines_count` column in the GROUP BY query preserves this gate. Use `has_lines_count > 0` in Rust to decide whether to compute `findings_per_kloc`. Semantically identical.

### 3c. Rolling Window Edge Case

`dimensions::rolling_window()` indexes from the end of the sorted array. With SQL, use `ORDER BY timestamp DESC LIMIT ? OFFSET ?`. The edge case: if records share the same timestamp, the window boundaries become non-deterministic without a tiebreaker. Add `run_id` as a secondary sort:

```sql
ORDER BY timestamp DESC, run_id DESC
```

ULIDs are time-ordered, so this is a natural tiebreaker.

### 3d. No Foreign Key Enforcement

`review_finding_ids` has `REFERENCES reviews(run_id)` but `PRAGMA foreign_keys` is not enabled in `initialize()`. At your scale and single-writer model, orphan rows are harmless. But if you ever add `DELETE FROM reviews`, enable FK enforcement or add `ON DELETE CASCADE`.

---

## 4. Index Recommendations

### Add This Index

```sql
CREATE INDEX idx_reviews_timestamp ON reviews(timestamp);
```

Every query pattern sorts or filters by timestamp: `load_all()` orders by it, `load_recent(n)` needs `ORDER BY DESC LIMIT n`, rolling windows need the same. Without an index, every query does a full table sort. The index also enables the planner to satisfy `LIMIT` queries without sorting all rows.

### Do Not Add These Indexes (Yet)

- **`reviews.repo`**: GROUP BY needs all rows regardless. An index only helps `WHERE repo = ?` which you do not do.
- **`reviews.invoked_from`**: Same reasoning.
- **`telemetry.model`**: Only used in GROUP BY on filtered set. Not worth it.
- **Covering indexes**: A wide index on `(repo, critical, high, ...)` doubles write cost for negligible read benefit at 5000 rows.

---

## 5. Schema Notes

### Merge Conflict in Design Spec

`docs/superpowers/specs/2026-05-13-sqlite-migration-design.md` has an unresolved git merge conflict marker (`<<<<<<< Updated upstream` / `>>>>>>> Stashed changes`). Should be cleaned up.

### JSON Columns in GROUP BY Queries

`telemetry.files` and `telemetry.findings` are JSON columns that are never queried by SQL -- only deserialized back to Rust. In GROUP BY queries, use explicit column lists (not `SELECT *`) to avoid pulling these blobs.

---

## 6. Prioritized Action Plan

| Priority | Change | ROI | Effort |
|----------|--------|-----|--------|
| 1 | `load_since()` SQL WHERE clause for TelemetryStore | High | Low |
| 2 | Add `idx_reviews_timestamp` index | High | Trivial |
| 3 | `load_recent(n)` for ReviewLog | High | Low |
| 4 | `load_all_finding_ids()` for linkage stats | Medium | Low |
| 5 | GROUP BY for `--by-repo` / `--by-caller` | Medium | Medium |
| 6 | SQL model distribution query | Low | Low |
| 7 | Sparklines via 2nd filtered query | Low | Medium |
| 8 | `suppressed_total` denormalized column | Low | Low + migration |

Items 1-4 are clear wins with immediate benefit and minimal risk. Item 5 is the largest structural change. Items 6-8 can wait until profiling shows need.
