# Calibrator Review-Time Feature Fix

## Problem

The logistic calibrator has 22 features in `ExpandedFeatures`. During training, all 22 are populated from the feedback corpus. At review time (`calibrate_core_decision`), 7 features are broken:

| Feature | Training value | Review-time value | Problem |
|---------|---------------|-------------------|---------|
| `log1p_full_suppress_weight` | `(fp + soft_fp + wontfix).ln_1p()` | `fp_weight.ln_1p()` | Duplicate of `log1p_fp_weight` |
| `category_fp_rate` | Beta-smoothed FP rate per category | `family_fp_rate` proxy | Wrong proxy |
| `severity_fp_rate` | Beta-smoothed FP rate per severity | `0.0` | Dead |
| `model_fp_rate` | Beta-smoothed FP rate per LLM model | `0.0` | Dead |
| `finding_count_same_file` | `ln_1p(corpus file occurrence count)` | `0.0` | Dead |
| `file_fp_rate` | Beta-smoothed FP rate per file path | `0.0` | Dead |
| `finding_span_lines` | `(span_lines as f64).ln_1p()` | `span_lines as f64` (raw) | Wrong scale |

Additionally:
- Trace emission at review time writes `fp_weight` as `full_suppress_weight`, corrupting future training data and making the duplicate permanent across retrain cycles.
- Word LOR tokenization differs: inference splits on `!is_alphanumeric()`, training uses `[a-z_]+` regex (drops digits). Fix: reuse `tokenize_title()` at inference.

### Historical corpus contamination

Old traces in `calibrator_traces.jsonl` have `full_suppress_weight == fp_weight` even when `soft_fp_weight > 0` or `wontfix_weight > 0`. After fixing trace emission, a `quorum calibrate` retrain will mix old (wrong) and new (correct) data. This is acceptable — the model will progressively improve as new traces dominate. No backfill required.

## Secondary Goal: Legacy Threshold Cleanup

The old heuristic threshold system (`calibrator_thresholds.toml` with `PathThreshold` suppress/boost using raw composite scores) is fully superseded by the logistic model's P(FP) thresholds. Remove the legacy system:
- Delete `src/threshold_config.rs` and `pub mod threshold_config` from `lib.rs`
- Remove `compute_thresholds()` from `calibrate.rs`
- Remove `suppress_threshold`, `boost_threshold`, `force_threshold` from `CalibratorConfig`
- Remove `calibrator_thresholds.toml` loading and threshold report output from `main.rs`
- Remove `--suppress-precision` / `--boost-precision` CLI args from calibrate subcommand
- Remove the non-logistic threshold decision branches in `calibrate_core_decision` (lines ~564-711)
- When no `LogisticModel` is loaded, the calibrator falls back to the existing heuristic suppress/boost logic that uses raw `tp_weight / (tp_weight + fp_weight)` ratios with hardcoded cutoffs. This path remains as the "no model" fallback.

**Migration**: When `calibrator_thresholds.toml` exists at startup, emit a one-time `tracing::warn` that it is no longer used and can be deleted. Do not load or apply its contents.

## Design

### Part A: Fix `log1p_full_suppress_weight` (calibrator.rs)

**Review-time fix** (line 424):
```rust
let full_suppress_weight = fp_weight + soft_fp_weight + wontfix_weight;
// ...
log1p_full_suppress_weight: full_suppress_weight.ln_1p(),
```

**Trace emission fix** — all `make_trace_entry` calls currently pass `fp_weight` as the `full_suppress_weight` argument. Change to pass `full_suppress_weight` (the sum computed above).

### Part B: Fix `finding_span_lines` (calibrator.rs)

```rust
// Before:
finding_span_lines: (finding.line_end.saturating_sub(finding.line_start) + 1) as f64,
// After:
finding_span_lines: ((finding.line_end.saturating_sub(finding.line_start) + 1) as f64).ln_1p(),
```

### Part C: Fix word LOR tokenization (calibrator.rs)

Replace the inline `split(|c| ...)` tokenizer in `calibrate_core_decision` with a call to `crate::calibrate::tokenize_title()`, which uses `WORD_RE = r"[a-z_]+"`. This ensures training and inference produce identical token sets for word LOR lookups.

### Part D: Add rate maps to `CalibratorModel` (calibrator_model.rs + calibrate.rs)

Add five new optional fields to `CalibratorModel`:
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

Note: `category_fp_rate_map` is named differently from the existing `family_fp_rate` field to avoid confusion. The existing `family_fp_rate` remains for backward compat with the non-logistic scoring path.

**Computation** (in `run_calibrate`): After computing `FoldLocalStats` from the full corpus (the same `all_stats` used for the final production model refit), store the global stats maps in the model before serialization. File path keys are normalized via `normalize_file_path_deep()` before insertion.

**Review-time lookup** (in `calibrate_core_decision`): Look up the finding's category/severity/model/file_path in the model's maps. Fallback semantics match training:
- When map exists but key is missing: fall back to `model.meta.global_fp_rate`
- When map is `None` (old model.toml): fall back to `0.0` (preserves current behavior)
- A `tracing::info_once!` fires when maps are missing, prompting recalibration

### Part E: Fix `finding_count_same_file` (calibrator.rs)

Use corpus-level file finding counts from `CalibratorModel.file_finding_counts`, not the current review's count. This matches training semantics (corpus-level counts, not per-review counts).

```rust
finding_count_same_file: config.model.as_ref()
    .and_then(|m| m.file_finding_counts.as_ref())
    .and_then(|counts| counts.get(&normalized_file_path))
    .map(|&c| (c as f64).ln_1p())
    .unwrap_or(0.0),
```

File path is normalized with `normalize_file_path_deep()` before lookup to match the serialized keys.

### Part F: Legacy Threshold Removal

Files affected:
- `src/threshold_config.rs` — delete entirely
- `src/calibrate.rs` — remove `compute_thresholds()` function and `use crate::threshold_config::*`
- `src/calibrator.rs` — remove `suppress_threshold: Option<f64>`, `boost_threshold: Option<f64>`, `force_threshold: Option<f64>` from `CalibratorConfig`; remove the `sanitize_threshold` calls that reference these fields; remove the non-logistic threshold decision branches; keep the heuristic no-model fallback path
- `src/main.rs` — remove `calibrator_thresholds.toml` loading, remove threshold report output, remove `--suppress-precision` / `--boost-precision` CLI args; add deprecation warning when old file exists
- `src/lib.rs` — remove `pub mod threshold_config`

The no-model fallback (when `config.model.logistic_model` is `None`) continues to use the existing hardcoded heuristic logic for suppress/boost. This is not "legacy thresholds" — it's the baseline calibrator behavior that exists independently of the threshold config system.

### Backward Compatibility

- Old `calibrator_model.toml` without rate maps: maps deserialize as `None`, features fall back to `0.0` (same as current broken behavior). `tracing::info_once!` recommends recalibration.
- Old `calibrator_thresholds.toml`: ignored with a warning. No behavioral change for users who have a logistic model (which superseded these thresholds).
- Users without a logistic model: heuristic calibrator continues to work as before.
- After upgrading, `quorum calibrate` produces the new maps and retrains the model with corrected features.

### Files Modified

| File | Changes |
|------|---------|
| `src/calibrator.rs` | Fix 7 review-time features, fix trace emission, fix tokenization, remove legacy threshold fields, simplify decision logic |
| `src/calibrator_model.rs` | Add 5 optional map fields |
| `src/calibrate.rs` | Compute global rate maps for model, remove `compute_thresholds()`, make `tokenize_title` pub |
| `src/main.rs` | Remove threshold report, remove threshold CLI args, remove thresholds.toml loading, add deprecation warning |
| `src/threshold_config.rs` | Delete |
| `src/lib.rs` | Remove `pub mod threshold_config` |

### Testing

- Unit tests for each fixed feature: verify training-time and review-time produce equivalent values for known inputs (same composite weight, same ln_1p scale, same tokenization, same rate lookups)
- Serde round-trip tests: old TOML without maps loads with `None`; new TOML serializes and deserializes maps correctly
- Integration test: run calibrate with test corpus, verify new maps appear in serialized model
- Regression test: old model.toml without maps still loads and produces valid scores
- Test that legacy threshold file triggers deprecation warning
- Test that `tokenize_title` and inference-time tokenization produce identical tokens
