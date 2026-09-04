//! Dimensional aggregation over `ReviewRecord` streams.
//!
//! Produces `DimensionSlice` rows for stats views: by-repo, by-caller,
//! rolling N-run windows. Respects MIN_SAMPLE gate.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

use crate::feedback::{FeedbackEntry, Verdict};
use crate::review_log::{ReviewRecord, SeverityCounts};
use crate::skill_audit::{
    ExitStatus, IntegratorDecision, IntegratorDecisionRecord, SkillInvocationRecord,
};

pub const MIN_SAMPLE: u32 = 5;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DimensionSlice {
    pub key: String,
    pub n_reviews: u32,
    pub n_findings: u32,
    /// Files reviewed across the slice. The denominator behind
    /// `findings_per_file`, exposed so callers can weigh a rate by its sample
    /// instead of recovering it by division.
    #[serde(default)]
    pub files_reviewed: u64,
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
        suppressed += r
            .suppressed_by_rule
            .values()
            .map(|v| *v as u64)
            .sum::<u64>();
        duration_total_ms += r.duration_ms as u128;
        tokens_in += r.tokens_in;
        tokens_out += r.tokens_out;
        tokens_cache_read += r.tokens_cache_read;
        match (r.lines_added, r.lines_removed) {
            (Some(a), Some(d)) => {
                lines_touched += a as u64 + d as u64;
                has_any_lines = true;
            }
            (Some(a), None) => {
                lines_touched += a as u64;
                has_any_lines = true;
            }
            (None, Some(d)) => {
                lines_touched += d as u64;
                has_any_lines = true;
            }
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
        files_reviewed,
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
        let fpf = if files == 0 {
            0.0
        } else {
            findings as f64 / files as f64
        };
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
    slices.sort_by(|a, b| {
        b.n_reviews
            .cmp(&a.n_reviews)
            .then_with(|| a.key.cmp(&b.key))
    });
    slices
}

pub fn group_by_caller(records: &[ReviewRecord]) -> Vec<DimensionSlice> {
    let mut buckets: HashMap<String, Vec<&ReviewRecord>> = HashMap::new();
    for r in records {
        buckets.entry(r.invoked_from.clone()).or_default().push(r);
    }
    let mut slices: Vec<_> = buckets.into_iter().map(|(k, v)| aggregate(k, &v)).collect();
    slices.sort_by(|a, b| {
        b.n_reviews
            .cmp(&a.n_reviews)
            .then_with(|| a.key.cmp(&b.key))
    });
    slices
}

/// Group reviews by the quorum version that produced them, oldest first.
///
/// Unlike the other dimensions this is ordered chronologically (by each
/// version's earliest review) rather than by volume, because the point is to
/// read it as a time series and spot a step change at a release boundary.
#[must_use]
pub fn group_by_version(records: &[ReviewRecord]) -> Vec<DimensionSlice> {
    let mut buckets: HashMap<String, Vec<&ReviewRecord>> = HashMap::new();
    let mut first_seen: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    for r in records {
        buckets.entry(r.quorum_version.clone()).or_default().push(r);
        first_seen
            .entry(r.quorum_version.clone())
            .and_modify(|t| {
                if r.timestamp < *t {
                    *t = r.timestamp;
                }
            })
            .or_insert(r.timestamp);
    }
    let mut slices: Vec<_> = buckets.into_iter().map(|(k, v)| aggregate(k, &v)).collect();
    slices.sort_by(|a, b| {
        first_seen
            .get(&a.key)
            .cmp(&first_seen.get(&b.key))
            .then_with(|| a.key.cmp(&b.key))
    });
    slices
}

/// Fire when the newest version's high-severity rate falls below this fraction
/// of the baseline median. See `detect_severity_regression` for the tuning data.
pub const DEFAULT_REGRESSION_RATIO: f64 = 0.2;

/// Minimum files reviewed before a version is judged, or used as baseline.
pub const DEFAULT_REGRESSION_MIN_FILES: u64 = 20;

/// A detected collapse in high-severity yield at a version boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct SeverityRegression {
    /// Version whose rate collapsed.
    pub version: String,
    /// crit+high per file reviewed, for that version.
    pub rate: f64,
    /// Median of the same rate across all preceding versions.
    pub baseline: f64,
    /// Files reviewed on the suspect version -- the sample behind `rate`.
    pub files: u64,
}

/// crit+high findings per file reviewed. The metric that collapsed 1.17 -> 0.014
/// across the 0.28.0 boundary while every other signal read "success".
fn high_severity_rate(s: &DimensionSlice) -> Option<f64> {
    let files = s.files_reviewed;
    if files == 0 {
        return None;
    }
    let ch = f64::from(s.severity_mix.critical + s.severity_mix.high);
    Some(ch / files as f64)
}

/// Flag the newest version when its high-severity yield falls below
/// `threshold_ratio` of the median of preceding versions with a comparable
/// sample.
///
/// **Tuned against real history, not guessed.** Replayed over 32 versions of
/// this project's own `reviews.jsonl`, which contains the 0.29.0 outage:
///
/// | ratio | versions flagged | caught 0.29.0 |
/// |-------|------------------|---------------|
/// | 0.50  | 8 of 32          | yes           |
/// | 0.35  | 6 of 32          | yes           |
/// | 0.25  | 2 of 32          | yes           |
/// | 0.20  | 1 of 32          | yes           |
///
/// 0.5 was the intuitive threshold and it fires on a quarter of all releases --
/// an alarm that cries wolf that often is one people learn to ignore, which is
/// the same failure it exists to prevent. `DEFAULT_REGRESSION_RATIO` is 0.2:
/// one alarm across the project's entire history, on the version that was
/// actually broken.
///
/// This exists because quorum recorded exactly this collapse for two months and
/// nothing read it back: 1.17 (April) -> 1.09 (May) -> 0.014 (July), across the
/// release that made the axis reviewer default. Aggregate rate is the only
/// signal that catches *systemic* silence, and unlike a false-negative channel
/// it needs no new data -- `reviews.jsonl` already carries `quorum_version`.
///
/// Returns `None` when there is no prior baseline, when the newest version has
/// too small a sample to judge, or when the rate is healthy.
#[must_use]
pub fn detect_severity_regression(
    slices: &[DimensionSlice],
    threshold_ratio: f64,
    min_files: u64,
) -> Option<SeverityRegression> {
    let (newest, prior) = slices.split_last()?;
    if prior.is_empty() {
        return None;
    }
    if newest.files_reviewed < min_files {
        return None;
    }
    let rate = high_severity_rate(newest)?;

    // Judge the baseline by the same sample bar as the candidate. Per-version
    // rates swing wildly with how much was reviewed (this project's history
    // ranges 3 to 1154 files per version), so a median polluted by 3-file
    // versions is not a baseline.
    let mut baseline: Vec<f64> = prior
        .iter()
        .filter(|s| s.files_reviewed >= min_files)
        .filter_map(high_severity_rate)
        .collect();
    if baseline.is_empty() {
        return None;
    }
    baseline.sort_by(f64::total_cmp);
    let mid = baseline.len() / 2;
    let median = if baseline.len().is_multiple_of(2) {
        (baseline[mid - 1] + baseline[mid]) / 2.0
    } else {
        baseline[mid]
    };

    // A baseline that never produced high-severity findings cannot regress.
    if median <= 0.0 {
        return None;
    }
    if rate >= median * threshold_ratio {
        return None;
    }
    Some(SeverityRegression {
        version: newest.key.clone(),
        rate,
        baseline: median,
        files: newest.files_reviewed,
    })
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
        if r.context.retriever_errored {
            errored += 1;
        }
        if r.context.adaptive_threshold_applied {
            adaptive += 1;
        }
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
        if start >= end {
            out.push(0.0);
            continue;
        }
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
        if !r.context.injector_available {
            continue;
        }
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
    out.sort_by(|a, b| {
        b.n_reviews
            .cmp(&a.n_reviews)
            .then_with(|| a.key.cmp(&b.key))
    });
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
        if !r.context.injector_available {
            continue;
        }
        buckets.entry(r.repo.clone()).or_default().push(r);
    }
    let mut out: Vec<_> = buckets
        .into_iter()
        .map(|(k, v)| aggregate_context_slice(k.unwrap_or_else(|| NO_REPO_KEY.to_string()), &v))
        .collect();
    out.sort_by(|a, b| {
        b.n_reviews
            .cmp(&a.n_reviews)
            .then_with(|| a.key.cmp(&b.key))
    });
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
        let is_phantom =
            r.context.rendered_prompt_hash.is_some() && r.context.injected_chunk_count == 0;
        if is_err {
            errored.push(r);
        }
        if is_phantom {
            phantom.push(r);
        }
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
pub fn rolling_window(
    records: &[ReviewRecord],
    n: usize,
    max_windows: usize,
) -> Vec<DimensionSlice> {
    if n == 0 || max_windows == 0 || records.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let total = records.len();
    for w in 0..max_windows {
        let offset = w.saturating_mul(n);
        let end = total.saturating_sub(offset);
        if end == 0 {
            break;
        }
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

/// Per-file hotspot row aggregated from feedback verdicts.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileHotspotRow {
    pub file_path: String,
    pub tp_count: u32,
    pub fp_count: u32,
    pub wontfix_count: u32,
    pub partial_count: u32,
    pub total: u32,
    pub last_seen: DateTime<Utc>,
}

pub fn group_by_file(entries: &[FeedbackEntry], top_n: Option<usize>) -> Vec<FileHotspotRow> {
    struct Accum {
        tp: u32,
        fp: u32,
        wontfix: u32,
        partial: u32,
        last_seen: DateTime<Utc>,
    }

    let mut buckets: HashMap<&str, Accum> = HashMap::new();
    for e in entries {
        if e.file_path.is_empty() {
            continue;
        }
        let acc = buckets.entry(&e.file_path).or_insert(Accum {
            tp: 0,
            fp: 0,
            wontfix: 0,
            partial: 0,
            last_seen: e.timestamp,
        });
        match &e.verdict {
            Verdict::Tp => acc.tp += 1,
            Verdict::Fp => acc.fp += 1,
            Verdict::Wontfix => acc.wontfix += 1,
            Verdict::Partial => acc.partial += 1,
            Verdict::ContextMisleading { .. } => {}
        }
        if e.timestamp > acc.last_seen {
            acc.last_seen = e.timestamp;
        }
    }

    let mut rows: Vec<FileHotspotRow> = buckets
        .into_iter()
        .map(|(path, acc)| FileHotspotRow {
            file_path: path.to_string(),
            tp_count: acc.tp,
            fp_count: acc.fp,
            wontfix_count: acc.wontfix,
            partial_count: acc.partial,
            total: acc.tp + acc.fp + acc.wontfix + acc.partial,
            last_seen: acc.last_seen,
        })
        .filter(|r| r.total > 0)
        .collect();

    rows.sort_by(|a, b| {
        b.tp_count
            .cmp(&a.tp_count)
            .then_with(|| b.total.cmp(&a.total))
            .then_with(|| a.file_path.cmp(&b.file_path))
    });

    if let Some(n) = top_n {
        rows.truncate(n);
    }

    rows
}

/// Per-rule precision row aggregated from feedback verdicts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuleDimensionSlice {
    pub key: String,
    pub tp: u32,
    pub fp: u32,
    pub partial: u32,
    pub wontfix: u32,
    pub total: u32,
    pub precision: f64,
    pub low_sample: bool,
}

pub fn group_by_rule(
    entries: &[FeedbackEntry],
    glob_filter: Option<&str>,
) -> Vec<RuleDimensionSlice> {
    let mut buckets: HashMap<String, (u32, u32, u32, u32)> = HashMap::new();

    for entry in entries {
        let rule_id = match &entry.rule_id {
            Some(r) if !r.is_empty() => r,
            _ => continue,
        };

        if let Some(pattern) = glob_filter
            && !glob_match(pattern, rule_id)
        {
            continue;
        }

        let counts = buckets.entry(rule_id.clone()).or_default();
        match &entry.verdict {
            Verdict::Tp => counts.0 += 1,
            Verdict::Fp => counts.1 += 1,
            Verdict::Partial => counts.2 += 1,
            Verdict::Wontfix => counts.3 += 1,
            Verdict::ContextMisleading { .. } => {}
        }
    }

    let mut slices: Vec<RuleDimensionSlice> = buckets
        .into_iter()
        .map(|(key, (tp, fp, partial, wontfix))| {
            let total = tp + fp + partial + wontfix;
            let precision = if tp + fp > 0 {
                tp as f64 / (tp + fp) as f64
            } else {
                0.0
            };
            RuleDimensionSlice {
                key,
                tp,
                fp,
                partial,
                wontfix,
                total,
                precision,
                low_sample: total < MIN_SAMPLE,
            }
        })
        .collect();

    slices.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.key.cmp(&b.key)));
    slices
}

fn glob_match(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        value == pattern
    }
}

// ---------------------------------------------------------------------------
// #491: skill invocation audit rollup
// ---------------------------------------------------------------------------

/// One row per skill (axis) aggregated from `skill_invocations.jsonl`.
///
/// The reader for that log existed and was tested for months with zero
/// production callers, which is how the axis reviewer emitted zero findings
/// across 440 invocations without anyone noticing (#491). `zero_streak` is the
/// column that would have made it obvious.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SkillAuditRow {
    pub skill: String,
    pub runs: u32,
    pub findings_emitted: u64,
    pub zero_finding_runs: u32,
    /// Consecutive most-recent runs that emitted no findings. A healthy skill
    /// resets this constantly; a broken one climbs forever.
    pub zero_streak: u32,
    pub findings_clamped: u64,
    pub findings_dropped_invalid_json: u64,
    /// Runs whose `exit_status` was `Error`.
    pub errors: u32,
    /// Runs that fell back off the requested model.
    pub model_fallbacks: u32,
    pub avg_duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// `parse_error_class` histogram. `wrong_schema` here is the exact signal
    /// that was logged 213 times and never read.
    pub parse_error_classes: BTreeMap<String, u32>,
    pub failure_reasons: BTreeMap<String, u32>,
    pub low_sample: bool,
}

impl SkillAuditRow {
    /// Share of this skill's runs that emitted nothing. Lifetime rate --
    /// `zero_streak` is the one that says whether it is broken *now*.
    pub fn zero_finding_rate(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            f64::from(self.zero_finding_runs) / f64::from(self.runs)
        }
    }
}

/// Aggregate skill invocation records into one row per skill.
///
/// Rows sort by `zero_streak` descending, then by run count -- a blackout
/// belongs at the top of the table, not buried alphabetically.
pub fn group_by_skill(records: &[SkillInvocationRecord]) -> Vec<SkillAuditRow> {
    let mut buckets: HashMap<&str, Vec<&SkillInvocationRecord>> = HashMap::new();
    for r in records {
        buckets.entry(r.skill_name.as_str()).or_default().push(r);
    }

    let mut rows: Vec<SkillAuditRow> = buckets
        .into_iter()
        .map(|(skill, mut runs)| {
            // The log is append-ordered, but sort defensively: `zero_streak`
            // is only meaningful on a chronological sequence.
            runs.sort_by_key(|r| r.ts);

            let mut row = SkillAuditRow {
                skill: skill.to_string(),
                runs: runs.len() as u32,
                findings_emitted: 0,
                zero_finding_runs: 0,
                zero_streak: 0,
                findings_clamped: 0,
                findings_dropped_invalid_json: 0,
                errors: 0,
                model_fallbacks: 0,
                avg_duration_ms: 0,
                tokens_in: 0,
                tokens_out: 0,
                parse_error_classes: BTreeMap::new(),
                failure_reasons: BTreeMap::new(),
                low_sample: (runs.len() as u32) < MIN_SAMPLE,
            };

            let mut duration_total: u64 = 0;
            for r in &runs {
                row.findings_emitted += u64::from(r.findings_emitted);
                if r.findings_emitted == 0 {
                    row.zero_finding_runs += 1;
                }
                row.findings_clamped += u64::from(r.findings_clamped);
                row.findings_dropped_invalid_json += u64::from(r.findings_dropped_invalid_json);
                if matches!(r.exit_status, ExitStatus::Error) {
                    row.errors += 1;
                }
                if r.model_was_fallback {
                    row.model_fallbacks += 1;
                }
                duration_total = duration_total.saturating_add(r.duration_ms);
                row.tokens_in = row.tokens_in.saturating_add(r.tokens_in);
                row.tokens_out = row.tokens_out.saturating_add(r.tokens_out);
                if let Some(class) = &r.parse_error_class {
                    *row.parse_error_classes
                        .entry(class.to_string())
                        .or_insert(0) += 1;
                }
                if let Some(reason) = &r.failure_reason {
                    *row.failure_reasons
                        .entry(format!("{reason:?}"))
                        .or_insert(0) += 1;
                }
            }

            row.zero_streak = runs
                .iter()
                .rev()
                .take_while(|r| r.findings_emitted == 0)
                .count() as u32;

            if !runs.is_empty() {
                row.avg_duration_ms = duration_total / runs.len() as u64;
            }
            row
        })
        .collect();

    rows.sort_by(|a, b| {
        b.zero_streak
            .cmp(&a.zero_streak)
            .then(b.runs.cmp(&a.runs))
            .then(a.skill.cmp(&b.skill))
    });
    rows
}

// ---------------------------------------------------------------------------
// #491: integrator decision audit rollup
// ---------------------------------------------------------------------------

/// One row per integrator decision kind from `integrator_decisions.jsonl`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IntegratorAuditRow {
    /// "merged" | "suppressed" | "pass_through".
    pub decision: String,
    pub count: u32,
    pub share: f64,
    pub avg_output_confidence: f64,
    /// Decisions where the integrator changed a finding's severity. The
    /// v0.28.0 severity collapse lived here for two months (#491).
    pub severity_changed: u32,
    pub reasons: BTreeMap<String, u32>,
    pub low_sample: bool,
}

/// Aggregate integrator decisions by kind, plus a severity-transition
/// histogram keyed `"high->medium"`.
pub fn group_by_integrator_decision(
    records: &[IntegratorDecisionRecord],
) -> (Vec<IntegratorAuditRow>, BTreeMap<String, u32>) {
    let mut buckets: BTreeMap<String, Vec<&IntegratorDecisionRecord>> = BTreeMap::new();
    let mut transitions: BTreeMap<String, u32> = BTreeMap::new();

    for r in records {
        let key = match r.decision {
            IntegratorDecision::Merged => "merged",
            IntegratorDecision::Suppressed => "suppressed",
            IntegratorDecision::PassThrough => "pass_through",
        };
        buckets.entry(key.to_string()).or_default().push(r);
        if r.severity_pre_clamp != r.severity_post_clamp {
            *transitions
                .entry(format!(
                    "{}->{}",
                    r.severity_pre_clamp, r.severity_post_clamp
                ))
                .or_insert(0) += 1;
        }
    }

    let total = records.len().max(1) as f64;
    let mut rows: Vec<IntegratorAuditRow> = buckets
        .into_iter()
        .map(|(decision, group)| {
            let count = group.len() as u32;
            let mut reasons: BTreeMap<String, u32> = BTreeMap::new();
            let mut confidence_total = 0.0;
            let mut severity_changed = 0;
            for r in &group {
                *reasons.entry(r.reason.clone()).or_insert(0) += 1;
                if r.output_confidence.is_finite() {
                    confidence_total += r.output_confidence;
                }
                if r.severity_pre_clamp != r.severity_post_clamp {
                    severity_changed += 1;
                }
            }
            IntegratorAuditRow {
                decision,
                count,
                share: f64::from(count) / total,
                avg_output_confidence: confidence_total / f64::from(count),
                severity_changed,
                reasons,
                low_sample: count < MIN_SAMPLE,
            }
        })
        .collect();

    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.decision.cmp(&b.decision)));
    (rows, transitions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FeedbackEntry, Provenance, Verdict};
    use crate::review_log::{Flags, ReviewRecord, SeverityCounts};
    use chrono::{TimeZone, Utc};

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
            findings_by_severity: SeverityCounts {
                critical: 0,
                high: findings,
                medium: 0,
                low: 0,
                info: 0,
            },
            suppressed_by_rule: Default::default(),
            tokens_in: 1000,
            tokens_out: 100,
            tokens_cache_read: 0,
            duration_ms: 500,
            flags: Flags::default(),
            mode: None,
            context: Default::default(),
            finding_ids: Vec::new(),
            skills_used: Vec::new(),
            skill_findings: None,
            integrator_findings_out: None,
        }
    }

    /// Build a record on a given version, at a given time, with `high` findings
    /// over `files` files.
    fn ver_rec(version: &str, days_ago: i64, files: u32, high: u32) -> ReviewRecord {
        let mut r = rec("repo", "cli", files, high);
        r.quorum_version = version.into();
        r.timestamp = Utc::now() - chrono::Duration::days(days_ago);
        r
    }

    #[test]
    fn group_by_version_orders_chronologically_not_by_volume() {
        // The newest version has the FEWEST reviews; volume ordering would put
        // it first and break reading the slices as a time series.
        let records = vec![
            ver_rec("0.27.0", 90, 10, 10),
            ver_rec("0.27.0", 89, 10, 10),
            ver_rec("0.27.0", 88, 10, 10),
            ver_rec("0.28.0", 30, 10, 0),
        ];
        let slices = group_by_version(&records);
        let keys: Vec<&str> = slices.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["0.27.0", "0.28.0"], "oldest version first");
        assert_eq!(slices[0].files_reviewed, 30);
        assert_eq!(slices[1].files_reviewed, 10);
    }

    /// Replays the real regression: crit+high per file ran ~1.1 for two
    /// versions, then collapsed to ~0.01 when the axis reviewer silently
    /// stopped emitting findings. quorum recorded this for two months and
    /// nothing read it back.
    #[test]
    fn detects_the_0_28_0_style_collapse() {
        let mut records = vec![];
        for d in 0..5 {
            records.push(ver_rec("0.26.0", 120 - d, 10, 11)); // 1.1/file
            records.push(ver_rec("0.27.0", 90 - d, 10, 11)); // 1.1/file
        }
        // Newest version: 100 files, a single high finding.
        for d in 0..10 {
            records.push(ver_rec("0.28.0", 30 - d, 10, u32::from(d == 0)));
        }
        let slices = group_by_version(&records);
        let hit = detect_severity_regression(
            &slices,
            DEFAULT_REGRESSION_RATIO,
            DEFAULT_REGRESSION_MIN_FILES,
        )
        .expect("a 100x collapse must be flagged");
        assert_eq!(hit.version, "0.28.0");
        assert!(hit.rate < 0.02, "rate was {}", hit.rate);
        assert!(hit.baseline > 1.0, "baseline was {}", hit.baseline);
        assert_eq!(hit.files, 100);
    }

    #[test]
    fn healthy_newest_version_is_not_flagged() {
        let mut records = vec![];
        for d in 0..5 {
            records.push(ver_rec("0.26.0", 120 - d, 10, 11));
            records.push(ver_rec("0.27.0", 90 - d, 10, 10));
        }
        for d in 0..5 {
            records.push(ver_rec("0.28.0", 30 - d, 10, 9)); // 0.9 vs ~1.05 median
        }
        let slices = group_by_version(&records);
        assert!(
            detect_severity_regression(
                &slices,
                DEFAULT_REGRESSION_RATIO,
                DEFAULT_REGRESSION_MIN_FILES
            )
            .is_none(),
            "a mild dip within the threshold must not fire"
        );
    }

    /// Guard against crying wolf on a handful of reviews: a new release
    /// typically has a tiny sample on day one.
    #[test]
    fn small_sample_on_newest_version_is_not_flagged() {
        let mut records = vec![];
        for d in 0..5 {
            records.push(ver_rec("0.27.0", 90 - d, 10, 11));
        }
        records.push(ver_rec("0.28.0", 1, 2, 0)); // only 2 files
        let slices = group_by_version(&records);
        assert!(
            detect_severity_regression(
                &slices,
                DEFAULT_REGRESSION_RATIO,
                DEFAULT_REGRESSION_MIN_FILES
            )
            .is_none(),
            "must not fire on a 2-file sample"
        );
    }

    /// The tuning that matters: 0.5 was the intuitive threshold and it fires on
    /// a quarter of this project's releases. A moderate dip -- the common case
    /// when a release happens to review cleaner code -- must NOT alarm, or the
    /// alarm gets ignored and recreates the failure it exists to catch.
    #[test]
    fn moderate_dip_does_not_alarm_at_the_tuned_ratio() {
        let mut records = vec![];
        for d in 0..5 {
            records.push(ver_rec("0.26.0", 120 - d, 20, 20)); // 1.0/file
            records.push(ver_rec("0.27.0", 90 - d, 20, 20)); // 1.0/file
        }
        for d in 0..5 {
            records.push(ver_rec("0.28.0", 30 - d, 20, 8)); // 0.4/file: 40% of baseline
        }
        let slices = group_by_version(&records);
        assert!(
            detect_severity_regression(&slices, 0.5, DEFAULT_REGRESSION_MIN_FILES).is_some(),
            "the rejected 0.5 threshold would have fired here"
        );
        assert!(
            detect_severity_regression(
                &slices,
                DEFAULT_REGRESSION_RATIO,
                DEFAULT_REGRESSION_MIN_FILES
            )
            .is_none(),
            "the tuned 0.2 threshold must stay quiet on a 40%-of-baseline dip"
        );
    }

    /// Small-sample versions must not pollute the baseline median. This
    /// project's per-version samples range 3 to 1154 files.
    #[test]
    fn tiny_versions_are_excluded_from_the_baseline() {
        let mut records = vec![];
        for d in 0..5 {
            records.push(ver_rec("0.26.0", 120 - d, 20, 20)); // 1.0/file, counts
        }
        // A 2-file version with a freak 0.0 rate would drag the median down.
        records.push(ver_rec("0.27.0", 60, 2, 0));
        for d in 0..5 {
            records.push(ver_rec("0.28.0", 30 - d, 20, 1)); // 0.05/file
        }
        let slices = group_by_version(&records);
        let hit = detect_severity_regression(
            &slices,
            DEFAULT_REGRESSION_RATIO,
            DEFAULT_REGRESSION_MIN_FILES,
        )
        .expect("baseline must ignore the 2-file version and still flag the collapse");
        assert_eq!(hit.version, "0.28.0");
        assert!(
            (hit.baseline - 1.0).abs() < 1e-9,
            "baseline should be 1.0 from the 20-file version, got {}",
            hit.baseline
        );
    }

    #[test]
    fn no_prior_baseline_cannot_regress() {
        let records = vec![ver_rec("0.28.0", 1, 50, 0)];
        let slices = group_by_version(&records);
        assert!(
            detect_severity_regression(
                &slices,
                DEFAULT_REGRESSION_RATIO,
                DEFAULT_REGRESSION_MIN_FILES
            )
            .is_none()
        );
    }

    /// A project that has never produced crit+high findings has no baseline to
    /// fall from; flagging it would fire forever.
    #[test]
    fn zero_baseline_never_fires() {
        let mut records = vec![];
        for d in 0..5 {
            records.push(ver_rec("0.27.0", 90 - d, 10, 0));
        }
        for d in 0..5 {
            records.push(ver_rec("0.28.0", 30 - d, 10, 0));
        }
        let slices = group_by_version(&records);
        assert!(
            detect_severity_regression(
                &slices,
                DEFAULT_REGRESSION_RATIO,
                DEFAULT_REGRESSION_MIN_FILES
            )
            .is_none()
        );
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
        let none_slice = slices
            .iter()
            .find(|s| s.key == "(no repo)")
            .expect("None repo should produce a '(no repo)' sentinel, got keys: {:?}");
        let real_slice = slices
            .iter()
            .find(|s| s.key == "unknown")
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
        assert_eq!(
            slices.len(),
            2,
            "None and real '(no repo)' must bucket separately"
        );
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
        assert!(
            (fpk - expected).abs() < 1e-12,
            "got {fpk}, expected {expected}"
        );
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
        assert!(
            slices[0].low_sample,
            "n < MIN_SAMPLE should set low_sample=true"
        );
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
        r.suppressed_by_rule.insert("b".into(), 1); // +1 = 2^32 (truncates to 0 as u32)
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
            sources_queried: 0,
            sources_contributing: 0,
            per_source_contributions: std::collections::BTreeMap::new(),
            dep_boost_applied: 0,
            current_repo_reserved_available: 0,
            current_repo_reserved_filled: 0,
            structural_fingerprints_computed: 0,
            structural_knn_queries: 0,
            structural_knn_hits: 0,
            structural_boost_applied: 0,
        };
        r
    }

    #[test]
    fn by_source_flattens_injected_sources_into_per_source_rows() {
        // 4 records: one with 2 sources should contribute to both buckets.
        // Verify unique-source row count and per-source review counts.
        let records = vec![
            ctx_rec("r", true, &["mini-rust"], 2, 100, false, false, Some("h1")),
            ctx_rec(
                "r",
                true,
                &["mini-rust", "mini-py"],
                4,
                200,
                false,
                false,
                Some("h2"),
            ),
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
            records.push(ctx_rec(
                "A",
                true,
                &["src"],
                2,
                100,
                false,
                false,
                Some("h"),
            ));
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
        let records = vec![ctx_rec("r", true, &[], 0, 0, true, false, Some("dual"))];
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
            records.push(ctx_rec(
                "r",
                true,
                &["new"],
                2,
                100,
                false,
                false,
                Some("h"),
            ));
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

    // -- file hotspot tests --

    fn fb_entry(file: &str, verdict: Verdict, ts: DateTime<Utc>) -> FeedbackEntry {
        FeedbackEntry {
            file_path: file.into(),
            finding_title: "test finding".into(),
            finding_category: "test".into(),
            verdict,
            reason: "test".into(),
            model: None,
            timestamp: ts,
            provenance: Provenance::Human,
            fp_kind: None,
            finding_id: None,
            rule_id: None,
            in_diff: None,
            skill_name: None,
            skill_version: None,
            manifest_sha256: None,
        }
    }

    #[test]
    fn group_by_file_empty_input() {
        let rows = group_by_file(&[], None);
        assert!(rows.is_empty());
    }

    #[test]
    fn group_by_file_aggregates_verdicts() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry("src/main.rs", Verdict::Tp, t),
            fb_entry("src/main.rs", Verdict::Tp, t),
            fb_entry("src/main.rs", Verdict::Fp, t),
            fb_entry("src/main.rs", Verdict::Wontfix, t),
            fb_entry("src/main.rs", Verdict::Partial, t),
        ];
        let rows = group_by_file(&entries, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tp_count, 2);
        assert_eq!(rows[0].fp_count, 1);
        assert_eq!(rows[0].wontfix_count, 1);
        assert_eq!(rows[0].partial_count, 1);
        assert_eq!(rows[0].total, 5);
    }

    #[test]
    fn group_by_file_sorted_by_tp_count_desc() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry("low.rs", Verdict::Tp, t),
            fb_entry("high.rs", Verdict::Tp, t),
            fb_entry("high.rs", Verdict::Tp, t),
            fb_entry("high.rs", Verdict::Tp, t),
            fb_entry("mid.rs", Verdict::Tp, t),
            fb_entry("mid.rs", Verdict::Tp, t),
        ];
        let rows = group_by_file(&entries, None);
        assert_eq!(rows[0].file_path, "high.rs");
        assert_eq!(rows[1].file_path, "mid.rs");
        assert_eq!(rows[2].file_path, "low.rs");
    }

    #[test]
    fn group_by_file_top_n_limits_output() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry("a.rs", Verdict::Tp, t),
            fb_entry("b.rs", Verdict::Tp, t),
            fb_entry("c.rs", Verdict::Tp, t),
        ];
        let rows = group_by_file(&entries, Some(2));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn group_by_file_top_n_none_returns_all() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry("a.rs", Verdict::Tp, t),
            fb_entry("b.rs", Verdict::Tp, t),
            fb_entry("c.rs", Verdict::Tp, t),
        ];
        let rows = group_by_file(&entries, None);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn group_by_file_last_seen_is_max_timestamp() {
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry("src/main.rs", Verdict::Tp, t1),
            fb_entry("src/main.rs", Verdict::Fp, t2),
            fb_entry("src/main.rs", Verdict::Tp, t3),
        ];
        let rows = group_by_file(&entries, None);
        assert_eq!(rows[0].last_seen, t2);
    }

    #[test]
    fn group_by_file_skips_empty_file_path() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry("", Verdict::Tp, t),
            fb_entry("real.rs", Verdict::Tp, t),
        ];
        let rows = group_by_file(&entries, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file_path, "real.rs");
    }

    #[test]
    fn group_by_file_all_fp_file_still_appears() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry("noisy.rs", Verdict::Fp, t),
            fb_entry("noisy.rs", Verdict::Fp, t),
            fb_entry("good.rs", Verdict::Tp, t),
        ];
        let rows = group_by_file(&entries, None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].file_path, "good.rs");
        assert_eq!(rows[1].file_path, "noisy.rs");
        assert_eq!(rows[1].tp_count, 0);
        assert_eq!(rows[1].fp_count, 2);
    }

    // ── group_by_rule tests ──

    fn feedback_entry_with_rule(rule_id: Option<&str>, verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "test".into(),
            finding_category: "security".into(),
            verdict,
            reason: "".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: Provenance::Human,
            fp_kind: None,
            finding_id: None,
            rule_id: rule_id.map(String::from),
            in_diff: None,
            skill_name: None,
            skill_version: None,
            manifest_sha256: None,
        }
    }

    #[test]
    fn group_by_rule_buckets_by_rule_id() {
        let entries = vec![
            feedback_entry_with_rule(Some("local-ast:python/eval-exec"), Verdict::Tp),
            feedback_entry_with_rule(Some("local-ast:python/eval-exec"), Verdict::Fp),
            feedback_entry_with_rule(Some("ast-grep:python/bare-except-pass"), Verdict::Tp),
            feedback_entry_with_rule(None, Verdict::Tp),
        ];
        let slices = group_by_rule(&entries, None);
        assert_eq!(slices.len(), 2, "None entries should be excluded");
        let eval_slice = slices
            .iter()
            .find(|s| s.key == "local-ast:python/eval-exec")
            .unwrap();
        assert_eq!(eval_slice.tp, 1);
        assert_eq!(eval_slice.fp, 1);
        assert!((eval_slice.precision - 0.5).abs() < 0.01);
    }

    #[test]
    fn group_by_rule_filters_by_glob() {
        let entries = vec![
            feedback_entry_with_rule(Some("local-ast:python/eval-exec"), Verdict::Tp),
            feedback_entry_with_rule(Some("ast-grep:typescript/as-any-cast"), Verdict::Tp),
        ];
        let slices = group_by_rule(&entries, Some("local-ast:python/*"));
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].key, "local-ast:python/eval-exec");
    }

    #[test]
    fn group_by_rule_empty_input() {
        let slices = group_by_rule(&[], None);
        assert!(slices.is_empty());
    }

    #[test]
    fn group_by_rule_low_sample_flag() {
        let entries = vec![feedback_entry_with_rule(Some("rule-a"), Verdict::Tp)];
        let slices = group_by_rule(&entries, None);
        assert!(
            slices[0].low_sample,
            "1 entry < MIN_SAMPLE should be flagged"
        );
    }

    #[test]
    fn group_by_file_context_misleading_only_file_excluded() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![
            fb_entry(
                "misleading.rs",
                Verdict::ContextMisleading {
                    blamed_chunk_ids: vec![],
                },
                t,
            ),
            fb_entry("real.rs", Verdict::Tp, t),
        ];
        let rows = group_by_file(&entries, None);
        assert_eq!(
            rows.len(),
            1,
            "context_misleading-only file should be excluded"
        );
        assert_eq!(rows[0].file_path, "real.rs");
    }

    // ─── #491: skill audit rollup ───

    fn inv(skill: &str, findings: u32, offset_secs: i64) -> SkillInvocationRecord {
        use crate::skill_audit::AxisSelectionSource;
        SkillInvocationRecord {
            skill_run_id: format!("run-{skill}-{offset_secs}"),
            run_id: "review-1".into(),
            ts: Utc::now() + chrono::Duration::seconds(offset_secs),
            skill_name: skill.into(),
            skill_version: "1.0.0".into(),
            manifest_sha256: "a".repeat(64),
            prompt_family: "default".into(),
            prompt_sha256: "b".repeat(64),
            model: "gpt-5.6".into(),
            model_was_fallback: false,
            axis_selection_source: AxisSelectionSource::Default,
            capability_mode: "pure".into(),
            trust_tier: "bundled".into(),
            file_path: "src/main.rs".into(),
            file_sha256: "c".repeat(64),
            tokens_in: 100,
            tokens_out: 20,
            tokens_cache_read: 0,
            llm_cache_hit: false,
            duration_ms: 1000,
            findings_emitted: findings,
            findings_clamped: 0,
            findings_dropped_invalid_json: 0,
            parse_error_class: None,
            exit_status: ExitStatus::Ok,
            failure_reason: None,
            calibrator_suppressions: 0,
            calibrator_precedents_matched: 0,
        }
    }

    #[test]
    fn group_by_skill_counts_findings_and_zero_runs() {
        let records = vec![
            inv("security", 2, 0),
            inv("security", 0, 1),
            inv("correctness", 1, 2),
        ];
        let rows = group_by_skill(&records);
        let sec = rows.iter().find(|r| r.skill == "security").unwrap();
        assert_eq!(sec.runs, 2);
        assert_eq!(sec.findings_emitted, 2);
        assert_eq!(sec.zero_finding_runs, 1);
        assert_eq!(sec.zero_finding_rate(), 0.5);
        assert_eq!(sec.avg_duration_ms, 1000);
        assert!(sec.low_sample, "2 runs is under MIN_SAMPLE");
    }

    #[test]
    fn zero_streak_counts_only_trailing_silent_runs() {
        // The 440-invocation blackout: a skill that used to work and then
        // went silent must show the streak, not just the lifetime rate.
        let mut records = vec![inv("axis", 3, 0)];
        for i in 1..=6 {
            records.push(inv("axis", 0, i));
        }
        let rows = group_by_skill(&records);
        assert_eq!(rows[0].zero_streak, 6);
        assert_eq!(rows[0].zero_finding_runs, 6);
    }

    #[test]
    fn zero_streak_resets_on_a_recent_finding() {
        let records = vec![inv("axis", 0, 0), inv("axis", 0, 1), inv("axis", 1, 2)];
        assert_eq!(group_by_skill(&records)[0].zero_streak, 0);
    }

    #[test]
    fn group_by_skill_sorts_blackouts_first() {
        let mut records: Vec<_> = (0..10).map(|i| inv("healthy", 2, i)).collect();
        records.extend((10..13).map(|i| inv("silent", 0, i)));
        let rows = group_by_skill(&records);
        assert_eq!(rows[0].skill, "silent", "the blackout must lead: {rows:?}");
    }

    #[test]
    fn group_by_skill_histograms_parse_errors_and_failures() {
        use crate::skill_audit::FailureReason;
        use quorum::skill_output::ParseErrorClass;
        let mut a = inv("security", 0, 0);
        a.parse_error_class = Some(ParseErrorClass::WrongSchema);
        let mut b = inv("security", 0, 1);
        b.parse_error_class = Some(ParseErrorClass::WrongSchema);
        let mut c = inv("security", 0, 2);
        c.exit_status = ExitStatus::Error;
        c.failure_reason = Some(FailureReason::ModelTimeout);

        let rows = group_by_skill(&[a, b, c]);
        assert_eq!(rows[0].parse_error_classes.get("wrong_schema"), Some(&2));
        assert_eq!(rows[0].errors, 1);
        assert_eq!(rows[0].failure_reasons.get("ModelTimeout"), Some(&1));
    }

    #[test]
    fn group_by_skill_on_empty_input_is_empty() {
        assert!(group_by_skill(&[]).is_empty());
    }

    // ─── #491: integrator audit rollup ───

    fn dec(
        decision: IntegratorDecision,
        pre: &str,
        post: &str,
        reason: &str,
    ) -> IntegratorDecisionRecord {
        use crate::skill_audit::ClusterKey;
        IntegratorDecisionRecord {
            run_id: "review-1".into(),
            ts: Utc::now(),
            decision,
            cluster_key: ClusterKey {
                file_path: "src/main.rs".into(),
                line_range: (1, 2),
                finding_kind: "security".into(),
            },
            input_finding_ids: vec!["f1".into()],
            input_confidences: vec![0.8],
            input_severities: vec![pre.into()],
            calibrator_weights: Default::default(),
            confidence_floor: 0.3,
            output_finding_id: Some("f1".into()),
            output_confidence: 0.7,
            severity_pre_clamp: pre.into(),
            severity_post_clamp: post.into(),
            reason: reason.into(),
            originating_skills: vec!["security".into()],
        }
    }

    #[test]
    fn integrator_rollup_counts_decisions_and_shares() {
        let records = vec![
            dec(IntegratorDecision::Merged, "high", "high", "duplicate"),
            dec(
                IntegratorDecision::Suppressed,
                "high",
                "high",
                "below floor",
            ),
            dec(
                IntegratorDecision::Suppressed,
                "high",
                "high",
                "below floor",
            ),
        ];
        let (rows, _) = group_by_integrator_decision(&records);
        assert_eq!(rows[0].decision, "suppressed");
        assert_eq!(rows[0].count, 2);
        assert!((rows[0].share - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(rows[0].reasons.get("below floor"), Some(&2));
        assert!((rows[0].avg_output_confidence - 0.7).abs() < 1e-9);
    }

    #[test]
    fn integrator_rollup_surfaces_severity_collapse() {
        // v0.28.0 collapsed crit+high per file from 1.17 to 0.014 and it sat
        // unread for two months (#491). The transition histogram is the tell.
        let records = vec![
            dec(IntegratorDecision::Merged, "high", "medium", "clamped"),
            dec(IntegratorDecision::Merged, "high", "medium", "clamped"),
            dec(IntegratorDecision::Merged, "critical", "low", "clamped"),
        ];
        let (rows, transitions) = group_by_integrator_decision(&records);
        assert_eq!(rows[0].severity_changed, 3);
        assert_eq!(transitions.get("high->medium"), Some(&2));
        assert_eq!(transitions.get("critical->low"), Some(&1));
    }

    #[test]
    fn integrator_rollup_on_empty_input_is_empty() {
        let (rows, transitions) = group_by_integrator_decision(&[]);
        assert!(rows.is_empty());
        assert!(transitions.is_empty());
    }
}
