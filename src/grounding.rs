use crate::finding::{Finding, GroundingStatus, Severity, Source};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static BACKTICK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").unwrap());

static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // Rust
        "self", "Self", "super", "crate", "true", "false", "None", "Some",
        "unwrap", "expect", "clone", "iter", "into", "from", "default",
        "push", "is_empty", "map", "filter", "collect", "Result", "Option",
        "String", "Vec", "Box", "Arc", "Mutex",
        // Python
        "True", "False", "print", "list", "dict", "str", "int", "float",
        "bool", "type", "init",
        // TypeScript/JS
        "this", "null", "undefined", "console", "log",
        "length", "toString", "Promise",
    ]
    .into_iter()
    .collect()
});

const MIN_IDENTIFIER_LEN: usize = 4;

/// Extract backtick-delimited identifiers from text, filtering stopwords
/// and short tokens.
pub fn extract_identifiers(text: &str) -> Vec<&str> {
    BACKTICK_RE
        .captures_iter(text)
        .filter_map(|cap| {
            let mut id = cap.get(1).unwrap().as_str().trim();
            if id.ends_with("()") {
                id = &id[..id.len() - 2];
            }
            if id.len() >= MIN_IDENTIFIER_LEN && !STOPWORDS.contains(id) {
                Some(id)
            } else {
                None
            }
        })
        .collect()
}

/// Extract identifiers from finding title first; fall back to description
/// if the title yields nothing.
pub fn extract_identifiers_from_finding_text<'a>(title: &'a str, description: &'a str) -> Vec<&'a str> {
    let mut ids = extract_identifiers(title);
    if ids.is_empty() {
        ids = extract_identifiers(description);
    }
    ids
}

/// Result of grounding verification for a single finding.
#[derive(Debug)]
pub struct GroundingResult {
    pub status: GroundingStatus,
    pub severity_change: Option<Severity>,
}

/// Step severity down one level. Returns `None` for `Info` (cannot demote further).
fn demote_severity(s: &Severity) -> Option<Severity> {
    match s {
        Severity::Critical => Some(Severity::High),
        Severity::High => Some(Severity::Medium),
        Severity::Medium => Some(Severity::Low),
        Severity::Low => Some(Severity::Info),
        Severity::Info => None,
    }
}

fn is_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Check if `id` appears in `text` with word boundaries only where the
/// identifier itself starts/ends with a word character. This handles
/// punctuated symbols like `foo()`, `std::io`, and `obj.method` that
/// would fail with `\b...\b` regex anchors.
fn contains_symbol(text: &str, id: &str) -> bool {
    let needs_leading_boundary = id.starts_with(is_word_char);
    let needs_trailing_boundary = id.ends_with(is_word_char);
    text.match_indices(id).any(|(idx, _)| {
        let before_ok = !needs_leading_boundary
            || text[..idx].chars().next_back().is_none_or(|c| !is_word_char(c));
        let after_ok = !needs_trailing_boundary
            || text[idx + id.len()..].chars().next().is_none_or(|c| !is_word_char(c));
        before_ok && after_ok
    })
}

/// Verify that backtick-wrapped identifiers in an LLM finding's title actually
/// exist in the source code at (or near) the cited line range.
///
/// Only LLM-sourced findings are checked. Non-LLM sources and findings with
/// no extractable identifiers return `NotChecked`. A +/- 2 line window around
/// the cited range accommodates off-by-one LLM citations.
pub fn verify_grounding(finding: &Finding, source: &str) -> GroundingResult {
    let source_lines: Vec<&str> = source.lines().collect();
    verify_grounding_with_lines(finding, &source_lines)
}

fn verify_grounding_with_lines(finding: &Finding, source_lines: &[&str]) -> GroundingResult {
    if !matches!(finding.source, Source::Llm(_)) {
        return GroundingResult {
            status: GroundingStatus::NotChecked,
            severity_change: None,
        };
    }

    let line_count = source_lines.len() as u32;

    // Check line range validity (1-indexed, ordered).
    if finding.line_start == 0
        || finding.line_end == 0
        || finding.line_start > finding.line_end
        || finding.line_start > line_count
        || finding.line_end > line_count
    {
        return GroundingResult {
            status: GroundingStatus::LineOutOfRange,
            severity_change: demote_severity(&finding.severity),
        };
    }

    // Extract identifiers from title (fallback to description).
    let identifiers = extract_identifiers_from_finding_text(&finding.title, &finding.description);
    if identifiers.is_empty() {
        return GroundingResult {
            status: GroundingStatus::NotChecked,
            severity_change: None,
        };
    }

    // Build the +/- 2 line window (1-indexed to 0-indexed).
    let start = (finding.line_start as usize).saturating_sub(3); // -1 for 0-index, -2 for window
    let end = ((finding.line_end as usize) + 2).min(source_lines.len());
    let window: String = source_lines[start..end].join("\n");

    // Check all identifiers exist with context-aware boundary matching.
    let all_found = identifiers.iter().all(|id| contains_symbol(&window, id));

    if all_found {
        GroundingResult {
            status: GroundingStatus::Verified,
            severity_change: None,
        }
    } else {
        GroundingResult {
            status: GroundingStatus::SymbolNotFound,
            severity_change: demote_severity(&finding.severity),
        }
    }
}

/// Aggregated counts of grounding outcomes for telemetry.
#[derive(Debug, Default)]
pub struct GroundingCounters {
    pub verified: u32,
    pub symbol_not_found: u32,
    pub line_out_of_range: u32,
    pub not_checked: u32,
}

/// Count grounding outcomes across a batch of findings for telemetry reporting.
pub fn count_grounding_outcomes(findings: &[Finding]) -> GroundingCounters {
    let mut c = GroundingCounters::default();
    for f in findings {
        match &f.grounding_status {
            Some(GroundingStatus::Verified) => c.verified += 1,
            Some(GroundingStatus::SymbolNotFound) => c.symbol_not_found += 1,
            Some(GroundingStatus::LineOutOfRange) => c.line_out_of_range += 1,
            Some(GroundingStatus::NotChecked) | None => c.not_checked += 1,
        }
    }
    c
}

/// Apply grounding verification to a batch of findings, mutating severity
/// in place for ungrounded LLM findings. When `disabled` is true, returns
/// findings unchanged.
pub fn apply_grounding(mut findings: Vec<Finding>, source: &str, disabled: bool) -> Vec<Finding> {
    if disabled {
        return findings;
    }
    let source_lines: Vec<&str> = source.lines().collect();
    for finding in &mut findings {
        if !matches!(finding.source, Source::Llm(_)) {
            continue;
        }
        let result = verify_grounding_with_lines(finding, &source_lines);
        finding.grounding_status = Some(result.status);
        if let Some(new_severity) = result.severity_change {
            finding.severity = new_severity;
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_backtick_identifiers_from_title() {
        let ids = extract_identifiers("Function `parse_unified_diff` panics on single-line hunks");
        assert_eq!(ids, vec!["parse_unified_diff"]);
    }

    #[test]
    fn extracts_multiple_identifiers() {
        let ids = extract_identifiers("`foo_bar` and `bar_baz` are both wrong");
        assert_eq!(ids, vec!["foo_bar", "bar_baz"]);
    }

    #[test]
    fn returns_empty_for_no_backticks() {
        let ids = extract_identifiers("Missing null check on return value");
        assert!(ids.is_empty());
    }

    #[test]
    fn filters_short_identifiers() {
        let ids = extract_identifiers("`fn` and `Ok` and `parse_diff` are mentioned");
        assert_eq!(ids, vec!["parse_diff"]);
    }

    #[test]
    fn filters_language_stopwords() {
        let ids = extract_identifiers("`self` calls `unwrap` on `parse_config`");
        assert_eq!(ids, vec!["parse_config"]);
    }

    #[test]
    fn empty_backtick_content_ignored() {
        let ids = extract_identifiers("some `` empty backticks");
        assert!(ids.is_empty());
    }

    #[test]
    fn backtick_with_whitespace_only() {
        let ids = extract_identifiers("some `   ` whitespace");
        assert!(ids.is_empty());
    }

    #[test]
    fn stoplist_entry_exact_match_only() {
        // "unwrap_or" should NOT be filtered even though "unwrap" is on the stoplist
        let ids = extract_identifiers("`unwrap_or` should pass");
        assert_eq!(ids, vec!["unwrap_or"]);
    }

    #[test]
    fn extracts_from_description_too() {
        let ids = extract_identifiers_from_finding_text(
            "Missing error handling",
            "The function `process_data` at line 42 swallows the error",
        );
        assert_eq!(ids, vec!["process_data"]);
    }

    #[test]
    fn multibyte_utf8_identifier() {
        let ids = extract_identifiers("`some_func` and `\u{65e5}\u{672c}\u{8a9e}\u{30c6}\u{30b9}\u{30c8}` both present");
        assert_eq!(ids.len(), 2);
    }

    // --- verify_grounding / apply_grounding tests (Task 3) ---

    use crate::finding::{FindingBuilder, GroundingStatus, Severity, Source};

    fn sample_source() -> &'static str {
        "use std::io;\n\
         \n\
         fn parse_unified_diff(input: &str) -> Vec<String> {\n\
             let lines = input.lines();\n\
             let mut hunks = Vec::new();\n\
             for line in lines {\n\
                 hunks.push(parse_hunk(line));\n\
             }\n\
             hunks\n\
         }\n"
    }

    #[test]
    fn grounding_verified_when_symbol_found_at_cited_lines() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` panics on single-line hunks")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::Verified);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_symbol_not_found_when_identifier_absent() {
        let f = FindingBuilder::new()
            .title("Function `nonexistent_func` has a bug")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::SymbolNotFound);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }

    #[test]
    fn grounding_line_out_of_range() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` panics")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(50, 60)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::LineOutOfRange);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }

    #[test]
    fn grounding_not_checked_for_non_llm_source() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` issue")
            .source(Source::LocalAst)
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::NotChecked);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_not_checked_when_no_identifiers_in_title() {
        let f = FindingBuilder::new()
            .title("Missing null check on return value")
            .description("Some generic description")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::NotChecked);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_demotion_steps_down_one_level() {
        for (input, expected) in [
            (Severity::Critical, Severity::High),
            (Severity::High, Severity::Medium),
            (Severity::Medium, Severity::Low),
            (Severity::Low, Severity::Info),
        ] {
            let f = FindingBuilder::new()
                .title("Function `nonexistent_func` has a bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(3, 9)
                .severity(input.clone())
                .build();
            let result = verify_grounding(&f, sample_source());
            assert_eq!(result.severity_change, Some(expected), "failed for {:?}", input);
        }
    }

    #[test]
    fn grounding_info_cannot_demote_further() {
        let f = FindingBuilder::new()
            .title("Function `nonexistent_func` has a bug")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::Info)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::SymbolNotFound);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_utf8_multibyte_does_not_panic() {
        let source = "fn process() {\n    let emoji = \"🎉\";\n    let cjk = \"中文\";\n}\n";
        let f = FindingBuilder::new()
            .title("Function `process` has an issue")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(1, 3)
            .severity(Severity::Medium)
            .build();
        let result = verify_grounding(&f, source);
        assert_eq!(result.status, GroundingStatus::Verified);
    }

    #[test]
    fn grounding_window_at_file_start() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` issue")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(1, 2)
            .severity(Severity::High)
            .build();
        // parse_unified_diff is on line 3, which is within +2 window of line_end=2
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::Verified);
    }

    #[test]
    fn grounding_word_boundary_prevents_substring_match() {
        // "parse" (5 chars) passes the min-length filter and is extracted,
        // but \bparse\b must NOT match "parse_unified_diff" or "parse_hunk"
        // because _ is a word character — no word boundary between 'e' and '_'.
        let f = FindingBuilder::new()
            .title("Function `parse` is wrong")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::SymbolNotFound);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }

    #[test]
    fn grounding_symbol_with_trailing_parens_verified() {
        // LLMs often backtick function calls: `parse_unified_diff()`.
        // The trailing parens must NOT prevent matching the source.
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff()` panics")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::Verified);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_symbol_with_colons_verified() {
        // Rust paths like `std::io` should match when present in source.
        let source = "use std::io;\nfn main() {}\n";
        let f = FindingBuilder::new()
            .title("Unused import `std::io`")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(1, 1)
            .severity(Severity::Medium)
            .build();
        let result = verify_grounding(&f, source);
        assert_eq!(result.status, GroundingStatus::Verified);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_partial_identifiers_one_found_one_not() {
        let f = FindingBuilder::new()
            .title("`parse_unified_diff` and `hallucinated_func` both referenced")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::SymbolNotFound);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }

    #[test]
    fn grounding_linter_source_skipped() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` issue")
            .source(Source::Linter("clippy".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::NotChecked);
        assert_eq!(result.severity_change, None);
    }

    // --- apply_grounding tests ---

    #[test]
    fn apply_grounding_pass_sets_status_and_demotes() {
        let source = "fn parse_unified_diff() {}\nfn other() {}\n";
        let findings = vec![
            FindingBuilder::new()
                .title("Function `parse_unified_diff` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
            FindingBuilder::new()
                .title("Function `nonexistent` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
            FindingBuilder::new()
                .title("AST finding")
                .source(Source::LocalAst)
                .lines(1, 1)
                .severity(Severity::Medium)
                .build(),
        ];
        let result = apply_grounding(findings, source, false);
        assert_eq!(result[0].grounding_status, Some(GroundingStatus::Verified));
        assert_eq!(result[0].severity, Severity::High);
        assert_eq!(result[1].grounding_status, Some(GroundingStatus::SymbolNotFound));
        assert_eq!(result[1].severity, Severity::Medium); // demoted
        assert!(result[2].grounding_status.is_none()); // LocalAst untouched
        assert_eq!(result[2].severity, Severity::Medium);
    }

    #[test]
    fn grounding_counters_correct() {
        let source = "fn parse_unified_diff() {}\nfn other() {}\n";
        let findings = vec![
            // Verified
            FindingBuilder::new()
                .title("Function `parse_unified_diff` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
            // SymbolNotFound
            FindingBuilder::new()
                .title("Function `nonexistent` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
            // NotChecked (LocalAst)
            FindingBuilder::new()
                .title("AST finding")
                .source(Source::LocalAst)
                .lines(1, 1)
                .severity(Severity::Medium)
                .build(),
            // LineOutOfRange
            FindingBuilder::new()
                .title("Function `parse_unified_diff` issue")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(50, 60)
                .severity(Severity::High)
                .build(),
        ];
        let result = apply_grounding(findings, source, false);
        let counters = count_grounding_outcomes(&result);
        assert_eq!(counters.verified, 1);
        assert_eq!(counters.symbol_not_found, 1);
        assert_eq!(counters.line_out_of_range, 1);
        assert_eq!(counters.not_checked, 1); // the LocalAst finding has no grounding_status
    }

    #[test]
    fn apply_grounding_disabled_returns_unchanged() {
        let source = "fn foo() {}\n";
        let findings = vec![
            FindingBuilder::new()
                .title("Function `nonexistent` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
        ];
        let result = apply_grounding(findings, source, true);
        assert!(result[0].grounding_status.is_none());
        assert_eq!(result[0].severity, Severity::High);
    }

    #[test]
    fn apply_grounding_env_var_true_also_disables() {
        let source = "fn foo() {}\n";
        let findings = vec![
            FindingBuilder::new()
                .title("Function `nonexistent` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
        ];
        // Test the parsing logic that will be used in the pipeline
        for val in ["1", "true", "TRUE", "True"] {
            // SAFETY: test-only; cargo test runs each test in its own thread
            // but we are not racing on this specific env var.
            unsafe { std::env::set_var("QUORUM_DISABLE_AST_GROUNDING", val) };
            let disabled = std::env::var("QUORUM_DISABLE_AST_GROUNDING")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            assert!(disabled, "Should be disabled for value: {val}");
            let result = apply_grounding(findings.clone(), source, disabled);
            assert!(result[0].grounding_status.is_none());
            assert_eq!(result[0].severity, Severity::High);
        }
        // SAFETY: test-only cleanup
        unsafe { std::env::remove_var("QUORUM_DISABLE_AST_GROUNDING") };
    }

    #[test]
    fn grounding_line_start_zero_treated_as_out_of_range() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` issue")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(0, 5)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::LineOutOfRange);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }

    #[test]
    fn grounding_inverted_range_treated_as_out_of_range() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` issue")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(10, 1)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::LineOutOfRange);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }
}
