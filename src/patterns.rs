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

/// Format a finding for embedding with additional discriminator tokens appended.
///
/// Discriminators are free-text fragments (user reason, LLM finding description,
/// evidence snippets, function signatures, framework names) that carry
/// token-level information the base title+category lacks. The goal is to push
/// unrelated-but-lexically-similar findings apart in embedding space — e.g.
/// "Missing input validation" (api args) vs "Missing input validation" (jwt
/// signature) — which at bge-small-en's 384 dims cluster too tightly without
/// additional signal.
///
/// Empty/whitespace-only fragments are filtered so absent fields do not perturb
/// the embedding vs the base representation.
pub fn embedding_text_enriched(
    finding_title: &str,
    category: &str,
    canonical_pattern: Option<&str>,
    discriminators: &[&str],
) -> String {
    let base = embedding_text(finding_title, category, canonical_pattern);
    let extras: Vec<&str> = discriminators.iter()
        .copied()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if extras.is_empty() {
        base
    } else {
        format!("{} {}", base, extras.join(" "))
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
    fn embedding_text_with_discriminators_includes_them() {
        // Enrichment: free-text discriminators (user reason, LLM description, evidence)
        // should flow into the embedded string so paraphrased titles can still be
        // disambiguated by their concrete tokens (jwt.verify, cursor.execute, etc.).
        // External review (Gemini 3 Pro + GPT-5.2) flagged that the base
        // "{pattern} {category} {title}" representation is too abstract — unrelated
        // findings conflate on generic programming vocabulary.
        let text = embedding_text_enriched(
            "Missing input validation",
            "security",
            None,
            &["jwt.verify", "HS256", "algorithm"],
        );
        assert!(text.contains("jwt.verify"), "discriminator tokens must be in embed text");
        assert!(text.contains("HS256"));
        assert!(text.contains("algorithm"));
        assert!(text.contains("security"));
        assert!(text.contains("Missing input validation"));
    }

    #[test]
    fn embedding_text_enriched_with_empty_discriminators_matches_base() {
        let base = embedding_text("X", "y", Some("p"));
        let enriched = embedding_text_enriched("X", "y", Some("p"), &[]);
        assert_eq!(base, enriched);
    }

    #[test]
    fn embedding_text_enriched_filters_empty_strings() {
        // Free-text fields are frequently empty/whitespace; must not inject extra spaces.
        let with_empties = embedding_text_enriched("T", "c", None, &["", "  ", "real"]);
        let without = embedding_text_enriched("T", "c", None, &["real"]);
        assert_eq!(with_empties, without);
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
