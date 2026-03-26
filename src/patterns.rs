/// Canonical pattern vocabulary for normalizing findings.
/// Maps diverse finding titles/descriptions to standard pattern names.

/// Known patterns with keywords that identify them.
const PATTERN_RULES: &[(&str, &[&str])] = &[
    ("sql_injection", &["sql injection", "sql query", "execute(", "cursor.execute"]),
    ("xss", &["innerhtml", "outerhtml", "document.write", "dangerouslysetinnerhtml", "cross-site scripting"]),
    ("eval_exec", &["eval(", "exec(", "code injection"]),
    ("hardcoded_secret", &["hardcoded secret", "hardcoded key", "hardcoded password", "secret_key"]),
    ("debug_mode", &["debug=true", "debug mode"]),
    ("open_binding", &["0.0.0.0", "all interfaces", "all network"]),
    ("bare_except", &["bare except", "broad except"]),
    ("blocking_in_async", &["block_in_place", "future.result()", "blocking call", "stall.*reactor", "stall.*event loop"]),
    ("resource_leak", &["resource leak", "tab.*leak", "connection.*leak", "file.*leak", "not closed"]),
    ("race_condition", &["race condition", "not synchronized", "not thread-safe", "thread safety"]),
    ("missing_timeout", &["no timeout", "missing timeout", "hang indefinitely"]),
    ("exception_disclosure", &["exception detail", "str(e)", "internal error.*client", "error.*expose"]),
    ("mutate_while_iterate", &["mutating.*while.*iterating", "mutating.*while.*iterate"]),
    ("mutable_default", &["mutable default"]),
    ("complexity_high", &["cyclomatic complexity", "complexity"]),
    ("unsafe_code", &["unsafe block", "unsafe code"]),
    ("unwrap_panic", &["unwrap()", "may panic"]),
    ("ssrf", &["ssrf", "server-side request", "unvalidated.*url", "arbitrary.*url"]),
    ("path_traversal", &["path traversal", "directory traversal", "../"]),
    ("weak_crypto", &["md5", "sha1", "weak.*hash", "weak.*crypto"]),
    ("unused_code", &["unused import", "unused variable", "dead code"]),
    ("non_atomic_write", &["non-atomic", "atomic write"]),
];

/// Classify a finding into a canonical pattern based on title + description + category.
pub fn classify_pattern(title: &str, description: &str, category: &str) -> Option<String> {
    let combined = format!("{} {} {}", title, description, category).to_lowercase();
    for (pattern, keywords) in PATTERN_RULES {
        if keywords.iter().any(|kw| combined.contains(kw)) {
            return Some(pattern.to_string());
        }
    }
    None
}

/// Format a finding for embedding: "{pattern} {category} {title}"
/// This normalized format improves embedding similarity for semantically similar findings.
pub fn embedding_text(finding_title: &str, category: &str, canonical_pattern: Option<&str>) -> String {
    match canonical_pattern {
        Some(pattern) => format!("{} {} {}", pattern, category, finding_title),
        None => format!("{} {}", category, finding_title),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_sql_injection_variants() {
        assert_eq!(classify_pattern("SQL injection via f-string", "", "security"), Some("sql_injection".into()));
        assert_eq!(classify_pattern("Unvalidated input in SQL query", "", "security"), Some("sql_injection".into()));
        assert_eq!(classify_pattern("Potential SQL injection via f-string in execute()", "", "security"), Some("sql_injection".into()));
    }

    #[test]
    fn classify_xss_variants() {
        assert_eq!(classify_pattern("innerHTML assignment is XSS risk", "", "security"), Some("xss".into()));
        assert_eq!(classify_pattern("document.write allows injection", "", "security"), Some("xss".into()));
    }

    #[test]
    fn classify_unknown_returns_none() {
        assert_eq!(classify_pattern("Some random finding", "nothing specific", "misc"), None);
    }

    #[test]
    fn embedding_text_with_pattern() {
        let text = embedding_text("SQL injection via f-string", "security", Some("sql_injection"));
        assert!(text.starts_with("sql_injection"));
        assert!(text.contains("security"));
    }

    #[test]
    fn embedding_text_without_pattern() {
        let text = embedding_text("Random finding", "misc", None);
        assert!(text.starts_with("misc"));
        assert!(!text.contains("sql_injection"));
    }

    #[test]
    fn classify_blocking_in_async() {
        assert_eq!(classify_pattern("Blocking future.result() in async", "", "concurrency"), Some("blocking_in_async".into()));
        assert_eq!(classify_pattern("block_in_place can panic", "", "bug"), Some("blocking_in_async".into()));
    }

    #[test]
    fn classify_complexity() {
        assert_eq!(classify_pattern("Function foo has cyclomatic complexity 12", "", "complexity"), Some("complexity_high".into()));
    }
}
