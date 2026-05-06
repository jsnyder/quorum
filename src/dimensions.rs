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
            (Some(a), Some(d)) => { lines_touched += a as u64 + d as u64; has_any_lines = true; }
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

/// Display key for records whose `repo` field is `None`. Bucketing groups
/// `None` separately from any real repo name (`Option<String>` keys), so a
/// repo literally named `(no repo)` never collides with the no-repo bucket.
pub const NO_REPO_KEY: &str = "(no repo)";

pub fn group_by_repo(records: &[ReviewRecord]) -> Vec<DimensionSlice> {
    let mut buckets: HashMap<Option<String>, Vec<&ReviewRecord>> = HashMap::new();
    for r in records {
        buckets.entry(r.repo.clone()).or_default().push(r);
    }
    let mut slices: Vec<_> = buckets
        .into_iter()
        .map(|(k, v)| aggregate(k.unwrap_or_else(|| NO_REPO_KEY.to_string()), &v))
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

/// Context-dimension slice: per-source / per-reviewed-repo / misleading-watch row.
///
/// Shares the `DimensionSlice` shape only partially (these dimensions care about
/// injection-pipeline outcomes, not LLM token spend). Kept as its own type so we
/// don't pollute `DimensionSlice` with `Option<...>` noise that is meaningless
/// for `--by-repo`/`--by-caller`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContextDimensionSlice {
    pub key: String,
    pub n_reviews: u32,
    /// Mean `injected_chunk_count` across the reviews in this slice.
    /// Includes zero-inject reviews so the "is injection actually landing?"
    /// signal is visible — excluding them would hide coverage gaps.
    pub avg_injected_chunk_count: f64,
    /// Mean `injected_tokens` across the reviews in this slice. Zero-inject
    /// reviews are counted, same rationale as chunks.
    pub avg_injected_tokens: f64,
    /// Fraction of reviews where `retriever_errored = true`.
    pub retriever_error_rate: f64,
    /// Fraction of reviews where `adaptive_threshold_applied = true`.
    pub adaptive_threshold_rate: f64,
    /// Sparkline buckets of `injected_chunk_count` over time. Empty when
    /// low-sample (same policy as `DimensionSlice`).
    pub sparkline_points: Vec<f64>,
    pub low_sample: bool,
}

fn aggregate_context_slice(key: String, records: &[&ReviewRecord]) -> ContextDimensionSlice {
    let n_reviews = records.len() as u32;
    let low_sample = n_reviews < MIN_SAMPLE;

    let mut sum_chunks: u64 = 0;
    let mut sum_tokens: u64 = 0;
    let mut errored: u32 = 0;
    let mut adaptive: u32 = 0;
    for r in records {
        sum_chunks += r.context.injected_chunk_count as u64;
        sum_tokens += r.context.injected_tokens as u64;
        if r.context.retriever_errored { errored += 1; }
        if r.context.adaptive_threshold_applied { adaptive += 1; }
    }

    let denom = n_reviews.max(1) as f64;
    let sparkline_points = if low_sample || records.len() < 2 {
        Vec::new()
    } else {
        context_sparkline_buckets(records, 5)
    };

    ContextDimensionSlice {
        key,
        n_reviews,
        avg_injected_chunk_count: sum_chunks as f64 / denom,
        avg_injected_tokens: sum_tokens as f64 / denom,
        retriever_error_rate: errored as f64 / denom,
        adaptive_threshold_rate: adaptive as f64 / denom,
        sparkline_points,
        low_sample,
    }
}

fn context_sparkline_buckets(records: &[&ReviewRecord], n_buckets: usize) -> Vec<f64> {
    if records.is_empty() || n_buckets == 0 {
        return Vec::new();
    }
    let total = records.len();
    let mut out = Vec::with_capacity(n_buckets);
    for b in 0..n_buckets {
        let start = b * total / n_buckets;
        let end = ((b + 1) * total / n_buckets).max(start + 1).min(total);
        if start >= end { out.push(0.0); continue; }
        let mut sum = 0u64;
        for r in &records[start..end] {
            sum += r.context.injected_chunk_count as u64;
        }
        let denom = (end - start) as f64;
        out.push(sum as f64 / denom);
    }
    out
}

/// One row per injected source name (flattens `context.injected_sources`).
///
/// Records without `injector_available` are ignored. A record listing two
/// sources contributes to *both* source buckets (one review counted twice
/// if it drew from two sources). MIN_SAMPLE gate applies — rows with
/// fewer than 5 reviews are flagged `low_sample` like every other
/// dimension, so the caller (table vs. compact) decides how to surface
/// them. No "other" roll-up: unlike `--by-repo`, an undersampled source
/// name is informative on its own ("source X was used but rarely").
pub fn aggregate_by_source(records: &[ReviewRecord]) -> Vec<ContextDimensionSlice> {
    let mut buckets: HashMap<String, Vec<&ReviewRecord>> = HashMap::new();
    for r in records {
        if !r.context.injector_available { continue; }
        // Defensive dedup: the injector already dedups injected_sources,
        // but legacy or externally-written records could contain
        // duplicates. Counting the same review twice in the same source
        // bucket would inflate n_reviews.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for src in &r.context.injected_sources {
            if seen.insert(src.as_str()) {
                buckets.entry(src.clone()).or_default().push(r);
            }
        }
    }
    let mut out: Vec<_> = buckets
        .into_iter()
        .map(|(k, v)| aggregate_context_slice(k, &v))
        .collect();
    out.sort_by(|a, b| b.n_reviews.cmp(&a.n_reviews).then_with(|| a.key.cmp(&b.key)));
    out
}

/// One row per repo, restricted to reviews where an injector was wired.
///
/// Callers who want the un-filtered repo view should keep using
/// `group_by_repo`; this dimension exists specifically to answer "how is
/// context injection behaving per repo?". Sorting matches
/// `group_by_repo` (most reviews first, then alphabetic tiebreak).
pub fn aggregate_by_reviewed_repo(records: &[ReviewRecord]) -> Vec<ContextDimensionSlice> {
    let mut buckets: HashMap<Option<String>, Vec<&ReviewRecord>> = HashMap::new();
    for r in records {
        if !r.context.injector_available { continue; }
        buckets.entry(r.repo.clone()).or_default().push(r);
    }
    let mut out: Vec<_> = buckets
        .into_iter()
        .map(|(k, v)| aggregate_context_slice(k.unwrap_or_else(|| NO_REPO_KEY.to_string()), &v))
        .collect();
    out.sort_by(|a, b| b.n_reviews.cmp(&a.n_reviews).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Breakdown of "misleading" context-injection telemetry: reviews where
/// the dashboards and the underlying pipeline tell different stories.
///
/// Two causes:
/// * `retriever_errored` — the retriever raised. Injection was attempted
///   but failed; downstream findings may be missing context.
/// * "phantom injection" — a rendered block was recorded
///   (`rendered_prompt_hash` is `Some`) but `injected_chunk_count == 0`.
///   Indicates either a telemetry accounting bug or a render of an
///   empty/header-only block.
///
/// Returns rows in this order (stable for JSON consumers):
/// 1. `"total"` — union of misleading reviews (any cause). This is the
///    headline "watch" number.
/// 2. `"retriever_errored"` — reviews with retriever errors.
/// 3. `"phantom_injection"` — reviews with rendered-but-zero.
///
/// A single review can contribute to multiple rows (the breakdown rows
/// are not mutually exclusive), but `total` uses set-union semantics
/// (counted at most once). MIN_SAMPLE gate applies.
pub fn aggregate_misleading(records: &[ReviewRecord]) -> Vec<ContextDimensionSlice> {
    let mut errored: Vec<&ReviewRecord> = Vec::new();
    let mut phantom: Vec<&ReviewRecord> = Vec::new();
    // `total` tracks set-union by run_id to avoid double-counting a review
    // that trips both causes. Ordered insertion so the aggregate's sparkline
    // (if the caller ever runs rolling) stays chronological.
    let mut total_seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut total: Vec<&ReviewRecord> = Vec::new();
    for r in records {
        let is_err = r.context.retriever_errored;
        let is_phantom = r.context.rendered_prompt_hash.is_some()
            && r.context.injected_chunk_count == 0;
        if is_err { errored.push(r); }
        if is_phantom { phantom.push(r); }
        if (is_err || is_phantom) && total_seen.insert(r.run_id.as_str()) {
            total.push(r);
        }
    }
    vec![
        aggregate_context_slice("total".into(), &total),
        aggregate_context_slice("retriever_errored".into(), &errored),
        aggregate_context_slice("phantom_injection".into(), &phantom),
    ]
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
        let offset = w.saturating_mul(n);
        let end = total.saturating_sub(offset);
        if end == 0 { break; }
        let start = end.saturating_sub(n);
        let slice: Vec<&ReviewRecord> = records[start..end].iter().collect();
        let label = match w {
            0 => format!("last {}", n),
            1 => format!("prev {}", n),
            _ => format!("prev {}", offset),
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
            mode: None,
            context: Default::default(),
        finding_ids: Vec::new(),
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
    fn group_by_repo_none_does_not_collide_with_repo_literally_named_no_repo() {
        // Regression: bucketing on the stringified sentinel `(no repo)` would
        // silently merge a real repo coincidentally named the same string with
        // None-repo records. Bucket on Option<String> instead.
        let r_real = rec("(no repo)", "tty", 1, 3);
        let mut r_none = rec("ignored", "tty", 1, 5);
        r_none.repo = None;
        let slices = group_by_repo(&[r_real, r_none]);
        assert_eq!(slices.len(), 2, "None and real '(no repo)' must bucket separately");
        let none_total: u32 = slices.iter().map(|s| s.n_findings).sum();
        assert_eq!(none_total, 8);
    }

    #[test]
    fn aggregate_lines_touched_does_not_overflow_on_large_diffs() {
        // Regression: previously `(a + d) as u64` overflowed u32 before widening,
        // panicking in debug builds and wrapping in release.
        let mut a = rec("r", "tty", 1, 1);
        a.lines_added = Some(u32::MAX);
        a.lines_removed = Some(u32::MAX);
        // The fix uses `a as u64 + d as u64`; this call must not panic.
        let slices = group_by_repo(&[a]);
        // findings_per_kloc = 1 * 1000 / (2 * u32::MAX) -- tiny but well-defined.
        let fpk = slices[0].findings_per_kloc.expect("kloc set when lines>0");
        let expected = 1000.0 / (2.0 * u32::MAX as f64);
        assert!((fpk - expected).abs() < 1e-12, "got {fpk}, expected {expected}");
    }

    #[test]
    fn rolling_window_does_not_panic_on_extreme_window_count() {
        // Regression: `w * n` could overflow usize for adversarial max_windows.
        let records: Vec<_> = (0..3).map(|_| rec("r", "tty", 1, 0)).collect();
        let out = rolling_window(&records, usize::MAX / 2, usize::MAX / 2);
        // Either returns the single first window or breaks early; must not panic.
        assert!(out.len() <= 1);
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

    // ---- Context dimensions (Task 6.3) ----------------------------------

    use crate::review_log::ContextTelemetry;

    #[allow(clippy::too_many_arguments)]
    fn ctx_rec(
        repo: &str,
        injector_available: bool,
        sources: &[&str],
        chunks: u32,
        tokens: u32,
        errored: bool,
        adaptive: bool,
        rendered_hash: Option<&str>,
    ) -> ReviewRecord {
        let mut r = rec(repo, "tty", 1, 0);
        r.context = ContextTelemetry {
            auto_inject_enabled: injector_available,
            injector_available,
            retriever_errored: errored,
            retrieved_chunk_count: chunks,
            injected_chunk_count: chunks,
            injected_tokens: tokens,
            below_threshold_count: 0,
            adaptive_threshold_applied: adaptive,
            effective_prose_threshold: 0.5,
            injected_chunk_ids: vec![],
            injected_sources: sources.iter().map(|s| (*s).to_string()).collect(),
            precedence_entries: 0,
            render_duration_ms: 0,
            rendered_prompt_hash: rendered_hash.map(String::from),
            rerank_score_min: None,
            rerank_score_p10: None,
            rerank_score_median: None,
            rerank_score_p90: None,
            suppressed_by_calibrator: 0,
            suppressed_by_floor: 0,
            nan_scores_dropped: 0,
            retrieved_by_leg: crate::review_log::LegCounts::default(),
            injected_by_leg: crate::review_log::LegCounts::default(),
        };
        r
    }

    #[test]
    fn by_source_flattens_injected_sources_into_per_source_rows() {
        // 4 records: one with 2 sources should contribute to both buckets.
        // Verify unique-source row count and per-source review counts.
        let records = vec![
            ctx_rec("r", true, &["mini-rust"], 2, 100, false, false, Some("h1")),
            ctx_rec("r", true, &["mini-rust", "mini-py"], 4, 200, false, false, Some("h2")),
            ctx_rec("r", true, &["mini-py"], 1, 50, false, false, Some("h3")),
            ctx_rec("r", true, &["mini-rust"], 3, 150, false, false, Some("h4")),
        ];
        let slices = aggregate_by_source(&records);
        assert_eq!(slices.len(), 2, "2 unique source names expected");
        let rust = slices.iter().find(|s| s.key == "mini-rust").unwrap();
        assert_eq!(rust.n_reviews, 3); // records 0, 1, 3
        let py = slices.iter().find(|s| s.key == "mini-py").unwrap();
        assert_eq!(py.n_reviews, 2); // records 1, 2
        // avg_injected_chunk_count for mini-rust = (2+4+3)/3 = 3.0
        assert!((rust.avg_injected_chunk_count - 3.0).abs() < 1e-9);
        // avg tokens mini-py = (200+50)/2 = 125
        assert!((py.avg_injected_tokens - 125.0).abs() < 1e-9);
    }

    #[test]
    fn by_source_defensively_dedups_duplicate_source_entries_in_a_single_record() {
        // Legacy / externally-written record where injected_sources has
        // duplicates must not count the same review twice in one bucket.
        let records = vec![ctx_rec(
            "r",
            true,
            &["mini-rust", "mini-rust", "mini-rust"],
            2,
            100,
            false,
            false,
            Some("h"),
        )];
        let slices = aggregate_by_source(&records);
        assert_eq!(slices.len(), 1);
        assert_eq!(
            slices[0].n_reviews, 1,
            "one review must be counted once even with duplicated source names"
        );
    }

    #[test]
    fn by_source_skips_reviews_without_injector_available() {
        // Sources only count when injector_available=true.
        let records = vec![
            ctx_rec("r", false, &["ghost-source"], 2, 100, false, false, None),
            ctx_rec("r", true, &["real-source"], 1, 50, false, false, Some("h")),
        ];
        let slices = aggregate_by_source(&records);
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].key, "real-source");
    }

    #[test]
    fn by_source_min_sample_gate_flags_undersampled_rows() {
        // 3 records with source X (< MIN_SAMPLE), 6 with source Y (>=).
        // X must be flagged low_sample; Y must not.
        let mut records = Vec::new();
        for _ in 0..3 {
            records.push(ctx_rec("r", true, &["X"], 1, 10, false, false, Some("h")));
        }
        for _ in 0..6 {
            records.push(ctx_rec("r", true, &["Y"], 2, 20, false, false, Some("h")));
        }
        let slices = aggregate_by_source(&records);
        let x = slices.iter().find(|s| s.key == "X").unwrap();
        let y = slices.iter().find(|s| s.key == "Y").unwrap();
        assert!(x.low_sample, "X had 3 reviews, must be low_sample");
        assert!(!y.low_sample, "Y had 6 reviews, must not be low_sample");
    }

    #[test]
    fn by_reviewed_repo_excludes_reviews_without_injector() {
        // Repo "A": 5 reviews, 3 with injector_available=false.
        // Only the 2 injector-wired reviews should count in context-dim output.
        let mut records = Vec::new();
        for _ in 0..3 {
            records.push(ctx_rec("A", false, &[], 0, 0, false, false, None));
        }
        for _ in 0..2 {
            records.push(ctx_rec("A", true, &["src"], 2, 100, false, false, Some("h")));
        }
        let slices = aggregate_by_reviewed_repo(&records);
        assert_eq!(slices.len(), 1, "only repo A with injector-wired reviews");
        assert_eq!(slices[0].key, "A");
        assert_eq!(slices[0].n_reviews, 2);
    }

    #[test]
    fn misleading_counts_retriever_errored_and_phantom_injections() {
        // 10 records total:
        //   2 errored (any other flag mix)
        //   1 phantom (rendered_prompt_hash=Some AND injected_chunk_count=0) and not errored
        //   7 clean
        // Expect misleading total = 3, errored row = 2, phantom row = 1.
        let mut records = Vec::new();
        records.push(ctx_rec("r", true, &["s"], 2, 100, true, false, Some("h1")));
        records.push(ctx_rec("r", true, &["s"], 0, 0, true, false, None));
        // Phantom: rendered block hash present but zero chunks.
        records.push(ctx_rec("r", true, &[], 0, 0, false, false, Some("phantom")));
        for _ in 0..7 {
            records.push(ctx_rec("r", true, &["s"], 3, 150, false, false, Some("h")));
        }
        let slices = aggregate_misleading(&records);
        assert_eq!(slices.len(), 3);
        assert_eq!(slices[0].key, "total");
        assert_eq!(slices[1].key, "retriever_errored");
        assert_eq!(slices[2].key, "phantom_injection");
        assert_eq!(slices[0].n_reviews, 3, "total=union(errored, phantom)");
        assert_eq!(slices[1].n_reviews, 2, "retriever_errored rows");
        assert_eq!(slices[2].n_reviews, 1, "phantom injection rows");
    }

    #[test]
    fn misleading_set_union_deduplicates_reviews_tripping_both_causes() {
        // A single record that is both errored AND phantom must only be
        // counted once in the "total" row (set-union semantics), even
        // though it contributes to both breakdown rows.
        let records = vec![
            ctx_rec("r", true, &[], 0, 0, true, false, Some("dual")),
        ];
        let slices = aggregate_misleading(&records);
        assert_eq!(slices[0].n_reviews, 1, "total must dedupe on run_id");
        assert_eq!(slices[1].n_reviews, 1);
        assert_eq!(slices[2].n_reviews, 1);
    }

    #[test]
    fn context_dim_json_output_has_stable_field_names() {
        // Guardrail for downstream consumers: field names on the wire must
        // stay as-documented in the design. Any rename is a breaking change.
        let slice = ContextDimensionSlice {
            key: "test".into(),
            n_reviews: 7,
            avg_injected_chunk_count: 2.5,
            avg_injected_tokens: 180.0,
            retriever_error_rate: 0.25,
            adaptive_threshold_rate: 0.1,
            sparkline_points: vec![1.0, 2.0, 3.0],
            low_sample: false,
        };
        let json = serde_json::to_string(&slice).unwrap();
        let back: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Required stable field names (used by dashboards, tests, CI jobs):
        for field in [
            "key",
            "n_reviews",
            "avg_injected_chunk_count",
            "avg_injected_tokens",
            "retriever_error_rate",
            "adaptive_threshold_rate",
            "sparkline_points",
            "low_sample",
        ] {
            assert!(
                back.get(field).is_some(),
                "missing stable field {field} in serialized ContextDimensionSlice: {json}",
            );
        }
        // Round-trip sanity
        let back2: ContextDimensionSlice = serde_json::from_str(&json).unwrap();
        assert_eq!(back2, slice);
    }

    #[test]
    fn context_dim_rolling_window_composes_with_by_source() {
        // Intersection sanity: taking the last N records of a by_source
        // aggregation must yield the correct count of source matches
        // from that suffix. This is what the stats CLI does when a user
        // combines `--by-source --rolling 50`.
        let mut records = Vec::new();
        // 10 old records (ignored by rolling 5)
        for _ in 0..10 {
            records.push(ctx_rec("r", true, &["old"], 1, 50, false, false, Some("h")));
        }
        // 5 recent records with source "new"
        for _ in 0..5 {
            records.push(ctx_rec("r", true, &["new"], 2, 100, false, false, Some("h")));
        }
        let recent: Vec<_> = records[records.len() - 5..].to_vec();
        let slices = aggregate_by_source(&recent);
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].key, "new");
        assert_eq!(slices[0].n_reviews, 5);
        assert!(!slices[0].low_sample);
    }

    #[test]
    fn context_dim_avg_includes_zero_inject_reviews() {
        // Decision documented: "avg chunks" does NOT exclude zero-inject
        // reviews. A source that was attempted 4 times and landed chunks
        // only once must show a low mean, not hide behind selection bias.
        let records = vec![
            ctx_rec("r", true, &["s"], 0, 0, false, false, Some("h1")),
            ctx_rec("r", true, &["s"], 0, 0, false, false, Some("h2")),
            ctx_rec("r", true, &["s"], 0, 0, false, false, Some("h3")),
            ctx_rec("r", true, &["s"], 4, 200, false, false, Some("h4")),
        ];
        let slices = aggregate_by_source(&records);
        assert_eq!(slices[0].n_reviews, 4);
        assert!((slices[0].avg_injected_chunk_count - 1.0).abs() < 1e-9);
        assert!((slices[0].avg_injected_tokens - 50.0).abs() < 1e-9);
    }
}
