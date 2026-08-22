# Calibrator Tuning Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Tune the feedback calibrator based on GPT-5.4 and Gemini 3 Pro consensus review to reduce over-boosting, prevent auto-calibrate feedback loops, and improve precision of similarity matching.

**Architecture:** Three targeted changes to `src/calibrator.rs` and `src/feedback_index.rs`: (1) split similarity thresholds for embeddings vs Jaccard, (2) cap auto-calibrate weight accumulation, (3) lengthen recency decay. All changes are to threshold constants and weight math — no structural refactors.

**Tech Stack:** Rust, fastembed (bge-small-en-v1.5), chrono

---

### Task 1: Split similarity threshold — embeddings vs Jaccard

The similarity_threshold of 0.5 is too low for BGE embeddings (where unrelated sentences can score 0.5+) but appropriate for Jaccard. Split the threshold so the embedding path uses 0.75 and Jaccard uses 0.5.

**Files:**
- Modify: `src/calibrator.rs:15-34` (CalibratorConfig)
- Modify: `src/feedback_index.rs:63-70` (find_similar)
- Test: `src/calibrator.rs` (mod tests)

**Step 1: Write failing test**

Add to `src/calibrator.rs` mod tests:

```rust
#[test]
fn calibrator_config_has_separate_thresholds() {
    let config = CalibratorConfig::default();
    assert!(config.embedding_similarity_threshold > config.similarity_threshold,
        "Embedding threshold should be higher than Jaccard threshold");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum calibrator_config_has_separate -- --nocapture`
Expected: FAIL (no field `embedding_similarity_threshold`)

**Step 3: Implement**

In `src/calibrator.rs`, add field to `CalibratorConfig`:

```rust
pub struct CalibratorConfig {
    /// Minimum similarity for Jaccard fallback (0.0 - 1.0)
    pub similarity_threshold: f64,
    /// Minimum similarity for embedding-based matching (higher because BGE clusters tightly)
    pub embedding_similarity_threshold: f64,
    pub fp_suppress_count: usize,
    pub boost_tp: bool,
    pub use_auto_feedback: bool,
}

impl Default for CalibratorConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.5,
            embedding_similarity_threshold: 0.75,
            fp_suppress_count: 2,
            boost_tp: true,
            use_auto_feedback: true,
        }
    }
}
```

Then in `src/feedback_index.rs`, update `find_similar` to accept and pass through both thresholds. The simplest approach: add an `embedding_threshold` parameter to `find_similar`, or pass the full config. Since the calibrator already filters by threshold AFTER calling find_similar (in `calibrate_with_index` at line 168-169), the cleanest fix is to change the threshold used in `calibrate_with_index`:

In `src/calibrator.rs` `calibrate_with_index`, change line 169:
```rust
// Before:
.filter(|s| s.similarity >= config.similarity_threshold as f32)
// After:
.filter(|s| s.similarity >= config.embedding_similarity_threshold as f32)
```

The vanilla `calibrate` function still uses `config.similarity_threshold` for Jaccard, which is correct.

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum calibrator_config_has_separate -- --nocapture`
Expected: PASS

**Step 5: Run full suite**

Run: `cargo test --bin quorum`
Expected: All 485+ tests pass

**Step 6: Commit**

```bash
git add src/calibrator.rs src/feedback_index.rs
git commit -m "feat: split similarity thresholds — 0.75 for embeddings, 0.5 for Jaccard"
```

---

### Task 2: Cap auto-calibrate weight accumulation

Prevent runaway feedback loops where 3+ auto-calibrate FPs (3 * 0.5 = 1.5) self-suppress a finding forever. Cap total auto-calibrate weight at 1.0 per finding — requires human/post_fix corroboration to fully suppress.

**Files:**
- Modify: `src/calibrator.rs:151-224` (calibrate_with_index)
- Modify: `src/calibrator.rs:53-146` (calibrate)
- Test: `src/calibrator.rs` (mod tests)

**Step 1: Write failing test**

Add to mod tests:

```rust
#[test]
fn auto_calibrate_weight_capped_at_one() {
    // 4 auto FPs: uncapped = 4 * 0.5 = 2.0 (would suppress)
    // capped = min(2.0, 1.0) = 1.0 (should NOT suppress alone)
    let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
    let auto_fb = FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "Bug".into(),
        finding_category: "test".into(),
        verdict: Verdict::Fp,
        reason: "auto".into(),
        model: Some("o3".into()),
        timestamp: Utc::now(),
        provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
    };
    let feedback = vec![auto_fb.clone(), auto_fb.clone(), auto_fb.clone(), auto_fb];
    let config = CalibratorConfig::default();
    let result = calibrate(findings, &feedback, &config);
    assert_eq!(result.suppressed, 0,
        "4 auto FPs should not suppress (capped at 1.0 weight, needs human corroboration)");
}

#[test]
fn auto_plus_human_still_suppresses() {
    // 2 auto FPs (capped at 1.0) + 1 human FP (1.0) = 2.0 >= 1.5 -> suppress
    let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
    let auto_fb = FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "Bug".into(),
        finding_category: "test".into(),
        verdict: Verdict::Fp,
        reason: "auto".into(),
        model: Some("o3".into()),
        timestamp: Utc::now(),
        provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
    };
    let human_fb = FeedbackEntry {
        provenance: crate::feedback::Provenance::Human,
        reason: "confirmed FP".into(),
        ..auto_fb.clone()
    };
    let feedback = vec![auto_fb.clone(), auto_fb, human_fb];
    let config = CalibratorConfig::default();
    let result = calibrate(findings, &feedback, &config);
    assert_eq!(result.suppressed, 1,
        "Auto (capped 1.0) + human (1.0) = 2.0 should suppress");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum auto_calibrate_weight_capped -- --nocapture`
Expected: FAIL (first test — 4 auto FPs currently suppress because 4*0.5=2.0 >= 1.5)

**Step 3: Implement**

In both `calibrate` and `calibrate_with_index`, split the weight calculation to cap auto-calibrate contributions. Replace the existing weight calculation blocks:

```rust
// Compute weighted verdict scores with auto-calibrate cap
let (auto_fp, other_fp): (Vec<_>, Vec<_>) = similar.iter()
    .filter(|e| e.verdict == Verdict::Fp || e.verdict == Verdict::Wontfix)
    .partition(|e| matches!(e.provenance, crate::feedback::Provenance::AutoCalibrate(_)));
let auto_fp_weight: f64 = auto_fp.iter().map(|e| verdict_weight(e)).sum::<f64>().min(1.0);
let other_fp_weight: f64 = other_fp.iter().map(|e| verdict_weight(e)).sum();
let fp_weight = auto_fp_weight + other_fp_weight;

let (auto_tp, other_tp): (Vec<_>, Vec<_>) = similar.iter()
    .filter(|e| e.verdict == Verdict::Tp || e.verdict == Verdict::Partial)
    .partition(|e| matches!(e.provenance, crate::feedback::Provenance::AutoCalibrate(_)));
let auto_tp_weight: f64 = auto_tp.iter().map(|e| verdict_weight(e)).sum::<f64>().min(1.0);
let other_tp_weight: f64 = other_tp.iter().map(|e| verdict_weight(e)).sum();
let tp_weight = auto_tp_weight + other_tp_weight;
```

Note: In `calibrate_with_index`, the entries are `SimilarEntry` structs — access `.entry.verdict` and `.entry.provenance`. Also multiply by `s.similarity as f64` as the existing code does.

For `calibrate_with_index`, the pattern is:
```rust
let (auto_fp, other_fp): (Vec<_>, Vec<_>) = similar.iter()
    .filter(|s| s.entry.verdict == Verdict::Fp || s.entry.verdict == Verdict::Wontfix)
    .partition(|s| matches!(s.entry.provenance, crate::feedback::Provenance::AutoCalibrate(_)));
let auto_fp_weight: f64 = auto_fp.iter()
    .map(|s| verdict_weight(&s.entry) * s.similarity as f64).sum::<f64>().min(1.0);
let other_fp_weight: f64 = other_fp.iter()
    .map(|s| verdict_weight(&s.entry) * s.similarity as f64).sum();
let fp_weight = auto_fp_weight + other_fp_weight;
```

Same pattern for tp_weight.

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum auto_calibrate_weight -- --nocapture`
Expected: PASS

**Step 5: Check existing auto-calibrate tests still pass**

Run: `cargo test --bin quorum calibrator -- --nocapture`
Expected: All calibrator tests pass. Note: the existing test `calibrator_includes_auto_feedback_by_default` (3 auto FPs) may now fail since 3 * 0.5 = 1.5 gets capped to 1.0. Update that test to use 2 auto + 1 human FP instead, or adjust the assertion.

**Step 6: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat: cap auto-calibrate weight at 1.0 to prevent self-suppression loops"
```

---

### Task 3: Lengthen recency decay half-life

Change from ~42 days to ~83 days. Code patterns don't change that fast — a 90-day-old FP verdict is still very relevant.

**Files:**
- Modify: `src/calibrator.rs:46-49` (verdict_weight)
- Test: `src/calibrator.rs` (mod tests)

**Step 1: Write failing test**

```rust
#[test]
fn recency_weight_90_day_old_still_meaningful() {
    let old_entry = FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "Bug".into(),
        finding_category: "test".into(),
        verdict: Verdict::Fp,
        reason: "old".into(),
        model: None,
        timestamp: Utc::now() - chrono::Duration::days(90),
        provenance: crate::feedback::Provenance::Human,
    };
    let weight = verdict_weight(&old_entry);
    assert!(weight >= 0.3,
        "90-day-old human feedback should retain >= 30% weight, got {:.3}", weight);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum recency_weight_90 -- --nocapture`
Expected: FAIL (current 90-day weight is ~0.22 with denominator 60)

**Step 3: Implement**

In `verdict_weight`, change:
```rust
// Before:
let recency_weight = (-age_days / 60.0).exp(); // half-life ~42 days
// After:
let recency_weight = (-age_days / 120.0).exp(); // half-life ~83 days
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum recency_weight_90 -- --nocapture`
Expected: PASS (90-day weight is now ~0.47)

**Step 5: Check existing recency test**

The existing `weighted_calibrator_recency_matters` test checks that 2 old (90-day) FPs don't suppress. With the new decay, 2 * 1.0 * 0.47 = 0.94 which is still < 1.5, so the test should still pass. Verify:

Run: `cargo test --bin quorum recency -- --nocapture`

**Step 6: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat: lengthen recency decay half-life from 42 to 83 days"
```

---

### Task 4: Full integration test + comparison

**Step 1: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass

**Step 2: Reinstall**

Run: `cargo install --path .`

**Step 3: Run the comparison script**

Run: `python3 /tmp/quorum-compare2.py`

Compare results against the pre-tuning baseline:
- Pre-tuning: 51 findings with feedback (37 boosted, 25 suppressed)
- Expected: Fewer boosts (stricter embedding threshold), similar or fewer suppressions (auto-cap), better precision

**Step 4: Commit version bump if results are good**

```bash
# Only if comparison shows improvement
cargo test --bin quorum
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.9.2 -- calibrator tuning"
```

---

## Verification Checklist

- [ ] Embedding similarity threshold is 0.75 (Jaccard stays 0.5)
- [ ] Auto-calibrate weight capped at 1.0 per finding
- [ ] Recency half-life is ~83 days (denominator 120)
- [ ] All existing calibrator tests pass (update any that relied on old thresholds)
- [ ] Comparison shows reduced over-boosting
- [ ] No regressions in finding count on clean code
