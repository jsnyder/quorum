# Backfill Linkage + Markdown Normalization Design

**Issues:** #439, #438

## Problem

Join health is at 0% despite PR #437 wiring the forward pipeline. Two issues:

1. **9775 legacy `review_finding_ids` rows** have empty `title`/`file_path` — the resolver can't match against them. Only 81 rows (from post-#437 reviews) have metadata. This will grow organically but a backfill command lets users re-run the resolver as metadata accumulates.

2. **Markdown formatting in stored titles** (backticks around identifiers) causes match failures. The resolver treats `` `predict_one` `` and `predict_one` as different words.

## Solution

### Part 1: Markdown normalization in resolver (#438)

In `resolve_finding_id` (review_log.rs), strip markdown formatting characters from both the stored title and the query title before tokenizing for Jaccard similarity.

```rust
fn normalize_title(s: &str) -> String {
    s.replace(['`', '*', '_'], "").to_lowercase()
}
```

Applied to both `query_lower` and `title_lower` before `split_whitespace()`. This fixes the backtick mismatch and prevents future issues with bold/italic formatting in titles.

### Part 2: `quorum backfill-linkage` CLI command (#439)

New top-level subcommand that re-runs `resolve_finding_id` on all feedback entries with `finding_id: None`.

**Algorithm:**
1. Open `feedback.jsonl` via `FeedbackStore::load_all()`
2. Open SQLite DB via `storage::initialize(quorum_home)`
3. Create `ReviewLog` from the storage handle
4. Partition entries into: already-linked (skip), needs-linking (resolve)
5. For each needs-linking entry, call `review_log.resolve_finding_id(file_path, finding_title)`
6. If matched, set `entry.finding_id = Some(id)`
7. Rewrite `feedback.jsonl` atomically: serialize all entries to a temp file, then rename over the original
8. Print summary

**Atomic rewrite:**
```rust
let tmp_path = feedback_path.with_extension("jsonl.tmp");
// write all entries to tmp_path
// fs2 file lock on feedback_path
// std::fs::rename(tmp_path, feedback_path)
```

**CLI definition:**
```rust
#[derive(Parser)]
pub struct BackfillLinkageOpts {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

**Output formats:**

Human (TTY):
```
Backfill complete
  Processed: 4813 entries
  Already linked: 4
  Newly linked: 3
  No match: 4806
```

JSON (`--json`):
```json
{"processed": 4813, "already_linked": 4, "newly_linked": 3, "no_match": 4806}
```

Compact (auto-detected):
```
backfill: 4813 processed, 4 already, 3 linked, 4806 no-match
```

**Safety:**
- Never overwrites existing `finding_id` values
- Idempotent — running twice produces the same result (second run: 0 newly linked)
- Atomic rename prevents partial writes on crash
- Uses file locking consistent with `FeedbackStore::record`

## Files changed

| File | Change |
|------|--------|
| `src/review_log.rs` | Add `normalize_title()`, apply in `resolve_finding_id` before tokenizing |
| `src/cli/mod.rs` | Add `BackfillLinkage(BackfillLinkageOpts)` variant to top-level command enum |
| `src/main.rs` | Add `run_backfill_linkage()` function implementing the algorithm; dispatch from main |

## What this does NOT do

- No reconstruction of legacy review metadata (titles were never persisted — can't be recovered)
- No changes to the forward pipeline (PR #437 already handles new reviews)
- No changes to the resolver algorithm (only the normalization preprocessing)
- No changes to the calibrator or stats

## Testing

- Unit test: `normalize_title` strips backticks, asterisks, underscores
- Unit test: `resolve_finding_id` matches title with backticks against query without (regression for #438)
- Unit test: `run_backfill_linkage` on a test store — entries get linked, already-linked entries preserved, no-match entries unchanged
- Unit test: backfill is idempotent (second run links 0 new)
- Unit test: atomic rewrite doesn't corrupt on empty store
