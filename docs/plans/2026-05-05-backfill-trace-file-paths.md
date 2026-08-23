# Backfill Trace File Paths Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `quorum calibrate --backfill-paths` to enrich legacy calibrator traces with file_path derived from the feedback corpus, improving corpus join quality from 2.7% to ~25% file_path coverage.

**Architecture:** A new public function `backfill_file_paths()` in `src/calibrate.rs` cross-references trace `finding_title` against feedback entries to infer `file_path`. Two-tier resolution: (1) normalized title match to feedback entries — if all feedback for that title points to one file, stamp it; (2) for unresolved traces, check `matched_precedents[*].file_path` — if all point to one file, use that. CLI wiring in `src/main.rs` reads traces, calls the function, writes an atomic backup + overwrite. Stats printed to stderr.

**Tech Stack:** Rust, serde_json, std::fs (atomic rename), clap (CLI flag)

---

### Task 1: `backfill_file_paths` core function — feedback cross-reference

**Files:**
- Modify: `src/calibrate.rs` (add function after `join_feedback_and_traces_with_options`)
- Test: `src/calibrate.rs` (inline `#[cfg(test)]` module)

**Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block in `src/calibrate.rs`:

```rust
#[test]
fn backfill_stamps_file_path_from_unambiguous_feedback() {
    let feedback = vec![
        serde_json::json!({
            "finding_title": "SQL injection risk",
            "file_path": "src/db.rs",
            "verdict": "tp"
        }),
        serde_json::json!({
            "finding_title": "SQL injection risk",
            "file_path": "src/db.rs",
            "verdict": "fp"
        }),
    ];
    let mut traces = vec![
        serde_json::json!({
            "finding_title": "SQL injection risk",
            "finding_category": "security",
            "tp_weight": 2.0,
            "fp_weight": 0.5
        }),
    ];
    let stats = backfill_file_paths(&mut traces, &feedback);
    assert_eq!(traces[0]["file_path"].as_str(), Some("src/db.rs"));
    assert_eq!(stats.feedback_exact, 1);
    assert_eq!(stats.total_backfilled, 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum backfill_stamps_file_path_from_unambiguous_feedback`
Expected: FAIL — `backfill_file_paths` does not exist

**Step 3: Write minimal implementation**

Add to `src/calibrate.rs`, after the `join_feedback_and_traces_with_options` function:

```rust
/// Stats from a `backfill_file_paths` run.
#[derive(Debug, Default)]
pub struct BackfillStats {
    /// Traces that already had file_path (skipped).
    pub already_present: usize,
    /// Backfilled via unambiguous feedback title match.
    pub feedback_exact: usize,
    /// Backfilled via unambiguous normalized feedback title match.
    pub feedback_normalized: usize,
    /// Backfilled via unambiguous matched_precedents file_path.
    pub precedent_inferred: usize,
    /// Ambiguous (2+ candidate files) — left as null.
    pub ambiguous: usize,
    /// No signal found — left as null.
    pub no_match: usize,
    /// Total traces modified.
    pub total_backfilled: usize,
}

/// Enrich legacy traces that lack `file_path` by cross-referencing the
/// feedback corpus. Two-tier resolution:
///
/// 1. **Feedback title match**: if all feedback entries for a given
///    `finding_title` (exact, then normalized) reference the same file,
///    stamp that file onto the trace.
/// 2. **Precedent inference**: if the trace's `matched_precedents` all
///    share a single `file_path`, use that.
///
/// Traces that already have `file_path` are skipped. Ambiguous cases
/// (multiple candidate files) are left as null.
pub fn backfill_file_paths(
    traces: &mut [serde_json::Value],
    feedback: &[serde_json::Value],
) -> BackfillStats {
    use std::collections::{HashMap, HashSet};

    // Build title -> set<file_path> from feedback (exact)
    let mut exact_map: HashMap<String, HashSet<String>> = HashMap::new();
    // Build normalized title -> set<file_path>
    let mut norm_map: HashMap<String, HashSet<String>> = HashMap::new();

    for f in feedback {
        let title = f["finding_title"].as_str().unwrap_or("");
        let fp = f["file_path"].as_str().unwrap_or("");
        if title.is_empty() || fp.is_empty() {
            continue;
        }
        exact_map
            .entry(title.to_string())
            .or_default()
            .insert(fp.to_string());
        let norm = normalize_title(title);
        if !norm.is_empty() {
            norm_map
                .entry(norm)
                .or_default()
                .insert(fp.to_string());
        }
    }

    let mut stats = BackfillStats::default();

    for trace in traces.iter_mut() {
        let existing = trace.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
        if !existing.is_empty() {
            stats.already_present += 1;
            continue;
        }

        let title = trace["finding_title"].as_str().unwrap_or("").to_string();
        if title.is_empty() {
            stats.no_match += 1;
            continue;
        }

        // Tier 1: exact title match
        if let Some(files) = exact_map.get(&title) {
            if files.len() == 1 {
                let fp = files.iter().next().unwrap().clone();
                trace["file_path"] = serde_json::Value::String(fp);
                stats.feedback_exact += 1;
                stats.total_backfilled += 1;
                continue;
            }
        }

        // Tier 2: normalized title match
        let norm = normalize_title(&title);
        if !norm.is_empty() {
            if let Some(files) = norm_map.get(&norm) {
                if files.len() == 1 {
                    let fp = files.iter().next().unwrap().clone();
                    trace["file_path"] = serde_json::Value::String(fp);
                    stats.feedback_normalized += 1;
                    stats.total_backfilled += 1;
                    continue;
                }
            }
        }

        // Tier 3: precedent inference
        let precs = trace.get("matched_precedents")
            .and_then(|v| v.as_array());
        if let Some(precs) = precs {
            let prec_files: HashSet<&str> = precs
                .iter()
                .filter_map(|p| p["file_path"].as_str())
                .filter(|s| !s.is_empty())
                .collect();
            if prec_files.len() == 1 {
                let fp = prec_files.into_iter().next().unwrap().to_string();
                trace["file_path"] = serde_json::Value::String(fp);
                stats.precedent_inferred += 1;
                stats.total_backfilled += 1;
                continue;
            }
        }

        // Check if any tier had candidates but they were ambiguous
        let had_exact = exact_map.get(&title).is_some_and(|f| f.len() > 1);
        let had_norm = !norm.is_empty()
            && norm_map.get(&norm).is_some_and(|f| f.len() > 1);
        if had_exact || had_norm {
            stats.ambiguous += 1;
        } else {
            stats.no_match += 1;
        }
    }

    stats
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum backfill_stamps_file_path_from_unambiguous_feedback`
Expected: PASS

**Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): add backfill_file_paths core function

Cross-references trace finding_title against feedback corpus to infer
file_path for legacy traces. Three tiers: exact title, normalized
title, matched_precedent inference."
```

---

### Task 2: Edge case tests for backfill

**Files:**
- Modify: `src/calibrate.rs` (tests module)

**Step 1: Write the failing tests**

Add three tests to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn backfill_skips_traces_with_existing_file_path() {
    let feedback = vec![
        serde_json::json!({
            "finding_title": "SQL injection",
            "file_path": "src/db.rs",
            "verdict": "tp"
        }),
    ];
    let mut traces = vec![
        serde_json::json!({
            "finding_title": "SQL injection",
            "finding_category": "security",
            "tp_weight": 1.0,
            "fp_weight": 0.0,
            "file_path": "src/other.rs"
        }),
    ];
    let stats = backfill_file_paths(&mut traces, &feedback);
    assert_eq!(traces[0]["file_path"].as_str(), Some("src/other.rs"));
    assert_eq!(stats.already_present, 1);
    assert_eq!(stats.total_backfilled, 0);
}

#[test]
fn backfill_leaves_ambiguous_as_null() {
    let feedback = vec![
        serde_json::json!({
            "finding_title": "Use of unwrap()",
            "file_path": "src/a.rs",
            "verdict": "tp"
        }),
        serde_json::json!({
            "finding_title": "Use of unwrap()",
            "file_path": "src/b.rs",
            "verdict": "fp"
        }),
    ];
    let mut traces = vec![
        serde_json::json!({
            "finding_title": "Use of unwrap()",
            "finding_category": "correctness",
            "tp_weight": 1.0,
            "fp_weight": 0.0
        }),
    ];
    let stats = backfill_file_paths(&mut traces, &feedback);
    assert!(
        traces[0].get("file_path").is_none()
            || traces[0]["file_path"].is_null()
            || traces[0]["file_path"].as_str() == Some(""),
        "ambiguous title should not be stamped"
    );
    assert_eq!(stats.ambiguous, 1);
    assert_eq!(stats.total_backfilled, 0);
}

#[test]
fn backfill_uses_precedent_when_feedback_ambiguous() {
    let feedback = vec![
        serde_json::json!({
            "finding_title": "Use of unwrap()",
            "file_path": "src/a.rs",
            "verdict": "tp"
        }),
        serde_json::json!({
            "finding_title": "Use of unwrap()",
            "file_path": "src/b.rs",
            "verdict": "fp"
        }),
    ];
    let mut traces = vec![
        serde_json::json!({
            "finding_title": "Use of unwrap()",
            "finding_category": "correctness",
            "tp_weight": 1.0,
            "fp_weight": 0.0,
            "matched_precedents": [
                {"finding_title": "unwrap risk", "file_path": "src/a.rs", "verdict": "tp"},
                {"finding_title": "unwrap again", "file_path": "src/a.rs", "verdict": "tp"}
            ]
        }),
    ];
    let stats = backfill_file_paths(&mut traces, &feedback);
    assert_eq!(traces[0]["file_path"].as_str(), Some("src/a.rs"));
    assert_eq!(stats.precedent_inferred, 1);
    assert_eq!(stats.total_backfilled, 1);
}
```

**Step 2: Run tests to verify they pass**

Run: `cargo test --bin quorum backfill_skips_traces backfill_leaves_ambiguous backfill_uses_precedent`
Expected: PASS (these exercise existing code paths from Task 1)

**Step 3: Commit**

```bash
git add src/calibrate.rs
git commit -m "test(calibrate): edge cases for backfill_file_paths

Cover: skip existing file_path, leave ambiguous as null,
fall through to precedent inference when feedback is ambiguous."
```

---

### Task 3: Normalized title fallback test

**Files:**
- Modify: `src/calibrate.rs` (tests module)

**Step 1: Write the failing test**

```rust
#[test]
fn backfill_falls_through_to_normalized_title() {
    let feedback = vec![
        serde_json::json!({
            "finding_title": "bare-except-pass: Using bare except: pass",
            "file_path": "src/handler.py",
            "verdict": "tp"
        }),
    ];
    let mut traces = vec![
        serde_json::json!({
            "finding_title": "Using bare except: pass",
            "finding_category": "correctness",
            "tp_weight": 1.0,
            "fp_weight": 0.0
        }),
    ];
    let stats = backfill_file_paths(&mut traces, &feedback);
    assert_eq!(traces[0]["file_path"].as_str(), Some("src/handler.py"));
    assert_eq!(stats.feedback_normalized, 1);
}
```

**Step 2: Run test to verify it passes**

Run: `cargo test --bin quorum backfill_falls_through_to_normalized_title`
Expected: PASS (normalized matching uses existing `normalize_title()` which strips rule prefixes)

**Step 3: Commit**

```bash
git add src/calibrate.rs
git commit -m "test(calibrate): normalized title fallback in backfill"
```

---

### Task 4: CLI flag `--backfill-paths` wiring

**Files:**
- Modify: `src/cli/mod.rs:258-294` (add flag to `CalibrateOpts`)
- Modify: `src/main.rs:1870-1968` (add backfill branch to `run_calibrate`)

**Step 1: Write the failing test**

Add to `src/calibrate.rs` tests — a test that exercises the function through the public API with realistic data:

```rust
#[test]
fn backfill_reports_correct_stats_on_mixed_corpus() {
    let feedback = vec![
        serde_json::json!({"finding_title": "A", "file_path": "f1.rs", "verdict": "tp"}),
        serde_json::json!({"finding_title": "B", "file_path": "f2.rs", "verdict": "tp"}),
        serde_json::json!({"finding_title": "C", "file_path": "f3.rs", "verdict": "tp"}),
        serde_json::json!({"finding_title": "C", "file_path": "f4.rs", "verdict": "fp"}),
    ];
    let mut traces = vec![
        // Already has file_path
        serde_json::json!({"finding_title": "A", "tp_weight": 1.0, "fp_weight": 0.0, "file_path": "f1.rs"}),
        // Exact match -> backfill
        serde_json::json!({"finding_title": "B", "tp_weight": 1.0, "fp_weight": 0.0}),
        // Ambiguous
        serde_json::json!({"finding_title": "C", "tp_weight": 1.0, "fp_weight": 0.0}),
        // No match
        serde_json::json!({"finding_title": "D", "tp_weight": 1.0, "fp_weight": 0.0}),
    ];
    let stats = backfill_file_paths(&mut traces, &feedback);
    assert_eq!(stats.already_present, 1);
    assert_eq!(stats.feedback_exact, 1);
    assert_eq!(stats.ambiguous, 1);
    assert_eq!(stats.no_match, 1);
    assert_eq!(stats.total_backfilled, 1);
}
```

**Step 2: Run test to verify it passes**

Run: `cargo test --bin quorum backfill_reports_correct_stats`
Expected: PASS

**Step 3: Add CLI flag**

In `src/cli/mod.rs`, add to `CalibrateOpts`:

```rust
    /// Backfill file_path on legacy traces using feedback cross-reference
    #[arg(long)]
    pub backfill_paths: bool,
```

**Step 4: Wire into `run_calibrate` in `src/main.rs`**

After loading feedback and traces (around line 1900), add a branch:

```rust
    if opts.backfill_paths {
        let mut traces_mut = traces;
        let stats = quorum::calibrate::backfill_file_paths(&mut traces_mut, &feedback);
        eprintln!("\nBackfill results:");
        eprintln!("  already had file_path: {}", stats.already_present);
        eprintln!("  feedback (exact):      {}", stats.feedback_exact);
        eprintln!("  feedback (normalized): {}", stats.feedback_normalized);
        eprintln!("  precedent inferred:    {}", stats.precedent_inferred);
        eprintln!("  ambiguous (skipped):   {}", stats.ambiguous);
        eprintln!("  no match (skipped):    {}", stats.no_match);
        eprintln!("  total backfilled:      {}", stats.total_backfilled);

        if stats.total_backfilled == 0 {
            eprintln!("\nNo traces to backfill.");
            return 0;
        }

        if opts.dry_run {
            eprintln!("\n(dry run -- no files written)");
            return 0;
        }

        // Atomic write: backup original, write new
        let bak_path = traces_path.with_extension("jsonl.bak");
        if let Err(e) = std::fs::copy(&traces_path, &bak_path) {
            eprintln!("error: failed to create backup: {e}");
            return 3;
        }
        eprintln!("Backup: {}", bak_path.display());

        let tmp_path = traces_path.with_extension("jsonl.tmp");
        let mut out = match std::fs::File::create(&tmp_path) {
            Ok(f) => std::io::BufWriter::new(f),
            Err(e) => {
                eprintln!("error: failed to create temp file: {e}");
                return 3;
            }
        };
        for t in &traces_mut {
            use std::io::Write;
            if let Err(e) = writeln!(out, "{}", serde_json::to_string(t).unwrap()) {
                eprintln!("error: write failed: {e}");
                return 3;
            }
        }
        drop(out);
        if let Err(e) = std::fs::rename(&tmp_path, &traces_path) {
            eprintln!("error: rename failed: {e}");
            return 3;
        }
        eprintln!("Wrote {}", traces_path.display());
        return 0;
    }
```

**Step 5: Run full test suite**

Run: `cargo test --bin quorum`
Expected: PASS (no regressions)

**Step 6: Commit**

```bash
git add src/cli/mod.rs src/main.rs src/calibrate.rs
git commit -m "feat(calibrate): add --backfill-paths CLI flag

Wires backfill_file_paths into quorum calibrate --backfill-paths.
Creates .bak backup before overwriting. Supports --dry-run. Prints
per-tier stats to stderr."
```

---

### Task 5: Integration test — dry run and actual backfill

**Files:**
- Modify: `tests/cli_integration.rs` (or whichever file has CLI integration tests)

**Step 1: Write the failing test**

```rust
#[test]
fn calibrate_backfill_paths_dry_run() {
    // Create temp dir with feedback + traces
    let dir = tempfile::tempdir().unwrap();
    let fb_path = dir.path().join("feedback.jsonl");
    let tr_path = dir.path().join("calibrator_traces.jsonl");

    std::fs::write(&fb_path, r#"{"finding_title":"A","file_path":"f.rs","verdict":"tp"}"#).unwrap();
    std::fs::write(&tr_path, r#"{"finding_title":"A","tp_weight":1.0,"fp_weight":0.0}"#).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_quorum"))
        .env("QUORUM_HOME", dir.path())
        .args(["calibrate", "--backfill-paths", "--dry-run"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("total backfilled:"), "should report stats");
    assert!(stderr.contains("dry run"), "should say dry run");

    // Traces file should be unchanged
    let content = std::fs::read_to_string(&tr_path).unwrap();
    assert!(!content.contains("f.rs"), "dry run should not modify file");
}
```

**Step 2: Run test to verify behavior**

Run: `cargo test --test cli_integration calibrate_backfill_paths_dry_run`

Note: This test depends on `QUORUM_HOME` being respected. Check whether main.rs `quorum_dir()` reads this env var. If not, this test may need adjustment (use the actual env var name or adjust `quorum_dir()` to support it).

**Step 3: Commit**

```bash
git add tests/cli_integration.rs
git commit -m "test: integration test for calibrate --backfill-paths --dry-run"
```

---

### Task 6: Run backfill on real corpus and measure join rate improvement

**Not a code task — manual verification step.**

**Step 1: Baseline**

```bash
cargo run -- calibrate 2>&1 | head -20
```

Record join stats (should match current: 362 samples, 16.1% join rate).

**Step 2: Dry run**

```bash
cargo run -- calibrate --backfill-paths --dry-run 2>&1
```

Verify stats look reasonable (~1,125 feedback + ~244 precedent).

**Step 3: Actual backfill**

```bash
cargo run -- calibrate --backfill-paths 2>&1
```

Verify backup created at `~/.quorum/calibrator_traces.jsonl.bak`.

**Step 4: Re-run calibrate and compare**

```bash
cargo run -- calibrate 2>&1 | head -20
```

Expected: join rate increases as file-scoped tiers activate.

**Step 5: Commit any adjustments**

If the backfill reveals issues (e.g., ambiguity logic needs tuning), fix and commit.
