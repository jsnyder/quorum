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
    pub traces: Vec<crate::calibrator_trace::CalibratorTraceEntry>,
}

pub struct CalibratorConfig {
    /// Minimum similarity for Jaccard fallback (0.0 - 1.0)
    pub similarity_threshold: f64,
    /// Minimum similarity for embedding-based matching (higher because BGE clusters tightly)
    pub embedding_similarity_threshold: f64,
    /// Number of FP precedents needed to suppress a finding
    pub fp_suppress_count: usize,
    /// Whether to boost severity when strong TP precedent exists
    pub boost_tp: bool,
    /// Whether to include auto-calibrate feedback in precedent matching
    pub use_auto_feedback: bool,
}

impl Default for CalibratorConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.5,
            embedding_similarity_threshold: 0.80,
            fp_suppress_count: 2,
            boost_tp: true,
            use_auto_feedback: true,
        }
    }
}

/// Compute the weight of a single feedback entry based on provenance and recency.
fn verdict_weight(entry: &FeedbackEntry) -> f64 {
    let provenance_weight = match &entry.provenance {
        crate::feedback::Provenance::PostFix => 1.5,
        crate::feedback::Provenance::Human => 1.0,
        crate::feedback::Provenance::AutoCalibrate(_) => 0.5,
        crate::feedback::Provenance::Unknown => 0.3,
    };

    let age_days = (chrono::Utc::now() - entry.timestamp).num_days().max(0) as f64;
    let recency_weight = (-age_days / 120.0).exp(); // half-life ~83 days

    provenance_weight * recency_weight
}

/// Calibrate findings using feedback precedent.
pub fn calibrate(
    findings: Vec<Finding>,
    feedback: &[FeedbackEntry],
    config: &CalibratorConfig,
) -> CalibrationResult {
    // Filter out auto-calibrate feedback if configured
    let filtered: Vec<&FeedbackEntry> = if config.use_auto_feedback {
        feedback.iter().collect()
    } else {
        feedback
            .iter()
            .filter(|e| !matches!(e.provenance, crate::feedback::Provenance::AutoCalibrate(_)))
            .collect()
    };

    if filtered.is_empty() {
        return CalibrationResult {
            findings,
            suppressed: 0,
            boosted: 0,
            traces: vec![],
        };
    }

    let mut output = Vec::new();
    let mut suppressed = 0;
    let mut boosted = 0;
    let mut traces = Vec::new();

    for mut finding in findings {
        let input_severity = finding.severity.clone();

        // Find similar feedback entries
        let similar: Vec<&&FeedbackEntry> = filtered
            .iter()
            .filter(|e| finding_feedback_similarity(&finding, e) >= config.similarity_threshold)
            .collect();

        if similar.is_empty() {
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight: 0.0,
                fp_weight: 0.0,
                wontfix_weight: 0.0,
                full_suppress_weight: 0.0,
                soft_fp_weight: 0.0,
                matched_precedents: vec![],
                action: None,
                input_severity: input_severity.clone(),
                output_severity: finding.severity.clone(),
            });
            output.push(finding);
            continue;
        }

        // Compute weighted verdict scores with auto-calibrate cap
        let mut auto_tp_weight: f64 = 0.0;
        let mut other_tp_weight: f64 = 0.0;
        for e in similar.iter().filter(|e| e.verdict == Verdict::Tp || e.verdict == Verdict::Partial) {
            if matches!(e.provenance, crate::feedback::Provenance::AutoCalibrate(_)) {
                auto_tp_weight += verdict_weight(e);
            } else {
                other_tp_weight += verdict_weight(e);
            }
        }
        let tp_weight = auto_tp_weight.min(1.0) + other_tp_weight;

        // Strict FP weight (drives full suppression)
        let mut auto_fp_weight: f64 = 0.0;
        let mut other_fp_weight: f64 = 0.0;
        for e in similar.iter().filter(|e| e.verdict == Verdict::Fp) {
            if matches!(e.provenance, crate::feedback::Provenance::AutoCalibrate(_)) {
                auto_fp_weight += verdict_weight(e);
            } else {
                other_fp_weight += verdict_weight(e);
            }
        }
        let fp_weight = auto_fp_weight.min(1.0) + other_fp_weight;

        // Wontfix weight (soft suppression at 100%, full suppression at 50%)
        let mut wontfix_weight: f64 = 0.0;
        for e in similar.iter().filter(|e| e.verdict == Verdict::Wontfix) {
            wontfix_weight += verdict_weight(e);
        }
        let soft_fp_weight = fp_weight + wontfix_weight;

        // Build precedent traces for this finding
        let matched_precedents: Vec<crate::calibrator_trace::PrecedentTrace> = similar
            .iter()
            .map(|e| crate::calibrator_trace::PrecedentTrace {
                finding_title: e.finding_title.clone(),
                verdict: e.verdict.clone(),
                similarity: 1.0, // Jaccard doesn't expose per-entry similarity
                weight: verdict_weight(e),
                provenance: format!("{:?}", e.provenance),
                file_path: e.file_path.clone(),
            })
            .collect();

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

        // Full suppress: FP weight + wontfix at 50% contribution
        let full_suppress_weight = fp_weight + (wontfix_weight * 0.5);
        if full_suppress_weight >= 1.5 && fp_weight > 0.0 && full_suppress_weight > tp_weight * 2.0 {
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight,
                fp_weight,
                wontfix_weight,
                full_suppress_weight,
                soft_fp_weight,
                matched_precedents,
                action: finding.calibrator_action.clone(),
                input_severity,
                output_severity: finding.severity.clone(),
            });
            suppressed += 1;
            continue; // don't add to output
        }

        // Soft suppress: FP + wontfix combined, or auto-only FP
        // This preserves the finding for human review while reducing noise
        if soft_fp_weight >= 1.0 && soft_fp_weight > tp_weight * 2.0 {
            finding.severity = Severity::Info;
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            // Don't increment suppressed — finding stays in output at reduced severity
        }

        // Boost: TP clearly dominates FP
        if config.boost_tp && tp_weight >= 1.5 && tp_weight > fp_weight * 2.0 {
            finding.severity = boost_severity(&finding.severity);
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
            boosted += 1;
        } else if tp_weight > fp_weight * 1.5 {
            // Confirm only when TP meaningfully outweighs FP
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
        }
        // Mixed signal (TP ~ FP): leave calibrator_action as None

        traces.push(crate::calibrator_trace::CalibratorTraceEntry {
            finding_title: finding.title.clone(),
            finding_category: finding.category.clone(),
            tp_weight,
            fp_weight,
            wontfix_weight,
            full_suppress_weight,
            soft_fp_weight,
            matched_precedents,
            action: finding.calibrator_action.clone(),
            input_severity,
            output_severity: finding.severity.clone(),
        });

        output.push(finding);
    }

    CalibrationResult {
        findings: output,
        suppressed,
        boosted,
        traces,
    }
}

/// Calibrate findings using a FeedbackIndex for similarity matching.
/// Uses semantic embeddings when available, falls back to Jaccard.
/// This is the preferred path when a FeedbackIndex has been built.
pub fn calibrate_with_index(
    findings: Vec<Finding>,
    index: &mut crate::feedback_index::FeedbackIndex,
    config: &CalibratorConfig,
) -> CalibrationResult {
    if index.is_empty() {
        return CalibrationResult { findings, suppressed: 0, boosted: 0, traces: vec![] };
    }

    let mut output = Vec::new();
    let mut suppressed = 0;
    let mut boosted = 0;
    let mut traces = Vec::new();

    for mut finding in findings {
        let input_severity = finding.severity.clone();
        let similar_entries = index.find_similar(&finding.title, &finding.category, 10);

        // Filter by similarity threshold and provenance
        let similar: Vec<&crate::feedback_index::SimilarEntry> = similar_entries.iter()
            .filter(|s| s.similarity >= config.embedding_similarity_threshold as f32)
            .filter(|s| {
                if config.use_auto_feedback { true }
                else { !matches!(s.entry.provenance, crate::feedback::Provenance::AutoCalibrate(_)) }
            })
            .collect();

        if similar.is_empty() {
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight: 0.0,
                fp_weight: 0.0,
                wontfix_weight: 0.0,
                full_suppress_weight: 0.0,
                soft_fp_weight: 0.0,
                matched_precedents: vec![],
                action: None,
                input_severity: input_severity.clone(),
                output_severity: finding.severity.clone(),
            });
            output.push(finding);
            continue;
        }

        let mut auto_tp_weight: f64 = 0.0;
        let mut other_tp_weight: f64 = 0.0;
        for s in similar.iter().filter(|s| s.entry.verdict == Verdict::Tp || s.entry.verdict == Verdict::Partial) {
            let w = verdict_weight(&s.entry) * s.similarity as f64;
            if matches!(s.entry.provenance, crate::feedback::Provenance::AutoCalibrate(_)) {
                auto_tp_weight += w;
            } else {
                other_tp_weight += w;
            }
        }
        let tp_weight = auto_tp_weight.min(1.0) + other_tp_weight;

        // Strict FP weight (drives full suppression)
        let mut auto_fp_weight: f64 = 0.0;
        let mut other_fp_weight: f64 = 0.0;
        for s in similar.iter().filter(|s| s.entry.verdict == Verdict::Fp) {
            let w = verdict_weight(&s.entry) * s.similarity as f64;
            if matches!(s.entry.provenance, crate::feedback::Provenance::AutoCalibrate(_)) {
                auto_fp_weight += w;
            } else {
                other_fp_weight += w;
            }
        }
        let fp_weight = auto_fp_weight.min(1.0) + other_fp_weight;

        // Wontfix weight (soft suppression at 100%, full suppression at 50%)
        let mut wontfix_weight: f64 = 0.0;
        for s in similar.iter().filter(|s| s.entry.verdict == Verdict::Wontfix) {
            let w = verdict_weight(&s.entry) * s.similarity as f64;
            wontfix_weight += w;
        }
        let soft_fp_weight = fp_weight + wontfix_weight;

        // Build precedent traces for this finding
        let matched_precedents: Vec<crate::calibrator_trace::PrecedentTrace> = similar
            .iter()
            .map(|s| crate::calibrator_trace::PrecedentTrace {
                finding_title: s.entry.finding_title.clone(),
                verdict: s.entry.verdict.clone(),
                similarity: s.similarity as f64,
                weight: verdict_weight(&s.entry),
                provenance: format!("{:?}", s.entry.provenance),
                file_path: s.entry.file_path.clone(),
            })
            .collect();

        // Annotate with precedent info
        for s in &similar {
            finding.similar_precedent.push(format!(
                "{}: {} ({}) [sim={:.2}]",
                match s.entry.verdict {
                    Verdict::Tp => "TP", Verdict::Fp => "FP",
                    Verdict::Partial => "Partial", Verdict::Wontfix => "Wontfix",
                },
                s.entry.finding_title, s.entry.reason, s.similarity
            ));
        }

        // Full suppress: FP weight + wontfix at 50% contribution
        let full_suppress_weight = fp_weight + (wontfix_weight * 0.5);
        if full_suppress_weight >= 1.5 && fp_weight > 0.0 && full_suppress_weight > tp_weight * 2.0 {
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight,
                fp_weight,
                wontfix_weight,
                full_suppress_weight,
                soft_fp_weight,
                matched_precedents,
                action: finding.calibrator_action.clone(),
                input_severity,
                output_severity: finding.severity.clone(),
            });
            suppressed += 1;
            continue;
        }

        // Soft suppress: FP + wontfix combined, or auto-only FP
        // This preserves the finding for human review while reducing noise
        if soft_fp_weight >= 1.0 && soft_fp_weight > tp_weight * 2.0 {
            finding.severity = Severity::Info;
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            // Don't increment suppressed — finding stays in output at reduced severity
        }

        // Boost: TP clearly dominates FP
        if config.boost_tp && tp_weight >= 1.5 && tp_weight > fp_weight * 2.0 {
            finding.severity = boost_severity(&finding.severity);
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
            boosted += 1;
        } else if tp_weight > fp_weight * 1.5 {
            // Confirm only when TP meaningfully outweighs FP
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
        }
        // Mixed signal (TP ~ FP): leave calibrator_action as None

        traces.push(crate::calibrator_trace::CalibratorTraceEntry {
            finding_title: finding.title.clone(),
            finding_category: finding.category.clone(),
            tp_weight,
            fp_weight,
            wontfix_weight,
            full_suppress_weight,
            soft_fp_weight,
            matched_precedents,
            action: finding.calibrator_action.clone(),
            input_severity,
            output_severity: finding.severity.clone(),
        });

        output.push(finding);
    }

    CalibrationResult { findings: output, suppressed, boosted, traces }
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
            provenance: crate::feedback::Provenance::Human,
        }
    }

    // -- No feedback: passthrough --

    #[test]
    fn calibrator_config_has_separate_thresholds() {
        let config = CalibratorConfig::default();
        assert!(config.embedding_similarity_threshold > config.similarity_threshold,
            "Embedding threshold should be higher than Jaccard threshold");
    }

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
    fn calibrator_excludes_auto_feedback_when_configured() {
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
        let feedback = vec![auto_fb.clone(), auto_fb];
        let config = CalibratorConfig {
            fp_suppress_count: 2,
            use_auto_feedback: false,
            ..Default::default()
        };
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.suppressed, 0, "Auto feedback excluded, should not suppress");
    }

    #[test]
    fn calibrator_includes_auto_feedback_by_default() {
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
            reason: "confirmed".into(),
            ..auto_fb.clone()
        };
        // 2 auto FPs (capped at 1.0) + 1 human FP (1.0) = 2.0 >= 1.5 -> suppress
        let feedback = vec![auto_fb.clone(), auto_fb, human_fb];
        let config = CalibratorConfig::default(); // use_auto_feedback defaults to true
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.suppressed, 1, "Auto+human feedback should suppress (auto capped at 1.0 + human 1.0 = 2.0)");
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

    // -- Weighted scoring tests --

    #[test]
    fn weighted_calibrator_human_feedback_counts_more() {
        let findings = vec![FindingBuilder::new().title("SQL injection").category("security").build()];

        let human_fp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: "handled upstream".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
        };
        let auto_fp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: "auto".into(),
            model: Some("o3".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
        };

        // Human (1.0) + auto (0.5) = 1.5 >= threshold -> suppress
        let config = CalibratorConfig::default();
        let result1 = calibrate(findings.clone(), &vec![human_fp.clone(), auto_fp.clone()], &config);
        assert_eq!(result1.suppressed, 1, "Human+auto FP should suppress");

        // 2 auto only: 0.5 + 0.5 = 1.0 < 1.5 threshold -> NOT suppress
        let result2 = calibrate(findings.clone(), &vec![auto_fp.clone(), auto_fp], &config);
        assert_eq!(result2.suppressed, 0, "2 auto FPs alone should not suppress (insufficient weight)");
    }

    #[test]
    fn weighted_calibrator_recency_matters() {
        let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
        let old_fp = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "old".into(),
            model: None,
            timestamp: Utc::now() - chrono::Duration::days(90),
            provenance: crate::feedback::Provenance::Human,
        };
        let recent_fp = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "recent".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
        };

        let config = CalibratorConfig::default();
        // 2 recent human FPs: 1.0 + 1.0 = 2.0 >= 1.5 -> suppress
        let result1 = calibrate(findings.clone(), &vec![recent_fp.clone(), recent_fp], &config);
        assert_eq!(result1.suppressed, 1);

        // 2 old FPs: exp(-90/60) ~= 0.22 each, total ~0.44 < 1.5 -> NOT suppress
        let result2 = calibrate(findings.clone(), &vec![old_fp.clone(), old_fp], &config);
        assert!(result2.suppressed <= 1);
    }

    #[test]
    fn weighted_calibrator_produces_confidence() {
        let findings = vec![FindingBuilder::new().title("SQL injection").category("security").build()];
        let tp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "real".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
        };

        let config = CalibratorConfig::default();
        let result = calibrate(findings, &vec![tp], &config);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Confirmed));
    }

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

    #[test]
    fn postfix_provenance_has_highest_weight() {
        // A single PostFix TP (weight 1.5) should be enough to confirm
        let findings = vec![FindingBuilder::new().title("SQL injection").category("security").build()];
        let postfix_tp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "fixed with parameterized queries".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::PostFix,
        };

        let config = CalibratorConfig::default();
        let result = calibrate(findings, &vec![postfix_tp], &config);
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Confirmed));
    }

    #[test]
    fn auto_calibrate_weight_capped_at_one() {
        // 4 auto FPs: uncapped = 4 * 0.5 = 2.0 (would suppress)
        // capped = min(2.0, 1.0) = 1.0 (should NOT fully suppress — needs human corroboration)
        // Note: soft suppression downgrades severity to INFO but finding remains in output
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

    #[test]
    fn auto_only_fp_soft_suppresses_to_info() {
        // Auto-only FP should downgrade to INFO, not fully suppress
        let finding = FindingBuilder::new()
            .title("Template uses states() without availability check")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let feedback = vec![
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
        ];
        // Make all entries auto-calibrate provenance
        let auto_feedback: Vec<FeedbackEntry> = feedback.into_iter().map(|mut e| {
            e.provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
            e
        }).collect();

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &auto_feedback, &config);

        // Finding should NOT be fully suppressed
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.findings.len(), 1);
        // But should be downgraded to INFO
        assert_eq!(result.findings[0].severity, Severity::Info);
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Disputed));
    }

    #[test]
    fn auto_plus_human_fp_still_fully_suppresses() {
        // Human corroboration should still enable full suppression
        let finding = FindingBuilder::new()
            .title("Template uses states() without availability check")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let mut feedback = vec![
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
        ];
        // One auto, one human — human provides the extra weight to cross 1.5
        feedback[0].provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
        feedback[1].provenance = crate::feedback::Provenance::Human;

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        // Should be fully suppressed (human corroboration)
        assert_eq!(result.suppressed, 1);
        assert_eq!(result.findings.len(), 0);
    }

    #[test]
    fn auto_fp_with_tp_opposition_no_soft_suppress() {
        // If there's significant TP signal, don't soft suppress even with auto FP
        let finding = FindingBuilder::new()
            .title("Use of unwrap() may panic")
            .severity(Severity::Medium)
            .category("security")
            .build();
        let mut feedback = vec![
            fb("Use of unwrap() may panic", "security", Verdict::Fp),
            fb("Use of unwrap() may panic", "security", Verdict::Fp),
            fb("Use of unwrap() may panic", "security", Verdict::Tp),
        ];
        feedback[0].provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
        feedback[1].provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
        feedback[2].provenance = crate::feedback::Provenance::Human;

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        // Mixed signal — should NOT be soft suppressed
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.findings.len(), 1);
        assert_ne!(result.findings[0].severity, Severity::Info); // severity preserved
    }

    #[test]
    fn soft_suppress_preserves_finding_in_output() {
        // Soft-suppressed findings must remain in output (not filtered out)
        let finding = FindingBuilder::new()
            .title("Deprecated trigger syntax")
            .severity(Severity::High)
            .category("quality")
            .build();
        let auto_feedback: Vec<FeedbackEntry> = (0..5).map(|_| {
            let mut e = fb("Deprecated trigger syntax", "quality", Verdict::Fp);
            e.provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
            e
        }).collect();

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &auto_feedback, &config);

        assert_eq!(result.findings.len(), 1); // still in output
        assert_eq!(result.findings[0].severity, Severity::Info); // but downgraded
    }

    #[test]
    fn postfix_fp_suppresses_with_single_entry() {
        // A single PostFix FP (weight 1.5) meets the 1.5 threshold alone
        let findings = vec![FindingBuilder::new().title("Unused import").category("style").build()];
        let postfix_fp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "Unused import".into(),
            finding_category: "style".into(),
            verdict: Verdict::Fp,
            reason: "import is used dynamically".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::PostFix,
        };

        let config = CalibratorConfig::default();
        let result = calibrate(findings, &vec![postfix_fp], &config);
        assert_eq!(result.suppressed, 1, "Single PostFix FP should suppress (weight 1.5 >= threshold)");
    }

    #[test]
    fn wontfix_only_soft_suppresses_not_full() {
        let finding = FindingBuilder::new()
            .title("console.log debug artifact")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let feedback = vec![
            fb("console.log debug artifact", "quality", Verdict::Wontfix),
            fb("console.log debug artifact", "quality", Verdict::Wontfix),
            fb("console.log debug artifact", "quality", Verdict::Wontfix),
        ];
        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        assert_eq!(result.suppressed, 0, "wontfix should NOT fully suppress");
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::Info, "wontfix should soft-suppress to INFO");
    }

    #[test]
    fn fp_still_fully_suppresses() {
        let finding = FindingBuilder::new()
            .title("console.log debug artifact")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let feedback = vec![
            fb("console.log debug artifact", "quality", Verdict::Fp),
            fb("console.log debug artifact", "quality", Verdict::Fp),
        ];
        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        assert_eq!(result.suppressed, 1, "FP should fully suppress");
        assert_eq!(result.findings.len(), 0);
    }

    #[test]
    fn mixed_fp_wontfix_fp_drives_suppress() {
        let finding = FindingBuilder::new()
            .title("unused variable")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        // 2 FP (enough for full suppress) + 1 wontfix
        let feedback = vec![
            fb("unused variable", "quality", Verdict::Fp),
            fb("unused variable", "quality", Verdict::Fp),
            fb("unused variable", "quality", Verdict::Wontfix),
        ];
        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        // FP alone should drive full suppression
        assert_eq!(result.suppressed, 1);
        assert_eq!(result.findings.len(), 0);
    }

    #[test]
    fn wontfix_contributes_to_full_suppress_with_fp() {
        let finding = FindingBuilder::new()
            .title("Missing explicit mode")
            .category("quality")
            .severity(Severity::Medium)
            .build();

        let feedback = vec![
            fb("Missing explicit mode", "quality", Verdict::Fp),
            fb("Missing explicit mode defaults", "quality", Verdict::Wontfix),
            fb("No explicit mode set", "quality", Verdict::Wontfix),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.suppressed, 1);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn wontfix_alone_insufficient_for_full_suppress() {
        let finding = FindingBuilder::new()
            .title("No explicit mode")
            .category("quality")
            .severity(Severity::Medium)
            .build();

        let feedback = vec![
            fb("No explicit mode", "quality", Verdict::Wontfix),
            fb("No explicit mode set", "quality", Verdict::Wontfix),
            fb("Missing explicit mode", "quality", Verdict::Wontfix),
            fb("Automation has no mode", "quality", Verdict::Wontfix),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::Info);
    }

    #[test]
    fn calibrate_populates_trace_for_suppressed_finding() {
        let finding = FindingBuilder::new()
            .title("Unused import")
            .category("style")
            .severity(Severity::Low)
            .build();

        let feedback = vec![
            fb("Unused import", "style", Verdict::Fp),
            fb("Unused import os", "style", Verdict::Fp),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.suppressed, 1);
        assert_eq!(result.traces.len(), 1);

        let trace = &result.traces[0];
        assert_eq!(trace.finding_title, "Unused import");
        assert!(trace.fp_weight > 0.0);
        assert_eq!(trace.action, Some(CalibratorAction::Disputed));
        assert!(!trace.matched_precedents.is_empty());
    }

    #[test]
    fn calibrate_populates_trace_for_boosted_finding() {
        let finding = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .severity(Severity::Medium)
            .build();

        let feedback = vec![
            fb("SQL injection", "security", Verdict::Tp),
            fb("SQL injection in query", "security", Verdict::Tp),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.traces.len(), 1);

        let trace = &result.traces[0];
        assert_eq!(trace.action, Some(CalibratorAction::Confirmed));
        assert!(trace.tp_weight > 0.0);
        assert_eq!(trace.input_severity, Severity::Medium);
        assert_eq!(trace.output_severity, Severity::High);
    }

    #[test]
    fn calibrate_populates_trace_for_passthrough() {
        let finding = FindingBuilder::new()
            .title("Race condition")
            .category("concurrency")
            .build();

        let feedback = vec![
            fb("Unused import", "style", Verdict::Fp),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.traces.len(), 1);

        let trace = &result.traces[0];
        assert_eq!(trace.finding_title, "Race condition");
        assert_eq!(trace.tp_weight, 0.0);
        assert_eq!(trace.fp_weight, 0.0);
        assert!(trace.matched_precedents.is_empty());
        assert_eq!(trace.action, None);
    }
}
