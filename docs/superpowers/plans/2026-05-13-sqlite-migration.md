# SQLite Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `reviews.jsonl` and `telemetry.jsonl` to SQLite (`~/.quorum/quorum.db`) with automatic startup migration for existing installs.

**Architecture:** New `src/storage.rs` module owns DB initialization, schema versioning, and JSONL migration. `ReviewLog` and `TelemetryStore` switch from JSONL file ops to SQLite queries behind the same public API. `finding_ids` normalized into a child table; `ContextTelemetry` (37 fields) stored as a JSON column.

**Tech Stack:** rusqlite 0.32 (already in Cargo.toml with `bundled` + `functions` features), sqlite-vec 0.1 (already a dependency), serde_json for JSON column serialization.

**Spec:** `docs/superpowers/specs/2026-05-13-sqlite-migration-design.md`

**Spec deviation:** ContextTelemetry has 37 fields (not 8 as simplified in spec). Stored as a single JSON text column instead of 8 flattened columns. This is consistent with the spec's "JSON text for complex structures" decision.

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/storage.rs` | Create | StorageHandle, initialize(), schema creation, JSONL migration |
| `src/review_log.rs` | Modify | Switch ReviewLog internals from JSONL to SQLite |
| `src/telemetry.rs` | Modify | Switch TelemetryStore internals from JSONL to SQLite |
| `src/main.rs` | Modify | Pass StorageHandle to ReviewLog/TelemetryStore constructors |
| `src/lib.rs` | Modify | Add `pub mod storage;` declaration |

---

### Task 1: StorageHandle and Schema Creation

**Files:**
- Create: `src/storage.rs`
- Modify: `src/lib.rs` (add module declaration)

- [ ] **Step 1: Write failing test for StorageHandle creation**

In `src/storage.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_creates_tables() {
        let dir = tempfile::tempdir().unwrap();
        let handle = initialize(dir.path()).unwrap();
        let conn = handle.lock().unwrap();

        // Verify reviews table exists
        let count: i64 = conn
            .query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Verify review_finding_ids table exists
        let count: i64 = conn
            .query_row("SELECT count(*) FROM review_finding_ids", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Verify telemetry table exists
        let count: i64 = conn
            .query_row("SELECT count(*) FROM telemetry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Verify schema version
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum storage::tests::initialize_creates_tables -- --nocapture`
Expected: FAIL — module doesn't exist yet.

- [ ] **Step 3: Add module declaration**

In `src/lib.rs`, add after existing module declarations:

```rust
pub mod storage;
```

- [ ] **Step 4: Write minimal implementation**

Create `src/storage.rs`:

```rust
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::Connection;

pub type StorageHandle = Arc<Mutex<Connection>>;

pub fn initialize(quorum_home: &Path) -> anyhow::Result<StorageHandle> {
    std::fs::create_dir_all(quorum_home)
        .with_context(|| format!("Failed to create quorum home: {}", quorum_home.display()))?;

    let db_path = quorum_home.join("quorum.db");
    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    run_migrations(&conn)?;

    Ok(Arc::new(Mutex::new(conn)))
}

fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if version < 1 {
        migrate_v0_to_v1(conn)?;
    }

    Ok(())
}

fn migrate_v0_to_v1(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "BEGIN;

        CREATE TABLE IF NOT EXISTS reviews (
            run_id            TEXT PRIMARY KEY,
            timestamp         TEXT NOT NULL,
            quorum_version    TEXT NOT NULL,
            repo              TEXT,
            invoked_from      TEXT NOT NULL,
            model             TEXT NOT NULL,
            files_reviewed    INTEGER NOT NULL,
            lines_added       INTEGER,
            lines_removed     INTEGER,
            critical          INTEGER NOT NULL DEFAULT 0,
            high              INTEGER NOT NULL DEFAULT 0,
            medium            INTEGER NOT NULL DEFAULT 0,
            low               INTEGER NOT NULL DEFAULT 0,
            info              INTEGER NOT NULL DEFAULT 0,
            suppressed_by_rule TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(suppressed_by_rule)),
            tokens_in         INTEGER NOT NULL,
            tokens_out        INTEGER NOT NULL,
            tokens_cache_read INTEGER NOT NULL DEFAULT 0,
            duration_ms       INTEGER NOT NULL,
            flag_deep         INTEGER NOT NULL DEFAULT 0,
            flag_parallel_n   INTEGER NOT NULL DEFAULT 0,
            flag_ensemble     INTEGER NOT NULL DEFAULT 0,
            mode              TEXT,
            context           TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(context))
        );

        CREATE TABLE IF NOT EXISTS review_finding_ids (
            run_id      TEXT NOT NULL REFERENCES reviews(run_id),
            finding_id  TEXT NOT NULL,
            PRIMARY KEY (run_id, finding_id)
        );
        CREATE INDEX IF NOT EXISTS idx_finding_id ON review_finding_ids(finding_id);

        CREATE TABLE IF NOT EXISTS telemetry (
            id                       INTEGER PRIMARY KEY AUTOINCREMENT,
            ts                       TEXT NOT NULL,
            files                    TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(files)),
            findings                 TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(findings)),
            model                    TEXT NOT NULL,
            tokens_in                INTEGER NOT NULL,
            tokens_out               INTEGER NOT NULL,
            duration_ms              INTEGER NOT NULL,
            suppressed               INTEGER NOT NULL DEFAULT 0,
            context7_resolved        INTEGER NOT NULL DEFAULT 0,
            context7_resolve_failed  INTEGER NOT NULL DEFAULT 0,
            context7_query_failed    INTEGER NOT NULL DEFAULT 0,
            context7_skipped_popular INTEGER NOT NULL DEFAULT 0,
            context7_budget_reduced  INTEGER NOT NULL DEFAULT 0,
            fp_kind_utilization_rate REAL,
            judge_calls              INTEGER NOT NULL DEFAULT 0,
            judge_approved           INTEGER NOT NULL DEFAULT 0,
            judge_rejected           INTEGER NOT NULL DEFAULT 0,
            judge_uncertain          INTEGER NOT NULL DEFAULT 0,
            judge_skipped            INTEGER NOT NULL DEFAULT 0,
            judge_cache_hits         INTEGER NOT NULL DEFAULT 0,
            judge_latency_ms         INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_telemetry_ts ON telemetry(ts);

        PRAGMA user_version = 1;
        COMMIT;",
    )?;
    Ok(())
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --bin quorum storage::tests::initialize_creates_tables -- --nocapture`
Expected: PASS

- [ ] **Step 6: Write test for idempotent initialization**

```rust
#[test]
fn initialize_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let _h1 = initialize(dir.path()).unwrap();
    drop(_h1);
    let h2 = initialize(dir.path()).unwrap();
    let conn = h2.lock().unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
}
```

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test --bin quorum storage::tests::initialize_is_idempotent -- --nocapture`
Expected: PASS (already handled by `CREATE TABLE IF NOT EXISTS` and version guard)

- [ ] **Step 8: Commit**

```bash
git add src/storage.rs src/lib.rs
git commit -m "feat(storage): add StorageHandle and SQLite schema v1 (#326)"
```

---

### Task 2: ReviewLog SQLite Write Path

**Files:**
- Modify: `src/review_log.rs`

- [ ] **Step 1: Write failing test for SQLite record()**

Add to `src/review_log.rs` tests (the file already has a `#[cfg(test)] mod tests` block):

```rust
#[test]
fn sqlite_record_round_trip() {
    use crate::storage;
    let dir = tempfile::tempdir().unwrap();
    let handle = storage::initialize(dir.path()).unwrap();
    let log = ReviewLog::with_storage(handle.clone());

    let entry = ReviewRecord {
        run_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        timestamp: chrono::Utc::now(),
        quorum_version: "0.22.0".to_string(),
        repo: Some("test/repo".to_string()),
        invoked_from: "/usr/bin/quorum".to_string(),
        model: "gpt-5.4".to_string(),
        files_reviewed: 3,
        lines_added: Some(10),
        lines_removed: None,
        findings_by_severity: SeverityCounts {
            critical: 1,
            high: 2,
            medium: 0,
            low: 0,
            info: 0,
        },
        suppressed_by_rule: {
            let mut m = std::collections::HashMap::new();
            m.insert("rule-a".to_string(), 2);
            m
        },
        tokens_in: 1000,
        tokens_out: 500,
        tokens_cache_read: 200,
        duration_ms: 3500,
        flags: Flags {
            deep: true,
            parallel_n: 4,
            ensemble: false,
        },
        mode: Some("code".to_string()),
        context: ContextTelemetry::default(),
        finding_ids: vec!["fid-1".to_string(), "fid-2".to_string()],
    };

    log.record(&entry).unwrap();

    let all = log.load_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].run_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert_eq!(all[0].repo, Some("test/repo".to_string()));
    assert_eq!(all[0].findings_by_severity.critical, 1);
    assert_eq!(all[0].findings_by_severity.high, 2);
    assert_eq!(all[0].suppressed_by_rule.get("rule-a"), Some(&2));
    assert_eq!(all[0].flags.deep, true);
    assert_eq!(all[0].flags.parallel_n, 4);
    assert_eq!(all[0].mode, Some("code".to_string()));
    assert_eq!(all[0].finding_ids, vec!["fid-1", "fid-2"]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum review_log::tests::sqlite_record_round_trip -- --nocapture`
Expected: FAIL — `with_storage` method doesn't exist.

- [ ] **Step 3: Implement ReviewLog with dual constructor and SQLite record()**

Modify `src/review_log.rs`. Change the `ReviewLog` struct to support both backends:

```rust
use crate::storage::StorageHandle;

enum Backend {
    Jsonl(PathBuf),
    Sqlite(StorageHandle),
}

pub struct ReviewLog {
    backend: Backend,
}

impl ReviewLog {
    pub fn new(path: PathBuf) -> Self {
        Self {
            backend: Backend::Jsonl(path),
        }
    }

    pub fn with_storage(handle: StorageHandle) -> Self {
        Self {
            backend: Backend::Sqlite(handle),
        }
    }

    pub fn path(&self) -> &Path {
        match &self.backend {
            Backend::Jsonl(p) => p,
            Backend::Sqlite(_) => Path::new(""),
        }
    }

    pub fn record(&self, entry: &ReviewRecord) -> anyhow::Result<()> {
        match &self.backend {
            Backend::Jsonl(path) => self.record_jsonl(path, entry),
            Backend::Sqlite(handle) => self.record_sqlite(handle, entry),
        }
    }

    fn record_jsonl(&self, path: &Path, entry: &ReviewRecord) -> anyhow::Result<()> {
        // existing JSONL implementation moved here
        use std::io::Write;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create review log dir: {}", parent.display())
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open review log: {}", path.display()))?;
        let mut buf = serde_json::to_string(entry)?;
        buf.push('\n');
        file.write_all(buf.as_bytes())?;
        Ok(())
    }

    fn record_sqlite(&self, handle: &StorageHandle, entry: &ReviewRecord) -> anyhow::Result<()> {
        let conn = handle.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        let tx = conn.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO reviews (
                run_id, timestamp, quorum_version, repo, invoked_from, model,
                files_reviewed, lines_added, lines_removed,
                critical, high, medium, low, info,
                suppressed_by_rule,
                tokens_in, tokens_out, tokens_cache_read, duration_ms,
                flag_deep, flag_parallel_n, flag_ensemble, mode, context
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14,
                ?15,
                ?16, ?17, ?18, ?19,
                ?20, ?21, ?22, ?23, ?24
            )",
            rusqlite::params![
                entry.run_id,
                entry.timestamp.to_rfc3339(),
                entry.quorum_version,
                entry.repo,
                entry.invoked_from,
                entry.model,
                entry.files_reviewed,
                entry.lines_added,
                entry.lines_removed,
                entry.findings_by_severity.critical,
                entry.findings_by_severity.high,
                entry.findings_by_severity.medium,
                entry.findings_by_severity.low,
                entry.findings_by_severity.info,
                serde_json::to_string(&entry.suppressed_by_rule)?,
                entry.tokens_in as i64,
                entry.tokens_out as i64,
                entry.tokens_cache_read as i64,
                entry.duration_ms as i64,
                entry.flags.deep as i32,
                entry.flags.parallel_n,
                entry.flags.ensemble as i32,
                entry.mode,
                serde_json::to_string(&entry.context)?,
            ],
        )?;

        for fid in &entry.finding_ids {
            tx.execute(
                "INSERT OR IGNORE INTO review_finding_ids (run_id, finding_id) VALUES (?1, ?2)",
                rusqlite::params![entry.run_id, fid],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<ReviewRecord>> {
        match &self.backend {
            Backend::Jsonl(_) => self.iter()?.collect(),
            Backend::Sqlite(handle) => self.load_all_sqlite(handle),
        }
    }

    fn load_all_sqlite(&self, handle: &StorageHandle) -> anyhow::Result<Vec<ReviewRecord>> {
        let conn = handle.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;

        // First load all finding_ids grouped by run_id
        let mut fid_stmt = conn.prepare(
            "SELECT run_id, finding_id FROM review_finding_ids ORDER BY run_id"
        )?;
        let mut finding_ids_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let fid_rows = fid_stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in fid_rows {
            let (run_id, fid) = row?;
            finding_ids_map.entry(run_id).or_default().push(fid);
        }

        let mut stmt = conn.prepare(
            "SELECT
                run_id, timestamp, quorum_version, repo, invoked_from, model,
                files_reviewed, lines_added, lines_removed,
                critical, high, medium, low, info,
                suppressed_by_rule,
                tokens_in, tokens_out, tokens_cache_read, duration_ms,
                flag_deep, flag_parallel_n, flag_ensemble, mode, context
            FROM reviews ORDER BY timestamp"
        )?;

        let rows = stmt.query_map([], |row| {
            let run_id: String = row.get(0)?;
            let ts_str: String = row.get(1)?;
            let suppressed_json: String = row.get(14)?;
            let context_json: String = row.get(23)?;

            Ok((run_id, ts_str, suppressed_json, context_json, row))
        })?;

        // Re-query to avoid borrow issues — use a collected intermediate
        drop(rows);
        drop(stmt);

        let mut stmt = conn.prepare(
            "SELECT
                run_id, timestamp, quorum_version, repo, invoked_from, model,
                files_reviewed, lines_added, lines_removed,
                critical, high, medium, low, info,
                suppressed_by_rule,
                tokens_in, tokens_out, tokens_cache_read, duration_ms,
                flag_deep, flag_parallel_n, flag_ensemble, mode, context
            FROM reviews ORDER BY timestamp"
        )?;

        let mut records = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let run_id: String = row.get(0)?;
            let ts_str: String = row.get(1)?;
            let suppressed_json: String = row.get(14)?;
            let context_json: String = row.get(23)?;
            let flag_deep: i32 = row.get(19)?;
            let flag_ensemble: i32 = row.get(21)?;

            let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    1, rusqlite::types::Type::Text, Box::new(e),
                ))?;

            let suppressed_by_rule: HashMap<String, u32> =
                serde_json::from_str(&suppressed_json).unwrap_or_default();
            let context: ContextTelemetry =
                serde_json::from_str(&context_json).unwrap_or_default();
            let finding_ids = finding_ids_map
                .remove(&run_id)
                .unwrap_or_default();

            records.push(ReviewRecord {
                run_id,
                timestamp,
                quorum_version: row.get(2)?,
                repo: row.get(3)?,
                invoked_from: row.get(4)?,
                model: row.get(5)?,
                files_reviewed: row.get(6)?,
                lines_added: row.get(7)?,
                lines_removed: row.get(8)?,
                findings_by_severity: SeverityCounts {
                    critical: row.get(9)?,
                    high: row.get(10)?,
                    medium: row.get(11)?,
                    low: row.get(12)?,
                    info: row.get(13)?,
                },
                suppressed_by_rule,
                tokens_in: row.get::<_, i64>(15)? as u64,
                tokens_out: row.get::<_, i64>(16)? as u64,
                tokens_cache_read: row.get::<_, i64>(17)? as u64,
                duration_ms: row.get::<_, i64>(18)? as u64,
                flags: Flags {
                    deep: flag_deep != 0,
                    parallel_n: row.get(20)?,
                    ensemble: flag_ensemble != 0,
                },
                mode: row.get(22)?,
                context,
                finding_ids,
            });
        }
        Ok(records)
    }

    pub fn iter(&self) -> anyhow::Result<ReviewLogIter> {
        // existing implementation unchanged — only used for JSONL backend
        // ...
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum review_log::tests::sqlite_record_round_trip -- --nocapture`
Expected: PASS

- [ ] **Step 5: Write test for optional fields round-trip**

```rust
#[test]
fn sqlite_optional_fields_round_trip() {
    use crate::storage;
    let dir = tempfile::tempdir().unwrap();
    let handle = storage::initialize(dir.path()).unwrap();
    let log = ReviewLog::with_storage(handle.clone());

    let entry = ReviewRecord {
        run_id: "01MINIMAL".to_string(),
        timestamp: chrono::Utc::now(),
        quorum_version: "0.22.0".to_string(),
        repo: None,
        invoked_from: "test".to_string(),
        model: "test".to_string(),
        files_reviewed: 0,
        lines_added: None,
        lines_removed: None,
        findings_by_severity: SeverityCounts::default(),
        suppressed_by_rule: HashMap::new(),
        tokens_in: 0,
        tokens_out: 0,
        tokens_cache_read: 0,
        duration_ms: 0,
        flags: Flags::default(),
        mode: None,
        context: ContextTelemetry::default(),
        finding_ids: vec![],
    };

    log.record(&entry).unwrap();
    let all = log.load_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].repo, None);
    assert_eq!(all[0].lines_added, None);
    assert_eq!(all[0].mode, None);
    assert!(all[0].finding_ids.is_empty());
    assert_eq!(all[0].suppressed_by_rule.len(), 0);
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --bin quorum review_log::tests::sqlite_optional_fields_round_trip -- --nocapture`
Expected: PASS

- [ ] **Step 7: Write test for ContextTelemetry JSON round-trip**

```rust
#[test]
fn sqlite_context_telemetry_round_trip() {
    use crate::storage;
    let dir = tempfile::tempdir().unwrap();
    let handle = storage::initialize(dir.path()).unwrap();
    let log = ReviewLog::with_storage(handle.clone());

    let mut ctx = ContextTelemetry::default();
    ctx.auto_inject_enabled = true;
    ctx.injected_chunk_count = 5;
    ctx.injected_tokens = 400;
    ctx.retrieved_chunk_count = 12;
    ctx.suppressed_by_calibrator = 2;

    let mut entry = ReviewRecord {
        run_id: "01CTX".to_string(),
        timestamp: chrono::Utc::now(),
        quorum_version: "0.22.0".to_string(),
        repo: None,
        invoked_from: "test".to_string(),
        model: "test".to_string(),
        files_reviewed: 1,
        lines_added: None,
        lines_removed: None,
        findings_by_severity: SeverityCounts::default(),
        suppressed_by_rule: HashMap::new(),
        tokens_in: 0,
        tokens_out: 0,
        tokens_cache_read: 0,
        duration_ms: 0,
        flags: Flags::default(),
        mode: None,
        context: ctx,
        finding_ids: vec![],
    };

    log.record(&entry).unwrap();
    let all = log.load_all().unwrap();
    assert_eq!(all[0].context.auto_inject_enabled, true);
    assert_eq!(all[0].context.injected_chunk_count, 5);
    assert_eq!(all[0].context.injected_tokens, 400);
    assert_eq!(all[0].context.retrieved_chunk_count, 12);
    assert_eq!(all[0].context.suppressed_by_calibrator, 2);
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test --bin quorum review_log::tests::sqlite_context_telemetry_round_trip -- --nocapture`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add src/review_log.rs
git commit -m "feat(review_log): add SQLite backend with dual constructor (#326)"
```

---

### Task 3: TelemetryStore SQLite Backend

**Files:**
- Modify: `src/telemetry.rs`

- [ ] **Step 1: Write failing test for SQLite record + load round-trip**

```rust
#[test]
fn sqlite_record_round_trip() {
    use crate::storage;
    let dir = tempfile::tempdir().unwrap();
    let handle = storage::initialize(dir.path()).unwrap();
    let store = TelemetryStore::with_storage(handle.clone());

    let entry = TelemetryEntry {
        ts: chrono::Utc::now(),
        files: vec!["src/main.rs".to_string()],
        findings: {
            let mut m = std::collections::HashMap::new();
            m.insert("security".to_string(), 2);
            m.insert("style".to_string(), 1);
            m
        },
        model: "gpt-5.4".to_string(),
        tokens_in: 1000,
        tokens_out: 500,
        duration_ms: 3500,
        suppressed: 1,
        context7_resolved: 3,
        context7_resolve_failed: 1,
        context7_query_failed: 0,
        context7_skipped_popular: 2,
        context7_budget_reduced: 1,
        fp_kind_utilization_rate: Some(0.45),
        judge_calls: 5,
        judge_approved: 3,
        judge_rejected: 1,
        judge_uncertain: 1,
        judge_skipped: 0,
        judge_cache_hits: 2,
        judge_latency_ms: 800,
    };

    store.record(&entry).unwrap();
    let (all, stats) = store.load_all_with_stats().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].model, "gpt-5.4");
    assert_eq!(all[0].tokens_in, 1000);
    assert_eq!(all[0].context7_resolved, 3);
    assert_eq!(all[0].judge_calls, 5);
    assert_eq!(all[0].judge_approved, 3);
    assert!((all[0].fp_kind_utilization_rate.unwrap() - 0.45).abs() < 0.001);
    assert_eq!(all[0].files, vec!["src/main.rs"]);
    assert_eq!(all[0].findings.get("security"), Some(&2));
    assert_eq!(stats.kept, 1);
    assert_eq!(stats.skipped, 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum telemetry::tests::sqlite_record_round_trip -- --nocapture`
Expected: FAIL — `with_storage` method doesn't exist.

- [ ] **Step 3: Implement TelemetryStore with dual constructor**

Modify `src/telemetry.rs`. Same pattern as ReviewLog — add `Backend` enum, `with_storage()` constructor, SQLite read/write paths:

```rust
use crate::storage::StorageHandle;

enum Backend {
    Jsonl(PathBuf),
    Sqlite(StorageHandle),
}

pub struct TelemetryStore {
    backend: Backend,
}

impl TelemetryStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            backend: Backend::Jsonl(path),
        }
    }

    pub fn with_storage(handle: StorageHandle) -> Self {
        Self {
            backend: Backend::Sqlite(handle),
        }
    }

    pub fn record(&self, entry: &TelemetryEntry) -> anyhow::Result<()> {
        match &self.backend {
            Backend::Jsonl(path) => self.record_jsonl(path, entry),
            Backend::Sqlite(handle) => self.record_sqlite(handle, entry),
        }
    }

    fn record_sqlite(&self, handle: &StorageHandle, entry: &TelemetryEntry) -> anyhow::Result<()> {
        let conn = handle.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO telemetry (
                ts, files, findings, model, tokens_in, tokens_out,
                duration_ms, suppressed,
                context7_resolved, context7_resolve_failed, context7_query_failed,
                context7_skipped_popular, context7_budget_reduced,
                fp_kind_utilization_rate,
                judge_calls, judge_approved, judge_rejected, judge_uncertain,
                judge_skipped, judge_cache_hits, judge_latency_ms
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8,
                ?9, ?10, ?11,
                ?12, ?13,
                ?14,
                ?15, ?16, ?17, ?18,
                ?19, ?20, ?21
            )",
            rusqlite::params![
                entry.ts.to_rfc3339(),
                serde_json::to_string(&entry.files)?,
                serde_json::to_string(&entry.findings)?,
                entry.model,
                entry.tokens_in as i64,
                entry.tokens_out as i64,
                entry.duration_ms as i64,
                entry.suppressed as i64,
                entry.context7_resolved,
                entry.context7_resolve_failed,
                entry.context7_query_failed,
                entry.context7_skipped_popular,
                entry.context7_budget_reduced,
                entry.fp_kind_utilization_rate,
                entry.judge_calls,
                entry.judge_approved,
                entry.judge_rejected,
                entry.judge_uncertain,
                entry.judge_skipped,
                entry.judge_cache_hits,
                entry.judge_latency_ms as i64,
            ],
        )?;
        Ok(())
    }

    pub fn load_all_with_stats(&self) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        match &self.backend {
            Backend::Jsonl(_) => self.load_all_with_stats_jsonl(),
            Backend::Sqlite(handle) => self.load_all_with_stats_sqlite(handle),
        }
    }

    fn load_all_with_stats_sqlite(
        &self,
        handle: &StorageHandle,
    ) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        let conn = handle.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT
                ts, files, findings, model, tokens_in, tokens_out,
                duration_ms, suppressed,
                context7_resolved, context7_resolve_failed, context7_query_failed,
                context7_skipped_popular, context7_budget_reduced,
                fp_kind_utilization_rate,
                judge_calls, judge_approved, judge_rejected, judge_uncertain,
                judge_skipped, judge_cache_hits, judge_latency_ms
            FROM telemetry ORDER BY ts"
        )?;

        let mut entries = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let ts_str: String = row.get(0)?;
            let files_json: String = row.get(1)?;
            let findings_json: String = row.get(2)?;

            let ts = chrono::DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    0, rusqlite::types::Type::Text, Box::new(e),
                ))?;

            entries.push(TelemetryEntry {
                ts,
                files: serde_json::from_str(&files_json).unwrap_or_default(),
                findings: serde_json::from_str(&findings_json).unwrap_or_default(),
                model: row.get(3)?,
                tokens_in: row.get::<_, i64>(4)? as u64,
                tokens_out: row.get::<_, i64>(5)? as u64,
                duration_ms: row.get::<_, i64>(6)? as u64,
                suppressed: row.get::<_, i64>(7)? as usize,
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
                judge_latency_ms: row.get::<_, i64>(20)? as u64,
            });
        }

        let stats = LoadStats {
            kept: entries.len(),
            skipped: 0,
            errors: vec![],
        };
        Ok((entries, stats))
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<TelemetryEntry>> {
        let (entries, _) = self.load_all_with_stats()?;
        Ok(entries)
    }

    // ... existing JSONL methods renamed to record_jsonl, load_all_with_stats_jsonl ...
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum telemetry::tests::sqlite_record_round_trip -- --nocapture`
Expected: PASS

- [ ] **Step 5: Write test for fp_kind_utilization_rate None round-trip**

```rust
#[test]
fn sqlite_null_fp_kind_rate() {
    use crate::storage;
    let dir = tempfile::tempdir().unwrap();
    let handle = storage::initialize(dir.path()).unwrap();
    let store = TelemetryStore::with_storage(handle.clone());

    let mut entry = TelemetryEntry {
        ts: chrono::Utc::now(),
        files: vec![],
        findings: std::collections::HashMap::new(),
        model: "test".to_string(),
        tokens_in: 0,
        tokens_out: 0,
        duration_ms: 0,
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
    };

    store.record(&entry).unwrap();
    let all = store.load_all().unwrap();
    assert_eq!(all[0].fp_kind_utilization_rate, None);
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --bin quorum telemetry::tests::sqlite_null_fp_kind_rate -- --nocapture`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/telemetry.rs
git commit -m "feat(telemetry): add SQLite backend with dual constructor (#326)"
```

---

### Task 4: JSONL Migration Logic

**Files:**
- Modify: `src/storage.rs`

- [ ] **Step 1: Write failing test for reviews.jsonl migration**

```rust
#[test]
fn migrate_reviews_jsonl() {
    let dir = tempfile::tempdir().unwrap();

    // Write a synthetic reviews.jsonl
    let jsonl_path = dir.path().join("reviews.jsonl");
    let record = serde_json::json!({
        "run_id": "01MIGRATE",
        "timestamp": "2026-05-13T00:00:00Z",
        "quorum_version": "0.21.0",
        "invoked_from": "test",
        "model": "gpt-5.4",
        "files_reviewed": 1,
        "findings_by_severity": {"critical": 0, "high": 1, "medium": 0, "low": 0, "info": 0},
        "tokens_in": 100,
        "tokens_out": 50,
        "tokens_cache_read": 0,
        "duration_ms": 500,
        "finding_ids": ["fid-a", "fid-b"]
    });
    std::fs::write(&jsonl_path, format!("{}\n", record)).unwrap();

    let handle = initialize(dir.path()).unwrap();

    // JSONL should be renamed
    assert!(!jsonl_path.exists());
    assert!(dir.path().join("reviews.jsonl.migrated").exists());

    // Data should be in SQLite
    let conn = handle.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let run_id: String = conn
        .query_row("SELECT run_id FROM reviews", [], |r| r.get(0))
        .unwrap();
    assert_eq!(run_id, "01MIGRATE");

    // finding_ids should be in child table
    let fid_count: i64 = conn
        .query_row("SELECT count(*) FROM review_finding_ids", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fid_count, 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum storage::tests::migrate_reviews_jsonl -- --nocapture`
Expected: FAIL — migration logic doesn't exist.

- [ ] **Step 3: Implement JSONL migration in initialize()**

Add to `src/storage.rs` after `run_migrations`:

```rust
pub fn initialize(quorum_home: &Path) -> anyhow::Result<StorageHandle> {
    std::fs::create_dir_all(quorum_home)
        .with_context(|| format!("Failed to create quorum home: {}", quorum_home.display()))?;

    let db_path = quorum_home.join("quorum.db");
    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    run_migrations(&conn)?;

    // Migrate existing JSONL files
    migrate_reviews_jsonl(&conn, quorum_home)?;
    migrate_telemetry_jsonl(&conn, quorum_home)?;

    Ok(Arc::new(Mutex::new(conn)))
}

fn migrate_reviews_jsonl(conn: &Connection, quorum_home: &Path) -> anyhow::Result<()> {
    let jsonl_path = quorum_home.join("reviews.jsonl");
    if !jsonl_path.exists() {
        return Ok(());
    }

    // Skip if reviews table already has data (don't double-import)
    let count: i64 = conn.query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))?;
    if count > 0 {
        eprintln!(
            "warning: reviews table already has {} rows and reviews.jsonl exists. \
             Skipping migration to avoid double-import.",
            count
        );
        return Ok(());
    }

    let file = std::fs::File::open(&jsonl_path)?;
    let reader = std::io::BufReader::new(file);

    let tx = conn.unchecked_transaction()?;
    let mut imported = 0u64;
    let mut skipped = 0u64;

    use std::io::BufRead;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<crate::review_log::ReviewRecord>(&line) {
            Ok(entry) => {
                tx.execute(
                    "INSERT OR IGNORE INTO reviews (
                        run_id, timestamp, quorum_version, repo, invoked_from, model,
                        files_reviewed, lines_added, lines_removed,
                        critical, high, medium, low, info,
                        suppressed_by_rule,
                        tokens_in, tokens_out, tokens_cache_read, duration_ms,
                        flag_deep, flag_parallel_n, flag_ensemble, mode, context
                    ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6,
                        ?7, ?8, ?9,
                        ?10, ?11, ?12, ?13, ?14,
                        ?15,
                        ?16, ?17, ?18, ?19,
                        ?20, ?21, ?22, ?23, ?24
                    )",
                    rusqlite::params![
                        entry.run_id,
                        entry.timestamp.to_rfc3339(),
                        entry.quorum_version,
                        entry.repo,
                        entry.invoked_from,
                        entry.model,
                        entry.files_reviewed,
                        entry.lines_added,
                        entry.lines_removed,
                        entry.findings_by_severity.critical,
                        entry.findings_by_severity.high,
                        entry.findings_by_severity.medium,
                        entry.findings_by_severity.low,
                        entry.findings_by_severity.info,
                        serde_json::to_string(&entry.suppressed_by_rule)?,
                        entry.tokens_in as i64,
                        entry.tokens_out as i64,
                        entry.tokens_cache_read as i64,
                        entry.duration_ms as i64,
                        entry.flags.deep as i32,
                        entry.flags.parallel_n,
                        entry.flags.ensemble as i32,
                        entry.mode,
                        serde_json::to_string(&entry.context)?,
                    ],
                )?;
                for fid in &entry.finding_ids {
                    tx.execute(
                        "INSERT OR IGNORE INTO review_finding_ids (run_id, finding_id) VALUES (?1, ?2)",
                        rusqlite::params![entry.run_id, fid],
                    )?;
                }
                imported += 1;
            }
            Err(e) => {
                eprintln!("warning: skipping malformed review record during migration: {}", e);
                skipped += 1;
            }
        }
    }

    tx.commit()?;
    std::fs::rename(&jsonl_path, quorum_home.join("reviews.jsonl.migrated"))?;
    eprintln!("Migrated {} reviews to quorum.db ({} skipped)", imported, skipped);
    Ok(())
}

fn migrate_telemetry_jsonl(conn: &Connection, quorum_home: &Path) -> anyhow::Result<()> {
    let jsonl_path = quorum_home.join("telemetry.jsonl");
    if !jsonl_path.exists() {
        return Ok(());
    }

    let count: i64 = conn.query_row("SELECT count(*) FROM telemetry", [], |r| r.get(0))?;
    if count > 0 {
        eprintln!(
            "warning: telemetry table already has {} rows and telemetry.jsonl exists. \
             Skipping migration to avoid double-import.",
            count
        );
        return Ok(());
    }

    let file = std::fs::File::open(&jsonl_path)?;
    let reader = std::io::BufReader::new(file);

    let tx = conn.unchecked_transaction()?;
    let mut imported = 0u64;
    let mut skipped = 0u64;

    use std::io::BufRead;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<crate::telemetry::TelemetryEntry>(&line) {
            Ok(entry) => {
                tx.execute(
                    "INSERT INTO telemetry (
                        ts, files, findings, model, tokens_in, tokens_out,
                        duration_ms, suppressed,
                        context7_resolved, context7_resolve_failed, context7_query_failed,
                        context7_skipped_popular, context7_budget_reduced,
                        fp_kind_utilization_rate,
                        judge_calls, judge_approved, judge_rejected, judge_uncertain,
                        judge_skipped, judge_cache_hits, judge_latency_ms
                    ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6,
                        ?7, ?8,
                        ?9, ?10, ?11,
                        ?12, ?13,
                        ?14,
                        ?15, ?16, ?17, ?18,
                        ?19, ?20, ?21
                    )",
                    rusqlite::params![
                        entry.ts.to_rfc3339(),
                        serde_json::to_string(&entry.files)?,
                        serde_json::to_string(&entry.findings)?,
                        entry.model,
                        entry.tokens_in as i64,
                        entry.tokens_out as i64,
                        entry.duration_ms as i64,
                        entry.suppressed as i64,
                        entry.context7_resolved,
                        entry.context7_resolve_failed,
                        entry.context7_query_failed,
                        entry.context7_skipped_popular,
                        entry.context7_budget_reduced,
                        entry.fp_kind_utilization_rate,
                        entry.judge_calls,
                        entry.judge_approved,
                        entry.judge_rejected,
                        entry.judge_uncertain,
                        entry.judge_skipped,
                        entry.judge_cache_hits,
                        entry.judge_latency_ms as i64,
                    ],
                )?;
                imported += 1;
            }
            Err(e) => {
                eprintln!("warning: skipping malformed telemetry entry during migration: {}", e);
                skipped += 1;
            }
        }
    }

    tx.commit()?;
    std::fs::rename(&jsonl_path, quorum_home.join("telemetry.jsonl.migrated"))?;
    eprintln!("Migrated {} telemetry entries to quorum.db ({} skipped)", imported, skipped);
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum storage::tests::migrate_reviews_jsonl -- --nocapture`
Expected: PASS

- [ ] **Step 5: Write test for telemetry migration**

```rust
#[test]
fn migrate_telemetry_jsonl() {
    let dir = tempfile::tempdir().unwrap();

    let jsonl_path = dir.path().join("telemetry.jsonl");
    let entry = serde_json::json!({
        "ts": "2026-05-13T00:00:00Z",
        "files": ["src/main.rs"],
        "findings": {"security": 1},
        "model": "gpt-5.4",
        "tokens_in": 100,
        "tokens_out": 50,
        "duration_ms": 500,
        "suppressed": 0
    });
    std::fs::write(&jsonl_path, format!("{}\n", entry)).unwrap();

    let handle = initialize(dir.path()).unwrap();

    assert!(!jsonl_path.exists());
    assert!(dir.path().join("telemetry.jsonl.migrated").exists());

    let conn = handle.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM telemetry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
```

- [ ] **Step 6: Write test for malformed lines during migration**

```rust
#[test]
fn migrate_skips_malformed_lines() {
    let dir = tempfile::tempdir().unwrap();

    let jsonl_path = dir.path().join("reviews.jsonl");
    let valid = serde_json::json!({
        "run_id": "01VALID",
        "timestamp": "2026-05-13T00:00:00Z",
        "quorum_version": "0.21.0",
        "invoked_from": "test",
        "model": "test",
        "files_reviewed": 1,
        "findings_by_severity": {"critical": 0, "high": 0, "medium": 0, "low": 0, "info": 0},
        "tokens_in": 0,
        "tokens_out": 0,
        "duration_ms": 0
    });
    let content = format!("{}\n{{bad json}}\n{}\n", valid, valid);
    std::fs::write(&jsonl_path, content).unwrap();

    let handle = initialize(dir.path()).unwrap();

    let conn = handle.lock().unwrap();
    // Only the valid line should be imported (deduped by run_id, so just 1)
    let count: i64 = conn
        .query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
```

- [ ] **Step 7: Write test for idempotent migration (already migrated)**

```rust
#[test]
fn migrate_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();

    let jsonl_path = dir.path().join("reviews.jsonl");
    let record = serde_json::json!({
        "run_id": "01IDEM",
        "timestamp": "2026-05-13T00:00:00Z",
        "quorum_version": "0.21.0",
        "invoked_from": "test",
        "model": "test",
        "files_reviewed": 1,
        "findings_by_severity": {"critical": 0, "high": 0, "medium": 0, "low": 0, "info": 0},
        "tokens_in": 0,
        "tokens_out": 0,
        "duration_ms": 0
    });
    std::fs::write(&jsonl_path, format!("{}\n", record)).unwrap();

    // First initialization migrates
    let h1 = initialize(dir.path()).unwrap();
    drop(h1);

    // Second initialization is a no-op
    let h2 = initialize(dir.path()).unwrap();
    let conn = h2.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
```

- [ ] **Step 8: Run all migration tests**

Run: `cargo test --bin quorum storage::tests -- --nocapture`
Expected: All PASS

- [ ] **Step 9: Commit**

```bash
git add src/storage.rs
git commit -m "feat(storage): add JSONL migration on startup (#326)"
```

---

### Task 5: Error Handling — Corruption Recovery

**Files:**
- Modify: `src/storage.rs`

- [ ] **Step 1: Write failing test for corrupt database recovery**

```rust
#[test]
fn corrupt_db_creates_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("quorum.db");

    // Write garbage to simulate corruption
    std::fs::write(&db_path, b"this is not a sqlite database").unwrap();

    // Should recover by creating fresh database
    let handle = initialize(dir.path()).unwrap();
    let conn = handle.lock().unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum storage::tests::corrupt_db_creates_fresh -- --nocapture`
Expected: FAIL — current code will error on corrupt DB without recovery.

- [ ] **Step 3: Add corruption recovery to initialize()**

Wrap the `Connection::open` in a recovery path:

```rust
pub fn initialize(quorum_home: &Path) -> anyhow::Result<StorageHandle> {
    std::fs::create_dir_all(quorum_home)
        .with_context(|| format!("Failed to create quorum home: {}", quorum_home.display()))?;

    let db_path = quorum_home.join("quorum.db");

    let conn = match open_and_migrate(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!(
                "warning: could not open quorum.db: {}. \
                 Creating fresh database. Your previous review/telemetry data \
                 may need recovery.",
                e
            );
            // Move corrupt file aside
            let backup = quorum_home.join("quorum.db.corrupt");
            let _ = std::fs::rename(&db_path, &backup);
            open_and_migrate(&db_path)
                .context("Failed to create fresh database after corruption recovery")?
        }
    };

    migrate_reviews_jsonl(&conn, quorum_home)?;
    migrate_telemetry_jsonl(&conn, quorum_home)?;

    Ok(Arc::new(Mutex::new(conn)))
}

fn open_and_migrate(db_path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    // Quick integrity check
    let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        anyhow::bail!("database integrity check failed: {}", ok);
    }
    run_migrations(&conn)?;
    Ok(conn)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum storage::tests::corrupt_db_creates_fresh -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage.rs
git commit -m "feat(storage): add corruption recovery with user warning (#326)"
```

---

### Task 6: Wire Up main.rs Callers

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add StorageHandle initialization early in main()**

After `quorum_home` is resolved (line 144), add:

```rust
let storage_handle = quorum::storage::initialize(&quorum_home)
    .unwrap_or_else(|e| {
        eprintln!("warning: failed to initialize storage: {}. Falling back to JSONL.", e);
        // Fallback: create an in-memory DB so SQLite paths don't crash
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory DB");
        std::sync::Arc::new(std::sync::Mutex::new(conn))
    });
```

- [ ] **Step 2: Replace ReviewLog::new() calls with ReviewLog::with_storage()**

Change all 5 call sites:

Line 257: `let log = review_log::ReviewLog::with_storage(storage_handle.clone());`
Line 319: `let log = review_log::ReviewLog::with_storage(storage_handle.clone());`
Line 372: `let review_log = review_log::ReviewLog::with_storage(storage_handle.clone());`
Line 427: `let log = review_log::ReviewLog::with_storage(storage_handle.clone());`
Line 1661: `let review_log = review_log::ReviewLog::with_storage(storage_handle.clone());`

- [ ] **Step 3: Replace TelemetryStore::new() calls with TelemetryStore::with_storage()**

Change both call sites:

Line 371: `let telemetry_store = telemetry::TelemetryStore::with_storage(storage_handle.clone());`
Line 1594: `let telem_store = telemetry::TelemetryStore::with_storage(storage_handle.clone());`

- [ ] **Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass. Existing tests use `ReviewLog::new()` (JSONL path) so they continue to work.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --bin quorum`
Expected: No new warnings.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): wire SQLite storage into review and telemetry paths (#326)"
```

---

### Task 7: Integration Test — End-to-End Migration

**Files:**
- Modify: `src/storage.rs` (add integration test)

- [ ] **Step 1: Write end-to-end integration test**

```rust
#[test]
fn end_to_end_migration_and_readback() {
    use crate::review_log::{ReviewLog, ReviewRecord, SeverityCounts, Flags, ContextTelemetry};
    use crate::telemetry::{TelemetryStore, TelemetryEntry};

    let dir = tempfile::tempdir().unwrap();

    // Simulate pre-migration state: write JSONL files using old-style stores
    let reviews_path = dir.path().join("reviews.jsonl");
    let old_log = ReviewLog::new(reviews_path.clone());
    let review = ReviewRecord {
        run_id: "01E2E".to_string(),
        timestamp: chrono::Utc::now(),
        quorum_version: "0.21.0".to_string(),
        repo: Some("e2e/test".to_string()),
        invoked_from: "test".to_string(),
        model: "gpt-5.4".to_string(),
        files_reviewed: 2,
        lines_added: Some(50),
        lines_removed: Some(10),
        findings_by_severity: SeverityCounts { critical: 0, high: 1, medium: 2, low: 0, info: 0 },
        suppressed_by_rule: std::collections::HashMap::new(),
        tokens_in: 2000,
        tokens_out: 800,
        tokens_cache_read: 500,
        duration_ms: 5000,
        flags: Flags { deep: false, parallel_n: 4, ensemble: true },
        mode: None,
        context: ContextTelemetry::default(),
        finding_ids: vec!["e2e-fid-1".to_string()],
    };
    old_log.record(&review).unwrap();

    let telem_path = dir.path().join("telemetry.jsonl");
    let old_telem = TelemetryStore::new(telem_path.clone());
    let telem = TelemetryEntry {
        ts: chrono::Utc::now(),
        files: vec!["src/lib.rs".to_string()],
        findings: {
            let mut m = std::collections::HashMap::new();
            m.insert("quality".to_string(), 3);
            m
        },
        model: "gpt-5.4".to_string(),
        tokens_in: 1500,
        tokens_out: 600,
        duration_ms: 4000,
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
    };
    old_telem.record(&telem).unwrap();

    // Now initialize — should migrate both JSONL files
    let handle = initialize(dir.path()).unwrap();

    // JSONL files should be renamed
    assert!(!reviews_path.exists());
    assert!(!telem_path.exists());
    assert!(dir.path().join("reviews.jsonl.migrated").exists());
    assert!(dir.path().join("telemetry.jsonl.migrated").exists());

    // Read back via SQLite-backed stores
    let new_log = ReviewLog::with_storage(handle.clone());
    let reviews = new_log.load_all().unwrap();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].run_id, "01E2E");
    assert_eq!(reviews[0].repo, Some("e2e/test".to_string()));
    assert_eq!(reviews[0].findings_by_severity.high, 1);
    assert_eq!(reviews[0].flags.ensemble, true);
    assert_eq!(reviews[0].finding_ids, vec!["e2e-fid-1"]);

    let new_telem = TelemetryStore::with_storage(handle.clone());
    let (entries, stats) = new_telem.load_all_with_stats().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "gpt-5.4");
    assert_eq!(entries[0].findings.get("quality"), Some(&3));
    assert_eq!(stats.kept, 1);
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --bin quorum storage::tests::end_to_end_migration_and_readback -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/storage.rs
git commit -m "test(storage): add end-to-end migration integration test (#326)"
```

---

### Task 8: Clean Up — Remove Dead JSONL Code Paths

**Files:**
- Modify: `src/review_log.rs`
- Modify: `src/telemetry.rs`

- [ ] **Step 1: Assess JSONL backend usage**

Check if any code still constructs `ReviewLog::new(path)` or `TelemetryStore::new(path)` outside of tests. If `main.rs` now exclusively uses `with_storage()`, consider whether the JSONL backend can be removed or kept only for tests and migration.

Decision: Keep `ReviewLog::new()` and `TelemetryStore::new()` for now — they're used by existing tests and by the migration code in `storage.rs` (which reads JSONL via the old path). Mark them with a doc comment noting they exist for backward compatibility and migration support.

- [ ] **Step 2: Add doc comments**

```rust
/// Create a ReviewLog backed by a JSONL file.
/// Retained for backward compatibility, migration support, and tests.
/// New callers should use `with_storage()`.
pub fn new(path: PathBuf) -> Self { ... }
```

Same for `TelemetryStore::new()`.

- [ ] **Step 3: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass.

- [ ] **Step 4: Run clippy and format**

Run: `cargo clippy --bin quorum && cargo fmt --check`
Expected: Clean.

- [ ] **Step 5: Commit**

```bash
git add src/review_log.rs src/telemetry.rs
git commit -m "docs: mark JSONL constructors as migration-only (#326)"
```

---

### Task 9: Release Build Verification

**Files:** None (verification only)

- [ ] **Step 1: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass (including new SQLite tests).

- [ ] **Step 2: Run release build**

Run: `cargo build --release`
Expected: Compiles successfully. Binary size may increase slightly from SQLite bundling (already included via context system).

- [ ] **Step 3: Verify binary runs**

Run: `./target/release/quorum version`
Expected: Prints version.

- [ ] **Step 4: Test migration on real data (manual)**

```bash
# Back up real data first
cp ~/.quorum/reviews.jsonl ~/.quorum/reviews.jsonl.backup
cp ~/.quorum/telemetry.jsonl ~/.quorum/telemetry.jsonl.backup

# Run any quorum command to trigger migration
./target/release/quorum stats

# Verify migration
ls ~/.quorum/quorum.db
ls ~/.quorum/reviews.jsonl.migrated
ls ~/.quorum/telemetry.jsonl.migrated
```

- [ ] **Step 5: Commit (if any fixes needed)**

```bash
git commit -m "fix: address issues found during release verification (#326)"
```
