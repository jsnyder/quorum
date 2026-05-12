# In-Diff Classification for Findings

> Issue: #310
> Related: #308 (composite calibrator), #309 (join refactor), #312 (learned weights)

**Goal:** Tag each finding as in-diff or pre-existing when `--diff-file` is active, propagate through calibrator and output, and down-weight out-of-diff verdicts in feedback.

**Architecture:** A post-collection stamp pass classifies findings by range overlap against parsed diff hunks. The classification flows into calibrator trace entries, feedback records, output grouping, and exit code scoping. A 0.7x confidence discount on out-of-diff verdicts mirrors the existing provenance weighting system.

**Tech Stack:** Rust, serde (backward-compat optional fields), existing pipeline infrastructure.

---

## 1. Data Model Changes

### Finding (`src/finding.rs`)

Add `in_diff: Option<bool>` to the `Finding` struct:

- `Some(true)` -- finding's line range overlaps at least one changed hunk
- `Some(false)` -- finding is in unchanged code (pre-existing)
- `None` -- no diff context available (full-file review)

Serde attributes: `#[serde(default, skip_serializing_if = "Option::is_none")]` for backward compatibility with existing JSON output consumers.

### FeedbackEntry (`src/feedback.rs`)

Add `in_diff: Option<bool>` with the same serde attributes. Value is copied from the finding at feedback recording time. Old feedback rows deserialize as `None`.

### CalibratorTraceEntry (`src/calibrator_trace.rs`)

Add `in_diff: Option<bool>` with the same serde attributes. Copied from the finding before `calibrate_core_decision()` runs, so traces show the classification alongside any applied confidence discount.

## 2. Classification Logic

### Core function

A single pure function in `src/pipeline.rs`:

```rust
fn classify_in_diff(findings: &mut [Finding], changed_lines: &[(u32, u32)]) {
    if changed_lines.is_empty() {
        return;
    }
    for finding in findings {
        let overlaps = changed_lines.iter().any(|(start, end)| {
            finding.line_start <= *end && finding.line_end >= *start
        });
        finding.in_diff = Some(overlaps);
    }
}
```

Any overlap between the finding's line range and a changed hunk counts as in-diff (inclusive approach).

### Pipeline placement

Called after findings are collected and before calibration, in the per-file review flow. Only called when `--diff-file` was explicitly provided -- when `changed_lines` falls back to `[(1, total_lines)]` (no diff file), the function is not called and `in_diff` remains `None`.

### Gating

The pipeline already tracks whether `--diff-file` was provided. The stamp pass is gated on that flag, not on the emptiness of `changed_lines`.

## 3. Calibrator Integration

### verdict_weight() (`src/calibrator.rs`)

`in_diff` acts as a multiplicative discount on the existing `provenance * recency` weight:

```rust
const OUT_OF_DIFF_WEIGHT: f64 = 0.7;

let in_diff_factor = match entry.in_diff {
    Some(false) => OUT_OF_DIFF_WEIGHT,
    _ => 1.0,  // in-diff or no diff context
};
// final = provenance_weight * recency_decay * in_diff_factor
```

Stacking examples:
- Human + in-diff: `1.0 * recency * 1.0`
- Human + out-of-diff: `1.0 * recency * 0.7`
- External + out-of-diff: `0.7 * recency * 0.7 = 0.49`
- AutoCalibrate + out-of-diff: `0.5 * recency * 0.7 = 0.35`

The 0.7x constant is defined as `OUT_OF_DIFF_WEIGHT` in `src/calibrator.rs`. Issue #312 may later tune this via learned weights.

### compute_calibrator_model() (`src/calibrate.rs`)

The same `in_diff_factor` applies when building lookup tables (word LOR, family FP rates, language FP rates). Out-of-diff entries contribute 0.7x weight to learned distributions since those verdicts are noisier.

### Exit codes

After calibration, only findings where `in_diff != Some(false)` contribute to severity-based exit codes (0/1/2). Pre-existing findings are informational. `in_diff: None` (full-file review, no diff context) contributes normally.

## 4. Output Formatting

### Human mode (`src/output/mod.rs`)

Findings are split into two groups per file:

```
-- src/main.rs (3 findings) ----------------------------------------

  ! [critical] SQL injection in query builder (line 42)
    ...

  ~ [medium] Missing error handling (line 88)
    ...

  -- Pre-existing (1 finding) ----------------------------------------

  - [low] Unused import (line 3)
    ...
```

- "Pre-existing" header only appears when out-of-diff findings exist
- In-diff findings render first, pre-existing after a visual separator
- No severity downgrade -- grouping does the work
- Summary line: `"3 findings (2 in this change, 1 pre-existing)"`

### Compact mode

Pre-existing findings get a `[pre]` prefix:

```
!|critical|42|SQL injection in query builder
~|medium|88|Missing error handling
[pre] -|low|3|Unused import
```

### JSON mode

`in_diff` field serializes via serde. No structural change to JSON shape.

## 5. Feedback & Stats

### Feedback recording

`in_diff` is captured from the finding being judged at recording time:

- **MCP path:** the `feedback` tool gains an optional `inDiff` field. Callers (Claude Code, dev:start workflow) propagate the finding's `in_diff` value.
- **CLI path:** `quorum feedback` gains optional `--in-diff` / `--no-in-diff` flags. When omitted, `in_diff` is `None`.
- **Programmatic path:** `FeedbackStore::record_human()` and `record_external()` accept `in_diff: Option<bool>`.

For feedback on findings from the current review session, callers should propagate the finding's `in_diff` value. For historical/manual feedback where the diff context is unknown, `in_diff` stays `None`.

Both `record_human()` and `record_external()` in `FeedbackStore` pass through `in_diff`.

### Stats

Existing `--by-file` and `--by-repo` views include an in-diff/out-of-diff breakdown column when any feedback has `in_diff` set. No new `--by-diff-scope` flag in this PR. Precision/action-rate can be sliced by `in_diff` in the rolling window display.

## 6. Files to Modify

| File | Change | Complexity |
|------|--------|-----------|
| `src/finding.rs` | Add `in_diff: Option<bool>` | trivial |
| `src/pipeline.rs` | `classify_in_diff()` + call site | low |
| `src/feedback.rs` | Add `in_diff: Option<bool>` to FeedbackEntry | trivial |
| `src/calibrator_trace.rs` | Add `in_diff: Option<bool>` to CalibratorTraceEntry | trivial |
| `src/calibrator.rs` | `OUT_OF_DIFF_WEIGHT` constant, discount in `verdict_weight()` | low |
| `src/calibrate.rs` | Down-weight out-of-diff in model building | low |
| `src/output/mod.rs` | Grouped human output, `[pre]` compact prefix, summary line | medium |
| `src/main.rs` | Exit code scoping to in-diff findings | low |

## 7. Design Decisions

1. **Any overlap = in-diff.** A finding touching changed code is relevant even if part of its range is pre-existing.
2. **0.7x out-of-diff discount.** Conservative enough to meaningfully down-weight without discarding signal. Matches External provenance weight. Tunable via #312.
3. **No severity downgrade.** Grouping communicates priority without losing information.
4. **Option<bool> everywhere.** Backward compatible, zero-cost for full-file reviews, simple to reason about.
5. **Stamp-after-collection.** One function, zero constructor changes, follows existing pipeline patterns (calibrator actions are also stamped post-collection).
6. **Gate on --diff-file flag, not changed_lines emptiness.** Avoids false `in_diff: true` when full-file fallback ranges are used.
7. **inferust/statsmodels deferred to #312.** No regression fitting needed for a constant multiplier.
