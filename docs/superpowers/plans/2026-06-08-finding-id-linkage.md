# Finding ID Linkage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire finding_id through feedback recording so join health rises from 0% toward 85%+.

**Architecture:** Three layers: (1) schema migration adds title/file_path columns to `review_finding_ids` and writes metadata at review time, (2) `resolve_finding_id()` auto-links feedback to findings by fuzzy-matching file+title against recent reviews, (3) explicit `--finding-id` CLI flag and MCP `findingId` field bypass auto-link.

**Tech Stack:** Rust, rusqlite, serde_json, ulid, clap 4.5

---

### Task 1: Schema migration v2 -> v3 (add title + file_path columns)

**Files:**
- Modify: `src/storage.rs:23` (bump SCHEMA_VERSION to 3)
- Modify: `src/storage.rs:122-124` (add v3 migration dispatch)
- Modify: `src/storage.rs:207` (add `migrate_v2_to_v3` function)

- [ ] **Step 1: Write failing test for schema v3**

Add to the test module in `src/storage.rs`:

```rust
#[test]
fn migrate_v2_to_v3_adds_title_and_file_path_columns() {
    let conn = Connection::open_in_memory().unwrap();
    // Bootstrap v1 + v2
    migrate_v0_to_v1(&conn).unwrap();
    migrate_v1_to_v2(&conn).unwrap();
    // Insert a finding_id row before migration
    conn.execute(
        "INSERT INTO reviews (run_id, timestamp, quorum_version, invoked_from, model, files_reviewed, tokens_in, tokens_out, duration_ms) VALUES ('R1', '2026-01-01T00:00:00Z', '0.1', 'tty', 'gpt', 1, 0, 0, 0)",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO review_finding_ids (run_id, finding_id) VALUES ('R1', 'F1')",
        [],
    ).unwrap();
    // Run v3 migration
    migrate_v2_to_v3(&conn).unwrap();
    // Verify columns exist and legacy row has empty defaults
    let (title, file_path): (String, String) = conn.query_row(
        "SELECT title, file_path FROM review_finding_ids WHERE finding_id = 'F1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(title, "");
    assert_eq!(file_path, "");
    // Verify new inserts work with the columns
    conn.execute(
        "INSERT INTO review_finding_ids (run_id, finding_id, title, file_path) VALUES ('R1', 'F2', 'SQL injection', 'src/auth.rs')",
        [],
    ).unwrap();
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, 3);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum migrate_v2_to_v3`
Expected: FAIL — function doesn't exist

- [ ] **Step 3: Implement migration**

In `src/storage.rs`, bump the constant:
```rust
const SCHEMA_VERSION: u32 = 3;
```

Add the migration dispatch in `run_migrations` (after the v2 block):
```rust
    if version < 3 {
        migrate_v2_to_v3(conn).context("schema migration v2 -> v3 failed")?;
    }
```

Add the migration function after `migrate_v1_to_v2`:
```rust
/// Schema v3: add title + file_path to review_finding_ids for feedback auto-link.
fn migrate_v2_to_v3(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE review_finding_ids ADD COLUMN title TEXT NOT NULL DEFAULT '';
         ALTER TABLE review_finding_ids ADD COLUMN file_path TEXT NOT NULL DEFAULT '';",
    )?;
    tx.pragma_update(None, "user_version", 3)?;
    tx.commit()?;
    Ok(())
}
```

- [ ] **Step 4: Update SCHEMA_VERSION assertions in existing tests**

Find all `assert_eq!(version, SCHEMA_VERSION)` or `assert_eq!(version, 2)` in storage.rs tests and update to `3` (or use the constant).

- [ ] **Step 5: Run tests**

Run: `rtk cargo test --bin quorum storage`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
rtk git add src/storage.rs
git commit -m "feat(storage): schema v3 — add title + file_path to review_finding_ids (#436)"
```

---

### Task 2: Write FindingMeta at review time

**Files:**
- Modify: `src/review_log.rs` (add `FindingMeta` struct, update `record_sqlite` to write title/file_path)
- Modify: `src/main.rs:2002-2009` (build `FindingMeta` vec from `file_results`, pass to `record`)

- [ ] **Step 1: Write failing test for FindingMeta round-trip**

Add to the test module in `src/review_log.rs`:

```rust
#[test]
fn sqlite_finding_meta_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let log = sqlite_review_log(&dir);
    let mut record = sample_review_record();
    record.finding_ids = vec!["F1".into(), "F2".into()];
    let meta = vec![
        FindingMeta { id: "F1".into(), title: "SQL injection".into(), file_path: "src/auth.rs".into() },
        FindingMeta { id: "F2".into(), title: "XSS risk".into(), file_path: "src/web.rs".into() },
    ];
    log.record_with_meta(&record, &meta).unwrap();

    // Query back the metadata
    let conn = match &log.backend {
        Backend::Sqlite(h) => h.lock().unwrap(),
        _ => panic!("expected sqlite"),
    };
    let mut stmt = conn.prepare(
        "SELECT finding_id, title, file_path FROM review_finding_ids WHERE run_id = ?1 ORDER BY rowid"
    ).unwrap();
    let rows: Vec<(String, String, String)> = stmt.query_map(
        params![record.run_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap().map(|r| r.unwrap()).collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], ("F1".into(), "SQL injection".into(), "src/auth.rs".into()));
    assert_eq!(rows[1], ("F2".into(), "XSS risk".into(), "src/web.rs".into()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum sqlite_finding_meta_round_trip`
Expected: FAIL — `FindingMeta` and `record_with_meta` don't exist

- [ ] **Step 3: Add FindingMeta struct and record_with_meta**

In `src/review_log.rs`, add the struct near the top (after imports):

```rust
pub struct FindingMeta {
    pub id: String,
    pub title: String,
    pub file_path: String,
}
```

Add a new public method alongside `record()`:

```rust
pub fn record_with_meta(&self, entry: &ReviewRecord, meta: &[FindingMeta]) -> anyhow::Result<()> {
    match &self.backend {
        Backend::Jsonl(path) => Self::record_jsonl(path, entry),
        Backend::Sqlite(handle) => Self::record_sqlite_with_meta(handle, entry, meta),
    }
}
```

Add `record_sqlite_with_meta` — copy of `record_sqlite` but with the extended INSERT:

```rust
fn record_sqlite_with_meta(
    handle: &StorageHandle,
    entry: &ReviewRecord,
    meta: &[FindingMeta],
) -> anyhow::Result<()> {
    use rusqlite::params;

    let conn = handle
        .lock()
        .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
    let tx = conn.unchecked_transaction()?;

    let context_json = serde_json::to_string(&entry.context)?;
    let suppressed_json = serde_json::to_string(&entry.suppressed_by_rule)?;
    let ts = entry.timestamp.to_rfc3339();

    #[allow(clippy::cast_possible_wrap)]
    tx.execute(
        "INSERT INTO reviews (
            run_id, timestamp, quorum_version, repo, invoked_from, model,
            files_reviewed, lines_added, lines_removed,
            critical, high, medium, low, info,
            suppressed_by_rule,
            tokens_in, tokens_out, tokens_cache_read, duration_ms,
            flag_deep, flag_parallel_n, flag_ensemble,
            mode, context
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9,
            ?10, ?11, ?12, ?13, ?14,
            ?15,
            ?16, ?17, ?18, ?19,
            ?20, ?21, ?22,
            ?23, ?24
        )",
        params![
            entry.run_id, ts, entry.quorum_version, entry.repo,
            entry.invoked_from, entry.model, entry.files_reviewed,
            entry.lines_added.map(i64::from), entry.lines_removed.map(i64::from),
            i64::from(entry.findings_by_severity.critical),
            i64::from(entry.findings_by_severity.high),
            i64::from(entry.findings_by_severity.medium),
            i64::from(entry.findings_by_severity.low),
            i64::from(entry.findings_by_severity.info),
            suppressed_json,
            entry.tokens_in as i64, entry.tokens_out as i64,
            entry.tokens_cache_read as i64, entry.duration_ms as i64,
            i32::from(entry.flags.deep), i64::from(entry.flags.parallel_n),
            i32::from(entry.flags.ensemble), entry.mode, context_json,
        ],
    )?;

    for fm in meta {
        tx.execute(
            "INSERT INTO review_finding_ids (run_id, finding_id, title, file_path) VALUES (?1, ?2, ?3, ?4)",
            params![entry.run_id, fm.id, fm.title, fm.file_path],
        )?;
    }

    tx.commit()?;
    Ok(())
}
```

- [ ] **Step 4: Run test**

Run: `rtk cargo test --bin quorum sqlite_finding_meta_round_trip`
Expected: PASS

- [ ] **Step 5: Wire into main.rs**

In `src/main.rs`, replace the `record()` call at line ~2007 with `record_with_meta()`. Build the meta from `file_results`:

```rust
        let finding_meta: Vec<review_log::FindingMeta> = file_results
            .iter()
            .flat_map(|fr| {
                fr.findings.iter().map(|f| review_log::FindingMeta {
                    id: f.id.clone(),
                    title: f.title.clone(),
                    file_path: fr.file_path.clone(),
                })
            })
            .collect();
        let record = review_log::ReviewRecord {
            // ... existing fields unchanged ...
            finding_ids: quorum::finding::collect_finding_ids(&all_findings),
            // ...
        };
        if let Err(e) = review_log.record_with_meta(&record, &finding_meta) {
            eprintln!("Warning: failed to write review log: {}", e);
        }
```

- [ ] **Step 6: Run all tests**

Run: `rtk cargo test --bin quorum review_log`
Expected: ALL PASS

- [ ] **Step 7: Commit**

```bash
rtk git add src/review_log.rs src/main.rs
git commit -m "feat(review-log): write FindingMeta (title + file_path) at review time (#436)"
```

---

### Task 3: Implement resolve_finding_id auto-link

**Files:**
- Modify: `src/review_log.rs` (add `resolve_finding_id` function)

- [ ] **Step 1: Write failing tests**

Add to the test module in `src/review_log.rs`:

```rust
#[test]
fn resolve_finding_id_exact_match() {
    let dir = tempfile::tempdir().unwrap();
    let log = sqlite_review_log(&dir);
    let mut record = sample_review_record();
    record.finding_ids = vec!["F1".into()];
    let meta = vec![FindingMeta {
        id: "F1".into(),
        title: "SQL injection risk".into(),
        file_path: "src/auth.rs".into(),
    }];
    log.record_with_meta(&record, &meta).unwrap();

    let result = log.resolve_finding_id("src/auth.rs", "SQL injection risk");
    assert_eq!(result, Some("F1".to_string()));
}

#[test]
fn resolve_finding_id_partial_match() {
    let dir = tempfile::tempdir().unwrap();
    let log = sqlite_review_log(&dir);
    let mut record = sample_review_record();
    record.finding_ids = vec!["F1".into()];
    let meta = vec![FindingMeta {
        id: "F1".into(),
        title: "SQL injection vulnerability in auth module".into(),
        file_path: "src/auth.rs".into(),
    }];
    log.record_with_meta(&record, &meta).unwrap();

    let result = log.resolve_finding_id("src/auth.rs", "SQL injection");
    assert_eq!(result, Some("F1".to_string()));
}

#[test]
fn resolve_finding_id_no_match_wrong_file() {
    let dir = tempfile::tempdir().unwrap();
    let log = sqlite_review_log(&dir);
    let mut record = sample_review_record();
    record.finding_ids = vec!["F1".into()];
    let meta = vec![FindingMeta {
        id: "F1".into(),
        title: "SQL injection".into(),
        file_path: "src/auth.rs".into(),
    }];
    log.record_with_meta(&record, &meta).unwrap();

    let result = log.resolve_finding_id("src/other.rs", "SQL injection");
    assert_eq!(result, None);
}

#[test]
fn resolve_finding_id_no_match_below_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let log = sqlite_review_log(&dir);
    let mut record = sample_review_record();
    record.finding_ids = vec!["F1".into()];
    let meta = vec![FindingMeta {
        id: "F1".into(),
        title: "SQL injection vulnerability".into(),
        file_path: "src/auth.rs".into(),
    }];
    log.record_with_meta(&record, &meta).unwrap();

    let result = log.resolve_finding_id("src/auth.rs", "completely unrelated finding title xyz");
    assert_eq!(result, None);
}

#[test]
fn resolve_finding_id_skips_legacy_empty_title() {
    let dir = tempfile::tempdir().unwrap();
    let log = sqlite_review_log(&dir);
    let mut record = sample_review_record();
    record.finding_ids = vec!["LEGACY".into()];
    // Use record() (not record_with_meta) to simulate legacy row with empty title
    log.record(&record).unwrap();

    let result = log.resolve_finding_id("src/auth.rs", "SQL injection");
    assert_eq!(result, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test --bin quorum resolve_finding_id`
Expected: FAIL — function doesn't exist

- [ ] **Step 3: Implement resolve_finding_id**

Add to `impl ReviewLog` in `src/review_log.rs`:

```rust
pub fn resolve_finding_id(&self, file_path: &str, finding_title: &str) -> Option<String> {
    let Backend::Sqlite(handle) = &self.backend else {
        return None;
    };
    let conn = handle.lock().ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT rfi.finding_id, rfi.title
             FROM review_finding_ids rfi
             JOIN reviews r ON r.run_id = rfi.run_id
             WHERE rfi.file_path = ?1
               AND rfi.title <> ''
             ORDER BY r.timestamp DESC
             LIMIT 50",
        )
        .ok()?;
    let candidates: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![file_path], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();

    let query_lower = finding_title.to_lowercase();
    let query_words: std::collections::HashSet<&str> = query_lower.split_whitespace().collect();

    let mut best_id: Option<String> = None;
    let mut best_score: f64 = 0.0;

    for (fid, title) in &candidates {
        let title_lower = title.to_lowercase();
        let title_words: std::collections::HashSet<&str> =
            title_lower.split_whitespace().collect();

        let intersection = query_words.intersection(&title_words).count();
        let union = query_words.union(&title_words).count();
        let jaccard = if union > 0 {
            intersection as f64 / union as f64
        } else {
            0.0
        };

        let substring_bonus = if title_lower.contains(&query_lower)
            || query_lower.contains(&title_lower)
        {
            0.3
        } else {
            0.0
        };

        let score = (jaccard + substring_bonus).min(1.0);
        if score > best_score {
            best_score = score;
            best_id = Some(fid.clone());
        }
    }

    if best_score >= 0.6 {
        tracing::debug!(
            finding_id = best_id.as_deref().unwrap_or(""),
            score = best_score,
            file = file_path,
            "auto-linked feedback to finding"
        );
        best_id
    } else {
        tracing::info!(
            file = file_path,
            title = finding_title,
            best_score = best_score,
            "no auto-link match found for feedback"
        );
        None
    }
}
```

- [ ] **Step 4: Run tests**

Run: `rtk cargo test --bin quorum resolve_finding_id`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
rtk git add src/review_log.rs
git commit -m "feat(review-log): add resolve_finding_id auto-link for feedback (#436)"
```

---

### Task 4: Wire auto-link into feedback recording paths

**Files:**
- Modify: `src/main.rs:2338-2354` (run_feedback_inner — populate finding_id)
- Modify: `src/main.rs:2406-2439` (run_feedback external path — populate finding_id)
- Modify: `src/feedback.rs` (add `finding_id` to `ExternalVerdictInput`, wire through `record_external`)

- [ ] **Step 1: Add finding_id to ExternalVerdictInput**

In `src/feedback.rs`, add the field to `ExternalVerdictInput`:

```rust
pub struct ExternalVerdictInput {
    pub file_path: String,
    pub finding_title: String,
    pub finding_category: Option<String>,
    pub verdict: Verdict,
    pub reason: String,
    pub agent: String,
    pub agent_model: Option<String>,
    pub confidence: Option<f32>,
    pub in_diff: Option<bool>,
    pub finding_id: Option<String>,  // NEW
}
```

In `record_external`, change `finding_id: None` to `finding_id: input.finding_id`:

```rust
        let entry = FeedbackEntry {
            // ... existing fields ...
            finding_id: input.finding_id,
            // ...
        };
```

- [ ] **Step 2: Wire auto-link into run_feedback_inner**

In `src/main.rs`, the `run_feedback_inner` function constructs a `FeedbackEntry` at line ~2338. Before the entry construction, add auto-link resolution. The function needs access to a `ReviewLog` — add a parameter or construct one inline:

```rust
fn run_feedback_inner(
    file: &str,
    finding: &str,
    verdict_str: &str,
    reason: &str,
    model: Option<&str>,
    blamed_chunks: Option<&str>,
    category: Option<&str>,
    fp_kind: Option<feedback::FpKind>,
    in_diff: Option<bool>,
    provenance: Option<feedback::Provenance>,
    finding_id_override: Option<String>,  // NEW
    json: bool,
    feedback_path: &std::path::Path,
) -> (i32, String) {
```

Before constructing the entry, resolve the finding_id:

```rust
    let finding_id = finding_id_override.or_else(|| {
        let review_path = feedback_path
            .parent()
            .map(|p| p.join("quorum.db"))
            .unwrap_or_default();
        if review_path.is_file() {
            let log = review_log::ReviewLog::new(review_path);
            log.resolve_finding_id(file, finding)
        } else {
            None
        }
    });
```

Then use it in the entry:
```rust
    let entry = feedback::FeedbackEntry {
        // ... existing fields ...
        finding_id,
        // ...
    };
```

- [ ] **Step 3: Wire auto-link into run_feedback external path**

In the external path at line ~2422, add the same resolution and pass through `ExternalVerdictInput`:

```rust
        let finding_id = opts.finding_id.clone().or_else(|| {
            let review_path = feedback_path
                .parent()
                .map(|p| p.join("quorum.db"))
                .unwrap_or_default();
            if review_path.is_file() {
                let log = review_log::ReviewLog::new(review_path);
                log.resolve_finding_id(&opts.file, &opts.finding)
            } else {
                None
            }
        });
        let input = feedback::ExternalVerdictInput {
            // ... existing fields ...
            finding_id,
        };
```

- [ ] **Step 4: Update all callers of run_feedback_inner**

Search for all calls to `run_feedback_inner` and add the new `finding_id_override` parameter (pass `opts.finding_id.clone()` from CLI, `None` from MCP if not yet wired).

- [ ] **Step 5: Run tests**

Run: `rtk cargo test --bin quorum feedback`
Expected: ALL PASS (compile check — integration tested later)

- [ ] **Step 6: Commit**

```bash
rtk git add src/main.rs src/feedback.rs
git commit -m "feat(feedback): wire auto-link into human and external feedback paths (#436)"
```

---

### Task 5: Add --finding-id CLI flag and MCP findingId field

**Files:**
- Modify: `src/cli/mod.rs:658-753` (add `--finding-id` to `FeedbackOpts`)
- Modify: `src/mcp/tools.rs:40-99` (add `findingId` to `FeedbackTool`)

- [ ] **Step 1: Write failing test for ULID validation**

Add to `src/cli/mod.rs` test module:

```rust
#[test]
fn finding_id_valid_ulid_accepted() {
    let valid = "01HXYZ1234567890ABCDEFGHJK";
    assert!(ulid::Ulid::from_string(valid).is_ok());
}

#[test]
fn finding_id_invalid_rejected() {
    let invalid = "not-a-ulid";
    assert!(ulid::Ulid::from_string(invalid).is_err());
}
```

- [ ] **Step 2: Add --finding-id flag to FeedbackOpts**

In `src/cli/mod.rs`, add to `FeedbackOpts`:

```rust
    /// Explicit finding ID (ULID) to link this feedback entry to a specific
    /// review finding. Bypasses auto-link resolution. Use when the finding ID
    /// is known from review JSON output.
    #[arg(long, value_parser = parse_finding_id)]
    pub finding_id: Option<String>,
```

Add the parser function:

```rust
fn parse_finding_id(s: &str) -> Result<String, String> {
    ulid::Ulid::from_string(s)
        .map(|u| u.to_string())
        .map_err(|e| format!("invalid ULID: {e}"))
}
```

- [ ] **Step 3: Add findingId to MCP FeedbackTool**

In `src/mcp/tools.rs`, add to `FeedbackTool`:

```rust
    /// Explicit finding ID (ULID) to link this feedback to a specific review
    /// finding. Bypasses auto-link resolution.
    #[serde(default, rename = "findingId", skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,
```

- [ ] **Step 4: Wire MCP findingId through to feedback recording**

In the MCP feedback handler (search for where `FeedbackTool` is consumed), pass `finding_id` through to `run_feedback_inner` or `ExternalVerdictInput`.

- [ ] **Step 5: Run tests**

Run: `rtk cargo test --bin quorum finding_id`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
rtk git add src/cli/mod.rs src/mcp/tools.rs
git commit -m "feat(cli+mcp): add --finding-id flag and findingId MCP field (#436)"
```

---

### Task 6: Verify JSON output includes Finding.id

**Files:**
- Test: `src/output/mod.rs` (verify existing behavior)

- [ ] **Step 1: Write test verifying Finding.id in JSON**

Add to `src/output/mod.rs` test module:

```rust
#[test]
fn json_output_includes_finding_id() {
    let f = Finding::builder()
        .title("SQL injection")
        .description("desc")
        .severity(Severity::High)
        .category(Category::Security)
        .source(Source::Llm)
        .line_start(42)
        .line_end(42)
        .build();
    assert!(!f.id.is_empty(), "Finding should have a ULID id");

    let json = serde_json::to_string(&f).unwrap();
    assert!(
        json.contains(&format!("\"id\":\"{}\"", f.id)),
        "JSON should include the finding id field: {json}"
    );
}
```

- [ ] **Step 2: Run test**

Run: `rtk cargo test --bin quorum json_output_includes_finding_id`
Expected: PASS (Finding already serializes `id` via derive(Serialize))

- [ ] **Step 3: Commit**

```bash
rtk git add src/output/mod.rs
git commit -m "test(output): verify Finding.id included in JSON output (#436)"
```
