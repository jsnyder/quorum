# Reviews Log & Stats Dimensions Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a per-review append-only log (`~/.quorum/reviews.jsonl`) and extend `quorum stats` with dimensional views (by-repo, by-caller, rolling windows) so users can answer: *"Is code quality trending over time? Does this agent produce systematically different findings than that one?"*

**Scope:** Review-level telemetry + aggregation views. Does NOT include git-walk-based metrics (revert rate, ghost towns), PR/merge metrics, or structural-duplication detection — those are tracked as separate issues.

**Architecture:** Bottom-up — schema first, writer second, aggregators third, UI last. Each task independently testable. No task depends on an unimplemented task above it.

**Tech Stack:** Rust, serde_json, chrono, clap. Reuses existing `Style`, `format_count`, and compact-mode detection from v0.10.0 telemetry work.

**Design alignment:** Follows DESIGN.md §2 (three modes), §3 (color restraint), §4 (label layout), §11 (numeric formatting). Compact mode stays under 100 tokens for the stats dashboard.

**Anti-pattern guide:** See `docs/TDD_ANTIPATTERN_GUIDE.md`.

---

## Design Principles (captured from frontier-model consensus, 2026-04-19)

Guardrails that shape every metric below:

1. **Normalize by volume, not run count.** A 10-line review and a 1000-line review are not comparable.
2. **Repo attribution is as important as caller attribution.** Without it, cross-agent comparisons are confounded by which codebase each agent touches.
3. **Rolling N-run windows beat fixed weekly buckets** for a solo developer. Weekly buckets produce 0%/100% whiplash from sparse data.
4. **Sample-size gates on every slice.** Suppress or flag any bucket with `n < MIN_SAMPLE` (default 5).
5. **Precision is misleading.** We cannot measure recall. Output labels it "observed acceptance rate" (or "accept rate" in compact mode).
6. **Severity drift ≠ quality drift.** Severity-mix trends mix policy/taxonomy drift with real change. Surface with caveat.

---

## Task 1: `ReviewRecord` schema + writer

**Why first:** Data foundation. Aggregators are useless without a record. Pure struct + append logic, cheap to test.

**Files:**
- Create: `src/review_log.rs`
- Modify: `src/main.rs` (add `mod review_log;`)
- Modify: `src/pipeline.rs` (call writer at end of review)

**Schema (`ReviewRecord`):**
```rust
pub struct ReviewRecord {
    pub run_id: String,                    // ULID — unique per invocation, enables exact joins to feedback
    pub timestamp: DateTime<Utc>,
    pub quorum_version: String,
    pub repo: Option<String>,              // normalized git root basename
    pub invoked_from: String,              // "claude_code" | "codex_ci" | "gemini_cli" | "agent" | "tty" | "pipe" | <--caller>
    pub model: String,                     // QUORUM_MODEL value at invocation
    pub files_reviewed: u32,
    pub lines_added: Option<u32>,          // from --diff-file if present
    pub lines_removed: Option<u32>,
    pub findings_by_severity: SeverityCounts,  // {critical, warning, info}
    pub suppressed_by_rule: HashMap<String, u32>,  // rule_id -> count
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tokens_cache_read: u64,            // prompt-cache hits; enables cache_hit_rate metric
    pub duration_ms: u64,
    pub flags: Flags,                      // {deep, parallel_n, ensemble}
}
```

**Notably NOT included:** `cost_usd`. Model pricing drifts and depends on caching tiers; storing USD at invocation time locks in inaccurate historical data. Cost is computed dynamically at display time from `tokens_in`/`tokens_out`/`tokens_cache_read` and the current pricing table.

**`run_id` rationale:** enables exact joins between `reviews.jsonl` and `feedback.jsonl` / `calibrator_traces.jsonl`. Retrofitting is impossible once logs grow, so it ships from day one. Use `ulid` crate (time-sortable, 26 chars, no deps beyond rand).

**Detection logic** (reuse `compact-mode` env sniffing):
- `CLAUDE_CODE` → `"claude_code"`
- `CODEX_CI` → `"codex_ci"`
- `GEMINI_CLI` → `"gemini_cli"`
- `AGENT` → value of `AGENT`
- `--caller <name>` flag → override wins
- else TTY detection → `"tty"` or `"pipe"`

**Repo detection:** walk parents of first reviewed file for `.git`; use basename. `None` if not in a git repo.

**Writer contract:** append JSON line to `~/.quorum/reviews.jsonl`. Atomic enough for solo use (no locking — append is single `write!`). Failures logged to stderr, never crash the review.

**Tests:**
- Serialize/deserialize round-trip preserves all fields
- Writer creates file if missing, appends otherwise
- Env-var detection table (CLAUDE_CODE, CODEX_CI, GEMINI_CLI, AGENT, --caller override)
- Repo detection finds basename when inside git repo, returns None otherwise

---

## Task 2: `--caller` CLI flag + pipeline wiring

**Files:**
- Modify: `src/cli.rs` — add `--caller <NAME>` to review subcommand
- Modify: `src/pipeline.rs` — construct `ReviewRecord` at end of review

**Writer site:** single call after findings are finalized, before output rendering. Duration = wall clock from review start.

**Tests:**
- Integration test: run `quorum review <file>` and assert `reviews.jsonl` gains one line with expected shape
- `--caller my-script` overrides env detection

---

## Task 3a: Streaming `ReviewLog` reader

**Why separate:** aggregation assumes records can be loaded cheaply. A JSONL file in CI could reach hundreds of MB; loading whole file into memory is wrong.

**Files:**
- Modify: `src/review_log.rs` — add `ReviewLog::iter()` returning `impl Iterator<Item = Result<ReviewRecord>>`, using `BufReader` + line-based streaming.

**Tests:**
- Iterator yields records in insertion order
- Malformed line is logged + skipped, iteration continues (parity with `FeedbackStore` behavior)
- 10k-record file iterated without loading all into memory (smoke-test with a large generated fixture)

---

## Task 3b: Aggregation core

**Files:**
- Create: `src/stats/dimensions.rs`

**Structures:**
```rust
pub struct DimensionSlice {
    pub key: String,                       // repo name, caller name, or rolling window label
    pub n_reviews: u32,
    pub n_findings: u32,
    pub findings_per_file: f64,
    pub findings_per_kloc: Option<f64>,    // None if no diff data
    pub accept_rate: Option<f64>,          // observed acceptance rate from feedback join
    pub severity_mix: SeverityCounts,
    pub suppression_rate: f64,
    pub avg_duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tokens_cache_read: u64,
    pub cache_hit_rate: f64,               // tokens_cache_read / tokens_in
    pub sparkline_points: Vec<f64>,        // findings_per_file per chronological sub-bucket; empty if low_sample
    pub low_sample: bool,                  // n_reviews < MIN_SAMPLE
}

pub const MIN_SAMPLE: u32 = 5;

pub fn group_by_repo(records: &[ReviewRecord], feedback: &FeedbackStore) -> Vec<DimensionSlice>;
pub fn group_by_caller(records: &[ReviewRecord], feedback: &FeedbackStore) -> Vec<DimensionSlice>;
pub fn rolling_window(records: &[ReviewRecord], n: usize) -> Vec<DimensionSlice>;  // last N, last 2N-to-N, ...
```

**Feedback join strategy (two-phase):**
1. **Preferred:** exact match on `run_id` field. Requires `FeedbackRecord` to persist `run_id` (small, non-breaking additive change — separate sub-task under Task 2).
2. **Fallback (for pre-run_id feedback entries):** match on `(file, finding_title)` within 7d of review timestamp. Marked as "approximate" in trace output.

Feedback matched approximately is counted at 0.5× weight in accept_rate, so imprecise matching degrades gracefully. Report explicitly labels metric "observed acceptance rate" (DESIGN.md already constrains us away from calling this precision).

**Tests:**
- Empty input → empty output (no panics)
- Single-repo corpus → single slice, low_sample=true until n >= 5
- Rolling window of 3 over 10 records yields sliding slices
- Exact `run_id` match counts at full weight; fallback match at 0.5×
- `sparkline_points` empty when `low_sample`

---

## Task 4: `--by-repo`, `--by-caller`, `--rolling N` flags

**Files:**
- Modify: `src/cli.rs` — extend stats subcommand
- Modify: `src/stats.rs` — dispatch to dimensional formatters

**CLI shape:**
```
quorum stats --by-repo
quorum stats --by-caller
quorum stats --rolling 50        # last 50 reviews as one window
quorum stats --by-repo --rolling 50  # combines
```

Mutually compatible where sensible. When both `--by-X` and `--rolling N` are set, rolling window applies first, then grouping.

---

## Task 5: Human UI — dimensional tables with inline semigraphics

**Files:**
- Create: `src/stats/glyphs.rs` — tiny hand-rolled renderer. **Not using `textplots`** (pulls deps; braille rendering causes line-height stretch in some terminals; violates the minimalist aesthetic in DESIGN.md §1).
- Modify: `src/stats.rs` — add `format_dimension_table`

**Glyph primitives (all gated on `unicode_ok()`):**

```rust
/// Horizontal bar: 10 cells, filled proportionally. Filled cells default color, unfilled cells dim.
/// Fallback when !unicode_ok(): returns "##########" / "..........". No color change.
pub fn hbar(value: f64, max: f64, style: &Style) -> String; // e.g. "█████·····"

/// Sparkline from U+2581–U+2588 block levels. Fallback: `+/-/=` segments.
pub fn sparkline(points: &[f64]) -> String; // e.g. "▃▅▇▆▄"

/// Trend arrow from first vs last sparkline point. Fallback: `↑`→`+`, `↓`→`-`, `→`→`=`.
pub fn trend_arrow(points: &[f64]) -> &'static str;
```

Rules enforced in one place:
- Bars never render when `low_sample` — just show the numeric value
- Filled glyphs are default color, unfilled/padding glyphs are `style.dim`
- Color is green only when metric encodes "good" state (accept_rate ≥ 70%), red for "bad" (accept_rate < 40%), else default. This *does* encode meaning, so it's permitted by DESIGN.md §3.

**By-repo layout:**
```
~ Stats: by repo (last 30d)

  Repo              Reviews   Findings/file   Accept rate          Suppressed        Cost
  quorum               142              2.3   ███████···   78%    █·········    6%   $1.84
  homeassist            89              4.1   ███████···   71%    ██········   12%   $0.93
  memory-bench          31              1.8   —            —      —             3%   $0.22   (low sample)

  4 repos  274 reviews  $3.07 total est.
```

- Filled `█` = default color, padding `·` = dim (per DESIGN.md §3 restraint)
- `—` for unavailable metrics (no feedback matches, no diff data)
- `(low sample)` dim-colored tag on `n_reviews < MIN_SAMPLE` rows
- Totals line dim-colored
- Header row bold, values default
- Repo names truncated to 16 chars with `.` suffix if longer (DESIGN.md §8)
- Cost is derived at display time from tokens, not persisted (see schema notes)

**By-caller variant:** same layout, caller replaces repo column. Shows `claude_code`, `codex_ci`, `gemini_cli`, `tty` (direct invocation). Adds a `Cache hit` column (uses `tokens_cache_read / tokens_in` — directly actionable for cost tuning):
```
  Caller          Reviews   Findings/file   Accept rate          Cache hit         Cost
  claude_code         201              2.6   ████████··   76%    ██████····  62%   $2.14
  codex_ci             43              3.8   █████████·   81%    ██········  18%   $0.71
  tty                  30              1.9   ███████···   72%    ████······  41%   $0.22
```

**Rolling window variant** — sparklines replace the numeric trend column entirely:
```
~ Stats: rolling 50-review windows

  Window        Reviews   Findings/file   Accept rate          Trend
  last 50            50              2.1   ████████··   79%    ▃▅▇▆▄  ↑ improving
  prev 50            50              2.4   ███████···   74%    ▂▃▄▅▇  ↓ degrading
  prev 100          100              2.8   ██████····   68%    ██▇▆▅  ↑ improving
```

- Sparkline = per-window findings/file sub-buckets (default 5 sub-buckets per window)
- Trend text (`improving` / `degrading` / `flat`) suppressed when any window is `low_sample`
- Trend direction color: `improving` green, `degrading` yellow, `flat` dim

**Fallback chain when `!unicode_ok()`:**
- `█` → `#`, `·` → `.` (bars stay 10 chars wide)
- Sparkline → `-\_-\_/` style using `.`/`_`/`-`/`='` levels (5 levels, degraded legibility but readable)
- Arrows → `+`, `-`, `=`

---

## Task 6: Compact UI — one-line dimensional dashboard

**DESIGN.md §2 budget: < 100 tokens for stats. NO semigraphics in compact mode — "zero chrome tokens" per DESIGN.md §2.**

```
by-repo: quorum(n142 fpf2.3 acc78 $1.84) homeassist(n89 fpf4.1 acc71 $0.93) +2 low-sample
```

```
by-caller: claude_code(n201 fpf2.6 acc76 cache62) codex_ci(n43 fpf3.8 acc81 cache18) tty(n30 fpf1.9 acc72 cache41)
```

```
rolling: last50 fpf2.1 acc79 trend=improving | prev50 fpf2.4 acc74 trend=degrading
```

Abbreviations:
- `fpf` = findings per file
- `acc` = accept rate (percent, integer)
- `cache` = cache hit rate (percent, integer)
- `n` = review count
- No severity spelled out (DESIGN.md abbreviation table covers this)
- No bars, no sparklines — LLMs don't benefit from glyphs

---

## Task 7: JSON UI

Flat array of `DimensionSlice` records plus a `meta` object. **No glyphs in JSON.** `sparkline_points` is emitted as raw floats for downstream consumers to render their own visualization.

```json
{
  "mode": "by-repo",
  "slices": [
    {
      "key": "quorum",
      "n_reviews": 142,
      "findings_per_file": 2.3,
      "accept_rate": 0.78,
      "sparkline_points": [2.6, 2.4, 2.1, 2.2, 2.1],
      "cache_hit_rate": 0.62,
      "low_sample": false,
      ...
    }
  ],
  "meta": {"min_sample": 5, "total_reviews": 274, "window_days": 30}
}
```

Stable schema — downstream scripts can diff across releases.

---

## Task 8: Docs

**Files:**
- Modify: `docs/ARCHITECTURE.md` — new "Reviews Log" section under telemetry
- Modify: `CLAUDE.md` — add `reviews.jsonl` location + field summary
- Modify: `DESIGN.md` — add dimensional-table layout to §5
- Update: `memory/MEMORY.md` — note v0.13.0 shape after implementation

---

## Out of scope (tracked as issues)

These were proposed during brainstorming but require data sources beyond JSONL logs, or need their own design passes. Filed as GitHub issues:

- **AI code churn (14-day revert rate)** — needs a git-walker subsystem to match review records against subsequent reverts/rewrites. Separate plan.
- **Time-to-merge / fix-rate velocity** — requires PR/merge metadata; no local source.
- **Structural duplicate/clone ratios** — needs tree-sitter similarity analysis; separate subsystem.
- **Ghost town alerts** — needs `git log --follow` per file; decide whether to whitelist dormant files.
- **Hotspot files view** — straightforward after reviews.jsonl exists, but lives in `--by-file` which is its own view.
- **Acceptance rate by category / rule** — straightforward after task 3 lands; add as `--by-category`.
- **Nit-to-value ratio** — derived metric, needs clear definition of what counts as "value."
- **Context-token vs diff-token split** — requires prompt instrumentation; small design pass.
- **`reviews.jsonl` log rotation** — append-only log will grow unbounded in heavy-CI use. Streaming reader (Task 3a) handles large files, but eventually we need monthly rotation or compaction to a summary file.

---

## Verification

- `cargo test --bin quorum` passes with >= 20 new unit tests across the 8 tasks
- Manual: run 10+ reviews across 2+ repos with different env vars set; confirm all dimensions populate
- `quorum stats --by-repo --json | jq` produces stable, parseable output
- Compact mode under 100 tokens for each dimensional dashboard
- `NO_COLOR=1 quorum stats --by-repo` emits no ANSI codes
