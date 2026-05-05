/// Per-source TP/FP analytics computed from the feedback store.

use std::collections::HashMap;
use chrono::{DateTime, Utc};

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
            // ContextMisleading is a retrieval signal, not a finding-quality verdict;
            // excluded from per-model TP/FP analytics.
            Verdict::ContextMisleading { .. } => {}
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

#[derive(Debug, Clone)]
pub struct PrecisionWindow {
    pub week_start: DateTime<Utc>,
    pub precision: f64,
    pub count: usize,
}

/// Tier-level aggregation by `Provenance`. Parallel to (and does not replace)
/// `compute_stats`, which aggregates by reviewer model.
#[derive(Debug, Clone, Default)]
pub struct TierSummary {
    pub human: SourceStats,
    pub post_fix: SourceStats,
    pub auto_calibrate: SourceStats,
    pub external: ExternalTierStats,
    pub unknown: SourceStats,
}

#[derive(Debug, Clone, Default)]
pub struct ExternalTierStats {
    pub total: SourceStats,
    /// Per-agent breakdown, sorted desc by total count.
    pub per_agent: Vec<(String, SourceStats)>,
}

pub fn compute_tier_stats(entries: &[FeedbackEntry]) -> TierSummary {
    use crate::feedback::Provenance;
    let mut summary = TierSummary::default();
    let mut per_agent: HashMap<String, SourceStats> = HashMap::new();

    fn bump(s: &mut SourceStats, v: &Verdict) {
        match v {
            Verdict::Tp => s.tp += 1,
            Verdict::Fp => s.fp += 1,
            Verdict::Partial => s.partial += 1,
            Verdict::Wontfix => s.wontfix += 1,
            // Retrieval-quality signal; excluded from finding TP/FP tallies.
            Verdict::ContextMisleading { .. } => {}
        }
    }

    for entry in entries {
        match &entry.provenance {
            Provenance::Human => bump(&mut summary.human, &entry.verdict),
            Provenance::PostFix => bump(&mut summary.post_fix, &entry.verdict),
            Provenance::AutoCalibrate(_) => bump(&mut summary.auto_calibrate, &entry.verdict),
            Provenance::External { agent, .. } => {
                bump(&mut summary.external.total, &entry.verdict);
                bump(per_agent.entry(agent.clone()).or_default(), &entry.verdict);
            }
            Provenance::Unknown => bump(&mut summary.unknown, &entry.verdict),
        }
    }

    let mut agents: Vec<(String, SourceStats)> = per_agent.into_iter().collect();
    agents.sort_by(|a, b| {
        b.1.total()
            .cmp(&a.1.total())
            .then_with(|| a.0.cmp(&b.0))
    });
    summary.external.per_agent = agents;
    summary
}

pub fn format_tier_report(summary: &TierSummary) -> String {
    let mut lines = Vec::new();
    lines.push("Feedback by provenance tier:".into());
    lines.push("-".repeat(65));
    let rows: [(&str, &SourceStats); 4] = [
        ("Human      ", &summary.human),
        ("PostFix    ", &summary.post_fix),
        ("External   ", &summary.external.total),
        ("AutoCalib  ", &summary.auto_calibrate),
    ];
    for (label, s) in rows {
        lines.push(format!(
            "{label}: {:>5} total  (tp {:>3}  fp {:>3}  partial {:>2}  wontfix {:>2})  {:>5.0}% prec",
            s.total(), s.tp, s.fp, s.partial, s.wontfix, s.precision() * 100.0
        ));
    }
    if !summary.external.per_agent.is_empty() {
        let top: Vec<String> = summary
            .external
            .per_agent
            .iter()
            .take(3)
            .map(|(name, s)| format!("{name} ({})", s.total()))
            .collect();
        lines.push(format!("    top agents: {}", top.join(", ")));
    }
    if summary.unknown.total() > 0 {
        let s = &summary.unknown;
        lines.push(format!(
            "Unknown    : {:>5} total  (legacy rows with no provenance field)",
            s.total()
        ));
    }
    lines.join("\n")
}

/// Compute rolling precision over time windows.
/// Requires minimum 10 entries per window to report.
pub fn precision_trend(entries: &[FeedbackEntry], window_days: i64) -> Vec<PrecisionWindow> {
    if entries.is_empty() || window_days <= 0 {
        return vec![];
    }

    let min_entries = 10;
    let mut sorted: Vec<&FeedbackEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.timestamp);

    let first_ts = sorted.first().unwrap().timestamp;
    let last_ts = sorted.last().unwrap().timestamp;
    let mut windows = Vec::new();

    let mut window_start = first_ts;
    while window_start <= last_ts {
        let window_end = window_start + chrono::Duration::days(window_days);
        let window_entries: Vec<_> = sorted.iter()
            .filter(|e| e.timestamp >= window_start && e.timestamp < window_end)
            .collect();

        if window_entries.len() >= min_entries {
            let mut tp = 0usize;
            let mut partial = 0usize;
            let mut fp = 0usize;
            for e in &window_entries {
                match e.verdict {
                    Verdict::Tp => tp += 1,
                    Verdict::Partial => partial += 1,
                    Verdict::Fp => fp += 1,
                    Verdict::Wontfix => {}
                    // Retrieval-quality signal; not part of rolling precision.
                    Verdict::ContextMisleading { .. } => {}
                }
            }
            let relevant = tp + partial;
            let total = relevant + fp;
            let precision = if total > 0 { relevant as f64 / total as f64 } else { 0.0 };
            windows.push(PrecisionWindow {
                week_start: window_start,
                precision,
                count: window_entries.len(),
            });
        }

        window_start = window_end;
    }

    windows
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
            fp_kind: None,
            finding_id: None,
            rule_id: None,
        }
    }

    fn entry_with(provenance: crate::feedback::Provenance, verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: "c".into(),
            verdict,
            reason: "r".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance,
            fp_kind: None,
            finding_id: None,
            rule_id: None,
        }
    }

    #[test]
    fn tier_stats_group_by_provenance() {
        use crate::feedback::Provenance;
        let fb = vec![
            entry_with(Provenance::Human, Verdict::Tp),
            entry_with(Provenance::Human, Verdict::Fp),
            entry_with(Provenance::PostFix, Verdict::Tp),
            entry_with(
                Provenance::External { agent: "pal".into(), model: None, confidence: None },
                Verdict::Tp,
            ),
            entry_with(
                Provenance::External { agent: "pal".into(), model: None, confidence: None },
                Verdict::Fp,
            ),
            entry_with(
                Provenance::External {
                    agent: "third-opinion".into(),
                    model: None,
                    confidence: None,
                },
                Verdict::Tp,
            ),
        ];
        let summary = compute_tier_stats(&fb);
        assert_eq!(summary.human.total(), 2);
        assert_eq!(summary.human.tp, 1);
        assert_eq!(summary.human.fp, 1);
        assert_eq!(summary.post_fix.total(), 1);
        assert_eq!(summary.external.total.total(), 3);
        assert_eq!(summary.external.total.tp, 2);
        assert_eq!(summary.external.total.fp, 1);
        assert_eq!(summary.external.per_agent[0].0, "pal");
        assert_eq!(summary.external.per_agent[0].1.total(), 2);
        assert_eq!(summary.external.per_agent[1].0, "third-opinion");
        assert_eq!(summary.external.per_agent[1].1.total(), 1);
    }

    #[test]
    fn tier_stats_format_shows_external_and_top_agents_stable() {
        use crate::feedback::Provenance;
        let fb = vec![
            entry_with(
                Provenance::External { agent: "pal".into(), model: None, confidence: None },
                Verdict::Tp,
            ),
            entry_with(
                Provenance::External { agent: "pal".into(), model: None, confidence: None },
                Verdict::Tp,
            ),
            entry_with(
                Provenance::External {
                    agent: "third-opinion".into(),
                    model: None,
                    confidence: None,
                },
                Verdict::Fp,
            ),
        ];
        let summary = compute_tier_stats(&fb);

        // Data contract (stable).
        assert_eq!(summary.external.total.total(), 3);
        assert_eq!(summary.external.per_agent.len(), 2);
        assert_eq!(summary.external.per_agent[0].0, "pal");
        assert_eq!(summary.external.per_agent[1].0, "third-opinion");

        // Format contract: sub-line lists agents with counts.
        let report = format_tier_report(&summary);
        let re = regex::Regex::new(r"top agents:\s+pal\s*\(\d+\).*third-opinion\s*\(\d+\)")
            .unwrap();
        assert!(
            re.is_match(&report),
            "sub-line format must list agents with counts: {report}"
        );
    }

    #[test]
    fn format_tier_report_handles_zero_external_entries() {
        use crate::feedback::Provenance;
        let fb = vec![entry_with(Provenance::Human, Verdict::Tp)];
        let summary = compute_tier_stats(&fb);
        assert_eq!(summary.external.total.total(), 0);
        assert!(summary.external.per_agent.is_empty());

        let report = format_tier_report(&summary);
        assert!(
            !report.contains("top agents:"),
            "must not emit empty 'top agents:' when no external entries: {report}"
        );
        assert!(
            report.contains("External"),
            "External row should still appear (with 0 total): {report}"
        );
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

    // -- precision_trend --

    #[test]
    fn precision_trend_by_week() {
        use chrono::Duration;
        let now = Utc::now();
        let mut entries = Vec::new();

        // Week 1: 5 TP, 5 FP = 50% precision (need 10 min)
        for _ in 0..5 {
            let mut e = entry("model", Verdict::Tp);
            e.timestamp = now - Duration::days(20);
            entries.push(e);
        }
        for _ in 0..5 {
            let mut e = entry("model", Verdict::Fp);
            e.timestamp = now - Duration::days(20);
            entries.push(e);
        }

        // Week 2: 8 TP, 2 FP = 80% precision (need 10 min)
        for _ in 0..8 {
            let mut e = entry("model", Verdict::Tp);
            e.timestamp = now - Duration::days(5);
            entries.push(e);
        }
        for _ in 0..2 {
            let mut e = entry("model", Verdict::Fp);
            e.timestamp = now - Duration::days(5);
            entries.push(e);
        }

        let trend = precision_trend(&entries, 7);
        assert!(trend.len() >= 2);
        let first = trend.first().unwrap();
        let last = trend.last().unwrap();
        assert!((first.precision - 0.5).abs() < 0.1);
        assert!((last.precision - 0.8).abs() < 0.1);
    }

    #[test]
    fn precision_trend_skips_sparse_windows() {
        let entries = vec![entry("model", Verdict::Tp)]; // only 1 entry
        let trend = precision_trend(&entries, 7);
        assert!(trend.is_empty()); // not enough data
    }
}
