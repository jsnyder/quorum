# Fuzzy Title Matching Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve `quorum calibrate` corpus join rate from 13% to ~25% by adding normalized exact matching and fuzzy token Jaccard matching to `join_feedback_and_traces`.

**Architecture:** Add `normalize_title()` and `token_jaccard()` helper functions, then restructure the join in `join_feedback_and_traces` to attempt 4 tiers of matching in priority order. All changes confined to `src/calibrate.rs`.

**Tech Stack:** Rust, serde_json, std collections. No new dependencies.

---

### Task 1: Add `normalize_title` function

**Files:**
- Modify: `src/calibrate.rs` (add function after imports, before `join_feedback_and_traces`)

**Step 1: Write failing tests for normalize_title**

```rust
#[test]
fn normalize_strips_backticks() {
    assert_eq!(
        normalize_title("uses a fixed `.tmp` filename"),
        "uses a fixed tmp filename"
    );
}

#[test]
fn normalize_strips_rule_prefix() {
    assert_eq!(
        normalize_title("expect-empty-message: Empty .expect() message"),
        "empty expect message"
    );
}

#[test]
fn normalize_lowercases_and_collapses_whitespace() {
    assert_eq!(
        normalize_title("  Missing  Error  Context  "),
        "missing error context"
    );
}

#[test]
fn normalize_preserves_underscores() {
    assert_eq!(
        normalize_title("unwrap_or_default() silently drops errors"),
        "unwrap_or_default silently drops errors"
    );
}

#[test]
fn normalize_handles_empty_and_prefix_only() {
    assert_eq!(normalize_title(""), "");
    assert_eq!(normalize_title("rule-name: "), "");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib normalize_ -- --nocapture 2>&1 | head -30`
Expected: FAIL — `normalize_title` not found

**Step 3: Write minimal implementation**

```rust
/// Normalize a finding title for fuzzy comparison.
///
/// Strips rule-name prefixes, backticks, punctuation (except `_`),
/// lowercases, and collapses whitespace.
fn normalize_title(raw: &str) -> String {
    let stripped = if let Some(rest) = raw.strip_prefix(|_: char| false) {
        raw
    } else {
        raw
    };
    // Strip "rule-name: " prefix (lowercase-kebab-case followed by colon+space)
    let after_prefix = strip_rule_prefix(stripped);
    // Replace non-alphanumeric-non-underscore with space, lowercase, collapse
    let normalized: String = after_prefix
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    // Collapse whitespace and trim
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_rule_prefix(s: &str) -> &str {
    // Match ^[a-z][a-z0-9-]+:\s*
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return s;
    }
    let mut colon_pos = None;
    for (i, &b) in bytes.iter().enumerate().skip(1) {
        if b == b':' {
            colon_pos = Some(i);
            break;
        }
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return s;
        }
    }
    match colon_pos {
        Some(pos) if pos >= 2 => {
            // Skip colon and any following whitespace
            let rest = &s[pos + 1..];
            rest.trim_start()
        }
        _ => s,
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib normalize_ -- --nocapture`
Expected: PASS (5 tests)

**Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): add normalize_title for fuzzy matching (#207)"
```

---

### Task 2: Add `token_jaccard` function

**Files:**
- Modify: `src/calibrate.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn jaccard_identical_titles() {
    let j = token_jaccard("missing error context", "missing error context");
    assert!((j - 1.0).abs() < 1e-9);
}

#[test]
fn jaccard_disjoint_titles() {
    let j = token_jaccard("sql injection risk", "memory leak detected");
    assert!(j < 0.01);
}

#[test]
fn jaccard_partial_overlap() {
    // "empty expect message" vs "empty expect message provide context"
    // intersection=3, union=5, J=0.6
    let j = token_jaccard(
        "empty expect message",
        "empty expect message provide context",
    );
    assert!((j - 0.6).abs() < 0.01);
}

#[test]
fn jaccard_empty_returns_zero() {
    assert!((token_jaccard("", "something")).abs() < 1e-9);
    assert!((token_jaccard("something", "")).abs() < 1e-9);
    assert!((token_jaccard("", "")).abs() < 1e-9);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib jaccard_ -- --nocapture 2>&1 | head -20`
Expected: FAIL — `token_jaccard` not found

**Step 3: Write minimal implementation**

```rust
/// Compute token Jaccard similarity between two pre-normalized title strings.
fn token_jaccard(a: &str, b: &str) -> f64 {
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();
    let union_size = set_a.union(&set_b).count();
    if union_size == 0 {
        return 0.0;
    }
    let intersection_size = set_a.intersection(&set_b).count();
    intersection_size as f64 / union_size as f64
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib jaccard_ -- --nocapture`
Expected: PASS (4 tests)

**Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): add token_jaccard similarity (#207)"
```

---

### Task 3: Add normalized exact matching (Tier 2)

**Files:**
- Modify: `src/calibrate.rs`

**Step 1: Write failing test**

```rust
#[test]
fn normalized_exact_matches_backtick_difference() {
    let feedback = vec![make_feedback("uses a fixed .tmp filename", "tp", "src/a.rs")];
    let traces = vec![make_trace(
        "uses a fixed `.tmp` filename",
        2.0, 0.3, Some("src/a.rs"),
    )];
    let samples = join_feedback_and_traces(&feedback, &traces);
    assert_eq!(samples.len(), 1, "normalized exact should match backtick variants");
}

#[test]
fn normalized_exact_matches_rule_prefix() {
    let feedback = vec![make_feedback("Empty .expect() message", "fp", "src/b.rs")];
    let traces = vec![make_trace(
        "expect-empty-message: Empty `.expect()` message",
        0.2, 1.5, Some("src/b.rs"),
    )];
    let samples = join_feedback_and_traces(&feedback, &traces);
    assert_eq!(samples.len(), 1, "normalized exact should match rule-prefix variants");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib normalized_exact -- --nocapture`
Expected: FAIL — no match found

**Step 3: Implement normalized exact index in join**

Add a `norm_trace_map: HashMap<(String, String), (f64, f64)>` built from `normalize_title(title)` + `file_path`. In the feedback loop, after raw exact miss, try `norm_trace_map.get(&(normalize_title(&title), fp))`.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib -- --nocapture 2>&1 | tail -5`
Expected: ALL PASS (existing + new)

**Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): tier-2 normalized exact matching (#207)"
```

---

### Task 4: Add fuzzy same-file matching (Tier 3)

**Files:**
- Modify: `src/calibrate.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn fuzzy_same_file_matches_extended_title() {
    // Title with extra words appended — Jaccard ~0.6, same file
    let feedback = vec![make_feedback(
        "Reset can race with visit processing",
        "tp", "src/visit.rs",
    )];
    let traces = vec![make_trace(
        "Reset can race with visit processing and lose the cleaned state",
        2.0, 0.5, Some("src/visit.rs"),
    )];
    let samples = join_feedback_and_traces(&feedback, &traces);
    assert_eq!(samples.len(), 1, "fuzzy same-file should match extended title");
}

#[test]
fn fuzzy_same_file_rejects_below_threshold() {
    // Jaccard well below 0.5
    let feedback = vec![make_feedback("API key leak", "tp", "src/a.rs")];
    let traces = vec![make_trace(
        "Database connection pool exhaustion under load",
        2.0, 0.5, Some("src/a.rs"),
    )];
    let samples = join_feedback_and_traces(&feedback, &traces);
    assert!(samples.is_empty(), "below-threshold fuzzy should not match");
}

#[test]
fn fuzzy_same_file_rejects_ambiguous() {
    // Two traces in same file with similar Jaccard to the feedback title
    let feedback = vec![make_feedback(
        "error handling is missing",
        "tp", "src/a.rs",
    )];
    let traces = vec![
        make_trace("error handling is missing for IO", 2.0, 0.5, Some("src/a.rs")),
        make_trace("error handling is missing for parse", 1.0, 0.8, Some("src/a.rs")),
    ];
    let samples = join_feedback_and_traces(&feedback, &traces);
    assert!(samples.is_empty(), "ambiguous fuzzy matches should be skipped");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib fuzzy_same_file -- --nocapture`
Expected: FAIL

**Step 3: Implement fuzzy same-file matching**

After tier 1 (raw exact) and tier 2 (normalized exact) miss, scan all traces with the same `file_path`. Compute `token_jaccard(normalize_title(fb_title), normalize_title(trace_title))`. Accept if best >= 0.5 and best - second_best >= 0.1.

Constants:
```rust
const FUZZY_THRESHOLD: f64 = 0.5;
const FUZZY_AMBIGUITY_MARGIN: f64 = 0.1;
```

Build a `file_to_traces: HashMap<String, Vec<(String, (f64, f64))>>` index mapping file_path to `(normalized_title, weights)` for fuzzy lookup.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib -- --nocapture 2>&1 | tail -5`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): tier-3 fuzzy same-file Jaccard matching (#207)"
```

---

### Task 5: Add normalized title-only fallback (Tier 4)

**Files:**
- Modify: `src/calibrate.rs`

**Step 1: Write failing test**

```rust
#[test]
fn normalized_title_only_fallback_matches() {
    // Old trace without file_path, title differs by backticks
    let feedback = vec![make_feedback("fixed .tmp filename", "tp", "src/a.rs")];
    let traces = vec![make_trace("fixed `.tmp` filename", 1.5, 0.5, None)];
    let samples = join_feedback_and_traces(&feedback, &traces);
    assert_eq!(samples.len(), 1, "normalized title-only fallback should match");
}

#[test]
fn normalized_title_only_blocked_when_file_scoped_exists() {
    // Same invariant as existing test but with normalized titles
    let feedback = vec![make_feedback("fixed .tmp filename", "tp", "src/b.rs")];
    let traces = vec![
        make_trace("fixed `.tmp` filename", 2.5, 0.3, Some("src/a.rs")),
        make_trace("fixed `.tmp` filename", 0.1, 1.8, None),
    ];
    let samples = join_feedback_and_traces(&feedback, &traces);
    assert!(samples.is_empty(),
        "normalized title-only fallback blocked when file-scoped traces exist");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib normalized_title_only -- --nocapture`
Expected: FAIL

**Step 3: Implement normalized title-only index**

Add `norm_title_only_map: HashMap<String, (f64, f64)>` keyed by `normalize_title(title)` for traces without `file_path`. Apply the same `titles_with_file_scoped` guard using normalized titles.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib -- --nocapture 2>&1 | tail -5`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): tier-4 normalized title-only fallback (#207)"
```

---

### Task 6: Add match strategy logging

**Files:**
- Modify: `src/calibrate.rs`

**Step 1: Write failing test**

```rust
#[test]
fn join_returns_match_stats() {
    let feedback = vec![
        make_feedback("SQL injection", "tp", "src/db.rs"),
        make_feedback("uses a fixed .tmp filename", "fp", "src/a.rs"),
        make_feedback("no match at all xyz", "tp", "src/c.rs"),
    ];
    let traces = vec![
        make_trace("SQL injection", 2.0, 0.3, Some("src/db.rs")),
        make_trace("uses a fixed `.tmp` filename", 0.2, 1.5, Some("src/a.rs")),
    ];
    let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
    assert_eq!(samples.len(), 2);
    assert_eq!(stats.exact_raw, 1);
    assert_eq!(stats.exact_normalized, 1);
    assert!(stats.unmatched >= 1);
}
```

**Step 2: Run tests to verify they fail**

Expected: FAIL — return type changed

**Step 3: Add JoinStats struct, update return type**

```rust
#[derive(Debug, Default)]
pub struct JoinStats {
    pub exact_raw: usize,
    pub exact_normalized: usize,
    pub fuzzy_same_file: usize,
    pub normalized_title_only: usize,
    pub ambiguous_skipped: usize,
    pub below_threshold: usize,
    pub unmatched: usize,
}

pub fn join_feedback_and_traces(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
) -> (Vec<(f64, bool)>, JoinStats) {
```

Update all call sites (only `src/main.rs::run_calibrate`) and existing tests to destructure the new return type.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib -- --nocapture 2>&1 | tail -5`
Expected: ALL PASS

**Step 5: Update `run_calibrate` in main.rs to log stats**

Add `tracing::info!` and a human-readable stderr summary after join.

**Step 6: Commit**

```bash
git add src/calibrate.rs src/main.rs
git commit -m "feat(calibrate): add JoinStats observability for match tiers (#207)"
```

---

### Task 7: Update `run_calibrate` display and verify end-to-end

**Files:**
- Modify: `src/main.rs` (update `run_calibrate` to print JoinStats summary)

**Step 1: Write failing test**

```rust
// In calibrate.rs — integration-level test with all 4 tiers exercised
#[test]
fn all_four_tiers_exercised() {
    let feedback = vec![
        // Tier 1: raw exact
        make_feedback("SQL injection", "tp", "src/db.rs"),
        // Tier 2: normalized exact (backtick diff)
        make_feedback("fixed .tmp filename", "fp", "src/a.rs"),
        // Tier 3: fuzzy same-file (extended title)
        make_feedback("reset can race with processing", "tp", "src/v.rs"),
        // Tier 4: normalized title-only (old trace, no file_path)
        make_feedback("missing error context", "fp", "src/z.rs"),
        // No match
        make_feedback("completely unrelated xyz", "tp", "src/q.rs"),
    ];
    let traces = vec![
        make_trace("SQL injection", 2.0, 0.3, Some("src/db.rs")),
        make_trace("fixed `.tmp` filename", 0.2, 1.5, Some("src/a.rs")),
        make_trace("reset can race with processing and lose state", 1.5, 0.5, Some("src/v.rs")),
        make_trace("missing error context", 0.8, 1.0, None),
    ];
    let (samples, stats) = join_feedback_and_traces(&feedback, &traces);
    assert_eq!(samples.len(), 4, "4 of 5 should match");
    assert_eq!(stats.exact_raw, 1);
    assert_eq!(stats.exact_normalized, 1);
    assert_eq!(stats.fuzzy_same_file, 1);
    assert_eq!(stats.normalized_title_only, 1);
    assert_eq!(stats.unmatched, 1);
}
```

**Step 2: Run test to verify it fails**

Expected: depends on implementation order — may already pass if tasks 1-6 are complete

**Step 3: Wire JoinStats display in main.rs**

```rust
eprintln!("\nJoin strategy breakdown:");
eprintln!("  exact (raw):        {}", stats.exact_raw);
eprintln!("  exact (normalized): {}", stats.exact_normalized);
eprintln!("  fuzzy (same-file):  {}", stats.fuzzy_same_file);
eprintln!("  title-only (norm):  {}", stats.normalized_title_only);
eprintln!("  ambiguous skipped:  {}", stats.ambiguous_skipped);
eprintln!("  unmatched:          {}", stats.unmatched);
```

**Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS

**Step 5: Run `quorum calibrate` against real data**

Run: `cargo run -- calibrate`
Expected: Join rate significantly improved, stats displayed

**Step 6: Commit**

```bash
git add src/calibrate.rs src/main.rs
git commit -m "feat(calibrate): end-to-end fuzzy title matching (#207)"
```
