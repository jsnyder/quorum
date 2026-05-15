# SQL Query Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Push filtering, limiting, and aggregation from Rust into SQLite queries so `quorum stats` avoids loading and deserializing all review/telemetry rows when it only needs a subset.

**Architecture:** Six targeted optimizations to the `ReviewLog`, `TelemetryStore`, and `storage` modules. Each adds a SQL-optimized query path for the SQLite backend while retaining the existing Rust-side logic for the JSONL fallback. A schema v2 migration adds a timestamp index to make the new `WHERE`/`ORDER BY` queries efficient.

**Tech Stack:** Rust, rusqlite, SQLite (PRAGMA user_version for schema versioning), chrono (DateTime<Utc>), serde_json (for JSON columns)

---

## Task 1: Schema v2 — Timestamp Index on `reviews`

Add `CREATE INDEX idx_reviews_timestamp ON reviews(timestamp)` via a new migration function. This index accelerates the `WHERE timestamp >= ?` and `ORDER BY timestamp DESC LIMIT ?` queries added in Tasks 2-3.

**Files:**
- Modify: `src/storage.rs:23` (SCHEMA_VERSION constant)
- Modify: `src/storage.rs:111-122` (run_migrations dispatch)
- Modify: `src/storage.rs:188-194` (after migrate_v0_to_v1, add migrate_v1_to_v2)

- [ ] **Step 1: Write the failing test**

Add a test that opens a v1 database, runs migrations, and asserts `PRAGMA user_version = 2` and the index exists.

```rust
// In src/storage.rs, inside #[cfg(test)] mod tests
#[test]
fn migrate_v1_to_v2_creates_timestamp_index() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    // Bring to v1 first.
    migrate_v0_to_v1(&conn).unwrap();
    assert_eq!(current_version(&conn).unwrap(), 1);

    // Run v2 migration.
    migrate_v1_to_v2(&conn).unwrap();
    assert_eq!(current_version(&conn).unwrap(), 2);

    // Verify the index exists.
    let idx_exists: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_reviews_timestamp'")
        .unwrap()
        .exists([])
        .unwrap();
    assert!(idx_exists, "idx_reviews_timestamp must exist after v2 migration");
}

#[test]
fn run_migrations_advances_to_v2() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run_migrations(&conn).unwrap();
    assert_eq!(current_version(&conn).unwrap(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum -- storage::tests::migrate_v1_to_v2 --nocapture`
Expected: FAIL — `migrate_v1_to_v2` function does not exist.

- [ ] **Step 3: Write minimal implementation**

In `src/storage.rs`, after `migrate_v0_to_v1`:

```rust
/// Schema v2: timestamp index on reviews for efficient range queries.
fn migrate_v1_to_v2(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_reviews_timestamp ON reviews(timestamp);",
    )?;
    tx.pragma_update(None, "user_version", 2)?;
    tx.commit()?;
    Ok(())
}
```

Update `run_migrations` (line 111) to dispatch v2:

```rust
fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    let version = current_version(conn)?;

    if version < 1 {
        migrate_v0_to_v1(conn).context("schema migration v0 -> v1 failed")?;
    }
    if version < 2 {
        migrate_v1_to_v2(conn).context("schema migration v1 -> v2 failed")?;
    }

    Ok(())
}
```

Update `SCHEMA_VERSION` constant (line 23, under `#[cfg(test)]`) from `1` to `2`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum -- storage::tests::migrate_v1_to_v2 storage::tests::run_migrations_advances_to_v2 --nocapture`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass (no regressions in existing migration tests).

- [ ] **Step 6: Commit**

```bash
git add src/storage.rs
git commit -m "feat(storage): schema v2 — add timestamp index on reviews table"
```

---

## Task 2: `ReviewLog::load_recent(n)` — SQL LIMIT for `--rolling`

Add a method that loads only the most recent N×W records (where W is the window count) using `ORDER BY timestamp DESC LIMIT ?`. This replaces loading all records just to slice the last portion in `rolling_window()`.

**Files:**
- Modify: `src/review_log.rs:410-466` (add `load_recent` method to `impl ReviewLog`)
- Modify: `src/review_log.rs:573-599` (extract shared finding_map loading into helper)
- Modify: `src/main.rs:323-339` (wire up `load_recent` for rolling path)

- [ ] **Step 1: Write the failing test**

```rust
// In src/review_log.rs, inside #[cfg(test)] mod tests
#[test]
fn load_recent_returns_last_n_records_in_chronological_order() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);

    // Insert 10 records with ascending timestamps.
    for i in 0..10u32 {
        let mut rec = test_record();
        rec.timestamp = chrono::Utc::now() + chrono::Duration::seconds(i64::from(i));
        rec.files_reviewed = i;
        log.record(&rec).unwrap();
    }

    // Load last 3.
    let recent = log.load_recent(3).unwrap();
    assert_eq!(recent.len(), 3);
    // Must be in chronological (ascending) order.
    assert_eq!(recent[0].files_reviewed, 7);
    assert_eq!(recent[1].files_reviewed, 8);
    assert_eq!(recent[2].files_reviewed, 9);
}

#[test]
fn load_recent_returns_all_when_n_exceeds_total() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);

    for _ in 0..3 {
        log.record(&test_record()).unwrap();
    }

    let recent = log.load_recent(100).unwrap();
    assert_eq!(recent.len(), 3);
}

#[test]
fn load_recent_zero_returns_empty() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);
    log.record(&test_record()).unwrap();

    let recent = log.load_recent(0).unwrap();
    assert!(recent.is_empty());
}
```

Note: `test_record()` is a helper that creates a minimal `ReviewRecord`. If it doesn't exist yet, create one:

```rust
fn test_record() -> ReviewRecord {
    ReviewRecord {
        run_id: ReviewRecord::new_ulid(),
        timestamp: chrono::Utc::now(),
        quorum_version: "test".into(),
        repo: Some("test-repo".into()),
        invoked_from: "test".into(),
        model: "test-model".into(),
        files_reviewed: 1,
        lines_added: None,
        lines_removed: None,
        findings_by_severity: SeverityCounts::default(),
        suppressed_by_rule: HashMap::new(),
        tokens_in: 100,
        tokens_out: 50,
        tokens_cache_read: 0,
        duration_ms: 500,
        flags: Flags::default(),
        mode: None,
        context: ContextTelemetry::default(),
        finding_ids: vec![],
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum -- review_log::tests::load_recent --nocapture`
Expected: FAIL — `load_recent` method does not exist.

- [ ] **Step 3: Write minimal implementation**

Add to `impl ReviewLog` (after `load_all` around line 466):

```rust
/// Load the most recent `n` records in chronological order.
///
/// SQLite backend: uses `ORDER BY timestamp DESC LIMIT ?` then reverses.
/// JSONL backend: falls back to loading all then taking the tail.
pub fn load_recent(&self, n: usize) -> anyhow::Result<Vec<ReviewRecord>> {
    if n == 0 {
        return Ok(Vec::new());
    }
    match &self.backend {
        Backend::Jsonl(_) => {
            let all = self.load_all()?;
            let start = all.len().saturating_sub(n);
            Ok(all[start..].to_vec())
        }
        Backend::Sqlite(handle) => Self::load_recent_sqlite(handle, n),
    }
}

fn load_recent_sqlite(
    handle: &StorageHandle,
    n: usize,
) -> anyhow::Result<Vec<ReviewRecord>> {
    let conn = handle
        .lock()
        .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

    // Pre-load finding_ids only for the rows we need.
    // First, get the run_ids of the most recent N reviews.
    let mut id_stmt = conn.prepare(
        "SELECT run_id FROM reviews ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let recent_ids: Vec<String> = id_stmt
        .query_map([n as i64], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut finding_map: HashMap<String, Vec<String>> = HashMap::new();
    if !recent_ids.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT run_id, finding_id FROM review_finding_ids WHERE run_id = ?1 ORDER BY rowid",
        )?;
        for rid in &recent_ids {
            let rows = stmt.query_map([rid], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (run_id, finding_id) = row?;
                finding_map.entry(run_id).or_default().push(finding_id);
            }
        }
    }

    // Fetch the full rows, DESC then reverse for chronological order.
    let mut stmt = conn.prepare(
        "SELECT
            run_id, timestamp, quorum_version, repo, invoked_from, model,
            files_reviewed, lines_added, lines_removed,
            critical, high, medium, low, info,
            suppressed_by_rule,
            tokens_in, tokens_out, tokens_cache_read, duration_ms,
            flag_deep, flag_parallel_n, flag_ensemble,
            mode, context
        FROM reviews
        ORDER BY timestamp DESC
        LIMIT ?1",
    )?;

    let raw_rows: Vec<RawReviewRow> = stmt
        .query_map([n as i64], |row| {
            Ok(RawReviewRow {
                run_id: row.get(0)?,
                ts_str: row.get(1)?,
                quorum_version: row.get(2)?,
                repo: row.get(3)?,
                invoked_from: row.get(4)?,
                model: row.get(5)?,
                files_reviewed: row.get(6)?,
                lines_added: row.get(7)?,
                lines_removed: row.get(8)?,
                critical: row.get(9)?,
                high: row.get(10)?,
                medium: row.get(11)?,
                low: row.get(12)?,
                info: row.get(13)?,
                suppressed_json: row.get(14)?,
                tokens_in: row.get(15)?,
                tokens_out: row.get(16)?,
                tokens_cache_read: row.get(17)?,
                duration_ms: row.get(18)?,
                flag_deep: row.get(19)?,
                flag_parallel_n: row.get(20)?,
                flag_ensemble: row.get(21)?,
                mode: row.get(22)?,
                context_json: row.get(23)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut records = Vec::with_capacity(raw_rows.len());
    for r in raw_rows {
        records.push(r.into_record(&mut finding_map)?);
    }
    records.reverse();
    Ok(records)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum -- review_log::tests::load_recent --nocapture`
Expected: PASS (all 3 tests)

- [ ] **Step 5: Wire up caller in `main.rs`**

In `src/main.rs` around line 323, where the rolling path currently calls `log.load_all()`, change the rolling branch to use `load_recent`:

```rust
// Before (line 323-338):
if want_classic_dim {
    let log = review_log::ReviewLog::with_storage(storage_handle.clone());
    let records = match log.load_all() {
        Ok(r) => r,
        Err(e) => { ... }
    };
    let (mode, slices) = if opts.by_repo {
        ("by-repo", dimensions::group_by_repo(&records))
    } else if opts.by_caller {
        ("by-caller", dimensions::group_by_caller(&records))
    } else {
        let n = opts.rolling.unwrap();
        ("rolling", dimensions::rolling_window(&records, n, 3))
    };

// After:
if want_classic_dim {
    let log = review_log::ReviewLog::with_storage(storage_handle.clone());
    let (mode, slices) = if opts.by_repo {
        let records = match log.load_all() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: cannot read reviews log: {e}");
                std::process::exit(3);
            }
        };
        ("by-repo", dimensions::group_by_repo(&records))
    } else if opts.by_caller {
        let records = match log.load_all() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: cannot read reviews log: {e}");
                std::process::exit(3);
            }
        };
        ("by-caller", dimensions::group_by_caller(&records))
    } else {
        let n = opts.rolling.unwrap();
        let window_count = 3usize;
        let needed = n.saturating_mul(window_count);
        let records = match log.load_recent(needed) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: cannot read reviews log: {e}");
                std::process::exit(3);
            }
        };
        ("rolling", dimensions::rolling_window(&records, n, window_count))
    };
```

- [ ] **Step 6: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/review_log.rs src/main.rs
git commit -m "feat(review_log): add load_recent(n) with SQL LIMIT for rolling windows"
```

---

## Task 3: `ReviewLog::load_since(since)` — SQL WHERE for `compute_report`

Add a method that loads only reviews with `timestamp >= since` using a SQL WHERE clause. This replaces `compute_report` loading ALL reviews when it only needs records for dimensional highlights and linkage.

**Files:**
- Modify: `src/review_log.rs` (add `load_since` method to `impl ReviewLog`)
- Modify: `src/stats.rs:148` (wire up `load_since` in compute_report)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn load_since_returns_only_records_after_cutoff() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);

    let t1 = chrono::Utc::now() - chrono::Duration::days(10);
    let t2 = chrono::Utc::now() - chrono::Duration::days(5);
    let t3 = chrono::Utc::now();
    let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

    let mut r1 = test_record();
    r1.timestamp = t1;
    r1.files_reviewed = 1;
    let mut r2 = test_record();
    r2.timestamp = t2;
    r2.files_reviewed = 2;
    let mut r3 = test_record();
    r3.timestamp = t3;
    r3.files_reviewed = 3;

    log.record(&r1).unwrap();
    log.record(&r2).unwrap();
    log.record(&r3).unwrap();

    let since = log.load_since(cutoff).unwrap();
    assert_eq!(since.len(), 2);
    assert_eq!(since[0].files_reviewed, 2);
    assert_eq!(since[1].files_reviewed, 3);
}

#[test]
fn load_since_returns_empty_when_all_older() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);

    let mut rec = test_record();
    rec.timestamp = chrono::Utc::now() - chrono::Duration::days(30);
    log.record(&rec).unwrap();

    let since = log.load_since(chrono::Utc::now()).unwrap();
    assert!(since.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum -- review_log::tests::load_since --nocapture`
Expected: FAIL — `load_since` method does not exist.

- [ ] **Step 3: Write minimal implementation**

Add to `impl ReviewLog`:

```rust
/// Load reviews with `timestamp >= since` in chronological order.
///
/// SQLite backend: uses `WHERE timestamp >= ?1` with the timestamp index.
/// JSONL backend: falls back to loading all then filtering in Rust.
pub fn load_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<ReviewRecord>> {
    match &self.backend {
        Backend::Jsonl(_) => {
            let all = self.load_all()?;
            Ok(all.into_iter().filter(|r| r.timestamp >= since).collect())
        }
        Backend::Sqlite(handle) => Self::load_since_sqlite(handle, since),
    }
}

fn load_since_sqlite(
    handle: &StorageHandle,
    since: DateTime<Utc>,
) -> anyhow::Result<Vec<ReviewRecord>> {
    let conn = handle
        .lock()
        .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

    let since_str = since.to_rfc3339();

    // Pre-load finding_ids for matching reviews.
    let mut finding_map: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT f.run_id, f.finding_id
             FROM review_finding_ids f
             INNER JOIN reviews r ON r.run_id = f.run_id
             WHERE r.timestamp >= ?1
             ORDER BY f.rowid",
        )?;
        let rows = stmt.query_map([&since_str], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (run_id, finding_id) = row?;
            finding_map.entry(run_id).or_default().push(finding_id);
        }
    }

    let mut stmt = conn.prepare(
        "SELECT
            run_id, timestamp, quorum_version, repo, invoked_from, model,
            files_reviewed, lines_added, lines_removed,
            critical, high, medium, low, info,
            suppressed_by_rule,
            tokens_in, tokens_out, tokens_cache_read, duration_ms,
            flag_deep, flag_parallel_n, flag_ensemble,
            mode, context
        FROM reviews
        WHERE timestamp >= ?1
        ORDER BY timestamp ASC",
    )?;

    let raw_rows: Vec<RawReviewRow> = stmt
        .query_map([&since_str], |row| {
            Ok(RawReviewRow {
                run_id: row.get(0)?,
                ts_str: row.get(1)?,
                quorum_version: row.get(2)?,
                repo: row.get(3)?,
                invoked_from: row.get(4)?,
                model: row.get(5)?,
                files_reviewed: row.get(6)?,
                lines_added: row.get(7)?,
                lines_removed: row.get(8)?,
                critical: row.get(9)?,
                high: row.get(10)?,
                medium: row.get(11)?,
                low: row.get(12)?,
                info: row.get(13)?,
                suppressed_json: row.get(14)?,
                tokens_in: row.get(15)?,
                tokens_out: row.get(16)?,
                tokens_cache_read: row.get(17)?,
                duration_ms: row.get(18)?,
                flag_deep: row.get(19)?,
                flag_parallel_n: row.get(20)?,
                flag_ensemble: row.get(21)?,
                mode: row.get(22)?,
                context_json: row.get(23)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut records = Vec::with_capacity(raw_rows.len());
    for r in raw_rows {
        records.push(r.into_record(&mut finding_map)?);
    }
    Ok(records)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum -- review_log::tests::load_since --nocapture`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/review_log.rs
git commit -m "feat(review_log): add load_since(since) with SQL WHERE for time-filtered queries"
```

Note: Wiring `load_since` into `compute_report` is deferred to Task 6 when we address the `stats.rs` callers holistically. Adding it now would create churn if Task 6 restructures the loading pattern further.

---

## Task 4: `ReviewLog::load_all_finding_ids()` — Lightweight Linkage Query

Add a method that returns `HashSet<String>` of all finding_ids without deserializing full `ReviewRecord` structs. This is used by `analytics::linkage_stats` and `format_join_health` which only need finding_ids for set membership checks.

**Files:**
- Modify: `src/review_log.rs` (add `load_all_finding_ids` method to `impl ReviewLog`)
- Modify: `src/analytics.rs:39-58` (add `linkage_stats_from_ids` variant)

Note: Wiring callers (stats.rs, main.rs) is deferred to Task 6 to avoid double-editing the same lines.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn load_all_finding_ids_returns_unique_ids() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);

    let mut r1 = test_record();
    r1.finding_ids = vec!["fid-1".into(), "fid-2".into()];
    let mut r2 = test_record();
    r2.finding_ids = vec!["fid-2".into(), "fid-3".into()];

    log.record(&r1).unwrap();
    log.record(&r2).unwrap();

    let ids = log.load_all_finding_ids().unwrap();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains("fid-1"));
    assert!(ids.contains("fid-2"));
    assert!(ids.contains("fid-3"));
}

#[test]
fn load_all_finding_ids_empty_when_no_records() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);

    let ids = log.load_all_finding_ids().unwrap();
    assert!(ids.is_empty());
}

#[test]
fn load_all_finding_ids_empty_when_records_have_no_findings() {
    let handle = crate::storage::in_memory_handle();
    let log = ReviewLog::with_storage(handle);

    log.record(&test_record()).unwrap();

    let ids = log.load_all_finding_ids().unwrap();
    assert!(ids.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum -- review_log::tests::load_all_finding_ids --nocapture`
Expected: FAIL — `load_all_finding_ids` method does not exist.

- [ ] **Step 3: Write minimal implementation**

Add to `impl ReviewLog`:

```rust
/// Load all finding_ids as a `HashSet`, without deserializing full records.
///
/// SQLite backend: `SELECT DISTINCT finding_id FROM review_finding_ids`.
/// JSONL backend: falls back to loading all records and collecting.
pub fn load_all_finding_ids(&self) -> anyhow::Result<std::collections::HashSet<String>> {
    match &self.backend {
        Backend::Jsonl(_) => {
            let all = self.load_all()?;
            Ok(all
                .into_iter()
                .flat_map(|r| r.finding_ids)
                .collect())
        }
        Backend::Sqlite(handle) => Self::load_all_finding_ids_sqlite(handle),
    }
}

fn load_all_finding_ids_sqlite(
    handle: &StorageHandle,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let conn = handle
        .lock()
        .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

    let mut stmt = conn.prepare(
        "SELECT DISTINCT finding_id FROM review_finding_ids",
    )?;
    let ids: std::collections::HashSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum -- review_log::tests::load_all_finding_ids --nocapture`
Expected: PASS

- [ ] **Step 5: Add `linkage_stats_from_ids` to analytics.rs**

Add a variant of `linkage_stats` that accepts pre-loaded finding_ids:

```rust
/// Compute linkage stats from pre-loaded finding_ids (avoids full record deserialization).
pub fn linkage_stats_from_ids(
    known_ids: &HashSet<String>,
    feedback: &[FeedbackEntry],
) -> LinkageStats {
    use crate::feedback::Provenance;

    let mut stats = LinkageStats::default();
    for entry in feedback {
        if !matches!(&entry.provenance, Provenance::Human | Provenance::PostFix) {
            continue;
        }
        match &entry.finding_id {
            Some(fid) if known_ids.contains(fid.as_str()) => stats.linked += 1,
            _ => stats.unlinked += 1,
        }
    }
    stats
}
```

- [ ] **Step 6: Write test for `linkage_stats_from_ids`**

```rust
// In src/analytics.rs tests
#[test]
fn linkage_stats_from_ids_matches_full_variant() {
    use crate::feedback::{FeedbackEntry, Provenance, Verdict};
    use std::collections::HashSet;

    let known: HashSet<String> = ["fid-1".to_string(), "fid-2".to_string()].into();

    let feedback = vec![
        FeedbackEntry {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: "c".into(),
            verdict: Verdict::Tp,
            reason: "r".into(),
            model: None,
            timestamp: chrono::Utc::now(),
            provenance: Provenance::Human,
            fp_kind: None,
            finding_id: Some("fid-1".into()),
            rule_id: None,
            in_diff: None,
        },
        FeedbackEntry {
            file_path: "b.rs".into(),
            finding_title: "t".into(),
            finding_category: "c".into(),
            verdict: Verdict::Fp,
            reason: "r".into(),
            model: None,
            timestamp: chrono::Utc::now(),
            provenance: Provenance::Human,
            fp_kind: None,
            finding_id: Some("fid-missing".into()),
            rule_id: None,
            in_diff: None,
        },
    ];

    let stats = linkage_stats_from_ids(&known, &feedback);
    assert_eq!(stats.linked, 1);
    assert_eq!(stats.unlinked, 1);
}
```

- [ ] **Step 7: Run test to verify**

Run: `cargo test --bin quorum -- analytics::tests::linkage_stats_from_ids --nocapture`
Expected: PASS

- [ ] **Step 8: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass.

- [ ] **Step 9: Commit**

```bash
git add src/review_log.rs src/analytics.rs
git commit -m "feat(analytics): add load_all_finding_ids() and linkage_stats_from_ids()"
```

---

## Task 5: `TelemetryStore::load_since` — SQL WHERE Instead of Rust Filter

Fix `load_since` to use a SQL `WHERE ts >= ?1` clause for the SQLite backend instead of loading all rows and filtering in Rust.

**Files:**
- Modify: `src/telemetry.rs:209-216` (rewrite `load_since` to dispatch by backend)
- Modify: `src/telemetry.rs:462-483` (add `load_since_sqlite` method)

- [ ] **Step 1: Write the failing test**

```rust
// In src/telemetry.rs, inside #[cfg(test)] mod tests
#[test]
fn load_since_sqlite_filters_at_query_level() {
    let handle = crate::storage::in_memory_handle();
    let store = TelemetryStore::with_storage(handle);

    let t_old = chrono::Utc::now() - chrono::Duration::days(10);
    let t_new = chrono::Utc::now();
    let cutoff = chrono::Utc::now() - chrono::Duration::days(5);

    let mut e1 = test_entry();
    e1.ts = t_old;
    e1.model = "old".into();
    let mut e2 = test_entry();
    e2.ts = t_new;
    e2.model = "new".into();

    store.record(&e1).unwrap();
    store.record(&e2).unwrap();

    let since = store.load_since(cutoff).unwrap();
    assert_eq!(since.len(), 1);
    assert_eq!(since[0].model, "new");
}
```

Note: `test_entry()` is a helper creating a minimal `TelemetryEntry`. If it doesn't exist, create one:

```rust
fn test_entry() -> TelemetryEntry {
    TelemetryEntry {
        ts: chrono::Utc::now(),
        files: vec![],
        findings: std::collections::HashMap::new(),
        model: "test-model".into(),
        tokens_in: 100,
        tokens_out: 50,
        duration_ms: 500,
        suppressed: 0,
        context7_resolved: 0,
        context7_resolve_failed: 0,
        context7_query_failed: 0,
        context7_skipped_popular: 0,
        context7_budget_reduced: 0,
        fp_kind_utilization_rate: None,
        judge_calls: 0,
        judge_approved: 0,
        judge_rejected: 0,
        judge_uncertain: 0,
        judge_skipped: 0,
        judge_cache_hits: 0,
        judge_latency_ms: 0,
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum -- telemetry::tests::load_since_sqlite_filters --nocapture`
Expected: PASS (the existing implementation happens to work, but it loads ALL rows first). The test itself will pass — this is a performance optimization, not a behavior change. We verify correctness here; the optimization is in the implementation.

- [ ] **Step 3: Write the optimized implementation**

Rewrite `load_since` in `src/telemetry.rs` (line 209):

```rust
pub fn load_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<TelemetryEntry>> {
    match &self.backend {
        Backend::Jsonl(_) => {
            Ok(self
                .load_all_with_stats()?
                .0
                .into_iter()
                .filter(|e| e.ts >= since)
                .collect())
        }
        Backend::Sqlite(handle) => Self::load_since_sqlite(handle, since),
    }
}
```

Add `load_since_sqlite` method:

```rust
fn load_since_sqlite(
    handle: &StorageHandle,
    since: DateTime<Utc>,
) -> anyhow::Result<Vec<TelemetryEntry>> {
    let conn = handle
        .lock()
        .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

    let since_str = since.to_rfc3339();

    let mut stmt = conn.prepare(
        "SELECT
            ts, files, findings, model,
            tokens_in, tokens_out, duration_ms, suppressed,
            context7_resolved, context7_resolve_failed, context7_query_failed,
            context7_skipped_popular, context7_budget_reduced,
            fp_kind_utilization_rate,
            judge_calls, judge_approved, judge_rejected,
            judge_uncertain, judge_skipped, judge_cache_hits,
            judge_latency_ms
        FROM telemetry
        WHERE ts >= ?1
        ORDER BY ts ASC",
    )?;

    let raw_rows: Vec<RawTelemetryRow> = stmt
        .query_map([&since_str], |row| {
            Ok(RawTelemetryRow {
                ts_str: row.get(0)?,
                files_json: row.get(1)?,
                findings_json: row.get(2)?,
                model: row.get(3)?,
                tokens_in: row.get(4)?,
                tokens_out: row.get(5)?,
                duration_ms: row.get(6)?,
                suppressed: row.get(7)?,
                context7_resolved: row.get(8)?,
                context7_resolve_failed: row.get(9)?,
                context7_query_failed: row.get(10)?,
                context7_skipped_popular: row.get(11)?,
                context7_budget_reduced: row.get(12)?,
                fp_kind_utilization_rate: row.get(13)?,
                judge_calls: row.get(14)?,
                judge_approved: row.get(15)?,
                judge_rejected: row.get(16)?,
                judge_uncertain: row.get(17)?,
                judge_skipped: row.get(18)?,
                judge_cache_hits: row.get(19)?,
                judge_latency_ms: row.get(20)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut entries = Vec::with_capacity(raw_rows.len());
    for r in raw_rows {
        entries.push(r.into_entry()?);
    }
    Ok(entries)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum -- telemetry::tests::load_since --nocapture`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/telemetry.rs
git commit -m "perf(telemetry): push load_since filtering to SQL WHERE for SQLite backend"
```

---

## Task 6: Wire Up All Callers — `compute_report` and `format_join_health`

Now that Tasks 2-5 added the optimized query methods, wire them into `compute_report` and `format_join_health` so the stats pipeline uses targeted queries instead of `load_all()` wherever possible.

**Files:**
- Modify: `src/stats.rs:148-157` (compute_report: load_recent for rolling, finding_ids for linkage)
- Modify: `src/main.rs:437-501` (format_join_health: load_all_finding_ids instead of load_all)

- [ ] **Step 1: Write the failing test**

This task is a wiring change — the behavior doesn't change, only which query methods are called. Write a test that verifies `compute_report` still produces correct output when the review log has time-filtered data:

```rust
// In src/stats.rs, inside #[cfg(test)] mod tests (if one exists, otherwise create)
#[test]
fn compute_report_produces_valid_output_with_mixed_age_records() {
    use crate::storage;

    let handle = storage::in_memory_handle();
    let log = crate::review_log::ReviewLog::with_storage(handle.clone());
    let telem = crate::telemetry::TelemetryStore::with_storage(handle.clone());
    let fb_store = crate::feedback::FeedbackStore::new(
        std::env::temp_dir().join(format!("quorum-test-{}.jsonl", std::process::id())),
    );

    // Old review (30 days ago).
    let mut old = crate::review_log::ReviewRecord {
        run_id: crate::review_log::ReviewRecord::new_ulid(),
        timestamp: chrono::Utc::now() - chrono::Duration::days(30),
        quorum_version: "test".into(),
        repo: Some("old-repo".into()),
        invoked_from: "test".into(),
        model: "gpt-5.4".into(),
        files_reviewed: 1,
        lines_added: None,
        lines_removed: None,
        findings_by_severity: Default::default(),
        suppressed_by_rule: Default::default(),
        tokens_in: 100,
        tokens_out: 50,
        tokens_cache_read: 0,
        duration_ms: 500,
        flags: Default::default(),
        mode: None,
        context: Default::default(),
        finding_ids: vec!["fid-old".into()],
    };
    log.record(&old).unwrap();

    // Recent review (today).
    let mut recent = old.clone();
    recent.run_id = crate::review_log::ReviewRecord::new_ulid();
    recent.timestamp = chrono::Utc::now();
    recent.repo = Some("new-repo".into());
    recent.finding_ids = vec!["fid-new".into()];
    log.record(&recent).unwrap();

    let report = compute_report(&fb_store, &telem, &log).unwrap();
    assert!(report.top_repos.len() <= 3);
    // Cleanup.
    let _ = std::fs::remove_file(
        std::env::temp_dir().join(format!("quorum-test-{}.jsonl", std::process::id())),
    );
}
```

- [ ] **Step 2: Run test to verify it passes (baseline)**

Run: `cargo test --bin quorum -- stats::tests::compute_report_produces_valid --nocapture`
Expected: PASS (this establishes the baseline before refactoring).

- [ ] **Step 3: Refactor `compute_report` to use optimized queries**

In `src/stats.rs`, modify `compute_report` (starting at line 148):

```rust
// Before (line 148):
let review_records = review_log.load_all().unwrap_or_default();
let top_repos = take_top(dimensions::group_by_repo(&review_records), HIGHLIGHT_TOP_N);
let top_callers = take_top(
    dimensions::group_by_caller(&review_records),
    HIGHLIGHT_TOP_N,
);
let rolling_windows = dimensions::rolling_window(&review_records, ROLLING_N, ROLLING_WINDOWS);

// Linkage / capture / external overlap (Phase A).
let link = analytics::linkage_stats(&review_records, &feedback);

// After:
// Dimensional highlights: load all for repo/caller bucketing.
// (SQL GROUP BY for these is a future optimization — requires
// two-query approach for sparklines. Not in this plan's scope.)
let review_records = review_log.load_all().unwrap_or_default();
let top_repos = take_top(dimensions::group_by_repo(&review_records), HIGHLIGHT_TOP_N);
let top_callers = take_top(
    dimensions::group_by_caller(&review_records),
    HIGHLIGHT_TOP_N,
);

// Rolling windows: only need last N * ROLLING_WINDOWS records.
let rolling_needed = ROLLING_N.saturating_mul(ROLLING_WINDOWS);
let rolling_records = review_log.load_recent(rolling_needed).unwrap_or_default();
let rolling_windows = dimensions::rolling_window(&rolling_records, ROLLING_N, ROLLING_WINDOWS);

// Linkage: only needs finding_ids, not full records.
let finding_ids = review_log.load_all_finding_ids().unwrap_or_default();
let link = analytics::linkage_stats_from_ids(&finding_ids, &feedback);
```

- [ ] **Step 3b: Refactor `format_join_health` to use `load_all_finding_ids`**

In `src/main.rs`, modify `format_join_health` (starting around line 437):

```rust
// Before:
let log = review_log::ReviewLog::with_storage(storage_handle);
let reviews = match log.load_all() {
    Ok(r) => r,
    Err(e) => {
        let mut out = String::new();
        writeln!(out, "Linkage health").unwrap();
        writeln!(out, "  ERROR: failed to read reviews: {e}").unwrap();
        return out;
    }
};

let store = feedback::FeedbackStore::new(quorum_home.join("feedback.jsonl"));
let feedback = match store.load_all() { ... };

let stats = analytics::linkage_stats(&reviews, &feedback);
let total_findings: usize = reviews.iter().map(|r| r.finding_ids.len()).sum();

// After:
let log = review_log::ReviewLog::with_storage(storage_handle);
let finding_ids = match log.load_all_finding_ids() {
    Ok(ids) => ids,
    Err(e) => {
        let mut out = String::new();
        writeln!(out, "Linkage health").unwrap();
        writeln!(out, "  ERROR: failed to read reviews: {e}").unwrap();
        return out;
    }
};

let store = feedback::FeedbackStore::new(quorum_home.join("feedback.jsonl"));
let feedback = match store.load_all() {
    Ok(f) => f,
    Err(e) => {
        let mut out = String::new();
        writeln!(out, "Linkage health").unwrap();
        writeln!(out, "  ERROR: failed to read feedback.jsonl: {e}").unwrap();
        return out;
    }
};

let stats = analytics::linkage_stats_from_ids(&finding_ids, &feedback);
let total_findings = finding_ids.len();
```

- [ ] **Step 4: Run test to verify it still passes**

Run: `cargo test --bin quorum -- stats::tests::compute_report --nocapture`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/stats.rs src/main.rs
git commit -m "perf(stats): use targeted queries in compute_report — load_recent for rolling, finding_ids for linkage"
```

---

## Post-Implementation Verification

After all tasks are complete:

- [ ] **Full test suite**: `cargo test --bin quorum`
- [ ] **Clippy**: `cargo clippy --bin quorum -- -D warnings`
- [ ] **Release build**: `cargo build --release`
- [ ] **Manual smoke test**: `cargo run -- stats --rolling 50` and `cargo run -- stats --by-repo` on a populated `~/.quorum/quorum.db`
