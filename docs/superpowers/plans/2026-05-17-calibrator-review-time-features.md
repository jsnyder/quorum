# Calibrator Review-Time Feature Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 7 broken logistic calibrator features at review time, fix trace emission corruption, align tokenization, add rate maps to the model, and remove the legacy threshold system.

**Architecture:** Add rate/count maps to `CalibratorModel` (serialized in model.toml), populate them from the full corpus during `quorum calibrate`, and look them up at review time. Fix feature scale mismatches (`ln_1p`, tokenization). Remove the legacy `ThresholdConfig` system and simplify the non-logistic decision path to magic-number heuristics only.

**Tech Stack:** Rust, serde (TOML serialization), cargo test

**Spec:** `docs/superpowers/specs/2026-05-17-calibrator-review-time-features-design.md`

---

### Task 1: Add rate map fields to CalibratorModel

**Files:**
- Modify: `src/calibrator_model.rs:56-66`

- [ ] **Step 1: Write failing serde round-trip test for new fields**

Add to the existing test section in `src/calibrator_model.rs` (after the existing round-trip tests around line ~344):

```rust
#[test]
fn rate_maps_round_trip() {
    let mut model = CalibratorModel {
        meta: ModelMeta {
            computed_at: "2026-01-01T00:00:00Z".to_string(),
            feedback_count: 100,
            global_fp_rate: 0.3,
            learned_weights: None,
        },
        weights: ScoreWeights {
            score: 1.0,
            word_lor: 0.5,
            family_fp_inv: 0.3,
            language_fp_inv: 0.2,
        },
        logistic_model: None,
        word_lor: HashMap::new(),
        family_fp_rate: HashMap::new(),
        language_fp_rate: HashMap::new(),
        category_fp_rate_map: None,
        severity_fp_rate: None,
        model_fp_rate: None,
        file_fp_rate: None,
        file_finding_counts: None,
    };

    // Round-trip with None maps
    let toml_str = model.to_toml();
    let loaded = CalibratorModel::from_toml(&toml_str).unwrap();
    assert!(loaded.category_fp_rate_map.is_none());
    assert!(loaded.severity_fp_rate.is_none());
    assert!(loaded.model_fp_rate.is_none());
    assert!(loaded.file_fp_rate.is_none());
    assert!(loaded.file_finding_counts.is_none());

    // Round-trip with populated maps
    let mut cat_map = HashMap::new();
    cat_map.insert("security".to_string(), 0.25);
    cat_map.insert("correctness".to_string(), 0.4);
    model.category_fp_rate_map = Some(cat_map);

    let mut sev_map = HashMap::new();
    sev_map.insert("critical".to_string(), 0.1);
    model.severity_fp_rate = Some(sev_map);

    let mut model_map = HashMap::new();
    model_map.insert("gpt-5.4".to_string(), 0.35);
    model.model_fp_rate = Some(model_map);

    let mut file_map = HashMap::new();
    file_map.insert("src/main.rs".to_string(), 0.5);
    model.file_fp_rate = Some(file_map);

    let mut count_map = HashMap::new();
    count_map.insert("src/main.rs".to_string(), 42);
    model.file_finding_counts = Some(count_map);

    let toml_str = model.to_toml();
    let loaded = CalibratorModel::from_toml(&toml_str).unwrap();
    assert_eq!(
        loaded.category_fp_rate_map.as_ref().unwrap().get("security"),
        Some(&0.25)
    );
    assert_eq!(
        loaded.severity_fp_rate.as_ref().unwrap().get("critical"),
        Some(&0.1)
    );
    assert_eq!(
        loaded.model_fp_rate.as_ref().unwrap().get("gpt-5.4"),
        Some(&0.35)
    );
    assert_eq!(
        loaded.file_fp_rate.as_ref().unwrap().get("src/main.rs"),
        Some(&0.5)
    );
    assert_eq!(
        loaded.file_finding_counts.as_ref().unwrap().get("src/main.rs"),
        Some(&42)
    );
}

#[test]
fn old_toml_without_rate_maps_loads() {
    // Simulate a pre-upgrade model.toml that lacks the new fields
    let old_toml = r#"
[meta]
computed_at = "2026-01-01T00:00:00Z"
feedback_count = 100
global_fp_rate = 0.3

[weights]
score = 1.0
word_lor = 0.5
family_fp_inv = 0.3
language_fp_inv = 0.2

[word_lor]

[family_fp_rate]

[language_fp_rate]
"#;
    let loaded = CalibratorModel::from_toml(old_toml).unwrap();
    assert!(loaded.category_fp_rate_map.is_none());
    assert!(loaded.severity_fp_rate.is_none());
    assert!(loaded.model_fp_rate.is_none());
    assert!(loaded.file_fp_rate.is_none());
    assert!(loaded.file_finding_counts.is_none());
    assert!((loaded.meta.global_fp_rate - 0.3).abs() < f64::EPSILON);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum rate_maps_round_trip -- --nocapture`
Expected: Compilation error — `CalibratorModel` has no field `category_fp_rate_map`

- [ ] **Step 3: Add the 5 new fields to CalibratorModel struct**

In `src/calibrator_model.rs`, add after the `language_fp_rate` field (line ~65):

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_fp_rate_map: Option<HashMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_fp_rate: Option<HashMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fp_rate: Option<HashMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_fp_rate: Option<HashMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_finding_counts: Option<HashMap<String, usize>>,
```

- [ ] **Step 4: Fix all existing CalibratorModel initializations**

Search for all `CalibratorModel {` struct literals across the codebase and add the 5 new fields as `None`. Key locations:
- `src/calibrate.rs` — where the model is built during `run_calibrate`
- `src/calibrator.rs` — any test fixtures
- `src/calibrator_model.rs` — existing test fixtures

For each, append:
```rust
    category_fp_rate_map: None,
    severity_fp_rate: None,
    model_fp_rate: None,
    file_fp_rate: None,
    file_finding_counts: None,
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum rate_maps_round_trip old_toml_without_rate_maps_loads -- --nocapture`
Expected: PASS for both tests

- [ ] **Step 6: Commit**

```bash
git add src/calibrator_model.rs src/calibrate.rs src/calibrator.rs
git commit -m "feat(calibrator): add rate map fields to CalibratorModel"
```

---

### Task 2: Make tokenize_title public and add parity test

**Files:**
- Modify: `src/calibrate.rs:1518-1525` (visibility change)

- [ ] **Step 1: Write failing test for tokenize_title parity**

Add to `src/calibrate.rs` test section:

```rust
#[test]
fn tokenize_title_drops_digits_and_lowercases() {
    let tokens = tokenize_title("Buffer overflow in parse123 at L42");
    // [a-z_]+ regex: keeps alpha+underscore, drops digits, lowercases
    assert!(tokens.contains(&"buffer".to_string()));
    assert!(tokens.contains(&"overflow".to_string()));
    assert!(tokens.contains(&"parse".to_string()));
    assert!(tokens.contains(&"at".to_string()));
    // Digits-only tokens are excluded
    assert!(!tokens.iter().any(|t| t == "123" || t == "42"));
    // Single-char tokens filtered by len >= 2
    assert!(!tokens.iter().any(|t| t.len() < 2));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum tokenize_title_drops_digits -- --nocapture`
Expected: Compilation error if `tokenize_title` is not accessible in test module, or PASS if already accessible (it's in the same file).

- [ ] **Step 3: Change tokenize_title visibility to pub**

In `src/calibrate.rs`, change `fn tokenize_title` (line ~1518) to:

```rust
pub fn tokenize_title(title: &str) -> Vec<String> {
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum tokenize_title_drops_digits -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): make tokenize_title pub for inference reuse"
```

---

### Task 3: Populate rate maps during `quorum calibrate`

**Files:**
- Modify: `src/calibrate.rs` — `compute_fold_local_stats` result extraction
- Modify: `src/main.rs:2400-2500` — store maps in model after `all_stats` computation

- [ ] **Step 1: Write failing test for rate map population**

Add to `src/calibrate.rs` test section:

```rust
#[test]
fn compute_fold_local_stats_returns_all_rate_maps() {
    let samples = vec![
        JoinedSample {
            title: "SQL injection".to_string(),
            category: "security".to_string(),
            severity: "critical".to_string(),
            model: "gpt-5.4".to_string(),
            tp_weight: 0.0,
            fp_weight: 1.0,
            soft_fp_weight: 0.0,
            full_suppress_weight: 1.0,
            wontfix_weight: 0.0,
            precedent_count: 1,
            max_similarity: 0.9,
            mean_similarity: 0.9,
            is_fp: true,
            family: "sql".to_string(),
            file_path: "src/db.rs".to_string(),
            source_is_ast: false,
            finding_span_lines: 5,
        },
        JoinedSample {
            title: "Buffer overflow".to_string(),
            category: "correctness".to_string(),
            severity: "warning".to_string(),
            model: "gpt-5.4".to_string(),
            tp_weight: 1.0,
            fp_weight: 0.0,
            soft_fp_weight: 0.0,
            full_suppress_weight: 0.0,
            wontfix_weight: 0.0,
            precedent_count: 1,
            max_similarity: 0.8,
            mean_similarity: 0.8,
            is_fp: false,
            family: "memory".to_string(),
            file_path: "src/db.rs".to_string(),
            source_is_ast: true,
            finding_span_lines: 10,
        },
    ];
    let refs: Vec<&JoinedSample> = samples.iter().collect();
    let stats = compute_fold_local_stats(&refs);

    // Category rates exist
    assert!(stats.category_fp_rates.contains_key("security"));
    assert!(stats.category_fp_rates.contains_key("correctness"));
    // Severity rates exist
    assert!(stats.severity_fp_rates.contains_key("critical"));
    assert!(stats.severity_fp_rates.contains_key("warning"));
    // Model rates exist
    assert!(stats.model_fp_rates.contains_key("gpt-5.4"));
    // File rates and counts exist
    assert!(stats.file_fp_rates.contains_key("src/db.rs"));
    assert_eq!(stats.file_finding_counts.get("src/db.rs"), Some(&2));
}
```

- [ ] **Step 2: Run test to verify it passes (stats already computed)**

Run: `cargo test --bin quorum compute_fold_local_stats_returns_all_rate_maps -- --nocapture`
Expected: PASS (the stats function already computes these maps — we just need to store them in the model)

- [ ] **Step 3: Add `store_rate_maps` helper to calibrate.rs**

Add a new public function in `src/calibrate.rs`:

```rust
/// Copy global rate maps from `FoldLocalStats` into a `CalibratorModel` so they
/// are available at review time. File path keys are normalized via
/// `normalize_file_path_deep`. Called after final full-corpus stats are computed.
pub fn store_rate_maps_in_model(
    model: &mut crate::calibrator_model::CalibratorModel,
    stats: &FoldLocalStats,
) {
    model.category_fp_rate_map = Some(stats.category_fp_rates.clone());
    model.severity_fp_rate = Some(stats.severity_fp_rates.clone());
    model.model_fp_rate = Some(stats.model_fp_rates.clone());

    // Normalize file paths before storing
    let file_fp: std::collections::HashMap<String, f64> = stats
        .file_fp_rates
        .iter()
        .map(|(k, &v)| (crate::file_util::normalize_file_path_deep(k), v))
        .collect();
    model.file_fp_rate = Some(file_fp);

    let file_counts: std::collections::HashMap<String, usize> = stats
        .file_finding_counts
        .iter()
        .map(|(k, &v)| (crate::file_util::normalize_file_path_deep(k), v))
        .collect();
    model.file_finding_counts = Some(file_counts);
}
```

- [ ] **Step 4: Wire store_rate_maps_in_model into run_calibrate in main.rs**

In `src/main.rs`, after the `all_stats` computation (around line 2408, after `let all_stats = compute_fold_local_stats(&all_refs);`), add the call to store maps into the composite model before it's serialized. Find where `composite_model` is mutated and add:

```rust
if let Some(ref mut model) = composite_model {
    quorum::calibrate::store_rate_maps_in_model(model, &all_stats);
}
```

This must be placed AFTER `all_stats` is computed and BEFORE the model is written to `calibrator_model.toml`.

- [ ] **Step 5: Run tests and build to verify**

Run: `cargo test --bin quorum compute_fold_local_stats_returns -- --nocapture && cargo build`
Expected: PASS + clean build

- [ ] **Step 6: Commit**

```bash
git add src/calibrate.rs src/main.rs
git commit -m "feat(calibrate): store global rate maps in CalibratorModel"
```

---

### Task 4: Fix review-time feature computation

**Files:**
- Modify: `src/calibrator.rs:338-450` — `calibrate_core_decision` feature extraction

- [ ] **Step 1: Write failing tests for corrected feature values**

Add to `src/calibrator.rs` test section:

```rust
#[test]
fn review_features_full_suppress_weight_is_composite() {
    // full_suppress_weight should be (fp + soft_fp + wontfix).ln_1p()
    // not just fp.ln_1p()
    let fp = 1.0_f64;
    let soft_fp = 0.5_f64;
    let wontfix = 0.3_f64;
    let expected = (fp + soft_fp + wontfix).ln_1p();
    let wrong = fp.ln_1p();
    assert!(
        (expected - wrong).abs() > 0.01,
        "composite and fp-only must differ for this test to be meaningful"
    );
    // The actual feature computation is inside calibrate_core_decision,
    // which is private. We test via the trace output.
}

#[test]
fn review_features_finding_span_lines_uses_ln1p() {
    // A 50-line finding should produce ln_1p(50) ≈ 3.93, not 50.0
    let span = 50_u32;
    let expected = (span as f64).ln_1p();
    assert!((expected - 3.932).abs() < 0.01);
    assert!((expected - 50.0).abs() > 1.0, "must differ from raw");
}
```

- [ ] **Step 2: Run tests to verify they pass (these are value sanity checks)**

Run: `cargo test --bin quorum review_features_full_suppress review_features_finding_span -- --nocapture`
Expected: PASS (these verify the math, not the wiring yet)

- [ ] **Step 3: Fix all 7 features in calibrate_core_decision**

In `src/calibrator.rs`, in the `calibrate_core_decision` function (lines ~350-450):

**3a. Compute full_suppress_weight at the top of the function** (after weights are available, around line ~355):
```rust
    let full_suppress_weight = fp_weight + soft_fp_weight + wontfix_weight;
```

**3b. Fix tokenization** (replace lines ~362-367):
```rust
    // Before:
    let lower = finding.title.to_lowercase();
    let word_lors: Vec<f64> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 2)
        .filter_map(|w| model.word_lor.get(w).copied())
        .collect();

    // After:
    let words = crate::calibrate::tokenize_title(&finding.title);
    let word_lors: Vec<f64> = words
        .iter()
        .filter_map(|w| model.word_lor.get(w.as_str()).copied())
        .collect();
```

**3c. Fix feature values** (in the `ReviewFeatures` struct literal, lines ~404-447):

```rust
    let normalized_file_path = crate::file_util::normalize_file_path_deep(file_path);
    let global_fp_rate = config
        .model
        .as_ref()
        .map(|m| m.meta.global_fp_rate)
        .unwrap_or(0.0);

    let review_features = ReviewFeatures {
        log1p_tp_weight: tp_weight.ln_1p(),
        log1p_fp_weight: fp_weight.ln_1p(),
        precedent_count: (matched_precedents.len() as f64).min(10.0),
        max_similarity: matched_precedents
            .iter()
            .map(|p| p.similarity)
            .fold(0.0_f64, f64::max),
        mean_similarity: if matched_precedents.is_empty() {
            0.0
        } else {
            matched_precedents.iter().map(|p| p.similarity).sum::<f64>()
                / matched_precedents.len() as f64
        },
        has_no_precedents: if matched_precedents.is_empty() {
            1.0
        } else {
            0.0
        },
        log1p_soft_fp_weight: soft_fp_weight.ln_1p(),
        log1p_full_suppress_weight: full_suppress_weight.ln_1p(),
        log1p_wontfix_weight: wontfix_weight.ln_1p(),
        category_fp_rate: config
            .model
            .as_ref()
            .and_then(|m| m.category_fp_rate_map.as_ref())
            .and_then(|map| map.get(&finding.category.to_string()).copied())
            .unwrap_or(global_fp_rate),
        severity_fp_rate: config
            .model
            .as_ref()
            .and_then(|m| m.severity_fp_rate.as_ref())
            .and_then(|map| map.get(&finding.severity.to_string()).copied())
            .unwrap_or(global_fp_rate),
        model_fp_rate: config
            .model
            .as_ref()
            .and_then(|m| m.model_fp_rate.as_ref())
            .and_then(|map| {
                let model_name = finding.model.as_deref().unwrap_or("unknown");
                map.get(model_name).copied()
            })
            .unwrap_or(global_fp_rate),
        max_word_lor,
        min_word_lor,
        count_negative_lor_tokens,
        is_test_file,
        source_is_ast: if source_is_ast { 1.0 } else { 0.0 },
        finding_count_same_file: config
            .model
            .as_ref()
            .and_then(|m| m.file_finding_counts.as_ref())
            .and_then(|counts| counts.get(&normalized_file_path))
            .map(|&c| (c as f64).ln_1p())
            .unwrap_or(0.0),
        file_fp_rate: config
            .model
            .as_ref()
            .and_then(|m| m.file_fp_rate.as_ref())
            .and_then(|map| map.get(&normalized_file_path).copied())
            .unwrap_or(global_fp_rate),
        finding_span_lines: ((finding.line_end.saturating_sub(finding.line_start) + 1)
            as f64)
            .ln_1p(),
        is_mock_or_fixture: if crate::calibrate::is_mock_or_fixture_path(file_path) {
            1.0
        } else {
            0.0
        },
        is_generated_or_vendor: if crate::calibrate::is_generated_or_vendor_path(file_path)
        {
            1.0
        } else {
            0.0
        },
    };
```

Note: `finding.model` — check if `Finding` has a `model` field. If not, use `"unknown"` unconditionally for the model_fp_rate lookup.

- [ ] **Step 4: Run tests to verify it compiles and existing tests pass**

Run: `cargo test --bin quorum -- --nocapture 2>&1 | tail -5`
Expected: All existing tests pass

- [ ] **Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "fix(calibrator): align 7 review-time features with training"
```

---

### Task 5: Fix trace emission for full_suppress_weight

**Files:**
- Modify: `src/calibrator.rs` — all `make_trace_entry` call sites

- [ ] **Step 1: Write failing test for trace full_suppress_weight**

Add to `src/calibrator.rs` test section:

```rust
#[test]
fn trace_full_suppress_weight_includes_soft_fp_and_wontfix() {
    use crate::finding::{Finding, Severity};
    use crate::calibrator_trace::CalibratorTraceEntry;

    let mut finding = Finding {
        title: "test finding".to_string(),
        category: "test".to_string(),
        severity: Severity::Warning,
        ..Finding::default()
    };

    let trace = make_trace_entry(
        &finding,
        1.0,   // tp_weight
        2.0,   // fp_weight
        0.5,   // wontfix_weight
        3.5,   // full_suppress_weight = fp(2.0) + soft_fp(1.0) + wontfix(0.5)
        1.0,   // soft_fp_weight
        vec![],
        None,
        Severity::Warning,
        None,
        "src/test.rs",
    );

    // full_suppress_weight in trace should be the composite, not just fp_weight
    assert!(
        (trace.full_suppress_weight - 3.5).abs() < f64::EPSILON,
        "trace.full_suppress_weight should be composite (3.5), got {}",
        trace.full_suppress_weight
    );
}
```

- [ ] **Step 2: Run test — should pass since make_trace_entry is a pass-through**

Run: `cargo test --bin quorum trace_full_suppress_weight_includes -- --nocapture`
Expected: PASS (make_trace_entry just stores what it receives; the bug is in the CALLER)

- [ ] **Step 3: Fix all make_trace_entry call sites**

In `src/calibrator.rs`, find every call to `make_trace_entry` and ensure the `full_suppress_weight` argument is the composite sum. The `full_suppress_weight` local variable was already computed in Task 4 Step 3a.

**Logistic suppress path** (line ~463): Change `fp_weight` → `full_suppress_weight`
```rust
    // Before:
    fp_weight, // full_suppress_weight = fp_weight
    // After:
    full_suppress_weight,
```

**Logistic boost path** (line ~498): Change `fp_weight` → `full_suppress_weight`

**Logistic fall-through path** (line ~522): Remove `let full_suppress_weight = fp_weight;` — this shadows the correct composite. The function-scoped `full_suppress_weight` from Task 4 Step 3a is already correct.

**Non-logistic suppress path** (line ~596): Already uses `full_suppress_weight` variable — but in the non-logistic path this was set to `fp_weight` at line ~583. Remove `let full_suppress_weight = fp_weight;` at line 583 — the function-scoped variable is correct.

**All remaining call sites**: Verify they use the function-scoped `full_suppress_weight` (the composite).

- [ ] **Step 4: Run tests**

Run: `cargo test --bin quorum -- --nocapture 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "fix(calibrator): emit composite full_suppress_weight in traces"
```

---

### Task 6: Delete threshold_config.rs and remove compute_thresholds

**Files:**
- Delete: `src/threshold_config.rs`
- Modify: `src/lib.rs:92` — remove `pub mod threshold_config`
- Modify: `src/calibrate.rs:12,728-786` — remove import and `compute_thresholds` function

- [ ] **Step 1: Delete threshold_config.rs**

```bash
rm src/threshold_config.rs
```

- [ ] **Step 2: Remove pub mod from lib.rs**

In `src/lib.rs`, remove the line:
```rust
pub mod threshold_config;
```

- [ ] **Step 3: Remove import and compute_thresholds from calibrate.rs**

In `src/calibrate.rs`:
- Remove `use crate::threshold_config::{PathThreshold, ThresholdConfig};` (line 12)
- Remove the entire `compute_thresholds` function (lines 728-786)

- [ ] **Step 4: Attempt to build — collect all dependent compilation errors**

Run: `cargo build 2>&1 | head -60`
Expected: Compilation errors in `src/main.rs` and `src/calibrator.rs` referencing removed types

- [ ] **Step 5: Commit (partial — compilation will fail until Task 7)**

```bash
git add -A
git commit -m "refactor(calibrator): remove threshold_config module and compute_thresholds"
```

---

### Task 7: Remove legacy threshold fields from CalibratorConfig and main.rs

**Files:**
- Modify: `src/calibrator.rs:24-66` — remove 3 fields from `CalibratorConfig`, remove `sanitize_threshold`, simplify non-logistic path
- Modify: `src/cli/mod.rs:251-257` — remove `--suppress-precision` and `--boost-precision` args
- Modify: `src/main.rs` — remove threshold loading, threshold report, force_threshold env var handling

- [ ] **Step 1: Remove threshold fields from CalibratorConfig**

In `src/calibrator.rs`, remove these fields from `CalibratorConfig` (lines ~44-53):
```rust
    // REMOVE these three:
    pub suppress_threshold: Option<f64>,
    pub boost_threshold: Option<f64>,
    pub force_threshold: Option<f64>,
```

Also remove them from the `Default` impl (lines ~68-90).

Remove the `sanitize_threshold` function (lines 319-335).

- [ ] **Step 2: Simplify the non-logistic decision path**

In `calibrate_core_decision`, replace the data-driven + magic-number interleaved path (lines ~564-711) with the magic-number-only path:

```rust
    // --- Non-logistic fallback: heuristic magic numbers ---

    let has_composite = config.model.is_some();
    let total = tp_weight + fp_weight;
    let precedent_score = if total > 0.0 { tp_weight / total } else { 0.5 };
    let lang = CalibratorModel::file_ext_language(file_path);
    let composite = config
        .model
        .as_ref()
        .map(|m| m.composite_score(precedent_score, &finding.title, lang));

    // Full suppress: magic number heuristic
    if full_suppress_weight >= 1.5 && fp_weight > 0.0 && full_suppress_weight > tp_weight * 2.0 {
        finding.calibrator_action = Some(CalibratorAction::Disputed);
        suppressed = true;
        let mut trace = make_trace_entry(
            finding, tp_weight, fp_weight, wontfix_weight,
            full_suppress_weight, soft_fp_weight, matched_precedents,
            finding.calibrator_action.clone(), input_severity,
            Some(crate::calibrator_trace::SeverityChangeReason::Disputed),
            file_path,
        );
        trace.composite_score = composite;
        return CoreDecision { suppressed, boosted, trace };
    }

    // Soft suppress
    let soft_suppressed = (soft_fp_weight >= 1.0 && soft_fp_weight > tp_weight * 2.0)
        || (soft_fp_weight >= 0.5 && tp_weight < 0.1);
    if soft_suppressed {
        finding.severity = Severity::Info;
        finding.calibrator_action = Some(CalibratorAction::Disputed);
        reason = Some(crate::calibrator_trace::SeverityChangeReason::Disputed);
    }

    // Boost: magic number heuristic
    let boost_triggered = if soft_suppressed {
        false
    } else {
        config.boost_tp && tp_weight >= 1.5 && tp_weight > fp_weight * 2.0
    };

    if boost_triggered {
        let proposed = boost_severity(&finding.severity);
        let gate_on = std::env::var("QUORUM_RUBRIC_GATE")
            .map(|v| v != "off" && v != "0")
            .unwrap_or(true);
        if !gate_on || rubric_supports_severity_bump(&proposed, finding) {
            finding.severity = proposed;
            boosted = true;
            reason = Some(crate::calibrator_trace::SeverityChangeReason::Boosted);
        } else {
            reason = Some(crate::calibrator_trace::SeverityChangeReason::BoostBlockedByGate);
        }
        finding.calibrator_action = Some(CalibratorAction::Confirmed);
    } else if tp_weight > fp_weight * 1.5 {
        finding.calibrator_action = Some(CalibratorAction::Confirmed);
        if reason.is_none() {
            reason = Some(crate::calibrator_trace::SeverityChangeReason::BoostWeightTooLow);
        }
    }

    if reason.is_none() {
        reason = Some(crate::calibrator_trace::SeverityChangeReason::BoostWeightTooLow);
    }

    let mut trace = make_trace_entry(
        finding, tp_weight, fp_weight, wontfix_weight,
        full_suppress_weight, soft_fp_weight, matched_precedents,
        finding.calibrator_action.clone(), input_severity, reason, file_path,
    );
    trace.composite_score = composite;

    CoreDecision { suppressed, boosted, trace }
```

- [ ] **Step 3: Remove CLI args from cli/mod.rs**

In `src/cli/mod.rs`, remove the `suppress_precision` and `boost_precision` fields (lines 251-257):
```rust
    // REMOVE:
    #[arg(long, default_value = "0.95")]
    pub suppress_precision: f64,
    #[arg(long, default_value = "0.85")]
    pub boost_precision: f64,
```

- [ ] **Step 4: Remove threshold loading and report from main.rs**

In `src/main.rs`:

**Remove threshold loading** (lines ~1070-1082): Remove the block that loads `calibrator_thresholds.toml` and maps its values into `calibrator_config.suppress_threshold` and `calibrator_config.boost_threshold`.

**Replace with deprecation warning:**
```rust
    let thresholds_path = qhome.join("calibrator_thresholds.toml");
    if thresholds_path.exists() {
        tracing::warn!(
            path = %thresholds_path.display(),
            "calibrator_thresholds.toml is deprecated and no longer used; \
             the logistic model's P(FP) thresholds supersede it. \
             You can safely delete this file."
        );
    }
```

**Remove force_threshold env var** (lines ~1108-1139): Remove the `QUORUM_FORCE_THRESHOLD` parsing block.

**Remove threshold report** (lines ~2583-2618): Remove the `compute_thresholds()` call and the `println!` block that reports suppress/boost thresholds and precision targets.

**Remove precision validation** (lines ~2259-2264): Remove the check on `opts.suppress_precision` and `opts.boost_precision` since these fields no longer exist.

- [ ] **Step 5: Fix all remaining compilation errors**

Run `cargo build` and fix any remaining references to removed fields. Common locations:
- Test fixtures that set `suppress_threshold`, `boost_threshold`, `force_threshold` on `CalibratorConfig`
- Any env var references to `QUORUM_FORCE_THRESHOLD` in tests

For each test fixture, simply remove the fields (they'll use the `Default` impl).

- [ ] **Step 6: Run full test suite**

Run: `cargo test --bin quorum -- --nocapture 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(calibrator): remove legacy threshold system

Remove ThresholdConfig, compute_thresholds, CLI precision args,
and data-driven threshold branches. Non-logistic path retains
magic-number heuristics. Logistic model thresholds are the
primary suppress/boost mechanism.

Closes #375"
```

---

### Task 8: Add missing-maps diagnostic logging

**Files:**
- Modify: `src/calibrator.rs` — add once-per-review logging when rate maps are absent

- [ ] **Step 1: Add diagnostic logging at model load time**

In `src/calibrator.rs`, at the top of the `calibrate` function (line ~811), after the early-return for disabled calibrator, add:

```rust
    if let Some(ref model) = config.model {
        if model.category_fp_rate_map.is_none()
            || model.severity_fp_rate.is_none()
            || model.file_fp_rate.is_none()
        {
            tracing::info!(
                "calibrator model lacks rate maps; run `quorum calibrate` to improve accuracy"
            );
        }
    }
```

This fires once per `calibrate()` call (once per file), not once per finding. For truly once-per-review, the caller in `pipeline.rs` could log it, but once-per-file is acceptable.

- [ ] **Step 2: Run tests**

Run: `cargo test --bin quorum -- --nocapture 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 3: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): log when rate maps are missing from model"
```

---

### Post-Implementation Checklist

After all tasks are complete:

- [ ] `cargo test --bin quorum` — all tests pass
- [ ] `cargo clippy` — no warnings
- [ ] `cargo build --release` — compiles cleanly
- [ ] Run `quorum calibrate` locally — verify new maps appear in `~/.quorum/calibrator_model.toml`
- [ ] Run `quorum review src/calibrator.rs` — verify features are populated (check trace output)
- [ ] Verify `calibrator_thresholds.toml` deprecation warning appears if file exists
