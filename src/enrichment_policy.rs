//! Precision-targeting logic for Context7 enrichment.
//!
//! Determines whether a dependency should receive Context7-enriched docs,
//! and if so, how large a token budget to allocate.
//!
//! Decision tree:
//!   1. Usage gate: if the dep appears 0 times in file imports → skip.
//!   2. Mainstream skip-list: if the LLM already knows the dep well → skip.
//!   3. Popularity tier (via registry): high-popularity deps get smaller budgets
//!      since the LLM's training data already covers them.
//!   4. Quality gate: Context7 docs with low benchmark score or few snippets
//!      are not worth injecting (would add noise, not signal).
//!   5. Usage multiplier: heavier usage in the file → more budget.

// ── Component 1: Mainstream Skip-List ────────────────────────────────────────

// Bundled skip-list of libraries the LLM knows well from training data.
/// Returns true if `dep_name` is in the built-in mainstream skip-list for `language`.
pub fn is_mainstream(dep_name: &str, language: &str) -> bool {
    let normalized = dep_name.to_lowercase().replace('-', "_");
    let list: &[&str] = match language {
        "rust" => &[
            "serde",
            "serde_json",
            "tokio",
            "anyhow",
            "thiserror",
            "clap",
            "reqwest",
            "tracing",
            "tracing_subscriber",
            "log",
            "rand",
            "chrono",
            "regex",
            "hyper",
            "axum",
            "actix_web",
            "rocket",
            "diesel",
            "futures",
            "bytes",
            "syn",
            "quote",
            "proc_macro2",
            "rayon",
            "crossbeam",
            "parking_lot",
            "once_cell",
            "lazy_static",
        ],
        "typescript" | "javascript" => &[
            "react",
            "react_dom",
            "next",
            "vue",
            "angular",
            "express",
            "lodash",
            "axios",
            "zod",
            "typescript",
            "webpack",
            "vite",
            "eslint",
            "prettier",
            "jest",
            "mocha",
            "chai",
            "jquery",
            "moment",
            "dayjs",
            "uuid",
        ],
        "python" => &[
            "django",
            "flask",
            "fastapi",
            "requests",
            "numpy",
            "pandas",
            "pydantic",
            "sqlalchemy",
            "pytest",
            "scipy",
            "matplotlib",
            "pillow",
            "boto3",
            "celery",
            "redis",
            "httpx",
            "uvicorn",
            "gunicorn",
            "click",
            "typer",
            "rich",
        ],
        _ => return false,
    };
    list.contains(&normalized.as_str())
}

// ── Component 5: Usage Relevance Gate ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLevel {
    None,
    Low,
    Meaningful,
    Heavy,
}

pub fn usage_relevance(dep_name: &str, imports: &[String]) -> UsageLevel {
    let count = imports
        .iter()
        .filter(|imp| {
            crate::context_enrichment::normalize_import_to_dep_names(imp)
                .iter()
                .any(|n| n == dep_name)
        })
        .count();
    match count {
        0 => UsageLevel::None,
        1 => UsageLevel::Low,
        2..=3 => UsageLevel::Meaningful,
        _ => UsageLevel::Heavy,
    }
}

impl UsageLevel {
    pub fn budget_multiplier(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Low => 0.3,
            Self::Meaningful => 1.0,
            Self::Heavy => 1.0,
        }
    }
}

// ── EnrichmentPolicy ─────────────────────────────────────────────────────────

/// Budget for a dep that passed layers 1 and 2, scaled by Context7's own
/// quality signals.
///
/// #522: this was `PopularityTier::token_budget` with the tier always
/// `Unknown`, because the registry that would have assigned a real tier was
/// opt-in and never enabled -- `context7_budget_reduced` was 0 across 1,697
/// recorded reviews. The tiers, the registry client and its 7-day cache are
/// deleted; the `Unknown` base of 1000 is inlined here so the budget this
/// returns is byte-for-byte what shipped.
fn quality_scaled_budget(benchmark: f64, snippets: u32) -> usize {
    const BASE_BUDGET: f64 = 1000.0;

    // Kept in the original positive form (`>=`) rather than inverted to `<`.
    // They are not equivalent: a NaN benchmark fails `>= 50.0` and returns 0,
    // but also fails `< 50.0` and would fall through to a granted budget.
    // Quorum's own review of #522 caught that inversion.
    let quality_ok = benchmark >= 50.0 && snippets >= 5;
    if !quality_ok {
        return 0;
    }

    let quality_multiplier = if benchmark >= 80.0 && snippets >= 50 {
        1.0
    } else if benchmark >= 65.0 && snippets >= 20 {
        0.6
    } else {
        0.3
    };

    (BASE_BUDGET * quality_multiplier) as usize
}

pub struct EnrichmentPolicy;

impl EnrichmentPolicy {
    /// Cheap local-only check: returns true if this dep would definitely get
    /// budget=0 without needing a Context7 resolve or registry lookup.
    pub fn would_skip_locally(&self, dep_name: &str, language: &str, imports: &[String]) -> bool {
        let usage = usage_relevance(dep_name, imports);
        if matches!(usage, UsageLevel::None) {
            return true;
        }
        is_mainstream(dep_name, language)
    }

    pub fn token_budget_for(
        &self,
        dep_name: &str,
        language: &str,
        imports: &[String],
        resolve: &crate::context_enrichment::ResolveResult,
    ) -> usize {
        let usage = usage_relevance(dep_name, imports);
        if matches!(usage, UsageLevel::None) {
            return 0;
        }

        if is_mainstream(dep_name, language) {
            return 0;
        }

        let benchmark = resolve.benchmark_score.unwrap_or(0.0);
        let snippets = resolve.snippet_count.unwrap_or(0);
        let base_budget = quality_scaled_budget(benchmark, snippets);

        (base_budget as f64 * usage.budget_multiplier()) as usize
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Skip-list tests
    #[test]
    fn mainstream_rust_deps_are_skipped() {
        assert!(is_mainstream("serde", "rust"));
        assert!(is_mainstream("tokio", "rust"));
        assert!(is_mainstream("anyhow", "rust"));
    }

    #[test]
    fn niche_rust_deps_are_not_skipped() {
        assert!(!is_mainstream("fastembed", "rust"));
        assert!(!is_mainstream("sqlite_vec", "rust"));
        assert!(!is_mainstream("tantivy", "rust"));
    }

    #[test]
    fn mainstream_js_deps_are_skipped() {
        assert!(is_mainstream("react", "typescript"));
        assert!(is_mainstream("express", "javascript"));
        assert!(is_mainstream("next", "typescript"));
    }

    #[test]
    fn mainstream_python_deps_are_skipped() {
        assert!(is_mainstream("django", "python"));
        assert!(is_mainstream("fastapi", "python"));
        assert!(is_mainstream("requests", "python"));
    }

    #[test]
    fn unknown_language_is_never_mainstream() {
        assert!(!is_mainstream("serde", "unknown"));
    }

    // Budget tests
    //
    // #522 deleted the popularity tiers and the registry that assigned them.
    // These port the assertions that still describe live behaviour: the
    // registry was never enabled, so every dep took the `Unknown` base of
    // 1000, and the quality gate and multipliers are unchanged.

    #[test]
    fn budget_is_zero_for_poor_quality_docs() {
        // Gate: benchmark >= 50 AND snippets >= 5, else nothing is worth sending.
        assert_eq!(quality_scaled_budget(30.0, 200), 0);
        assert_eq!(quality_scaled_budget(80.0, 2), 0);
    }

    #[test]
    fn budget_scales_with_doc_quality() {
        // Same three multipliers (1.0 / 0.6 / 0.3) that PopularityTier applied,
        // against the Unknown base of 1000. The first line is the old
        // `unknown_tier_budget` assertion, preserved verbatim in value.
        assert_eq!(quality_scaled_budget(80.0, 100), 1000);
        assert_eq!(quality_scaled_budget(70.0, 30), 600);
        assert_eq!(quality_scaled_budget(55.0, 10), 300);
    }

    #[test]
    fn budget_is_zero_for_a_non_finite_benchmark() {
        // NaN must fail the gate. Writing the gate as `benchmark < 50.0`
        // instead of `benchmark >= 50.0` silently grants a budget here,
        // because every NaN comparison is false.
        assert_eq!(quality_scaled_budget(f64::NAN, 100), 0);
        assert_eq!(quality_scaled_budget(f64::NEG_INFINITY, 100), 0);
        // Positive infinity legitimately clears the gate.
        assert_eq!(quality_scaled_budget(f64::INFINITY, 100), 1000);
    }

    #[test]
    fn budget_boundaries_are_inclusive() {
        // Exactly on each threshold, so an accidental `>` for `>=` is caught.
        assert_eq!(quality_scaled_budget(50.0, 5), 300);
        assert_eq!(quality_scaled_budget(65.0, 20), 600);
        assert_eq!(quality_scaled_budget(80.0, 50), 1000);
    }

    // Usage relevance tests
    #[test]
    fn single_import_is_low_usage() {
        let imports = vec!["Deserialize: use serde::Deserialize;".into()];
        assert_eq!(usage_relevance("serde", &imports), UsageLevel::Low);
    }

    #[test]
    fn multiple_imports_is_meaningful() {
        let imports = vec![
            "Deserialize: use serde::Deserialize;".into(),
            "Serialize: use serde::Serialize;".into(),
            "Value: use serde_json::Value;".into(),
        ];
        assert_eq!(usage_relevance("serde", &imports), UsageLevel::Meaningful);
    }

    #[test]
    fn many_imports_is_heavy() {
        let imports: Vec<String> = (0..5)
            .map(|i| format!("sym{i}: use tokio::sync::sym{i};"))
            .collect();
        assert_eq!(usage_relevance("tokio", &imports), UsageLevel::Heavy);
    }

    #[test]
    fn no_matching_imports_is_none() {
        let imports = vec!["useState: import { useState } from 'react'".into()];
        assert_eq!(usage_relevance("express", &imports), UsageLevel::None);
    }

    // EnrichmentPolicy integration tests
    #[test]
    fn policy_skips_mainstream_dep() {
        let policy = EnrichmentPolicy;
        let resolve = crate::context_enrichment::ResolveResult {
            library_id: "/serde-rs/serde".into(),
            benchmark_score: Some(83.7),
            snippet_count: Some(366),
            reputation: Some("High".into()),
        };
        let imports = vec!["Deserialize: use serde::Deserialize;".into()];
        assert_eq!(
            policy.token_budget_for("serde", "rust", &imports, &resolve),
            0
        );
    }

    #[test]
    fn policy_enriches_niche_dep() {
        let policy = EnrichmentPolicy;
        let resolve = crate::context_enrichment::ResolveResult {
            library_id: "/qdrant/fastembed".into(),
            benchmark_score: Some(79.5),
            snippet_count: Some(317),
            reputation: Some("High".into()),
        };
        let imports = vec![
            "TextEmbedding: use fastembed::TextEmbedding;".into(),
            "EmbeddingModel: use fastembed::EmbeddingModel;".into(),
        ];
        let budget = policy.token_budget_for("fastembed", "rust", &imports, &resolve);
        assert!(budget > 0, "niche dep should get a budget, got {budget}");
    }

    #[test]
    fn policy_skips_no_usage() {
        let policy = EnrichmentPolicy;
        let resolve = crate::context_enrichment::ResolveResult {
            library_id: "/foo/bar".into(),
            benchmark_score: Some(90.0),
            snippet_count: Some(500),
            reputation: Some("High".into()),
        };
        assert_eq!(policy.token_budget_for("bar", "rust", &[], &resolve), 0);
    }
}
