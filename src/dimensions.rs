//! Dimensional aggregation over `ReviewRecord` streams.
//!
//! Produces `DimensionSlice` rows for stats views: by-repo, by-caller,
//! rolling N-run windows. Respects MIN_SAMPLE gate.

use std::collections::HashMap;

use crate::review_log::{ReviewRecord, SeverityCounts};

pub const MIN_SAMPLE: u32 = 5;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DimensionSlice {
    pub key: String,
    pub n_reviews: u32,
    pub n_findings: u32,
    pub findings_per_file: f64,
    pub findings_per_kloc: Option<f64>,
    pub accept_rate: Option<f64>,
    pub severity_mix: SeverityCounts,
    pub suppression_rate: f64,
    pub avg_duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tokens_cache_read: u64,
    pub cache_hit_rate: f64,
    pub sparkline_points: Vec<f64>,
    pub low_sample: bool,
}

fn aggregate(key: String, records: &[&ReviewRecord]) -> DimensionSlice {
    let n_reviews = records.len() as u32;
    let low_sample = n_reviews < MIN_SAMPLE;

    let mut sev = SeverityCounts::default();
    let mut n_findings: u32 = 0;
    let mut files_reviewed: u64 = 0;
    let mut lines_touched: u64 = 0;
    let mut has_any_lines = false;
    let mut suppressed: u64 = 0;
    let mut duration_total_ms: u128 = 0;
    let mut tokens_in: u64 = 0;
    let mut tokens_out: u64 = 0;
    let mut tokens_cache_read: u64 = 0;
    for r in records {
        sev.critical += r.findings_by_severity.critical;
        sev.high += r.findings_by_severity.high;
        sev.medium += r.findings_by_severity.medium;
        sev.low += r.findings_by_severity.low;
        sev.info += r.findings_by_severity.info;
        n_findings += r.findings_by_severity.total();
        files_reviewed += r.files_reviewed as u64;
        suppressed += r.suppressed_by_rule.values().map(|v| *v as u64).sum::<u64>();
        duration_total_ms += r.duration_ms as u128;
        tokens_in += r.tokens_in;
        tokens_out += r.tokens_out;
        tokens_cache_read += r.tokens_cache_read;
        match (r.lines_added, r.lines_removed) {
            (Some(a), Some(d)) => { lines_touched += (a + d) as u64; has_any_lines = true; }
            (Some(a), None) => { lines_touched += a as u64; has_any_lines = true; }
            (None, Some(d)) => { lines_touched += d as u64; has_any_lines = true; }
            (None, None) => {}
        }
    }

    let findings_per_file = if files_reviewed == 0 {
        0.0
    } else {
        n_findings as f64 / files_reviewed as f64
    };

    let findings_per_kloc = if has_any_lines && lines_touched > 0 {
        Some(n_findings as f64 * 1000.0 / lines_touched as f64)
    } else {
        None
    };

    let suppression_rate = {
        let denom = n_findings as u64 + suppressed;
        if denom == 0 {
            0.0
        } else {
            suppressed as f64 / denom as f64
        }
    };

    let avg_duration_ms = if n_reviews == 0 {
        0
    } else {
        (duration_total_ms / n_reviews as u128) as u64
    };

    let cache_hit_rate = if tokens_in == 0 {
        0.0
    } else {
        tokens_cache_read as f64 / tokens_in as f64
    };

    let sparkline_points = if low_sample || records.len() < 2 {
        Vec::new()
    } else {
        sparkline_buckets(records, 5)
    };

    DimensionSlice {
        key,
        n_reviews,
        n_findings,
        findings_per_file,
        findings_per_kloc,
        accept_rate: None, // feedback join is a later sub-task
        severity_mix: sev,
        suppression_rate,
        avg_duration_ms,
        tokens_in,
        tokens_out,
        tokens_cache_read,
        cache_hit_rate,
        sparkline_points,
        low_sample,
    }
}

fn sparkline_buckets(records: &[&ReviewRecord], n_buckets: usize) -> Vec<f64> {
    if records.is_empty() || n_buckets == 0 {
        return Vec::new();
    }
    let total = records.len();
    let mut out = Vec::with_capacity(n_buckets);
    for b in 0..n_buckets {
        let start = b * total / n_buckets;
        let end = ((b + 1) * total / n_buckets).max(start + 1).min(total);
        if start >= end {
            out.push(0.0);
            continue;
        }
        let mut findings = 0u32;
        let mut files = 0u64;
        for r in &records[start..end] {
            findings += r.findings_by_severity.total();
            files += r.files_reviewed as u64;
        }
        let fpf = if files == 0 { 0.0 } else { findings as f64 / files as f64 };
        out.push(fpf);
    }
    out
}

/// Display key for records whose `repo` field is `None`. Parentheses are
/// disallowed in git repo names on any reasonable system, so this cannot
/// collide with a real repo basename.
pub const NO_REPO_KEY: &str = "(no repo)";

pub fn group_by_repo(records: &[ReviewRecord]) -> Vec<DimensionSlice> {
    let mut buckets: HashMap<String, Vec<&ReviewRecord>> = HashMap::new();
    for r in records {
        let key = r.repo.clone().unwrap_or_else(|| NO_REPO_KEY.to_string());
        buckets.entry(key).or_default().push(r);
    }
    let mut slices: Vec<_> = buckets
        .into_iter()
        .map(|(k, v)| aggregate(k, &v))
        .collect();
    slices.sort_by(|a, b| b.n_reviews.cmp(&a.n_reviews).then_with(|| a.key.cmp(&b.key)));
    slices
}

pub fn group_by_caller(records: &[ReviewRecord]) -> Vec<DimensionSlice> {
    let mut buckets: HashMap<String, Vec<&ReviewRecord>> = HashMap::new();
    for r in records {
        buckets.entry(r.invoked_from.clone()).or_default().push(r);
    }
    let mut slices: Vec<_> = buckets
        .into_iter()
        .map(|(k, v)| aggregate(k, &v))
        .collect();
    slices.sort_by(|a, b| b.n_reviews.cmp(&a.n_reviews).then_with(|| a.key.cmp(&b.key)));
    slices
}

/// Rolling N-record windows over the chronologically-last `n * max_windows` records.
/// Returns: [last N, prev N, prev 2N, ...]. Records assumed in chronological insertion order.
pub fn rolling_window(records: &[ReviewRecord], n: usize, max_windows: usize) -> Vec<DimensionSlice> {
    if n == 0 || max_windows == 0 || records.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let total = records.len();
    for w in 0..max_windows {
        let end = total.saturating_sub(w * n);
        if end == 0 { break; }
        let start = end.saturating_sub(n);
        let slice: Vec<&ReviewRecord> = records[start..end].iter().collect();
        let label = match w {
            0 => format!("last {}", n),
            1 => format!("prev {}", n),
            k => format!("prev {}", k * n),
        };
        out.push(aggregate(label, &slice));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_log::{Flags, ReviewRecord, SeverityCounts};
    use chrono::Utc;

    fn rec(repo: &str, caller: &str, files: u32, findings: u32) -> ReviewRecord {
        ReviewRecord {
            run_id: ReviewRecord::new_ulid(),
            timestamp: Utc::now(),
            quorum_version: "test".into(),
            repo: Some(repo.into()),
            invoked_from: caller.into(),
            model: "gpt-5.4".into(),
            files_reviewed: files,
            lines_added: None,
            lines_removed: None,
            findings_by_severity: SeverityCounts { critical: 0, high: findings, medium: 0, low: 0, info: 0 },
            suppressed_by_rule: Default::default(),
            tokens_in: 1000,
            tokens_out: 100,
            tokens_cache_read: 0,
            duration_ms: 500,
            flags: Flags::default(),
        }
    }

    #[test]
    fn group_by_repo_empty_input_yields_empty_output() {
        let slices = group_by_repo(&[]);
        assert!(slices.is_empty());
    }

    #[test]
    fn group_by_repo_buckets_by_repo_field() {
        let records = vec![
            rec("alpha", "tty", 1, 2),
            rec("alpha", "tty", 2, 1),
            rec("beta", "tty", 1, 5),
        ];
        let slices = group_by_repo(&records);
        assert_eq!(slices.len(), 2);
        let alpha = slices.iter().find(|s| s.key == "alpha").unwrap();
        assert_eq!(alpha.n_reviews, 2);
        assert_eq!(alpha.n_findings, 3);
        assert_eq!(alpha.findings_per_file, 1.0); // 3 findings / 3 files
        let beta = slices.iter().find(|s| s.key == "beta").unwrap();
        assert_eq!(beta.n_reviews, 1);
        assert_eq!(beta.n_findings, 5);
    }

    #[test]
    fn group_by_repo_none_does_not_collide_with_real_repo_named_unknown() {
        // Real repo literally named "unknown" must stay distinct from records
        // with `repo: None`. Sentinel key must be unambiguous.
        let r_real = rec("unknown", "tty", 1, 3);
        let mut r_none = rec("ignored", "tty", 1, 5);
        r_none.repo = None;
        let slices = group_by_repo(&[r_real, r_none]);
        let none_slice = slices.iter().find(|s| s.key == "(no repo)")
            .expect("None repo should produce a '(no repo)' sentinel, got keys: {:?}");
        let real_slice = slices.iter().find(|s| s.key == "unknown")
            .expect("real 'unknown' repo must remain addressable by its name");
        assert_eq!(none_slice.n_findings, 5);
        assert_eq!(real_slice.n_findings, 3);
    }

    #[test]
    fn slices_sorted_by_review_count_descending() {
        let records = vec![
            rec("small", "tty", 1, 0),
            rec("big", "tty", 1, 0),
            rec("big", "tty", 1, 0),
            rec("big", "tty", 1, 0),
        ];
        let slices = group_by_repo(&records);
        assert_eq!(slices[0].key, "big");
        assert_eq!(slices[1].key, "small");
    }

    #[test]
    fn low_sample_flagged_below_min_sample() {
        let records: Vec<_> = (0..MIN_SAMPLE - 1).map(|_| rec("x", "tty", 1, 0)).collect();
        let slices = group_by_repo(&records);
        assert!(slices[0].low_sample, "n < MIN_SAMPLE should set low_sample=true");
    }

    #[test]
    fn at_min_sample_not_flagged() {
        let records: Vec<_> = (0..MIN_SAMPLE).map(|_| rec("x", "tty", 1, 0)).collect();
        let slices = group_by_repo(&records);
        assert!(!slices[0].low_sample);
    }

    #[test]
    fn group_by_caller_buckets_by_invoked_from() {
        let records = vec![
            rec("r", "claude_code", 1, 1),
            rec("r", "claude_code", 1, 1),
            rec("r", "tty", 1, 1),
        ];
        let slices = group_by_caller(&records);
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0].key, "claude_code");
        assert_eq!(slices[0].n_reviews, 2);
    }

    #[test]
    fn rolling_window_empty_when_no_records() {
        let out = rolling_window(&[], 5, 3);
        assert!(out.is_empty());
    }

    #[test]
    fn rolling_window_returns_labeled_slices() {
        let records: Vec<_> = (0..12).map(|_| rec("r", "tty", 1, 1)).collect();
        let out = rolling_window(&records, 5, 3);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].key, "last 5");
        assert_eq!(out[1].key, "prev 5");
        assert_eq!(out[2].key, "prev 10");
    }

    #[test]
    fn rolling_window_last_slice_covers_most_recent_records() {
        let mut records: Vec<_> = (0..10).map(|_| rec("r", "tty", 1, 0)).collect();
        records.last_mut().unwrap().findings_by_severity.high = 99;
        let out = rolling_window(&records, 5, 2);
        assert!(out[0].n_findings >= 99);
        assert_eq!(out[1].n_findings, 0);
    }

    #[test]
    fn rolling_window_stops_at_available_records() {
        let records: Vec<_> = (0..3).map(|_| rec("r", "tty", 1, 0)).collect();
        let out = rolling_window(&records, 5, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].n_reviews, 3);
    }

    #[test]
    fn sparkline_empty_for_single_record() {
        let records = vec![rec("r", "tty", 1, 1)];
        let slices = group_by_repo(&records);
        assert!(slices[0].sparkline_points.is_empty());
    }

    #[test]
    fn sparkline_populated_for_multi_record_non_low_sample() {
        let records: Vec<_> = (0..MIN_SAMPLE).map(|_| rec("r", "tty", 1, 2)).collect();
        let slices = group_by_repo(&records);
        assert_eq!(slices[0].sparkline_points.len(), 5);
    }

    #[test]
    fn sparkline_empty_when_low_sample() {
        let records = vec![rec("r", "tty", 1, 2), rec("r", "tty", 1, 2)];
        let slices = group_by_repo(&records);
        assert!(slices[0].low_sample);
        assert!(slices[0].sparkline_points.is_empty());
    }

    #[test]
    fn findings_per_kloc_computed_when_lines_present() {
        let mut r = rec("r", "tty", 1, 5);
        r.lines_added = Some(100);
        r.lines_removed = Some(0);
        let slices = group_by_repo(&[r]);
        assert_eq!(slices[0].findings_per_kloc, Some(50.0));
    }

    #[test]
    fn findings_per_kloc_none_when_no_diff_data() {
        let records = vec![rec("r", "tty", 1, 5)];
        let slices = group_by_repo(&records);
        assert!(slices[0].findings_per_kloc.is_none());
    }

    #[test]
    fn suppression_rate_handles_large_counts_without_truncation() {
        // With u32 truncation, sum = 2^32 truncates to 0, then the "zero" branch
        // returns 0.0 instead of the correct 1.0. Reproduces the bug exactly.
        let mut r = rec("r", "tty", 0, 0);
        r.suppressed_by_rule.insert("a".into(), u32::MAX); // 2^32 - 1
        r.suppressed_by_rule.insert("b".into(), 1);        // +1 = 2^32 (truncates to 0 as u32)
        let slices = group_by_repo(&[r]);
        assert!(
            (slices[0].suppression_rate - 1.0).abs() < 1e-9,
            "expected 1.0, got {}",
            slices[0].suppression_rate,
        );
    }

    #[test]
    fn cache_hit_rate_from_tokens() {
        let mut r = rec("r", "tty", 1, 0);
        r.tokens_in = 1000;
        r.tokens_cache_read = 250;
        let slices = group_by_repo(&[r]);
        assert!((slices[0].cache_hit_rate - 0.25).abs() < 1e-9);
    }
}
