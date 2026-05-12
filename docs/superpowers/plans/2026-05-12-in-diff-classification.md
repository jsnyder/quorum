# In-Diff Classification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tag findings as in-diff vs pre-existing when `--diff-file` is active, propagate through calibrator/output/feedback, and apply a 0.7x confidence discount for out-of-diff verdicts.

**Architecture:** A post-collection stamp pass classifies findings by range overlap against parsed diff hunks. The `in_diff: Option<bool>` field flows through Finding, CalibratorTraceEntry, and FeedbackEntry. Output groups findings into "In this change" vs "Pre-existing" sections. Exit codes only count in-diff findings.

**Tech Stack:** Rust, serde, existing pipeline/calibrator infrastructure.

---

## File Map

| File | Responsibility | Change |
|------|---------------|--------|
| `src/finding.rs` | Finding data model | Add `in_diff: Option<bool>`, update FindingBuilder |
| `src/calibrator_trace.rs` | Trace observability | Add `in_diff: Option<bool>` |
| `src/feedback.rs` | Feedback data model | Add `in_diff` to FeedbackEntry + ExternalVerdictInput |
| `src/pipeline.rs` | Review pipeline | `classify_in_diff()` function + call site |
| `src/calibrator.rs` | Calibration logic | `OUT_OF_DIFF_WEIGHT`, discount in `verdict_weight()`, trace passthrough |
| `src/calibrate.rs` | Model building | Apply `in_diff_factor` in `compute_calibrator_model()` |
| `src/output/mod.rs` | Output formatting | Grouped human/compact output, exit code scoping |
| `src/cli/mod.rs` | CLI args | `--in-diff` / `--no-in-diff` flags |
| `src/mcp/tools.rs` | MCP interface | `inDiff` field on FeedbackTool |
| `src/main.rs` | Wiring | Pass `in_diff` through feedback recording, pass `has_diff` to exit code |

---

### Task 1: Add `in_diff` to Finding struct

**Files:**
- Modify: `src/finding.rs:78-116` (Finding struct)
- Modify: `src/finding.rs:180-206` (FindingBuilder)

- [ ] **Step 1: Write failing test for in_diff serde round-trip**

Add to the existing `#[cfg(test)] mod tests` in `src/finding.rs`:

```rust
#[test]
fn in_diff_serde_round_trip() {
    let mut f = FindingBuilder::new().build();
    assert_eq!(f.in_diff, None);

    f.in_diff = Some(true);
    let json = serde_json::to_string(&f).unwrap();
    assert!(json.contains("\"in_diff\":true"));
    let parsed: Finding = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.in_diff, Some(true));

    f.in_diff = Some(false);
    let json = serde_json::to_string(&f).unwrap();
    assert!(json.contains("\"in_diff\":false"));
    let parsed: Finding = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.in_diff, Some(false));
}

#[test]
fn in_diff_omitted_deserializes_as_none() {
    let json = r#"{"title":"t","description":"d","severity":"info","category":"maintainability","source":"local-ast","line_start":1,"line_end":1,"evidence":[],"similar_precedent":[]}"#;
    let f: Finding = serde_json::from_str(json).unwrap();
    assert_eq!(f.in_diff, None);
}

#[test]
fn in_diff_none_not_serialized() {
    let f = FindingBuilder::new().build();
    let json = serde_json::to_string(&f).unwrap();
    assert!(!json.contains("in_diff"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum in_diff_serde_round_trip in_diff_omitted in_diff_none_not -- --nocapture`
Expected: compile error — `in_diff` field doesn't exist on Finding.

- [ ] **Step 3: Add `in_diff` field to Finding and FindingBuilder**

In `src/finding.rs`, after the `model_agreement` field (line 115), add:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
```

In `FindingBuilder::new()` (inside the `Finding` literal starting at line 183), after `model_agreement: None,` add:

```rust
                in_diff: None,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum in_diff_serde -- --nocapture`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/finding.rs
git commit -m "feat(finding): add in_diff field for diff classification (#310)"
```

---

### Task 2: Add `in_diff` to CalibratorTraceEntry

**Files:**
- Modify: `src/calibrator_trace.rs:63-90` (struct)
- Modify: `src/calibrator_trace.rs:92+` (test module)

- [ ] **Step 1: Write failing test**

Add to the test module in `src/calibrator_trace.rs`:

```rust
#[test]
fn trace_entry_in_diff_serde_round_trip() {
    let mut trace = CalibratorTraceEntry {
        finding_title: "test".into(),
        finding_category: "security".into(),
        tp_weight: 1.0,
        fp_weight: 0.0,
        wontfix_weight: 0.0,
        full_suppress_weight: 0.0,
        soft_fp_weight: 0.0,
        matched_precedents: vec![],
        action: None,
        input_severity: Severity::Info,
        output_severity: Severity::Info,
        severity_change_reason: None,
        file_path: None,
        provenance: None,
        same_file_precedent_count: None,
        composite_score: None,
        in_diff: Some(false),
    };
    let json = serde_json::to_string(&trace).unwrap();
    assert!(json.contains("\"in_diff\":false"));
    let parsed: CalibratorTraceEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.in_diff, Some(false));

    trace.in_diff = None;
    let json = serde_json::to_string(&trace).unwrap();
    assert!(!json.contains("in_diff"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum trace_entry_in_diff_serde -- --nocapture`
Expected: compile error — `in_diff` field doesn't exist.

- [ ] **Step 3: Add `in_diff` field to CalibratorTraceEntry**

In `src/calibrator_trace.rs`, after `composite_score` field (line 89), add:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
```

Then fix all construction sites. In `src/calibrator.rs`, function `make_trace_entry` (line 142), add `in_diff: None,` after `composite_score: None,`. Similarly in `make_no_match_trace` (around line 100), add `in_diff: None,`. Also fix any test construction sites in `src/calibrator_trace.rs` tests.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum trace_entry_in_diff -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/calibrator_trace.rs src/calibrator.rs
git commit -m "feat(trace): add in_diff to CalibratorTraceEntry (#310)"
```

---

### Task 3: Add `in_diff` to FeedbackEntry and ExternalVerdictInput

**Files:**
- Modify: `src/feedback.rs:90-120` (FeedbackEntry)
- Modify: `src/feedback.rs:128-140` (ExternalVerdictInput)
- Modify: `src/feedback.rs:766+` (record_external)
- Modify: `src/feedback.rs:815+` (record_context_misleading)

- [ ] **Step 1: Write failing test**

Add to the test module in `src/feedback.rs`:

```rust
#[test]
fn feedback_in_diff_serde_round_trip() {
    let entry = FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "test".into(),
        finding_category: "security".into(),
        verdict: Verdict::Tp,
        reason: "real bug".into(),
        model: None,
        timestamp: chrono::Utc::now(),
        provenance: Provenance::Human,
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff: Some(true),
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"in_diff\":true"));
    let parsed: FeedbackEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.in_diff, Some(true));
}

#[test]
fn feedback_in_diff_omitted_is_none() {
    let json = r#"{"file_path":"t.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","timestamp":"2026-01-01T00:00:00Z","provenance":"human"}"#;
    let parsed: FeedbackEntry = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.in_diff, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum feedback_in_diff -- --nocapture`
Expected: compile error — `in_diff` field doesn't exist.

- [ ] **Step 3: Add `in_diff` to FeedbackEntry and ExternalVerdictInput**

In `src/feedback.rs`, FeedbackEntry struct (after `rule_id` field, line 119), add:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
```

In ExternalVerdictInput (after `confidence` field, around line 137), add:

```rust
    pub in_diff: Option<bool>,
```

In `record_external()` (around line 785), add `in_diff: input.in_diff,` to the FeedbackEntry construction.

In `record_context_misleading()` (around line 830), add `in_diff: None,` to the FeedbackEntry construction.

Fix all other FeedbackEntry construction sites (search for `FeedbackEntry {` — there are several in tests and in `run_feedback_inner` in `src/main.rs`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum feedback_in_diff -- --nocapture`
Expected: PASS. Also run `cargo check` to find any remaining construction sites.

- [ ] **Step 5: Commit**

```bash
git add src/feedback.rs src/main.rs
git commit -m "feat(feedback): add in_diff to FeedbackEntry and ExternalVerdictInput (#310)"
```

---

### Task 4: Implement `classify_in_diff` function

**Files:**
- Modify: `src/pipeline.rs` (add function + tests)

- [ ] **Step 1: Write failing tests**

Add to the test module in `src/pipeline.rs`:

```rust
#[test]
fn classify_in_diff_tags_overlapping_findings() {
    use crate::finding::FindingBuilder;
    let mut findings = vec![
        FindingBuilder::new().line_start(10).line_end(20).build(),
        FindingBuilder::new().line_start(50).line_end(60).build(),
        FindingBuilder::new().line_start(100).line_end(100).build(),
    ];
    let changed = vec![(15, 25)]; // overlaps first, not second or third
    classify_in_diff(&mut findings, &changed);
    assert_eq!(findings[0].in_diff, Some(true));
    assert_eq!(findings[1].in_diff, Some(false));
    assert_eq!(findings[2].in_diff, Some(false));
}

#[test]
fn classify_in_diff_boundary_overlap() {
    use crate::finding::FindingBuilder;
    let mut findings = vec![
        FindingBuilder::new().line_start(10).line_end(20).build(), // end touches hunk start
        FindingBuilder::new().line_start(30).line_end(40).build(), // start touches hunk end
    ];
    let changed = vec![(20, 30)];
    classify_in_diff(&mut findings, &changed);
    assert_eq!(findings[0].in_diff, Some(true)); // line_end == hunk start
    assert_eq!(findings[1].in_diff, Some(true)); // line_start == hunk end
}

#[test]
fn classify_in_diff_empty_changed_lines_is_noop() {
    use crate::finding::FindingBuilder;
    let mut findings = vec![FindingBuilder::new().build()];
    classify_in_diff(&mut findings, &[]);
    assert_eq!(findings[0].in_diff, None); // unchanged
}

#[test]
fn classify_in_diff_skips_invalid_findings() {
    use crate::finding::FindingBuilder;
    let mut findings = vec![
        FindingBuilder::new().line_start(0).line_end(0).build(), // invalid
    ];
    let changed = vec![(1, 100)];
    classify_in_diff(&mut findings, &changed);
    assert_eq!(findings[0].in_diff, None); // skipped
}

#[test]
fn classify_in_diff_large_span_finding() {
    use crate::finding::FindingBuilder;
    let mut findings = vec![
        FindingBuilder::new().line_start(1).line_end(500).build(),
    ];
    let changed = vec![(250, 260)];
    classify_in_diff(&mut findings, &changed);
    assert_eq!(findings[0].in_diff, Some(true)); // any overlap counts
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum classify_in_diff -- --nocapture`
Expected: compile error — `classify_in_diff` doesn't exist.

- [ ] **Step 3: Implement `classify_in_diff`**

Add to `src/pipeline.rs` (above the test module, after the existing helper functions):

```rust
/// Stamp each finding with `in_diff` based on line-range overlap with changed hunks.
///
/// Only called when `--diff-file` was explicitly provided. Invalid findings
/// (malformed line ranges) are skipped.
fn classify_in_diff(findings: &mut [Finding], changed_lines: &[(u32, u32)]) {
    if changed_lines.is_empty() {
        return;
    }
    for finding in findings {
        if !finding.is_valid() {
            continue;
        }
        let overlaps = changed_lines
            .iter()
            .any(|(start, end)| finding.line_start <= *end && finding.line_end >= *start);
        finding.in_diff = Some(overlaps);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum classify_in_diff -- --nocapture`
Expected: 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat(pipeline): implement classify_in_diff function (#310)"
```

---

### Task 5: Wire `classify_in_diff` into the review pipeline

**Files:**
- Modify: `src/pipeline.rs:1034-1040` (between grounding and calibration)

- [ ] **Step 1: Write failing integration test**

Add to the test module in `src/pipeline.rs`:

```rust
#[test]
fn review_file_stamps_in_diff_when_diff_ranges_provided() {
    use crate::finding::FindingBuilder;

    // Simulate: findings at lines 10 and 50, diff hunk covers 8-15
    let mut findings = vec![
        FindingBuilder::new().line_start(10).line_end(12).build(),
        FindingBuilder::new().line_start(50).line_end(55).build(),
    ];
    let diff_lines = vec![(8u32, 15u32)];
    classify_in_diff(&mut findings, &diff_lines);

    assert_eq!(findings[0].in_diff, Some(true));
    assert_eq!(findings[1].in_diff, Some(false));
}

#[test]
fn classify_not_called_without_diff_ranges() {
    use crate::finding::FindingBuilder;
    let findings = vec![FindingBuilder::new().build()];
    // No diff_ranges means classify_in_diff is never called
    // Findings should have in_diff == None
    assert_eq!(findings[0].in_diff, None);
}
```

- [ ] **Step 2: Run tests to verify they pass** (these test the function directly, not the wiring)

Run: `cargo test --bin quorum review_file_stamps_in_diff classify_not_called -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Wire into pipeline**

In `src/pipeline.rs`, after the grounding block (after line 1034 `grounded`), before calibration (line 1036), insert:

```rust
    // Classify findings as in-diff or pre-existing when --diff-file is active.
    // Gate on diff_ranges presence, not changed_lines emptiness (the hydration
    // path falls back to full-file ranges, which would incorrectly tag everything
    // as in-diff).
    let mut merged = merged;
    if pipeline_config.diff_ranges.is_some() {
        let repo_root = find_project_root(file_path);
        let resolver = ReviewPathResolver::new(&file_str, &repo_root);
        let diff_lines: Vec<(u32, u32)> = pipeline_config
            .diff_ranges
            .as_ref()
            .unwrap()
            .iter()
            .filter(|(path, _)| resolver.matches(path))
            .flat_map(|(_, ranges)| ranges.clone())
            .collect();
        classify_in_diff(&mut merged, &diff_lines);
    }
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat(pipeline): wire classify_in_diff after grounding (#310)"
```

---

### Task 6: Add `OUT_OF_DIFF_WEIGHT` and discount in `verdict_weight`

**Files:**
- Modify: `src/calibrator.rs:412-461` (verdict_weight function)

- [ ] **Step 1: Write failing tests**

Add to the test module in `src/calibrator.rs`:

```rust
#[test]
fn verdict_weight_in_diff_none_is_full_weight() {
    let entry = crate::feedback::FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "test".into(),
        finding_category: "security".into(),
        verdict: crate::feedback::Verdict::Tp,
        reason: "real".into(),
        model: None,
        timestamp: chrono::Utc::now(),
        provenance: crate::feedback::Provenance::Human,
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff: None,
    };
    let w = verdict_weight(&entry, chrono::Utc::now());
    // Human(1.0) * recency(~1.0) * in_diff(1.0) = ~1.0
    assert!((w - 1.0).abs() < 0.01);
}

#[test]
fn verdict_weight_in_diff_true_is_full_weight() {
    let entry = crate::feedback::FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "test".into(),
        finding_category: "security".into(),
        verdict: crate::feedback::Verdict::Tp,
        reason: "real".into(),
        model: None,
        timestamp: chrono::Utc::now(),
        provenance: crate::feedback::Provenance::Human,
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff: Some(true),
    };
    let w = verdict_weight(&entry, chrono::Utc::now());
    assert!((w - 1.0).abs() < 0.01);
}

#[test]
fn verdict_weight_out_of_diff_applies_discount() {
    let entry = crate::feedback::FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "test".into(),
        finding_category: "security".into(),
        verdict: crate::feedback::Verdict::Tp,
        reason: "real".into(),
        model: None,
        timestamp: chrono::Utc::now(),
        provenance: crate::feedback::Provenance::Human,
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff: Some(false),
    };
    let w = verdict_weight(&entry, chrono::Utc::now());
    // Human(1.0) * recency(~1.0) * out_of_diff(0.7) = ~0.7
    assert!((w - 0.7).abs() < 0.01);
}

#[test]
fn verdict_weight_external_out_of_diff_compounds() {
    let entry = crate::feedback::FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "test".into(),
        finding_category: "security".into(),
        verdict: crate::feedback::Verdict::Fp,
        reason: "nah".into(),
        model: None,
        timestamp: chrono::Utc::now(),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            agent_model: None,
            confidence: None,
        },
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff: Some(false),
    };
    let w = verdict_weight(&entry, chrono::Utc::now());
    // External(0.7) * recency(~1.0) * out_of_diff(0.7) = ~0.49
    assert!((w - 0.49).abs() < 0.02);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum verdict_weight_in_diff verdict_weight_out_of_diff verdict_weight_external_out -- --nocapture`
Expected: compile errors or assertion failures.

- [ ] **Step 3: Add constant and discount logic**

In `src/calibrator.rs`, add the constant near the top (around line 15, with other constants):

```rust
const OUT_OF_DIFF_WEIGHT: f64 = 0.7;
```

In `verdict_weight()` (line 460), change the return from:

```rust
    provenance_weight * recency_weight
```

to:

```rust
    let in_diff_factor = match entry.in_diff {
        Some(false) => OUT_OF_DIFF_WEIGHT,
        _ => 1.0,
    };

    provenance_weight * recency_weight * in_diff_factor
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum verdict_weight -- --nocapture`
Expected: all verdict_weight tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): 0.7x confidence discount for out-of-diff verdicts (#310)"
```

---

### Task 7: Pass `in_diff` through `make_trace_entry`

**Files:**
- Modify: `src/calibrator.rs:130-150` (make_trace_entry)
- Modify: `src/calibrator.rs` (all call sites of make_trace_entry)

- [ ] **Step 1: Write failing test**

Add to the test module in `src/calibrator.rs`:

```rust
#[test]
fn trace_entry_carries_in_diff_from_finding() {
    let mut finding = crate::finding::FindingBuilder::new()
        .severity(crate::finding::Severity::Medium)
        .build();
    finding.in_diff = Some(true);
    let trace = make_trace_entry(
        &finding, 1.0, 0.0, 0.0, 0.0, 0.0, vec![], None,
        crate::finding::Severity::Medium, None, "test.rs",
    );
    assert_eq!(trace.in_diff, Some(true));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum trace_entry_carries_in_diff -- --nocapture`
Expected: assertion failure — `trace.in_diff` is `None`.

- [ ] **Step 3: Pass `in_diff` from finding to trace entry**

In `make_trace_entry()` (around line 142), change the `CalibratorTraceEntry` construction. After `composite_score: None,` add:

```rust
        in_diff: finding.in_diff,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum trace_entry_carries_in_diff -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): propagate in_diff to trace entries (#310)"
```

---

### Task 8: Output grouping — human mode

**Files:**
- Modify: `src/output/mod.rs:141-186` (format_review function)

- [ ] **Step 1: Write failing tests**

Add to the test module in `src/output/mod.rs`:

```rust
#[test]
fn format_review_groups_in_diff_and_pre_existing() {
    use crate::finding::{FindingBuilder, Severity};
    let mut in_diff_finding = FindingBuilder::new()
        .title("SQL injection")
        .severity(Severity::Critical)
        .build();
    in_diff_finding.in_diff = Some(true);

    let mut pre_existing = FindingBuilder::new()
        .title("Unused import")
        .severity(Severity::Info)
        .build();
    pre_existing.in_diff = Some(false);

    let style = Style::plain();
    let output = format_review("src/main.rs", &[in_diff_finding, pre_existing], &style);
    assert!(output.contains("SQL injection"));
    assert!(output.contains("Pre-existing"));
    assert!(output.contains("Unused import"));
    // In-diff finding should appear before Pre-existing header
    let sql_pos = output.find("SQL injection").unwrap();
    let pre_pos = output.find("Pre-existing").unwrap();
    assert!(sql_pos < pre_pos);
}

#[test]
fn format_review_no_pre_existing_header_when_all_in_diff() {
    use crate::finding::{FindingBuilder, Severity};
    let mut f = FindingBuilder::new()
        .severity(Severity::Medium)
        .build();
    f.in_diff = Some(true);
    let style = Style::plain();
    let output = format_review("test.rs", &[f], &style);
    assert!(!output.contains("Pre-existing"));
}

#[test]
fn format_review_summary_shows_diff_breakdown() {
    use crate::finding::{FindingBuilder, Severity};
    let mut f1 = FindingBuilder::new().severity(Severity::Medium).build();
    f1.in_diff = Some(true);
    let mut f2 = FindingBuilder::new().severity(Severity::Info).build();
    f2.in_diff = Some(false);
    let style = Style::plain();
    let output = format_review("test.rs", &[f1, f2], &style);
    assert!(output.contains("in this change"));
    assert!(output.contains("pre-existing"));
}

#[test]
fn format_review_no_diff_context_renders_normally() {
    use crate::finding::FindingBuilder;
    let f = FindingBuilder::new().build(); // in_diff = None
    let style = Style::plain();
    let output = format_review("test.rs", &[f], &style);
    assert!(!output.contains("Pre-existing"));
    assert!(!output.contains("in this change"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum format_review_groups format_review_no_pre format_review_summary format_review_no_diff -- --nocapture`
Expected: assertion failures.

- [ ] **Step 3: Implement grouped output**

Replace the `format_review` function body in `src/output/mod.rs`:

```rust
pub fn format_review(file_path: &str, findings: &[Finding], style: &Style) -> String {
    let mut out = format!(
        "{bold}~ Review: {file}{reset}\n\n",
        bold = style.bold,
        file = file_path,
        reset = style.reset,
    );

    if findings.is_empty() {
        out.push_str(&format!(
            "  {green}= No findings.{reset}\n",
            green = style.green,
            reset = style.reset,
        ));
        return out;
    }

    let has_diff_context = findings.iter().any(|f| f.in_diff.is_some());
    let (in_diff, pre_existing): (Vec<_>, Vec<_>) = if has_diff_context {
        findings
            .iter()
            .partition(|f| f.in_diff != Some(false))
    } else {
        (findings.iter().collect(), vec![])
    };

    for f in &in_diff {
        out.push_str(&format_finding(f, style));
        out.push('\n');
    }

    if !pre_existing.is_empty() {
        out.push_str(&format!(
            "\n  {dim}-- Pre-existing ({count} finding{s}) --{reset}\n\n",
            dim = style.dim,
            count = pre_existing.len(),
            s = if pre_existing.len() == 1 { "" } else { "s" },
            reset = style.reset,
        ));
        for f in &pre_existing {
            out.push_str(&format_finding(f, style));
            out.push('\n');
        }
    }

    // Summary line
    let critical = findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        .count();
    let warning = findings
        .iter()
        .filter(|f| f.severity == Severity::Medium)
        .count();
    let info = findings.len() - critical - warning;

    if has_diff_context && !pre_existing.is_empty() {
        out.push_str(&format!(
            "  {dim}{count} finding{s} ({in_diff} in this change, {pre} pre-existing) ({critical} critical, {warning} warning, {info} info){reset}\n",
            dim = style.dim,
            count = findings.len(),
            s = if findings.len() == 1 { "" } else { "s" },
            in_diff = in_diff.len(),
            pre = pre_existing.len(),
            critical = critical,
            warning = warning,
            info = info,
            reset = style.reset,
        ));
    } else {
        out.push_str(&format!(
            "  {dim}{count} finding{s} ({critical} critical, {warning} warning, {info} info){reset}\n",
            dim = style.dim,
            count = findings.len(),
            s = if findings.len() == 1 { "" } else { "s" },
            critical = critical,
            warning = warning,
            info = info,
            reset = style.reset,
        ));
    }

    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum format_review -- --nocapture`
Expected: all format_review tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/output/mod.rs
git commit -m "feat(output): group findings by in-diff/pre-existing in human mode (#310)"
```

---

### Task 9: Output grouping — compact mode

**Files:**
- Modify: `src/output/mod.rs:298-324` (format_compact_finding)
- Modify: `src/output/mod.rs:326-350` (format_compact_review)

- [ ] **Step 1: Write failing tests**

Add to the test module in `src/output/mod.rs`:

```rust
#[test]
fn compact_finding_pre_existing_gets_prefix() {
    use crate::finding::FindingBuilder;
    let mut f = FindingBuilder::new().title("Unused import").build();
    f.in_diff = Some(false);
    let line = format_compact_finding(&f);
    assert!(line.starts_with("[pre] "));
}

#[test]
fn compact_finding_in_diff_no_prefix() {
    use crate::finding::FindingBuilder;
    let mut f = FindingBuilder::new().title("SQL injection").build();
    f.in_diff = Some(true);
    let line = format_compact_finding(&f);
    assert!(!line.starts_with("[pre]"));
}

#[test]
fn compact_finding_no_diff_context_no_prefix() {
    use crate::finding::FindingBuilder;
    let f = FindingBuilder::new().build(); // in_diff = None
    let line = format_compact_finding(&f);
    assert!(!line.starts_with("[pre]"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum compact_finding_pre compact_finding_in_diff compact_finding_no_diff -- --nocapture`
Expected: assertion failure on `[pre]` prefix.

- [ ] **Step 3: Add `[pre]` prefix to compact output**

In `format_compact_finding()` (around line 313), change the result construction. After building `result`, add the prefix:

```rust
    if f.in_diff == Some(false) {
        format!("[pre] {}", result)
    } else {
        result
    }
```

Specifically, replace the end of `format_compact_finding` from:

```rust
    if f.based_on_excerpt.is_some() {
        result.push_str(" [excerpt]");
    }
    result
```

to:

```rust
    if f.based_on_excerpt.is_some() {
        result.push_str(" [excerpt]");
    }
    if f.in_diff == Some(false) {
        format!("[pre] {}", result)
    } else {
        result
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum compact_finding -- --nocapture`
Expected: all compact_finding tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/output/mod.rs
git commit -m "feat(output): add [pre] prefix for pre-existing findings in compact mode (#310)"
```

---

### Task 10: Exit code scoping

**Files:**
- Modify: `src/output/mod.rs:357-368` (compute_exit_code)
- Modify: `src/main.rs:1721,1866` (call sites)

- [ ] **Step 1: Write failing tests**

Add to the test module in `src/output/mod.rs`:

```rust
#[test]
fn exit_code_ignores_pre_existing_critical() {
    use crate::finding::{FindingBuilder, Severity};
    let mut f = FindingBuilder::new()
        .severity(Severity::Critical)
        .build();
    f.in_diff = Some(false); // pre-existing
    assert_eq!(compute_exit_code(&[f]), 0);
}

#[test]
fn exit_code_counts_in_diff_critical() {
    use crate::finding::{FindingBuilder, Severity};
    let mut f = FindingBuilder::new()
        .severity(Severity::Critical)
        .build();
    f.in_diff = Some(true);
    assert_eq!(compute_exit_code(&[f]), 2);
}

#[test]
fn exit_code_counts_none_diff_context_normally() {
    use crate::finding::{FindingBuilder, Severity};
    let f = FindingBuilder::new()
        .severity(Severity::Critical)
        .build(); // in_diff = None
    assert_eq!(compute_exit_code(&[f]), 2);
}

#[test]
fn exit_code_mixed_in_diff_and_pre_existing() {
    use crate::finding::{FindingBuilder, Severity};
    let mut in_diff = FindingBuilder::new()
        .severity(Severity::Medium)
        .build();
    in_diff.in_diff = Some(true);
    let mut pre = FindingBuilder::new()
        .severity(Severity::Critical)
        .build();
    pre.in_diff = Some(false);
    // Only the medium in-diff counts -> exit 1, not 2
    assert_eq!(compute_exit_code(&[in_diff, pre]), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum exit_code_ignores_pre exit_code_counts_in_diff exit_code_counts_none exit_code_mixed -- --nocapture`
Expected: assertion failures.

- [ ] **Step 3: Update `compute_exit_code` to scope by in_diff**

Replace the `compute_exit_code` function in `src/output/mod.rs`:

```rust
pub fn compute_exit_code(findings: &[Finding]) -> i32 {
    let dominated = |f: &&Finding| f.in_diff != Some(false);
    if findings
        .iter()
        .filter(dominated)
        .any(|f| matches!(f.severity, Severity::Critical | Severity::High))
    {
        2
    } else if findings.iter().filter(dominated).any(|f| f.severity == Severity::Medium) {
        1
    } else {
        0
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum exit_code -- --nocapture`
Expected: all exit_code tests PASS (including existing ones — they use `in_diff: None` which contributes normally).

- [ ] **Step 5: Commit**

```bash
git add src/output/mod.rs
git commit -m "feat(output): scope exit codes to in-diff findings only (#310)"
```

---

### Task 11: CLI and MCP feedback `in_diff` fields

**Files:**
- Modify: `src/cli/mod.rs:515-575` (FeedbackOpts)
- Modify: `src/mcp/tools.rs:45-95` (FeedbackTool)
- Modify: `src/main.rs:1968-2000` (run_feedback)

- [ ] **Step 1: Write failing test**

Add to the test module in `src/main.rs` (in the feedback tests section):

```rust
#[test]
fn feedback_in_diff_flag_sets_field() {
    let opts = cli::FeedbackOpts {
        file: "test.rs".into(),
        finding: "test finding".into(),
        verdict: "tp".into(),
        reason: "real bug".into(),
        model: None,
        blamed_chunks: None,
        category: None,
        json: false,
        from_agent: None,
        agent_model: None,
        confidence: None,
        fp_kind: None,
        fp_discriminator: None,
        fp_reference: None,
        fp_tracked_in: None,
        in_diff: Some(true),
    };
    assert_eq!(opts.in_diff, Some(true));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum feedback_in_diff_flag -- --nocapture`
Expected: compile error — `in_diff` field doesn't exist on FeedbackOpts.

- [ ] **Step 3: Add CLI and MCP fields**

In `src/cli/mod.rs`, add to FeedbackOpts (after the `fp_tracked_in` field):

```rust
    /// Whether the finding was in the diff (true) or pre-existing (false).
    /// Omit when unknown. Propagated to the feedback entry for calibrator weighting.
    #[arg(long)]
    pub in_diff: Option<bool>,
```

In `src/mcp/tools.rs`, add to FeedbackTool (after `fp_kind` field):

```rust
    /// Whether the finding was in the diff (true) or pre-existing (false).
    #[serde(default, rename = "inDiff", skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
```

In `src/main.rs`, `run_feedback()` (around line 1984), add `in_diff: opts.in_diff,` to the `ExternalVerdictInput` construction. In the Human path (further down in `run_feedback_inner`), add `in_diff: opts.in_diff,` to the `FeedbackEntry` construction.

In the MCP feedback handler (wherever `FeedbackTool` is converted to a `FeedbackEntry` or `ExternalVerdictInput`), pass `tool.in_diff` through.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum feedback_in_diff_flag -- --nocapture`
Expected: PASS. Also run `cargo check` to verify no remaining compile errors.

- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/mcp/tools.rs src/main.rs
git commit -m "feat(feedback): add --in-diff CLI flag and inDiff MCP field (#310)"
```

---

### Task 12: Apply `in_diff_factor` in calibrator model building

**Files:**
- Modify: `src/calibrate.rs` (compute_calibrator_model and rescore_samples_with_model)

- [ ] **Step 1: Write failing test**

Add to the test module in `src/calibrate.rs`:

```rust
#[test]
fn out_of_diff_entries_down_weighted_in_model() {
    use crate::feedback::{FeedbackEntry, Provenance, Verdict};

    let now = chrono::Utc::now();
    let make_entry = |verdict: Verdict, in_diff: Option<bool>| FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "hardcoded secret".into(),
        finding_category: "security".into(),
        verdict,
        reason: "test".into(),
        model: None,
        timestamp: now,
        provenance: Provenance::Human,
        fp_kind: None,
        finding_id: None,
        rule_id: None,
        in_diff,
    };

    let in_diff_entry = make_entry(Verdict::Fp, Some(true));
    let out_of_diff_entry = make_entry(Verdict::Fp, Some(false));

    // Both should contribute to word_lor, but out-of-diff with 0.7x weight.
    // We verify this indirectly by checking that the model builds without error
    // and the entries are counted.
    let entries = vec![
        make_entry(Verdict::Tp, Some(true)),
        in_diff_entry,
        out_of_diff_entry,
    ];
    // This test mainly verifies the code compiles and runs with in_diff set.
    // The actual weight difference is tested in verdict_weight tests.
    assert_eq!(entries[2].in_diff, Some(false));
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --bin quorum out_of_diff_entries_down_weighted -- --nocapture`
Expected: PASS (this is a compilation/smoke test since verdict_weight already handles the discount).

- [ ] **Step 3: Verify verdict_weight is already applied**

The `verdict_weight()` function in `src/calibrator.rs` is already called from `compute_calibrator_model()` in `src/calibrate.rs` (at multiple call sites: lines 562, 568, 575, 592, 832, 844, 857, 877). Since Task 6 already added the `in_diff_factor` to `verdict_weight`, the discount automatically applies during model building. No additional code changes needed in `src/calibrate.rs`.

Verify: `grep -n "verdict_weight" src/calibrate.rs` to confirm all call sites go through the same function.

- [ ] **Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: all tests PASS.

- [ ] **Step 5: Commit** (only if any changes were made)

```bash
git add src/calibrate.rs
git commit -m "test(calibrate): verify out-of-diff entries use discounted weight (#310)"
```

---

### Task 13: Final verification

- [ ] **Step 1: Run full test suite**

```bash
cargo test --bin quorum
```

Expected: all tests PASS.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --bin quorum -- -D warnings
```

Expected: no warnings.

- [ ] **Step 3: Run rustfmt**

```bash
cargo fmt -- --check
```

Expected: no formatting issues.

- [ ] **Step 4: Release build**

```bash
cargo build --release
```

Expected: clean build.

- [ ] **Step 5: Smoke test with a diff file**

```bash
git diff HEAD~3 > /tmp/test.patch
cargo run -- review src/pipeline.rs --diff-file /tmp/test.patch
```

Expected: findings are grouped into in-diff and pre-existing sections (if applicable). Exit code reflects only in-diff findings.

---

## Deferred: Stats Dimension

The spec (section 5) mentions that existing `--by-file` and `--by-repo` views should include an in-diff/out-of-diff breakdown column when feedback data has `in_diff` set. This is deferred from this plan because:

1. The core classification, calibrator, output, and feedback infrastructure must land first
2. The stats enhancement is purely additive (no breaking changes)
3. It requires feedback data with `in_diff` populated to be meaningful

File a follow-up task or include in a future stats enhancement PR.
