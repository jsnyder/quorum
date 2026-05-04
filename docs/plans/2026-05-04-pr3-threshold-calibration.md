# PR-Curve Threshold Calibration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace hardcoded magic numbers in the calibrator with data-driven thresholds derived from a precision-recall curve on the feedback corpus.

**Architecture:** New `src/metrics.rs` module computes PR curves from `(score, label)` pairs. New `quorum calibrate` CLI subcommand joins feedback labels with calibrator trace scores, runs the curve, and writes `~/.quorum/calibrator_thresholds.toml`. The calibrator reads thresholds at startup, falling back to legacy behavior when no file exists.

**Tech Stack:** Rust 2024, MSRV 1.88, serde + toml for config, clap for CLI. No new crate dependencies (toml and clap already in Cargo.toml).

---

### Task 1: `src/metrics.rs` — PR curve computation

**Files:**
- Create: `src/metrics.rs`
- Modify: `src/lib.rs` — add `pub mod metrics;`

**Step 1: Write the failing test**

In `src/metrics.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_curve_trivial_four_samples() {
        // Scores: 0.9(TP), 0.7(FP), 0.5(TP), 0.3(FP)
        let samples = vec![
            (0.9, true),
            (0.7, false),
            (0.5, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        // At threshold 0.9: TP=1, FP=0, FN=1 → P=1.0, R=0.5
        // At threshold 0.7: TP=1, FP=1, FN=1 → P=0.5, R=0.5
        // At threshold 0.5: TP=2, FP=1, FN=0 → P=0.667, R=1.0
        // At threshold 0.3: TP=2, FP=2, FN=0 → P=0.5, R=1.0
        assert_eq!(curve.len(), 4);
        let (p, r, t) = curve[0];
        assert!((p - 1.0).abs() < 1e-9);
        assert!((r - 0.5).abs() < 1e-9);
        assert!((t - 0.9).abs() < 1e-9);
    }

    #[test]
    fn pr_curve_tied_scores_produces_one_point_per_distinct_score() {
        let samples = vec![
            (0.8, true),
            (0.8, false),
            (0.5, true),
        ];
        let curve = precision_recall_curve(&samples);
        assert_eq!(curve.len(), 2, "tied scores should collapse to one point");
    }

    #[test]
    fn pr_curve_empty_input() {
        let curve = precision_recall_curve(&[]);
        assert!(curve.is_empty());
    }

    #[test]
    fn pr_curve_all_positive() {
        let samples = vec![(0.9, true), (0.5, true)];
        let curve = precision_recall_curve(&samples);
        // Every threshold yields precision=1.0
        for (p, _, _) in &curve {
            assert!((p - 1.0).abs() < 1e-9);
        }
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum pr_curve_trivial -v`
Expected: FAIL — `precision_recall_curve` not found.

**Step 3: Write minimal implementation**

```rust
/// Compute a precision-recall curve from labeled scores.
///
/// Input: `(score, is_positive)` pairs. Higher scores should indicate
/// more likely positive (TP-like). Returns `(precision, recall, threshold)`
/// triples sorted by descending threshold.
pub fn precision_recall_curve(samples: &[(f64, bool)]) -> Vec<(f64, f64, f64)> {
    if samples.is_empty() {
        return vec![];
    }

    let mut sorted: Vec<(f64, bool)> = samples.to_vec();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let total_positives = sorted.iter().filter(|(_, p)| *p).count() as f64;
    if total_positives == 0.0 {
        return vec![];
    }

    let mut curve = Vec::new();
    let mut tp: f64 = 0.0;
    let mut fp: f64 = 0.0;
    let mut i = 0;

    while i < sorted.len() {
        let threshold = sorted[i].0;
        // Consume all samples at this score (handle ties)
        while i < sorted.len() && (sorted[i].0 - threshold).abs() < 1e-12 {
            if sorted[i].1 {
                tp += 1.0;
            } else {
                fp += 1.0;
            }
            i += 1;
        }
        let precision = tp / (tp + fp);
        let recall = tp / total_positives;
        curve.push((precision, recall, threshold));
    }

    curve
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum pr_curve -v`
Expected: all 4 tests PASS.

**Step 5: Commit**

```bash
git add src/metrics.rs src/lib.rs
git commit -m "feat(metrics): precision-recall curve computation"
```

---

### Task 2: Threshold selection functions

**Files:**
- Modify: `src/metrics.rs`

**Step 1: Write the failing test**

```rust
    #[test]
    fn threshold_at_precision_finds_lowest_meeting_target() {
        let samples = vec![
            (0.9, true),
            (0.7, false),
            (0.5, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        // Only threshold 0.9 achieves P>=0.95
        let t = threshold_at_precision(&curve, 0.95);
        assert_eq!(t, Some(0.9));
    }

    #[test]
    fn threshold_at_precision_returns_none_when_unachievable() {
        // All FP — no threshold achieves any precision on positives
        let samples = vec![(0.9, false), (0.5, false)];
        let curve = precision_recall_curve(&samples);
        let t = threshold_at_precision(&curve, 0.5);
        assert_eq!(t, None);
    }

    #[test]
    fn threshold_at_precision_picks_lowest_for_max_recall() {
        // Multiple thresholds achieve target — pick lowest (highest recall)
        let samples = vec![
            (0.9, true),
            (0.8, true),
            (0.7, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        // At 0.9: P=1.0, at 0.8: P=1.0, at 0.7: P=1.0, at 0.3: P=0.75
        let t = threshold_at_precision(&curve, 0.95);
        assert!((t.unwrap() - 0.7).abs() < 1e-9, "should pick lowest threshold achieving P>=0.95");
    }

    #[test]
    fn f1_optimal_threshold_picks_best_f1() {
        let samples = vec![
            (0.9, true),
            (0.7, false),
            (0.5, true),
            (0.3, false),
        ];
        let curve = precision_recall_curve(&samples);
        let t = f1_optimal_threshold(&curve);
        // At 0.9: F1=2*(1.0*0.5)/(1.0+0.5)=0.667
        // At 0.5: F1=2*(0.667*1.0)/(0.667+1.0)=0.800
        assert!((t.unwrap() - 0.5).abs() < 1e-9, "threshold 0.5 has best F1");
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum threshold_at_precision -v`
Expected: FAIL — functions not found.

**Step 3: Write minimal implementation**

```rust
/// Find the lowest threshold that achieves at least `min_precision`.
/// Returns `None` if no threshold meets the target or the curve is empty.
pub fn threshold_at_precision(curve: &[(f64, f64, f64)], min_precision: f64) -> Option<f64> {
    curve
        .iter()
        .filter(|(p, _, _)| *p >= min_precision)
        .last() // curve is descending by threshold — last qualifying is lowest
        .map(|(_, _, t)| *t)
}

/// Find the threshold that maximizes F1 score.
pub fn f1_optimal_threshold(curve: &[(f64, f64, f64)]) -> Option<f64> {
    curve
        .iter()
        .filter(|(p, r, _)| *p + *r > 0.0)
        .max_by(|(p1, r1, _), (p2, r2, _)| {
            let f1_a = 2.0 * p1 * r1 / (p1 + r1);
            let f1_b = 2.0 * p2 * r2 / (p2 + r2);
            f1_a.partial_cmp(&f1_b).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, _, t)| *t)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum metrics::tests -v`
Expected: all 8 tests PASS.

**Step 5: Commit**

```bash
git add src/metrics.rs
git commit -m "feat(metrics): threshold selection at target precision + F1 optimal"
```

---

### Task 3: Add `file_path` to `CalibratorTraceEntry` for join support

**Files:**
- Modify: `src/calibrator_trace.rs:42-58` — add `file_path` field
- Modify: `src/calibrator.rs` — pass `file_path` through `make_trace_entry`

**Step 1: Write the failing test**

In `src/calibrator_trace.rs`, add to existing tests:

```rust
    #[test]
    fn trace_entry_with_file_path_round_trips() {
        let trace = CalibratorTraceEntry {
            finding_title: "test".into(),
            finding_category: "security".into(),
            tp_weight: 1.0,
            fp_weight: 0.5,
            wontfix_weight: 0.0,
            full_suppress_weight: 0.5,
            soft_fp_weight: 0.5,
            matched_precedents: vec![],
            action: None,
            input_severity: Severity::Medium,
            output_severity: Severity::Medium,
            severity_change_reason: None,
            file_path: Some("src/main.rs".to_string()),
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains("\"file_path\":\"src/main.rs\""));
        let back: CalibratorTraceEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file_path.as_deref(), Some("src/main.rs"));
    }

    #[test]
    fn old_trace_entry_without_file_path_deserializes() {
        let json = r#"{"finding_title":"test","finding_category":"security","tp_weight":1.0,"fp_weight":0.5,"wontfix_weight":0.0,"full_suppress_weight":0.5,"soft_fp_weight":0.5,"matched_precedents":[],"action":null,"input_severity":"medium","output_severity":"medium"}"#;
        let trace: CalibratorTraceEntry = serde_json::from_str(json).unwrap();
        assert_eq!(trace.file_path, None, "old entries should parse with None file_path");
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum trace_entry_with_file_path -v`
Expected: FAIL — `file_path` field doesn't exist on `CalibratorTraceEntry`.

**Step 3: Write minimal implementation**

Add to `CalibratorTraceEntry`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
```

Update `make_trace_entry` in `src/calibrator.rs` to accept and pass through a `file_path: &str` parameter. Update all call sites to pass the file path (already available in the pipeline spans).

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum calibrator_trace -v`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/calibrator_trace.rs src/calibrator.rs
git commit -m "feat(trace): add file_path to CalibratorTraceEntry for join support"
```

---

### Task 4: Threshold config TOML read/write

**Files:**
- Create: `src/threshold_config.rs`
- Modify: `src/lib.rs` — add `pub mod threshold_config;`

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn round_trip_full_config() {
        let config = ThresholdConfig {
            suppress: Some(PathThreshold {
                precision_target: 0.95,
                threshold: 0.78,
            }),
            boost: Some(PathThreshold {
                precision_target: 0.85,
                threshold: 0.42,
            }),
        };
        let toml_str = config.to_toml();
        let parsed = ThresholdConfig::from_toml(&toml_str).unwrap();
        assert!((parsed.suppress.unwrap().threshold - 0.78).abs() < 1e-9);
        assert!((parsed.boost.unwrap().threshold - 0.42).abs() < 1e-9);
    }

    #[test]
    fn partial_config_only_boost() {
        let toml_str = "[boost]\nprecision_target = 0.85\nthreshold = 0.42\n";
        let parsed = ThresholdConfig::from_toml(toml_str).unwrap();
        assert!(parsed.suppress.is_none());
        assert!(parsed.boost.is_some());
    }

    #[test]
    fn malformed_toml_returns_error() {
        let result = ThresholdConfig::from_toml("not valid [[[toml");
        assert!(result.is_err());
    }

    #[test]
    fn read_from_missing_file_returns_none() {
        let result = ThresholdConfig::load_from("/nonexistent/path/thresholds.toml");
        assert!(result.is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum threshold_config -v`
Expected: FAIL — module not found.

**Step 3: Write minimal implementation**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathThreshold {
    pub precision_target: f64,
    pub threshold: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThresholdConfig {
    pub suppress: Option<PathThreshold>,
    pub boost: Option<PathThreshold>,
}

impl ThresholdConfig {
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Load from file path. Returns `None` if file doesn't exist or is malformed
    /// (logs warning on malformed).
    pub fn load_from(path: &str) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        match Self::from_toml(&content) {
            Ok(config) => Some(config),
            Err(e) => {
                tracing::warn!(path, error = %e, "malformed calibrator_thresholds.toml, using defaults");
                None
            }
        }
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum threshold_config -v`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/threshold_config.rs src/lib.rs
git commit -m "feat(config): threshold config TOML read/write"
```

---

### Task 5: Wire thresholds into `CalibratorConfig` + decision logic

**Files:**
- Modify: `src/calibrator.rs:17-50` — add threshold fields to `CalibratorConfig`
- Modify: `src/calibrator.rs:140-212` — use thresholds in `calibrate_core_decision`

**Step 1: Write the failing test**

Add to calibrator tests:

```rust
    #[test]
    fn data_driven_suppress_threshold_overrides_legacy() {
        // Legacy: needs fp_weight >= 1.5 && fp > tp*2. With fp=1.2, tp=0.1
        // this would NOT suppress under legacy rules (1.2 < 1.5).
        // But with suppress_threshold=0.95, score = 0.1/(0.1+1.2)=0.077,
        // which is < (1-0.95)=0.05 ... wait, 0.077 > 0.05, so still not suppressed.
        // Let's use fp=2.0, tp=0.05 → score=0.024 < 0.05 → suppress.
        // Legacy would also suppress (fp=2.0 >= 1.5, 2.0 > 0.05*2). 
        // Better test: fp=1.0, tp=0.01 → score=0.0099 < 0.05 → suppress.
        // Legacy: fp=1.0 < 1.5 → NOT suppress.
        let mut finding = make_test_finding("test finding", Severity::High);
        let mut config = CalibratorConfig::default();
        config.suppress_threshold = Some(0.95);
        let decision = calibrate_core_decision(
            &mut finding, &config,
            0.01, // tp_weight
            1.0,  // fp_weight
            0.0,  // wontfix_weight
            1.0,  // soft_fp_weight
            vec![], Severity::High,
        );
        assert!(decision.suppressed, "data-driven threshold should suppress when score < (1-0.95)");
    }

    #[test]
    fn legacy_suppress_unchanged_without_threshold() {
        // fp=1.0, tp=0.01 — NOT suppressed under legacy (fp < 1.5)
        let mut finding = make_test_finding("test finding", Severity::High);
        let config = CalibratorConfig::default(); // no suppress_threshold
        let decision = calibrate_core_decision(
            &mut finding, &config,
            0.01, 1.0, 0.0, 1.0,
            vec![], Severity::High,
        );
        assert!(!decision.suppressed, "legacy rules should not suppress when fp < 1.5");
    }

    #[test]
    fn force_threshold_env_overrides_config() {
        // With QUORUM_FORCE_THRESHOLD=0.99, score must be < 0.01 to suppress.
        // fp=1.0, tp=0.01 → score=0.0099 < 0.01 → suppress.
        let mut finding = make_test_finding("test finding", Severity::High);
        let mut config = CalibratorConfig::default();
        config.force_threshold = Some(0.99); // injectable, no env mutation
        let decision = calibrate_core_decision(
            &mut finding, &config,
            0.01, 1.0, 0.0, 1.0,
            vec![], Severity::High,
        );
        assert!(decision.suppressed, "force_threshold should override");
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum data_driven_suppress -v`
Expected: FAIL — `suppress_threshold` field not on config.

**Step 3: Write minimal implementation**

Add to `CalibratorConfig`:

```rust
    pub suppress_threshold: Option<f64>,
    pub boost_threshold: Option<f64>,
    pub force_threshold: Option<f64>,
```

In `calibrate_core_decision`, replace the suppress block:

```rust
    let effective_suppress = config.force_threshold.or(config.suppress_threshold);
    if let Some(target_precision) = effective_suppress {
        let total = tp_weight + fp_weight;
        if total > 0.0 {
            let score = tp_weight / total;
            if score <= (1.0 - target_precision) && fp_weight > 0.0 {
                finding.calibrator_action = Some(CalibratorAction::Disputed);
                suppressed = true;
                // ... trace + return
            }
        }
    } else {
        // Legacy fallback
        if full_suppress_weight >= 1.5 && fp_weight > 0.0 && full_suppress_weight > tp_weight * 2.0 {
            // ... existing code
        }
    }
```

Same pattern for boost. Update `Default` to have `None` for all three.

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum calibrat -v` (covers all calibrator tests)
Expected: PASS (all existing + 3 new).

**Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "feat(calibrator): data-driven thresholds with legacy fallback"
```

---

### Task 6: `quorum calibrate` CLI subcommand — corpus join + curve + write

**Files:**
- Modify: `src/cli/mod.rs` — add `Calibrate` variant + `CalibrateOpts`
- Create: `src/calibrate.rs` — join logic, curve computation, TOML output
- Modify: `src/lib.rs` — add `pub mod calibrate;`
- Modify: `src/main.rs` — dispatch `Command::Calibrate`

**Step 1: Write the failing test**

In `src/calibrate.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_feedback(title: &str, verdict: &str, file_path: &str) -> serde_json::Value {
        serde_json::json!({
            "finding_title": title,
            "verdict": verdict,
            "file_path": file_path,
            "finding_category": "security",
            "reason": "test",
            "timestamp": "2026-01-01T00:00:00Z",
            "provenance": "human"
        })
    }

    fn make_trace(title: &str, tp: f64, fp: f64, file_path: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "finding_title": title,
            "finding_category": "security",
            "tp_weight": tp,
            "fp_weight": fp,
            "wontfix_weight": 0.0,
            "full_suppress_weight": fp,
            "soft_fp_weight": fp,
            "matched_precedents": [],
            "action": null,
            "input_severity": "medium",
            "output_severity": "medium"
        });
        if let Some(fp) = file_path {
            v["file_path"] = serde_json::json!(fp);
        }
        v
    }

    #[test]
    fn join_produces_labeled_scores() {
        let feedback = vec![
            make_feedback("SQL injection", "tp", "src/db.rs"),
            make_feedback("Unused var", "fp", "src/main.rs"),
        ];
        let traces = vec![
            make_trace("SQL injection", 2.5, 0.3, Some("src/db.rs")),
            make_trace("Unused var", 0.1, 1.8, Some("src/main.rs")),
        ];
        let samples = join_feedback_and_traces(&feedback, &traces);
        assert_eq!(samples.len(), 2);
        // SQL injection: score = 2.5/(2.5+0.3) ≈ 0.893, label = true
        assert!(samples.iter().any(|(s, l)| *l && (*s - 0.893).abs() < 0.01));
        // Unused var: score = 0.1/(0.1+1.8) ≈ 0.053, label = false
        assert!(samples.iter().any(|(s, l)| !*l && (*s - 0.053).abs() < 0.01));
    }

    #[test]
    fn wontfix_entries_are_skipped() {
        let feedback = vec![make_feedback("Style issue", "wontfix", "src/x.rs")];
        let traces = vec![make_trace("Style issue", 0.5, 0.5, Some("src/x.rs"))];
        let samples = join_feedback_and_traces(&feedback, &traces);
        assert!(samples.is_empty(), "wontfix should be excluded");
    }

    #[test]
    fn class_balance_gate_rejects_insufficient_fps() {
        // 25 TPs, 2 FPs — suppress path should be gated
        let mut samples: Vec<(f64, bool)> = (0..25).map(|i| (0.5 + i as f64 * 0.01, true)).collect();
        samples.extend((0..2).map(|i| (0.1 + i as f64 * 0.01, false)));
        let result = compute_thresholds(&samples, 0.95, 0.85);
        assert!(result.suppress.is_none(), "insufficient FPs for suppress");
        assert!(result.boost.is_some(), "enough TPs for boost");
    }

    #[test]
    fn minimum_total_gate() {
        let samples: Vec<(f64, bool)> = vec![(0.9, true), (0.1, false)];
        let result = compute_thresholds(&samples, 0.95, 0.85);
        assert!(result.suppress.is_none());
        assert!(result.boost.is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum calibrate::tests -v`
Expected: FAIL — module not found.

**Step 3: Write minimal implementation**

```rust
use crate::metrics;
use crate::threshold_config::{PathThreshold, ThresholdConfig};

const MIN_TOTAL_SAMPLES: usize = 20;
const MIN_MINORITY_CLASS: usize = 10;

pub fn join_feedback_and_traces(
    feedback: &[serde_json::Value],
    traces: &[serde_json::Value],
) -> Vec<(f64, bool)> {
    // Build lookup: (finding_title, file_path) → (tp_weight, fp_weight)
    let mut trace_map: std::collections::HashMap<(String, String), (f64, f64)> =
        std::collections::HashMap::new();
    for t in traces {
        let title = t["finding_title"].as_str().unwrap_or("").to_string();
        let fp = t["file_path"].as_str().unwrap_or("").to_string();
        let tp_w = t["tp_weight"].as_f64().unwrap_or(0.0);
        let fp_w = t["fp_weight"].as_f64().unwrap_or(0.0);
        trace_map.entry((title, fp)).or_insert((tp_w, fp_w));
    }

    let mut samples = Vec::new();
    for f in feedback {
        let verdict = f["verdict"].as_str().unwrap_or("");
        let is_positive = match verdict {
            "tp" | "partial" => true,
            "fp" => false,
            _ => continue, // skip wontfix, unknown
        };
        let title = f["finding_title"].as_str().unwrap_or("").to_string();
        let fp = f["file_path"].as_str().unwrap_or("").to_string();
        if let Some((tp_w, fp_w)) = trace_map.get(&(title, fp)) {
            let total = tp_w + fp_w;
            if total > 0.0 {
                samples.push((tp_w / total, is_positive));
            }
        }
    }
    samples
}

pub fn compute_thresholds(
    samples: &[(f64, bool)],
    suppress_precision: f64,
    boost_precision: f64,
) -> ThresholdConfig {
    let total = samples.len();
    let positives = samples.iter().filter(|(_, l)| *l).count();
    let negatives = total - positives;

    let mut config = ThresholdConfig::default();

    if total < MIN_TOTAL_SAMPLES {
        return config;
    }

    // Suppress path: need enough FPs (negatives in the labeled set)
    if negatives >= MIN_MINORITY_CLASS {
        let curve = metrics::precision_recall_curve(samples);
        if let Some(t) = metrics::threshold_at_precision(&curve, suppress_precision) {
            config.suppress = Some(PathThreshold {
                precision_target: suppress_precision,
                threshold: t,
            });
        }
    }

    // Boost path: need enough TPs — invert labels, run curve on inverted
    // Actually: boost uses tp_weight dominance. For the boost threshold,
    // we want to find when the score is HIGH enough that the finding is
    // likely a real TP. The PR curve already captures this: high score =
    // high precision among positives. We just need a different target.
    if positives >= MIN_MINORITY_CLASS {
        let curve = metrics::precision_recall_curve(samples);
        if let Some(t) = metrics::threshold_at_precision(&curve, boost_precision) {
            config.boost = Some(PathThreshold {
                precision_target: boost_precision,
                threshold: t,
            });
        }
    }

    config
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum calibrate::tests -v`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/calibrate.rs src/lib.rs
git commit -m "feat(calibrate): corpus join + threshold computation with data quality gates"
```

---

### Task 7: Wire `quorum calibrate` CLI subcommand

**Files:**
- Modify: `src/cli/mod.rs` — add `Calibrate(CalibrateOpts)` variant
- Modify: `src/main.rs` — add dispatch + `run_calibrate()` function

**Step 1: Write the failing test**

Integration-level — verify the CLI wiring:

```rust
    #[test]
    fn calibrate_subcommand_parses() {
        use clap::Parser;
        let args = crate::cli::Args::try_parse_from(["quorum", "calibrate"]);
        assert!(args.is_ok(), "calibrate subcommand should parse");
    }

    #[test]
    fn calibrate_dry_run_parses() {
        use clap::Parser;
        let args = crate::cli::Args::try_parse_from(["quorum", "calibrate", "--dry-run"]);
        assert!(args.is_ok(), "calibrate --dry-run should parse");
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum calibrate_subcommand_parses -v`
Expected: FAIL — no `calibrate` variant.

**Step 3: Write minimal implementation**

In `src/cli/mod.rs`, add:

```rust
    /// Compute calibrator thresholds from feedback corpus
    Calibrate(CalibrateOpts),
```

```rust
#[derive(Parser)]
pub struct CalibrateOpts {
    /// Compute and print thresholds without writing the config file
    #[arg(long)]
    pub dry_run: bool,

    /// Target precision for suppress path (default: 0.95)
    #[arg(long, default_value = "0.95")]
    pub suppress_precision: f64,

    /// Target precision for boost path (default: 0.85)
    #[arg(long, default_value = "0.85")]
    pub boost_precision: f64,
}
```

In `src/main.rs`, add dispatch:

```rust
        cli::Command::Calibrate(opts) => std::process::exit(run_calibrate(opts)),
```

Implement `run_calibrate()`:
1. Load `~/.quorum/feedback.jsonl` (line-by-line JSON parse).
2. Load `~/.quorum/calibrator_traces.jsonl` (same).
3. Call `calibrate::join_feedback_and_traces`.
4. Call `calibrate::compute_thresholds`.
5. Print summary to stdout.
6. If not `--dry-run`, write `~/.quorum/calibrator_thresholds.toml`.

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum calibrate_subcommand -v`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "feat(cli): quorum calibrate subcommand"
```

---

### Task 8: Load thresholds at startup + end-to-end test

**Files:**
- Modify: `src/main.rs` — load `calibrator_thresholds.toml` into `CalibratorConfig`
- Add integration test verifying full flow

**Step 1: Write the failing test**

```rust
    #[test]
    fn config_loads_thresholds_from_toml_into_calibrator() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calibrator_thresholds.toml");
        std::fs::write(&path, "[suppress]\nprecision_target = 0.95\nthreshold = 0.78\n\n[boost]\nprecision_target = 0.85\nthreshold = 0.42\n").unwrap();
        let config = ThresholdConfig::load_from(path.to_str().unwrap());
        assert!(config.is_some());
        let config = config.unwrap();
        assert!((config.suppress.unwrap().threshold - 0.78).abs() < 1e-9);
        assert!((config.boost.unwrap().threshold - 0.42).abs() < 1e-9);
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum config_loads_thresholds -v`
Expected: Should PASS if Task 4 is done. This is a regression guard.

**Step 3: Wire into main.rs**

In the main review path where `CalibratorConfig` is constructed, add:

```rust
    let thresholds_path = format!(
        "{}/.quorum/calibrator_thresholds.toml",
        std::env::var("HOME").unwrap_or_default()
    );
    if let Some(tc) = threshold_config::ThresholdConfig::load_from(&thresholds_path) {
        calibrator_config.suppress_threshold = tc.suppress.map(|p| p.threshold);
        calibrator_config.boost_threshold = tc.boost.map(|p| p.threshold);
    }
    // QUORUM_FORCE_THRESHOLD overrides
    if let Ok(v) = std::env::var("QUORUM_FORCE_THRESHOLD") {
        if let Ok(t) = v.parse::<f64>() {
            calibrator_config.force_threshold = Some(t);
        }
    }
```

**Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: all tests PASS (existing + ~15 new).

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: load calibrator thresholds from TOML at startup"
```

---

### Task 9: Verification + cleanup

**Files:**
- All modified files

**Step 1: Run full verification gates**

```bash
cargo test --bin quorum
cargo clippy --bin quorum
cargo build --release
```

Expected: all pass, no new clippy warnings.

**Step 2: Run `quorum calibrate` on the real corpus**

```bash
./target/release/quorum calibrate --dry-run
```

Inspect the output: corpus size, class balance, thresholds, precision/recall at each operating point. Verify the numbers are sane.

**Step 3: Commit any fixups**

```bash
git add -A
git commit -m "chore: verification fixes"
```
