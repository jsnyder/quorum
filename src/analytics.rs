/// Per-source TP/FP analytics computed from the feedback store.

use std::collections::HashMap;

use crate::feedback::{FeedbackEntry, Verdict};

#[derive(Debug, Clone, Default)]
pub struct SourceStats {
    pub tp: usize,
    pub fp: usize,
    pub partial: usize,
    pub wontfix: usize,
}

impl SourceStats {
    pub fn total(&self) -> usize {
        self.tp + self.fp + self.partial + self.wontfix
    }

    pub fn precision(&self) -> f64 {
        let relevant = self.tp + self.partial;
        let total = relevant + self.fp;
        if total == 0 {
            0.0
        } else {
            relevant as f64 / total as f64
        }
    }
}

pub fn compute_stats(entries: &[FeedbackEntry]) -> HashMap<String, SourceStats> {
    let mut stats: HashMap<String, SourceStats> = HashMap::new();
    for entry in entries {
        let source = entry.model.as_deref().unwrap_or("unknown").to_string();
        let s = stats.entry(source).or_default();
        match entry.verdict {
            Verdict::Tp => s.tp += 1,
            Verdict::Fp => s.fp += 1,
            Verdict::Partial => s.partial += 1,
            Verdict::Wontfix => s.wontfix += 1,
        }
    }
    stats
}

pub fn format_stats_report(stats: &HashMap<String, SourceStats>) -> String {
    if stats.is_empty() {
        return "No feedback data recorded yet.".into();
    }

    let mut lines = Vec::new();
    lines.push("Source             TP   FP  Partial  Wontfix  Total  Precision".into());
    lines.push("-".repeat(65));

    let mut sources: Vec<_> = stats.iter().collect();
    sources.sort_by(|a, b| b.1.total().cmp(&a.1.total()));

    for (source, s) in &sources {
        lines.push(format!(
            "{:<18} {:>3}  {:>3}  {:>7}  {:>7}  {:>5}  {:>6.0}%",
            source, s.tp, s.fp, s.partial, s.wontfix, s.total(),
            s.precision() * 100.0
        ));
    }

    let total_tp: usize = sources.iter().map(|(_, s)| s.tp).sum();
    let total_fp: usize = sources.iter().map(|(_, s)| s.fp).sum();
    let total: usize = sources.iter().map(|(_, s)| s.total()).sum();
    lines.push("-".repeat(65));
    lines.push(format!("Total: {} entries ({} TP, {} FP)", total, total_tp, total_fp));

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(model: &str, verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "security".into(),
            verdict,
            reason: "test".into(),
            model: Some(model.into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Unknown,
        }
    }

    #[test]
    fn stats_empty_entries() {
        let stats = compute_stats(&[]);
        assert!(stats.is_empty());
    }

    #[test]
    fn stats_single_source() {
        let entries = vec![
            entry("gpt-5.4", Verdict::Tp),
            entry("gpt-5.4", Verdict::Tp),
            entry("gpt-5.4", Verdict::Fp),
        ];
        let stats = compute_stats(&entries);
        let s = &stats["gpt-5.4"];
        assert_eq!(s.tp, 2);
        assert_eq!(s.fp, 1);
        assert_eq!(s.total(), 3);
    }

    #[test]
    fn stats_multiple_sources() {
        let entries = vec![
            entry("gpt-5.4", Verdict::Tp),
            entry("claude", Verdict::Fp),
            entry("gpt-5.4", Verdict::Fp),
            entry("claude", Verdict::Tp),
        ];
        let stats = compute_stats(&entries);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats["gpt-5.4"].tp, 1);
        assert_eq!(stats["claude"].fp, 1);
    }

    #[test]
    fn stats_precision_calculation() {
        let mut s = SourceStats::default();
        s.tp = 8;
        s.fp = 2;
        assert!((s.precision() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_precision_all_fp() {
        let mut s = SourceStats::default();
        s.fp = 5;
        assert!((s.precision() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_precision_empty() {
        let s = SourceStats::default();
        assert!((s.precision() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_partial_counts_as_relevant() {
        let mut s = SourceStats::default();
        s.tp = 3;
        s.partial = 2;
        s.fp = 5;
        // precision = (3+2) / (3+2+5) = 0.5
        assert!((s.precision() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_entries_without_model() {
        let mut e = entry("", Verdict::Tp);
        e.model = None;
        let stats = compute_stats(&[e]);
        assert!(stats.contains_key("unknown"));
    }

    #[test]
    fn format_report_contains_source_names() {
        let entries = vec![
            entry("gpt-5.4", Verdict::Tp),
            entry("gpt-5.4", Verdict::Fp),
            entry("local-ast", Verdict::Tp),
        ];
        let stats = compute_stats(&entries);
        let report = format_stats_report(&stats);
        assert!(report.contains("gpt-5.4"));
        assert!(report.contains("local-ast"));
    }

    #[test]
    fn format_report_shows_precision() {
        let entries = vec![
            entry("test-model", Verdict::Tp),
            entry("test-model", Verdict::Fp),
        ];
        let stats = compute_stats(&entries);
        let report = format_stats_report(&stats);
        assert!(report.contains("50")); // 50% precision
    }

    #[test]
    fn format_report_empty() {
        let stats = compute_stats(&[]);
        let report = format_stats_report(&stats);
        assert!(report.contains("No feedback"));
    }
}
