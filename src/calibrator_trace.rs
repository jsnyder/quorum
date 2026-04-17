//! Calibrator decision tracing: structured records of per-finding calibration decisions.

use serde::Serialize;
use crate::finding::{CalibratorAction, Severity};
use crate::feedback::Verdict;

#[derive(Debug, Clone, Serialize)]
pub struct PrecedentTrace {
    pub finding_title: String,
    pub verdict: Verdict,
    pub similarity: f64,
    pub weight: f64,
    pub provenance: String,
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize)]
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
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains("\"matched_precedents\":[]"));
        assert!(json.contains("\"action\":null"));
    }
}
