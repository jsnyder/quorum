/// Calibrator: adjusts findings using feedback precedent.
/// For each finding, searches for similar past findings and their TP/FP verdicts.
/// FP precedent suppresses findings; TP precedent boosts confidence.

use crate::feedback::{FeedbackEntry, Verdict};
use crate::finding::{CalibratorAction, Finding, Severity};

#[derive(Debug, Clone)]
pub struct CalibrationResult {
    pub findings: Vec<Finding>,
    pub suppressed: usize,
    pub boosted: usize,
}

pub struct CalibratorConfig {
    /// Minimum similarity to consider a precedent match (0.0 - 1.0)
    pub similarity_threshold: f64,
    /// Number of FP precedents needed to suppress a finding
    pub fp_suppress_count: usize,
    /// Whether to boost severity when strong TP precedent exists
    pub boost_tp: bool,
}

impl Default for CalibratorConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.5,
            fp_suppress_count: 2,
            boost_tp: true,
        }
    }
}

/// Calibrate findings using feedback precedent.
pub fn calibrate(
    findings: Vec<Finding>,
    feedback: &[FeedbackEntry],
    config: &CalibratorConfig,
) -> CalibrationResult {
    if feedback.is_empty() {
        return CalibrationResult {
            findings,
            suppressed: 0,
            boosted: 0,
        };
    }

    let mut output = Vec::new();
    let mut suppressed = 0;
    let mut boosted = 0;

    for mut finding in findings {
        // Find similar feedback entries
        let similar: Vec<&FeedbackEntry> = feedback
            .iter()
            .filter(|e| finding_feedback_similarity(&finding, e) >= config.similarity_threshold)
            .collect();

        if similar.is_empty() {
            output.push(finding);
            continue;
        }

        // Count verdicts
        let tp_count = similar.iter().filter(|e| e.verdict == Verdict::Tp).count();
        let fp_count = similar.iter().filter(|e| e.verdict == Verdict::Fp).count();

        // Annotate with precedent info
        for entry in &similar {
            finding.similar_precedent.push(format!(
                "{}: {} ({})",
                match entry.verdict {
                    Verdict::Tp => "TP",
                    Verdict::Fp => "FP",
                    Verdict::Partial => "Partial",
                    Verdict::Wontfix => "Wontfix",
                },
                entry.finding_title,
                entry.reason
            ));
        }

        // Suppress if enough FP precedent and FP is majority
        if fp_count >= config.fp_suppress_count && fp_count > tp_count {
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            suppressed += 1;
            continue; // don't add to output
        }

        // Boost if TP precedent exists and boosting enabled
        if config.boost_tp && tp_count >= 2 && tp_count > fp_count {
            finding.severity = boost_severity(&finding.severity);
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
            boosted += 1;
        } else if tp_count > 0 {
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
        }

        output.push(finding);
    }

    CalibrationResult {
        findings: output,
        suppressed,
        boosted,
    }
}

fn boost_severity(severity: &Severity) -> Severity {
    match severity {
        Severity::Info => Severity::Low,
        Severity::Low => Severity::Medium,
        Severity::Medium => Severity::High,
        Severity::High => Severity::Critical,
        Severity::Critical => Severity::Critical,
    }
}

/// Compute similarity between a finding and a feedback entry.
/// Weighted combination of title similarity and category match.
fn finding_feedback_similarity(finding: &Finding, entry: &FeedbackEntry) -> f64 {
    let mut score = 0.0;

    // Title word overlap (Jaccard) — weight 3
    let title_sim = word_jaccard(&finding.title, &entry.finding_title);
    score += title_sim * 3.0;

    // Category exact match — weight 2
    if !entry.finding_category.is_empty() && finding.category == entry.finding_category {
        score += 2.0;
    }

    score / 5.0
}

fn word_jaccard(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let words_a: std::collections::HashSet<&str> = a.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    let intersection = words_a.intersection(&words_b).count() as f64;
    let union = words_a.union(&words_b).count() as f64;
    if union == 0.0 { 0.0 } else { intersection / union }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::FindingBuilder;
    use chrono::Utc;

    fn fb(title: &str, category: &str, verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: title.into(),
            finding_category: category.into(),
            verdict,
            reason: "test".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
        }
    }

    // -- No feedback: passthrough --

    #[test]
    fn calibrate_no_feedback_passthrough() {
        let findings = vec![
            FindingBuilder::new().title("Bug A").build(),
            FindingBuilder::new().title("Bug B").build(),
        ];
        let result = calibrate(findings, &[], &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 2);
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.boosted, 0);
    }

    // -- FP suppression --

    #[test]
    fn calibrate_suppresses_finding_with_fp_precedent() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![
            fb("SQL injection", "security", Verdict::Fp),
            fb("SQL injection", "security", Verdict::Fp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 0, "Finding should be suppressed by 2 FP precedents");
        assert_eq!(result.suppressed, 1);
    }

    #[test]
    fn calibrate_does_not_suppress_with_insufficient_fp() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![
            fb("SQL injection", "security", Verdict::Fp),
            // Only 1 FP, need 2 to suppress
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.suppressed, 0);
    }

    #[test]
    fn calibrate_marks_suppressed_as_disputed() {
        let findings = vec![
            FindingBuilder::new()
                .title("Unused import")
                .category("style")
                .build(),
        ];
        let feedback = vec![
            fb("Unused import", "style", Verdict::Fp),
            fb("Unused import", "style", Verdict::Fp),
            fb("Unused import", "style", Verdict::Fp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.suppressed, 1);
    }

    // -- TP boosting --

    #[test]
    fn calibrate_boosts_severity_with_tp_precedent() {
        let findings = vec![
            FindingBuilder::new()
                .title("Buffer overflow")
                .category("security")
                .severity(Severity::Medium)
                .build(),
        ];
        let feedback = vec![
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 1);
        assert!(result.findings[0].severity > Severity::Medium, "Should be boosted above Medium");
        assert_eq!(result.boosted, 1);
    }

    #[test]
    fn calibrate_no_boost_when_disabled() {
        let findings = vec![
            FindingBuilder::new()
                .title("Buffer overflow")
                .category("security")
                .severity(Severity::Medium)
                .build(),
        ];
        let feedback = vec![
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
        ];
        let config = CalibratorConfig {
            boost_tp: false,
            ..Default::default()
        };
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.findings[0].severity, Severity::Medium);
        assert_eq!(result.boosted, 0);
    }

    // -- Mixed precedent --

    #[test]
    fn calibrate_mixed_tp_fp_uses_majority() {
        let findings = vec![
            FindingBuilder::new()
                .title("Race condition")
                .category("concurrency")
                .build(),
        ];
        let feedback = vec![
            fb("Race condition", "concurrency", Verdict::Tp),
            fb("Race condition", "concurrency", Verdict::Tp),
            fb("Race condition", "concurrency", Verdict::Fp),
        ];
        // 2 TP vs 1 FP = keep (and possibly boost)
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 1);
    }

    // -- Similarity matching --

    #[test]
    fn similarity_exact_match() {
        let finding = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .build();
        let entry = fb("SQL injection", "security", Verdict::Tp);
        assert!(finding_feedback_similarity(&finding, &entry) > 0.8);
    }

    #[test]
    fn similarity_different_finding() {
        let finding = FindingBuilder::new()
            .title("SQL injection in auth module")
            .category("security")
            .build();
        let entry = fb("Unused import os", "style", Verdict::Fp);
        assert!(finding_feedback_similarity(&finding, &entry) < 0.3);
    }

    #[test]
    fn similarity_partial_title_match() {
        let finding = FindingBuilder::new()
            .title("SQL injection via string concatenation")
            .category("security")
            .build();
        let entry = fb("SQL injection in query builder", "security", Verdict::Tp);
        let sim = finding_feedback_similarity(&finding, &entry);
        assert!(sim > 0.4 && sim < 0.9, "Partial match should be moderate: {}", sim);
    }

    // -- Precedent annotation --

    #[test]
    fn calibrate_annotates_findings_with_precedent() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![
            fb("SQL injection", "security", Verdict::Tp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert!(!result.findings[0].similar_precedent.is_empty(),
            "Finding should have precedent annotation");
    }

    #[test]
    fn calibrate_confirmed_action_on_tp_match() {
        let findings = vec![
            FindingBuilder::new()
                .title("Null pointer")
                .category("safety")
                .build(),
        ];
        let feedback = vec![
            fb("Null pointer", "safety", Verdict::Tp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Confirmed));
    }
}
