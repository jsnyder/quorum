# Context7 Precision Targeting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace always-on Context7 enrichment with a precision-targeting system that only injects framework docs when the LLM is unlikely to know the library AND the docs are good enough to help.

**Architecture:** Three-layer decision pipeline: (1) usage relevance gate — skip deps the file barely touches, (2) popularity prior — bundled skip-list for obvious mainstream + live registry download counts for the long tail, cached 7d, (3) Context7 doc quality score — benchmark + snippet count gate the token budget. Each dep gets assigned a token budget from 0 (skip) to 3000 based on the combination of all three signals.

**Tech Stack:** Rust, reqwest 0.12 (already a dep), lru 0.18 (already a dep), crates.io/npm/PyPI REST APIs, serde_json

---

## File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `src/enrichment_policy.rs` | Popularity tiers, quality scoring, budget assignment, skip-list, registry client |
| Modify | `src/context_enrichment.rs` | Extend `ContextFetcher` trait to return metadata, wire policy into `enrich_for_review` |
| Modify | `src/pipeline.rs` | Pass policy into enrichment call sites |
| Modify | `src/telemetry.rs` | Add `context7_skipped_popular` and `context7_budget_reduced` counters |
| Modify | `src/main.rs` | Wire new telemetry fields into trace output |
| Modify | `Cargo.toml` | No new deps needed (reqwest, lru, serde_json already present) |

---

### Task 1: Extend ContextFetcher to Return Resolve Metadata

Currently `resolve_library` returns `Option<String>` (just the library ID). We need the benchmark score, snippet count, and reputation that Context7 sends back but we currently discard.

**Files:**
- Modify: `src/context_enrichment.rs:4-14` (ContextDoc, ContextFetcher trait)
- Modify: `src/context_enrichment.rs:543-580` (Context7HttpFetcher::resolve_library)
- Modify: `src/context_enrichment.rs:405-408` (ResolveCacheEntry)
- Modify: `src/context_enrichment.rs:656-668` (test Spy)

- [ ] **Step 1: Write failing test for ResolveResult metadata**

In `src/context_enrichment.rs`, add to the `mod tests` block (after line 695):

```rust
#[test]
fn resolve_result_carries_metadata() {
    let result = ResolveResult {
        library_id: "/serde-rs/serde".into(),
        benchmark_score: Some(83.7),
        snippet_count: Some(366),
        reputation: Some("High".into()),
    };
    assert_eq!(result.library_id, "/serde-rs/serde");
    assert_eq!(result.benchmark_score, Some(83.7));
    assert_eq!(result.snippet_count, Some(366));
    assert_eq!(result.reputation.as_deref(), Some("High"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum resolve_result_carries_metadata 2>&1 | tail -5`
Expected: FAIL — `ResolveResult` not defined

- [ ] **Step 3: Define ResolveResult struct and update trait**

In `src/context_enrichment.rs`, after the `ContextDoc` struct (after line 8), add:

```rust
/// Metadata returned by Context7's resolve endpoint alongside the library ID.
#[derive(Debug, Clone)]
pub struct ResolveResult {
    pub library_id: String,
    pub benchmark_score: Option<f64>,
    pub snippet_count: Option<u32>,
    pub reputation: Option<String>,
}
```

Update the `ContextFetcher` trait (line 11-14) to:

```rust
pub trait ContextFetcher: Send + Sync {
    fn resolve_library(&self, name: &str) -> Option<ResolveResult>;
    fn query_docs(&self, library_id: &str, query: &str, max_tokens: usize) -> Option<String>;
}
```

- [ ] **Step 4: Update Context7HttpFetcher::resolve_library to extract metadata**

Replace lines 574-579 (`json["results"]...`) with:

```rust
let first = json["results"].as_array()?.first()?;
let library_id = first.get("id")?.as_str()?.to_string();
let benchmark_score = first.get("benchmarkScore")
    .or_else(|| first.get("benchmark_score"))
    .and_then(|v| v.as_f64());
let snippet_count = first.get("codeSnippets")
    .or_else(|| first.get("code_snippets"))
    .and_then(|v| v.as_u64())
    .map(|n| n as u32);
let reputation = first.get("sourceReputation")
    .or_else(|| first.get("source_reputation"))
    .and_then(|v| v.as_str())
    .map(|s| s.to_string());
Some(ResolveResult { library_id, benchmark_score, snippet_count, reputation })
```

- [ ] **Step 5: Update ResolveCacheEntry to cache ResolveResult**

Change `ResolveCacheEntry` (line 405-408) to:

```rust
struct ResolveCacheEntry {
    result: Option<ResolveResult>,
    cached_at: std::time::Instant,
}
```

Update `CachedContextFetcher::resolve_library` to return `Option<ResolveResult>` and clone the cached result.

- [ ] **Step 6: Update try_fetch_one to use ResolveResult**

In `try_fetch_one` (line 268-298), change the `match` arm from `Some(lib_id)` to `Some(resolve)` and use `resolve.library_id` where `lib_id` was used:

```rust
match fetcher.resolve_library(name) {
    Some(resolve) => {
        metrics.context7_resolved += 1;
        let enriched = build_code_aware_query(query, imports);
        if let Some(content) = fetcher.query_docs(&resolve.library_id, &enriched, 5000) {
            docs.push(ContextDoc {
                library: name.into(),
                content,
            });
        } else {
            metrics.context7_query_failed += 1;
        }
    }
    None => {
        metrics.context7_resolve_failed += 1;
    }
}
```

- [ ] **Step 7: Update test Spy and CapturingSpy**

Update `Spy` (line 660-668):

```rust
pub struct Spy;
impl ContextFetcher for Spy {
    fn resolve_library(&self, name: &str) -> Option<ResolveResult> {
        Some(ResolveResult {
            library_id: format!("/lib/{name}"),
            benchmark_score: Some(80.0),
            snippet_count: Some(100),
            reputation: Some("High".into()),
        })
    }
    fn query_docs(&self, lib: &str, _: &str, _: usize) -> Option<String> {
        Some(format!("docs for {lib}"))
    }
}
```

Update `CapturingSpy` similarly (its `resolve_library` should also return `ResolveResult`).

- [ ] **Step 8: Run all tests to verify nothing broke**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: All 1303+ tests pass

- [ ] **Step 9: Commit**

```bash
git add src/context_enrichment.rs
git commit -m "refactor: ContextFetcher::resolve_library returns ResolveResult with metadata

Extract benchmark_score, snippet_count, and reputation from Context7
resolve response. Previously discarded — needed for precision targeting."
```

---

### Task 2: Create Enrichment Policy Module — Skip-List and Popularity Tiers

**Files:**
- Create: `src/enrichment_policy.rs`
- Modify: `src/main.rs` (add `mod enrichment_policy;`)

- [ ] **Step 1: Write failing tests for the skip-list**

Create `src/enrichment_policy.rs` with test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum enrichment_policy 2>&1 | tail -5`
Expected: FAIL — module doesn't exist yet

- [ ] **Step 3: Add mod declaration**

In `src/main.rs`, add `mod enrichment_policy;` alongside the other mod declarations.

- [ ] **Step 4: Implement is_mainstream**

At the top of `src/enrichment_policy.rs`:

```rust
/// Bundled skip-list of libraries the LLM knows well from training data.
/// Kept deliberately small — only indisputable giants per ecosystem.
/// Purpose: eliminate obvious token waste with zero false negatives.
pub fn is_mainstream(dep_name: &str, language: &str) -> bool {
    let normalized = dep_name.to_lowercase().replace('-', "_");
    let list: &[&str] = match language {
        "rust" => &[
            "serde", "serde_json", "tokio", "anyhow", "thiserror", "clap",
            "reqwest", "tracing", "tracing_subscriber", "log", "rand",
            "chrono", "regex", "hyper", "axum", "actix_web", "rocket",
            "diesel", "futures", "bytes", "syn", "quote", "proc_macro2",
            "rayon", "crossbeam", "parking_lot", "once_cell", "lazy_static",
        ],
        "typescript" | "javascript" => &[
            "react", "react_dom", "next", "vue", "angular", "express",
            "lodash", "axios", "zod", "typescript", "webpack", "vite",
            "eslint", "prettier", "jest", "mocha", "chai",
            "jquery", "moment", "dayjs", "uuid",
        ],
        "python" => &[
            "django", "flask", "fastapi", "requests", "numpy", "pandas",
            "pydantic", "sqlalchemy", "pytest", "scipy", "matplotlib",
            "pillow", "boto3", "celery", "redis", "httpx",
            "uvicorn", "gunicorn", "click", "typer", "rich",
        ],
        _ => return false,
    };
    list.contains(&normalized.as_str())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum enrichment_policy 2>&1 | tail -5`
Expected: All 5 tests pass

- [ ] **Step 6: Commit**

```bash
git add src/enrichment_policy.rs src/main.rs
git commit -m "feat(enrichment): add mainstream skip-list for precision targeting

Bundled list of ~70 well-known libraries across Rust/JS/Python that
LLMs already know from training data. Phase 1 of Context7 precision
targeting (#29)."
```

---

### Task 3: Popularity Tiers and Download-Count Registry Client

**Files:**
- Modify: `src/enrichment_policy.rs`

- [ ] **Step 1: Write failing tests for popularity tiers**

Add to the test module in `src/enrichment_policy.rs`:

```rust
#[test]
fn popularity_tier_from_downloads() {
    assert_eq!(PopularityTier::from_downloads(5_000_000, "rust"), PopularityTier::VeryHigh);
    assert_eq!(PopularityTier::from_downloads(500_000, "rust"), PopularityTier::High);
    assert_eq!(PopularityTier::from_downloads(50_000, "rust"), PopularityTier::Medium);
    assert_eq!(PopularityTier::from_downloads(5_000, "rust"), PopularityTier::Low);
    assert_eq!(PopularityTier::from_downloads(0, "rust"), PopularityTier::Low);
}

#[test]
fn npm_thresholds_are_higher_than_crates_io() {
    // 500k/month is "High" for crates.io but only "Medium" for npm
    assert_eq!(PopularityTier::from_downloads(500_000, "rust"), PopularityTier::High);
    assert_eq!(PopularityTier::from_downloads(500_000, "typescript"), PopularityTier::Medium);
}

#[test]
fn unknown_tier_is_explicit() {
    assert_eq!(PopularityTier::Unknown.token_budget(80.0, 100), 1000);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum popularity_tier 2>&1 | tail -5`
Expected: FAIL — `PopularityTier` not defined

- [ ] **Step 3: Implement PopularityTier**

Add to `src/enrichment_policy.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopularityTier {
    VeryHigh,
    High,
    Medium,
    Low,
    Unknown,
}

impl PopularityTier {
    /// Classify a monthly download count into a tier, per-ecosystem.
    /// Thresholds differ because npm packages get 10-100x more downloads
    /// than equivalent crates.io or PyPI packages due to ecosystem size.
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
}
```

- [ ] **Step 4: Run tests to verify tier logic passes**

Run: `cargo test --bin quorum popularity_tier 2>&1 | tail -5`
Expected: First two tests pass, third fails (no `token_budget` method yet)

- [ ] **Step 5: Implement token_budget**

Add to `impl PopularityTier`:

```rust
/// Assign a token budget based on popularity and doc quality.
/// `benchmark`: Context7 benchmark score (0-100), `snippets`: snippet count.
/// Returns 0 to skip enrichment entirely.
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
```

- [ ] **Step 6: Write tests for token_budget**

Add to tests:

```rust
#[test]
fn budget_zero_for_popular_deps() {
    assert_eq!(PopularityTier::VeryHigh.token_budget(95.0, 5000), 0);
    assert_eq!(PopularityTier::High.token_budget(90.0, 200), 0);
}

#[test]
fn budget_scales_with_quality() {
    // Low popularity + high quality = full budget
    assert_eq!(PopularityTier::Low.token_budget(85.0, 100), 3000);
    // Low popularity + medium quality = reduced
    assert_eq!(PopularityTier::Low.token_budget(70.0, 30), 1800);
    // Low popularity + low quality = minimal
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
```

- [ ] **Step 7: Run all enrichment_policy tests**

Run: `cargo test --bin quorum enrichment_policy 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 8: Commit**

```bash
git add src/enrichment_policy.rs
git commit -m "feat(enrichment): popularity tiers with per-ecosystem thresholds

Per-ecosystem download-count thresholds (npm is 10x crates.io).
Token budget assigned from popularity x doc quality matrix.
VeryHigh/High = skip, Medium = 1500, Low = 3000, Unknown = 1000,
scaled by quality multiplier."
```

---

### Task 4: Registry Download-Count Client

**Files:**
- Modify: `src/enrichment_policy.rs`

- [ ] **Step 1: Write failing test for RegistryClient trait**

Add to tests in `src/enrichment_policy.rs`:

```rust
#[test]
fn mock_registry_returns_downloads() {
    let client = MockRegistry { downloads: 50_000 };
    assert_eq!(client.monthly_downloads("fastembed", "rust"), Some(50_000));
}

struct MockRegistry {
    downloads: u64,
}

impl RegistryClient for MockRegistry {
    fn monthly_downloads(&self, _name: &str, _language: &str) -> Option<u64> {
        Some(self.downloads)
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum mock_registry 2>&1 | tail -5`
Expected: FAIL — `RegistryClient` not defined

- [ ] **Step 3: Define RegistryClient trait**

Add to `src/enrichment_policy.rs`:

```rust
/// Trait for fetching download counts from package registries.
pub trait RegistryClient: Send + Sync {
    fn monthly_downloads(&self, name: &str, language: &str) -> Option<u64>;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum mock_registry 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 5: Write failing test for HTTP registry client**

Add to tests:

```rust
#[test]
fn crates_io_url_is_correct() {
    let url = HttpRegistryClient::registry_url("serde", "rust");
    assert_eq!(url, Some("https://crates.io/api/v1/crates/serde".into()));
}

#[test]
fn npm_url_is_correct() {
    let url = HttpRegistryClient::registry_url("react", "typescript");
    assert_eq!(url, Some("https://api.npmjs.org/downloads/point/last-month/react".into()));
}

#[test]
fn pypi_url_is_correct() {
    let url = HttpRegistryClient::registry_url("django", "python");
    assert_eq!(url, Some("https://pypistats.org/api/packages/django/recent".into()));
}

#[test]
fn unknown_language_has_no_url() {
    assert!(HttpRegistryClient::registry_url("foo", "haskell").is_none());
}
```

- [ ] **Step 6: Implement HttpRegistryClient**

Add to `src/enrichment_policy.rs`:

```rust
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
            "typescript" | "javascript" => {
                Some(format!("https://api.npmjs.org/downloads/point/last-month/{name}"))
            }
            "python" => {
                let normalized = name.to_lowercase().replace('-', "_");
                Some(format!("https://pypistats.org/api/packages/{normalized}/recent"))
            }
            _ => None,
        }
    }

    fn parse_downloads(json: &serde_json::Value, language: &str) -> Option<u64> {
        match language {
            "rust" => {
                json["crate"]["recent_downloads"].as_u64()
                    .or_else(|| json["crate"]["downloads"].as_u64())
            }
            "typescript" | "javascript" => {
                json["downloads"].as_u64()
            }
            "python" => {
                json["data"]["last_month"].as_u64()
            }
            _ => None,
        }
    }
}

impl RegistryClient for HttpRegistryClient {
    fn monthly_downloads(&self, name: &str, language: &str) -> Option<u64> {
        let url = Self::registry_url(name, language)?;
        let resp = crate::llm_client::block_on_async(
            self.http.get(&url).send()
        ).ok()?;
        if !resp.status().is_success() {
            tracing::debug!(
                name, language, status = %resp.status(),
                "registry download lookup failed"
            );
            return None;
        }
        let json: serde_json::Value = crate::llm_client::block_on_async(
            resp.json()
        ).ok()?;
        Self::parse_downloads(&json, language)
    }
}
```

- [ ] **Step 7: Write tests for parse_downloads**

Add to tests:

```rust
#[test]
fn parse_crates_io_downloads() {
    let json = serde_json::json!({
        "crate": { "recent_downloads": 1_234_567 }
    });
    assert_eq!(
        HttpRegistryClient::parse_downloads(&json, "rust"),
        Some(1_234_567)
    );
}

#[test]
fn parse_npm_downloads() {
    let json = serde_json::json!({ "downloads": 9_876_543 });
    assert_eq!(
        HttpRegistryClient::parse_downloads(&json, "typescript"),
        Some(9_876_543)
    );
}

#[test]
fn parse_pypi_downloads() {
    let json = serde_json::json!({
        "data": { "last_month": 456_789 }
    });
    assert_eq!(
        HttpRegistryClient::parse_downloads(&json, "python"),
        Some(456_789)
    );
}
```

- [ ] **Step 8: Run all enrichment_policy tests**

Run: `cargo test --bin quorum enrichment_policy 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 9: Commit**

```bash
git add src/enrichment_policy.rs
git commit -m "feat(enrichment): registry download-count client for crates.io/npm/PyPI

RegistryClient trait + HttpRegistryClient hitting three ecosystem APIs.
URL construction and JSON response parsing per-ecosystem.
5s timeout, user-agent header for crates.io compliance."
```

---

### Task 5: Cached Registry Lookups

**Files:**
- Modify: `src/enrichment_policy.rs`

- [ ] **Step 1: Write failing test for cached registry**

Add to tests:

```rust
#[test]
fn cached_registry_returns_same_result() {
    let inner = MockRegistry { downloads: 42_000 };
    let cached = CachedRegistryClient::new(&inner, 32);
    let first = cached.monthly_downloads("foo", "rust");
    let second = cached.monthly_downloads("foo", "rust");
    assert_eq!(first, Some(42_000));
    assert_eq!(second, Some(42_000));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum cached_registry 2>&1 | tail -5`
Expected: FAIL — `CachedRegistryClient` not defined

- [ ] **Step 3: Implement CachedRegistryClient**

Add to `src/enrichment_policy.rs`:

```rust
struct RegistryCacheEntry {
    downloads: Option<u64>,
    cached_at: std::time::Instant,
}

const REGISTRY_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

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
            cache.put(key, RegistryCacheEntry {
                downloads,
                cached_at: std::time::Instant::now(),
            });
        }
        downloads
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum cached_registry 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/enrichment_policy.rs
git commit -m "feat(enrichment): cached registry client with 7d TTL

LRU cache wrapper for registry download lookups. 7-day TTL because
popularity doesn't shift fast. Negative results cached too."
```

---

### Task 6: Usage Relevance Gate

The file must meaningfully use a dependency before we consider enrichment. A single type import shouldn't trigger 3000 tokens of docs.

**Files:**
- Modify: `src/enrichment_policy.rs`

- [ ] **Step 1: Write failing tests for usage relevance**

Add to tests:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum usage_relevance 2>&1 | tail -5`
Expected: FAIL — types not defined

- [ ] **Step 3: Implement usage_relevance**

Add to `src/enrichment_policy.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLevel {
    None,
    Low,
    Meaningful,
    Heavy,
}

/// Count how many import targets in the file reference a given dependency.
/// Uses the same normalize_import_to_dep_names logic as the enrichment loop.
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
    /// Budget multiplier: None and Low skip or reduce enrichment.
    pub fn budget_multiplier(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Low => 0.3,
            Self::Meaningful => 1.0,
            Self::Heavy => 1.0,
        }
    }
}
```

Note: `normalize_import_to_dep_names` must be made `pub` in `src/context_enrichment.rs` (currently has no visibility modifier — it's private). Change `fn normalize_import_to_dep_names` to `pub fn normalize_import_to_dep_names` at line 110.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum usage_relevance 2>&1 | tail -5`
Expected: All 4 pass

- [ ] **Step 5: Commit**

```bash
git add src/enrichment_policy.rs src/context_enrichment.rs
git commit -m "feat(enrichment): usage relevance gate based on import count

Skip enrichment when a file barely touches a dependency (0-1 imports).
Meaningful (2-3) gets full budget, Heavy (4+) same. Prevents wasting
tokens on deps that appear in Cargo.toml but aren't meaningfully used."
```

---

### Task 7: Wire Policy Into enrich_for_review

This is the integration task — replace the fixed `5000` token budget with the policy-driven budget.

**Files:**
- Modify: `src/context_enrichment.rs:198-298`

- [ ] **Step 1: Write failing integration test**

Add to `src/context_enrichment.rs` test module:

```rust
#[test]
fn popular_dep_is_skipped_by_policy() {
    use crate::enrichment_policy::{EnrichmentPolicy, PopularityTier};

    let deps = vec![
        crate::dep_manifest::Dependency { name: "serde".into(), language: "rust".into() },
    ];
    let imports = vec!["Deserialize: use serde::Deserialize;".into()];
    let fetcher = test_support::Spy;

    let result = enrich_for_review_with_policy(
        &deps,
        &[],
        &imports,
        &fetcher,
        &EnrichmentPolicy::default(),
    );
    // serde is mainstream — should be skipped
    assert!(result.docs.is_empty());
    assert_eq!(result.metrics.context7_skipped_popular, 1);
}

#[test]
fn niche_dep_gets_enriched() {
    use crate::enrichment_policy::{EnrichmentPolicy, PopularityTier};

    let deps = vec![
        crate::dep_manifest::Dependency { name: "fastembed".into(), language: "rust".into() },
    ];
    let imports = vec![
        "TextEmbedding: use fastembed::TextEmbedding;".into(),
        "EmbeddingModel: use fastembed::EmbeddingModel;".into(),
    ];
    let fetcher = test_support::Spy;

    let result = enrich_for_review_with_policy(
        &deps,
        &[],
        &imports,
        &fetcher,
        &EnrichmentPolicy::default(),
    );
    assert_eq!(result.docs.len(), 1);
    assert_eq!(result.docs[0].library, "fastembed");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum popular_dep_is_skipped 2>&1 | tail -5`
Expected: FAIL — `enrich_for_review_with_policy` and `EnrichmentPolicy` not defined

- [ ] **Step 3: Define EnrichmentPolicy struct**

Add to `src/enrichment_policy.rs`:

```rust
/// Configuration for the enrichment decision pipeline.
pub struct EnrichmentPolicy<'a> {
    pub registry: Option<&'a dyn RegistryClient>,
}

impl Default for EnrichmentPolicy<'_> {
    fn default() -> Self {
        Self { registry: None }
    }
}

impl EnrichmentPolicy<'_> {
    /// Compute the token budget for a dependency based on all signals.
    pub fn token_budget_for(
        &self,
        dep_name: &str,
        language: &str,
        imports: &[String],
        resolve: &crate::context_enrichment::ResolveResult,
    ) -> usize {
        // Gate 1: usage relevance
        let usage = usage_relevance(dep_name, imports);
        if matches!(usage, UsageLevel::None) {
            return 0;
        }

        // Gate 2: mainstream skip-list
        if is_mainstream(dep_name, language) {
            return 0;
        }

        // Gate 3: popularity tier from registry
        let tier = match &self.registry {
            Some(reg) => match reg.monthly_downloads(dep_name, language) {
                Some(count) => PopularityTier::from_downloads(count, language),
                None => PopularityTier::Unknown,
            },
            None => PopularityTier::Unknown,
        };

        // Gate 4: doc quality + budget assignment
        let benchmark = resolve.benchmark_score.unwrap_or(0.0);
        let snippets = resolve.snippet_count.unwrap_or(0);
        let base_budget = tier.token_budget(benchmark, snippets);

        // Apply usage multiplier (Low = 0.3x)
        let final_budget = (base_budget as f64 * usage.budget_multiplier()) as usize;
        final_budget
    }
}
```

- [ ] **Step 4: Add context7_skipped_popular to EnrichmentMetrics**

In `src/context_enrichment.rs`, update `EnrichmentMetrics` (line 18-23):

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentMetrics {
    pub context7_resolved: u32,
    pub context7_resolve_failed: u32,
    pub context7_query_failed: u32,
    pub context7_skipped_popular: u32,
    pub context7_budget_reduced: u32,
}
```

- [ ] **Step 5: Implement enrich_for_review_with_policy**

Add to `src/context_enrichment.rs`, after `enrich_for_review` (after line 254):

```rust
/// Policy-aware enrichment: skips popular deps, tiers budgets, gates on quality.
pub fn enrich_for_review_with_policy(
    deps: &[crate::dep_manifest::Dependency],
    curated_frameworks: &[String],
    imports: &[String],
    fetcher: &dyn ContextFetcher,
    policy: &crate::enrichment_policy::EnrichmentPolicy,
) -> EnrichmentResult {
    let mut metrics = EnrichmentMetrics::default();
    let mut docs: Vec<ContextDoc> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut import_matched: Vec<&crate::dep_manifest::Dependency> = Vec::new();
    for imp in imports {
        for name in normalize_import_to_dep_names(imp) {
            if let Some(dep) = deps.iter().find(|d| d.name == name)
                && !import_matched.iter().any(|d| d.name == dep.name)
            {
                import_matched.push(dep);
            }
        }
    }

    for dep in import_matched.into_iter().take(ENRICH_K) {
        if seen.contains(&dep.name) {
            continue;
        }
        seen.insert(dep.name.clone());

        // Resolve first to get metadata for policy decision
        let resolve = match fetcher.resolve_library(&dep.name) {
            Some(r) => {
                metrics.context7_resolved += 1;
                r
            }
            None => {
                metrics.context7_resolve_failed += 1;
                continue;
            }
        };

        let budget = policy.token_budget_for(&dep.name, &dep.language, imports, &resolve);
        if budget == 0 {
            metrics.context7_skipped_popular += 1;
            continue;
        }
        if budget < 5000 {
            metrics.context7_budget_reduced += 1;
        }

        let query = curated_query_for(&dep.name)
            .unwrap_or_else(|| generic_query_for_language(&dep.language).into());
        let enriched = build_code_aware_query(&query, imports);
        if let Some(content) = fetcher.query_docs(&resolve.library_id, &enriched, budget) {
            docs.push(ContextDoc {
                library: dep.name.clone(),
                content,
            });
        } else {
            metrics.context7_query_failed += 1;
        }
    }

    // Curated frameworks bypass policy — they're directory-detected (HA/ESPHome)
    for fw in curated_frameworks {
        if seen.contains(fw) {
            continue;
        }
        if let Some(query) = curated_query_for(fw) {
            try_fetch_one(fw, &query, imports, fetcher, &mut docs, &mut metrics, &mut seen);
        }
    }

    EnrichmentResult { docs, metrics }
}
```

- [ ] **Step 6: Run integration tests**

Run: `cargo test --bin quorum popular_dep_is_skipped niche_dep_gets_enriched 2>&1 | tail -10`
Expected: Both pass

- [ ] **Step 7: Run full test suite to verify no regressions**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: All tests pass (some existing tests may need `context7_skipped_popular: 0, context7_budget_reduced: 0` added to metric assertions)

- [ ] **Step 8: Commit**

```bash
git add src/context_enrichment.rs src/enrichment_policy.rs
git commit -m "feat(enrichment): wire precision-targeting policy into enrichment loop

enrich_for_review_with_policy applies three gates:
1. Usage relevance (import count)
2. Mainstream skip-list
3. Popularity tier x doc quality -> token budget

Popular deps skipped, niche deps get full budget, unknown gets
conservative 1000. Curated frameworks (HA/ESPHome) bypass policy."
```

---

### Task 8: Wire Into Pipeline

**Files:**
- Modify: `src/pipeline.rs:569-584`
- Modify: `src/context_enrichment.rs:258-266` (enrich_for_review_in_project)

- [ ] **Step 1: Update enrich_for_review_in_project to accept policy**

In `src/context_enrichment.rs`, update the convenience wrapper (line 258-266):

```rust
pub fn enrich_for_review_in_project(
    project_root: &std::path::Path,
    imports: &[String],
    curated_frameworks: &[String],
    fetcher: &dyn ContextFetcher,
    policy: &crate::enrichment_policy::EnrichmentPolicy,
) -> EnrichmentResult {
    let deps = crate::dep_manifest::parse_dependencies(project_root);
    enrich_for_review_with_policy(&deps, curated_frameworks, imports, fetcher, policy)
}
```

- [ ] **Step 2: Update pipeline.rs call sites**

At the call sites (line 569-584), add the policy parameter. Construct the policy near the top of `review_file` or in the `PipelineConfig`:

```rust
let policy = crate::enrichment_policy::EnrichmentPolicy {
    registry: None, // Phase 1: skip-list + quality gate only, no live registry yet
};
```

Pass `&policy` to both `enrich_for_review_in_project` call sites.

- [ ] **Step 3: Add PipelineConfig field for registry client**

In `PipelineConfig` (line 126-179), add:

```rust
pub registry_client: Option<std::sync::Arc<dyn crate::enrichment_policy::RegistryClient>>,
```

Default to `None` in construction. When `Some`, pass it into the policy.

- [ ] **Step 4: Update old enrich_for_review callers**

The old `enrich_for_review` function (without policy) should remain as a backward-compat wrapper that creates a default policy:

```rust
pub fn enrich_for_review(
    deps: &[crate::dep_manifest::Dependency],
    curated_frameworks: &[String],
    imports: &[String],
    fetcher: &dyn ContextFetcher,
) -> EnrichmentResult {
    enrich_for_review_with_policy(
        deps,
        curated_frameworks,
        imports,
        fetcher,
        &crate::enrichment_policy::EnrichmentPolicy::default(),
    )
}
```

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
git add src/context_enrichment.rs src/pipeline.rs
git commit -m "feat(enrichment): wire precision-targeting policy into review pipeline

enrich_for_review_in_project now accepts EnrichmentPolicy.
Pipeline constructs policy from PipelineConfig. Phase 1 ships with
skip-list + quality gate only (registry_client = None). Old
enrich_for_review function preserved as backward-compat wrapper."
```

---

### Task 9: Update Telemetry

**Files:**
- Modify: `src/telemetry.rs`
- Modify: `src/main.rs` (where TelemetryEntry is populated)

- [ ] **Step 1: Add new fields to TelemetryEntry**

In `src/telemetry.rs`, add to `TelemetryEntry`:

```rust
#[serde(default)]
pub context7_skipped_popular: u32,
#[serde(default)]
pub context7_budget_reduced: u32,
```

- [ ] **Step 2: Wire metrics into TelemetryEntry at the write site**

Find where `TelemetryEntry` is constructed from `EnrichmentMetrics` (in `src/main.rs` or `src/pipeline.rs`) and add the two new fields from `metrics.context7_skipped_popular` and `metrics.context7_budget_reduced`.

- [ ] **Step 3: Run full test suite**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/telemetry.rs src/main.rs
git commit -m "feat(telemetry): track context7_skipped_popular and budget_reduced

New counters in TelemetryEntry for observability into precision
targeting decisions. serde(default) for backward-compat with
pre-existing telemetry rows."
```

---

### Task 10: Phase 2 Prep — Enable Live Registry (Behind Flag)

This task wires the `HttpRegistryClient` into the pipeline, gated behind a `--live-registry` flag or `QUORUM_CONTEXT7_LIVE_REGISTRY=1` env var.

**Files:**
- Modify: `src/main.rs` (CLI arg parsing)
- Modify: `src/pipeline.rs` (construct registry client)

- [ ] **Step 1: Add CLI flag**

Add `--live-registry` flag to the review command's CLI args. When set, construct an `HttpRegistryClient` wrapped in `CachedRegistryClient` and pass it into `PipelineConfig.registry_client`.

- [ ] **Step 2: Wire into PipelineConfig**

In the review command handler:

```rust
let registry_client: Option<std::sync::Arc<dyn crate::enrichment_policy::RegistryClient>> =
    if live_registry {
        let http = crate::enrichment_policy::HttpRegistryClient::new()?;
        let cached = crate::enrichment_policy::CachedRegistryClient::new_boxed(http, 128);
        Some(std::sync::Arc::new(cached))
    } else {
        None
    };
```

Note: `CachedRegistryClient` needs an owned variant (`new_boxed`) that takes `Box<dyn RegistryClient>` instead of a reference, since the registry client needs to live as long as the pipeline. Add this alongside the existing `new`:

```rust
pub fn new_owned(inner: Box<dyn RegistryClient>, max_entries: usize) -> CachedRegistryClientOwned {
    // ...
}
```

Alternatively, make `CachedRegistryClient` use `Arc<dyn RegistryClient>` instead of a reference.

- [ ] **Step 3: Run full test suite**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/pipeline.rs src/enrichment_policy.rs
git commit -m "feat(enrichment): gate live registry lookups behind --live-registry flag

Phase 2 of precision targeting: live download-count lookups from
crates.io/npm/PyPI, cached 7d. Off by default; enable with
--live-registry or QUORUM_CONTEXT7_LIVE_REGISTRY=1."
```

---

## Phase Summary

| Phase | What ships | Token savings | Complexity |
|-------|-----------|--------------|------------|
| **Phase 1 (Tasks 1-9)** | Skip-list + usage gate + quality gate | Eliminates enrichment for ~70 mainstream libs | Low |
| **Phase 2 (Task 10)** | Live registry lookups (flagged) | Tiers budget for entire long tail | Medium |
| **Phase 3 (future)** | Advanced-usage overrides for mainstream | Recovers niche API coverage in popular libs | High |
