/// Context7 integration: fetch framework-specific docs for LLM review enrichment.
/// Detected frameworks are mapped to library queries, docs fetched, and injected into prompts.

/// A document fetched from Context7 for a specific library/framework.
#[derive(Debug, Clone)]
pub struct ContextDoc {
    pub library: String,
    pub content: String,
}

/// Trait for fetching framework documentation — allows testing with fake implementations.
pub trait ContextFetcher: Send + Sync {
    fn resolve_library(&self, name: &str) -> Option<String>;
    fn query_docs(&self, library_id: &str, query: &str, max_tokens: usize) -> Option<String>;
}

/// Map framework names to (library_name, query) pairs for Context7.
/// Generic Context7 query baseline for an arbitrary dep, parameterized by language.
/// Used when the dep name is not in the curated allow-list.
pub fn generic_query_for_language(lang: &str) -> &'static str {
    match lang {
        "rust" => "common pitfalls async safety error handling",
        "python" => "common pitfalls security type safety",
        "typescript" | "javascript" => "common pitfalls security type safety async",
        _ => "common pitfalls security",
    }
}

/// Look up the curated Context7 query for a known framework name.
/// Returns None for uncurated names — callers should fall back to a generic query.
pub fn curated_query_for(name: &str) -> Option<String> {
    let q = match name {
        "react" => "hooks rules component lifecycle common pitfalls",
        "nextjs" | "next" | "next.js" => "server components data fetching security",
        "django" => "ORM security CSRF protection middleware",
        "fastapi" => "dependency injection security validation",
        "flask" => "request handling security session management",
        "express" => "middleware security input validation",
        "vue" => "reactivity composition API common pitfalls",
        "fastify" => "plugin system validation security hooks",
        "home-assistant" => "automations templates blueprints Jinja2 states triggers conditions actions",
        "esphome" => "yaml components lambda sensors substitutions",
        "terraform" => "provider resource data module security best practices",
        _ => return None,
    };
    Some(q.into())
}

/// Build a code-aware Context7 query by appending relevant import targets to the baseline query.
/// Preserves the baseline to avoid context starvation, appends up to 10 import keywords.
pub fn build_code_aware_query(base_query: &str, import_targets: &[String]) -> String {
    if import_targets.is_empty() {
        return base_query.to_string();
    }
    // Extract short names from import paths (e.g., "os.path.join" -> "join")
    let keywords: Vec<&str> = import_targets.iter()
        .filter_map(|imp| imp.split(&['.', '/', ':'][..]).last())
        .filter(|s| s.len() > 2) // skip very short names like "os", "re"
        .take(10)
        .collect();
    if keywords.is_empty() {
        return base_query.to_string();
    }
    format!("{} {}", base_query, keywords.join(" "))
}

/// Fetch docs for detected frameworks using a ContextFetcher.
/// (Kept temporarily as a thin wrapper over curated_query_for; replaced by
/// enrich_for_review in Task 11 + deleted in Task 15.)
pub fn fetch_framework_docs(frameworks: &[String], fetcher: &dyn ContextFetcher, import_targets: &[String]) -> Vec<ContextDoc> {
    let mut docs = Vec::new();
    for fw in frameworks {
        if let Some(query) = curated_query_for(fw) {
            if let Some(library_id) = fetcher.resolve_library(fw) {
                let enriched_query = build_code_aware_query(&query, import_targets);
                if let Some(content) = fetcher.query_docs(&library_id, &enriched_query, 5000) {
                    docs.push(ContextDoc { library: fw.clone(), content });
                }
            }
        }
    }
    docs
}

/// Format context docs as a prompt section.
pub fn format_context_section(docs: &[ContextDoc]) -> String {
    if docs.is_empty() {
        return String::new();
    }
    let mut section = "## Framework Documentation (via Context7)\n\n".to_string();
    for doc in docs {
        // Wrap in fenced block to isolate fetched content from prompt instructions.
        // Sanitize triple backticks in content to prevent fence breakout.
        let sanitized = doc.content.replace("```", "'''");
        section.push_str(&format!("### {}\n```\n{}\n```\n\n", doc.library, sanitized));
    }
    section
}

/// Caching wrapper around a ContextFetcher. Caches query_docs results by (library_id, query, max_tokens) key.
pub struct CachedContextFetcher<'a> {
    inner: &'a dyn ContextFetcher,
    cache: std::sync::Mutex<std::collections::HashMap<(String, String, usize), Option<String>>>,
    max_entries: usize,
}

impl<'a> CachedContextFetcher<'a> {
    pub fn new(inner: &'a dyn ContextFetcher, max_entries: usize) -> Self {
        Self {
            inner,
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            max_entries,
        }
    }
}

impl<'a> ContextFetcher for CachedContextFetcher<'a> {
    fn resolve_library(&self, name: &str) -> Option<String> {
        self.inner.resolve_library(name)
    }

    fn query_docs(&self, library_id: &str, query: &str, max_tokens: usize) -> Option<String> {
        let key = (library_id.to_string(), query.to_string(), max_tokens);

        // Check cache
        if let Ok(cache) = self.cache.lock() {
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        // Cache miss — fetch from inner
        let result = self.inner.query_docs(library_id, query, max_tokens);

        // Store in cache
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= self.max_entries {
                cache.clear(); // simple eviction: clear all when full
            }
            cache.insert(key, result.clone());
        }

        result
    }
}

/// Real Context7 fetcher — calls Context7 HTTP API directly.
/// Uses async reqwest::Client internally, bridged to sync via block_in_place.
/// Requires CONTEXT7_API_KEY env var. Gracefully degrades if not set.
pub struct Context7HttpFetcher {
    http: reqwest::Client,
    api_key: Option<String>,
}

impl Context7HttpFetcher {
    /// Build a fetcher.
    ///
    /// Returns `Err` if the underlying reqwest client builder fails
    /// (rare in practice — bad TLS backend, exotic environment) so the
    /// 10s timeout config doesn't get silently dropped via
    /// `unwrap_or_default()` (issue #66).
    pub fn new() -> anyhow::Result<Self> {
        let api_key = std::env::var("CONTEXT7_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| {
                let home = std::env::var("HOME").ok()?;
                std::fs::read_to_string(format!("{}/.context7_key", home)).ok()
                    .map(|s| s.trim().to_string())
                    .filter(|k| !k.is_empty())
            });
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build Context7 reqwest client: {e}"))?;
        Ok(Self { http, api_key })
    }

    fn block_on<F>(&self, f: F) -> F::Output
    where
        F: std::future::Future + Send,
        F::Output: Send,
    {
        crate::llm_client::block_on_async(f)
    }
}

impl ContextFetcher for Context7HttpFetcher {
    fn resolve_library(&self, name: &str) -> Option<String> {
        let api_key = self.api_key.as_ref()?;

        let resp = match self.block_on(
            self.http
                .get("https://context7.com/api/v1/search")
                .query(&[("libraryName", name), ("query", name)])
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Context7 resolve_library error: {}", e);
                return None;
            }
        };

        let status = resp.status();
        if !status.is_success() {
            eprintln!("Context7 resolve_library: HTTP {}", status);
            return None;
        }

        let json: serde_json::Value = match self.block_on(resp.json()) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Context7 resolve_library: JSON parse error: {}", e);
                return None;
            }
        };
        json["results"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn query_docs(&self, library_id: &str, query: &str, max_tokens: usize) -> Option<String> {
        let api_key = self.api_key.as_ref()?;

        let resp = match self.block_on(
            self.http
                .get(format!("https://context7.com/api/v1/{}",
                    library_id.trim_start_matches('/').split('/')
                        .map(|seg| url::form_urlencoded::byte_serialize(seg.as_bytes()).collect::<String>())
                        .collect::<Vec<_>>().join("/")))
                .query(&[("query", query), ("tokens", &max_tokens.to_string())])
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Context7 query_docs error: {}", e);
                return None;
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body = self.block_on(resp.text()).unwrap_or_default();
            eprintln!("Context7 query_docs: HTTP {} - {}", status, &body[..200.min(body.len())]);
            return None;
        }

        let body_text = match self.block_on(resp.text()) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Context7 query_docs: read body error: {}", e);
                return None;
            }
        };

        if body_text.trim().is_empty() {
            return None;
        }

        // Context7 API may return plain text/markdown or JSON
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(content) = json["content"].as_str() {
                if !content.is_empty() {
                    return Some(content.to_string());
                }
            }
            if let Some(snippets) = json["snippets"].as_array() {
                let combined: String = snippets.iter()
                    .filter_map(|s| s["content"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                if !combined.is_empty() {
                    return Some(combined);
                }
            }
        }

        let truncated: String = body_text.chars().take(max_tokens * 4).collect();
        Some(truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_query_for_known_frameworks_contains_semantic_markers() {
        // Each framework's curated query MUST contain a load-bearing keyword.
        // Catches the "silently changed to empty string" failure mode without
        // brittling on exact wording.
        let cases = [
            ("react", "hooks"),
            ("nextjs", "server"),
            ("django", "ORM"),
            ("fastapi", "dependency"),
            ("flask", "session"),
            ("express", "middleware"),
            ("vue", "reactivity"),
            ("fastify", "plugin"),
            ("home-assistant", "Jinja2"),
            ("esphome", "lambda"),
            ("terraform", "provider"),
        ];
        for (name, marker) in cases {
            let q = curated_query_for(name)
                .unwrap_or_else(|| panic!("missing curated query for {name}"));
            assert!(q.contains(marker),
                "curated query for {name} missing marker '{marker}': got {q:?}");
        }
    }

    #[test]
    fn curated_query_for_unknown_returns_none() {
        assert!(curated_query_for("tokio").is_none());
        assert!(curated_query_for("xyz-does-not-exist").is_none());
    }

    #[test]
    fn generic_query_for_rust_targets_async_and_errors() {
        let q = generic_query_for_language("rust");
        assert!(q.contains("async"), "rust query missing async: {q:?}");
        assert!(q.contains("error"), "rust query missing error: {q:?}");
    }

    #[test]
    fn generic_query_for_python_targets_security_and_types() {
        let q = generic_query_for_language("python");
        assert!(q.contains("security"));
        assert!(q.contains("type"));
    }

    #[test]
    fn generic_query_for_typescript_and_javascript_target_async_security_types() {
        for lang in ["typescript", "javascript"] {
            let q = generic_query_for_language(lang);
            assert!(q.contains("async"), "{lang}: {q:?}");
            assert!(q.contains("security"), "{lang}: {q:?}");
            assert!(q.contains("type"), "{lang}: {q:?}");
        }
    }

    #[test]
    fn generic_query_for_unknown_language_falls_back_to_minimal_security() {
        let q = generic_query_for_language("brainfuck");
        assert!(q.contains("security"), "fallback must mention security: {q:?}");
    }

    #[test]
    fn async_fetcher_resolves_library() {
        struct FakeAsyncFetcher;
        impl ContextFetcher for FakeAsyncFetcher {
            fn resolve_library(&self, name: &str) -> Option<String> {
                Some(format!("/ctx/{}", name))
            }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> {
                Some("async docs content".into())
            }
        }
        let docs = fetch_framework_docs(&["react".into()], &FakeAsyncFetcher, &[]);
        assert_eq!(docs.len(), 1);
        assert!(docs[0].content.contains("async"));
    }

    #[test]
    fn fetch_with_fake_fetcher() {
        struct FakeFetcher;
        impl ContextFetcher for FakeFetcher {
            fn resolve_library(&self, name: &str) -> Option<String> {
                Some(format!("/context7/{}", name))
            }
            fn query_docs(&self, library_id: &str, _query: &str, _max_tokens: usize) -> Option<String> {
                Some(format!("Docs for {}: use hooks correctly", library_id))
            }
        }
        let docs = fetch_framework_docs(&["react".into()], &FakeFetcher, &[]);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].library, "react");
        assert!(docs[0].content.contains("hooks"));
    }

    #[test]
    fn fetch_missing_library_skipped() {
        struct NullFetcher;
        impl ContextFetcher for NullFetcher {
            fn resolve_library(&self, _: &str) -> Option<String> { None }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> { None }
        }
        let docs = fetch_framework_docs(&["react".into()], &NullFetcher, &[]);
        assert!(docs.is_empty());
    }

    #[test]
    fn format_context_section_with_docs() {
        let docs = vec![
            ContextDoc { library: "react".into(), content: "useEffect requires deps array".into() },
        ];
        let section = format_context_section(&docs);
        assert!(section.contains("react"));
        assert!(section.contains("useEffect"));
        assert!(section.contains("Framework Documentation"));
    }

    #[test]
    fn format_context_section_empty() {
        assert!(format_context_section(&[]).is_empty());
    }

    #[test]
    fn build_code_aware_query_appends_imports() {
        let base = "hooks rules component lifecycle common pitfalls";
        let imports = vec!["useEffect".to_string(), "useState".to_string(), "useCallback".to_string()];
        let query = build_code_aware_query(base, &imports);
        assert!(query.contains("hooks rules"));  // baseline preserved
        assert!(query.contains("useEffect"));
        assert!(query.contains("useState"));
    }

    #[test]
    fn build_code_aware_query_no_imports_returns_base() {
        let base = "hooks rules component lifecycle";
        let query = build_code_aware_query(base, &[]);
        assert_eq!(query, base);
    }

    #[test]
    fn build_code_aware_query_truncates_long_imports() {
        let base = "security validation";
        let imports: Vec<String> = (0..50).map(|i| format!("import_{}", i)).collect();
        let query = build_code_aware_query(base, &imports);
        assert!(query.len() < 300);
        assert!(query.contains("security validation")); // baseline preserved
    }

    #[test]
    fn build_code_aware_query_extracts_short_names() {
        let base = "ORM security";
        let imports = vec!["os.path.join".to_string(), "collections.OrderedDict".to_string()];
        let query = build_code_aware_query(base, &imports);
        assert!(query.contains("join"));
        assert!(query.contains("OrderedDict"));
    }

    #[test]
    fn build_code_aware_query_filters_short_names() {
        let base = "hooks";
        let imports = vec!["os".to_string(), "re".to_string(), "useEffect".to_string()];
        let query = build_code_aware_query(base, &imports);
        assert!(query.contains("useEffect"));
        assert!(!query.contains(" os ")); // "os" is too short (<=2 chars), filtered
    }

    #[test]
    fn cached_fetcher_avoids_duplicate_calls() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct CountingFetcher {
            calls: Arc<AtomicUsize>,
        }
        impl ContextFetcher for CountingFetcher {
            fn resolve_library(&self, name: &str) -> Option<String> {
                Some(format!("/lib/{}", name))
            }
            fn query_docs(&self, library_id: &str, _query: &str, _max_tokens: usize) -> Option<String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Some(format!("docs for {}", library_id))
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let inner = CountingFetcher { calls: calls.clone() };
        let cached = CachedContextFetcher::new(&inner, 16);

        // First call hits inner
        let r1 = cached.query_docs("/lib/react", "hooks", 5000);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(r1.is_some());

        // Second call with same args should be cached
        let r2 = cached.query_docs("/lib/react", "hooks", 5000);
        assert_eq!(calls.load(Ordering::SeqCst), 1); // no additional call
        assert_eq!(r1, r2);

        // Different query hits inner again
        let _r3 = cached.query_docs("/lib/react", "different query", 5000);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cached_fetcher_delegates_resolve() {
        struct StubFetcher;
        impl ContextFetcher for StubFetcher {
            fn resolve_library(&self, name: &str) -> Option<String> {
                Some(format!("/resolved/{}", name))
            }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> {
                None
            }
        }

        let inner = StubFetcher;
        let cached = CachedContextFetcher::new(&inner, 16);
        assert_eq!(cached.resolve_library("react"), Some("/resolved/react".into()));
    }
}
