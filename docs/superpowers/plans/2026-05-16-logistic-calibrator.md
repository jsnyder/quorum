# Logistic Calibrator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the broken 4-feature grid-search weight learner with an inline L2-regularized logistic regression over ~15 expanded features, modeling P(FP) directly, with dual thresholds for suppress and boost decisions at review time.

**Architecture:** New `src/logistic.rs` implements batch gradient descent with backtracking line search. `src/calibrate.rs` gains expanded feature extraction (15 features) with per-fold univariate screening. `src/calibrator_model.rs` gains a `LogisticModel` struct stored in TOML. `src/calibrator.rs` gains a logistic scoring path that replaces composite scoring when a logistic model is present.

**Tech Stack:** Rust (no external deps beyond existing), inline logistic regression, z-score standardization, stepwise Average Precision metric.

---

## File Structure

| File | Responsibility | Change Type |
|------|---------------|-------------|
| `src/logistic.rs` | Logistic regression: standardization, gradient descent, line search, prediction | New (~200 LOC) |
| `src/metrics.rs` | Add `average_precision_stepwise()` and `fp_recall_at_tp_recall()` | Modify |
| `src/calibrate.rs` | `ExpandedFeatures` struct (15 fields), fold-local extraction, univariate screening, `learn_logistic()` orchestrator | Modify |
| `src/calibrator_model.rs` | `LogisticModel` struct + TOML ser/de | Modify |
| `src/calibrator.rs` | Review-time logistic scoring path (dual threshold) | Modify |
| `src/main.rs` | Wire `--feature-importance`, upgraded `--learn-weights` flow | Modify |
| `src/cli/mod.rs` | `--feature-importance` flag definition | Modify |

---

### Task 1: Stepwise Average Precision Metric

**Files:**
- Modify: `src/metrics.rs`

This task adds the two metrics needed for feature screening and model evaluation. The existing `pr_auc` uses trapezoidal integration which inflates at high prevalence — stepwise AP fixes this.

- [ ] **Step 1: Write failing test for `average_precision_stepwise`**

```rust
#[test]
fn average_precision_stepwise_basic() {
    // 3 positives, 2 negatives. Scores: [0.9(+), 0.8(-), 0.7(+), 0.4(+), 0.2(-)]
    let samples = vec![
        (0.9, true),
        (0.8, false),
        (0.7, true),
        (0.4, true),
        (0.2, false),
    ];
    let ap = average_precision_stepwise(&samples);
    // At rank 1: P=1/1, recall jumps -> contributes 1.0 * (1/3)
    // At rank 3: P=2/3, recall jumps -> contributes (2/3) * (1/3)
    // At rank 4: P=3/4, recall jumps -> contributes (3/4) * (1/3)
    // AP = (1.0 + 2/3 + 3/4) / 3 = 2.4167/3 = 0.8056
    assert!((ap - 0.8056).abs() < 0.001);
}

#[test]
fn average_precision_stepwise_perfect() {
    let samples = vec![(0.9, true), (0.8, true), (0.1, false)];
    let ap = average_precision_stepwise(&samples);
    assert!((ap - 1.0).abs() < 1e-9);
}

#[test]
fn average_precision_stepwise_worst() {
    // All negatives ranked first
    let samples = vec![(0.9, false), (0.8, false), (0.1, true)];
    let ap = average_precision_stepwise(&samples);
    // Only recall jump at rank 3: P=1/3
    assert!((ap - 1.0 / 3.0).abs() < 0.001);
}

#[test]
fn average_precision_stepwise_empty() {
    let samples: Vec<(f64, bool)> = vec![];
    let ap = average_precision_stepwise(&samples);
    assert_eq!(ap, 0.0);
}

#[test]
fn average_precision_stepwise_no_positives() {
    let samples = vec![(0.9, false), (0.5, false)];
    let ap = average_precision_stepwise(&samples);
    assert_eq!(ap, 0.0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum average_precision_stepwise`
Expected: FAIL — function not found

- [ ] **Step 3: Implement `average_precision_stepwise`**

Add to `src/metrics.rs`:

```rust
/// Stepwise Average Precision (interpolation-free).
/// Sums P(k) * delta_recall(k) over ranks where recall increases.
/// Higher score = more likely positive class.
pub fn average_precision_stepwise(samples: &[(f64, bool)]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total_pos = samples.iter().filter(|(_, p)| *p).count();
    if total_pos == 0 {
        return 0.0;
    }

    let mut sorted: Vec<(f64, bool)> = samples.to_vec();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut tp = 0u64;
    let mut ap_sum = 0.0;
    for (rank, &(_, is_pos)) in sorted.iter().enumerate() {
        if is_pos {
            tp += 1;
            let precision = tp as f64 / (rank as f64 + 1.0);
            ap_sum += precision;
        }
    }
    ap_sum / total_pos as f64
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum average_precision_stepwise`
Expected: PASS (all 5 tests)

- [ ] **Step 5: Write failing test for `fp_recall_at_tp_recall`**

```rust
#[test]
fn fp_recall_at_tp_recall_basic() {
    // 10 samples: 8 TP (label=false in FP-positive), 2 FP (label=true)
    // Model P(FP): FP items scored high, TP items scored low
    // FP items at scores 0.9, 0.7. TP items at 0.1-0.5.
    let predictions: Vec<(f64, bool)> = vec![
        (0.9, true),  // FP
        (0.7, true),  // FP
        (0.5, false), // TP
        (0.4, false), // TP
        (0.3, false), // TP
        (0.3, false), // TP
        (0.2, false), // TP
        (0.2, false), // TP
        (0.1, false), // TP
        (0.1, false), // TP
    ];
    // At threshold=0.6: suppress 2 items (both FP). FP recall = 2/2 = 1.0.
    // TP recall = 8/8 = 1.0 (no TPs suppressed). Meets 99% TP recall.
    let fp_recall = fp_recall_at_tp_recall(&predictions, 0.99);
    assert!((fp_recall - 1.0).abs() < 1e-9);
}

#[test]
fn fp_recall_at_tp_recall_no_separation() {
    // All same score — can't separate
    let predictions: Vec<(f64, bool)> = vec![
        (0.5, true),
        (0.5, true),
        (0.5, false),
        (0.5, false),
    ];
    let fp_recall = fp_recall_at_tp_recall(&predictions, 0.99);
    assert_eq!(fp_recall, 0.0);
}
```

- [ ] **Step 6: Implement `fp_recall_at_tp_recall`**

```rust
/// FP recall achievable at a given minimum TP recall constraint.
///
/// predictions: (P(FP), is_fp) — higher P(FP) means more likely false positive.
/// Sweeps thresholds from high to low, counting how many FPs we catch
/// while keeping TP false-suppression rate within (1 - min_tp_recall).
pub fn fp_recall_at_tp_recall(predictions: &[(f64, bool)], min_tp_recall: f64) -> f64 {
    if predictions.is_empty() {
        return 0.0;
    }
    let total_fp = predictions.iter().filter(|(_, fp)| *fp).count();
    let total_tp = predictions.iter().filter(|(_, fp)| !*fp).count();
    if total_fp == 0 || total_tp == 0 {
        return 0.0;
    }

    let max_tp_suppressed = ((1.0 - min_tp_recall) * total_tp as f64).floor() as usize;

    let mut sorted: Vec<(f64, bool)> = predictions.to_vec();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut fp_caught = 0usize;
    let mut tp_suppressed = 0usize;

    for &(_, is_fp) in &sorted {
        if is_fp {
            fp_caught += 1;
        } else {
            tp_suppressed += 1;
            if tp_suppressed > max_tp_suppressed {
                break;
            }
        }
    }

    fp_caught as f64 / total_fp as f64
}
```

- [ ] **Step 7: Run tests to verify both pass**

Run: `cargo test --bin quorum fp_recall_at_tp_recall`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add src/metrics.rs
git commit -m "feat(metrics): add stepwise average precision and fp_recall_at_tp_recall"
```

---

### Task 2: Logistic Regression Core (`src/logistic.rs`)

**Files:**
- Create: `src/logistic.rs`
- Modify: `src/main.rs` (add `mod logistic;`)

The logistic regression module handles standardization, gradient computation, backtracking line search, and prediction. No external deps.

- [ ] **Step 1: Write failing tests**

Create the test module at the bottom of the new file. Tests cover: sigmoid, standardization, gradient computation, fitting on linearly separable data, and prediction.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_at_zero() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn sigmoid_extreme_positive() {
        assert!((sigmoid(100.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sigmoid_extreme_negative() {
        assert!(sigmoid(-100.0) < 1e-6);
    }

    #[test]
    fn standardize_basic() {
        let data = vec![vec![1.0, 10.0], vec![3.0, 20.0], vec![5.0, 30.0]];
        let (normed, means, stddevs) = standardize(&data);
        // Mean of col 0: 3.0, stddev: ~1.633
        assert!((means[0] - 3.0).abs() < 1e-9);
        // Normalized first element: (1.0 - 3.0) / stddev
        assert!(normed[0][0] < 0.0);
        // All columns have mean ~0
        let col0_mean: f64 = normed.iter().map(|r| r[0]).sum::<f64>() / 3.0;
        assert!(col0_mean.abs() < 1e-9);
    }

    #[test]
    fn standardize_constant_column() {
        let data = vec![vec![5.0, 1.0], vec![5.0, 2.0], vec![5.0, 3.0]];
        let (normed, _, stddevs) = standardize(&data);
        // Constant column: stddev ~ 0, but epsilon prevents div by zero
        assert!(normed[0][0].is_finite());
        assert!(stddevs[0] < 1e-6);
    }

    #[test]
    fn fit_linearly_separable() {
        // Class 1 (FP): feature > 0.5; Class 0 (TP): feature < 0.5
        let x: Vec<Vec<f64>> = (0..100)
            .map(|i| vec![i as f64 / 100.0])
            .collect();
        let y: Vec<bool> = (0..100).map(|i| i >= 50).collect();
        let result = fit(&x, &y, 0.1, 500);
        // Should achieve high accuracy
        let correct = x.iter().zip(y.iter()).filter(|(xi, &yi)| {
            let p = result.predict_one(xi);
            (p >= 0.5) == yi
        }).count();
        assert!(correct >= 90);
    }

    #[test]
    fn predict_batch() {
        let model = LogisticFit {
            coefficients: vec![1.0],
            intercept: 0.0,
            feature_means: vec![0.0],
            feature_stddevs: vec![1.0],
        };
        let x = vec![vec![2.0], vec![-2.0]];
        let preds = model.predict(&x);
        assert!(preds[0] > 0.5);
        assert!(preds[1] < 0.5);
    }
}
```

- [ ] **Step 2: Create `src/logistic.rs` with stubs and tests, add module declaration**

Add `mod logistic;` to `src/main.rs` (near other mod declarations).

Create `src/logistic.rs`:

```rust
const EPSILON: f64 = 1e-8;
const ARMIJO_C: f64 = 1e-4;
const ARMIJO_BETA: f64 = 0.5;

pub fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        let e = (-z).exp();
        1.0 / (1.0 + e)
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

/// Z-score normalization. Returns (normalized_data, means, stddevs).
pub fn standardize(data: &[Vec<f64>]) -> (Vec<Vec<f64>>, Vec<f64>, Vec<f64>) {
    let n = data.len();
    if n == 0 {
        return (vec![], vec![], vec![]);
    }
    let d = data[0].len();
    let mut means = vec![0.0; d];
    let mut stddevs = vec![0.0; d];

    for row in data {
        for (j, val) in row.iter().enumerate() {
            means[j] += val;
        }
    }
    for m in &mut means {
        *m /= n as f64;
    }

    for row in data {
        for (j, val) in row.iter().enumerate() {
            let diff = val - means[j];
            stddevs[j] += diff * diff;
        }
    }
    for s in &mut stddevs {
        *s = (*s / n as f64).sqrt();
    }

    let normed: Vec<Vec<f64>> = data
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(j, val)| (val - means[j]) / (stddevs[j] + EPSILON))
                .collect()
        })
        .collect();

    (normed, means, stddevs)
}

/// Result of logistic regression fitting.
#[derive(Debug, Clone)]
pub struct LogisticFit {
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    pub feature_means: Vec<f64>,
    pub feature_stddevs: Vec<f64>,
}

impl LogisticFit {
    /// Predict P(positive) for a single raw (unstandardized) sample.
    pub fn predict_one(&self, x: &[f64]) -> f64 {
        let z: f64 = x
            .iter()
            .enumerate()
            .map(|(j, val)| {
                let normed = (val - self.feature_means[j]) / (self.feature_stddevs[j] + EPSILON);
                self.coefficients[j] * normed
            })
            .sum::<f64>()
            + self.intercept;
        sigmoid(z)
    }

    /// Predict P(positive) for a batch of raw samples.
    pub fn predict(&self, data: &[Vec<f64>]) -> Vec<f64> {
        data.iter().map(|x| self.predict_one(x)).collect()
    }
}

/// L2-regularized logistic regression via batch gradient descent with
/// backtracking line search (Armijo condition).
///
/// - `x`: feature matrix (raw, will be standardized internally)
/// - `y`: labels (true = positive class)
/// - `lambda`: L2 regularization strength
/// - `max_iter`: maximum gradient descent iterations
pub fn fit(x: &[Vec<f64>], y: &[bool], lambda: f64, max_iter: usize) -> LogisticFit {
    let (normed, means, stddevs) = standardize(x);
    let n = normed.len();
    let d = if n > 0 { normed[0].len() } else { 0 };

    let mut w = vec![0.0; d];
    let mut b = 0.0;

    let y_f: Vec<f64> = y.iter().map(|&v| if v { 1.0 } else { 0.0 }).collect();

    let mut prev_loss = f64::MAX;

    for _iter in 0..max_iter {
        let (loss, grad_w, grad_b) = compute_loss_and_grad(&normed, &y_f, &w, b, lambda);

        // Convergence check: relative loss change
        if prev_loss < f64::MAX {
            let rel_change = (prev_loss - loss) / (prev_loss.abs() + EPSILON);
            if rel_change < 1e-4 && rel_change >= 0.0 {
                break;
            }
        }
        prev_loss = loss;

        // Backtracking line search (Armijo)
        let mut step = 1.0;
        let grad_norm_sq: f64 =
            grad_w.iter().map(|g| g * g).sum::<f64>() + grad_b * grad_b;

        for _ in 0..20 {
            let new_w: Vec<f64> = w
                .iter()
                .zip(grad_w.iter())
                .map(|(wi, gi)| wi - step * gi)
                .collect();
            let new_b = b - step * grad_b;
            let new_loss = compute_loss(&normed, &y_f, &new_w, new_b, lambda);
            if new_loss <= loss - ARMIJO_C * step * grad_norm_sq {
                w = new_w;
                b = new_b;
                break;
            }
            step *= ARMIJO_BETA;
            if step < 1e-10 {
                // Step too small, just take the gradient step
                w = w
                    .iter()
                    .zip(grad_w.iter())
                    .map(|(wi, gi)| wi - step * gi)
                    .collect();
                b -= step * grad_b;
                break;
            }
        }
    }

    LogisticFit {
        coefficients: w,
        intercept: b,
        feature_means: means,
        feature_stddevs: stddevs,
    }
}

fn compute_loss_and_grad(
    x: &[Vec<f64>],
    y: &[f64],
    w: &[f64],
    b: f64,
    lambda: f64,
) -> (f64, Vec<f64>, f64) {
    let n = x.len();
    let d = w.len();
    let mut loss = 0.0;
    let mut grad_w = vec![0.0; d];
    let mut grad_b = 0.0;

    for (i, xi) in x.iter().enumerate() {
        let z: f64 = xi.iter().zip(w.iter()).map(|(xj, wj)| xj * wj).sum::<f64>() + b;
        let p = sigmoid(z);
        let yi = y[i];

        // Cross-entropy loss (numerically stable)
        loss += -yi * z + z.max(0.0) + (1.0 + (-z.abs()).exp()).ln();

        let err = p - yi;
        for (j, xj) in xi.iter().enumerate() {
            grad_w[j] += err * xj;
        }
        grad_b += err;
    }

    // Average + L2 regularization (not on intercept)
    loss /= n as f64;
    loss += 0.5 * lambda * w.iter().map(|wi| wi * wi).sum::<f64>();

    for (j, gj) in grad_w.iter_mut().enumerate() {
        *gj = *gj / n as f64 + lambda * w[j];
    }
    grad_b /= n as f64;

    (loss, grad_w, grad_b)
}

fn compute_loss(x: &[Vec<f64>], y: &[f64], w: &[f64], b: f64, lambda: f64) -> f64 {
    let n = x.len();
    let mut loss = 0.0;
    for (i, xi) in x.iter().enumerate() {
        let z: f64 = xi.iter().zip(w.iter()).map(|(xj, wj)| xj * wj).sum::<f64>() + b;
        let yi = y[i];
        loss += -yi * z + z.max(0.0) + (1.0 + (-z.abs()).exp()).ln();
    }
    loss /= n as f64;
    loss += 0.5 * lambda * w.iter().map(|wi| wi * wi).sum::<f64>();
    loss
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --bin quorum logistic::tests`
Expected: PASS (all 6 tests)

- [ ] **Step 4: Commit**

```bash
git add src/logistic.rs src/main.rs
git commit -m "feat: add inline L2-regularized logistic regression (src/logistic.rs)"
```

---

### Task 3: Expanded Feature Extraction

**Files:**
- Modify: `src/calibrate.rs`

Replace the 4-field `SampleFeatures` with 15-field `ExpandedFeatures`. Keep `SampleFeatures` intact for backward compat with existing grid search (it still runs as a comparison). Add a new `extract_expanded_features()` function.

- [ ] **Step 1: Write failing test for `ExpandedFeatures` construction**

```rust
#[test]
fn expanded_features_to_vec_correct_order() {
    let f = ExpandedFeatures {
        log1p_tp_weight: 1.0,
        log1p_fp_weight: 0.5,
        precedent_count: 3.0,
        max_similarity: 0.9,
        mean_similarity: 0.7,
        has_no_precedents: 0.0,
        log1p_soft_fp_weight: 0.3,
        log1p_full_suppress_weight: 0.1,
        log1p_wontfix_weight: 0.0,
        category_fp_rate: 0.25,
        severity_fp_rate: 0.18,
        model_fp_rate: 0.22,
        max_word_lor: 2.1,
        min_word_lor: -1.5,
        count_negative_lor_tokens: 3.0,
    };
    let v = f.to_vec();
    assert_eq!(v.len(), 15);
    assert!((v[0] - 1.0).abs() < 1e-9);
    assert!((v[14] - 3.0).abs() < 1e-9);
}

#[test]
fn expanded_features_names_match_vec_order() {
    let names = ExpandedFeatures::feature_names();
    assert_eq!(names.len(), 15);
    assert_eq!(names[0], "log1p_tp_weight");
    assert_eq!(names[5], "has_no_precedents");
    assert_eq!(names[14], "count_negative_lor_tokens");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum expanded_features`
Expected: FAIL — struct not found

- [ ] **Step 3: Implement `ExpandedFeatures` struct**

Add to `src/calibrate.rs` below the existing `SampleFeatures`:

```rust
/// Expanded feature vector for logistic calibrator (15 dimensions).
/// Field order matches `to_vec()` and `feature_names()`.
#[derive(Debug, Clone)]
pub struct ExpandedFeatures {
    // Precedent decomposition (6)
    pub log1p_tp_weight: f64,
    pub log1p_fp_weight: f64,
    pub precedent_count: f64,
    pub max_similarity: f64,
    pub mean_similarity: f64,
    pub has_no_precedents: f64,
    // Weight accumulators (3)
    pub log1p_soft_fp_weight: f64,
    pub log1p_full_suppress_weight: f64,
    pub log1p_wontfix_weight: f64,
    // Smoothed priors (3)
    pub category_fp_rate: f64,
    pub severity_fp_rate: f64,
    pub model_fp_rate: f64,
    // Text statistics (3)
    pub max_word_lor: f64,
    pub min_word_lor: f64,
    pub count_negative_lor_tokens: f64,
}

impl ExpandedFeatures {
    pub fn to_vec(&self) -> Vec<f64> {
        vec![
            self.log1p_tp_weight,
            self.log1p_fp_weight,
            self.precedent_count,
            self.max_similarity,
            self.mean_similarity,
            self.has_no_precedents,
            self.log1p_soft_fp_weight,
            self.log1p_full_suppress_weight,
            self.log1p_wontfix_weight,
            self.category_fp_rate,
            self.severity_fp_rate,
            self.model_fp_rate,
            self.max_word_lor,
            self.min_word_lor,
            self.count_negative_lor_tokens,
        ]
    }

    pub fn feature_names() -> Vec<&'static str> {
        vec![
            "log1p_tp_weight",
            "log1p_fp_weight",
            "precedent_count",
            "max_similarity",
            "mean_similarity",
            "has_no_precedents",
            "log1p_soft_fp_weight",
            "log1p_full_suppress_weight",
            "log1p_wontfix_weight",
            "category_fp_rate",
            "severity_fp_rate",
            "model_fp_rate",
            "max_word_lor",
            "min_word_lor",
            "count_negative_lor_tokens",
        ]
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum expanded_features`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): add ExpandedFeatures struct with 15 dimensions"
```

---

### Task 4: Fold-Local Feature Extraction

**Files:**
- Modify: `src/calibrate.rs`

Add `extract_expanded_features()` that computes all 15 features from the joined corpus. Target-encoded features (category_fp_rate, severity_fp_rate, model_fp_rate, word_lor) are computed from a provided subset (enabling fold-local computation in Task 6).

- [ ] **Step 1: Write failing test for fold-local target encoding**

```rust
#[test]
fn beta_smoothed_rate_basic() {
    // 3 FP out of 10 total, alpha=5
    let rate = beta_smoothed_rate(3, 10, 5.0);
    // (3 + 5*global) / (10 + 5) where global = 0.18 (example)
    // But we pass the global_rate in, so: (3 + 5*0.18) / (10 + 5) = 3.9/15 = 0.26
    assert!((rate - 0.26).abs() < 0.01);
}

#[test]
fn extract_expanded_features_produces_correct_count() {
    // Minimal integration test with synthetic data
    let features = extract_expanded_features_from_fold(
        &sample_traces(),
        &sample_feedback(),
        &FoldLocalStats::default(),
    );
    assert!(!features.is_empty());
    for (f, _label) in &features {
        let v = f.to_vec();
        assert_eq!(v.len(), 15);
        assert!(v.iter().all(|x| x.is_finite()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum extract_expanded_features`
Expected: FAIL

- [ ] **Step 3: Implement `FoldLocalStats` and `extract_expanded_features_from_fold`**

```rust
/// Per-fold computed statistics for target-encoded features.
/// Prevents leakage by computing rates only from the training partition.
#[derive(Debug, Clone)]
pub struct FoldLocalStats {
    pub category_fp_rates: HashMap<String, f64>,
    pub severity_fp_rates: HashMap<String, f64>,
    pub model_fp_rates: HashMap<String, f64>,
    pub word_lor: HashMap<String, f64>,
    pub global_fp_rate: f64,
}

impl Default for FoldLocalStats {
    fn default() -> Self {
        Self {
            category_fp_rates: HashMap::new(),
            severity_fp_rates: HashMap::new(),
            model_fp_rates: HashMap::new(),
            word_lor: HashMap::new(),
            global_fp_rate: 0.18,
        }
    }
}

const BETA_ALPHA: f64 = 5.0;

pub fn beta_smoothed_rate(fp_count: usize, total: usize, global_rate: f64) -> f64 {
    (fp_count as f64 + BETA_ALPHA * global_rate) / (total as f64 + BETA_ALPHA)
}

/// Compute FoldLocalStats from a training partition of feedback entries.
pub fn compute_fold_local_stats(
    feedback_partition: &[&serde_json::Value],
    traces_partition: &[&serde_json::Value],
) -> FoldLocalStats {
    let mut total_fp = 0usize;
    let mut total = 0usize;
    let mut category_counts: HashMap<String, (usize, usize)> = HashMap::new(); // (fp, total)
    let mut severity_counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut model_counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut word_tp: HashMap<String, usize> = HashMap::new();
    let mut word_fp: HashMap<String, usize> = HashMap::new();

    for entry in feedback_partition {
        let verdict = entry["verdict"].as_str().unwrap_or("");
        let is_fp = verdict == "fp";
        let is_tp = verdict == "tp" || verdict == "partial";
        if !is_fp && !is_tp {
            continue;
        }
        total += 1;
        if is_fp {
            total_fp += 1;
        }

        let category = entry["finding_category"].as_str().unwrap_or("unknown").to_string();
        let severity = entry["input_severity"].as_str().unwrap_or("medium").to_string();
        let model = entry["model"].as_str().unwrap_or("unknown").to_string();

        let cat_entry = category_counts.entry(category).or_insert((0, 0));
        cat_entry.1 += 1;
        if is_fp { cat_entry.0 += 1; }

        let sev_entry = severity_counts.entry(severity).or_insert((0, 0));
        sev_entry.1 += 1;
        if is_fp { sev_entry.0 += 1; }

        let mod_entry = model_counts.entry(model).or_insert((0, 0));
        mod_entry.1 += 1;
        if is_fp { mod_entry.0 += 1; }

        // Word LOR from title
        let title = entry["finding_title"].as_str().unwrap_or("");
        for word in tokenize_title(title) {
            if is_tp {
                *word_tp.entry(word.clone()).or_insert(0) += 1;
            } else {
                *word_fp.entry(word.clone()).or_insert(0) += 1;
            }
        }
    }

    let global_fp_rate = if total > 0 { total_fp as f64 / total as f64 } else { 0.18 };

    let category_fp_rates = category_counts
        .iter()
        .map(|(k, (fp, t))| (k.clone(), beta_smoothed_rate(*fp, *t, global_fp_rate)))
        .collect();

    let severity_fp_rates = severity_counts
        .iter()
        .map(|(k, (fp, t))| (k.clone(), beta_smoothed_rate(*fp, *t, global_fp_rate)))
        .collect();

    let model_fp_rates = model_counts
        .iter()
        .map(|(k, (fp, t))| (k.clone(), beta_smoothed_rate(*fp, *t, global_fp_rate)))
        .collect();

    // Word LOR: ln((fp_rate + eps) / (tp_rate + eps)), min support 5
    let mut word_lor = HashMap::new();
    let all_words: HashSet<&String> = word_tp.keys().chain(word_fp.keys()).collect();
    for word in all_words {
        let tp_c = word_tp.get(word).copied().unwrap_or(0);
        let fp_c = word_fp.get(word).copied().unwrap_or(0);
        if tp_c + fp_c < 5 {
            continue;
        }
        let tp_rate = (tp_c as f64 + 0.5) / (total.saturating_sub(total_fp) as f64 + 1.0);
        let fp_rate = (fp_c as f64 + 0.5) / (total_fp as f64 + 1.0);
        word_lor.insert(word.clone(), (fp_rate / tp_rate).ln());
    }

    FoldLocalStats {
        category_fp_rates,
        severity_fp_rates,
        model_fp_rates,
        word_lor,
        global_fp_rate,
    }
}

fn tokenize_title(title: &str) -> Vec<String> {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 2)
        .map(|s| s.to_string())
        .collect()
}

/// Extract expanded features for each sample using fold-local stats.
pub fn extract_expanded_features_from_fold(
    joined_samples: &[JoinedSample],
    fold_stats: &FoldLocalStats,
) -> Vec<(ExpandedFeatures, bool)> {
    joined_samples
        .iter()
        .map(|s| {
            let words = tokenize_title(&s.title);
            let word_lors: Vec<f64> = words
                .iter()
                .filter_map(|w| fold_stats.word_lor.get(w).copied())
                .collect();

            let max_word_lor = word_lors.iter().copied().fold(0.0_f64, f64::max);
            let min_word_lor = word_lors.iter().copied().fold(0.0_f64, f64::min);
            let count_negative_lor = word_lors.iter().filter(|&&v| v < -0.5).count() as f64;

            let category_fp = fold_stats
                .category_fp_rates
                .get(&s.category)
                .copied()
                .unwrap_or(fold_stats.global_fp_rate);
            let severity_fp = fold_stats
                .severity_fp_rates
                .get(&s.severity)
                .copied()
                .unwrap_or(fold_stats.global_fp_rate);
            let model_fp = fold_stats
                .model_fp_rates
                .get(&s.model)
                .copied()
                .unwrap_or(fold_stats.global_fp_rate);

            let features = ExpandedFeatures {
                log1p_tp_weight: s.tp_weight.ln_1p(),
                log1p_fp_weight: s.fp_weight.ln_1p(),
                precedent_count: (s.precedent_count as f64).min(10.0),
                max_similarity: s.max_similarity,
                mean_similarity: s.mean_similarity,
                has_no_precedents: if s.precedent_count == 0 { 1.0 } else { 0.0 },
                log1p_soft_fp_weight: s.soft_fp_weight.ln_1p(),
                log1p_full_suppress_weight: s.full_suppress_weight.ln_1p(),
                log1p_wontfix_weight: s.wontfix_weight.ln_1p(),
                category_fp_rate: category_fp,
                severity_fp_rate: severity_fp,
                model_fp_rate: model_fp,
                max_word_lor,
                min_word_lor,
                count_negative_lor_tokens: count_negative_lor,
            };
            (features, s.is_fp)
        })
        .collect()
}

/// Intermediate representation of a joined sample before feature extraction.
#[derive(Debug, Clone)]
pub struct JoinedSample {
    pub title: String,
    pub category: String,
    pub severity: String,
    pub model: String,
    pub tp_weight: f64,
    pub fp_weight: f64,
    pub soft_fp_weight: f64,
    pub full_suppress_weight: f64,
    pub wontfix_weight: f64,
    pub precedent_count: usize,
    pub max_similarity: f64,
    pub mean_similarity: f64,
    pub is_fp: bool,
    pub family: String,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum extract_expanded_features`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): fold-local feature extraction with 15 expanded features"
```

---

### Task 5: Univariate Feature Screening

**Files:**
- Modify: `src/calibrate.rs`

Per-fold screening: compute stepwise AP for each feature individually, retain those with AP >= baseline + 0.02.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn univariate_screen_selects_discriminative_features() {
    // Feature 0: perfect separation (AP=1.0)
    // Feature 1: random (AP~=baseline)
    // Feature 2: moderate (AP=0.5)
    let samples: Vec<(ExpandedFeatures, bool)> = (0..100)
        .map(|i| {
            let is_fp = i < 20; // 20% FP
            let mut f = ExpandedFeatures::zeros();
            f.log1p_tp_weight = if is_fp { 0.9 } else { 0.1 }; // perfect
            f.log1p_fp_weight = (i as f64 * 0.01) % 1.0; // random
            f.precedent_count = if is_fp { 0.6 } else { 0.3 }; // moderate
            (f, is_fp)
        })
        .collect();

    let selected = univariate_screen(&samples, 0.20);
    // baseline AP = 0.20 (prevalence). Threshold = 0.22.
    // Feature 0 (AP=1.0): selected
    // Feature 1 (AP~0.20): NOT selected
    // Feature 2 (AP~0.5+): selected
    assert!(selected.contains(&0));
    assert!(!selected.contains(&1));
    assert!(selected.contains(&2));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum univariate_screen`
Expected: FAIL

- [ ] **Step 3: Implement `univariate_screen`**

```rust
impl ExpandedFeatures {
    pub fn zeros() -> Self {
        Self {
            log1p_tp_weight: 0.0,
            log1p_fp_weight: 0.0,
            precedent_count: 0.0,
            max_similarity: 0.0,
            mean_similarity: 0.0,
            has_no_precedents: 0.0,
            log1p_soft_fp_weight: 0.0,
            log1p_full_suppress_weight: 0.0,
            log1p_wontfix_weight: 0.0,
            category_fp_rate: 0.0,
            severity_fp_rate: 0.0,
            model_fp_rate: 0.0,
            max_word_lor: 0.0,
            min_word_lor: 0.0,
            count_negative_lor_tokens: 0.0,
        }
    }
}

/// Univariate feature screening: returns indices of features with
/// stepwise AP >= baseline_ap + 0.02.
pub fn univariate_screen(
    samples: &[(ExpandedFeatures, bool)],
    baseline_ap: f64,
) -> Vec<usize> {
    let threshold = baseline_ap + 0.02;
    let n_features = 15;
    let mut selected = Vec::new();

    for feat_idx in 0..n_features {
        let univariate: Vec<(f64, bool)> = samples
            .iter()
            .map(|(f, label)| (f.to_vec()[feat_idx], *label))
            .collect();
        let ap = crate::metrics::average_precision_stepwise(&univariate);
        if ap >= threshold {
            selected.push(feat_idx);
        }
    }

    selected
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum univariate_screen`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): univariate feature screening by stepwise AP"
```

---

### Task 6: Cross-Validated Logistic Training Orchestrator

**Files:**
- Modify: `src/calibrate.rs`

Implements the full `learn_logistic()` pipeline: GroupKFold splitting, per-fold feature extraction + screening, logistic fitting with lambda grid, OOF prediction collection, consensus feature selection, and production model training.

- [ ] **Step 1: Write failing test for `learn_logistic`**

```rust
#[test]
fn learn_logistic_returns_none_below_min_samples() {
    let samples: Vec<(JoinedSample, &str)> = vec![]; // empty
    let result = learn_logistic(&[], 5);
    assert!(result.is_none());
}

#[test]
fn learn_logistic_on_synthetic_separable_data() {
    // Generate 200 samples with known separation
    let mut samples = Vec::new();
    for i in 0..200 {
        let is_fp = i < 40; // 20% FP
        let s = JoinedSample {
            title: if is_fp { "bad pattern".to_string() } else { "good code".to_string() },
            category: "correctness".to_string(),
            severity: "medium".to_string(),
            model: "gpt-5.4".to_string(),
            tp_weight: if is_fp { 0.1 } else { 2.0 },
            fp_weight: if is_fp { 2.0 } else { 0.1 },
            soft_fp_weight: if is_fp { 1.5 } else { 0.0 },
            full_suppress_weight: if is_fp { 2.0 } else { 0.0 },
            wontfix_weight: 0.0,
            precedent_count: if is_fp { 0 } else { 3 },
            max_similarity: if is_fp { 0.3 } else { 0.8 },
            mean_similarity: if is_fp { 0.2 } else { 0.7 },
            is_fp,
            family: format!("family_{}", i % 20),
        };
        samples.push(s);
    }

    let result = learn_logistic(&samples, 5);
    assert!(result.is_some());
    let model = result.unwrap();
    assert!(model.ap_score > 0.30); // should beat baseline 0.20
    assert!(!model.selected_features.is_empty());
    assert!(model.selected_features.len() >= 2);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum learn_logistic`
Expected: FAIL

- [ ] **Step 3: Implement `LogisticResult` and `learn_logistic`**

```rust
const MIN_SAMPLES_FOR_LOGISTIC: usize = 200;
const MIN_CLASS_COUNT: usize = 30;
const LAMBDA_GRID: &[f64] = &[0.01, 0.1, 1.0, 10.0];
const MAX_LOGISTIC_ITER: usize = 500;

#[derive(Debug, Clone)]
pub struct LogisticResult {
    pub selected_features: Vec<usize>,
    pub selected_feature_names: Vec<String>,
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    pub feature_means: Vec<f64>,
    pub feature_stddevs: Vec<f64>,
    pub suppress_threshold: f64,
    pub boost_threshold: f64,
    pub ap_score: f64,
    pub fp_recall_at_99_tp_recall: f64,
    pub baseline_ap: f64,
    pub n_samples: usize,
    pub n_fp: usize,
}

pub fn learn_logistic(samples: &[JoinedSample], k_folds: usize) -> Option<LogisticResult> {
    let n = samples.len();
    let n_fp = samples.iter().filter(|s| s.is_fp).count();
    let n_tp = n - n_fp;

    if n < MIN_SAMPLES_FOR_LOGISTIC {
        return None;
    }
    if n_fp.min(n_tp) < MIN_CLASS_COUNT {
        return None;
    }

    let baseline_ap = n_fp as f64 / n as f64;

    // GroupKFold by family
    let families: Vec<&str> = samples.iter().map(|s| s.family.as_str()).collect();
    let folds = group_k_fold(&families, k_folds);

    // Collect OOF predictions and per-fold feature selections
    let mut oof_predictions: Vec<(f64, bool)> = vec![(0.0, false); n];
    let mut fold_selected_features: Vec<Vec<usize>> = Vec::new();

    for fold_idx in 0..k_folds {
        let train_indices: Vec<usize> = (0..n).filter(|i| folds[*i] != fold_idx).collect();
        let val_indices: Vec<usize> = (0..n).filter(|i| folds[*i] == fold_idx).collect();

        // Compute fold-local stats from training partition feedback
        let train_feedback: Vec<&serde_json::Value> = Vec::new(); // placeholder — see step 4
        let fold_stats = compute_fold_local_stats_from_samples(
            &train_indices.iter().map(|&i| &samples[i]).collect::<Vec<_>>(),
        );

        // Extract features for train and val
        let train_features: Vec<(ExpandedFeatures, bool)> = train_indices
            .iter()
            .map(|&i| extract_single_expanded(&samples[i], &fold_stats))
            .collect();
        let val_features: Vec<(ExpandedFeatures, bool)> = val_indices
            .iter()
            .map(|&i| extract_single_expanded(&samples[i], &fold_stats))
            .collect();

        // Univariate screening on train
        let selected = univariate_screen(&train_features, baseline_ap);
        if selected.len() < 2 {
            return None;
        }
        fold_selected_features.push(selected.clone());

        // Build feature matrices (selected features only)
        let train_x: Vec<Vec<f64>> = train_features
            .iter()
            .map(|(f, _)| {
                let v = f.to_vec();
                selected.iter().map(|&idx| v[idx]).collect()
            })
            .collect();
        let train_y: Vec<bool> = train_features.iter().map(|(_, l)| *l).collect();
        let val_x: Vec<Vec<f64>> = val_features
            .iter()
            .map(|(f, _)| {
                let v = f.to_vec();
                selected.iter().map(|&idx| v[idx]).collect()
            })
            .collect();

        // Lambda selection via validation AP
        let mut best_lambda = 1.0;
        let mut best_val_ap = 0.0;
        for &lambda in LAMBDA_GRID {
            let fit = crate::logistic::fit(&train_x, &train_y, lambda, MAX_LOGISTIC_ITER);
            let val_preds = fit.predict(&val_x);
            let val_scored: Vec<(f64, bool)> = val_preds
                .into_iter()
                .zip(val_features.iter().map(|(_, l)| *l))
                .collect();
            let ap = crate::metrics::average_precision_stepwise(&val_scored);
            if ap > best_val_ap {
                best_val_ap = ap;
                best_lambda = lambda;
            }
        }

        // Refit with best lambda and produce OOF predictions
        let fit = crate::logistic::fit(&train_x, &train_y, best_lambda, MAX_LOGISTIC_ITER);
        let val_preds = fit.predict(&val_x);
        for (i, &val_idx) in val_indices.iter().enumerate() {
            oof_predictions[val_idx] = (val_preds[i], samples[val_idx].is_fp);
        }
    }

    // Consensus feature selection: features selected in >= 3/5 folds
    let consensus_threshold = (k_folds + 1) / 2 + 1; // 3 for 5-fold
    let mut feature_votes = vec![0usize; 15];
    for selected in &fold_selected_features {
        for &idx in selected {
            feature_votes[idx] += 1;
        }
    }
    let consensus_features: Vec<usize> = (0..15)
        .filter(|&i| feature_votes[i] >= consensus_threshold)
        .collect();
    if consensus_features.len() < 2 {
        return None;
    }

    // Aggregated OOF metrics
    let ap_score = crate::metrics::average_precision_stepwise(&oof_predictions);
    let fp_recall = crate::metrics::fp_recall_at_tp_recall(&oof_predictions, 0.99);

    if ap_score <= baseline_ap + 0.02 {
        return None;
    }

    // Production model: retrain on 100% with consensus features
    let full_stats = compute_fold_local_stats_from_samples(
        &samples.iter().collect::<Vec<_>>(),
    );
    let all_features: Vec<(ExpandedFeatures, bool)> = samples
        .iter()
        .map(|s| extract_single_expanded(s, &full_stats))
        .collect();
    let full_x: Vec<Vec<f64>> = all_features
        .iter()
        .map(|(f, _)| {
            let v = f.to_vec();
            consensus_features.iter().map(|&idx| v[idx]).collect()
        })
        .collect();
    let full_y: Vec<bool> = all_features.iter().map(|(_, l)| *l).collect();

    // Use best lambda from last fold (or median — simple approach: 1.0)
    let production_fit = crate::logistic::fit(&full_x, &full_y, 1.0, MAX_LOGISTIC_ITER);

    // Threshold selection
    let full_preds = production_fit.predict(&full_x);
    let tp_preds: Vec<f64> = full_preds
        .iter()
        .zip(full_y.iter())
        .filter(|(_, &is_fp)| !is_fp)
        .map(|(&p, _)| p)
        .collect();
    let fp_preds: Vec<f64> = full_preds
        .iter()
        .zip(full_y.iter())
        .filter(|(_, &is_fp)| *is_fp)
        .map(|(&p, _)| p)
        .collect();

    // Suppress: 1st percentile of TP predictions (99% TP safety)
    let mut tp_sorted = tp_preds.clone();
    tp_sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let suppress_idx = (tp_sorted.len() as f64 * 0.01).ceil() as usize;
    let suppress_threshold = tp_sorted.get(suppress_idx).copied().unwrap_or(0.5);

    // Boost: 5th percentile of FP predictions (95% FP safety)
    let mut fp_sorted = fp_preds.clone();
    fp_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let boost_idx = (fp_sorted.len() as f64 * 0.05).ceil() as usize;
    let boost_threshold = fp_sorted.get(boost_idx).copied().unwrap_or(0.1);

    let feature_names = ExpandedFeatures::feature_names();
    let selected_names: Vec<String> = consensus_features
        .iter()
        .map(|&i| feature_names[i].to_string())
        .collect();

    Some(LogisticResult {
        selected_features: consensus_features,
        selected_feature_names: selected_names,
        coefficients: production_fit.coefficients,
        intercept: production_fit.intercept,
        feature_means: production_fit.feature_means,
        feature_stddevs: production_fit.feature_stddevs,
        suppress_threshold,
        boost_threshold,
        ap_score,
        fp_recall_at_99_tp_recall: fp_recall,
        baseline_ap,
        n_samples: n,
        n_fp,
    })
}

/// GroupKFold assignment: samples with the same family go in the same fold.
fn group_k_fold(families: &[&str], k: usize) -> Vec<usize> {
    let mut family_to_fold: HashMap<&str, usize> = HashMap::new();
    let mut next_fold = 0usize;
    families
        .iter()
        .map(|f| {
            *family_to_fold.entry(f).or_insert_with(|| {
                let fold = next_fold % k;
                next_fold += 1;
                fold
            })
        })
        .collect()
}

fn compute_fold_local_stats_from_samples(samples: &[&JoinedSample]) -> FoldLocalStats {
    let total = samples.len();
    let total_fp = samples.iter().filter(|s| s.is_fp).count();
    let global_fp_rate = if total > 0 { total_fp as f64 / total as f64 } else { 0.18 };

    let mut category_counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut severity_counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut model_counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut word_tp: HashMap<String, usize> = HashMap::new();
    let mut word_fp: HashMap<String, usize> = HashMap::new();

    for s in samples {
        let cat = category_counts.entry(s.category.clone()).or_insert((0, 0));
        cat.1 += 1;
        if s.is_fp { cat.0 += 1; }

        let sev = severity_counts.entry(s.severity.clone()).or_insert((0, 0));
        sev.1 += 1;
        if s.is_fp { sev.0 += 1; }

        let m = model_counts.entry(s.model.clone()).or_insert((0, 0));
        m.1 += 1;
        if s.is_fp { m.0 += 1; }

        for word in tokenize_title(&s.title) {
            if s.is_fp {
                *word_fp.entry(word).or_insert(0) += 1;
            } else {
                *word_tp.entry(word).or_insert(0) += 1;
            }
        }
    }

    let category_fp_rates = category_counts
        .iter()
        .map(|(k, (fp, t))| (k.clone(), beta_smoothed_rate(*fp, *t, global_fp_rate)))
        .collect();
    let severity_fp_rates = severity_counts
        .iter()
        .map(|(k, (fp, t))| (k.clone(), beta_smoothed_rate(*fp, *t, global_fp_rate)))
        .collect();
    let model_fp_rates = model_counts
        .iter()
        .map(|(k, (fp, t))| (k.clone(), beta_smoothed_rate(*fp, *t, global_fp_rate)))
        .collect();

    let all_words: HashSet<String> = word_tp.keys().chain(word_fp.keys()).cloned().collect();
    let tp_total = total - total_fp;
    let word_lor: HashMap<String, f64> = all_words
        .into_iter()
        .filter_map(|word| {
            let tp_c = word_tp.get(&word).copied().unwrap_or(0);
            let fp_c = word_fp.get(&word).copied().unwrap_or(0);
            if tp_c + fp_c < 5 { return None; }
            let tp_rate = (tp_c as f64 + 0.5) / (tp_total as f64 + 1.0);
            let fp_rate = (fp_c as f64 + 0.5) / (total_fp as f64 + 1.0);
            Some((word, (fp_rate / tp_rate).ln()))
        })
        .collect();

    FoldLocalStats { category_fp_rates, severity_fp_rates, model_fp_rates, word_lor, global_fp_rate }
}

fn extract_single_expanded(s: &JoinedSample, stats: &FoldLocalStats) -> (ExpandedFeatures, bool) {
    let words = tokenize_title(&s.title);
    let word_lors: Vec<f64> = words
        .iter()
        .filter_map(|w| stats.word_lor.get(w).copied())
        .collect();

    let features = ExpandedFeatures {
        log1p_tp_weight: s.tp_weight.ln_1p(),
        log1p_fp_weight: s.fp_weight.ln_1p(),
        precedent_count: (s.precedent_count as f64).min(10.0),
        max_similarity: s.max_similarity,
        mean_similarity: s.mean_similarity,
        has_no_precedents: if s.precedent_count == 0 { 1.0 } else { 0.0 },
        log1p_soft_fp_weight: s.soft_fp_weight.ln_1p(),
        log1p_full_suppress_weight: s.full_suppress_weight.ln_1p(),
        log1p_wontfix_weight: s.wontfix_weight.ln_1p(),
        category_fp_rate: stats.category_fp_rates.get(&s.category).copied().unwrap_or(stats.global_fp_rate),
        severity_fp_rate: stats.severity_fp_rates.get(&s.severity).copied().unwrap_or(stats.global_fp_rate),
        model_fp_rate: stats.model_fp_rates.get(&s.model).copied().unwrap_or(stats.global_fp_rate),
        max_word_lor: word_lors.iter().copied().fold(0.0_f64, f64::max),
        min_word_lor: word_lors.iter().copied().fold(0.0_f64, f64::min),
        count_negative_lor_tokens: word_lors.iter().filter(|&&v| v < -0.5).count() as f64,
    };
    (features, s.is_fp)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum learn_logistic`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): cross-validated logistic training with GroupKFold and lambda grid"
```

---

### Task 7: `LogisticModel` Storage

**Files:**
- Modify: `src/calibrator_model.rs`

Add `LogisticModel` struct with TOML serialization, stored as `[logistic_model]` section.

- [ ] **Step 1: Write failing test for TOML roundtrip**

```rust
#[test]
fn logistic_model_toml_roundtrip() {
    let lm = LogisticModel {
        computed_at: "2026-05-16T12:00:00Z".to_string(),
        n_samples: 1567,
        n_fp: 279,
        selected_features: vec!["log1p_fp_weight".to_string(), "category_fp_rate".to_string()],
        coefficients: vec![0.42, -1.3],
        intercept: -1.2,
        feature_means: vec![0.5, 0.27],
        feature_stddevs: vec![0.3, 0.15],
        suppress_threshold: 0.45,
        boost_threshold: 0.08,
        ap_score: 0.67,
        fp_recall_at_99_tp_recall: 0.35,
        baseline_ap: 0.18,
    };
    let model = CalibratorModel {
        meta: ModelMeta::default(),
        weights: ScoreWeights::default(),
        word_lor: HashMap::new(),
        family_fp_rate: HashMap::new(),
        language_fp_rate: HashMap::new(),
        logistic_model: Some(lm.clone()),
    };
    let toml_str = model.to_toml();
    let parsed = CalibratorModel::from_toml(&toml_str).unwrap();
    let parsed_lm = parsed.logistic_model.unwrap();
    assert_eq!(parsed_lm.n_samples, 1567);
    assert_eq!(parsed_lm.selected_features.len(), 2);
    assert!((parsed_lm.suppress_threshold - 0.45).abs() < 1e-9);
    assert!((parsed_lm.boost_threshold - 0.08).abs() < 1e-9);
    assert!((parsed_lm.coefficients[0] - 0.42).abs() < 1e-9);
}

#[test]
fn logistic_model_absent_is_none() {
    let model = CalibratorModel {
        meta: ModelMeta::default(),
        weights: ScoreWeights::default(),
        word_lor: HashMap::new(),
        family_fp_rate: HashMap::new(),
        language_fp_rate: HashMap::new(),
        logistic_model: None,
    };
    let toml_str = model.to_toml();
    let parsed = CalibratorModel::from_toml(&toml_str).unwrap();
    assert!(parsed.logistic_model.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum logistic_model_toml`
Expected: FAIL

- [ ] **Step 3: Implement `LogisticModel` struct**

Add to `src/calibrator_model.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogisticModel {
    pub computed_at: String,
    pub n_samples: usize,
    pub n_fp: usize,
    pub selected_features: Vec<String>,
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    pub feature_means: Vec<f64>,
    pub feature_stddevs: Vec<f64>,
    pub suppress_threshold: f64,
    pub boost_threshold: f64,
    pub ap_score: f64,
    pub fp_recall_at_99_tp_recall: f64,
    pub baseline_ap: f64,
}
```

Add `logistic_model: Option<LogisticModel>` field to `CalibratorModel` struct. Use `#[serde(skip_serializing_if = "Option::is_none")]` to omit when absent.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum logistic_model_toml`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrator_model.rs
git commit -m "feat(model): add LogisticModel struct with TOML persistence"
```

---

### Task 8: Review-Time Logistic Scoring

**Files:**
- Modify: `src/calibrator.rs`

When `CalibratorConfig` has a logistic model loaded, use it for suppress/boost decisions instead of the composite score path.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn logistic_scoring_suppresses_high_pfp() {
    let lm = LogisticModel {
        computed_at: "2026-05-16T12:00:00Z".to_string(),
        n_samples: 1567,
        n_fp: 279,
        selected_features: vec!["log1p_fp_weight".to_string()],
        coefficients: vec![2.0],
        intercept: 0.0,
        feature_means: vec![0.5],
        feature_stddevs: vec![0.3],
        suppress_threshold: 0.6,
        boost_threshold: 0.1,
        ap_score: 0.67,
        fp_recall_at_99_tp_recall: 0.35,
        baseline_ap: 0.18,
    };
    // A finding with high fp_weight should get P(FP) > suppress_threshold
    let features = ReviewFeatures {
        log1p_fp_weight: 2.0, // well above mean
        ..ReviewFeatures::zeros()
    };
    let p_fp = logistic_score(&lm, &features);
    assert!(p_fp > 0.6);
}

#[test]
fn logistic_scoring_boosts_low_pfp() {
    let lm = LogisticModel {
        computed_at: "2026-05-16T12:00:00Z".to_string(),
        n_samples: 1567,
        n_fp: 279,
        selected_features: vec!["log1p_tp_weight".to_string()],
        coefficients: vec![-2.0], // negative = more TP weight → lower P(FP)
        intercept: 0.0,
        feature_means: vec![0.5],
        feature_stddevs: vec![0.3],
        suppress_threshold: 0.6,
        boost_threshold: 0.1,
        ap_score: 0.67,
        fp_recall_at_99_tp_recall: 0.35,
        baseline_ap: 0.18,
    };
    let features = ReviewFeatures {
        log1p_tp_weight: 2.0,
        ..ReviewFeatures::zeros()
    };
    let p_fp = logistic_score(&lm, &features);
    assert!(p_fp < 0.1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum logistic_scoring`
Expected: FAIL

- [ ] **Step 3: Implement `logistic_score` and integrate into `calibrate_core_decision`**

```rust
/// Feature vector computed at review time for logistic model scoring.
/// Same 15 fields as ExpandedFeatures but computed from runtime data.
pub struct ReviewFeatures {
    pub log1p_tp_weight: f64,
    pub log1p_fp_weight: f64,
    pub precedent_count: f64,
    pub max_similarity: f64,
    pub mean_similarity: f64,
    pub has_no_precedents: f64,
    pub log1p_soft_fp_weight: f64,
    pub log1p_full_suppress_weight: f64,
    pub log1p_wontfix_weight: f64,
    pub category_fp_rate: f64,
    pub severity_fp_rate: f64,
    pub model_fp_rate: f64,
    pub max_word_lor: f64,
    pub min_word_lor: f64,
    pub count_negative_lor_tokens: f64,
}

impl ReviewFeatures {
    pub fn zeros() -> Self {
        Self {
            log1p_tp_weight: 0.0, log1p_fp_weight: 0.0, precedent_count: 0.0,
            max_similarity: 0.0, mean_similarity: 0.0, has_no_precedents: 1.0,
            log1p_soft_fp_weight: 0.0, log1p_full_suppress_weight: 0.0,
            log1p_wontfix_weight: 0.0, category_fp_rate: 0.0, severity_fp_rate: 0.0,
            model_fp_rate: 0.0, max_word_lor: 0.0, min_word_lor: 0.0,
            count_negative_lor_tokens: 0.0,
        }
    }

    fn get_by_name(&self, name: &str) -> f64 {
        match name {
            "log1p_tp_weight" => self.log1p_tp_weight,
            "log1p_fp_weight" => self.log1p_fp_weight,
            "precedent_count" => self.precedent_count,
            "max_similarity" => self.max_similarity,
            "mean_similarity" => self.mean_similarity,
            "has_no_precedents" => self.has_no_precedents,
            "log1p_soft_fp_weight" => self.log1p_soft_fp_weight,
            "log1p_full_suppress_weight" => self.log1p_full_suppress_weight,
            "log1p_wontfix_weight" => self.log1p_wontfix_weight,
            "category_fp_rate" => self.category_fp_rate,
            "severity_fp_rate" => self.severity_fp_rate,
            "model_fp_rate" => self.model_fp_rate,
            "max_word_lor" => self.max_word_lor,
            "min_word_lor" => self.min_word_lor,
            "count_negative_lor_tokens" => self.count_negative_lor_tokens,
            _ => 0.0,
        }
    }
}

/// Compute P(FP) using the stored logistic model.
pub fn logistic_score(model: &LogisticModel, features: &ReviewFeatures) -> f64 {
    let mut logit = model.intercept;
    for (i, feat_name) in model.selected_features.iter().enumerate() {
        let raw = features.get_by_name(feat_name);
        let normed = (raw - model.feature_means[i]) / (model.feature_stddevs[i] + 1e-8);
        logit += model.coefficients[i] * normed;
    }
    crate::logistic::sigmoid(logit)
}
```

Then modify `calibrate_core_decision` to check for logistic model before the existing composite path:

```rust
// Logistic model path: when loaded, use P(FP) for suppress/boost decisions.
if let Some(ref logistic) = config.logistic_model {
    let review_features = compute_review_features(/* ... */);
    let p_fp = logistic_score(logistic, &review_features);
    
    // Record both scores in trace for A/B comparison
    trace.logistic_p_fp = Some(p_fp);
    trace.composite_score = composite;
    
    if p_fp > logistic.suppress_threshold && fp_weight > 0.0 {
        // Suppress
        finding.calibrator_action = Some(CalibratorAction::Disputed);
        suppressed = true;
        return CoreDecision { suppressed, boosted, trace };
    }
    if p_fp < logistic.boost_threshold && tp_weight > 0.0 && config.boost_tp {
        // Boost
        let proposed = boost_severity(&finding.severity);
        // ... rubric gate same as existing ...
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum logistic_scoring`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): review-time logistic scoring with dual thresholds"
```

---

### Task 9: CLI Integration (`--feature-importance` and Upgraded `--learn-weights`)

**Files:**
- Modify: `src/cli/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add `--feature-importance` flag to CLI**

In `src/cli/mod.rs`, add to the calibrate subcommand:

```rust
#[arg(long, help = "Show per-feature importance diagnostics (univariate AP)")]
pub feature_importance: bool,
```

- [ ] **Step 2: Wire up `--feature-importance` in `src/main.rs`**

After `extract_join_features`, add:

```rust
if args.feature_importance {
    let joined = extract_joined_samples(&feedback, &traces, &model, &filter, disable_fuzzy);
    let stats = compute_fold_local_stats_from_samples(&joined.iter().collect::<Vec<_>>());
    let expanded = joined.iter().map(|s| extract_single_expanded(s, &stats)).collect::<Vec<_>>();
    let n_fp = expanded.iter().filter(|(_, l)| *l).count();
    let n_tp = expanded.len() - n_fp;
    let baseline = n_fp as f64 / expanded.len() as f64;
    
    eprintln!("Feature importance ({} FP, {} non-FP):", n_fp, n_tp);
    let names = ExpandedFeatures::feature_names();
    let mut importances: Vec<(usize, f64)> = (0..15)
        .map(|i| {
            let univariate: Vec<(f64, bool)> = expanded.iter().map(|(f, l)| (f.to_vec()[i], *l)).collect();
            (i, crate::metrics::average_precision_stepwise(&univariate))
        })
        .collect();
    importances.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    
    for (idx, ap) in &importances {
        let lift = ap - baseline;
        let marker = if lift < 0.02 { "  [below threshold]" } else { "" };
        eprintln!("  {:30} AP={:.4}  (lift {:+.4}){}", names[*idx], ap, lift, marker);
    }
    return Ok(());
}
```

- [ ] **Step 3: Upgrade `--learn-weights` to use logistic pipeline**

Replace the grid search flow in `src/main.rs` with:

```rust
if args.learn_weights {
    let joined = extract_joined_samples(&feedback, &traces, &model, &filter, disable_fuzzy);
    
    // Attempt logistic model first
    match learn_logistic(&joined, 5) {
        Some(result) => {
            eprintln!("Logistic model ({} FP, {} non-FP, 5-fold GroupKFold):", result.n_fp, result.n_samples - result.n_fp);
            eprintln!("  Selected features ({}/15): {:?}", result.selected_feature_names.len(), result.selected_feature_names);
            eprintln!("  AP (OOF):    {:.4}", result.ap_score);
            eprintln!("  AP (baseline): {:.4}", result.baseline_ap);
            eprintln!("  FP recall @ 99% TP recall: {:.4}", result.fp_recall_at_99_tp_recall);
            eprintln!("  Suppress threshold: {:.4}", result.suppress_threshold);
            eprintln!("  Boost threshold: {:.4}", result.boost_threshold);
            
            // Store logistic model
            let lm = LogisticModel {
                computed_at: chrono::Utc::now().to_rfc3339(),
                n_samples: result.n_samples,
                n_fp: result.n_fp,
                selected_features: result.selected_feature_names,
                coefficients: result.coefficients,
                intercept: result.intercept,
                feature_means: result.feature_means,
                feature_stddevs: result.feature_stddevs,
                suppress_threshold: result.suppress_threshold,
                boost_threshold: result.boost_threshold,
                ap_score: result.ap_score,
                fp_recall_at_99_tp_recall: result.fp_recall_at_99_tp_recall,
                baseline_ap: result.baseline_ap,
            };
            model.logistic_model = Some(lm);
            eprintln!("  -> Logistic model written to calibrator_model.toml");
        }
        None => {
            eprintln!("  Logistic model: insufficient data or no improvement over baseline.");
            eprintln!("  Falling back to grid search...");
            // existing grid search logic as fallback
        }
    }
}
```

- [ ] **Step 4: Test the CLI integration**

Run: `cargo build --bin quorum && target/debug/quorum calibrate --feature-importance 2>&1 | head -20`
Expected: Feature importance table with 15 rows

- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "feat(cli): --feature-importance diagnostics and logistic --learn-weights upgrade"
```

---

### Task 10: Joined Sample Extraction (`extract_joined_samples`)

**Files:**
- Modify: `src/calibrate.rs`

Bridge function that walks the same multi-tier join as `extract_join_features` but produces `JoinedSample` structs with the full metadata needed for expanded feature computation (category, severity, model, soft_fp_weight, precedent_count, etc.).

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn extract_joined_samples_captures_metadata() {
    let traces = make_test_traces_with_metadata();
    let feedback = make_test_feedback_with_categories();
    let model = CalibratorModel::default();
    let filter = JoinFilter::default();
    
    let samples = extract_joined_samples(&feedback, &traces, &model, &filter, false);
    assert!(!samples.is_empty());
    for s in &samples {
        assert!(!s.title.is_empty());
        assert!(!s.category.is_empty());
        assert!(!s.severity.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum extract_joined_samples_captures`
Expected: FAIL

- [ ] **Step 3: Implement `extract_joined_samples`**

This function walks the same multi-tier index logic as existing `extract_join_features` but extracts all metadata fields. The trace entries contain `finding_category`, `input_severity`, `model` (from review telemetry). For missing metadata, use sensible defaults:

```rust
pub fn extract_joined_samples(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
    model: &CalibratorModel,
    filter: &JoinFilter,
    disable_fuzzy: bool,
) -> Vec<JoinedSample> {
    // ... same index-building and matching logic as extract_join_features ...
    // But for each match, produce a JoinedSample with:
    //   title, category (from trace or "unknown"), severity (from trace or "medium"),
    //   model (from trace or "unknown"), tp_weight, fp_weight,
    //   soft_fp_weight (from trace["soft_fp_weight"]),
    //   full_suppress_weight (= fp_weight, matching calibrator logic),
    //   wontfix_weight (from trace["wontfix_weight"]),
    //   precedent_count (from trace["precedent_count"] or inferred from weight magnitude),
    //   max_similarity (from trace["max_similarity"] or 0.0),
    //   mean_similarity (from trace["mean_similarity"] or 0.0),
    //   is_fp (verdict == "fp"),
    //   family (CalibratorModel::title_family(&title))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum extract_joined_samples`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/calibrate.rs
git commit -m "feat(calibrate): extract_joined_samples with full metadata for expanded features"
```

---

### Task 11: Backfill Validation

**Files:**
- Modify: `src/main.rs` (or add as integration test)

Validate that the existing ~1567-sample feedback corpus can successfully produce expanded features and train a logistic model. This exercises the full pipeline end-to-end against real data.

- [ ] **Step 1: Add integration test that loads real corpus**

```rust
#[test]
#[ignore] // requires real feedback file at ~/.quorum/feedback.jsonl
fn backfill_real_corpus_produces_logistic_model() {
    let feedback_path = dirs::home_dir().unwrap().join(".quorum/feedback.jsonl");
    if !feedback_path.exists() {
        return; // skip in CI
    }
    let feedback = load_feedback_jsonl(&feedback_path);
    let traces_path = dirs::home_dir().unwrap().join(".quorum/traces.jsonl");
    let traces = load_traces_jsonl(&traces_path);
    let model = compute_calibrator_model(&feedback).unwrap();
    let filter = JoinFilter::default();
    
    let joined = extract_joined_samples(&feedback, &traces, &model, &filter, false);
    eprintln!("Joined samples: {} ({} FP)", joined.len(), joined.iter().filter(|s| s.is_fp).count());
    assert!(joined.len() >= 200);
    
    let result = learn_logistic(&joined, 5);
    assert!(result.is_some(), "Logistic model should succeed on real corpus");
    let r = result.unwrap();
    eprintln!("AP: {:.4} (baseline {:.4}, lift {:.4})", r.ap_score, r.baseline_ap, r.ap_score - r.baseline_ap);
    eprintln!("FP recall @ 99% TP recall: {:.4}", r.fp_recall_at_99_tp_recall);
    eprintln!("Selected: {:?}", r.selected_feature_names);
    eprintln!("Suppress threshold: {:.4}", r.suppress_threshold);
    eprintln!("Boost threshold: {:.4}", r.boost_threshold);
    
    assert!(r.ap_score > r.baseline_ap + 0.02);
    assert!(r.selected_feature_names.len() >= 2);
}
```

- [ ] **Step 2: Run the backfill validation**

Run: `cargo test --bin quorum backfill_real_corpus -- --ignored --nocapture`
Expected: PASS, prints metrics showing lift over baseline

- [ ] **Step 3: Run the CLI end-to-end**

Run: `cargo run -- calibrate --feature-importance`
Run: `cargo run -- calibrate --learn-weights`
Expected: Both produce output, learn-weights writes model to `~/.quorum/calibrator_model.toml`

- [ ] **Step 4: Verify model file was written correctly**

Run: `grep -A5 '\[logistic_model\]' ~/.quorum/calibrator_model.toml`
Expected: Shows selected_features, coefficients, thresholds

- [ ] **Step 5: Commit integration test**

```bash
git add tests/
git commit -m "test: add backfill validation for logistic calibrator on real corpus"
```

---

### Task 12: Trace Enrichment for A/B Comparison

**Files:**
- Modify: `src/calibrator_trace.rs`

Add `logistic_p_fp: Option<f64>` to `CalibratorTraceEntry` so both old composite and new logistic scores are visible in trace output for validation.

- [ ] **Step 1: Add field to trace struct**

```rust
pub logistic_p_fp: Option<f64>,
```

With `#[serde(skip_serializing_if = "Option::is_none")]`.

- [ ] **Step 2: Populate in `calibrate_core_decision`**

When logistic model is present, set `trace.logistic_p_fp = Some(p_fp)` before returning.

- [ ] **Step 3: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/calibrator_trace.rs src/calibrator.rs
git commit -m "feat(trace): emit logistic_p_fp alongside composite_score for A/B comparison"
```

---

### Task 13: Final Verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass (existing + new)

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --bin quorum -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Run release build**

Run: `cargo build --release`
Expected: Compiles successfully

- [ ] **Step 4: Run backfill on real data**

Run: `cargo run --release -- calibrate --learn-weights`
Expected: Prints logistic model metrics, writes to calibrator_model.toml

- [ ] **Step 5: Run a review with the new model**

Run: `cargo run --release -- review src/calibrate.rs --trace`
Then check trace: `tail -1 ~/.quorum/trace.jsonl | jq '.logistic_p_fp'`
Expected: Trace entries contain `logistic_p_fp` field

- [ ] **Step 6: Final commit (if any fixups needed)**

```bash
git add -u
git commit -m "fix: address clippy/test issues from final verification"
```

---

## Task Dependency Graph

```
Task 1 (metrics) ──┐
                    ├─→ Task 5 (screening) ──→ Task 6 (orchestrator) ──→ Task 9 (CLI)
Task 2 (logistic) ─┘                                                         │
                                                                              ▼
Task 3 (struct) ──→ Task 4 (extraction) ──→ Task 10 (join bridge) ──→ Task 11 (backfill)
                                                                              │
Task 7 (model storage) ──→ Task 8 (review-time scoring) ──→ Task 12 (trace) ──→ Task 13 (verify)
```

Tasks 1, 2, 3, 7 can be done in parallel. Tasks 4 and 5 depend on 3 and 1 respectively. Task 6 depends on 1, 2, 4, 5. Tasks 8-13 are sequential.
