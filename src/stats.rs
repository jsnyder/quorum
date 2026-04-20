/// Stats dashboard -- reads local data files and computes metrics.

use crate::analytics;
use crate::dimensions::DimensionSlice;
use crate::feedback::FeedbackStore;
use crate::formatting;
use crate::glyphs;
use crate::telemetry::TelemetryStore;
use crate::output::Style;

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
}

pub fn compute_report(
    feedback_store: &FeedbackStore,
    telemetry_store: &TelemetryStore,
) -> anyhow::Result<StatsReport> {
    let feedback = feedback_store.load_all().unwrap_or_default();
    let feedback_count = feedback.len();

    // Aggregate feedback stats
    let stats = analytics::compute_stats(&feedback);
    let total_tp: usize = stats.values().map(|s| s.tp).sum();
    let total_fp: usize = stats.values().map(|s| s.fp).sum();
    let total_partial: usize = stats.values().map(|s| s.partial).sum();
    let total_wontfix: usize = stats.values().map(|s| s.wontfix).sum();
    let relevant = total_tp + total_partial;
    let precision_denom = relevant + total_fp;
    let precision = if precision_denom > 0 { relevant as f64 / precision_denom as f64 } else { 0.0 };

    // Precision trend (7-day windows)
    let precision_trend = analytics::precision_trend(&feedback, 7);

    // Telemetry: last 7 days
    let since_7d = chrono::Utc::now() - chrono::Duration::days(7);
    let recent = telemetry_store.load_since(since_7d).unwrap_or_default();
    let reviews_7d = recent.len();
    let tokens_in_7d: u64 = recent.iter().map(|e| e.tokens_in).sum();
    let tokens_out_7d: u64 = recent.iter().map(|e| e.tokens_out).sum();

    let total_findings_7d: usize = recent.iter()
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
    })
}

pub fn format_human(report: &StatsReport, style: &Style) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "{bold}Feedback Health{reset}\n",
        bold = style.bold, reset = style.reset
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

    if !report.precision_trend.is_empty() {
        let trend_str: Vec<String> = report.precision_trend.iter()
            .map(|w| formatting::format_pct(w.precision))
            .collect();
        out.push_str(&format!("  Trend: {}\n", trend_str.join(">")));
    }

    out.push_str(&format!(
        "\n{bold}Activity (7d){reset}\n",
        bold = style.bold, reset = style.reset
    ));
    out.push_str(&format!(
        "  Reviews: {}  Findings/review: {:.1}  Suppression: {}\n",
        report.reviews_7d, report.findings_per_review,
        formatting::format_pct(report.suppression_rate),
    ));

    if report.tokens_in_7d > 0 || report.tokens_out_7d > 0 {
        out.push_str(&format!(
            "\n{bold}Spend (7d){reset}\n",
            bold = style.bold, reset = style.reset
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
                format!("{}{bar}{reset} {}", color_for_accept(r, style), pct,
                    bar = bar, reset = style.reset)
            }
            _ => format!("{dim}—                    {reset}", dim = style.dim, reset = style.reset),
        };

        let trend_cell = if s.sparkline_points.is_empty() {
            format!("{dim}—{reset}", dim = style.dim, reset = style.reset)
        } else {
            let spark = glyphs::sparkline(&s.sparkline_points, unicode);
            let arrow = glyphs::trend_arrow(&s.sparkline_points, unicode);
            format!("{} {}", spark, arrow)
        };

        let low_tag = if s.low_sample {
            format!("  {dim}(low sample){reset}", dim = style.dim, reset = style.reset)
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
        if slices.len() == 1 { unit_label_singular(mode) } else { unit_label_plural(mode) },
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
    if rate >= 0.70 { style.green }
    else if rate < 0.40 { style.red }
    else { "" }
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
        let acc = s.accept_rate
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
        let trend: Vec<String> = report.precision_trend.iter()
            .map(|w| formatting::format_pct(w.precision))
            .collect();
        parts.push(format!("trend:{}", trend.join(">")));
    }

    parts.push(format!("reviews_7d:{}", report.reviews_7d));
    parts.push(format!("findings_per_review:{:.1}", report.findings_per_review));

    if report.tokens_in_7d > 0 {
        parts.push(format!("tokens_in:{}", formatting::format_count(report.tokens_in_7d)));
        parts.push(format!("tokens_out:{}", formatting::format_count(report.tokens_out_7d)));
        parts.push(format!("cost:{}", formatting::format_cost(report.cost_7d)));
    }

    format!("{}\n", parts.join(" "))
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
    });
    Ok(serde_json::to_string_pretty(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::path::PathBuf;

    #[test]
    fn compute_report_empty_stores() {
        let dir = TempDir::new().unwrap();
        let fb = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let tl = TelemetryStore::new(dir.path().join("tl.jsonl"));
        let report = compute_report(&fb, &tl).unwrap();
        assert_eq!(report.feedback_count, 0);
        assert_eq!(report.reviews_7d, 0);
        assert!((report.precision - 0.0).abs() < f64::EPSILON);
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
        };
        let out = format_compact(&report);
        assert!(out.contains("feedback:100"));
        assert!(out.contains("precision:75%"));
        assert!(out.contains("tp:60"));
        assert!(out.contains("fp:20"));
        assert!(out.contains("reviews_7d:5"));
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
        };
        let out = format_human(&report, &Style::plain());
        assert!(out.contains("Feedback Health"));
        assert!(out.contains("Activity (7d)"));
        assert!(out.contains("Spend (7d)"));
        assert!(out.contains("2.0k"));  // feedback count
        assert!(out.contains("74%"));   // precision
    }

    fn slice(key: &str, n: u32, findings: u32, files: u64, low_sample: bool) -> DimensionSlice {
        DimensionSlice {
            key: key.into(),
            n_reviews: n,
            n_findings: findings,
            findings_per_file: if files == 0 { 0.0 } else { findings as f64 / files as f64 },
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
        assert!(out.contains('█'), "unicode bar should contain full-block char, got:\n{}", out);
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
        let slices = vec![slice("alpha", 10, 23, 10, false), slice("beta", 3, 6, 3, true)];
        let out = format_dimension_compact("by-repo", &slices);
        assert!(!out.contains('\n') || out.trim_end().lines().count() == 1,
            "compact mode must be single-line, got: {:?}", out);
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
        };
        let json = format_json(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["feedback_count"], 50);
        assert_eq!(parsed["tp"], 32);
    }
}
