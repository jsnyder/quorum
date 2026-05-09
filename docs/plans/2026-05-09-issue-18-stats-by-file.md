# Stats: File Hotspots from Feedback (`--by-file`) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Surface files that consistently produce true-positive findings, ranked by TP count, using the existing feedback store (no schema migration).

**Architecture:** Aggregate FeedbackEntry by file_path at stats time. Dedicated `FileHotspotRow` type, `group_by_file()` function, `--by-file` CLI flag, dedicated formatter. Zero changes to ReviewRecord.

**Tech Stack:** Rust, serde, clap, chrono

**Reviewed by:** GPT-5.4 (PAL codereview). Original schema-heavy plan scrapped in favor of feedback-based approach after complexity/utility review.

---

### Task 1: FileHotspotRow and group_by_file

**Files:**
- Modify: `src/dimensions.rs`

**Step 1: Define FileHotspotRow**

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FileHotspotRow {
    pub file_path: String,
    pub tp_count: u32,
    pub fp_count: u32,
    pub wontfix_count: u32,
    pub partial_count: u32,
    pub total: u32,
    pub last_seen: DateTime<Utc>,
}
```

**Step 2: Write failing tests for group_by_file**

Test cases:
- Multiple entries for same file aggregate correctly
- Sorting: by tp_count desc, then total desc
- top_n limits output (None = unlimited)
- Empty input returns empty vec
- last_seen is max timestamp for that file
- Different verdicts counted in correct buckets

**Step 3: Implement group_by_file**

```rust
pub fn group_by_file(entries: &[FeedbackEntry], top_n: Option<usize>) -> Vec<FileHotspotRow>
```

Iterate entries, group by file_path, count verdicts, track max timestamp, sort, optionally truncate.

**Step 4: Run tests, verify pass**

**Step 5: Commit**

---

### Task 2: CLI flag and routing

**Files:**
- Modify: `src/cli/mod.rs` — add `--by-file` and `--top` to StatsOpts
- Modify: `src/main.rs` — route to group_by_file when by_file is set

**Step 1: Add flags to StatsOpts**

```rust
/// Rank files by finding frequency from feedback (hotspot detection)
#[arg(long)]
pub by_file: bool,

/// Limit output rows for --by-file (default: show all)
#[arg(long)]
pub top: Option<usize>,
```

**Step 2: Add routing in main.rs stats handler**

When `opts.by_file`, call `dimensions::group_by_file(&feedback, opts.top)` and format.
Feedback entries are already loaded in compute_report via `feedback_store.load_all()`.

**Step 3: Write CLI parse test**

**Step 4: Commit**

---

### Task 3: Dedicated by-file formatter

**Files:**
- Modify: `src/stats.rs`

**Step 1: Write format_file_hotspots**

Columns: `File | TPs | FPs | Wontfix | Total | Last seen`

Sorted by TP count desc. Use existing glyphs::hbar for TP count visualization.

JSON mode: serialize `Vec<FileHotspotRow>` directly.

Compact mode: one-line per file, pipe-friendly.

**Step 2: Write formatter tests**

- Human output contains expected headers and values
- JSON output roundtrips correctly
- Empty input produces "No file hotspot data" message

**Step 3: Wire into main.rs stats output path**

**Step 4: Commit**

---
