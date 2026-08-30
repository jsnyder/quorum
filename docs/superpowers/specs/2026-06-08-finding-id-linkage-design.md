# Finding ID Linkage Design

**Issue:** #436

## Problem

Join health is at 0%. Every `FeedbackEntry` has `finding_id: None` because no recording path populates it. The schema exists (`ReviewRecord.finding_ids`, `FeedbackEntry.finding_id`, `review_finding_ids` SQLite table) but the plumbing to connect feedback to findings is completely missing.

## Solution

Three changes, ordered by impact:

### 1. Auto-link at feedback recording time

When `quorum feedback` or the MCP feedback tool is called without an explicit `finding_id`, automatically resolve it by matching against recent reviews.

**New function:** `resolve_finding_id(file_path: &str, finding_title: &str, review_log: &ReviewLog) -> Option<String>` in `src/review_log.rs`.

Algorithm:
1. Query the `review_finding_ids` table JOINed with `reviews` for the target `file_path`, ordered by `reviews.timestamp DESC`, limited to 50 candidates
2. Filter out legacy rows where `title = ''` (pre-migration rows with no metadata)
3. For each candidate, compute normalized string similarity (case-insensitive substring containment + Jaccard word overlap) between the feedback's `finding_title` and the stored `title`
4. Pick the best match above a 0.6 confidence threshold
5. Return `Some(finding_id)` on match, `None` if no match or below threshold
6. `tracing::debug!` when auto-linked with match score, `tracing::info!` when no match found (for threshold tuning)

**Integration points:**
- `run_feedback_inner` (main.rs:2304): call `resolve_finding_id` before constructing `FeedbackEntry`, populate `finding_id` field
- `run_feedback` external path (main.rs:2406): same — call before `record_external`
- MCP feedback handler: same — call before recording

#### Schema change

Extend `review_finding_ids` to include `title TEXT` and `file_path TEXT`:
```sql
ALTER TABLE review_finding_ids ADD COLUMN title TEXT DEFAULT '';
ALTER TABLE review_finding_ids ADD COLUMN file_path TEXT DEFAULT '';
```

Legacy rows (pre-migration) will have `title = ''` and `file_path = ''`. The resolver query filters these out (`title <> '' AND file_path <> ''`).

#### Write path

The current `record_sqlite` (review_log.rs:609) receives `ReviewRecord` which only carries `finding_ids: Vec<String>` — no titles or file paths. To populate the new columns:

**New struct:**
```rust
pub struct FindingMeta {
    pub id: String,
    pub title: String,
    pub file_path: String,
}
```

**Change `ReviewLog::record()` signature** to accept `finding_meta: &[FindingMeta]` alongside the `ReviewRecord`. The INSERT becomes:
```sql
INSERT INTO review_finding_ids (run_id, finding_id, title, file_path)
VALUES (?1, ?2, ?3, ?4)
```

**Call site** in `run_review` (main.rs): build `Vec<FindingMeta>` from the `all_findings` + `file_path` data available at review completion, pass to `review_log.record()`.

#### Resolver query

```sql
SELECT rfi.finding_id, rfi.title
FROM review_finding_ids rfi
JOIN reviews r ON r.run_id = rfi.run_id
WHERE rfi.file_path = ?1
  AND rfi.title <> ''
ORDER BY r.timestamp DESC
LIMIT 50
```

### 2. Explicit `--finding-id` CLI flag and MCP field

**CLI:** Add `--finding-id <ULID>` to `FeedbackOpts` in `src/cli/mod.rs`. When provided, bypasses auto-link and uses the explicit value directly. Validates via `ulid::Ulid::from_string()` (rejects invalid ULIDs at parse time).

**MCP:** Add `findingId: Option<String>` to the `FeedbackTool` struct in `src/mcp/tools.rs`. Same bypass behavior.

**ExternalVerdictInput:** Add `finding_id: Option<String>` field so external agents can pass it through `record_external`.

**Priority:** Explicit `--finding-id` always wins over auto-link. If provided, no resolver query runs.

### 3. Include finding IDs in review output

**JSON output:** The `Finding.id` field already exists and is serialized via serde. Verify it appears in `--json` output (it should already — `Finding` derives `Serialize`). No code change expected.

**Compact output:** No change to the existing `icon|cat|line|title` format. Adding a 5th field would break existing parsers. Finding IDs are available in `--json` output for tool integrations that need them.

## Files changed

| File | Change |
|------|--------|
| `src/storage.rs` | ALTER TABLE migration for `review_finding_ids` — add `title` and `file_path` columns |
| `src/review_log.rs` | Add `FindingMeta` struct; update `record()` to accept and write metadata; add `resolve_finding_id()` |
| `src/feedback.rs` | Add `finding_id` to `ExternalVerdictInput`; wire through `record_external` |
| `src/main.rs` | Build `Vec<FindingMeta>` at review end; wire auto-link into `run_feedback_inner` and external path |
| `src/cli/mod.rs` | Add `--finding-id` flag with ULID validation to `FeedbackOpts` |
| `src/mcp/tools.rs` | Add `findingId` to `FeedbackTool` schema |

## What this does NOT do

- No backfill of existing feedback entries
- No changes to the calibrator — it already reads `finding_id` when present
- No new dependencies — string similarity is hand-rolled (Jaccard word overlap + substring)
- No changes to the join-health diagnostic — it already checks `finding_id` linkage
- No change to compact output format (preserves backward compatibility)

## Testing

- Unit test: `resolve_finding_id` with exact match, partial match, no match, below threshold, legacy empty-title rows skipped
- Unit test: `--finding-id` ULID validation (valid ULID accepted, invalid rejected via `ulid::Ulid::from_string`)
- Unit test: explicit `--finding-id` bypasses auto-link
- Unit test: `FindingMeta` round-trip through SQLite (title + file_path persisted and retrieved)
- Unit test: migration adds columns to existing `review_finding_ids` table without data loss
- Integration test: feedback recorded after review has `finding_id` populated
- Verify `--json` output includes Finding.id field
