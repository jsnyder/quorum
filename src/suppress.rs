use serde::Deserialize;

use crate::finding::Finding;

#[derive(Debug, Clone, Deserialize)]
pub struct SuppressionRule {
    pub pattern: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SuppressConfig {
    #[serde(default)]
    suppress: Vec<SuppressionRule>,
}

pub fn parse_suppress_config(toml_str: &str) -> anyhow::Result<Vec<SuppressionRule>> {
    if toml_str.trim().is_empty() {
        return Ok(Vec::new());
    }
    let config: SuppressConfig = toml::from_str(toml_str)?;
    Ok(config.suppress)
}

/// Check if a suppression rule matches a finding for the given file path.
pub fn rule_matches(rule: &SuppressionRule, finding: &Finding, file_path: &str) -> bool {
    // Pattern: case-insensitive substring match on title
    let pattern_matches = finding
        .title
        .to_lowercase()
        .contains(&rule.pattern.to_lowercase());
    if !pattern_matches {
        return false;
    }

    // Category: exact match (case-insensitive) if specified
    if let Some(ref cat) = rule.category {
        if finding.category.to_lowercase() != cat.to_lowercase() {
            return false;
        }
    }

    // File: glob match if specified (normalize path separators for cross-platform)
    if let Some(ref file_glob) = rule.file {
        let normalized_path = file_path.replace('\\', "/");
        let normalized_glob = file_glob.replace('\\', "/");
        let match_opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        let pattern = glob::Pattern::new(&normalized_glob);
        match pattern {
            Ok(p) => {
                if !p.matches_with(&normalized_path, match_opts) {
                    return false;
                }
            }
            Err(_) => {
                // Invalid glob -- treat as exact string match
                if normalized_path != normalized_glob {
                    return false;
                }
            }
        }
    }

    true
}

/// Result of applying suppression rules to findings.
pub struct SuppressionResult {
    pub kept: Vec<Finding>,
    pub suppressed: Vec<(Finding, SuppressionRule)>,
}

/// Filter findings through suppression rules.
pub fn apply_suppressions(
    findings: Vec<Finding>,
    rules: &[SuppressionRule],
    file_path: &str,
) -> SuppressionResult {
    if rules.is_empty() {
        return SuppressionResult {
            kept: findings,
            suppressed: Vec::new(),
        };
    }
    let mut kept = Vec::new();
    let mut suppressed = Vec::new();
    for f in findings {
        if let Some(matched_rule) = rules.iter().find(|r| rule_matches(r, &f, file_path)) {
            suppressed.push((f, matched_rule.clone()));
        } else {
            kept.push(f);
        }
    }
    SuppressionResult { kept, suppressed }
}

/// Load suppression rules from a .quorum/suppress.toml file.
/// Returns empty vec if file doesn't exist or can't be parsed.
pub fn load_project_suppressions(path: &std::path::Path) -> Vec<SuppressionRule> {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse_suppress_config(&contents).unwrap_or_else(|e| {
            eprintln!("Warning: Failed to parse {}: {}", path.display(), e);
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

/// Format a suppressed finding for --show-suppressed output.
pub fn format_suppressed_finding(finding: &Finding, rule: &SuppressionRule) -> String {
    let reason = rule.reason.as_deref().unwrap_or("no reason given");
    format!(
        "  [SUPPRESSED] {}  [{}]
    Reason: {}
",
        finding.title, finding.category, reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::FindingBuilder;

    // --- Part A: Parsing tests ---

    #[test]
    fn parse_valid_suppress_config() {
        let toml = r#"
[[suppress]]
pattern = "TLS certificate"
category = "security"
file = "src/*.py"
reason = "Internal service, TLS not required"
"#;
        let rules = parse_suppress_config(toml).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "TLS certificate");
        assert_eq!(rules[0].category.as_deref(), Some("security"));
        assert_eq!(rules[0].file.as_deref(), Some("src/*.py"));
        assert_eq!(
            rules[0].reason.as_deref(),
            Some("Internal service, TLS not required")
        );
    }

    #[test]
    fn parse_empty_config_returns_empty_vec() {
        let rules = parse_suppress_config("").unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn parse_invalid_toml_returns_error() {
        let result = parse_suppress_config("this is not valid [[[ toml");
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_pattern_returns_error() {
        let toml = r#"
[[suppress]]
category = "security"
"#;
        let result = parse_suppress_config(toml);
        assert!(result.is_err());
    }

    #[test]
    fn parse_comments_only_returns_empty_vec() {
        let toml = r#"
# This is a comment
# Another comment
"#;
        let rules = parse_suppress_config(toml).unwrap();
        assert!(rules.is_empty());
    }

    // --- Part B: Matching tests ---

    #[test]
    fn rule_matches_by_pattern_substring() {
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: None,
            file: None,
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .build();
        assert!(rule_matches(&rule, &finding, "any.rs"));
    }

    #[test]
    fn rule_matches_case_insensitive() {
        let rule = SuppressionRule {
            pattern: "tls certificate".into(),
            category: None,
            file: None,
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS Certificate Verification Disabled")
            .build();
        assert!(rule_matches(&rule, &finding, "any.rs"));
    }

    #[test]
    fn rule_no_match_wrong_pattern() {
        let rule = SuppressionRule {
            pattern: "SQL injection".into(),
            category: None,
            file: None,
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .build();
        assert!(!rule_matches(&rule, &finding, "any.rs"));
    }

    #[test]
    fn rule_matches_with_category_filter() {
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: Some("security".into()),
            file: None,
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .category("security")
            .build();
        assert!(rule_matches(&rule, &finding, "any.rs"));
    }

    #[test]
    fn rule_no_match_wrong_category() {
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: Some("performance".into()),
            file: None,
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .category("security")
            .build();
        assert!(!rule_matches(&rule, &finding, "any.rs"));
    }

    #[test]
    fn rule_matches_with_file_glob() {
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: None,
            file: Some("src/*.py".into()),
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .build();
        assert!(rule_matches(&rule, &finding, "src/url_resolver.py"));
        assert!(!rule_matches(&rule, &finding, "src/main.rs"));
    }

    #[test]
    fn rule_matches_file_exact_path() {
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: None,
            file: Some("src/url_resolver.py".into()),
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .build();
        assert!(rule_matches(&rule, &finding, "src/url_resolver.py"));
        assert!(!rule_matches(&rule, &finding, "src/other.py"));
    }

    #[test]
    fn rule_all_fields_must_match_and_logic() {
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: Some("security".into()),
            file: Some("src/*.py".into()),
            reason: Some("Known safe".into()),
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .category("security")
            .build();
        // All match
        assert!(rule_matches(&rule, &finding, "src/url_resolver.py"));
        // Wrong file
        assert!(!rule_matches(&rule, &finding, "lib/url_resolver.py"));
        // Wrong category
        let finding_perf = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .category("performance")
            .build();
        assert!(!rule_matches(&rule, &finding_perf, "src/url_resolver.py"));
    }

    #[test]
    fn rule_matches_recursive_glob() {
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: None,
            file: Some("src/**/*.py".into()),
            reason: None,
        };
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .build();
        assert!(rule_matches(&rule, &finding, "src/sub/deep/file.py"));
        assert!(!rule_matches(&rule, &finding, "lib/file.py"));
    }

    // --- Part C: apply_suppressions tests ---

    #[test]
    fn apply_suppressions_filters_matching_findings() {
        let rules = vec![SuppressionRule {
            pattern: "TLS certificate".into(),
            category: Some("security".into()),
            file: None,
            reason: Some("Internal service".into()),
        }];
        let findings = vec![
            FindingBuilder::new()
                .title("TLS certificate verification disabled")
                .category("security")
                .build(),
            FindingBuilder::new()
                .title("SQL injection risk")
                .category("security")
                .build(),
        ];
        let result = apply_suppressions(findings, &rules, "src/main.py");
        assert_eq!(result.kept.len(), 1);
        assert_eq!(result.kept[0].title, "SQL injection risk");
        assert_eq!(result.suppressed.len(), 1);
        assert_eq!(result.suppressed[0].0.title, "TLS certificate verification disabled");
    }

    #[test]
    fn apply_suppressions_empty_rules_passes_all() {
        let findings = vec![
            FindingBuilder::new().title("Finding A").build(),
            FindingBuilder::new().title("Finding B").build(),
        ];
        let result = apply_suppressions(findings, &[], "src/main.py");
        assert_eq!(result.kept.len(), 2);
        assert!(result.suppressed.is_empty());
    }

    #[test]
    fn apply_suppressions_multiple_rules() {
        let rules = vec![
            SuppressionRule {
                pattern: "TLS certificate".into(),
                category: None,
                file: None,
                reason: Some("Internal service".into()),
            },
            SuppressionRule {
                pattern: "SQL injection".into(),
                category: None,
                file: None,
                reason: Some("Parameterized queries used".into()),
            },
        ];
        let findings = vec![
            FindingBuilder::new()
                .title("TLS certificate verification disabled")
                .build(),
            FindingBuilder::new()
                .title("SQL injection risk in query builder")
                .build(),
            FindingBuilder::new()
                .title("Unused import")
                .build(),
        ];
        let result = apply_suppressions(findings, &rules, "src/main.py");
        assert_eq!(result.kept.len(), 1);
        assert_eq!(result.kept[0].title, "Unused import");
        assert_eq!(result.suppressed.len(), 2);
    }

    #[test]
    fn apply_suppressions_returns_matched_rule() {
        let rules = vec![SuppressionRule {
            pattern: "TLS certificate".into(),
            category: None,
            file: None,
            reason: Some("Internal service".into()),
        }];
        let findings = vec![FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .build()];
        let result = apply_suppressions(findings, &rules, "src/main.py");
        assert_eq!(result.suppressed.len(), 1);
        let (_, matched_rule) = &result.suppressed[0];
        assert_eq!(matched_rule.pattern, "TLS certificate");
        assert_eq!(matched_rule.reason.as_deref(), Some("Internal service"));
    }

    // --- Part D: File loading tests ---

    #[test]
    fn load_suppressions_returns_empty_for_missing_file() {
        let path = std::path::Path::new("/tmp/nonexistent_quorum_suppress.toml");
        let rules = load_project_suppressions(path);
        assert!(rules.is_empty());
    }

    // --- Part E: format_suppressed_finding tests ---

    #[test]
    fn format_suppressed_finding_shows_rule_reason() {
        let finding = FindingBuilder::new()
            .title("TLS certificate verification disabled")
            .category("security")
            .build();
        let rule = SuppressionRule {
            pattern: "TLS certificate".into(),
            category: Some("security".into()),
            file: None,
            reason: Some("Internal service, TLS not required".into()),
        };
        let output = format_suppressed_finding(&finding, &rule);
        assert!(output.contains("SUPPRESSED"));
        assert!(output.contains("TLS certificate verification disabled"));
        assert!(output.contains("Internal service, TLS not required"));
    }

    #[test]
    fn format_suppressed_finding_no_reason() {
        let finding = FindingBuilder::new()
            .title("Some finding")
            .category("test")
            .build();
        let rule = SuppressionRule {
            pattern: "Some finding".into(),
            category: None,
            file: None,
            reason: None,
        };
        let output = format_suppressed_finding(&finding, &rule);
        assert!(output.contains("SUPPRESSED"));
        assert!(output.contains("no reason given"));
    }
}
