//! PR1a Phase 1 stub. Phase 2 GREEN replaces with full mapping table + tests.
//!
//! Strict 10-variant Category enum that replaces the free-text
//! `Finding.category: String`. The `From<String>` shim folds the 40+
//! observed legacy strings (see tests/fixtures/feedback_categories_observed.txt)
//! into a target variant — `bug`, `code_quality`, `code-quality` etc all
//! map to `Maintainability` per plan target #2.
//!
//! Phase 1 stubs are minimal so RED tests fail at runtime, not compile-time:
//!   - `all()` returns empty Vec (the 10-variant assertion fails RED)
//!   - `From<String>` returns Security default (the mapping-table tests fail RED)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Security,
    Correctness,
    Logic,
    Concurrency,
    Reliability,
    Robustness,
    ErrorHandling,
    Validation,
    Performance,
    Maintainability,
}

impl Category {
    pub fn all() -> Vec<Category> {
        vec![
            Category::Security,
            Category::Correctness,
            Category::Logic,
            Category::Concurrency,
            Category::Reliability,
            Category::Robustness,
            Category::ErrorHandling,
            Category::Validation,
            Category::Performance,
            Category::Maintainability,
        ]
    }

    /// Kebab-case string for this variant (matches serde rename).
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Security => "security",
            Category::Correctness => "correctness",
            Category::Logic => "logic",
            Category::Concurrency => "concurrency",
            Category::Reliability => "reliability",
            Category::Robustness => "robustness",
            Category::ErrorHandling => "error-handling",
            Category::Validation => "validation",
            Category::Performance => "performance",
            Category::Maintainability => "maintainability",
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for Category {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// Category strings that carry no information: ingestion-path defaults
/// written when the recorder stated nothing. `From` folds them to
/// `Maintainability` like any unrecognized string, which makes "nobody said"
/// indistinguishable from a real Maintainability verdict (#499).
///
/// ponytail: hand-maintained two-item list, deliberately not a heuristic.
/// Add to it only when a new ingestion path invents another placeholder.
const PLACEHOLDER_CATEGORIES: &[&str] = &["manual", "unknown"];

impl Category {
    /// The category the recorder actually stated, if any.
    ///
    /// Returns `None` for a blank category and for the ingestion-path
    /// placeholders. Any caller *deciding* something on a category --
    /// precedent matching, FP-rate maps -- must use this rather than `From`,
    /// so an unstated category stays unmatched instead of voting for
    /// `Maintainability`.
    ///
    /// Genuine labels are unaffected: the 40-odd legacy spellings in
    /// `tests/fixtures/feedback_categories_observed.txt` (`style`, `quality`,
    /// `design`, `docs`, ...) still fold through `From` exactly as before.
    /// The distinction is placeholder vs. label, not recognized vs.
    /// unrecognized -- folding unknown *labels* into Maintainability is
    /// designed behavior, and narrowing it would silently drop real
    /// precedents.
    pub fn stated(s: &str) -> Option<Self> {
        let norm = s.to_lowercase().trim().replace([' ', '_'], "-");
        if norm.is_empty() || PLACEHOLDER_CATEGORIES.contains(&norm.as_str()) {
            return None;
        }
        Some(Category::from(norm))
    }
}

impl From<String> for Category {
    fn from(s: String) -> Self {
        // Lenient by design: unrecognized *labels* fold to Maintainability.
        // Callers that must not guess use `Category::stated` instead (#499).
        match s.to_lowercase().trim().replace([' ', '_'], "-").as_str() {
            "security" | "safety" => Category::Security,
            "correctness" | "functional-bug" | "bug" => Category::Correctness,
            "logic" | "logic-error" => Category::Logic,
            "concurrency" => Category::Concurrency,
            "reliability" | "resource-lifecycle" | "resource-management" => Category::Reliability,
            "robustness" | "compatibility" | "hardware" => Category::Robustness,
            "error-handling" => Category::ErrorHandling,
            "validation" | "schema-evolution" | "data-quality" => Category::Validation,
            "performance" | "complexity" => Category::Performance,
            _ => Category::Maintainability,
        }
    }
}

impl From<&str> for Category {
    fn from(s: &str) -> Self {
        Category::from(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #499: placeholders must read as "no category stated", not as a vote
    // for Maintainability.
    #[test]
    fn stated_rejects_placeholders_and_blanks() {
        assert_eq!(Category::stated("manual"), None);
        assert_eq!(Category::stated("unknown"), None);
        assert_eq!(Category::stated(""), None);
        assert_eq!(Category::stated("   "), None);
        assert_eq!(Category::stated("Manual"), None, "must normalize case");
        // `From` keeps its lenient contract for display paths.
        assert_eq!(Category::from("manual"), Category::Maintainability);
    }

    // The distinction is placeholder vs. label -- NOT recognized vs.
    // unrecognized. Narrowing to "recognized only" would silently drop the
    // ~13 legacy Maintainability spellings below, which is a precedent-
    // matching regression, not a fix. This test exists to fail if someone
    // re-tightens `stated`.
    #[test]
    fn stated_keeps_folding_unrecognized_labels_to_maintainability() {
        for label in [
            "style",
            "quality",
            "code-quality",
            "code_quality",
            "design",
            "docs",
            "documentation",
            "testing",
            "test-quality",
            "observability",
            "debuggability",
            "configuration",
            "best-practices",
            "api-design",
            "ast-pattern",
            "a-label-nobody-has-used-yet",
        ] {
            assert_eq!(
                Category::stated(label),
                Some(Category::Maintainability),
                "{label} must still fold to Maintainability"
            );
        }
    }

    #[test]
    fn stated_agrees_with_from_on_real_labels() {
        assert_eq!(Category::stated("security"), Some(Category::Security));
        assert_eq!(Category::stated("safety"), Some(Category::Security));
        assert_eq!(
            Category::stated("Error_Handling"),
            Some(Category::ErrorHandling)
        );
        assert_eq!(Category::stated("logic error"), Some(Category::Logic));
        assert_eq!(Category::stated("complexity"), Some(Category::Performance));
        for c in Category::all() {
            assert_eq!(Category::stated(c.as_str()), Some(c));
        }
    }

    // Guards the two lists against drift: every string quorum has actually
    // observed in the wild must be classifiable, and exactly one of them
    // ("manual") is a placeholder.
    #[test]
    fn every_observed_legacy_category_is_a_label_except_the_placeholder() {
        let fixture = include_str!("../tests/fixtures/feedback_categories_observed.txt");
        let mut placeholders = Vec::new();
        for line in fixture.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if Category::stated(line).is_none() {
                placeholders.push(line.to_string());
            }
        }
        assert_eq!(
            placeholders,
            vec!["manual".to_string()],
            "observed-category fixture drifted from PLACEHOLDER_CATEGORIES"
        );
    }
}
