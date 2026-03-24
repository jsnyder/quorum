use std::io::IsTerminal;

use crate::finding::{Finding, Severity};

/// Terminal style detection -- resolved once at startup.
pub struct Style {
    pub dim: &'static str,
    pub bold: &'static str,
    pub green: &'static str,
    pub red: &'static str,
    pub yellow: &'static str,
    pub reset: &'static str,
}

impl Style {
    pub fn detect(no_color_flag: bool) -> Self {
        if should_disable_color(no_color_flag) {
            Self::plain()
        } else {
            Self::ansi()
        }
    }

    pub fn ansi() -> Self {
        Self {
            dim: "\x1b[2m",
            bold: "\x1b[1m",
            green: "\x1b[32m",
            red: "\x1b[31m",
            yellow: "\x1b[33m",
            reset: "\x1b[0m",
        }
    }

    pub fn plain() -> Self {
        Self {
            dim: "",
            bold: "",
            green: "",
            red: "",
            yellow: "",
            reset: "",
        }
    }
}

fn should_disable_color(no_color_flag: bool) -> bool {
    no_color_flag
        || !std::io::stdout().is_terminal()
        || std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty())
        || std::env::var("TERM").is_ok_and(|v| v == "dumb")
}

pub fn severity_icon(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical | Severity::High => "!",
        Severity::Medium => "~",
        Severity::Low => "-",
        Severity::Info => "-",
    }
}

pub fn format_finding(f: &Finding, style: &Style) -> String {
    let icon = severity_icon(&f.severity);
    let (icon_color, icon_reset) = match f.severity {
        Severity::Critical | Severity::High => (style.red, style.reset),
        Severity::Medium => (style.yellow, style.reset),
        _ => (style.dim, style.reset),
    };

    let line_label = if f.line_start == f.line_end {
        format!("L{}", f.line_start)
    } else {
        format!("L{}-{}", f.line_start, f.line_end)
    };

    format!(
        "  {icon_color}{icon}{icon_reset} {bold}{title}{reset}  [{dim}{category}{reset}] {line}\n    {desc}\n",
        icon_color = icon_color,
        icon = icon,
        icon_reset = icon_reset,
        bold = style.bold,
        title = f.title,
        reset = style.reset,
        dim = style.dim,
        category = f.category,
        line = line_label,
        desc = f.description,
    )
}

pub fn format_review(file_path: &str, findings: &[Finding], style: &Style) -> String {
    let mut out = format!(
        "{bold}~ Review: {file}{reset}\n\n",
        bold = style.bold,
        file = file_path,
        reset = style.reset,
    );

    if findings.is_empty() {
        out.push_str(&format!(
            "  {green}= No findings.{reset}\n",
            green = style.green,
            reset = style.reset,
        ));
        return out;
    }

    for f in findings {
        out.push_str(&format_finding(f, style));
        out.push('\n');
    }

    // Summary line
    let critical = findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        .count();
    let warning = findings
        .iter()
        .filter(|f| f.severity == Severity::Medium)
        .count();
    let info = findings.len() - critical - warning;

    out.push_str(&format!(
        "  {dim}{count} finding{s} ({critical} critical, {warning} warning, {info} info){reset}\n",
        dim = style.dim,
        count = findings.len(),
        s = if findings.len() == 1 { "" } else { "s" },
        critical = critical,
        warning = warning,
        info = info,
        reset = style.reset,
    ));

    out
}

pub fn format_json(findings: &[Finding]) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(findings)?)
}

pub fn compute_exit_code(findings: &[Finding]) -> i32 {
    if findings.iter().any(|f| matches!(f.severity, Severity::Critical | Severity::High)) {
        2
    } else if findings.iter().any(|f| f.severity == Severity::Medium) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{FindingBuilder, Source};

    // -- severity_icon --

    #[test]
    fn severity_icon_critical() {
        assert_eq!(severity_icon(&Severity::Critical), "!");
    }

    #[test]
    fn severity_icon_medium() {
        assert_eq!(severity_icon(&Severity::Medium), "~");
    }

    #[test]
    fn severity_icon_info() {
        assert_eq!(severity_icon(&Severity::Info), "-");
    }

    // -- format_finding (plain, no ANSI) --

    #[test]
    fn format_finding_plain_contains_title_and_category() {
        let f = FindingBuilder::new()
            .title("SQL injection")
            .description("User input flows to db")
            .category("security")
            .severity(Severity::Critical)
            .lines(42, 42)
            .build();
        let out = format_finding(&f, &Style::plain());
        assert!(out.contains("SQL injection"));
        assert!(out.contains("[security]"));
        assert!(out.contains("L42"));
        assert!(out.contains("User input flows to db"));
    }

    #[test]
    fn format_finding_multiline_shows_range() {
        let f = FindingBuilder::new()
            .title("Complex function")
            .lines(10, 25)
            .build();
        let out = format_finding(&f, &Style::plain());
        assert!(out.contains("L10-25"));
    }

    #[test]
    fn format_finding_plain_no_ansi() {
        let f = FindingBuilder::new().title("Test").build();
        let out = format_finding(&f, &Style::plain());
        assert!(!out.contains("\x1b["));
    }

    // -- format_review --

    #[test]
    fn format_review_clean() {
        let out = format_review("src/main.rs", &[], &Style::plain());
        assert!(out.contains("~ Review: src/main.rs"));
        assert!(out.contains("No findings."));
    }

    #[test]
    fn format_review_with_findings() {
        let findings = vec![
            FindingBuilder::new()
                .title("Bug")
                .severity(Severity::Critical)
                .build(),
        ];
        let out = format_review("src/main.rs", &findings, &Style::plain());
        assert!(out.contains("Bug"));
        assert!(out.contains("1 finding"));
    }

    #[test]
    fn format_review_summary_counts() {
        let findings = vec![
            FindingBuilder::new().severity(Severity::Critical).build(),
            FindingBuilder::new().severity(Severity::Medium).build(),
            FindingBuilder::new().severity(Severity::Info).build(),
        ];
        let out = format_review("test.rs", &findings, &Style::plain());
        assert!(out.contains("3 findings"));
        assert!(out.contains("1 critical"));
        assert!(out.contains("1 warning"));
        assert!(out.contains("1 info"));
    }

    // -- format_json --

    #[test]
    fn format_json_valid() {
        let findings = vec![FindingBuilder::new().title("Bug").build()];
        let json = format_json(&findings).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["title"], "Bug");
    }

    #[test]
    fn format_json_no_ansi_codes() {
        let findings = vec![FindingBuilder::new().build()];
        let json = format_json(&findings).unwrap();
        assert!(!json.contains("\x1b["));
    }

    #[test]
    fn format_json_empty() {
        let json = format_json(&[]).unwrap();
        assert_eq!(json, "[]");
    }

    // -- compute_exit_code --

    #[test]
    fn exit_code_clean() {
        assert_eq!(compute_exit_code(&[]), 0);
    }

    #[test]
    fn exit_code_info_only() {
        let findings = vec![FindingBuilder::new().severity(Severity::Info).build()];
        assert_eq!(compute_exit_code(&findings), 0);
    }

    #[test]
    fn exit_code_warning() {
        let findings = vec![FindingBuilder::new().severity(Severity::Medium).build()];
        assert_eq!(compute_exit_code(&findings), 1);
    }

    #[test]
    fn exit_code_critical() {
        let findings = vec![FindingBuilder::new().severity(Severity::Critical).build()];
        assert_eq!(compute_exit_code(&findings), 2);
    }

    #[test]
    fn exit_code_mixed_takes_worst() {
        let findings = vec![
            FindingBuilder::new().severity(Severity::Info).build(),
            FindingBuilder::new().severity(Severity::Critical).build(),
        ];
        assert_eq!(compute_exit_code(&findings), 2);
    }
}
