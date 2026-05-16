# Logistic Calibrator Scoring — Design Spec

**Date:** 2026-05-16
**Status:** Approved
**Issue:** Continuation of #312 (learned weights)

## Problem

The grid-search weight learner (`quorum calibrate --learn-weights`) cannot beat the trivial baseline on the current feedback corpus (1567 samples, 82% TP). Root causes:

1. **Metric artifact**: Trapezoidal PR-AUC inflates the trivial classifier to 0.911 at 82% prevalence.
2. **Feature poverty**: 4 features (precedent ratio, avg word_lor, family FP inv, language FP inv) lack discriminative power. The ratio compresses evidence strength.
3. **Model poverty**: Coarse grid search over 4 weights is too blunt.

## Solution

Replace the grid-search approach with an inline L2-regularized logistic regression over ~15 expanded features, modeling P(FP|features) (label flip). Per-fold univariate screening selects the features that carry signal.

## Design

### 1. Label Flip

The model predicts P(FP). Positive class = FP (18% prevalence after flip). This gives massive headroom over baseline AP (~0.18 vs prior 0.91).

### 2. Expanded Feature Set (15 candidates)

All computed during `extract_join_features`. Per-fold univariate screening (AP ≥ baseline + 0.02) typically retains 8-12.

**Precedent decomposition (6):**
- `log1p_tp_weight`: log1p of accumulated TP precedent weight
- `log1p_fp_weight`: log1p of accumulated FP precedent weight
- `precedent_count`: number of matched precedents (capped at 10)
- `max_similarity`: highest similarity score among matched precedents
- `mean_similarity`: mean similarity across matched precedents (0.0 if none)
- `has_no_precedents`: 1.0 if no precedents matched, 0.0 otherwise

**Weight accumulators (3):**
- `log1p_soft_fp_weight`: log1p of soft FP weight from trace
- `log1p_full_suppress_weight`: log1p of full suppress weight
- `log1p_wontfix_weight`: log1p of wontfix weight

**Smoothed priors (3):**
- `category_fp_rate`: Beta-smoothed FP rate for finding_category (α=5)
- `severity_fp_rate`: Beta-smoothed FP rate for input_severity
- `model_fp_rate`: Beta-smoothed FP rate for generating model

**Text statistics (3):**
- `max_word_lor`: maximum word log-odds ratio in title
- `min_word_lor`: minimum word log-odds ratio in title
- `count_negative_lor_tokens`: count of tokens with LOR < -0.5

### 3. Logistic Regression (~200 LOC, src/logistic.rs)

Inline implementation, no external dependencies.

- **Standardization**: z-score normalization (mean/stddev per feature, computed from training fold). Division-safe: use `stddev + 1e-8` to handle constant features.
- **Training**: Batch gradient descent with L2 regularization and backtracking line search (Armijo condition) for step size selection.
- **Lambda selection**: Small grid {0.01, 0.1, 1.0, 10.0}, best by validation AP (Average Precision, not operational metric — see §7).
- **Convergence**: max 500 iterations, early stop when relative loss change < 1e-4 (`(prev_loss - loss) / prev_loss`).
- **Output**: coefficient vector, intercept, feature means/stddevs, selected feature indices

### 4. Per-Fold Feature Selection

Inside each CV fold's training partition:
1. Compute univariate AP (FP-positive) for each of the 15 features
2. Retain features where AP ≥ baseline_ap + 0.02 (baseline = class prevalence in fold)
3. Train logistic regression on retained features only
4. Report selected features and their univariate APs

### 5. Fold-Local Feature Computation

All target-encoded features recomputed per training fold to prevent leakage:
- `category_fp_rate`: computed from train fold's feedback only
- `severity_fp_rate`: computed from train fold's feedback only
- `model_fp_rate`: computed from train fold's feedback only
- `word_lor` vocabulary: computed from train fold's feedback only
- `family_fp_rate` / `language_fp_rate`: computed from train fold's traces only

### 6. Cross-Validation

- 5-fold GroupKFold by title family (avoids near-duplicate leakage)
- Each fold: univariate screen → fit logistic → evaluate on held-out fold
- Stability check: feature selection agreement across folds (≥3/5 folds must select a feature for it to be in the final model)
- **Separation of concerns**: CV phase produces out-of-fold predictions for evaluation only. Production model is retrained on 100% of data with the consensus feature set.

### 7. Metrics

- **Fold-level evaluation (for lambda selection):** Average Precision (stepwise, FP-positive). NOT the operational metric — per-fold TP counts (~257) have only ~2 TPs of margin at 99% recall, making fold-level operational metrics too volatile.
- **Aggregated OOF metric (reported):** FP recall at TP recall ≥ 99%, computed on concatenated out-of-fold predictions across all 5 folds.
- **Secondary:** Average Precision (stepwise, FP-positive)
- **Safety:** TP false-suppression rate
- **Baseline comparison:** all metrics vs trivial classifier (predict all negative in FP-positive framing)

### 8. Model Storage (calibrator_model.toml)

New section `[logistic_model]`:

```toml
[logistic_model]
computed_at = "2026-05-16T..."
n_samples = 1567
n_fp = 279
selected_features = ["log1p_fp_weight", "category_fp_rate", "max_similarity", ...]
coefficients = [0.42, -1.3, 0.88, ...]
intercept = -1.2
feature_means = [0.5, 0.27, 0.6, ...]
feature_stddevs = [0.3, 0.15, 0.25, ...]
suppress_threshold = 0.45
boost_threshold = 0.08
ap_score = 0.67
fp_recall_at_99_tp_recall = 0.35
baseline_ap = 0.18
```

### 9. Threshold Selection

During the production training phase (on 100% of data):
1. Compute P(FP) for all training samples
2. **Suppress threshold**: Sort TP samples by their P(FP) descending. Pick threshold at the 1st percentile of TP predictions (ensures 99% TP recall — at most 1% of TPs will be falsely suppressed).
3. **Boost threshold**: Sort FP samples by their P(FP) ascending. Pick threshold at the 5th percentile of FP predictions (ensures 95% of FPs will not be falsely boosted).
4. Store both thresholds in the model file

### 10. Review-Time Scoring (src/calibrator.rs)

When `CalibratorModel` has a `logistic_model` section:
1. Compute the selected features for the finding (same extraction logic)
2. Standardize using stored means/stddevs (+ 1e-8 for division safety)
3. Compute logit = dot(coefficients, features) + intercept
4. P(FP) = sigmoid(logit)
5. If P(FP) > suppress_threshold → suppress (severity → Info, action → Suppressed)
6. If P(FP) < boost_threshold → boost (severity upgraded one level, e.g. medium → high)
7. Also emit the old composite score in the trace for A/B comparison

When no logistic model exists → fall back to existing composite scoring (unchanged behavior).

### 10. CLI Surface

```
quorum calibrate --learn-weights      # existing flag, upgraded behavior
quorum calibrate --feature-importance  # new: univariate diagnostics only
```

`--feature-importance` output:
```
Feature importance (279 FP, 1288 non-FP):
  log1p_fp_weight:       AP=0.42  (lift +0.24)
  category_fp_rate:      AP=0.38  (lift +0.20)
  max_similarity:        AP=0.31  (lift +0.13)
  ...
  language_fp_inv:       AP=0.19  (lift +0.01)  [below threshold]
```

`--learn-weights` output:
```
Logistic model (279 FP, 1288 non-FP, 5-fold GroupKFold):
  Selected features (9/15): [...]
  Lambda: 1.0
  AP (full):     0.67
  AP (baseline): 0.18
  AP (5-fold):   0.61
  FP recall @ 99% TP recall: 0.35
  -> Model written to ~/.quorum/calibrator_model.toml
```

### 11. Fallback & Safety

- If fewer than 200 joined samples: skip learning, print "insufficient data" diagnostic
- If min(FP count, TP count) < 30: skip learning (need both classes represented)
- If best AP ≤ baseline + 0.02: refuse to write model, print diagnostic
- If feature selection retains < 2 features: refuse to write model
- Missing model file at review time: silent fallback to composite scoring
- Malformed model file: warn + fallback

### 12. Files Changed

| File | Change |
|------|--------|
| `src/logistic.rs` | New: logistic regression implementation |
| `src/calibrate.rs` | Expanded `SampleFeatures`, fold-local computation, feature screening, `--feature-importance` |
| `src/calibrator_model.rs` | `LogisticModel` struct, serialization |
| `src/calibrator.rs` | Review-time logistic scoring path |
| `src/cli/mod.rs` | `--feature-importance` flag |
| `src/main.rs` | Wire up feature importance + upgraded learn-weights |
| `src/metrics.rs` | `average_precision()` (stepwise), `fp_recall_at_tp_recall()` |

### 13. Not In Scope

- Online learning / incremental updates (future)
- Non-linear models (decision trees, etc.)
- Feature interactions as explicit terms
- Automated lambda tuning beyond the 4-point grid
