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
    /// Returns every variant. Phase 1: empty — Phase 2 returns all 10.
    pub fn all() -> Vec<Category> {
        Vec::new()
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
    fn from(_s: String) -> Self {
        // Phase 1: wrong default so the from-string mapping tests fail RED.
        Category::Security
    }
}

impl From<&str> for Category {
    fn from(s: &str) -> Self {
        Category::from(s.to_string())
    }
}
