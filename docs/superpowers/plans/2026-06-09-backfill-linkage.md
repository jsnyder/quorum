# Backfill Linkage + Markdown Normalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix markdown normalization in the resolver (#438), add `quorum backfill-linkage` command to re-link legacy feedback entries (#439).

**Architecture:** Two tasks: (1) add `normalize_title()` to the resolver's tokenization path, (2) add a CLI command that loads all feedback, re-runs the resolver on unlinked entries, and atomically rewrites the file.

**Tech Stack:** Rust, rusqlite, serde_json, clap 4.5, fs2 (file locking)

---

### Task 1: Markdown normalization in resolve_finding_id (#438)

**Files:**
- Modify: `src/review_log.rs:622-696` (resolve_finding_id function)
- Test: `src/review_log.rs` (inline test module)

- [ ] **Step 1: Write failing test for backtick normalization**

Add to the test module in `src/review_log.rs`:

```rust
#[test]
fn resolve_finding_id_matches_despite_backticks() {
    let dir = tempfile::tempdir().unwrap();
    let log = sqlite_review_log(&dir);
    let mut record = sample_review_record();
    record.finding_ids = vec!["F1".into()];
    let meta = vec![FindingMeta {
        id: "F1".into(),
        title: "`predict_one` trusts inconsistent public `LogisticFit` field lengths".into(),
        file_path: "src/logistic.rs".into(),
    }];
    log.record_with_meta(&record, &meta).unwrap();

    let result = log.resolve_finding_id(
        "src/logistic.rs",
        "predict_one trusts inconsistent public LogisticFit field lengths",
    );
    assert_eq!(result, Some("F1".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum resolve_finding_id_matches_despite_backticks`
Expected: FAIL — backtick tokens don't match plain tokens

- [ ] **Step 3: Add normalize_title and apply in resolve_finding_id**

In `src/review_log.rs`, add a helper function near `resolve_finding_id`:

```rust
fn normalize_title(s: &str) -> String {
    s.replace(['`', '*', '_'], "").to_lowercase()
}
```

In `resolve_finding_id`, replace the two lines that do `.to_lowercase()` with calls to `normalize_title`:

```rust
// Before:
        let query_lower = finding_title.to_lowercase();
// After:
        let query_lower = normalize_title(finding_title);

// Before (inside the loop):
            let title_lower = title.to_lowercase();
// After:
            let title_lower = normalize_title(title);
```

Also update the substring bonus comparison to use the normalized versions (already using `query_lower` and `title_lower`, so no additional change needed there).

- [ ] **Step 4: Write test for normalize_title directly**

```rust
#[test]
fn normalize_title_strips_markdown() {
    assert_eq!(normalize_title("`predict_one` is **bad**"), "predict_one is bad");
    assert_eq!(normalize_title("no_formatting_here"), "noformattinghere");
    assert_eq!(normalize_title(""), "");
    assert_eq!(normalize_title("plain text"), "plain text");
}
```

- [ ] **Step 5: Run all resolve tests**

Run: `rtk cargo test --bin quorum resolve_finding_id`
Expected: ALL PASS (including the new backtick test)

- [ ] **Step 6: Commit**

```bash
rtk git add src/review_log.rs
git commit -m "fix(review-log): normalize markdown formatting in resolve_finding_id (#438)"
```

---

### Task 2: quorum backfill-linkage CLI command (#439)

**Files:**
- Modify: `src/cli/mod.rs:12-31` (add BackfillLinkage variant to Command enum)
- Modify: `src/main.rs:449-458` (add dispatch for BackfillLinkage)
- Modify: `src/main.rs` (add `run_backfill_linkage` function)
- Test: `src/main.rs` (inline test)

- [ ] **Step 1: Add BackfillLinkageOpts to CLI**

In `src/cli/mod.rs`, add to the `Command` enum (after `Calibrate`):

```rust
    /// Re-link legacy feedback entries to review findings
    BackfillLinkage(BackfillLinkageOpts),
```

Add the opts struct (near other opts structs):

```rust
/// Options for `quorum backfill-linkage`.
#[derive(Parser)]
pub struct BackfillLinkageOpts {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

- [ ] **Step 2: Add dispatch in main.rs**

In `src/main.rs`, in the main match block (around line 453), add:

```rust
        cli::Command::BackfillLinkage(opts) => {
            std::process::exit(run_backfill_linkage(opts))
        }
```

- [ ] **Step 3: Write failing test for backfill logic**

Add to the test module in `src/main.rs`:

```rust
#[test]
fn backfill_linkage_links_matching_entries() {
    let dir = tempfile::tempdir().unwrap();
    let quorum_home = dir.path();

    // Set up SQLite DB with a review that has metadata
    let conn = crate::storage::initialize(quorum_home).unwrap();
    let log = review_log::ReviewLog::with_storage(conn);
    let mut record = review_log::ReviewRecord::sample_for_test();
    record.finding_ids = vec!["FIND1".into()];
    let meta = vec![review_log::FindingMeta {
        id: "FIND1".into(),
        title: "SQL injection risk".into(),
        file_path: "src/auth.rs".into(),
    }];
    log.record_with_meta(&record, &meta).unwrap();

    // Set up feedback.jsonl with an unlinked entry that matches
    let feedback_path = quorum_home.join("feedback.jsonl");
    let entry = feedback::FeedbackEntry {
        file_path: "src/auth.rs".into(),
        finding_title: "SQL injection risk".into(),
        finding_category: "security".into(),
        verdict: feedback::Verdict::Tp,
        reason: "confirmed".into(),
        model: None,
        timestamp: chrono::Utc::now(),
        provenance: feedback::Provenance::Human,
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff: None,
        skill_name: None,
        skill_version: None,
        manifest_sha256: None,
    };
    let store = feedback::FeedbackStore::new(feedback_path.clone());
    store.record(&entry).unwrap();

    // Run backfill
    let (linked, total) = backfill_linkage_inner(quorum_home);

    // Verify
    assert_eq!(total, 1, "should process 1 entry");
    assert_eq!(linked, 1, "should link 1 entry");

    // Verify the file was rewritten with finding_id populated
    let reloaded = feedback::FeedbackStore::new(feedback_path).load_all().unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].finding_id.as_deref(), Some("FIND1"));
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `rtk cargo test --bin quorum backfill_linkage_links`
Expected: FAIL — functions don't exist

- [ ] **Step 5: Implement backfill_linkage_inner and run_backfill_linkage**

Add to `src/main.rs`:

```rust
fn backfill_linkage_inner(quorum_home: &std::path::Path) -> (usize, usize) {
    let feedback_path = quorum_home.join("feedback.jsonl");
    let store = feedback::FeedbackStore::new(feedback_path.clone());

    let entries = match store.load_all() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error: failed to load feedback: {e}");
            return (0, 0);
        }
    };

    let conn = match crate::storage::initialize(quorum_home) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: failed to open review database: {e}");
            return (0, 0);
        }
    };
    let log = review_log::ReviewLog::with_storage(conn);

    let mut linked = 0usize;
    let mut already = 0usize;
    let mut updated_entries = Vec::with_capacity(entries.len());

    for mut entry in entries {
        if entry.finding_id.is_some() {
            already += 1;
            updated_entries.push(entry);
            continue;
        }
        if let Some(fid) = log.resolve_finding_id(&entry.file_path, &entry.finding_title) {
            entry.finding_id = Some(fid);
            linked += 1;
        }
        updated_entries.push(entry);
    }

    // Atomic rewrite
    let tmp_path = feedback_path.with_extension("jsonl.tmp");
    if let Err(e) = (|| -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp_path)?;
        for entry in &updated_entries {
            let mut line = serde_json::to_string(entry)?;
            line.push('\n');
            file.write_all(line.as_bytes())?;
        }
        file.sync_all()?;
        std::fs::rename(&tmp_path, &feedback_path)?;
        Ok(())
    })() {
        eprintln!("Error: failed to rewrite feedback file: {e}");
        let _ = std::fs::remove_file(&tmp_path);
        return (0, updated_entries.len() - already);
    }

    (linked, updated_entries.len() - already)
}

fn run_backfill_linkage(opts: cli::BackfillLinkageOpts) -> i32 {
    let quorum_home = quorum_dir().unwrap_or_else(|| std::path::PathBuf::from(".quorum"));
    let feedback_path = quorum_home.join("feedback.jsonl");

    let store = feedback::FeedbackStore::new(feedback_path);
    let entries = match store.load_all() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error: failed to load feedback: {e}");
            return 3;
        }
    };
    let total = entries.len();
    let already_linked = entries.iter().filter(|e| e.finding_id.is_some()).count();
    drop(entries);

    let (newly_linked, candidates) = backfill_linkage_inner(&quorum_home);
    let no_match = candidates - newly_linked;

    let use_compact = output::should_use_compact(false);
    let use_json = opts.json
        || (!use_compact && !std::io::IsTerminal::is_terminal(&std::io::stdout()));

    if use_json {
        let json = serde_json::json!({
            "processed": total,
            "already_linked": already_linked,
            "newly_linked": newly_linked,
            "no_match": no_match,
        });
        println!("{}", json);
    } else if use_compact {
        println!(
            "backfill: {} processed, {} already, {} linked, {} no-match",
            total, already_linked, newly_linked, no_match
        );
    } else {
        println!("Backfill complete");
        println!("  Processed: {} entries", total);
        println!("  Already linked: {}", already_linked);
        println!("  Newly linked: {}", newly_linked);
        println!("  No match: {}", no_match);
    }

    0
}
```

- [ ] **Step 6: Run test**

Run: `rtk cargo test --bin quorum backfill_linkage_links`
Expected: PASS

- [ ] **Step 7: Write idempotency test**

```rust
#[test]
fn backfill_linkage_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let quorum_home = dir.path();

    let conn = crate::storage::initialize(quorum_home).unwrap();
    let log = review_log::ReviewLog::with_storage(conn);
    let mut record = review_log::ReviewRecord::sample_for_test();
    record.finding_ids = vec!["FIND1".into()];
    let meta = vec![review_log::FindingMeta {
        id: "FIND1".into(),
        title: "Bug".into(),
        file_path: "src/a.rs".into(),
    }];
    log.record_with_meta(&record, &meta).unwrap();

    let feedback_path = quorum_home.join("feedback.jsonl");
    let entry = feedback::FeedbackEntry {
        file_path: "src/a.rs".into(),
        finding_title: "Bug".into(),
        finding_category: "manual".into(),
        verdict: feedback::Verdict::Tp,
        reason: "r".into(),
        model: None,
        timestamp: chrono::Utc::now(),
        provenance: feedback::Provenance::Human,
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff: None,
        skill_name: None,
        skill_version: None,
        manifest_sha256: None,
    };
    let store = feedback::FeedbackStore::new(feedback_path.clone());
    store.record(&entry).unwrap();

    // First run links it
    let (linked1, _) = backfill_linkage_inner(quorum_home);
    assert_eq!(linked1, 1);

    // Second run links 0 (already linked)
    let (linked2, _) = backfill_linkage_inner(quorum_home);
    assert_eq!(linked2, 0);

    // Entry still has the finding_id
    let reloaded = feedback::FeedbackStore::new(feedback_path).load_all().unwrap();
    assert_eq!(reloaded[0].finding_id.as_deref(), Some("FIND1"));
}
```

- [ ] **Step 8: Run all tests**

Run: `rtk cargo test --bin quorum backfill_linkage`
Expected: ALL PASS

- [ ] **Step 9: Commit**

```bash
rtk git add src/cli/mod.rs src/main.rs
git commit -m "feat(cli): add quorum backfill-linkage command (#439)"
```
