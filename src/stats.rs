/// Stats dashboard -- reads local data files and computes metrics.

use crate::analytics;
use crate::feedback::FeedbackStore;
use crate::formatting;
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
