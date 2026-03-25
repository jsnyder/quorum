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

/// Real Context7 fetcher - calls Context7 MCP tools via subprocess.
/// In quorum's MCP server mode, Context7 is a sibling MCP server.
/// For CLI mode, we call it via subprocess.
pub struct Context7SubprocessFetcher;

impl ContextFetcher for Context7SubprocessFetcher {
    fn resolve_library(&self, _name: &str) -> Option<String> {
        // Shell out to: context7 resolve-library-id --name <name>
        // Or use the MCP protocol directly
        // For now, return None (graceful degradation)
        // Real implementation will come when we wire MCP-to-MCP
        None
    }

    fn query_docs(&self, _library_id: &str, _query: &str, _max_tokens: usize) -> Option<String> {
        None
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
