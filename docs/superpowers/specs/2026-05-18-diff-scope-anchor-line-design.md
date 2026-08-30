# Diff-Scope Anchor-Line Fix

**Date:** 2026-05-18
**Issue:** #383
**Branch:** `fix/diff-scope-leak`

## Problem

`classify_in_diff()` in `pipeline.rs:1483-1494` uses inclusive span overlap: any finding whose `[line_start, line_end]` overlaps any changed hunk gets `in_diff: true`. This causes 46 FPs where pre-existing findings (especially function-level ones like complexity) are marked as in-diff because the function was partially edited.

## Root Cause

A function-level finding spanning lines 1-500 with a 10-line change at line 250 gets `in_diff: true` because `finding.line_start(1) <= hunk_end(260) && finding.line_end(500) >= hunk_start(250)` is true.

## Fix: Anchor-Line Matching

Replace span overlap with anchor-line matching:

1. Determine the finding's **anchor line**: `cited_lines[0]` if available, else `line_start`
2. Mark `in_diff: true` only if the anchor line falls within a changed hunk
3. This means wide-span findings (complexity, whole-function patterns) are only in-diff if their specific flagged line is in a changed region

### Algorithm

```rust
fn classify_in_diff(findings: &mut [Finding], changed_lines: &[(u32, u32)]) {
    for finding in findings {
        if !finding.is_valid() { continue; }
        let anchor = finding.anchor_line();
        let in_changed = !changed_lines.is_empty()
            && changed_lines.iter().any(|(start, end)| anchor >= *start && anchor <= *end);
        finding.in_diff = Some(in_changed);
    }
}
```

Where `anchor_line()` returns `cited_lines[0]` if non-empty, else `line_start`.

## Backward Compatibility

- `in_diff` field type unchanged (`Option<bool>`)
- Calibrator weighting unchanged (0.7x for out-of-diff)
- Only behavior change: fewer findings marked in-diff (strictly tighter)

## Non-Goals

- No new DiffRelevance enum (keep it simple)
- No changes to hydration or diff parsing
- No changes to calibrator weighting logic
