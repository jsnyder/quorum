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
pub fn framework_queries(frameworks: &[String]) -> Vec<(String, String)> {
    let mut queries = Vec::new();
    for fw in frameworks {
        let pair = match fw.as_str() {
            "react" => Some(("react".into(), "hooks rules component lifecycle common pitfalls".into())),
            "nextjs" => Some(("next.js".into(), "server components data fetching security".into())),
            "django" => Some(("django".into(), "ORM security CSRF protection middleware".into())),
            "fastapi" => Some(("fastapi".into(), "dependency injection security validation".into())),
            "flask" => Some(("flask".into(), "request handling security session management".into())),
            "express" => Some(("express".into(), "middleware security input validation".into())),
            "vue" => Some(("vue".into(), "reactivity composition API common pitfalls".into())),
            "fastify" => Some(("fastify".into(), "plugin system validation security hooks".into())),
            "home-assistant" => Some(("home-assistant".into(), "automations templates blueprints Jinja2 states triggers conditions actions".into())),
            "esphome" => Some(("esphome".into(), "yaml components lambda sensors substitutions".into())),
            _ => None,
        };
        if let Some(p) = pair {
            queries.push(p);
        }
    }
    queries
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
pub fn fetch_framework_docs(frameworks: &[String], fetcher: &dyn ContextFetcher, import_targets: &[String]) -> Vec<ContextDoc> {
    let queries = framework_queries(frameworks);
    let mut docs = Vec::new();
    for (lib_name, query) in queries {
        if let Some(library_id) = fetcher.resolve_library(&lib_name) {
            let enriched_query = build_code_aware_query(&query, import_targets);
            if let Some(content) = fetcher.query_docs(&library_id, &enriched_query, 5000) {
                docs.push(ContextDoc { library: lib_name, content });
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
        // Wrap in fenced block to isolate fetched content from prompt instructions
        section.push_str(&format!("### {}\n```\n{}\n```\n\n", doc.library, doc.content));
    }
    section
}

/// Real Context7 fetcher — calls Context7 HTTP API directly.
/// Uses async reqwest::Client internally, bridged to sync via block_in_place.
/// Requires CONTEXT7_API_KEY env var. Gracefully degrades if not set.
pub struct Context7HttpFetcher {
    http: reqwest::Client,
    api_key: Option<String>,
}

impl Context7HttpFetcher {
    pub fn new() -> Self {
        let api_key = std::env::var("CONTEXT7_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| {
                let home = std::env::var("HOME").ok()?;
                std::fs::read_to_string(format!("{}/.context7_key", home)).ok()
                    .map(|s| s.trim().to_string())
                    .filter(|k| !k.is_empty())
            });
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            api_key,
        }
    }

    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
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
                .get(format!("https://context7.com/api/v1{}", library_id))
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
    fn framework_queries_react() {
        let queries = framework_queries(&["react".into()]);
        assert!(!queries.is_empty());
        assert!(queries.iter().any(|(lib, _)| lib == "react"));
    }

    #[test]
    fn framework_queries_django() {
        let queries = framework_queries(&["django".into()]);
        assert!(queries.iter().any(|(lib, _)| lib == "django"));
    }

    #[test]
    fn framework_queries_empty() {
        let queries = framework_queries(&[]);
        assert!(queries.is_empty());
    }

    #[test]
    fn framework_queries_unknown_framework_skipped() {
        let queries = framework_queries(&["unknown-framework".into()]);
        assert!(queries.is_empty());
    }

    #[test]
    fn framework_queries_home_assistant() {
        let queries = framework_queries(&["home-assistant".into()]);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].0, "home-assistant");
        assert!(queries[0].1.contains("automation"));
    }

    #[test]
    fn framework_queries_esphome() {
        let queries = framework_queries(&["esphome".into()]);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].0, "esphome");
        assert!(queries[0].1.contains("yaml"));
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
}
