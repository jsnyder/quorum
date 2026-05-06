use chrono::{DateTime, Utc};
/// Per-source TP/FP analytics computed from the feedback store.
use std::collections::{HashMap, HashSet};

use crate::feedback::{FeedbackEntry, Verdict};
use crate::review_log::ReviewRecord;

/// Reviews ↔ feedback linkage health.
///
/// Counts feedback entries that have a `finding_id` matching some review's
/// `finding_ids` list. Used by the headline trend to decide whether
/// per-finding precision math is trustworthy (≥85% linked) or should
/// fall back to entry-level math with a banner. Also surfaced directly
/// via `quorum stats --join-health`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkageStats {
    pub linked: usize,
    pub unlinked: usize,
}

impl LinkageStats {
    /// Fraction of feedback entries that join back to a known finding.
    /// Returns 0.0 for an empty corpus rather than NaN.
    pub fn rate(&self) -> f64 {
        let total = self.linked + self.unlinked;
        if total == 0 {
            0.0
        } else {
            self.linked as f64 / total as f64
        }
    }
}

/// Compute reviews ↔ feedback linkage statistics.
///
/// O(R + F) where R = total finding_ids across reviews and F = feedback
/// entries. Builds a HashSet of known finding_ids so duplicate IDs in the
/// review log don't inflate the linked count.
pub fn linkage_stats(reviews: &[ReviewRecord], feedback: &[FeedbackEntry]) -> LinkageStats {
    use crate::feedback::Provenance;

    let known: HashSet<&str> = reviews
        .iter()
        .flat_map(|r| r.finding_ids.iter().map(String::as_str))
        .collect();

    let mut stats = LinkageStats::default();
    for entry in feedback {
        if !matches!(&entry.provenance, Provenance::Human | Provenance::PostFix) {
            continue;
        }
        match &entry.finding_id {
            Some(fid) if known.contains(fid.as_str()) => stats.linked += 1,
            _ => stats.unlinked += 1,
        }
    }
    stats
}

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
            source,
            s.tp,
            s.fp,
            s.partial,
            s.wontfix,
            s.total(),
            s.precision() * 100.0
        ));
    }

    let total_tp: usize = sources.iter().map(|(_, s)| s.tp).sum();
    let total_fp: usize = sources.iter().map(|(_, s)| s.fp).sum();
    let total: usize = sources.iter().map(|(_, s)| s.total()).sum();
    lines.push("-".repeat(65));
    lines.push(format!(
        "Total: {} entries ({} TP, {} FP)",
        total, total_tp, total_fp
    ));

    lines.join("\n")
}

#[derive(Debug, Clone)]
pub struct PrecisionWindow {
    pub week_start: DateTime<Utc>,
    pub precision: f64,
    pub count: usize,
    pub precision_denom: usize,
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
    agents.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(&b.0)));
    summary.external.per_agent = agents;
    summary
}

#[deprecated(
    since = "0.19.0",
    note = "Use `format_channel_attribution` instead. Tier-precision rollups are misleading: External and AutoCalibrate aren't comparable to Human/PostFix on a precision axis."
)]
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
        let window_entries: Vec<_> = sorted
            .iter()
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
            let precision = if total > 0 {
                relevant as f64 / total as f64
            } else {
                0.0
            };
            windows.push(PrecisionWindow {
                week_start: window_start,
                precision,
                count: window_entries.len(),
                precision_denom: total,
            });
        }

        window_start = window_end;
    }

    windows
}

/// Channel attribution table — replaces format_tier_report.
///
/// Reports counts only (no precision column) per provenance channel.
/// Precision belongs on the headline trend, not the channel rollup —
/// External and AutoCalibrate aren't comparable to Human/PostFix on a
/// precision axis (different sampling distributions, different signal
/// quality).
///
/// Layout per DESIGN.md §4.x: thin dim `─` rule under header only;
/// numeric columns right-aligned; empty cells render as em-dash.
pub fn format_channel_attribution(summary: &TierSummary) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let rows: [(&str, &SourceStats); 4] = [
        ("Human", &summary.human),
        ("PostFix", &summary.post_fix),
        ("External", &summary.external.total),
        ("AutoCalib", &summary.auto_calibrate),
    ];

    // Header.
    writeln!(
        out,
        "  {label:<10}  {total:>6}  {tp:>5}  {fp:>5}  {part:>5}  {wfix:>5}",
        label = "Channel",
        total = "Total",
        tp = "TP",
        fp = "FP",
        part = "Part",
        wfix = "Wfix",
    )
    .unwrap();
    // Single dim rule under header only.
    writeln!(
        out,
        "  {}",
        "─".repeat(10 + 2 + 6 + 2 + 5 + 2 + 5 + 2 + 5 + 2 + 5)
    )
    .unwrap();

    fn cell(n: usize) -> String {
        if n == 0 {
            "—".to_string()
        } else {
            n.to_string()
        }
    }

    for (label, s) in rows {
        let total = s.total();
        if total == 0 && label != "Human" {
            continue; // hide empty channels except Human (always shown)
        }
        writeln!(
            out,
            "  {label:<10}  {total:>6}  {tp:>5}  {fp:>5}  {part:>5}  {wfix:>5}",
            label = label,
            total = if total == 0 {
                "—".to_string()
            } else {
                total.to_string()
            },
            tp = cell(s.tp),
            fp = cell(s.fp),
            part = cell(s.partial),
            wfix = cell(s.wontfix),
        )
        .unwrap();
    }

    if summary.unknown.total() > 0 {
        let s = &summary.unknown;
        writeln!(
            out,
            "  {label:<10}  {total:>6}  (legacy rows, no provenance field)",
            label = "Unknown",
            total = s.total(),
        )
        .unwrap();
    }

    out
}

/// External-agent corpus overlap with quorum's own verdicts.
///
/// `per_agent[i].findings`  — total External entries for that agent in the corpus
/// `per_agent[i].overlap`   — entries whose finding_id is also in `quorum_verdicts`
/// `per_agent[i].agree`     — overlap entries where the External verdict matches
///                            the quorum verdict on the same finding
///
/// Surfaced in the External corpus block so users can read agent
/// contribution without it polluting headline precision.
#[derive(Debug, Clone, Default)]
pub struct ExternalOverlap {
    pub per_agent: Vec<AgentOverlap>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentOverlap {
    pub agent: String,
    pub findings: usize,
    pub overlap: usize,
    pub agree: usize,
}

impl AgentOverlap {
    /// Fraction of overlap entries where External and quorum agreed.
    /// Returns 0.0 (not NaN) when there's no overlap to compute on.
    pub fn agreement_rate(&self) -> f64 {
        if self.overlap == 0 {
            0.0
        } else {
            self.agree as f64 / self.overlap as f64
        }
    }
}

/// Compute External-agent overlap against a map of quorum verdicts keyed
/// by finding_id. The quorum-side map is provided externally so callers
/// can build it however suits them (e.g. the Human path for headline
/// math, or PostFix-confirmed for stricter ground truth). The function
/// itself is verdict-source-agnostic.
pub fn compute_external_overlap(
    entries: &[FeedbackEntry],
    quorum_verdicts: &HashMap<String, Verdict>,
) -> ExternalOverlap {
    use crate::feedback::Provenance;
    let mut per_agent: HashMap<String, AgentOverlap> = HashMap::new();
    for e in entries {
        let Provenance::External { agent, .. } = &e.provenance else {
            continue;
        };
        let row = per_agent
            .entry(agent.clone())
            .or_insert_with(|| AgentOverlap {
                agent: agent.clone(),
                ..Default::default()
            });
        row.findings += 1;
        if let Some(fid) = &e.finding_id
            && let Some(qv) = quorum_verdicts.get(fid) {
                row.overlap += 1;
                if verdict_eq(qv, &e.verdict) {
                    row.agree += 1;
                }
            }
    }
    let mut agents: Vec<AgentOverlap> = per_agent.into_values().collect();
    agents.sort_by(|a, b| {
        b.findings
            .cmp(&a.findings)
            .then_with(|| a.agent.cmp(&b.agent))
    });
    ExternalOverlap { per_agent: agents }
}

/// Verdict equivalence for agreement counting. Treats Tp/Partial as
/// "real" and Fp as "not real" — Wontfix and ContextMisleading are
/// counted as not-equal to anything since they're not finding-quality
/// verdicts.
fn verdict_eq(a: &Verdict, b: &Verdict) -> bool {
    fn category(v: &Verdict) -> Option<u8> {
        match v {
            Verdict::Tp | Verdict::Partial => Some(0),
            Verdict::Fp => Some(1),
            Verdict::Wontfix | Verdict::ContextMisleading { .. } => None,
        }
    }
    match (category(a), category(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Per-finding precision trend with disposition precedence.
///
/// Same window-bucketing as `precision_trend`, but feedback entries that
/// share a `finding_id` collapse to a single row using the precedence
/// `Human > PostFix > drop`. External and AutoCalibrate are excluded
/// from the pool entirely (they're a different channel — see Tier
/// breakdown). Legacy entries (`finding_id == None`) cannot be
/// deduplicated against and are skipped.
///
/// This is the headline trend once linkage rate ≥85%; below that, the
/// dashboard falls back to entry-level `precision_trend` with a banner.
pub fn precision_trend_per_finding(
    entries: &[FeedbackEntry],
    window_days: i64,
) -> Vec<PrecisionWindow> {
    use crate::feedback::Provenance;

    if entries.is_empty() || window_days <= 0 {
        return vec![];
    }

    // Filter to Human/PostFix entries with a finding_id, then dedup by
    // finding_id with Human winning over PostFix. Earliest timestamp wins
    // among same-tier entries so the resulting row sits in a stable window.
    let mut keep: HashMap<String, FeedbackEntry> = HashMap::new();
    for e in entries {
        let Some(fid) = &e.finding_id else { continue };
        let tier_rank = match &e.provenance {
            Provenance::Human => 0,
            Provenance::PostFix => 1,
            Provenance::External { .. } | Provenance::AutoCalibrate(_) | Provenance::Unknown => {
                continue;
            }
        };
        match keep.get(fid) {
            None => {
                keep.insert(fid.clone(), e.clone());
            }
            Some(prev) => {
                let prev_rank = match &prev.provenance {
                    Provenance::Human => 0,
                    Provenance::PostFix => 1,
                    _ => unreachable!("only Human/PostFix enter the map"),
                };
                if tier_rank < prev_rank || (tier_rank == prev_rank && e.timestamp < prev.timestamp)
                {
                    keep.insert(fid.clone(), e.clone());
                }
            }
        }
    }

    let deduped: Vec<FeedbackEntry> = keep.into_values().collect();
    precision_trend(&deduped, window_days)
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
                Provenance::External {
                    agent: "pal".into(),
                    model: None,
                    confidence: None,
                },
                Verdict::Tp,
            ),
            entry_with(
                Provenance::External {
                    agent: "pal".into(),
                    model: None,
                    confidence: None,
                },
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
    #[allow(deprecated)]
    fn tier_stats_format_shows_external_and_top_agents_stable() {
        use crate::feedback::Provenance;
        let fb = vec![
            entry_with(
                Provenance::External {
                    agent: "pal".into(),
                    model: None,
                    confidence: None,
                },
                Verdict::Tp,
            ),
            entry_with(
                Provenance::External {
                    agent: "pal".into(),
                    model: None,
                    confidence: None,
                },
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
        let re =
            regex::Regex::new(r"top agents:\s+pal\s*\(\d+\).*third-opinion\s*\(\d+\)").unwrap();
        assert!(
            re.is_match(&report),
            "sub-line format must list agents with counts: {report}"
        );
    }

    #[test]
    #[allow(deprecated)]
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
        let s = SourceStats {
            tp: 8,
            fp: 2,
            ..Default::default()
        };
        assert!((s.precision() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_precision_all_fp() {
        let s = SourceStats {
            fp: 5,
            ..Default::default()
        };
        assert!((s.precision() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_precision_empty() {
        let s = SourceStats::default();
        assert!((s.precision() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_partial_counts_as_relevant() {
        let s = SourceStats {
            tp: 3,
            partial: 2,
            fp: 5,
            ..Default::default()
        };
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

    #[test]
    fn precision_denom_excludes_wontfix() {
        let now = Utc::now();
        let mut entries = Vec::new();
        for _ in 0..8 {
            let mut e = entry("model", Verdict::Tp);
            e.timestamp = now - chrono::Duration::days(3);
            entries.push(e);
        }
        for _ in 0..2 {
            let mut e = entry("model", Verdict::Fp);
            e.timestamp = now - chrono::Duration::days(3);
            entries.push(e);
        }
        for _ in 0..5 {
            let mut e = entry("model", Verdict::Wontfix);
            e.timestamp = now - chrono::Duration::days(3);
            entries.push(e);
        }
        let trend = precision_trend(&entries, 7);
        assert!(!trend.is_empty());
        let w = &trend[0];
        assert_eq!(w.count, 15, "count includes all entries");
        assert_eq!(w.precision_denom, 10, "precision_denom excludes wontfix");
        assert!((w.precision - 0.8).abs() < 1e-9);
    }

    // ─── Stats redesign Phase 0: linkage_stats ───
    //
    // Counts feedback entries that have a finding_id matching some review's
    // finding_ids list. The linkage rate gates whether the headline trend
    // can compute per-finding precision (≥85%) or must fall back to entry-
    // level math with a banner.

    fn review_with_finding_ids(ids: &[&str]) -> crate::review_log::ReviewRecord {
        crate::review_log::ReviewRecord {
            run_id: crate::review_log::ReviewRecord::new_ulid(),
            timestamp: Utc::now(),
            quorum_version: "0.1".into(),
            repo: None,
            invoked_from: "tty".into(),
            model: "test".into(),
            files_reviewed: 1,
            lines_added: None,
            lines_removed: None,
            findings_by_severity: crate::review_log::SeverityCounts::default(),
            suppressed_by_rule: HashMap::new(),
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            duration_ms: 0,
            flags: crate::review_log::Flags::default(),
            context: crate::review_log::ContextTelemetry::default(),
            finding_ids: ids.iter().map(|s| s.to_string()).collect(),
            mode: None,
        }
    }

    fn fb_with_finding_id(id: &str) -> FeedbackEntry {
        let mut e = entry_with(crate::feedback::Provenance::Human, Verdict::Tp);
        e.finding_id = Some(id.into());
        e
    }

    #[test]
    fn linkage_with_zero_reviews_and_zero_feedback_returns_zero_rate() {
        let stats = linkage_stats(&[], &[]);
        assert_eq!(stats.linked, 0);
        assert_eq!(stats.unlinked, 0);
        assert_eq!(stats.rate(), 0.0);
    }

    #[test]
    fn linkage_full_when_every_feedback_has_matching_finding_id() {
        let reviews = vec![review_with_finding_ids(&["A", "B"])];
        let feedback = vec![fb_with_finding_id("A"), fb_with_finding_id("B")];
        let stats = linkage_stats(&reviews, &feedback);
        assert_eq!(stats.linked, 2);
        assert_eq!(stats.unlinked, 0);
        assert!((stats.rate() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn linkage_partial_with_legacy_entries_in_corpus() {
        let reviews = vec![review_with_finding_ids(&["A", "B"])];
        let mut legacy = entry_with(crate::feedback::Provenance::Human, Verdict::Tp);
        legacy.finding_id = None;
        let feedback = vec![fb_with_finding_id("A"), legacy];
        let stats = linkage_stats(&reviews, &feedback);
        assert_eq!(stats.linked, 1);
        assert_eq!(stats.unlinked, 1);
        assert!((stats.rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn linkage_dangling_finding_id_counts_as_unlinked() {
        // A feedback entry references a finding_id not present in any
        // review's finding_ids — counts as unlinked, not as match-error.
        let reviews = vec![review_with_finding_ids(&["A"])];
        let feedback = vec![fb_with_finding_id("Z-NONEXISTENT")];
        let stats = linkage_stats(&reviews, &feedback);
        assert_eq!(stats.linked, 0);
        assert_eq!(stats.unlinked, 1);
    }

    #[test]
    fn linkage_duplicate_finding_id_in_reviews_does_not_double_count() {
        // Pathological case: the same finding_id appears in two reviews.
        // The HashSet lookup means we count the feedback entry once.
        let reviews = vec![
            review_with_finding_ids(&["A"]),
            review_with_finding_ids(&["A"]),
        ];
        let feedback = vec![fb_with_finding_id("A")];
        let stats = linkage_stats(&reviews, &feedback);
        assert_eq!(stats.linked, 1);
        assert_eq!(stats.unlinked, 0);
    }

    // ─── Stats redesign Task 7: per-finding precision trend ───
    //
    // Same window-bucketing as `precision_trend`, but feedback entries that
    // share a finding_id are deduplicated under a fixed precedence:
    // Human > PostFix > drop. External and AutoCalibrate are excluded
    // (different channel; counted separately under Tier breakdown). Legacy
    // entries (finding_id == None) cannot be deduplicated against and are
    // skipped entirely so per-finding precision reflects only linkable data.

    fn fb_with_id_and_provenance(
        id: &str,
        verdict: Verdict,
        provenance: crate::feedback::Provenance,
    ) -> FeedbackEntry {
        let mut e = entry_with(provenance, verdict);
        e.finding_id = Some(id.into());
        e
    }

    #[test]
    fn per_finding_dedups_human_plus_postfix_on_same_finding_id() {
        use crate::feedback::Provenance;
        // Same finding gets a Human TP and a PostFix TP — Human wins, count
        // once. (Without dedup, the trend over-counts confirmed findings
        // because every TP gets a "yes I fixed it" PostFix companion.)
        let entries = vec![
            fb_with_id_and_provenance("A", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("A", Verdict::Tp, Provenance::PostFix),
            fb_with_id_and_provenance("B", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("C", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("D", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("E", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("F", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("G", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("H", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("I", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("J", Verdict::Tp, Provenance::Human),
        ];
        let trend = precision_trend_per_finding(&entries, 7);
        assert_eq!(trend.len(), 1);
        // 10 distinct findings (A..J), all TP after dedup. Clears the
        // min_entries=10 gate inherited from precision_trend.
        assert_eq!(trend[0].count, 10);
        assert!((trend[0].precision - 1.0).abs() < 1e-9);
    }

    #[test]
    fn per_finding_human_takes_precedence_over_postfix() {
        use crate::feedback::Provenance;
        // Human FP on finding A; PostFix TP on finding A. Human wins —
        // resulting verdict for A is FP. (PostFix without prior Human is a
        // weak signal; if Human says FP, that overrides the auto-fix
        // confirmation.)
        let mut entries = vec![
            fb_with_id_and_provenance("A", Verdict::Fp, Provenance::Human),
            fb_with_id_and_provenance("A", Verdict::Tp, Provenance::PostFix),
        ];
        // Pad to clear the min_entries=10 gate.
        for i in 0..9 {
            let id = format!("PAD-{i}");
            entries.push(fb_with_id_and_provenance(
                &id,
                Verdict::Tp,
                Provenance::Human,
            ));
        }
        let trend = precision_trend_per_finding(&entries, 7);
        assert_eq!(trend.len(), 1);
        // 1 FP + 9 TP = 10 distinct findings; 9/10 = 0.9 precision.
        assert_eq!(trend[0].count, 10);
        assert!((trend[0].precision - 0.9).abs() < 1e-9);
    }

    #[test]
    fn per_finding_excludes_external_and_auto_calibrate() {
        use crate::feedback::Provenance;
        // External and AutoCalibrate are different channels — they should
        // not enter the per-finding precision pool. Only Human and PostFix
        // count.
        let mut entries: Vec<FeedbackEntry> = (0..10)
            .map(|i| {
                let id = format!("H-{i}");
                fb_with_id_and_provenance(&id, Verdict::Tp, Provenance::Human)
            })
            .collect();
        entries.push(fb_with_id_and_provenance(
            "EXT-1",
            Verdict::Fp,
            Provenance::External {
                agent: "pal".into(),
                model: None,
                confidence: None,
            },
        ));
        entries.push(fb_with_id_and_provenance(
            "AC-1",
            Verdict::Fp,
            Provenance::AutoCalibrate("gpt-5.4".into()),
        ));
        let trend = precision_trend_per_finding(&entries, 7);
        assert_eq!(trend.len(), 1);
        assert_eq!(trend[0].count, 10, "External + AutoCalibrate excluded");
        assert!((trend[0].precision - 1.0).abs() < 1e-9);
    }

    #[test]
    fn per_finding_skips_legacy_entries_without_finding_id() {
        use crate::feedback::Provenance;
        let mut entries: Vec<FeedbackEntry> = (0..10)
            .map(|i| {
                let id = format!("ID-{i}");
                fb_with_id_and_provenance(&id, Verdict::Tp, Provenance::Human)
            })
            .collect();
        // Legacy: no finding_id. Cannot be deduplicated, so excluded.
        entries.push(entry_with(Provenance::Human, Verdict::Fp));
        let trend = precision_trend_per_finding(&entries, 7);
        assert_eq!(trend.len(), 1);
        assert_eq!(trend[0].count, 10);
    }

    #[test]
    fn per_finding_partial_counts_as_relevant_in_precision() {
        use crate::feedback::Provenance;
        // Mirror precision_trend semantics: Partial is "relevant" — it
        // counts in the numerator. Wontfix counts in neither.
        let mut entries = vec![
            fb_with_id_and_provenance("A", Verdict::Partial, Provenance::Human),
            fb_with_id_and_provenance("B", Verdict::Wontfix, Provenance::Human),
            fb_with_id_and_provenance("C", Verdict::Fp, Provenance::Human),
        ];
        for i in 0..7 {
            let id = format!("TP-{i}");
            entries.push(fb_with_id_and_provenance(
                &id,
                Verdict::Tp,
                Provenance::Human,
            ));
        }
        let trend = precision_trend_per_finding(&entries, 7);
        assert_eq!(trend.len(), 1);
        // Distinct findings: A (Partial), B (Wontfix), C (FP), TP-0..6 (TP) = 10.
        // Precision: (Partial + TP) / (Partial + TP + FP) = (1+7) / (1+7+1) = 8/9.
        assert_eq!(trend[0].count, 10);
        assert!((trend[0].precision - (8.0 / 9.0)).abs() < 1e-9);
    }

    #[test]
    fn per_finding_same_tier_keeps_earliest_timestamp() {
        use crate::feedback::Provenance;
        // Two Human entries for F1: earlier is TP, later is FP.
        // Earliest timestamp wins, so F1 resolves to TP.
        let mut later = fb_with_id_and_provenance("F1", Verdict::Fp, Provenance::Human);
        later.timestamp = Utc::now() - chrono::Duration::hours(1);
        let mut earlier = fb_with_id_and_provenance("F1", Verdict::Tp, Provenance::Human);
        earlier.timestamp = Utc::now() - chrono::Duration::hours(3);
        let mut all = vec![later, earlier];
        for i in 0..9 {
            all.push(fb_with_id_and_provenance(
                &format!("P{}", i),
                Verdict::Tp,
                Provenance::Human,
            ));
        }
        let trend = precision_trend_per_finding(&all, 7);
        assert_eq!(trend.len(), 1);
        assert!((trend[0].precision - 1.0).abs() < 1e-9);
    }

    // ─── Stats redesign Task 8: channel attribution table ───

    #[test]
    fn channel_attribution_header_lists_count_columns_only() {
        // No precision column on the channel rollup; precision lives on
        // the headline trend instead.
        let summary = TierSummary::default();
        let out = format_channel_attribution(&summary);
        assert!(out.contains("Total"));
        assert!(out.contains("TP"));
        assert!(out.contains("FP"));
        assert!(out.contains("Part"));
        assert!(out.contains("Wfix"));
        assert!(!out.contains("prec"), "no precision column expected");
        assert!(!out.contains('%'), "no percent rendering expected");
    }

    #[test]
    fn channel_attribution_renders_em_dash_for_zero_cells() {
        // Counts of 0 render as `—` so the eye lands on actual signal,
        // not on rows of zeros.
        let summary = TierSummary::default(); // all zero
        let out = format_channel_attribution(&summary);
        // Human row is always rendered, even when all-zero — its zero
        // cells should be em-dashes.
        let human_line = out.lines().find(|l| l.contains("Human")).unwrap();
        assert!(
            human_line.contains("—"),
            "Human row with zeros should use em-dash: {human_line}"
        );
    }

    #[test]
    fn channel_attribution_uses_single_dim_rule_under_header() {
        let summary = TierSummary::default();
        let out = format_channel_attribution(&summary);
        let rule_count = out.matches("──").count();
        assert!(
            rule_count >= 1,
            "expected at least one box-rule run under header, got {rule_count}"
        );
        // Sanity: no double rule below data rows
        let line_count = out.lines().count();
        assert!(line_count < 12, "too many lines for an empty summary");
    }

    #[test]
    fn channel_attribution_hides_empty_postfix_external_autocalib() {
        // Empty channels (other than Human) are hidden — keeps the table
        // tight before the user has any non-Human verdicts.
        let summary = TierSummary::default();
        let out = format_channel_attribution(&summary);
        assert!(!out.contains("PostFix"));
        assert!(!out.contains("External"));
        assert!(!out.contains("AutoCalib"));
    }

    #[test]
    fn channel_attribution_shows_postfix_when_any_postfix_entries_exist() {
        use crate::feedback::Provenance;
        let entries = vec![entry_with(Provenance::PostFix, Verdict::Tp)];
        let summary = compute_tier_stats(&entries);
        let out = format_channel_attribution(&summary);
        assert!(out.contains("PostFix"), "PostFix should appear: {out}");
    }

    // ─── Stats redesign Task 9: external corpus overlap ───
    //
    // For each external agent, count how many of its findings overlap
    // with quorum's own verdicts (linked by finding_id) and what fraction
    // agree on the disposition. Used by the External corpus block to
    // surface "this agent flags things quorum also flags X% of the time"
    // without conflating the External channel into Human precision.

    fn external_fb(id: &str, verdict: Verdict, agent: &str) -> FeedbackEntry {
        use crate::feedback::Provenance;
        fb_with_id_and_provenance(
            id,
            verdict,
            Provenance::External {
                agent: agent.into(),
                model: None,
                confidence: None,
            },
        )
    }

    #[test]
    fn external_overlap_empty_for_no_external_entries() {
        let entries: Vec<FeedbackEntry> = vec![];
        let quorum: HashMap<String, Verdict> = HashMap::new();
        let overlap = compute_external_overlap(&entries, &quorum);
        assert!(overlap.per_agent.is_empty());
    }

    #[test]
    fn external_overlap_counts_findings_per_agent() {
        let entries = vec![
            external_fb("A", Verdict::Tp, "pal"),
            external_fb("B", Verdict::Fp, "pal"),
            external_fb("C", Verdict::Tp, "third-opinion"),
        ];
        let quorum: HashMap<String, Verdict> = HashMap::new();
        let overlap = compute_external_overlap(&entries, &quorum);
        assert_eq!(overlap.per_agent.len(), 2);
        let pal = overlap.per_agent.iter().find(|a| a.agent == "pal").unwrap();
        assert_eq!(pal.findings, 2);
    }

    #[test]
    fn external_overlap_agreement_rate_against_quorum() {
        // pal flagged A as TP, B as FP. Quorum independently has A=TP and
        // B=TP. Agreement: A matches (1/2), B disagrees (Quorum says TP,
        // pal says FP). Overlap considers the 2 findings pal also has on
        // a quorum-known finding_id; agreement_rate = 1/2.
        let entries = vec![
            external_fb("A", Verdict::Tp, "pal"),
            external_fb("B", Verdict::Fp, "pal"),
        ];
        let mut quorum: HashMap<String, Verdict> = HashMap::new();
        quorum.insert("A".into(), Verdict::Tp);
        quorum.insert("B".into(), Verdict::Tp);
        let overlap = compute_external_overlap(&entries, &quorum);
        let pal = overlap.per_agent.iter().find(|a| a.agent == "pal").unwrap();
        assert_eq!(pal.overlap, 2);
        assert_eq!(pal.agree, 1);
        assert!((pal.agreement_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn external_overlap_skips_external_entries_quorum_did_not_flag() {
        // pal flagged A and Z, but quorum only has A. The Z verdict has no
        // overlap to compute against — count it in `findings` but exclude
        // from `overlap` / `agree`.
        let entries = vec![
            external_fb("A", Verdict::Tp, "pal"),
            external_fb("Z", Verdict::Tp, "pal"),
        ];
        let mut quorum: HashMap<String, Verdict> = HashMap::new();
        quorum.insert("A".into(), Verdict::Tp);
        let overlap = compute_external_overlap(&entries, &quorum);
        let pal = overlap.per_agent.iter().find(|a| a.agent == "pal").unwrap();
        assert_eq!(pal.findings, 2);
        assert_eq!(pal.overlap, 1);
        assert_eq!(pal.agree, 1);
        assert!((pal.agreement_rate() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn external_overlap_agreement_rate_zero_for_no_overlap() {
        // No quorum overlap at all → agreement_rate is 0.0 (not NaN).
        let entries = vec![external_fb("Z", Verdict::Tp, "pal")];
        let quorum: HashMap<String, Verdict> = HashMap::new();
        let overlap = compute_external_overlap(&entries, &quorum);
        let pal = overlap.per_agent.iter().find(|a| a.agent == "pal").unwrap();
        assert_eq!(pal.overlap, 0);
        assert_eq!(pal.agreement_rate(), 0.0);
    }

    #[test]
    fn linkage_two_feedback_for_same_finding_each_count_separately() {
        // Both entries reference the same finding (e.g. Human + PostFix on
        // the same finding) — each is a linked entry. Per-finding dedup
        // happens later in Task 7, not here.
        let reviews = vec![review_with_finding_ids(&["A"])];
        let feedback = vec![fb_with_finding_id("A"), fb_with_finding_id("A")];
        let stats = linkage_stats(&reviews, &feedback);
        assert_eq!(stats.linked, 2);
    }

    #[test]
    fn linkage_excludes_non_human_provenance() {
        use crate::feedback::Provenance;
        let reviews = vec![review_with_finding_ids(&["A", "B", "C"])];
        let feedback = vec![
            fb_with_id_and_provenance("A", Verdict::Tp, Provenance::Human),
            fb_with_id_and_provenance("B", Verdict::Tp, Provenance::PostFix),
            fb_with_id_and_provenance(
                "C",
                Verdict::Tp,
                Provenance::External {
                    agent: "pal".into(),
                    model: None,
                    confidence: None,
                },
            ),
        ];
        let stats = linkage_stats(&reviews, &feedback);
        assert_eq!(stats.linked, 2, "only Human + PostFix should count");
        assert_eq!(stats.unlinked, 0);
    }
}
