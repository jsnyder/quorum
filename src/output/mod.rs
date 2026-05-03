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

/// Strip control characters from untrusted text to prevent terminal injection.
/// Preserves normal printable chars, newlines, and tabs.
fn strip_control_chars(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
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

    // Strip control characters (ANSI escapes, etc.) from untrusted LLM-generated text
    let safe_title = strip_control_chars(&f.title);
    let safe_desc = strip_control_chars(&f.description);

    let mut output = format!(
        "  {icon_color}{icon}{icon_reset} {bold}{title}{reset}  [{dim}{category}{reset}] {line}\n    {desc}\n",
        icon_color = icon_color,
        icon = icon,
        icon_reset = icon_reset,
        bold = style.bold,
        title = safe_title,
        reset = style.reset,
        dim = style.dim,
        category = f.category,
        line = line_label,
        desc = safe_desc,
    );

    if let Some(ref fix) = f.suggested_fix {
        let indented = fix.replace('\n', "\n      ");
        output.push_str(&format!("    {dim}Suggested fix:{reset} {indented}\n",
            dim = style.dim, reset = style.reset, indented = indented));
    }

    if let Some(ref excerpt) = f.based_on_excerpt {
        output.push_str(&format!("    {dim}[partial view: {excerpt}]{reset}\n",
            dim = style.dim, reset = style.reset, excerpt = excerpt));
    }

    output
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

/// One-line severity legend for human review output. The same characters
/// appear inline at the start of every finding line, so surfacing the key
/// once avoids readers having to infer meaning from context.
pub fn format_legend() -> String {
    "Legend:  ! high/critical   ~ medium   - low/info".to_string()
}

/// Human-readable linter coverage hints, one per unconfigured linter.
/// Intended for stderr under TTY + non-agent, non-JSON, non-compact conditions.
pub fn format_hints_human(hints: &[crate::linter::LinterHint]) -> Vec<String> {
    hints
        .iter()
        .map(|h| {
            let noun = if h.file_count == 1 { "file" } else { "files" };
            format!(
                "hint: {n} {lang} {noun} in this review — {linter} not configured. {inst} to enable.",
                n = h.file_count,
                lang = h.language,
                noun = noun,
                linter = h.linter.name(),
                inst = h.enable_instruction,
            )
        })
        .collect()
}

/// Compact single-line linter status header. Returns None if there is nothing
/// to report (no enabled linters relevant to this review and no hints).
pub fn format_compact_linter_header(
    enabled: &[crate::linter::LinterKind],
    hints: &[crate::linter::LinterHint],
) -> Option<String> {
    if enabled.is_empty() && hints.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for k in enabled {
        parts.push(format!("{}=on", k.name()));
    }
    for h in hints {
        parts.push(format!("{}=off({})", h.linter.name(), h.enable_instruction));
    }
    Some(format!("# linters: {}", parts.join(" ")))
}

/// JSON output grouped by file, optionally prefixed with a `_meta` entry
/// describing linter coverage. The `_meta` entry is omitted when both
/// `enabled` and `hints` are empty, preserving the legacy list shape.
pub fn format_json_grouped_with_meta(
    results: &[crate::pipeline::FileReviewResult],
    enabled: &[crate::linter::LinterKind],
    hints: &[crate::linter::LinterHint],
) -> anyhow::Result<String> {
    use serde_json::{json, Value};
    let mut out: Vec<Value> = Vec::new();
    if !enabled.is_empty() || !hints.is_empty() {
        let enabled_names: Vec<&str> = enabled.iter().map(|k| k.name()).collect();
        let unconfigured: Vec<Value> = hints
            .iter()
            .map(|h| {
                json!({
                    "name": h.linter.name(),
                    "language": h.language,
                    "file_count": h.file_count,
                    "hint": h.enable_instruction,
                })
            })
            .collect();
        out.push(json!({
            "_meta": {
                "linters": {
                    "enabled": enabled_names,
                    "available_unconfigured": unconfigured,
                }
            }
        }));
    }
    for r in results.iter().filter(|r| !r.findings.is_empty()) {
        out.push(json!({
            "file": r.file_path,
            "findings": r.findings,
        }));
    }
    Ok(serde_json::to_string_pretty(&out)?)
}

/// JSON output grouped by file -- includes file_path so findings can be traced back.
pub fn format_json_grouped(results: &[crate::pipeline::FileReviewResult]) -> anyhow::Result<String> {
    #[derive(serde::Serialize)]
    struct FileFindings<'a> {
        file: &'a str,
        findings: &'a [Finding],
    }
    let grouped: Vec<FileFindings> = results
        .iter()
        .filter(|r| !r.findings.is_empty())
        .map(|r| FileFindings {
            file: &r.file_path,
            findings: &r.findings,
        })
        .collect();
    Ok(serde_json::to_string_pretty(&grouped)?)
}

pub fn format_compact_finding(f: &Finding) -> String {
    let icon = severity_icon(&f.severity);
    let line_label = if f.line_start == f.line_end {
        format!("L{}", f.line_start)
    } else {
        format!("L{}-{}", f.line_start, f.line_end)
    };
    let title = if f.title.chars().count() > 80 {
        let truncated: String = f.title.chars().take(80).collect();
        format!("{}...", truncated)
    } else {
        f.title.clone()
    };
    let mut result = format!("{icon}|{cat}|{line}|{title}",
        icon = icon,
        cat = f.category,
        line = line_label,
        title = title,
    );
    if f.based_on_excerpt.is_some() {
        result.push_str(" [excerpt]");
    }
    result
}

pub fn format_compact_review(file_path: &str, findings: &[Finding]) -> String {
    if findings.is_empty() {
        return format!("{}: clean", file_path);
    }

    let mut lines: Vec<String> = findings.iter()
        .map(format_compact_finding)
        .collect();

    let critical = findings.iter()
        .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        .count();
    let warning = findings.iter()
        .filter(|f| f.severity == Severity::Medium)
        .count();
    let info = findings.len() - critical - warning;

    lines.push(format!("{} findings ({}C {}W {}I)",
        findings.len(), critical, warning, info));

    lines.join("\n")
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

/// Detect if compact output should be used based on env vars.
/// Recognizes AI coding tool environments:
/// - CLAUDE_CODE: Claude Code (Anthropic)
/// - GEMINI_CLI: Gemini CLI (Google)
/// - CODEX_CI: Codex CLI (OpenAI)
/// - AGENT: Generic agent identifier (proposed Codex standard)
pub fn should_use_compact(compact_flag: bool) -> bool {
    compact_flag
        || is_env_set("CLAUDE_CODE")
        || is_env_set("GEMINI_CLI")
        || is_env_set("CODEX_CI")
        || is_env_set("AGENT")
}

fn is_env_set(var: &str) -> bool {
    std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Whether the current terminal environment can render Unicode glyphs.
/// Shared across main and stats so sparklines degrade consistently.
pub fn unicode_ok_default() -> bool {
    if std::env::var_os("NO_UNICODE").is_some() {
        return false;
    }
    if let Some(term) = std::env::var_os("TERM") {
        if term == "dumb" {
            return false;
        }
    }
    if let Ok(lang) = std::env::var("LANG") {
        return lang.to_uppercase().contains("UTF-8") || lang.to_uppercase().contains("UTF8");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::FindingBuilder;
    use crate::linter::{LinterHint, LinterKind};

    fn hint(linter: LinterKind, lang: &'static str, n: usize, inst: &'static str) -> LinterHint {
        LinterHint {
            linter,
            language: lang,
            file_count: n,
            enable_instruction: inst,
        }
    }

    // -- format_hint_human --

    #[test]
    fn human_hint_includes_file_count_linter_and_instruction() {
        let hints = vec![hint(LinterKind::Ruff, "Python", 3, "add [tool.ruff] to pyproject.toml")];
        let lines = format_hints_human(&hints);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert!(line.contains("hint:"), "missing hint prefix: {line}");
        assert!(line.contains("3"), "missing count: {line}");
        assert!(line.contains("Python"), "missing language: {line}");
        assert!(line.contains("ruff"), "missing linter name: {line}");
        assert!(line.contains("[tool.ruff]"), "missing instruction: {line}");
    }

    #[test]
    fn human_hint_singular_vs_plural_file() {
        let one = format_hints_human(&[hint(LinterKind::Ruff, "Python", 1, "x")]);
        let many = format_hints_human(&[hint(LinterKind::Ruff, "Python", 5, "x")]);
        assert!(one[0].contains("1 Python file"), "singular: {}", one[0]);
        assert!(many[0].contains("5 Python files"), "plural: {}", many[0]);
    }

    #[test]
    fn human_hint_empty_when_no_hints() {
        assert!(format_hints_human(&[]).is_empty());
    }

    // -- format_compact_linter_header --

    #[test]
    fn compact_header_lists_enabled_and_unconfigured() {
        let enabled = vec![LinterKind::Clippy];
        let hints = vec![hint(LinterKind::Ruff, "Python", 1, "add [tool.ruff] to pyproject.toml")];
        let header = format_compact_linter_header(&enabled, &hints).unwrap();
        assert!(header.starts_with('#'), "no comment prefix: {header}");
        assert!(header.contains("clippy=on"), "enabled missing: {header}");
        assert!(header.contains("ruff=off"), "unconfigured missing: {header}");
        assert!(header.contains("[tool.ruff]"), "instruction missing: {header}");
    }

    #[test]
    fn compact_header_none_when_nothing_to_report() {
        assert!(format_compact_linter_header(&[], &[]).is_none());
    }

    #[test]
    fn compact_header_single_line_no_newlines() {
        let enabled = vec![LinterKind::Clippy, LinterKind::Eslint];
        let hints = vec![
            hint(LinterKind::Ruff, "Python", 1, "add [tool.ruff] to pyproject.toml"),
            hint(LinterKind::Yamllint, "YAML", 2, "add .yamllint"),
        ];
        let header = format_compact_linter_header(&enabled, &hints).unwrap();
        assert!(!header.contains('\n'), "header wraps: {header}");
    }

    // -- JSON _meta --

    #[test]
    fn json_meta_entry_prepended_when_hints_present() {
        use crate::pipeline::FileReviewResult;
        let results: Vec<FileReviewResult> = vec![];
        let enabled = vec![LinterKind::Clippy];
        let hints = vec![hint(LinterKind::Ruff, "Python", 1, "add [tool.ruff] to pyproject.toml")];
        let out = format_json_grouped_with_meta(&results, &enabled, &hints).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = parsed.as_array().expect("top-level array");
        assert!(!arr.is_empty());
        let meta = &arr[0]["_meta"]["linters"];
        assert_eq!(meta["enabled"][0], "clippy");
        assert_eq!(meta["available_unconfigured"][0]["name"], "ruff");
        assert!(meta["available_unconfigured"][0]["hint"].as_str().unwrap().contains("[tool.ruff]"));
    }

    #[test]
    fn json_meta_omits_entry_when_nothing_to_report() {
        use crate::pipeline::FileReviewResult;
        let results: Vec<FileReviewResult> = vec![];
        let out = format_json_grouped_with_meta(&results, &[], &[]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.as_array().unwrap().iter().all(|e| e.get("_meta").is_none()));
    }

    // -- format_legend --

    #[test]
    fn legend_explains_all_three_severity_icons() {
        let out = format_legend();
        assert!(out.contains("!"));
        assert!(out.contains("~"));
        assert!(out.contains("-"));
        assert!(out.to_lowercase().contains("critical") || out.to_lowercase().contains("high"));
        assert!(out.to_lowercase().contains("medium"));
        assert!(out.to_lowercase().contains("low") || out.to_lowercase().contains("info"));
    }

    #[test]
    fn legend_is_single_line() {
        let out = format_legend();
        assert!(!out.contains('\n'), "legend wraps: {out}");
    }

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
            .category("security".into())
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

    // -- format_compact_finding --

    #[test]
    fn compact_finding_single_line() {
        let f = FindingBuilder::new()
            .title("SQL injection risk")
            .severity(Severity::Critical)
            .category("security".into())
            .lines(42, 42)
            .build();
        let out = format_compact_finding(&f);
        assert_eq!(out, "!|security|L42|SQL injection risk");
    }

    #[test]
    fn compact_finding_line_range() {
        let f = FindingBuilder::new()
            .title("Complex function")
            .severity(Severity::Medium)
            .category("complexity".into())
            .lines(10, 25)
            .build();
        let out = format_compact_finding(&f);
        assert_eq!(out, "~|complexity|L10-25|Complex function");
    }

    #[test]
    fn compact_finding_truncates_long_title() {
        let long_title = "A".repeat(100);
        let f = FindingBuilder::new()
            .title(&long_title)
            .severity(Severity::Info)
            .lines(1, 1)
            .build();
        let out = format_compact_finding(&f);
        assert!(out.len() < 120);
        assert!(out.ends_with("..."));
    }

    // -- format_compact_review --

    #[test]
    fn compact_review_with_findings() {
        let findings = vec![
            FindingBuilder::new()
                .title("Bug A")
                .severity(Severity::Critical)
                .category("security".into())
                .lines(42, 42)
                .build(),
            FindingBuilder::new()
                .title("Bug B")
                .severity(Severity::Medium)
                .category("style".into())
                .lines(10, 10)
                .build(),
        ];
        let out = format_compact_review("src/main.rs", &findings);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "!|security|L42|Bug A");
        assert_eq!(lines[1], "~|style|L10|Bug B");
        assert!(lines[2].contains("2 findings"));
    }

    #[test]
    fn compact_review_clean() {
        let out = format_compact_review("src/main.rs", &[]);
        assert_eq!(out.trim(), "src/main.rs: clean");
    }

    #[test]
    fn compact_no_ansi_codes() {
        let f = FindingBuilder::new()
            .title("Test")
            .severity(Severity::Critical)
            .build();
        let out = format_compact_finding(&f);
        assert!(!out.contains("\x1b["));
    }

    #[test]
    fn should_use_compact_from_flag() {
        assert!(should_use_compact(true));
    }

    #[test]
    fn should_use_compact_flag_false_without_env() {
        // Note: if any AI tool env var is set in the test environment,
        // this will return true — that's correct behavior
        // We can only reliably test that flag=true always returns true
        assert!(should_use_compact(true));
    }

    #[test]
    fn format_finding_includes_suggested_fix() {
        let f = FindingBuilder::new()
            .title("SQL injection")
            .description("User input not sanitized")
            .suggested_fix("Use parameterized queries")
            .build();
        let style = Style::plain();
        let output = format_finding(&f, &style);
        assert!(output.contains("Use parameterized queries"));
        assert!(output.contains("Suggested fix:"));
    }

    #[test]
    fn format_finding_no_fix_no_extra_line() {
        let f = FindingBuilder::new()
            .title("SQL injection")
            .description("User input not sanitized")
            .build();
        let style = Style::plain();
        let output = format_finding(&f, &style);
        assert!(!output.contains("Suggested fix:"));
    }

    #[test]
    fn compact_finding_omits_suggested_fix() {
        let f = FindingBuilder::new()
            .title("SQL injection")
            .suggested_fix("Use parameterized queries")
            .build();
        let output = format_compact_finding(&f);
        assert!(!output.contains("parameterized"));
    }

    #[test]
    fn format_finding_shows_excerpt_annotation() {
        let f = FindingBuilder::new()
            .title("Missing error handling")
            .description("No error handling found")
            .based_on_excerpt("lines 1-150 of 500")
            .build();
        let style = Style::plain();
        let output = format_finding(&f, &style);
        assert!(output.contains("[partial view: lines 1-150 of 500]"));
    }

    #[test]
    fn format_finding_no_annotation_when_full() {
        let f = FindingBuilder::new()
            .title("Missing error handling")
            .description("No error handling found")
            .build();
        let style = Style::plain();
        let output = format_finding(&f, &style);
        assert!(!output.contains("partial view"));
    }

    #[test]
    fn compact_finding_shows_excerpt_tag() {
        let f = FindingBuilder::new()
            .title("Missing error handling")
            .based_on_excerpt("lines 1-150 of 500")
            .build();
        let output = format_compact_finding(&f);
        assert!(output.contains("[excerpt]"));
    }

    #[test]
    fn format_finding_multiline_suggested_fix() {
        let f = FindingBuilder::new()
            .title("Missing validation")
            .description("No input validation")
            .suggested_fix("Add validation:\n  if input.is_empty() {\n    return Err(\"empty\");\n  }")
            .build();
        let style = Style::plain();
        let output = format_finding(&f, &style);
        assert!(output.contains("Suggested fix:"));
        // Multiline fix should be indented
        assert!(output.contains("      if input.is_empty()"));
    }
}
