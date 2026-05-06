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

impl From<String> for Category {
    fn from(s: String) -> Self {
        match s
            .to_lowercase()
            .trim()
            .replace([' ', '_'], "-")
            .as_str()
        {
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
