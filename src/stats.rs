/// Stats dashboard -- reads local data files and computes metrics.
use crate::analytics;
use crate::dimensions::{self, ContextDimensionSlice, DimensionSlice};
use crate::feedback::FeedbackStore;
use crate::formatting;
use crate::glyphs;
use crate::output::Style;
use crate::review_log::ReviewLog;
use crate::telemetry::TelemetryStore;

/// Highlights cap: we show the most active slices, trimmed hard to keep the
/// default dashboard compact. Callers wanting everything use --by-repo etc.
const HIGHLIGHT_TOP_N: usize = 3;
/// Rolling-50 windows for the precision trajectory sparkline. 4 windows
/// covers ~200 recent reviews which is enough to read a trend.
const ROLLING_N: usize = 50;
const ROLLING_WINDOWS: usize = 4;

pub struct StatsReport {
    pub feedback_count: usize,
    pub precision: f64,
    pub tp: usize,
    pub fp: usize,
    pub partial: usize,
    pub wontfix: usize,
    pub precision_trend: Vec<analytics::PrecisionWindow>,
    pub reviews_7d: usize,
    pub findings_per_review: f64,
    pub suppression_rate: f64,
    pub tokens_in_7d: u64,
    pub tokens_out_7d: u64,
    pub cost_7d: f64,
    pub tokens_per_finding: f64,
    pub model: String,
    pub top_repos: Vec<DimensionSlice>,
    pub top_callers: Vec<DimensionSlice>,
    pub rolling_windows: Vec<DimensionSlice>,
    /// Provenance-tier breakdown (Human / PostFix / External / AutoCalib /
    /// Unknown). Computed from the same feedback store as `precision`.
    pub tier_summary: analytics::TierSummary,

    // ─── Stats redesign Phase A: linkage / capture / external overlap ───
    //
    /// Fraction of feedback entries whose finding_id matches some review's
    /// finding_ids. Gates whether per-finding precision math is shown.
    pub linkage_rate: f64,
    pub linkage_linked: usize,
    pub linkage_unlinked: usize,
    /// Fraction of recent (7d) findings that received any feedback. Used
    /// inline with the headline trend so trend footing is visible.
    pub capture_rate: f64,
    pub capture_labeled: usize,
    pub capture_total: usize,
    /// True iff `linkage_rate >= 0.85`. Drives the headline-trend
    /// rendering between per-finding (`precision_trend_per_finding`) and
    /// the entry-level fallback with banner.
    pub headline_trend_uses_finding_id: bool,
    /// Per-agent overlap with quorum's own (Human-tier) verdicts. Surfaced
    /// in the External corpus block.
    pub external_overlap: analytics::ExternalOverlap,
    /// Per-finding precision trend (None when linkage rate is below the
    /// per-finding gate so renderers don't have to recompute the cutoff).
    pub precision_trend_per_finding: Vec<analytics::PrecisionWindow>,
}

/// Take top-N slices by review volume. Ties resolved by insertion order
/// (which reflects first-seen-in-log); stable enough for a highlight.
fn take_top(mut slices: Vec<DimensionSlice>, n: usize) -> Vec<DimensionSlice> {
    slices.sort_by_key(|s| std::cmp::Reverse(s.n_reviews));
    slices.truncate(n);
    slices
}

pub fn compute_report(
    feedback_store: &FeedbackStore,
    telemetry_store: &TelemetryStore,
    review_log: &ReviewLog,
) -> anyhow::Result<StatsReport> {
    let feedback = feedback_store.load_all().unwrap_or_default();
    let feedback_count = feedback.len();
    let tier_summary = analytics::compute_tier_stats(&feedback);

    // Aggregate feedback stats
    let stats = analytics::compute_stats(&feedback);
    let total_tp: usize = stats.values().map(|s| s.tp).sum();
    let total_fp: usize = stats.values().map(|s| s.fp).sum();
    let total_partial: usize = stats.values().map(|s| s.partial).sum();
    let total_wontfix: usize = stats.values().map(|s| s.wontfix).sum();
    let relevant = total_tp + total_partial;
    let precision_denom = relevant + total_fp;
    let precision = if precision_denom > 0 {
        relevant as f64 / precision_denom as f64
    } else {
        0.0
    };

    // Precision trend (7-day windows)
    let precision_trend = analytics::precision_trend(&feedback, 7);

    // Telemetry: last 7 days
    let since_7d = chrono::Utc::now() - chrono::Duration::days(7);
    let recent = telemetry_store.load_since(since_7d).unwrap_or_default();
    let reviews_7d = recent.len();
    let tokens_in_7d: u64 = recent.iter().map(|e| e.tokens_in).sum();
    let tokens_out_7d: u64 = recent.iter().map(|e| e.tokens_out).sum();

    let total_findings_7d: usize = recent
        .iter()
        .map(|e| e.findings.values().sum::<usize>())
        .sum();
    let total_suppressed_7d: usize = recent.iter().map(|e| e.suppressed).sum();

    let findings_per_review = if reviews_7d > 0 {
        total_findings_7d as f64 / reviews_7d as f64
    } else {
        0.0
    };

    let suppression_rate = if total_findings_7d + total_suppressed_7d > 0 {
        total_suppressed_7d as f64 / (total_findings_7d + total_suppressed_7d) as f64
    } else {
        0.0
    };

    let tokens_per_finding = if total_findings_7d > 0 {
        (tokens_in_7d + tokens_out_7d) as f64 / total_findings_7d as f64
    } else {
        0.0
    };

    // Most frequent model in recent telemetry
    let mut model_counts = std::collections::HashMap::<String, usize>::new();
    for entry in &recent {
        if !entry.model.is_empty() {
            *model_counts.entry(entry.model.clone()).or_insert(0) += 1;
        }
    }
    let model = model_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(m, _)| m)
        .unwrap_or_default();

    let cost_7d = formatting::estimate_cost(&model, tokens_in_7d, tokens_out_7d);

    // Dimensional highlights. Best-effort: missing reviews.jsonl yields
    // empty vectors, which the renderer treats as "no data" and hides.
    let review_records = review_log.load_all().unwrap_or_default();
    let top_repos = take_top(dimensions::group_by_repo(&review_records), HIGHLIGHT_TOP_N);
    let top_callers = take_top(
        dimensions::group_by_caller(&review_records),
        HIGHLIGHT_TOP_N,
    );
    let rolling_windows = dimensions::rolling_window(&review_records, ROLLING_N, ROLLING_WINDOWS);

    // Linkage / capture / external overlap (Phase A).
    let link = analytics::linkage_stats(&review_records, &feedback);
    let linkage_rate = link.rate();
    let headline_trend_uses_finding_id = linkage_rate >= 0.85;

    let capture_total = total_findings_7d;
    // Capture-labeled = feedback entries timestamped in the 7d window.
    // Coarse but useful — exact would require joining each feedback row
    // to a review timestamp, which we deliberately don't do here.
    let capture_labeled = feedback.iter().filter(|e| e.timestamp >= since_7d).count();
    let capture_rate = if capture_total > 0 {
        (capture_labeled as f64 / capture_total as f64).min(1.0)
    } else {
        0.0
    };

    // Build a quorum verdict map (Human-tier) keyed by finding_id for
    // External overlap computation.
    let mut quorum_verdicts: std::collections::HashMap<String, crate::feedback::Verdict> =
        std::collections::HashMap::new();
    for e in &feedback {
        if !matches!(e.provenance, crate::feedback::Provenance::Human) {
            continue;
        }
        if let Some(fid) = &e.finding_id {
            quorum_verdicts.insert(fid.clone(), e.verdict.clone());
        }
    }
    let external_overlap = analytics::compute_external_overlap(&feedback, &quorum_verdicts);

    let precision_trend_per_finding = if headline_trend_uses_finding_id {
        analytics::precision_trend_per_finding(&feedback, 7)
    } else {
        Vec::new()
    };

    Ok(StatsReport {
        feedback_count,
        precision,
        tp: total_tp,
        fp: total_fp,
        partial: total_partial,
        wontfix: total_wontfix,
        precision_trend,
        reviews_7d,
        findings_per_review,
        suppression_rate,
        tokens_in_7d,
        tokens_out_7d,
        cost_7d,
        tokens_per_finding,
        model,
        top_repos,
        top_callers,
        rolling_windows,
        tier_summary,
        linkage_rate,
        linkage_linked: link.linked,
        linkage_unlinked: link.unlinked,
        capture_rate,
        capture_labeled,
        capture_total,
        headline_trend_uses_finding_id,
        external_overlap,
        precision_trend_per_finding,
    })
}

/// Default-shape dashboard without dimensional highlights. For callers that
/// want the pre-highlights output; reachable via `stats --minimal`.
pub fn format_human_minimal(report: &StatsReport, style: &Style) -> String {
    format_human_core(report, style)
}

/// Minimum window count for showing a precision percentage. Below this
/// the window is rendered as `n<30` to flag low-N noise — matches the
/// design doc's stability cutoff.
const MIN_TREND_WINDOW_N: usize = 30;

/// Render the External corpus block — per-agent contribution and
/// agreement rate against quorum's Human-tier verdicts.
pub fn format_external_corpus(overlap: &analytics::ExternalOverlap) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(out, "\nExternal corpus").unwrap();
    for agent in &overlap.per_agent {
        if agent.overlap == 0 {
            writeln!(
                out,
                "  {agent}: {findings} entries (no overlap with quorum verdicts)",
                agent = agent.agent,
                findings = agent.findings,
            )
            .unwrap();
        } else {
            writeln!(
                out,
                "  {agent}: {findings} entries, {overlap} overlap, {pct}% agreement",
                agent = agent.agent,
                findings = agent.findings,
                overlap = agent.overlap,
                pct = (agent.agreement_rate() * 100.0).round() as u32,
            )
            .unwrap();
        }
    }
    out
}

/// Render the headline precision trend with Wilson CI on the most recent
/// window. Uses `precision_trend_per_finding` when the linkage gate is
/// open; otherwise falls back to entry-level `precision_trend` with a
/// "entry-level pending finding-id rollout" banner.
pub fn format_headline_trend(report: &StatsReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let (windows, banner) = if report.headline_trend_uses_finding_id {
        (&report.precision_trend_per_finding, None)
    } else {
        (
            &report.precision_trend,
            Some("entry-level pending finding-id rollout"),
        )
    };

    if windows.is_empty() {
        if let Some(b) = banner {
            writeln!(out, "  Trend: (no data) — {b}").unwrap();
        } else {
            writeln!(out, "  Trend: (no data)").unwrap();
        }
        return out;
    }

    // Build "77 → 81 → 78 → 76%" chain — tail window keeps `%` to anchor
    // the eye on the current value.
    let mut chain = String::new();
    for (i, w) in windows.iter().enumerate() {
        let is_last = i == windows.len() - 1;
        let cell = if w.count < MIN_TREND_WINDOW_N {
            "n<30".to_string()
        } else if is_last {
            format!("{}%", (w.precision * 100.0).round() as u32)
        } else {
            format!("{}", (w.precision * 100.0).round() as u32)
        };
        if i > 0 {
            chain.push_str(" → ");
        }
        chain.push_str(&cell);
    }

    let tail = windows.last().unwrap();
    let ci = if tail.count >= MIN_TREND_WINDOW_N && tail.precision_denom > 0 {
        let successes = (tail.precision * tail.precision_denom as f64).round() as usize;
        let (lo, hi) = crate::stats_math::wilson_interval(successes, tail.precision_denom, 0.95);
        format!(
            " [{lo}-{hi}]",
            lo = (lo * 100.0).round() as u32,
            hi = (hi * 100.0).round() as u32,
        )
    } else {
        String::new()
    };

    let n_annotation = format!(" n={}", tail.count);
    let capture = if report.capture_total > 0 {
        format!(
            ", capture: {}% ({}/{})",
            (report.capture_rate * 100.0).round() as u32,
            report.capture_labeled,
            report.capture_total,
        )
    } else {
        String::new()
    };

    if let Some(b) = banner {
        writeln!(out, "  Trend: {chain}{ci}{n_annotation}{capture} — {b}").unwrap();
    } else {
        writeln!(out, "  Trend: {chain}{ci}{n_annotation}{capture}").unwrap();
    }
    out
}

pub fn format_human(report: &StatsReport, style: &Style) -> String {
    // Default = !full: hide By caller and Rolling, keep By repo. See
    // format_human_with_full for the full-fat variant.
    format_human_with_full(report, style, false)
}

/// Render the dashboard, gating dimensional drill-downs (By caller,
/// Rolling N) on `full`. By repo stays in default — it's the most
/// scannable orientation.
pub fn format_human_with_full(report: &StatsReport, style: &Style, full: bool) -> String {
    let mut out = format_human_core(report, style);
    let unicode = crate::output::unicode_ok_default();
    if !report.top_repos.is_empty() {
        out.push_str(&format_highlight_block(
            "By repo (top)",
            &report.top_repos,
            style,
            unicode,
        ));
    }
    if full {
        if !report.top_callers.is_empty() {
            out.push_str(&format_highlight_block(
                "By caller (top)",
                &report.top_callers,
                style,
                unicode,
            ));
        }
        if !report.rolling_windows.is_empty() {
            out.push_str(&format_highlight_block(
                "Rolling windows (50 reviews each)",
                &report.rolling_windows,
                style,
                unicode,
            ));
        }
    }
    out
}

/// One highlight section: a mini-table (up to 3 rows) sized to not compete
/// with the full --by-repo/--by-caller tables. Intentionally narrower than
/// format_dimension_table.
fn format_highlight_block(
    title: &str,
    slices: &[DimensionSlice],
    style: &Style,
    unicode: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "\n{bold}{title}{reset}\n",
        bold = style.bold,
        reset = style.reset,
        title = title,
    ));
    for s in slices {
        let key = if s.key.chars().count() > 18 {
            let t: String = s.key.chars().take(17).collect();
            format!("{}.", t)
        } else {
            s.key.clone()
        };
        let trend = if s.sparkline_points.is_empty() {
            String::new()
        } else {
            let spark = glyphs::sparkline(&s.sparkline_points, unicode);
            let arrow = glyphs::trend_arrow(&s.sparkline_points, unicode);
            format!("  {} {}", spark, arrow)
        };
        let low = if s.low_sample {
            format!(
                "  {dim}(low sample){reset}",
                dim = style.dim,
                reset = style.reset
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  {key:<18}  {reviews:>4} reviews  {fpf:>5.1} find/file{trend}{low}\n",
            key = key,
            reviews = s.n_reviews,
            fpf = s.findings_per_file,
            trend = trend,
            low = low,
        ));
    }
    out
}

fn format_human_core(report: &StatsReport, style: &Style) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "{bold}Feedback Health{reset}\n",
        bold = style.bold,
        reset = style.reset
    ));
    out.push_str(&format!(
        "  Entries: {}  Precision: {}\n",
        formatting::format_count(report.feedback_count as u64),
        formatting::format_pct(report.precision),
    ));
    out.push_str(&format!(
        "  TP: {}  FP: {}  Partial: {}  Wontfix: {}\n",
        report.tp, report.fp, report.partial, report.wontfix,
    ));

    // Channel attribution table — counts only, no precision column.
    // Always rendered (even all-zero Human) so the dashboard shape is
    // stable; format_channel_attribution hides empty non-Human channels.
    out.push('\n');
    out.push_str(&analytics::format_channel_attribution(&report.tier_summary));

    // Headline trend with Wilson CI on the most recent window, plus
    // capture-rate inline so trend footing is visible.
    out.push_str(&format_headline_trend(report));

    // External corpus block — per-agent overlap + agreement rate.
    if !report.external_overlap.per_agent.is_empty() {
        out.push_str(&format_external_corpus(&report.external_overlap));
    }

    out.push_str(&format!(
        "\n{bold}Activity (last 7 days){reset}\n",
        bold = style.bold,
        reset = style.reset
    ));
    out.push_str(&format!(
        "  Reviews: {}  Findings/review: {:.1}  Suppression: {}\n",
        report.reviews_7d,
        report.findings_per_review,
        formatting::format_pct(report.suppression_rate),
    ));

    if report.tokens_in_7d > 0 || report.tokens_out_7d > 0 {
        out.push_str(&format!(
            "\n{bold}Spend (last 7 days){reset}\n",
            bold = style.bold,
            reset = style.reset
        ));
        out.push_str(&format!(
            "  Tokens: {} in / {} out  Cost: {}  Tokens/finding: {}\n",
            formatting::format_count(report.tokens_in_7d),
            formatting::format_count(report.tokens_out_7d),
            formatting::format_cost(report.cost_7d),
            formatting::format_count(report.tokens_per_finding as u64),
        ));
    }

    out
}

/// Render a dimensional-stats table (by-repo / by-caller / rolling) for humans.
/// Follows DESIGN.md §4 (dim labels, default values, 2-space indent) and §11 (numeric formatting).
pub fn format_dimension_table(
    mode: &str,
    slices: &[DimensionSlice],
    style: &Style,
    unicode: bool,
) -> String {
    let mut out = String::new();
    let key_header = match mode {
        "by-caller" => "Caller",
        "rolling" => "Window",
        _ => "Repo",
    };

    out.push_str(&format!(
        "{bold}~ Stats: {mode}{reset}\n\n",
        bold = style.bold,
        reset = style.reset,
        mode = mode,
    ));

    if slices.is_empty() {
        out.push_str("  (no data)\n");
        return out;
    }

    // Column widths -- fixed so layout stays aligned in monospace terminals.
    let key_width = 16usize;
    out.push_str(&format!(
        "  {bold}{key:<kw$}  {:>7}  {:>13}  {:<22}  {:<16}{reset}\n",
        "Reviews",
        "Findings/file",
        "Accept rate",
        "Trend",
        bold = style.bold,
        reset = style.reset,
        key = key_header,
        kw = key_width,
    ));

    for s in slices {
        let display_key = truncate_key(&s.key, key_width);

        let accept_cell = match s.accept_rate {
            Some(r) if !s.low_sample => {
                let bar = glyphs::hbar(r * 100.0, 100.0, unicode);
                let pct = format!("{:>3}%", (r * 100.0).round() as i64);
                format!(
                    "{}{bar}{reset} {}",
                    color_for_accept(r, style),
                    pct,
                    bar = bar,
                    reset = style.reset
                )
            }
            _ => format!(
                "{dim}—                    {reset}",
                dim = style.dim,
                reset = style.reset
            ),
        };

        let trend_cell = if s.sparkline_points.is_empty() {
            format!("{dim}—{reset}", dim = style.dim, reset = style.reset)
        } else {
            let spark = glyphs::sparkline(&s.sparkline_points, unicode);
            let arrow = glyphs::trend_arrow(&s.sparkline_points, unicode);
            format!("{} {}", spark, arrow)
        };

        let low_tag = if s.low_sample {
            format!(
                "  {dim}(low sample){reset}",
                dim = style.dim,
                reset = style.reset
            )
        } else {
            String::new()
        };

        out.push_str(&format!(
            "  {key:<kw$}  {reviews:>7}  {fpf:>13.1}  {accept:<22}  {trend:<16}{low}\n",
            key = display_key,
            kw = key_width,
            reviews = s.n_reviews,
            fpf = s.findings_per_file,
            accept = accept_cell,
            trend = trend_cell,
            low = low_tag,
        ));
    }

    // Totals line (dim).
    let total_reviews: u32 = slices.iter().map(|s| s.n_reviews).sum();
    let low_count = slices.iter().filter(|s| s.low_sample).count();
    let low_note = if low_count > 0 {
        format!(" ({} low-sample)", low_count)
    } else {
        String::new()
    };
    out.push_str(&format!(
        "\n  {dim}{} {}  {} reviews{}{reset}\n",
        slices.len(),
        if slices.len() == 1 {
            unit_label_singular(mode)
        } else {
            unit_label_plural(mode)
        },
        total_reviews,
        low_note,
        dim = style.dim,
        reset = style.reset,
    ));

    out
}

fn truncate_key(key: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if key.chars().count() <= width {
        key.to_string()
    } else if width == 1 {
        // Can't fit ellipsis; just take the first char so layout stays aligned.
        key.chars().take(1).collect()
    } else {
        let mut s: String = key.chars().take(width - 1).collect();
        s.push('.');
        s
    }
}

fn color_for_accept(rate: f64, style: &Style) -> &str {
    if rate >= 0.70 {
        style.green
    } else if rate < 0.40 {
        style.red
    } else {
        ""
    }
}

fn unit_label_singular(mode: &str) -> &'static str {
    match mode {
        "by-caller" => "caller",
        "rolling" => "window",
        _ => "repo",
    }
}

fn unit_label_plural(mode: &str) -> &'static str {
    match mode {
        "by-caller" => "callers",
        "rolling" => "windows",
        _ => "repos",
    }
}

/// Compact one-line dimensional summary (LLM-targeted, no glyphs per DESIGN.md §2).
pub fn format_dimension_compact(mode: &str, slices: &[DimensionSlice]) -> String {
    let mut parts = Vec::with_capacity(slices.len() + 1);
    let mut low_count = 0usize;
    for s in slices {
        if s.low_sample {
            low_count += 1;
            continue;
        }
        let acc = s
            .accept_rate
            .map(|r| format!(" acc{}", (r * 100.0).round() as i64))
            .unwrap_or_default();
        parts.push(format!(
            "{}(n{} fpf{:.1}{})",
            s.key, s.n_reviews, s.findings_per_file, acc,
        ));
    }
    let low_suffix = if low_count > 0 {
        format!(" +{} low-sample", low_count)
    } else {
        String::new()
    };
    format!("{}: {}{}", mode, parts.join(" "), low_suffix)
}

pub fn format_compact(report: &StatsReport) -> String {
    let mut parts = vec![
        format!("feedback:{}", report.feedback_count),
        format!("precision:{}", formatting::format_pct(report.precision)),
        format!("tp:{}", report.tp),
        format!("fp:{}", report.fp),
    ];

    if !report.precision_trend.is_empty() {
        let trend: Vec<String> = report
            .precision_trend
            .iter()
            .map(|w| formatting::format_pct(w.precision))
            .collect();
        parts.push(format!("trend:{}", trend.join(">")));
    }

    parts.push(format!("reviews_7d:{}", report.reviews_7d));
    parts.push(format!(
        "findings_per_review:{:.1}",
        report.findings_per_review
    ));

    if report.tokens_in_7d > 0 {
        parts.push(format!(
            "tokens_in:{}",
            formatting::format_count(report.tokens_in_7d)
        ));
        parts.push(format!(
            "tokens_out:{}",
            formatting::format_count(report.tokens_out_7d)
        ));
        parts.push(format!("cost:{}", formatting::format_cost(report.cost_7d)));
    }

    parts.push(format!(
        "linkage:{}",
        formatting::format_pct(report.linkage_rate)
    ));
    parts.push(format!(
        "capture:{}",
        formatting::format_pct(report.capture_rate)
    ));

    if !report.external_overlap.per_agent.is_empty() {
        let agent_parts: Vec<String> = report
            .external_overlap
            .per_agent
            .iter()
            .map(|a| format!("{}:{}/{}", a.agent, a.agree, a.overlap))
            .collect();
        parts.push(format!("external:{}", agent_parts.join(",")));
    }

    if !report.precision_trend_per_finding.is_empty() {
        let pf: Vec<String> = report
            .precision_trend_per_finding
            .iter()
            .map(|w| formatting::format_pct(w.precision))
            .collect();
        parts.push(format!("per-finding:{}", pf.join(">")));
    }

    format!("{}\n", parts.join(" "))
}

/// Render a context-dimension table (--by-source / --by-reviewed-repo / --misleading)
/// using the same semigraphics conventions as `format_dimension_table`.
pub fn format_context_dimension_table(
    mode: &str,
    slices: &[ContextDimensionSlice],
    style: &Style,
    unicode: bool,
) -> String {
    let mut out = String::new();
    let key_header = match mode {
        "by-source" => "Source",
        "by-reviewed-repo" => "Repo",
        "misleading" => "Cause",
        _ => "Key",
    };

    out.push_str(&format!(
        "{bold}~ Stats: {mode}{reset}\n\n",
        bold = style.bold,
        reset = style.reset,
        mode = mode,
    ));

    if slices.is_empty() {
        out.push_str("  (no data)\n");
        return out;
    }

    let key_width = 20usize;
    out.push_str(&format!(
        "  {bold}{key:<kw$}  {:>7}  {:>9}  {:>9}  {:>10}  {:>10}  {:<16}{reset}\n",
        "Reviews",
        "AvgChunks",
        "AvgTokens",
        "ErrRate",
        "AdaptRate",
        "Trend",
        bold = style.bold,
        reset = style.reset,
        key = key_header,
        kw = key_width,
    ));

    for s in slices {
        let display_key = truncate_key(&s.key, key_width);

        let err_cell = rate_cell(s.retriever_error_rate, style, unicode);
        let adapt_cell = rate_cell(s.adaptive_threshold_rate, style, unicode);

        let trend_cell = if s.sparkline_points.is_empty() {
            format!("{dim}—{reset}", dim = style.dim, reset = style.reset)
        } else {
            let spark = glyphs::sparkline(&s.sparkline_points, unicode);
            let arrow = glyphs::trend_arrow(&s.sparkline_points, unicode);
            format!("{} {}", spark, arrow)
        };

        let low_tag = if s.low_sample {
            format!(
                "  {dim}(low sample){reset}",
                dim = style.dim,
                reset = style.reset
            )
        } else {
            String::new()
        };

        out.push_str(&format!(
            "  {key:<kw$}  {reviews:>7}  {chunks:>9.2}  {tokens:>9.1}  {err:<10}  {adapt:<10}  {trend:<16}{low}\n",
            key = display_key,
            kw = key_width,
            reviews = s.n_reviews,
            chunks = s.avg_injected_chunk_count,
            tokens = s.avg_injected_tokens,
            err = err_cell,
            adapt = adapt_cell,
            trend = trend_cell,
            low = low_tag,
        ));
    }

    let total_reviews: u32 = slices.iter().map(|s| s.n_reviews).sum();
    let low_count = slices.iter().filter(|s| s.low_sample).count();
    let low_note = if low_count > 0 {
        format!(" ({} low-sample)", low_count)
    } else {
        String::new()
    };
    out.push_str(&format!(
        "\n  {dim}{} rows  {} reviews{}{reset}\n",
        slices.len(),
        total_reviews,
        low_note,
        dim = style.dim,
        reset = style.reset,
    ));

    out
}

fn rate_cell(rate: f64, style: &Style, unicode: bool) -> String {
    let bar = glyphs::hbar(rate * 100.0, 100.0, unicode);
    let pct = format!("{:>3}%", (rate * 100.0).round() as i64);
    format!("{}{bar} {}{reset}", "", pct, bar = bar, reset = style.reset)
}

/// Compact one-line summary for context dimensions (LLM-targeted, no glyphs).
pub fn format_context_dimension_compact(mode: &str, slices: &[ContextDimensionSlice]) -> String {
    let mut parts = Vec::with_capacity(slices.len() + 1);
    let mut low_count = 0usize;
    for s in slices {
        if s.low_sample {
            low_count += 1;
            continue;
        }
        parts.push(format!(
            "{}(n{} ch{:.1} tk{:.0} err{} adp{})",
            s.key,
            s.n_reviews,
            s.avg_injected_chunk_count,
            s.avg_injected_tokens,
            (s.retriever_error_rate * 100.0).round() as i64,
            (s.adaptive_threshold_rate * 100.0).round() as i64,
        ));
    }
    let low_suffix = if low_count > 0 {
        format!(" +{} low-sample", low_count)
    } else {
        String::new()
    };
    format!("{}: {}{}", mode, parts.join(" "), low_suffix)
}

pub fn format_json(report: &StatsReport) -> anyhow::Result<String> {
    let json = serde_json::json!({
        "feedback_count": report.feedback_count,
        "precision": report.precision,
        "tp": report.tp,
        "fp": report.fp,
        "partial": report.partial,
        "wontfix": report.wontfix,
        "precision_trend": report.precision_trend.iter().map(|w| {
            serde_json::json!({
                "week_start": w.week_start.to_rfc3339(),
                "precision": w.precision,
                "count": w.count,
            })
        }).collect::<Vec<_>>(),
        "reviews_7d": report.reviews_7d,
        "findings_per_review": report.findings_per_review,
        "suppression_rate": report.suppression_rate,
        "tokens_in_7d": report.tokens_in_7d,
        "tokens_out_7d": report.tokens_out_7d,
        "cost_7d": report.cost_7d,
        "tokens_per_finding": report.tokens_per_finding,
        "model": report.model,
        "top_repos": report.top_repos,
        "top_callers": report.top_callers,
        "rolling_windows": report.rolling_windows,
        "linkage_rate": report.linkage_rate,
        "linkage_linked": report.linkage_linked,
        "linkage_unlinked": report.linkage_unlinked,
        "capture_rate": report.capture_rate,
        "capture_labeled": report.capture_labeled,
        "capture_total": report.capture_total,
        "headline_trend_uses_finding_id": report.headline_trend_uses_finding_id,
        "external_overlap": {
            "agents": report.external_overlap.per_agent.iter().map(|a| {
                serde_json::json!({
                    "agent": a.agent,
                    "findings": a.findings,
                    "overlap": a.overlap,
                    "agree": a.agree,
                    "agreement_rate": a.agreement_rate(),
                })
            }).collect::<Vec<_>>()
        },
        "precision_trend_per_finding": report.precision_trend_per_finding.iter().map(|w| {
            serde_json::json!({
                "week_start": w.week_start.to_rfc3339(),
                "precision": w.precision,
                "count": w.count,
            })
        }).collect::<Vec<_>>(),
    });
    Ok(serde_json::to_string_pretty(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    use tempfile::TempDir;

    fn make_review_log(
        dir: &TempDir,
        records: &[crate::review_log::ReviewRecord],
    ) -> crate::review_log::ReviewLog {
        let log = crate::review_log::ReviewLog::new(dir.path().join("reviews.jsonl"));
        for r in records {
            log.record(r).unwrap();
        }
        log
    }

    fn rec(repo: &str, caller: &str, findings: u32) -> crate::review_log::ReviewRecord {
        use crate::review_log::{Flags, ReviewRecord, SeverityCounts};
        ReviewRecord {
            run_id: ReviewRecord::new_ulid(),
            timestamp: chrono::Utc::now(),
            quorum_version: "test".into(),
            repo: Some(repo.into()),
            invoked_from: caller.into(),
            model: "gpt-5.4".into(),
            files_reviewed: 1,
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
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            duration_ms: 100,
            flags: Flags {
                deep: false,
                parallel_n: 1,
                ensemble: false,
            },
            mode: None,
            context: Default::default(),
            finding_ids: Vec::new(),
        }
    }

    // ─── Stats redesign Task 11: section label normalization ───

    #[test]
    fn format_human_uses_normalized_section_labels() {
        // The redesign explicitly states "(7d)" reads as jargon — replace
        // with "(last 7 days)". Same for "Rolling 50 reviews" which is
        // ambiguous about whether 50 is the window size or the count.
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let records: Vec<_> = (0..6).map(|_| rec("alpha", "claude_code", 2)).collect();
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        let out = format_human(&report, &Style::plain());
        assert!(
            !out.contains("(7d)"),
            "old jargon label '(7d)' should be replaced: {out}"
        );
        assert!(
            out.contains("last 7 days"),
            "expected 'last 7 days' label: {out}"
        );
    }

    // ─── Stats redesign Task 12: --full flag ───

    #[test]
    fn format_human_default_omits_by_caller_and_rolling() {
        // The default dashboard hides the dimensional drill-downs (caller,
        // rolling) — they live behind --full.
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let records: Vec<_> = (0..120).map(|_| rec("alpha", "claude_code", 2)).collect();
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        let out = format_human_with_full(&report, &Style::plain(), false);
        assert!(out.contains("By repo"), "By repo stays in default: {out}");
        assert!(
            !out.contains("By caller"),
            "By caller hides without --full: {out}"
        );
        assert!(
            !out.contains("Rolling"),
            "Rolling hides without --full: {out}"
        );
    }

    #[test]
    fn format_human_full_shows_all_dimensional_views() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let records: Vec<_> = (0..120).map(|_| rec("alpha", "claude_code", 2)).collect();
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        let out = format_human_with_full(&report, &Style::plain(), true);
        assert!(out.contains("By repo"));
        assert!(out.contains("By caller"));
        assert!(out.contains("Rolling"));
    }

    // ─── Stats redesign Task 10: headline trend rendering ───

    fn pw(precision: f64, count: usize) -> analytics::PrecisionWindow {
        analytics::PrecisionWindow {
            week_start: chrono::Utc::now(),
            precision,
            count,
            precision_denom: count,
        }
    }

    fn make_report_for_trend(
        trend_per_finding: Vec<analytics::PrecisionWindow>,
        capture_labeled: usize,
        capture_total: usize,
        uses_finding_id: bool,
    ) -> StatsReport {
        StatsReport {
            feedback_count: 0,
            precision: 0.0,
            tp: 0,
            fp: 0,
            partial: 0,
            wontfix: 0,
            precision_trend: vec![],
            reviews_7d: 0,
            findings_per_review: 0.0,
            suppression_rate: 0.0,
            tokens_in_7d: 0,
            tokens_out_7d: 0,
            cost_7d: 0.0,
            tokens_per_finding: 0.0,
            model: String::new(),
            top_repos: Vec::new(),
            top_callers: Vec::new(),
            rolling_windows: Vec::new(),
            tier_summary: analytics::TierSummary::default(),
            linkage_rate: 0.0,
            linkage_linked: 0,
            linkage_unlinked: 0,
            capture_rate: if capture_total > 0 {
                capture_labeled as f64 / capture_total as f64
            } else {
                0.0
            },
            capture_labeled,
            capture_total,
            headline_trend_uses_finding_id: uses_finding_id,
            external_overlap: analytics::ExternalOverlap::default(),
            precision_trend_per_finding: trend_per_finding,
        }
    }

    #[test]
    fn headline_trend_renders_arrow_chain_with_ci_on_current_window() {
        // 4-window trend, last window n=145 ≥ 30 → Wilson CI shown.
        let report = make_report_for_trend(
            vec![pw(0.77, 30), pw(0.81, 32), pw(0.78, 90), pw(0.76, 145)],
            212,
            1159,
            true,
        );
        let out = format_headline_trend(&report);
        assert!(out.contains("77 → 81"), "trend chain missing: {out}");
        assert!(out.contains("76%"), "current window pct missing: {out}");
        assert!(out.contains("["), "expected CI bracket: {out}");
        assert!(
            out.contains("n=145"),
            "expected sample-size annotation: {out}"
        );
        assert!(
            out.contains("capture"),
            "capture-rate inline missing: {out}"
        );
    }

    #[test]
    fn headline_trend_replaces_low_n_window_with_n_too_low() {
        let report = make_report_for_trend(
            vec![pw(0.77, 8), pw(0.81, 32), pw(0.78, 90), pw(0.76, 145)],
            10,
            100,
            true,
        );
        let out = format_headline_trend(&report);
        assert!(
            out.contains("n<30") || out.contains("n=8"),
            "low-n window must be marked: {out}"
        );
    }

    #[test]
    fn headline_trend_shows_legacy_banner_when_finding_id_unused() {
        let report = make_report_for_trend(vec![], 0, 0, false);
        let out = format_headline_trend(&report);
        assert!(
            out.contains("entry-level") && out.contains("finding-id"),
            "legacy banner should indicate the fallback: {out}"
        );
    }

    // ─── Stats redesign Task 6: extended StatsReport fields ───

    #[test]
    fn compute_report_populates_linkage_and_capture_metadata() {
        // Empty stores still expose the new fields with safe defaults so
        // callers don't have to None-check or panic on missing data.
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let rl = crate::review_log::ReviewLog::new(dir.path().join("reviews.jsonl"));
        let report = compute_report(&fb, &tl, &rl).unwrap();
        assert!(report.linkage_rate >= 0.0 && report.linkage_rate <= 1.0);
        assert_eq!(report.linkage_linked, 0);
        assert_eq!(report.linkage_unlinked, 0);
        assert!(report.capture_rate >= 0.0 && report.capture_rate <= 1.0);
        assert_eq!(report.capture_labeled, 0);
        assert_eq!(report.capture_total, 0);
        assert!(!report.headline_trend_uses_finding_id, "no data → false");
        assert!(report.external_overlap.per_agent.is_empty());
    }

    #[test]
    fn headline_trend_uses_finding_id_flips_on_at_85_percent_linkage() {
        // 17 linked + 3 unlinked = 85% — clears the threshold exactly.
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));

        let mut record = rec("alpha", "tty", 1);
        record.finding_ids = (0..17).map(|i| format!("FID-{i}")).collect();
        let rl = make_review_log(&dir, &[record]);

        for i in 0..17 {
            fb.record(&crate::feedback::FeedbackEntry {
                file_path: "x.rs".into(),
                finding_title: "t".into(),
                finding_category: "c".into(),
                verdict: crate::feedback::Verdict::Tp,
                reason: "r".into(),
                model: None,
                timestamp: chrono::Utc::now(),
                provenance: crate::feedback::Provenance::Human,
                fp_kind: None,
                finding_id: Some(format!("FID-{i}")),
                rule_id: None,
            })
            .unwrap();
        }
        for _ in 0..3 {
            fb.record(&crate::feedback::FeedbackEntry {
                file_path: "y.rs".into(),
                finding_title: "t".into(),
                finding_category: "c".into(),
                verdict: crate::feedback::Verdict::Fp,
                reason: "r".into(),
                model: None,
                timestamp: chrono::Utc::now(),
                provenance: crate::feedback::Provenance::Human,
                fp_kind: None,
                finding_id: None,
                rule_id: None,
            })
            .unwrap();
        }
        let report = compute_report(&fb, &tl, &rl).unwrap();
        assert!((report.linkage_rate - 0.85).abs() < 1e-9);
        assert!(
            report.headline_trend_uses_finding_id,
            "85% must flip the gate"
        );
    }

    #[test]
    fn compute_report_empty_stores() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let rl = crate::review_log::ReviewLog::new(dir.path().join("reviews.jsonl"));
        let report = compute_report(&fb, &tl, &rl).unwrap();
        assert_eq!(report.feedback_count, 0);
        assert_eq!(report.reviews_7d, 0);
        assert!((report.precision - 0.0).abs() < f64::EPSILON);
        assert!(report.top_repos.is_empty());
        assert!(report.top_callers.is_empty());
        assert!(report.rolling_windows.is_empty());
    }

    #[test]
    fn compute_report_populates_top_repos_by_volume() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let mut records = Vec::new();
        for _ in 0..5 {
            records.push(rec("alpha", "tty", 2));
        }
        for _ in 0..3 {
            records.push(rec("beta", "tty", 1));
        }
        for _ in 0..1 {
            records.push(rec("gamma", "tty", 1));
        }
        for _ in 0..2 {
            records.push(rec("delta", "tty", 1));
        }
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        assert_eq!(report.top_repos.len(), 3, "should cap at 3");
        assert_eq!(report.top_repos[0].key, "alpha");
        assert_eq!(report.top_repos[1].key, "beta");
        assert_eq!(report.top_repos[2].key, "delta");
    }

    #[test]
    fn compute_report_populates_top_callers_by_volume() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let mut records = Vec::new();
        for _ in 0..4 {
            records.push(rec("r", "claude_code", 2));
        }
        for _ in 0..2 {
            records.push(rec("r", "tty", 1));
        }
        for _ in 0..1 {
            records.push(rec("r", "codex_ci", 1));
        }
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        assert_eq!(report.top_callers[0].key, "claude_code");
        assert_eq!(report.top_callers[1].key, "tty");
    }

    #[test]
    fn compute_report_populates_rolling_windows() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let records: Vec<_> = (0..120).map(|_| rec("r", "tty", 1)).collect();
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        assert!(
            !report.rolling_windows.is_empty(),
            "should produce at least one rolling-50 window"
        );
        assert!(report.rolling_windows.len() <= 4);
    }

    #[test]
    fn format_human_default_includes_by_repo_when_data_present() {
        // After Task 12, default omits By caller / Rolling — only By repo
        // is shown by default.
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let records: Vec<_> = (0..6).map(|_| rec("alpha", "claude_code", 2)).collect();
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        let out = format_human(&report, &Style::plain());
        assert!(out.contains("By repo"), "missing repo section: {}", out);
        assert!(out.contains("alpha"));
    }

    #[test]
    fn format_human_minimal_omits_highlights() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let records: Vec<_> = (0..6).map(|_| rec("alpha", "claude_code", 2)).collect();
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        let out = format_human_minimal(&report, &Style::plain());
        assert!(
            !out.contains("By repo"),
            "minimal output should omit repo block: {}",
            out
        );
        assert!(!out.contains("By caller"));
        assert!(out.contains("Feedback Health"), "minimal keeps core blocks");
    }

    #[test]
    fn format_human_omits_highlights_when_no_review_data() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let rl = crate::review_log::ReviewLog::new(dir.path().join("reviews.jsonl"));
        let report = compute_report(&fb, &tl, &rl).unwrap();
        let out = format_human(&report, &Style::plain());
        assert!(!out.contains("By repo"));
        assert!(!out.contains("By caller"));
    }

    #[test]
    fn format_json_exposes_new_highlight_fields() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let records: Vec<_> = (0..6).map(|_| rec("alpha", "claude_code", 2)).collect();
        let rl = make_review_log(&dir, &records);
        let report = compute_report(&fb, &tl, &rl).unwrap();
        let json = format_json(&report).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["top_repos"].is_array());
        assert!(v["top_callers"].is_array());
        assert!(v["rolling_windows"].is_array());
        assert!(v["linkage_rate"].is_f64());
        assert!(v["capture_rate"].is_f64());
        assert!(v["headline_trend_uses_finding_id"].is_boolean());
        assert!(v["external_overlap"]["agents"].is_array());
        assert!(v["precision_trend_per_finding"].is_array());
    }

    #[test]
    fn format_compact_contains_key_metrics() {
        let report = StatsReport {
            feedback_count: 100,
            precision: 0.75,
            tp: 60,
            fp: 20,
            partial: 10,
            wontfix: 10,
            precision_trend: vec![],
            reviews_7d: 5,
            findings_per_review: 3.2,
            suppression_rate: 0.1,
            tokens_in_7d: 0,
            tokens_out_7d: 0,
            cost_7d: 0.0,
            tokens_per_finding: 0.0,
            model: String::new(),
            top_repos: Vec::new(),
            top_callers: Vec::new(),
            rolling_windows: Vec::new(),
            tier_summary: analytics::TierSummary::default(),
            linkage_rate: 0.0,
            linkage_linked: 0,
            linkage_unlinked: 0,
            capture_rate: 0.0,
            capture_labeled: 0,
            capture_total: 0,
            headline_trend_uses_finding_id: false,
            external_overlap: analytics::ExternalOverlap::default(),
            precision_trend_per_finding: Vec::new(),
        };
        let out = format_compact(&report);
        assert!(out.contains("feedback:100"));
        assert!(out.contains("precision:75%"));
        assert!(out.contains("tp:60"));
        assert!(out.contains("fp:20"));
        assert!(out.contains("reviews_7d:5"));
        assert!(out.contains("linkage:0%"));
        assert!(out.contains("capture:0%"));
    }

    #[test]
    fn format_compact_includes_external_overlap_when_present() {
        let report = StatsReport {
            feedback_count: 100,
            precision: 0.75,
            tp: 60,
            fp: 20,
            partial: 10,
            wontfix: 10,
            precision_trend: vec![],
            reviews_7d: 5,
            findings_per_review: 3.2,
            suppression_rate: 0.1,
            tokens_in_7d: 0,
            tokens_out_7d: 0,
            cost_7d: 0.0,
            tokens_per_finding: 0.0,
            model: String::new(),
            top_repos: Vec::new(),
            top_callers: Vec::new(),
            rolling_windows: Vec::new(),
            tier_summary: analytics::TierSummary::default(),
            linkage_rate: 0.92,
            linkage_linked: 46,
            linkage_unlinked: 4,
            capture_rate: 0.6,
            capture_labeled: 3,
            capture_total: 5,
            headline_trend_uses_finding_id: true,
            external_overlap: analytics::ExternalOverlap {
                per_agent: vec![analytics::AgentOverlap {
                    agent: "pal".into(),
                    findings: 10,
                    overlap: 8,
                    agree: 6,
                }],
            },
            precision_trend_per_finding: Vec::new(),
        };
        let out = format_compact(&report);
        assert!(out.contains("linkage:92%"));
        assert!(out.contains("capture:60%"));
        assert!(out.contains("external:pal:6/8"));
    }

    #[test]
    fn format_human_contains_sections() {
        let report = StatsReport {
            feedback_count: 2000,
            precision: 0.74,
            tp: 1200,
            fp: 400,
            partial: 200,
            wontfix: 200,
            precision_trend: vec![],
            reviews_7d: 10,
            findings_per_review: 4.5,
            suppression_rate: 0.15,
            tokens_in_7d: 50000,
            tokens_out_7d: 20000,
            cost_7d: 1.5,
            tokens_per_finding: 1500.0,
            model: "gpt-5.4".into(),
            top_repos: Vec::new(),
            top_callers: Vec::new(),
            rolling_windows: Vec::new(),
            tier_summary: analytics::TierSummary::default(),
            linkage_rate: 0.0,
            linkage_linked: 0,
            linkage_unlinked: 0,
            capture_rate: 0.0,
            capture_labeled: 0,
            capture_total: 0,
            headline_trend_uses_finding_id: false,
            external_overlap: analytics::ExternalOverlap::default(),
            precision_trend_per_finding: Vec::new(),
        };
        let out = format_human(&report, &Style::plain());
        assert!(out.contains("Feedback Health"));
        assert!(out.contains("Activity (last 7 days)"));
        assert!(out.contains("Spend (last 7 days)"));
        assert!(out.contains("2.0k")); // feedback count
        assert!(out.contains("74%")); // precision
    }

    fn slice(key: &str, n: u32, findings: u32, files: u64, low_sample: bool) -> DimensionSlice {
        DimensionSlice {
            key: key.into(),
            n_reviews: n,
            n_findings: findings,
            findings_per_file: if files == 0 {
                0.0
            } else {
                findings as f64 / files as f64
            },
            findings_per_kloc: None,
            accept_rate: None,
            severity_mix: Default::default(),
            suppression_rate: 0.0,
            avg_duration_ms: 0,
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            cache_hit_rate: 0.0,
            sparkline_points: vec![],
            low_sample,
        }
    }

    #[test]
    fn dimension_table_has_header_and_keys() {
        let slices = vec![
            slice("alpha", 10, 20, 10, false),
            slice("beta", 3, 6, 3, true),
        ];
        let out = format_dimension_table("by-repo", &slices, &Style::plain(), true);
        assert!(out.contains("Repo"), "by-repo header should use 'Repo'");
        assert!(out.contains("Reviews"));
        assert!(out.contains("Findings/file"));
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
    }

    #[test]
    fn dimension_table_header_matches_mode() {
        let s = vec![slice("claude_code", 10, 5, 10, false)];
        let repo = format_dimension_table("by-repo", &s, &Style::plain(), true);
        let caller = format_dimension_table("by-caller", &s, &Style::plain(), true);
        let rolling = format_dimension_table("rolling", &s, &Style::plain(), true);
        assert!(repo.contains("Repo") && !repo.contains("Caller"));
        assert!(caller.contains("Caller"));
        assert!(rolling.contains("Window"));
    }

    #[test]
    fn dimension_table_marks_low_sample_rows() {
        let slices = vec![slice("tiny", 2, 1, 2, true)];
        let out = format_dimension_table("by-repo", &slices, &Style::plain(), true);
        assert!(out.contains("low sample"), "should tag low-sample rows");
    }

    #[test]
    fn dimension_table_uses_bar_glyph_for_accept_rate() {
        let mut s = slice("r", 10, 5, 10, false);
        s.accept_rate = Some(0.78);
        let out = format_dimension_table("by-repo", &[s], &Style::plain(), true);
        // A 78% bar should have some filled and some empty cells.
        assert!(
            out.contains('█'),
            "unicode bar should contain full-block char, got:\n{}",
            out
        );
    }

    #[test]
    fn dimension_table_ascii_fallback_has_no_unicode_blocks() {
        let mut s = slice("r", 10, 5, 10, false);
        s.accept_rate = Some(0.78);
        let out = format_dimension_table("by-repo", &[s], &Style::plain(), false);
        for c in out.chars() {
            let cp = c as u32;
            assert!(
                !(0x2581..=0x2588).contains(&cp) && cp != 0x00b7,
                "ASCII fallback leaked unicode char {:?}",
                c,
            );
        }
    }

    #[test]
    fn dimension_table_empty_slices_does_not_panic() {
        let out = format_dimension_table("by-repo", &[], &Style::plain(), true);
        assert!(out.contains("no data") || out.is_empty());
    }

    #[test]
    fn truncate_key_handles_zero_width_without_panic() {
        assert_eq!(truncate_key("anything", 0), "");
    }

    #[test]
    fn truncate_key_single_width_does_not_underflow() {
        // width=1 would become take(width-1)=take(0), and we'd still need to return
        // something non-empty or the table layout breaks. Contract: fit exactly in `width`.
        let out = truncate_key("long-name", 1);
        assert_eq!(out.chars().count(), 1);
    }

    #[test]
    fn dimension_compact_single_line_no_glyphs() {
        let slices = vec![
            slice("alpha", 10, 23, 10, false),
            slice("beta", 3, 6, 3, true),
        ];
        let out = format_dimension_compact("by-repo", &slices);
        assert!(
            !out.contains('\n') || out.trim_end().lines().count() == 1,
            "compact mode must be single-line, got: {:?}",
            out
        );
        assert!(!out.contains('█'), "compact mode must not use semigraphics");
        assert!(out.contains("alpha"));
    }

    #[test]
    fn format_json_valid() {
        let report = StatsReport {
            feedback_count: 50,
            precision: 0.8,
            tp: 32,
            fp: 8,
            partial: 5,
            wontfix: 5,
            precision_trend: vec![],
            reviews_7d: 3,
            findings_per_review: 2.0,
            suppression_rate: 0.0,
            tokens_in_7d: 0,
            tokens_out_7d: 0,
            cost_7d: 0.0,
            tokens_per_finding: 0.0,
            model: String::new(),
            top_repos: Vec::new(),
            top_callers: Vec::new(),
            rolling_windows: Vec::new(),
            tier_summary: analytics::TierSummary::default(),
            linkage_rate: 0.0,
            linkage_linked: 0,
            linkage_unlinked: 0,
            capture_rate: 0.0,
            capture_labeled: 0,
            capture_total: 0,
            headline_trend_uses_finding_id: false,
            external_overlap: analytics::ExternalOverlap::default(),
            precision_trend_per_finding: Vec::new(),
        };
        let json = format_json(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["feedback_count"], 50);
        assert_eq!(parsed["tp"], 32);
    }
}
