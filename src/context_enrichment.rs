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
            _ => None,
        };
        if let Some(p) = pair {
            queries.push(p);
        }
    }
    queries
}

/// Fetch docs for detected frameworks using a ContextFetcher.
pub fn fetch_framework_docs(frameworks: &[String], fetcher: &dyn ContextFetcher) -> Vec<ContextDoc> {
    let queries = framework_queries(frameworks);
    let mut docs = Vec::new();
    for (lib_name, query) in queries {
        if let Some(library_id) = fetcher.resolve_library(&lib_name) {
            if let Some(content) = fetcher.query_docs(&library_id, &query, 5000) {
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
/// Requires CONTEXT7_API_KEY env var. Gracefully degrades if not set.
pub struct Context7HttpFetcher {
    http: reqwest::blocking::Client,
    api_key: Option<String>,
}

impl Context7HttpFetcher {
    pub fn new() -> Self {
        let api_key = std::env::var("CONTEXT7_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| {
                // Fallback: read from ~/.context7_key file
                let home = std::env::var("HOME").ok()?;
                std::fs::read_to_string(format!("{}/.context7_key", home)).ok()
                    .map(|s| s.trim().to_string())
                    .filter(|k| !k.is_empty())
            });
        Self {
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            api_key,
        }
    }
}

impl ContextFetcher for Context7HttpFetcher {
    fn resolve_library(&self, name: &str) -> Option<String> {
        let api_key = self.api_key.as_ref()?;

        // reqwest::blocking can't run inside tokio async runtime directly — use block_in_place
        let resp = match tokio::task::block_in_place(|| {
            self.http
                .get("https://context7.com/api/v1/search")
                .query(&[("libraryName", name), ("query", name)])
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
        }) {
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

        let json: serde_json::Value = match resp.json() {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Context7 resolve_library: JSON parse error: {}", e);
                return None;
            }
        };
        let id = json["results"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        id
    }

    fn query_docs(&self, library_id: &str, query: &str, max_tokens: usize) -> Option<String> {
        let api_key = self.api_key.as_ref()?;

        let lib_id = library_id.to_string();
        let q = query.to_string();
        let tok = max_tokens.to_string();
        let key = api_key.clone();
        let resp = match tokio::task::block_in_place(|| {
            self.http
                .get(format!("https://context7.com/api/v1{}", lib_id))
                .query(&[("query", &q), ("tokens", &tok)])
                .header("Authorization", format!("Bearer {}", key))
                .send()
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Context7 query_docs error: {}", e);
                return None;
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            eprintln!("Context7 query_docs: HTTP {} - {}", status, &body[..200.min(body.len())]);
            return None;
        }

        let body_text = match resp.text() {
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
        // Try JSON first, fall back to plain text
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

        // Plain text/markdown response — use directly, truncate to max_tokens chars
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
        let docs = fetch_framework_docs(&["react".into()], &FakeFetcher);
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
        let docs = fetch_framework_docs(&["react".into()], &NullFetcher);
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
}
