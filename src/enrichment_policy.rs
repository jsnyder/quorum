/// Precision-targeting logic for Context7 enrichment.
///
/// Determines whether a dependency should receive Context7-enriched docs,
/// and if so, how large a token budget to allocate.
///
/// Decision tree:
///   1. Usage gate: if the dep appears 0 times in file imports → skip.
///   2. Mainstream skip-list: if the LLM already knows the dep well → skip.
///   3. Popularity tier (via registry): high-popularity deps get smaller budgets
///      since the LLM's training data already covers them.
///   4. Quality gate: Context7 docs with low benchmark score or few snippets
///      are not worth injecting (would add noise, not signal).
///   5. Usage multiplier: heavier usage in the file → more budget.

// ── Component 1: Mainstream Skip-List ────────────────────────────────────────

/// Bundled skip-list of libraries the LLM knows well from training data.
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

// ── Component 2: Popularity Tiers ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopularityTier {
    VeryHigh,
    High,
    Medium,
    Low,
    Unknown,
}

impl PopularityTier {
    pub fn from_downloads(monthly: u64, language: &str) -> Self {
        let (very_high, high, medium) = match language {
            "rust" => (1_000_000, 100_000, 10_000),
            "typescript" | "javascript" => (10_000_000, 1_000_000, 100_000),
            "python" => (5_000_000, 500_000, 50_000),
            _ => (1_000_000, 100_000, 10_000),
        };
        if monthly >= very_high {
            Self::VeryHigh
        } else if monthly >= high {
            Self::High
        } else if monthly >= medium {
            Self::Medium
        } else {
            Self::Low
        }
    }

    pub fn token_budget(self, benchmark: f64, snippets: u32) -> usize {
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

        let base = match self {
            Self::VeryHigh => 0,
            Self::High => 0,
            Self::Medium => 1500,
            Self::Low => 3000,
            Self::Unknown => 1000,
        };
        (base as f64 * quality_multiplier) as usize
    }
}

// ── Component 3: Registry Client ─────────────────────────────────────────────

pub trait RegistryClient: Send + Sync {
    fn monthly_downloads(&self, name: &str, language: &str) -> Option<u64>;
}

pub struct HttpRegistryClient {
    http: reqwest::Client,
}

impl HttpRegistryClient {
    pub fn new() -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .user_agent("quorum-code-review/1.0")
            .build()?;
        Ok(Self { http })
    }

    pub fn registry_url(name: &str, language: &str) -> Option<String> {
        match language {
            "rust" => Some(format!("https://crates.io/api/v1/crates/{name}")),
            "typescript" | "javascript" => Some(format!(
                "https://api.npmjs.org/downloads/point/last-month/{name}"
            )),
            "python" => {
                let normalized = name.to_lowercase().replace('-', "_");
                Some(format!(
                    "https://pypistats.org/api/packages/{normalized}/recent"
                ))
            }
            _ => None,
        }
    }

    pub fn parse_downloads(json: &serde_json::Value, language: &str) -> Option<u64> {
        match language {
            "rust" => json["crate"]["recent_downloads"]
                .as_u64()
                .or_else(|| json["crate"]["downloads"].as_u64()),
            "typescript" | "javascript" => json["downloads"].as_u64(),
            "python" => json["data"]["last_month"].as_u64(),
            _ => None,
        }
    }
}

impl RegistryClient for HttpRegistryClient {
    fn monthly_downloads(&self, name: &str, language: &str) -> Option<u64> {
        let url = Self::registry_url(name, language)?;
        let resp =
            crate::llm_client::block_on_async(self.http.get(&url).send()).ok()?;
        if !resp.status().is_success() {
            tracing::debug!(
                name,
                language,
                status = %resp.status(),
                "registry download lookup failed"
            );
            return None;
        }
        let json: serde_json::Value =
            crate::llm_client::block_on_async(resp.json()).ok()?;
        Self::parse_downloads(&json, language)
    }
}

// ── Component 4: Cached Registry ─────────────────────────────────────────────

struct RegistryCacheEntry {
    downloads: Option<u64>,
    cached_at: std::time::Instant,
}

const REGISTRY_CACHE_TTL: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 3600);

pub struct CachedRegistryClient<'a> {
    inner: &'a dyn RegistryClient,
    cache: std::sync::Mutex<lru::LruCache<(String, String), RegistryCacheEntry>>,
    ttl: std::time::Duration,
}

impl<'a> CachedRegistryClient<'a> {
    pub fn new(inner: &'a dyn RegistryClient, max_entries: usize) -> Self {
        let cap = std::num::NonZeroUsize::new(max_entries.max(1)).unwrap();
        Self {
            inner,
            cache: std::sync::Mutex::new(lru::LruCache::new(cap)),
            ttl: REGISTRY_CACHE_TTL,
        }
    }
}

impl RegistryClient for CachedRegistryClient<'_> {
    fn monthly_downloads(&self, name: &str, language: &str) -> Option<u64> {
        let key = (name.to_string(), language.to_string());
        {
            let mut cache = self.cache.lock().unwrap();
            if let Some(entry) = cache.get(&key) {
                if entry.cached_at.elapsed() < self.ttl {
                    return entry.downloads;
                }
            }
        }
        let downloads = self.inner.monthly_downloads(name, language);
        {
            let mut cache = self.cache.lock().unwrap();
            cache.put(
                key,
                RegistryCacheEntry {
                    downloads,
                    cached_at: std::time::Instant::now(),
                },
            );
        }
        downloads
    }
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

pub struct EnrichmentPolicy<'a> {
    pub registry: Option<&'a dyn RegistryClient>,
}

impl Default for EnrichmentPolicy<'_> {
    fn default() -> Self {
        Self { registry: None }
    }
}

impl EnrichmentPolicy<'_> {
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

        let tier = match &self.registry {
            Some(reg) => match reg.monthly_downloads(dep_name, language) {
                Some(count) => PopularityTier::from_downloads(count, language),
                None => PopularityTier::Unknown,
            },
            None => PopularityTier::Unknown,
        };

        let benchmark = resolve.benchmark_score.unwrap_or(0.0);
        let snippets = resolve.snippet_count.unwrap_or(0);
        let base_budget = tier.token_budget(benchmark, snippets);

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

    // Popularity tier tests
    #[test]
    fn popularity_tier_from_downloads() {
        assert_eq!(
            PopularityTier::from_downloads(5_000_000, "rust"),
            PopularityTier::VeryHigh
        );
        assert_eq!(
            PopularityTier::from_downloads(500_000, "rust"),
            PopularityTier::High
        );
        assert_eq!(
            PopularityTier::from_downloads(50_000, "rust"),
            PopularityTier::Medium
        );
        assert_eq!(
            PopularityTier::from_downloads(5_000, "rust"),
            PopularityTier::Low
        );
        assert_eq!(
            PopularityTier::from_downloads(0, "rust"),
            PopularityTier::Low
        );
    }

    #[test]
    fn npm_thresholds_are_higher_than_crates_io() {
        assert_eq!(
            PopularityTier::from_downloads(500_000, "rust"),
            PopularityTier::High
        );
        assert_eq!(
            PopularityTier::from_downloads(500_000, "typescript"),
            PopularityTier::Medium
        );
    }

    #[test]
    fn budget_zero_for_popular_deps() {
        assert_eq!(PopularityTier::VeryHigh.token_budget(95.0, 5000), 0);
        assert_eq!(PopularityTier::High.token_budget(90.0, 200), 0);
    }

    #[test]
    fn budget_scales_with_quality() {
        assert_eq!(PopularityTier::Low.token_budget(85.0, 100), 3000);
        assert_eq!(PopularityTier::Low.token_budget(70.0, 30), 1800);
        assert_eq!(PopularityTier::Low.token_budget(55.0, 10), 900);
    }

    #[test]
    fn budget_zero_for_bad_docs() {
        assert_eq!(PopularityTier::Low.token_budget(30.0, 200), 0);
        assert_eq!(PopularityTier::Low.token_budget(80.0, 2), 0);
    }

    #[test]
    fn medium_popularity_gets_reduced_budget() {
        assert_eq!(PopularityTier::Medium.token_budget(85.0, 100), 1500);
    }

    #[test]
    fn unknown_tier_budget() {
        assert_eq!(PopularityTier::Unknown.token_budget(80.0, 100), 1000);
    }

    // Registry client tests
    #[test]
    fn crates_io_url_is_correct() {
        assert_eq!(
            HttpRegistryClient::registry_url("serde", "rust"),
            Some("https://crates.io/api/v1/crates/serde".into())
        );
    }

    #[test]
    fn npm_url_is_correct() {
        assert_eq!(
            HttpRegistryClient::registry_url("react", "typescript"),
            Some("https://api.npmjs.org/downloads/point/last-month/react".into())
        );
    }

    #[test]
    fn pypi_url_is_correct() {
        assert_eq!(
            HttpRegistryClient::registry_url("django", "python"),
            Some("https://pypistats.org/api/packages/django/recent".into())
        );
    }

    #[test]
    fn unknown_language_has_no_url() {
        assert!(HttpRegistryClient::registry_url("foo", "haskell").is_none());
    }

    #[test]
    fn parse_crates_io_downloads() {
        let json = serde_json::json!({"crate": {"recent_downloads": 1_234_567u64}});
        assert_eq!(
            HttpRegistryClient::parse_downloads(&json, "rust"),
            Some(1_234_567)
        );
    }

    #[test]
    fn parse_npm_downloads() {
        let json = serde_json::json!({"downloads": 9_876_543u64});
        assert_eq!(
            HttpRegistryClient::parse_downloads(&json, "typescript"),
            Some(9_876_543)
        );
    }

    #[test]
    fn parse_pypi_downloads() {
        let json = serde_json::json!({"data": {"last_month": 456_789u64}});
        assert_eq!(
            HttpRegistryClient::parse_downloads(&json, "python"),
            Some(456_789)
        );
    }

    // Cached registry tests
    struct MockRegistry {
        downloads: u64,
    }
    impl RegistryClient for MockRegistry {
        fn monthly_downloads(&self, _: &str, _: &str) -> Option<u64> {
            Some(self.downloads)
        }
    }

    #[test]
    fn cached_registry_returns_same_result() {
        let inner = MockRegistry { downloads: 42_000 };
        let cached = CachedRegistryClient::new(&inner, 32);
        let first = cached.monthly_downloads("foo", "rust");
        let second = cached.monthly_downloads("foo", "rust");
        assert_eq!(first, Some(42_000));
        assert_eq!(second, Some(42_000));
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
        let policy = EnrichmentPolicy::default();
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
        let policy = EnrichmentPolicy::default();
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
        let policy = EnrichmentPolicy::default();
        let resolve = crate::context_enrichment::ResolveResult {
            library_id: "/foo/bar".into(),
            benchmark_score: Some(90.0),
            snippet_count: Some(500),
            reputation: Some("High".into()),
        };
        assert_eq!(
            policy.token_budget_for("bar", "rust", &[], &resolve),
            0
        );
    }
}
