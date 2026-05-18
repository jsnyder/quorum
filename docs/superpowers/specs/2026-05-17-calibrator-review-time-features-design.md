# Calibrator Review-Time Feature Fix

## Problem

The logistic calibrator has 22 features in `ExpandedFeatures`. During training, all 22 are populated from the feedback corpus. At review time (`calibrate_core_decision`), 6 features are broken:

| Feature | Training value | Review-time value | Problem |
|---------|---------------|-------------------|---------|
| `log1p_full_suppress_weight` | `(fp + soft_fp + wontfix).ln_1p()` | `fp_weight.ln_1p()` | Duplicate of `log1p_fp_weight` |
| `severity_fp_rate` | Beta-smoothed FP rate per severity | `0.0` | Dead |
| `model_fp_rate` | Beta-smoothed FP rate per LLM model | `0.0` | Dead |
| `finding_count_same_file` | `ln_1p(corpus file occurrence count)` | `0.0` | Dead |
| `file_fp_rate` | Beta-smoothed FP rate per file path | `0.0` | Dead |
| `finding_span_lines` | `(span_lines as f64).ln_1p()` | `span_lines as f64` (raw) | Wrong scale |

Additionally, the trace emission at review time writes `fp_weight` as `full_suppress_weight`, which corrupts future training data — making the duplicate permanent across retrain cycles.

## Secondary Goal: Legacy Threshold Cleanup

The old heuristic threshold system (`calibrator_thresholds.toml` with `PathThreshold` suppress/boost using raw composite scores) is fully superseded by the logistic model's P(FP) thresholds. Remove the legacy system:
- Remove `ThresholdConfig` loading and `calibrator_thresholds.toml` file
- Remove the "Calibrator Threshold Report" print section from `quorum calibrate`
- Remove `compute_thresholds()` and `PathThreshold` types
- Remove `suppress_threshold` / `boost_threshold` fields from `CalibratorConfig` (these were the legacy ones; logistic thresholds live on `LogisticModel`)
- Simplify `calibrate_core_decision` to only use logistic model thresholds

## Design

### Part A: Fix `log1p_full_suppress_weight` (calibrator.rs)

**Review-time fix** (line 424):
```rust
// Before:
log1p_full_suppress_weight: fp_weight.ln_1p(),
// After:
log1p_full_suppress_weight: (fp_weight + soft_fp_weight + wontfix_weight).ln_1p(),
```

**Trace emission fix** — all `make_trace_entry` calls pass `fp_weight` as the `full_suppress_weight` argument. Change to pass `fp_weight + soft_fp_weight + wontfix_weight`.

### Part B: Fix `finding_span_lines` (calibrator.rs)

```rust
// Before:
finding_span_lines: (finding.line_end.saturating_sub(finding.line_start) + 1) as f64,
// After:
finding_span_lines: ((finding.line_end.saturating_sub(finding.line_start) + 1) as f64).ln_1p(),
```

### Part C: Add rate maps to `CalibratorModel` (calibrator_model.rs + calibrate.rs)

Add four new optional fields to `CalibratorModel`:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub severity_fp_rate: Option<HashMap<String, f64>>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub model_fp_rate: Option<HashMap<String, f64>>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub file_fp_rate: Option<HashMap<String, f64>>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub file_finding_counts: Option<HashMap<String, usize>>,
```

**Computation** (in `run_calibrate` / training pipeline): After computing `FoldLocalStats` from the full corpus, store the global stats maps in the model before serialization.

**Review-time lookup** (in `calibrate_core_decision`): Look up finding's severity/model/file_path in the model's maps, falling back to `0.0` when the map is `None` (backward compat with old model.toml files). A `tracing::info!` fires once per review when maps are missing, prompting recalibration.

### Part D: Fix `finding_count_same_file` (calibrator.rs)

Use corpus-level file finding counts from `CalibratorModel.file_finding_counts`, not the current review's count. This matches training semantics.

```rust
finding_count_same_file: config.model.as_ref()
    .and_then(|m| m.file_finding_counts.as_ref())
    .and_then(|counts| counts.get(file_path))
    .map(|&c| (c as f64).ln_1p())
    .unwrap_or(0.0),
```

### Part E: Legacy Threshold Removal

Files affected:
- `src/threshold_config.rs` — delete entirely
- `src/calibrate.rs` — remove `compute_thresholds()` function
- `src/calibrator.rs` — remove `suppress_threshold: Option<f64>`, `boost_threshold: Option<f64>`, `force_threshold: Option<f64>` from `CalibratorConfig`; simplify `calibrate_core_decision` to use only `LogisticModel` thresholds
- `src/main.rs` — remove `calibrator_thresholds.toml` loading, remove threshold report output, remove `--suppress-precision` / `--boost-precision` CLI args from calibrate subcommand
- `src/lib.rs` — remove `pub mod threshold_config`

**Migration**: When `calibrator_thresholds.toml` exists, emit a one-time warning that it is no longer used and can be deleted.

### Backward Compatibility

- Old `calibrator_model.toml` files (without rate maps) continue to work — features fall back to `0.0` (same as current behavior). A `tracing::info!` recommends recalibration.
- Old `calibrator_thresholds.toml` is ignored with a warning.
- After upgrading, users run `quorum calibrate` to get the new maps and retrain the logistic model with corrected features.

### Files Modified

| File | Changes |
|------|---------|
| `src/calibrator.rs` | Fix review-time features, fix trace emission, remove legacy threshold fields, simplify decision logic |
| `src/calibrator_model.rs` | Add 4 optional map fields |
| `src/calibrate.rs` | Compute global rate maps for model, remove `compute_thresholds()` |
| `src/main.rs` | Remove threshold report, remove threshold CLI args, remove thresholds.toml loading, add deprecation warning |
| `src/threshold_config.rs` | Delete |
| `src/lib.rs` | Remove `pub mod threshold_config` |

### Testing

- Unit tests for each fixed feature value (verify training-time and review-time produce equivalent values for known inputs)
- Integration test: run calibrate with test corpus, verify new maps appear in serialized model
- Regression test: old model.toml without maps still loads and produces valid (zero-fallback) scores
- Test that legacy threshold file triggers deprecation warning
