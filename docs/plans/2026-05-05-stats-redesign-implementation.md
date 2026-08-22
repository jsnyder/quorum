# Stats Dashboard Redesign — Phase 0 + A Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `quorum stats` interpretable under biased feedback by adding finding-level identity (Phase 0) and confidence-aware presentation (Phase A).

**Architecture:** Add `finding_id`/`rule_id` to `FeedbackEntry` (forward-only, legacy entries flagged). Compute Wilson 95% CI on per-finding precision. Replace tier-precision table with channel-attribution-only table. Surface capture rate inline with headline trend. Move dimensional drill-downs (By caller, Rolling N) behind a new `--full` flag. Update DESIGN.md to codify table conventions and trend-interpretation rules.

**Tech Stack:** Rust 1.x (existing), `chrono` for time windows (existing), `serde` for schema migration (existing). No new dependencies.

**Design doc:** [`2026-05-05-stats-redesign.md`](./2026-05-05-stats-redesign.md) (consensus-reviewed)

---

## Conventions

- **Testing:** unit tests live alongside the module under `#[cfg(test)] mod tests`. New tests for analytics go in `src/analytics.rs`; stats rendering tests in `src/stats.rs`.
- **Test review:** every test in this plan is **draft** — `testing-antipatterns-expert` reviews them before any are written (Phase 3 of the dev workflow). The agent may add, drop, or rewrite tests; the canonical test list is whatever survives that review.
- **Commits:** one task = one commit. Conventional Commits style (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`).
- **Crucial:** `FeedbackEntry` is persisted JSONL — schema additions MUST use `#[serde(default)]` for backward compatibility.

---

## Phase 0 — Schema + Diagnostic

### Task 1: Add `finding_id` and `rule_id` fields to FeedbackEntry

**Files:**
- Modify: `src/feedback.rs` (around line 93 — the `FeedbackEntry` struct)
- Test: `src/feedback.rs` `#[cfg(test)] mod tests`

**Step 1: Write the failing test**

```rust
#[test]
fn feedback_entry_deserializes_legacy_rows_without_finding_id() {
    let legacy_json = r#"{"file_path":"x.rs","finding_title":"t","finding_category":"security","verdict":"tp","reason":"r","model":"gpt","timestamp":"2026-01-01T00:00:00Z"}"#;
    let entry: FeedbackEntry = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(entry.finding_id, None);
    assert_eq!(entry.rule_id, None);
}

#[test]
fn feedback_entry_roundtrips_finding_id_when_present() {
    let entry = FeedbackEntry {
        // ... existing fields ...
        finding_id: Some("01HXYZ...ULID".to_string()),
        rule_id: Some("python/eval-non-literal".to_string()),
        // ... rest ...
    };
    let json = serde_json::to_string(&entry).unwrap();
    let back: FeedbackEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.finding_id, Some("01HXYZ...ULID".to_string()));
    assert_eq!(back.rule_id, Some("python/eval-non-literal".to_string()));
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --bin quorum -- feedback_entry_deserializes_legacy_rows feedback_entry_roundtrips_finding_id
```
Expected: FAIL with "no field `finding_id`".

**Step 3: Add the fields**

```rust
pub struct FeedbackEntry {
    // ... existing fields ...

    /// Stable identifier for the source finding. Forward-only — None on legacy entries.
    /// Used for per-finding deduplication when computing precision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,

    /// AST rule that produced the finding, if any. Forward-only — None on legacy entries
    /// and on findings from non-rule sources (LLM, linter, custom AST).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test --bin quorum -- feedback_entry_deserializes_legacy_rows feedback_entry_roundtrips_finding_id
```
Expected: PASS.

**Step 5: Commit**

```bash
git add src/feedback.rs
git commit -m "feat(feedback): add finding_id and rule_id fields (forward-only)

Schema migration with #[serde(default)] for backward-compat with legacy
entries. finding_id enables per-finding precision deduplication; rule_id
enables per-AST-rule scoring (Phase C, future)."
```

---

### Task 2: Add Wilson interval helper

**Files:**
- Create: `src/stats_math.rs` (new module)
- Modify: `src/lib.rs` to add `pub mod stats_math;`

**Step 1: Write the failing tests**

```rust
#[test]
fn wilson_interval_with_zero_n_returns_unit_band() {
    let (lo, hi) = wilson_interval(0, 0, 0.95);
    assert_eq!((lo, hi), (0.0, 1.0));
}

#[test]
fn wilson_interval_centers_on_proportion() {
    let (lo, hi) = wilson_interval(50, 100, 0.95);
    let center = (lo + hi) / 2.0;
    assert!((center - 0.5).abs() < 0.02, "center {} not near 0.5", center);
}

#[test]
fn wilson_interval_narrows_with_more_samples() {
    let (lo_small, hi_small) = wilson_interval(15, 30, 0.95);
    let (lo_large, hi_large) = wilson_interval(150, 300, 0.95);
    let small_width = hi_small - lo_small;
    let large_width = hi_large - lo_large;
    assert!(large_width < small_width, "CI should narrow with more data");
}

#[test]
fn wilson_interval_handles_extremes_without_div_by_zero() {
    let (lo, _) = wilson_interval(0, 30, 0.95);
    assert!(lo >= 0.0);
    let (_, hi) = wilson_interval(30, 30, 0.95);
    assert!(hi <= 1.0);
}
```

**Step 2: Run tests** (will fail — module doesn't exist).

**Step 3: Implement**

```rust
// src/stats_math.rs

/// Wilson score interval for a binomial proportion.
///
/// Returns (lower, upper) bounds at the given confidence level.
/// Stable for small n and proportions near 0 or 1, where normal-approximation
/// (Wald) intervals break down.
///
/// Reference: Wilson (1927), "Probable Inference, the Law of Succession,
/// and Statistical Inference."
pub fn wilson_interval(successes: usize, total: usize, confidence: f64) -> (f64, f64) {
    if total == 0 {
        return (0.0, 1.0);
    }
    let n = total as f64;
    let p = successes as f64 / n;
    let z = z_score(confidence);
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let half_width = (z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt()) / denom;
    let lo = (center - half_width).max(0.0);
    let hi = (center + half_width).min(1.0);
    (lo, hi)
}

/// Two-sided z-score for a given confidence level.
/// Hard-coded for the values we actually use (95%, 90%, 99%) — avoids
/// pulling in a stats crate for one function.
fn z_score(confidence: f64) -> f64 {
    match (confidence * 100.0).round() as u32 {
        99 => 2.576,
        95 => 1.96,
        90 => 1.645,
        _ => 1.96, // default to 95% for unrecognized levels
    }
}
```

**Step 4: Run tests** — expect PASS.

**Step 5: Commit**

```bash
git add src/stats_math.rs src/lib.rs
git commit -m "feat(stats): add Wilson 95% CI helper for proportions

Wilson interval is stable for small n and edge proportions, unlike Wald.
Used by per-window precision rendering to surface uncertainty bands."
```

---

### Task 3: Compute linkage rate between reviews.jsonl and feedback.jsonl

**Files:**
- Modify: `src/analytics.rs` (add new function near `precision_trend`)
- Test: same file

**Step 1: Write the failing tests**

```rust
#[test]
fn linkage_rate_zero_when_no_feedback() {
    let reviews = vec![review_record("run1", &["f1"])];
    let feedback: Vec<FeedbackEntry> = vec![];
    let stats = linkage_stats(&reviews, &feedback);
    assert_eq!(stats.linked, 0);
    assert_eq!(stats.unlinked, 0);
    assert_eq!(stats.rate(), 0.0);
}

#[test]
fn linkage_rate_one_when_all_feedback_has_finding_id() {
    let reviews = vec![review_record("run1", &["finding-A"])];
    let feedback = vec![
        feedback_with_finding_id("finding-A"),
    ];
    let stats = linkage_stats(&reviews, &feedback);
    assert_eq!(stats.linked, 1);
    assert_eq!(stats.unlinked, 0);
    assert!((stats.rate() - 1.0).abs() < 1e-9);
}

#[test]
fn linkage_rate_partial_with_legacy_entries() {
    let reviews = vec![review_record("run1", &["finding-A", "finding-B"])];
    let feedback = vec![
        feedback_with_finding_id("finding-A"),
        feedback_legacy("file.rs", "title"),  // no finding_id
    ];
    let stats = linkage_stats(&reviews, &feedback);
    assert_eq!(stats.linked, 1);
    assert_eq!(stats.unlinked, 1);
    assert!((stats.rate() - 0.5).abs() < 1e-9);
}

#[test]
fn linkage_rate_dangling_finding_id_counts_as_unlinked() {
    let reviews = vec![review_record("run1", &["finding-A"])];
    let feedback = vec![
        feedback_with_finding_id("finding-NONEXISTENT"),
    ];
    let stats = linkage_stats(&reviews, &feedback);
    assert_eq!(stats.linked, 0);
    assert_eq!(stats.unlinked, 1);
}
```

**Step 2: Run tests** — fail.

**Step 3: Implement**

```rust
#[derive(Debug, Clone, Default)]
pub struct LinkageStats {
    pub linked: usize,
    pub unlinked: usize,
}

impl LinkageStats {
    pub fn rate(&self) -> f64 {
        let total = self.linked + self.unlinked;
        if total == 0 { 0.0 } else { self.linked as f64 / total as f64 }
    }
}

pub fn linkage_stats(
    reviews: &[ReviewRecord],
    feedback: &[FeedbackEntry],
) -> LinkageStats {
    use std::collections::HashSet;
    let known_finding_ids: HashSet<&str> = reviews.iter()
        .flat_map(|r| r.finding_ids.iter().map(|s| s.as_str()))
        .collect();
    let mut stats = LinkageStats::default();
    for entry in feedback {
        match &entry.finding_id {
            Some(fid) if known_finding_ids.contains(fid.as_str()) => stats.linked += 1,
            _ => stats.unlinked += 1,
        }
    }
    stats
}
```

(May require adding `finding_ids: Vec<String>` to `ReviewRecord` — check what's there now; if absent, this is a separate sub-task gated on review.jsonl format inspection.)

**Step 4: Run tests** — PASS.

**Step 5: Commit**

```bash
git add src/analytics.rs
git commit -m "feat(analytics): linkage_stats for reviews↔feedback join health

Counts linked (feedback.finding_id matches a review's finding_ids) and
unlinked entries. Used by --join-health diagnostic and to gate
per-finding precision."
```

---

### Task 4: Inspect ReviewRecord for finding_ids field

**Pre-implementation check** — not a TDD task. Read `src/review_log.rs` (or wherever `ReviewRecord` is defined) and verify whether `finding_ids: Vec<String>` exists. If not, this is a blocking sub-task: emit finding_ids when writing review.jsonl. Add a sub-task here and commit separately.

If absent:

**Files:** `src/review_log.rs`, `src/main.rs` (where reviews are written)

Add `finding_ids: Vec<String>` (default empty, `#[serde(default)]`). Populate at review-write time from the finding ULIDs.

**Tests:** roundtrip serde test, default-empty test.

**Commit:** `feat(review_log): add finding_ids to ReviewRecord for linkage tracking`

---

### Task 5: Add `quorum stats --join-health` diagnostic flag

**Files:**
- Modify: `src/cli/mod.rs` (or wherever `StatsOpts` lives)
- Modify: `src/main.rs` (`run_stats` function)
- Test: integration test in `tests/cli/stats.rs` (or existing CLI test file)

**Step 1: Write the failing test**

```rust
#[test]
fn join_health_flag_emits_linkage_rate() {
    let temp = tempdir().unwrap();
    write_test_reviews(&temp, /* with known finding_ids */);
    write_test_feedback(&temp, /* with mixed legacy + linked */);

    let output = run_quorum(&["stats", "--join-health"], &temp);

    assert!(output.contains("Linkage:"));
    assert!(output.contains("linked"));
    assert!(output.contains("unlinked"));
}
```

**Step 2: Run** — fail.

**Step 3: Implement**

Add `#[arg(long)] pub join_health: bool` to `StatsOpts`. In `run_stats`, branch on `opts.join_health`: short-circuit, compute `linkage_stats`, print:

```
Linkage health
  Reviews: 138 with 1,159 findings
  Feedback: 415 entries (212 linked, 203 unlinked legacy)
  Linkage rate: 51%   ← below 85% threshold; per-finding precision falls back to entry-level
```

**Step 4: Run test** — PASS.

**Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs tests/...
git commit -m "feat(stats): add --join-health diagnostic flag

Surfaces reviews↔feedback linkage rate. Used to assess whether per-finding
precision math is trustworthy or should fall back to entry-level."
```

---

### Task 6: Extend StatsReport with new fields

**Files:**
- Modify: `src/stats.rs` (`StatsReport` struct)

**Step 1: Test** — extend an existing `compute_report` integration test to assert presence of new fields.

```rust
#[test]
fn stats_report_includes_linkage_rate_and_capture() {
    // ... setup ...
    let report = compute_report(&fb, &tl, &rl).unwrap();
    assert!(report.linkage_rate >= 0.0 && report.linkage_rate <= 1.0);
    assert!(report.capture_rate >= 0.0 && report.capture_rate <= 1.0);
    assert!(report.headline_trend_uses_finding_id || !report.headline_trend_uses_finding_id);  // either is valid
}
```

**Step 2-4: Implement and pass.**

Add to `StatsReport`:
```rust
pub linkage_rate: f64,
pub linkage_linked: usize,
pub linkage_unlinked: usize,
pub capture_rate: f64,           // labeled / total findings in 7d
pub capture_labeled: usize,
pub capture_total: usize,
pub headline_trend_uses_finding_id: bool,  // false during rollout
pub external_overlap: ExternalOverlap,     // new struct, see Task 9
```

Wire `compute_report` to populate them.

**Step 5: Commit:** `refactor(stats): extend StatsReport with linkage and capture metadata`

---

## Phase A — Rendering

### Task 7: Per-finding deduplication

**Files:**
- Modify: `src/analytics.rs` — new `precision_trend_per_finding` function

**Step 1: Tests**

```rust
#[test]
fn per_finding_dedup_collapses_human_plus_postfix_on_same_finding() {
    let entries = vec![
        feedback("finding-A", Verdict::Tp, Provenance::Human),
        feedback("finding-A", Verdict::Tp, Provenance::PostFix),  // same finding
    ];
    let trend = precision_trend_per_finding(&entries, 7);
    // Should count finding-A once, not twice
    assert_eq!(trend[0].count, 1);
}

#[test]
fn per_finding_human_verdict_takes_precedence_over_postfix() {
    let entries = vec![
        feedback("finding-A", Verdict::Fp, Provenance::Human),
        feedback("finding-A", Verdict::Tp, Provenance::PostFix),
    ];
    let trend = precision_trend_per_finding(&entries, 7);
    // Human FP wins; not a TP from PostFix
    assert!((trend[0].precision - 0.0).abs() < 1e-9);
}

#[test]
fn per_finding_excludes_external_and_autocalib() {
    let entries = vec![
        feedback("finding-A", Verdict::Tp, Provenance::Human),
        feedback("finding-B", Verdict::Tp, Provenance::External { /* ... */ }),
        feedback("finding-C", Verdict::Tp, Provenance::AutoCalibrate(/* ... */)),
    ];
    let trend = precision_trend_per_finding(&entries, 7);
    assert_eq!(trend[0].count, 1);  // only Human counted
}

#[test]
fn per_finding_skips_legacy_entries_without_finding_id() {
    let entries = vec![
        feedback_legacy("file.rs", "title"),  // no finding_id
        feedback("finding-A", Verdict::Tp, Provenance::Human),
    ];
    let trend = precision_trend_per_finding(&entries, 7);
    assert_eq!(trend[0].count, 1);
}
```

**Step 3: Implement** — group by `finding_id`, apply precedence (Human > PostFix > drop), compute precision per window.

**Step 5: Commit:** `feat(analytics): per-finding precision trend with disposition precedence`

---

### Task 8: Replace `format_tier_report` with `format_channel_attribution`

**Files:**
- Modify: `src/analytics.rs` — new fn replaces `format_tier_report` (keep old for one release as `#[deprecated]`)
- Test: same file

**Step 1: Tests**

```rust
#[test]
fn channel_attribution_omits_precision_columns_for_external_and_autocalib() {
    let summary = TierSummary { /* mixed */ };
    let out = format_channel_attribution(&summary);
    assert!(!out.contains("100% prec"), "PostFix prec column should be absent");
    // Header row check
    assert!(out.contains("Total"));
    assert!(out.contains("TP"));
    assert!(out.contains("FP"));
}

#[test]
fn channel_attribution_renders_em_dash_for_zero_cells() {
    let summary = TierSummary { /* PostFix has 45 TP and 0 FP */ };
    let out = format_channel_attribution(&summary);
    // PostFix row should show "—" not "0" for FP/Part/Wfix
    let postfix_line = out.lines().find(|l| l.contains("PostFix")).unwrap();
    assert!(postfix_line.contains("—"));
}

#[test]
fn channel_attribution_uses_thin_dim_rule_under_header_only() {
    let summary = TierSummary::default();
    let out = format_channel_attribution(&summary);
    let rule_count = out.matches("──").count();
    assert!(rule_count >= 1 && rule_count <= 6, "expected one ─ row, got {rule_count}");
}
```

**Step 3: Implement** with right-aligned numeric columns, em-dash for zeros, single dim `─` rule.

**Step 5: Commit:** `feat(analytics): channel attribution table replaces tier-precision`

---

### Task 9: External corpus block

**Files:**
- Modify: `src/analytics.rs` — new `ExternalOverlap` struct + `compute_external_overlap`
- Modify: `src/stats.rs` — `format_external_corpus`

**Step 1: Tests**

```rust
#[test]
fn external_overlap_computes_agreement_rate_per_agent() {
    // External agent "pal" verdicts on findings that quorum also flagged
    let entries = vec![
        external_feedback("finding-A", Verdict::Tp, "pal"),  // agreement (quorum also TP)
        external_feedback("finding-B", Verdict::Fp, "pal"),  // disagreement
    ];
    let quorum_verdicts: HashMap<&str, Verdict> = [
        ("finding-A", Verdict::Tp),
        ("finding-B", Verdict::Tp),
    ].into_iter().collect();
    let overlap = compute_external_overlap(&entries, &quorum_verdicts);
    let pal = overlap.per_agent.get("pal").unwrap();
    assert_eq!(pal.findings, 2);
    assert!((pal.agreement_rate - 0.5).abs() < 1e-9);
}
```

**Step 3: Implement** plus rendering function.

**Step 5: Commit:** `feat(stats): external corpus block with agreement rates`

---

### Task 10: Headline trend rendering

**Files:**
- Modify: `src/stats.rs` — new `format_headline_trend`

**Step 1: Tests**

```rust
#[test]
fn headline_trend_shows_per_window_pct_with_ci_on_current() {
    let report = StatsReport {
        precision_trend: vec![
            window(0.77, 30), window(0.81, 32), /* ... */ window(0.76, 145),
        ],
        capture_rate: 0.18,
        capture_labeled: 212,
        capture_total: 1159,
        headline_trend_uses_finding_id: true,
        // ...
    };
    let out = format_headline_trend(&report, &Style::default());
    assert!(out.contains("77 → 81"));
    assert!(out.contains("76%"));
    assert!(out.contains("[71-81]") || out.contains("[71"));  // CI band
    assert!(out.contains("n=145"));
    assert!(out.contains("capture: 18%"));
}

#[test]
fn headline_trend_replaces_low_n_window_with_n_too_low() {
    let report = StatsReport {
        precision_trend: vec![
            window(0.77, 8),   // below 30
            window(0.81, 32),
        ],
        // ...
    };
    let out = format_headline_trend(&report, &Style::default());
    assert!(out.contains("n<30") || out.contains("—"));  // marker
}

#[test]
fn headline_trend_shows_legacy_banner_when_finding_id_unused() {
    let report = StatsReport {
        headline_trend_uses_finding_id: false,
        // ...
    };
    let out = format_headline_trend(&report, &Style::default());
    assert!(out.contains("entry-level pending finding-id rollout"));
}
```

**Step 3: Implement** matching design mockup option (c).

**Step 5: Commit:** `feat(stats): headline precision trend with Wilson CI and capture inline`

---

### Task 11: Section label normalization

**Files:**
- Modify: `src/stats.rs` — search and replace `(7d)` → `(last 7 days)` in headers; `Rolling 50 reviews` → `Rolling windows (50 reviews each)`.

**Step 1: Test** — snapshot test on `format_human` output asserting the new strings.

**Step 3:** mechanical edit.

**Step 5: Commit:** `refactor(stats): normalize section time-window labels`

---

### Task 12: --full flag and dimensional view trim

**Files:**
- Modify: `src/cli/mod.rs` — add `#[arg(long)] pub full: bool` to StatsOpts
- Modify: `src/stats.rs` — gate "By caller" and "Rolling N reviews" blocks on `opts.full`. By repo stays in default.

**Step 1: Tests**

```rust
#[test]
fn default_stats_omits_by_caller_and_rolling_windows() {
    let report = report_with_dimensions();
    let out = format_human(&report, &Style::default(), /* full = */ false);
    assert!(!out.contains("By caller"));
    assert!(!out.contains("Rolling"));
    assert!(out.contains("By repo"));  // still shown
}

#[test]
fn full_stats_shows_all_dimensions() {
    let report = report_with_dimensions();
    let out = format_human(&report, &Style::default(), /* full = */ true);
    assert!(out.contains("By caller"));
    assert!(out.contains("Rolling"));
    assert!(out.contains("By repo"));
}
```

**Step 3: Implement**, threading `full: bool` from CLI to `format_human`.

**Step 5: Commit:** `feat(stats): --full flag for diagnostic dimensional views`

---

### Task 13: DESIGN.md updates

**Files:**
- Modify: `DESIGN.md` — append §4.x and §12.x

**Step 1: Test** — n/a (docs).

**Step 3: Add sections:**

```markdown
### 4.x Tables

Use a single dim `─` rule beneath the column header row only. Never above,
beside, or below data rows. No box characters, no vertical separators.
Numeric columns right-align to the value, not the header. Empty cells render as `—`.

### 12.x Trend interpretation

Trends label **scope** (what's rolled in) and **unit** (`7d windows × N` or
`50 reviews × N`) explicitly. Headline trend includes a 95% Wilson confidence
interval on the most recent window when n≥30, and is replaced with `n<30` otherwise.
Capture rate (labeled findings / total findings) is shown inline so trend footing
is visible.
```

**Step 5: Commit:** `docs: codify table conventions and trend-interpretation rules`

---

### Task 14: Wire it together — final `format_human` integration

**Files:**
- Modify: `src/stats.rs` — `format_human_core` to call new functions in correct order:
  1. Feedback Health header
  2. Channel attribution table (replaces tier table)
  3. Headline trend
  4. Activity (with capture line if Phase B were enabled — but we're not, so skip for now)
  5. Spend
  6. External corpus block
  7. By repo (always)
  8. By caller, Rolling — only if `--full`

**Step 1: Snapshot test** asserting full default output structure.

**Step 3:** integrate.

**Step 5: Commit:** `refactor(stats): assemble new dashboard layout`

---

## Verification gates

After every task: `cargo test --bin quorum` must pass. After Task 14: full verification battery:

```bash
cargo test --bin quorum
cargo test  # includes CLI integration tests
cargo clippy -- -D warnings
cargo build --release
cargo run -- stats              # smoke test, default
cargo run -- stats --full       # smoke test, full
cargo run -- stats --join-health  # smoke test, diagnostic
```

Each smoke test should be inspected by eye against the mockup.

## Out of scope (deferred)

- Phase B (capture metric as headline; bias mix annotation) — gated on Phase 0 linkage audit ≥85%.
- Phase C (rule attribution dashboard, `quorum rules stats` command) — gated on rule_id sample accumulation ≥10 per rule for ≥5 rules.
- Two-trend split (one trend is enough; channel attribution gives composition).
- Inverse-propensity reweighting.
- Bootstrap CI (Wilson is the consensus pick).
- Stratified-by-severity headline.
- Hidden low-capture windows (we dim instead).
- Weighted External-blended precision.

## Open implementation decisions

(Surface during execution, ask before finalizing.)

1. **Backfill `finding_id` for existing reviews.jsonl entries via ULID emission?** — depends on whether reviews.jsonl already has stable per-finding ULIDs we can reuse.
2. **Migration of `format_tier_report` callers** — is it called anywhere besides `format_human_core`?
3. **Compact mode (`format_compact`)** — does it need updating to reflect new fields, or stays as-is for LLM consumption?
