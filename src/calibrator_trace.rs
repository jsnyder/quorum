//! Calibrator decision tracing: structured records of per-finding calibration decisions.

use serde::{Deserialize, Serialize};
use crate::finding::{CalibratorAction, Severity};
use crate::feedback::Verdict;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrecedentTrace {
    pub finding_title: String,
    pub verdict: Verdict,
    pub similarity: f64,
    pub weight: f64,
    pub provenance: String,
    pub file_path: String,
}

/// Why the calibrator did (or did not) change a finding's severity.
///
/// Serialized into `~/.quorum/calibrator_traces.jsonl` for eval-harness
/// measurement. `None` means "the field wasn't set" (backward compat with
/// pre-Track-B trace lines); not a value the live calibrator should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityChangeReason {
    /// Calibrator raised severity (gate allowed). `output_severity > input_severity`.
    Boosted,
    /// Calibrator wanted to raise severity but the rubric gate refused.
    /// `output_severity == input_severity`, but a boost was attempted.
    BoostBlockedByGate,
    /// FP / wontfix precedent demoted finding to Info.
    /// `output_severity < input_severity`.
    Disputed,
    /// TP precedent existed but didn't reach the 1.5 / 2x boost thresholds.
    /// `output_severity == input_severity`, no boost attempted.
    BoostWeightTooLow,
    /// No precedents in the corpus matched this finding.
    /// `output_severity == input_severity`, no calibration applied.
    NoMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TraceProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibratorTraceEntry {
    pub finding_title: String,
    pub finding_category: String,
    pub tp_weight: f64,
    pub fp_weight: f64,
    pub wontfix_weight: f64,
    pub full_suppress_weight: f64,
    pub soft_fp_weight: f64,
    pub matched_precedents: Vec<PrecedentTrace>,
    pub action: Option<CalibratorAction>,
    pub input_severity: Severity,
    pub output_severity: Severity,
    /// Track B: why severity did or did not change.
    /// `None` only for backward-compat with pre-Track-B trace lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_change_reason: Option<SeverityChangeReason>,
    /// File path of the finding this trace belongs to (PR3: join support).
    /// `None` for backward-compat with pre-PR3 trace lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<TraceProvenance>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_entry_serializes_to_json() {
        let trace = CalibratorTraceEntry {
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            tp_weight: 2.5,
            fp_weight: 0.3,
            wontfix_weight: 0.0,
            full_suppress_weight: 0.3,
            soft_fp_weight: 0.3,
            matched_precedents: vec![PrecedentTrace {
                finding_title: "SQL injection via f-string".into(),
                verdict: Verdict::Tp,
                similarity: 0.92,
                weight: 1.5,
                provenance: "human".into(),
                file_path: "src/db.py".into(),
            }],
            action: Some(CalibratorAction::Confirmed),
            input_severity: Severity::Medium,
            output_severity: Severity::High,
            severity_change_reason: None,
            file_path: None,
            provenance: None,
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains("\"tp_weight\":2.5"));
        assert!(json.contains("\"similarity\":0.92"));
    }

    #[test]
    fn trace_entry_with_no_precedents() {
        let trace = CalibratorTraceEntry {
            finding_title: "Unused variable".into(),
            finding_category: "quality".into(),
            tp_weight: 0.0,
            fp_weight: 0.0,
            wontfix_weight: 0.0,
            full_suppress_weight: 0.0,
            soft_fp_weight: 0.0,
            matched_precedents: vec![],
            action: None,
            input_severity: Severity::Low,
            output_severity: Severity::Low,
            severity_change_reason: None,
            file_path: None,
            provenance: None,
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains("\"matched_precedents\":[]"));
        assert!(json.contains("\"action\":null"));
    }

    #[test]
    fn severity_change_reason_serializes_snake_case() {
        use SeverityChangeReason::*;
        for (variant, expected) in [
            (Boosted, "\"boosted\""),
            (BoostBlockedByGate, "\"boost_blocked_by_gate\""),
            (Disputed, "\"disputed\""),
            (BoostWeightTooLow, "\"boost_weight_too_low\""),
            (NoMatch, "\"no_match\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "variant {variant:?} must serialize to {expected}");
        }
    }

    #[test]
    fn trace_entry_omits_reason_when_none() {
        // Backward compat: pre-Track-B trace lines carry no reason field.
        let trace = CalibratorTraceEntry {
            finding_title: "x".into(),
            finding_category: "y".into(),
            tp_weight: 0.0,
            fp_weight: 0.0,
            wontfix_weight: 0.0,
            full_suppress_weight: 0.0,
            soft_fp_weight: 0.0,
            matched_precedents: vec![],
            action: None,
            input_severity: Severity::Low,
            output_severity: Severity::Low,
            severity_change_reason: None,
            file_path: None,
            provenance: None,
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(!json.contains("severity_change_reason"),
            "None reason must be omitted from JSON to keep traces backward-compatible");
    }

    #[test]
    fn trace_entry_deserializes_pre_track_b_lines() {
        // A trace line written before Track B has no severity_change_reason field.
        // It must still deserialize, with reason defaulting to None.
        let pre_track_b = r#"{
            "finding_title":"x","finding_category":"y",
            "tp_weight":0.0,"fp_weight":0.0,"wontfix_weight":0.0,
            "full_suppress_weight":0.0,"soft_fp_weight":0.0,
            "matched_precedents":[],"action":null,
            "input_severity":"low","output_severity":"low"
        }"#;
        let trace: CalibratorTraceEntry = serde_json::from_str(pre_track_b).unwrap();
        assert!(trace.severity_change_reason.is_none());
    }

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
            provenance: None,
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

    #[test]
    fn trace_provenance_round_trips() {
        let prov = TraceProvenance {
            quorum_version: Some("0.19.0".into()),
            repo: Some("quorum".into()),
            commit_sha: Some("abc123def".into()),
            dirty: Some(false),
            review_model: Some("gpt-5.4".into()),
            run_id: Some("01JTEST000".into()),
            timestamp: Some("2026-05-05T12:00:00Z".into()),
        };
        let json = serde_json::to_string(&prov).unwrap();
        let back: TraceProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(prov, back);
    }

    #[test]
    fn all_none_provenance_serializes_empty() {
        let prov = TraceProvenance::default();
        let json = serde_json::to_string(&prov).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn trace_entry_with_provenance_nested_object() {
        let trace = CalibratorTraceEntry {
            finding_title: "test".into(),
            finding_category: "security".into(),
            tp_weight: 0.0, fp_weight: 0.0, wontfix_weight: 0.0,
            full_suppress_weight: 0.0, soft_fp_weight: 0.0,
            matched_precedents: vec![], action: None,
            input_severity: Severity::Low, output_severity: Severity::Low,
            severity_change_reason: None, file_path: None,
            provenance: Some(TraceProvenance {
                quorum_version: Some("0.19.0".into()),
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains(r#""provenance":{"quorum_version":"0.19.0"}"#),
            "provenance must be a nested object, got: {json}");
    }

    #[test]
    fn old_trace_without_provenance_deserializes() {
        let json = r#"{"finding_title":"x","finding_category":"y","tp_weight":0.0,"fp_weight":0.0,"wontfix_weight":0.0,"full_suppress_weight":0.0,"soft_fp_weight":0.0,"matched_precedents":[],"action":null,"input_severity":"low","output_severity":"low"}"#;
        let trace: CalibratorTraceEntry = serde_json::from_str(json).unwrap();
        assert!(trace.provenance.is_none());
    }

    #[test]
    fn provenance_accepts_unknown_keys_for_forward_compat() {
        let json = r#"{"quorum_version":"0.19.0","future_field":"value"}"#;
        let prov: TraceProvenance = serde_json::from_str(json).unwrap();
        assert_eq!(prov.quorum_version.as_deref(), Some("0.19.0"));
    }

    #[test]
    fn provenance_omitted_when_none() {
        let trace = CalibratorTraceEntry {
            finding_title: "x".into(),
            finding_category: "y".into(),
            tp_weight: 0.0, fp_weight: 0.0, wontfix_weight: 0.0,
            full_suppress_weight: 0.0, soft_fp_weight: 0.0,
            matched_precedents: vec![], action: None,
            input_severity: Severity::Low, output_severity: Severity::Low,
            severity_change_reason: None, file_path: None,
            provenance: None,
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(!json.contains("provenance"),
            "None provenance must be omitted from JSON");
    }
}
