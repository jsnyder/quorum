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
/// Per-review counters tracking Context7 enrichment outcomes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentMetrics {
    pub context7_resolved: u32,
    pub context7_resolve_failed: u32,
    pub context7_query_failed: u32,
}

/// Result of enrich_for_review: docs to splice into the prompt + telemetry counters.
#[derive(Debug, Default)]
pub struct EnrichmentResult {
    pub docs: Vec<ContextDoc>,
    pub metrics: EnrichmentMetrics,
}

/// Normalize an import target to the dep name(s) it could match.
///
/// Handles two input shapes:
///
/// 1. **Production hydration form** (`hydration::populate_imports`):
///    `"{imported_symbol}: {full_statement}"`. Strip the `"{symbol}: "` prefix
///    and parse the statement:
///    - Rust `"Mutex: use tokio::sync::Mutex;"` -> `["tokio"]`
///    - Python `"join: from os.path import join"` -> `["os"]`
///    - Python `"sys: import sys"` -> `["sys"]`
///    - TS `"useState: import { useState } from 'react'"` -> `["react"]`
///    - TS scoped `"M: import { M } from '@nestjs/core'"` -> `["@nestjs/core"]`
///    - TS side-effect `"reflect-metadata: import 'reflect-metadata'"` -> `["reflect-metadata"]`
///
/// 2. **Clean module-path form** (used by tests and other callers):
///    - Rust `tokio::sync::Mutex` -> `["tokio"]`
///    - Python `fastapi.routing` -> `["fastapi"]`
///    - JS `react` -> `["react"]`
///    - JS scoped `@nestjs/core` -> `["@nestjs/core"]`
///    - JS scoped deep `@nestjs/common/decorators` -> `["@nestjs/common"]`
///    - Bare `@foo` -> `["@foo"]`
///    - Leading `::std::ptr` -> `["std"]` (skips empty heads)
pub(crate) fn normalize_import_to_dep_names(imp: &str) -> Vec<String> {
    // Production hydration form: "{symbol}: {statement}". Detection requires
    // both ": " and a recognized verb (`use`, `from`, `import`) — a clean
    // path like `tokio::sync::Mutex` has no ": " (no space) and falls through.
    if let Some((_symbol, rest)) = imp.split_once(": ") {
        let rest = rest.trim_start();
        if let Some(stmt) = strip_verb(rest, "use") {
            return parse_rust_use(stmt);
        }
        if let Some(stmt) = strip_verb(rest, "from") {
            return parse_python_from(stmt);
        }
        if let Some(stmt) = strip_verb(rest, "import") {
            // TS forms always carry a quoted source (`from '<pkg>'` or bare
            // `'<pkg>'` for side-effect imports). Python `import X.Y` does not.
            if let Some(pkg) = extract_quoted_source(stmt) {
                return normalize_ts_package(&pkg);
            }
            return parse_python_import(stmt);
        }
    }
    normalize_clean(imp)
}

/// Return body after `verb` if `rest` starts with `verb` followed by EOS or whitespace.
/// Distinguishes a degenerate empty body (`"import "` -> `Some("")`) from a similar-looking
/// identifier (`"importable"` -> `None`), so detection doesn't fall through to
/// `normalize_clean` on a malformed-but-recognized statement.
fn strip_verb<'a>(rest: &'a str, verb: &str) -> Option<&'a str> {
    let tail = rest.strip_prefix(verb)?;
    if tail.is_empty() { return Some(""); }
    let first = tail.chars().next()?;
    if first.is_whitespace() {
        Some(tail.trim_start())
    } else {
        None
    }
}

fn parse_rust_use(stmt: &str) -> Vec<String> {
    let stmt = stmt.trim().trim_end_matches(';').trim();

    // Outer-grouped form: `use {crate_a::A, crate_b::B};`. Each comma-separated
    // member contributes its own crate. Without this, `split("::")` returns
    // `"{crate_a"` and the whole import silently misses dep enrichment (CR6).
    if let Some(inner) = stmt.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        return inner
            .split(',')
            .filter_map(|seg| first_use_segment(seg.trim()))
            .collect();
    }

    // Standard form: `use crate::sub::Item;`, `use crate::{A, B};`, or
    // `use ::crate::Item;`. Skip empty leading segments so absolute paths
    // resolve to their crate (CR3).
    first_use_segment(stmt).map(|s| vec![s]).unwrap_or_default()
}

fn first_use_segment(s: &str) -> Option<String> {
    let head = s.split("::").find(|seg| !seg.is_empty())?.trim();
    (!head.is_empty()).then(|| head.to_string())
}

fn parse_python_from(stmt: &str) -> Vec<String> {
    // stmt = "os.path import join"
    let module = stmt.split_whitespace().next().unwrap_or("");
    let head = module.split('.').next().unwrap_or("");
    if head.is_empty() { return Vec::new(); }
    vec![head.to_string()]
}

fn parse_python_import(stmt: &str) -> Vec<String> {
    // stmt is the body after "import ": "sys", "os.path as p",
    // "os, sys, json", "os.path, urllib.parse as up", possibly with a
    // trailing inline comment ("os  # bootstrap") or trailing comma.
    // For each comma-separated segment: drop `as <alias>`, collapse
    // dotted submodule to root, skip empties.
    let body = stmt.split('#').next().unwrap_or(stmt);
    let mut out = Vec::new();
    for seg in body.split(',') {
        let token = seg.split_whitespace().next().unwrap_or("");
        let head = token.split('.').next().unwrap_or("");
        if !head.is_empty() {
            out.push(head.to_string());
        }
    }
    out
}

fn extract_quoted_source(stmt: &str) -> Option<String> {
    for delim in ['\'', '"'] {
        if let Some(start) = stmt.find(delim) {
            let after = &stmt[start + 1..];
            if let Some(end) = after.find(delim) {
                return Some(after[..end].to_string());
            }
        }
    }
    None
}

fn normalize_ts_package(pkg: &str) -> Vec<String> {
    if let Some(stripped) = pkg.strip_prefix('@') {
        let parts: Vec<&str> = stripped.splitn(3, '/').collect();
        if parts.len() >= 2 {
            return vec![format!("@{}/{}", parts[0], parts[1])];
        }
        return vec![pkg.to_string()];
    }
    let head = pkg.split('/').next().unwrap_or(pkg);
    if head.is_empty() { return Vec::new(); }
    vec![head.to_string()]
}

fn normalize_clean(imp: &str) -> Vec<String> {
    if let Some(stripped) = imp.strip_prefix('@') {
        let parts: Vec<&str> = stripped.splitn(3, '/').collect();
        if parts.len() >= 2 {
            return vec![format!("@{}/{}", parts[0], parts[1])];
        }
        return vec![imp.to_string()];
    }
    let head = imp
        .split(&['.', '/', ':'][..])
        .find(|s| !s.is_empty())
        .unwrap_or(imp)
        .to_string();
    vec![head]
}

const ENRICH_K: usize = 5;

/// Orchestrator: parse-aware Context7 enrichment.
///
/// 1. Filter `deps` to those whose name matches an import in `imports` (in import order).
/// 2. Cap at K=5 (drops tail in import order, not random).
/// 3. For each kept dep, use curated query if available, else language-aware generic.
/// 4. Then add directory-detected `curated_frameworks` (HA/ESPHome) — additive, deduped.
pub fn enrich_for_review(
    deps: &[crate::dep_manifest::Dependency],
    curated_frameworks: &[String],
    imports: &[String],
    fetcher: &dyn ContextFetcher,
) -> EnrichmentResult {
    let mut metrics = EnrichmentMetrics::default();
    let mut docs: Vec<ContextDoc> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut import_matched: Vec<&crate::dep_manifest::Dependency> = Vec::new();
    for imp in imports {
        for name in normalize_import_to_dep_names(imp) {
            if let Some(dep) = deps.iter().find(|d| d.name == name) {
                if !import_matched.iter().any(|d| d.name == dep.name) {
                    import_matched.push(dep);
                }
            }
        }
    }

    for dep in import_matched.into_iter().take(ENRICH_K) {
        if seen.contains(&dep.name) { continue; }
        let query = curated_query_for(&dep.name)
            .unwrap_or_else(|| generic_query_for_language(&dep.language).into());
        try_fetch_one(&dep.name, &query, imports, fetcher, &mut docs, &mut metrics, &mut seen);
    }

    for fw in curated_frameworks {
        if seen.contains(fw) { continue; }
        if let Some(query) = curated_query_for(fw) {
            try_fetch_one(fw, &query, imports, fetcher, &mut docs, &mut metrics, &mut seen);
        }
    }

    EnrichmentResult { docs, metrics }
}

/// Convenience wrapper: parse the project's manifests, then run enrich_for_review.
/// This is the main public entry point used by pipeline.rs.
pub fn enrich_for_review_in_project(
    project_root: &std::path::Path,
    imports: &[String],
    curated_frameworks: &[String],
    fetcher: &dyn ContextFetcher,
) -> EnrichmentResult {
    let deps = crate::dep_manifest::parse_dependencies(project_root);
    enrich_for_review(&deps, curated_frameworks, imports, fetcher)
}

fn try_fetch_one(
    name: &str,
    query: &str,
    imports: &[String],
    fetcher: &dyn ContextFetcher,
    docs: &mut Vec<ContextDoc>,
    metrics: &mut EnrichmentMetrics,
    seen: &mut std::collections::HashSet<String>,
) {
    // Insert into `seen` unconditionally so a name attempted in the import-matched
    // loop isn't re-attempted in the curated-frameworks loop. Without this, a
    // resolve-success/query-failure pair would double-count `context7_query_failed`
    // (and a resolve-failure would double-count `context7_resolve_failed`).
    seen.insert(name.into());
    match fetcher.resolve_library(name) {
        Some(lib_id) => {
            metrics.context7_resolved += 1;
            let enriched = build_code_aware_query(query, imports);
            if let Some(content) = fetcher.query_docs(&lib_id, &enriched, 5000) {
                docs.push(ContextDoc { library: name.into(), content });
            } else {
                metrics.context7_query_failed += 1;
            }
        }
        None => { metrics.context7_resolve_failed += 1; }
    }
}

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
    let keywords: Vec<String> = import_targets.iter()
        .filter_map(|imp| extract_query_keyword(imp))
        .filter(|s| s.len() > 2) // skip very short names like "os", "re"
        .take(10)
        .collect();
    if keywords.is_empty() {
        return base_query.to_string();
    }
    format!("{} {}", base_query, keywords.join(" "))
}

/// Extract one Context7-query keyword from an import target.
///
/// Two input shapes (matches `normalize_import_to_dep_names`):
///   1. Production hydration `"{symbol}: {use|from|import ...}"`: the
///      imported symbol IS the keyword (`Mutex`, `useState`, `Deserialize`).
///      The statement body is a syntactic carrier, not a search term —
///      treating it as one (the old behavior) leaks `"Mutex;"`, `"react'"`,
///      and full sub-phrases into the query.
///   2. Clean form: scoped `@scope/pkg` -> `scope` (framework hint),
///      else trailing path segment (`os.path.join` -> `join`).
fn extract_query_keyword(imp: &str) -> Option<String> {
    if let Some((symbol, rest)) = imp.split_once(": ") {
        let rest_trim = rest.trim();
        if rest_trim.starts_with("use ")
            || rest_trim.starts_with("from ")
            || rest_trim.starts_with("import ")
        {
            let symbol = symbol.trim();
            if !symbol.is_empty() { return Some(symbol.to_string()); }
        }
    }
    if let Some(stripped) = imp.strip_prefix('@') {
        return stripped.split('/').next().map(|s| s.to_string());
    }
    imp.split(&['.', '/', ':'][..]).next_back().map(|s| s.to_string())
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
        // `text` fence language: heterogeneous content (HCL, YAML, prose, Rust);
        // explicit `text` keeps Markdown renderers from highlighting as Bash by
        // default and makes the intent ("opaque blob, do not parse") explicit (N2).
        let sanitized = doc.content.replace("```", "'''");
        section.push_str(&format!("### {}\n```text\n{}\n```\n\n", doc.library, sanitized));
    }
    section
}

/// Cache entry for resolve_library results, with TTL gating.
struct ResolveCacheEntry {
    result: Option<String>,
    cached_at: std::time::Instant,
}

type Clock = Box<dyn Fn() -> std::time::Instant + Send + Sync>;

const DEFAULT_RESOLVE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Caching wrapper around a ContextFetcher.
/// - query_docs results cached by (library_id, query, max_tokens).
/// - resolve_library results cached by name with a TTL (default 24h).
///   Negative results (None) are cached too — avoids re-hammering Context7
///   for known-missing names (private crates, typos).
pub struct CachedContextFetcher<'a> {
    inner: &'a dyn ContextFetcher,
    query_cache: std::sync::Mutex<lru::LruCache<(String, String, usize), Option<String>>>,
    resolve_cache: std::sync::Mutex<lru::LruCache<String, ResolveCacheEntry>>,
    resolve_ttl: std::time::Duration,
    now: Clock,
}

impl<'a> CachedContextFetcher<'a> {
    pub fn new(inner: &'a dyn ContextFetcher, max_entries: usize) -> Self {
        Self::new_with_clock(inner, max_entries, DEFAULT_RESOLVE_TTL, std::time::Instant::now)
    }

    pub fn new_with_clock(
        inner: &'a dyn ContextFetcher,
        max_entries: usize,
        resolve_ttl: std::time::Duration,
        now: impl Fn() -> std::time::Instant + Send + Sync + 'static,
    ) -> Self {
        // LruCache requires a non-zero capacity. Floor at 1 so callers can't
        // accidentally create a zero-capacity cache that silently fails to cache.
        let cap = std::num::NonZeroUsize::new(max_entries.max(1))
            .expect("max_entries.max(1) is non-zero");
        Self {
            inner,
            query_cache: std::sync::Mutex::new(lru::LruCache::new(cap)),
            resolve_cache: std::sync::Mutex::new(lru::LruCache::new(cap)),
            resolve_ttl,
            now: Box::new(now),
        }
    }
}

impl<'a> ContextFetcher for CachedContextFetcher<'a> {
    fn resolve_library(&self, name: &str) -> Option<String> {
        let now = (self.now)();
        if let Ok(mut cache) = self.resolve_cache.lock() {
            if let Some(entry) = cache.get(name) {
                if now.duration_since(entry.cached_at) < self.resolve_ttl {
                    return entry.result.clone();
                }
            }
        }
        let result = self.inner.resolve_library(name);
        if let Ok(mut cache) = self.resolve_cache.lock() {
            // LruCache::put auto-evicts the LRU entry at capacity — preserves
            // hot entries instead of dropping the entire cache like the prior
            // `clear()` did.
            cache.put(name.to_string(), ResolveCacheEntry {
                result: result.clone(),
                cached_at: now,
            });
        }
        result
    }

    fn query_docs(&self, library_id: &str, query: &str, max_tokens: usize) -> Option<String> {
        let key = (library_id.to_string(), query.to_string(), max_tokens);

        if let Ok(mut cache) = self.query_cache.lock() {
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        let result = self.inner.query_docs(library_id, query, max_tokens);

        if let Ok(mut cache) = self.query_cache.lock() {
            cache.put(key, result.clone());
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
mod test_support {
    use super::*;
    use std::sync::Mutex;

    pub struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(format!("/lib/{name}")) }
        fn query_docs(&self, lib: &str, _: &str, _: usize) -> Option<String> {
            Some(format!("docs for {lib}"))
        }
    }

    pub struct CapturingSpy { pub queries: Mutex<Vec<(String, String)>> }
    impl CapturingSpy {
        pub fn new() -> Self { Self { queries: Mutex::new(Vec::new()) } }
    }
    impl ContextFetcher for CapturingSpy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
        fn query_docs(&self, lib: &str, query: &str, _: usize) -> Option<String> {
            self.queries.lock().unwrap().push((lib.into(), query.into()));
            Some("doc".into())
        }
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

    // --- normalize_import_to_dep_names: direct unit tests ---

    #[test]
    fn normalize_bare_import_returns_root() {
        assert_eq!(normalize_import_to_dep_names("tokio"), vec!["tokio"]);
    }

    #[test]
    fn normalize_module_path_returns_root_segment() {
        assert_eq!(normalize_import_to_dep_names("tokio::sync::Mutex"), vec!["tokio"]);
        assert_eq!(normalize_import_to_dep_names("fastapi.routing"), vec!["fastapi"]);
    }

    #[test]
    fn normalize_local_paths_yield_keyword_that_wont_match_real_deps() {
        // crate::foo / super::foo / self::foo: yield "crate"/"super"/"self".
        // These won't appear in any real Cargo.toml so import-set lookup misses harmlessly.
        // Pin this so a future "filter locals" change doesn't accidentally match a "crate" dep.
        assert_eq!(normalize_import_to_dep_names("crate::foo"), vec!["crate"]);
        assert_eq!(normalize_import_to_dep_names("super::foo"), vec!["super"]);
        assert_eq!(normalize_import_to_dep_names("self::foo"), vec!["self"]);
    }

    #[test]
    fn normalize_leading_colon_does_not_yield_empty_string() {
        let out = normalize_import_to_dep_names("::std::ptr");
        assert!(out.iter().all(|s| !s.is_empty()),
            "leading :: must not yield empty head: {out:?}");
    }

    #[test]
    fn normalize_scoped_pkg_with_subpath_keeps_first_two_segments() {
        assert_eq!(
            normalize_import_to_dep_names("@nestjs/common/decorators"),
            vec!["@nestjs/common"]
        );
    }

    #[test]
    fn normalize_scoped_pkg_without_slash_kept_verbatim() {
        assert_eq!(normalize_import_to_dep_names("@foo"), vec!["@foo"]);
    }

    // --- normalize: production hydration format ---
    //
    // `hydration::populate_imports` emits `import_targets` as
    // `"{imported_name}: {full_use_statement}"` (e.g. `"Mutex: use tokio::sync::Mutex;"`).
    // Earlier tests above cover the *clean* form. These pin the *real* form
    // so the dep-based enrichment path doesn't silently no-op in production.

    #[test]
    fn normalize_rust_hydration_form_extracts_crate_name() {
        assert_eq!(
            normalize_import_to_dep_names("Mutex: use tokio::sync::Mutex;"),
            vec!["tokio"]
        );
        assert_eq!(
            normalize_import_to_dep_names("Deserialize: use serde::{Deserialize, Serialize};"),
            vec!["serde"]
        );
        assert_eq!(
            normalize_import_to_dep_names("Result: use anyhow::Result;"),
            vec!["anyhow"]
        );
    }

    #[test]
    fn normalize_rust_hydration_form_handles_absolute_paths() {
        // CR3: `use ::tokio::sync::Mutex;` (absolute path) used to return no
        // dep because split("::").next() yielded an empty leading segment.
        // Skip empties so absolute paths work like relative ones.
        assert_eq!(
            normalize_import_to_dep_names("Mutex: use ::tokio::sync::Mutex;"),
            vec!["tokio"]
        );
        assert_eq!(
            normalize_import_to_dep_names("HashMap: use ::std::collections::HashMap;"),
            vec!["std"]
        );
    }

    #[test]
    fn normalize_rust_hydration_form_skips_local_use_paths() {
        // `use crate::foo;` / `use self::bar;` / `use super::baz;` should not
        // resolve to a Cargo.toml dep — emit the keyword so downstream lookup
        // misses harmlessly (matches existing local-path contract).
        assert_eq!(
            normalize_import_to_dep_names("foo: use crate::foo;"),
            vec!["crate"]
        );
        assert_eq!(
            normalize_import_to_dep_names("bar: use self::bar;"),
            vec!["self"]
        );
        assert_eq!(
            normalize_import_to_dep_names("baz: use super::baz;"),
            vec!["super"]
        );
    }

    #[test]
    fn normalize_rust_hydration_form_extracts_all_crates_from_outer_grouped_use() {
        // CR6: outer-grouped `use {tokio::sync::Mutex, serde::Serialize};`
        // used to return `["{tokio"]` because parse_rust_use only handled the
        // inner-group form (`use serde::{A, B}`). Production hydration emits
        // one entry per imported symbol, so callers see N copies of the same
        // statement; each must extract every crate so dep-name lookup matches
        // any of them.
        let mut got = normalize_import_to_dep_names(
            "Mutex: use {tokio::sync::Mutex, serde::Serialize};",
        );
        got.sort();
        assert_eq!(got, vec!["serde", "tokio"]);
    }

    #[test]
    fn normalize_rust_hydration_form_outer_grouped_use_with_absolute_paths() {
        // Same fix applied per-segment: empty leading segments inside the
        // group must be skipped, so `use {::tokio::A, ::serde::B};` yields
        // both crates, not `["{"]`.
        let mut got = normalize_import_to_dep_names(
            "A: use {::tokio::A, ::serde::B};",
        );
        got.sort();
        assert_eq!(got, vec!["serde", "tokio"]);
    }

    #[test]
    fn normalize_rust_hydration_form_outer_grouped_use_with_single_member() {
        // Degenerate single-member group still works.
        assert_eq!(
            normalize_import_to_dep_names("Mutex: use {tokio::sync::Mutex};"),
            vec!["tokio"]
        );
    }

    #[test]
    fn normalize_python_hydration_form_extracts_module_root() {
        // `from os.path import join` -> root `os`
        assert_eq!(
            normalize_import_to_dep_names("join: from os.path import join"),
            vec!["os"]
        );
        // `import sys` -> `sys`
        assert_eq!(
            normalize_import_to_dep_names("sys: import sys"),
            vec!["sys"]
        );
        // `from fastapi import FastAPI` -> `fastapi`
        assert_eq!(
            normalize_import_to_dep_names("FastAPI: from fastapi import FastAPI"),
            vec!["fastapi"]
        );
    }

    // --- Bug 1: parse_python_import dropped trailing modules ---
    // Tests assert through the production hydrated form (not the bare helper)
    // because the original-sin antipattern from issue #29 was tests using a
    // synthetic input shape that didn't match what hydration emits.

    #[test]
    fn parse_python_import_returns_all_modules_in_comma_list() {
        assert_eq!(
            normalize_import_to_dep_names("join: import os, sys, json"),
            vec!["os", "sys", "json"]
        );
    }

    #[test]
    fn parse_python_import_strips_as_alias() {
        // `import sys as s` — alias is a local rename, the module is `sys`.
        assert_eq!(
            normalize_import_to_dep_names("s: import sys as s"),
            vec!["sys"]
        );
    }

    #[test]
    fn parse_python_import_dotted_returns_root_only_per_module() {
        // Dotted submodule collapses to root; alias must not leak as a fake module.
        assert_eq!(
            normalize_import_to_dep_names("path: import os.path, urllib.parse as up"),
            vec!["os", "urllib"]
        );
    }

    #[test]
    fn parse_python_import_skips_empty_segments_and_whitespace() {
        // Trailing comma + irregular whitespace must not yield empty heads.
        assert_eq!(
            normalize_import_to_dep_names("x: import   os  ,  sys ,"),
            vec!["os", "sys"]
        );
    }

    #[test]
    fn parse_python_import_strips_trailing_inline_comment() {
        // "import os  # bootstrap" -> ["os"], not ["os  # bootstrap"].
        assert_eq!(
            normalize_import_to_dep_names("os: import os  # bootstrap"),
            vec!["os"]
        );
    }

    #[test]
    fn parse_python_import_empty_body_returns_empty() {
        // No panic, no spurious empty-string head.
        assert_eq!(
            normalize_import_to_dep_names("nothing: import "),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parse_python_from_relative_import_returns_empty() {
        // `from . import x` / `from .. import y` are package-relative imports;
        // the "module" is a dot, not a real dep. Must not yield "" or ".".
        assert_eq!(
            normalize_import_to_dep_names("x: from . import x"),
            Vec::<String>::new()
        );
        assert_eq!(
            normalize_import_to_dep_names("y: from .. import y"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parse_python_import_does_not_break_ts_quoted_source_branch() {
        // Antipatterns reviewer MUST-FIX 1.2: a naive comma-split before the
        // quoted-source check would mangle `import 'reflect-metadata'` (which
        // has no `from` clause). The TS branch must stay exclusive.
        assert_eq!(
            normalize_import_to_dep_names("reflect-metadata: import 'reflect-metadata'"),
            vec!["reflect-metadata"]
        );
    }

    #[test]
    fn normalize_typescript_hydration_form_extracts_package_from_quoted_source() {
        // Named import: `import { useState } from 'react'` -> `react`
        assert_eq!(
            normalize_import_to_dep_names("useState: import { useState } from 'react'"),
            vec!["react"]
        );
        // Double quotes
        assert_eq!(
            normalize_import_to_dep_names("z: import { z } from \"zod\""),
            vec!["zod"]
        );
        // Scoped npm package: keep `@scope/name`
        assert_eq!(
            normalize_import_to_dep_names("Module: import { Module } from '@nestjs/core'"),
            vec!["@nestjs/core"]
        );
        // Scoped with subpath: trim to first two segments
        assert_eq!(
            normalize_import_to_dep_names("x: import { x } from '@nestjs/common/decorators'"),
            vec!["@nestjs/common"]
        );
        // Default import: `import express from 'express'` -> `express`
        assert_eq!(
            normalize_import_to_dep_names("express: import express from 'express'"),
            vec!["express"]
        );
        // Side-effect: `import 'reflect-metadata'`
        assert_eq!(
            normalize_import_to_dep_names("reflect-metadata: import 'reflect-metadata'"),
            vec!["reflect-metadata"]
        );
    }

    #[test]
    fn normalize_clean_paths_still_work_after_hydration_aware_parsing() {
        // Regression guard: the existing clean-form contract must keep working.
        // A future change that breaks bare `tokio` / `tokio::sync::Mutex` would
        // also break the test fixtures used by other modules.
        assert_eq!(normalize_import_to_dep_names("tokio"), vec!["tokio"]);
        assert_eq!(normalize_import_to_dep_names("tokio::sync::Mutex"), vec!["tokio"]);
        assert_eq!(normalize_import_to_dep_names("fastapi.routing"), vec!["fastapi"]);
    }

    // --- enrich_for_review: behavior tests ---

    #[test]
    fn enrich_skips_deps_not_in_imports() {
        use crate::dep_manifest::Dependency;
        use test_support::Spy;
        let deps = vec![
            Dependency { name: "tokio".into(), language: "rust".into() },
            Dependency { name: "serde".into(), language: "rust".into() },
            Dependency { name: "axum".into(), language: "rust".into() },
        ];
        let imports = vec!["tokio::sync::Mutex".into(), "serde::Serialize".into()];
        let result = enrich_for_review(&deps, &[], &imports, &Spy);
        let libs: Vec<_> = result.docs.iter().map(|d| d.library.as_str()).collect();
        assert!(libs.contains(&"tokio"));
        assert!(libs.contains(&"serde"));
        assert!(!libs.contains(&"axum"), "axum not in imports — must be skipped");
    }

    #[test]
    fn enrich_uses_curated_query_when_available() {
        use crate::dep_manifest::Dependency;
        use test_support::CapturingSpy;
        let spy = CapturingSpy::new();
        let deps = vec![Dependency { name: "react".into(), language: "javascript".into() }];
        let imports = vec!["react".into()];
        let _ = enrich_for_review(&deps, &[], &imports, &spy);
        let captured = spy.queries.lock().unwrap();
        assert!(captured.iter().any(|(_, q)| q.contains("hooks")),
            "curated query expected, got {captured:?}");
    }

    #[test]
    fn enrich_uses_generic_query_when_no_curated_match() {
        use crate::dep_manifest::Dependency;
        use test_support::CapturingSpy;
        let spy = CapturingSpy::new();
        let deps = vec![Dependency { name: "tokio".into(), language: "rust".into() }];
        let imports = vec!["tokio::spawn".into()];
        let _ = enrich_for_review(&deps, &[], &imports, &spy);
        let captured = spy.queries.lock().unwrap();
        assert!(captured.iter().any(|(_, q)| q.contains("async")),
            "rust generic query expected, got {captured:?}");
    }

    #[test]
    fn enrich_for_review_with_empty_inputs_returns_no_docs_and_zero_metrics() {
        struct Spy;
        impl ContextFetcher for Spy {
            fn resolve_library(&self, _: &str) -> Option<String> { None }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> { None }
        }
        let result = enrich_for_review(&[], &[], &[], &Spy);
        assert!(result.docs.is_empty());
        assert_eq!(result.metrics.context7_resolved, 0);
        assert_eq!(result.metrics.context7_resolve_failed, 0);
        assert_eq!(result.metrics.context7_query_failed, 0);
    }

    #[test]
    fn enrich_with_exactly_five_matched_deps_returns_five_docs() {
        use crate::dep_manifest::Dependency;
        use test_support::Spy;
        let deps: Vec<_> = (0..5).map(|i| Dependency {
            name: format!("dep{i}"), language: "rust".into(),
        }).collect();
        let imports: Vec<_> = (0..5).map(|i| format!("dep{i}::x")).collect();
        let result = enrich_for_review(&deps, &[], &imports, &Spy);
        assert_eq!(result.docs.len(), 5);
    }

    #[test]
    fn enrich_with_six_matched_drops_the_last_in_import_order() {
        use crate::dep_manifest::Dependency;
        use test_support::Spy;
        let deps: Vec<_> = (0..6).map(|i| Dependency {
            name: format!("dep{i}"), language: "rust".into(),
        }).collect();
        let imports: Vec<_> = (0..6).map(|i| format!("dep{i}::x")).collect();
        let result = enrich_for_review(&deps, &[], &imports, &Spy);
        let libs: Vec<_> = result.docs.iter().map(|d| d.library.clone()).collect();
        assert_eq!(libs.len(), 5);
        assert!(!libs.contains(&"dep5".to_string()),
            "dep5 should be dropped; got {libs:?}");
    }

    #[test]
    fn enrich_returns_first_five_in_import_occurrence_order() {
        use crate::dep_manifest::Dependency;
        use test_support::Spy;
        let deps: Vec<_> = (0..10).map(|i| Dependency {
            name: format!("dep{i}"), language: "rust".into(),
        }).collect();
        let imports: Vec<_> = (0..10).map(|i| format!("dep{i}::x")).collect();
        let result = enrich_for_review(&deps, &[], &imports, &Spy);
        let libs: Vec<_> = result.docs.iter().map(|d| d.library.clone()).collect();
        assert_eq!(libs, vec!["dep0", "dep1", "dep2", "dep3", "dep4"],
            "must be import-order, not HashMap iteration order");
    }

    #[test]
    fn enrich_dedupes_curated_framework_already_in_deps() {
        use crate::dep_manifest::Dependency;
        use test_support::Spy;
        let deps = vec![Dependency { name: "react".into(), language: "javascript".into() }];
        let imports = vec!["react".into()];
        let frameworks = vec!["react".into()];
        let result = enrich_for_review(&deps, &frameworks, &imports, &Spy);
        let count = result.docs.iter().filter(|d| d.library == "react").count();
        assert_eq!(count, 1, "react must appear exactly once");
    }

    #[test]
    fn enrich_ha_framework_path_runs_without_manifest_match() {
        use test_support::Spy;
        let frameworks = vec!["home-assistant".into()];
        let result = enrich_for_review(&[], &frameworks, &[], &Spy);
        assert!(result.docs.iter().any(|d| d.library == "home-assistant"));
    }

    #[test]
    fn enrich_for_review_in_project_parses_cargo_and_filters_by_imports() {
        // Integration test: manifest parsing + enrich orchestration end-to-end.
        // Given a tempdir with Cargo.toml [dependencies] tokio + serde + axum,
        // and imports referencing tokio + serde only, the spy fetcher must be
        // asked to resolve only tokio and serde — NOT axum.
        use tempfile::TempDir;
        use test_support::CapturingSpy;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
tokio = "1"
serde = "1"
axum = "0.7"
"#).unwrap();

        let spy = CapturingSpy::new();
        let imports = vec!["tokio::sync::Mutex".into(), "serde::Serialize".into()];
        let result = enrich_for_review_in_project(dir.path(), &imports, &[], &spy);

        let libs: Vec<_> = result.docs.iter().map(|d| d.library.clone()).collect();
        assert!(libs.contains(&"tokio".to_string()));
        assert!(libs.contains(&"serde".to_string()));
        assert!(!libs.contains(&"axum".to_string()), "axum not in imports — must be skipped");

        // Telemetry: 2 deps were import-matched and resolved.
        assert_eq!(result.metrics.context7_resolved, 2);
        assert_eq!(result.metrics.context7_resolve_failed, 0);
        assert_eq!(result.metrics.context7_query_failed, 0);
    }

    #[test]
    fn cached_fetcher_negative_resolve_result_is_cached() {
        use std::sync::Mutex;
        struct CountingSpy { calls: Mutex<u32> }
        impl ContextFetcher for CountingSpy {
            fn resolve_library(&self, _: &str) -> Option<String> {
                *self.calls.lock().unwrap() += 1;
                None
            }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> { None }
        }
        let inner = CountingSpy { calls: Mutex::new(0) };
        let cached = CachedContextFetcher::new(&inner, 16);
        assert!(cached.resolve_library("missing").is_none());
        assert!(cached.resolve_library("missing").is_none());
        assert!(cached.resolve_library("missing").is_none());
        assert_eq!(*inner.calls.lock().unwrap(), 1,
            "subsequent calls must hit negative cache");
    }

    #[test]
    fn cached_fetcher_positive_resolve_result_is_cached() {
        use std::sync::Mutex;
        struct CountingSpy { calls: Mutex<u32> }
        impl ContextFetcher for CountingSpy {
            fn resolve_library(&self, name: &str) -> Option<String> {
                *self.calls.lock().unwrap() += 1;
                Some(format!("/lib/{name}"))
            }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> { None }
        }
        let inner = CountingSpy { calls: Mutex::new(0) };
        let cached = CachedContextFetcher::new(&inner, 16);
        assert_eq!(cached.resolve_library("react"), Some("/lib/react".into()));
        assert_eq!(cached.resolve_library("react"), Some("/lib/react".into()));
        assert_eq!(*inner.calls.lock().unwrap(), 1);
    }

    #[test]
    fn cached_fetcher_negative_resolve_cache_expires_after_ttl() {
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        struct CountingSpy { calls: Mutex<u32> }
        impl ContextFetcher for CountingSpy {
            fn resolve_library(&self, _: &str) -> Option<String> {
                *self.calls.lock().unwrap() += 1;
                None
            }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> { None }
        }
        let inner = CountingSpy { calls: Mutex::new(0) };
        let now = Instant::now();
        let time = Arc::new(Mutex::new(now));
        let time_clone = time.clone();
        let cached = CachedContextFetcher::new_with_clock(
            &inner,
            16,
            Duration::from_secs(60),
            move || *time_clone.lock().unwrap(),
        );
        let _ = cached.resolve_library("missing");
        *time.lock().unwrap() = now + Duration::from_secs(120);
        let _ = cached.resolve_library("missing");
        assert_eq!(*inner.calls.lock().unwrap(), 2,
            "expired entry must trigger fresh inner call");
    }

    #[test]
    fn enrich_does_not_double_count_when_dep_appears_in_both_paths_and_query_fails() {
        // Regression test: if a name is in both deps (import-matched) AND
        // curated_frameworks, AND resolve succeeds but query fails, the curated
        // loop must NOT retry — telemetry counters must be incremented once each.
        use crate::dep_manifest::Dependency;
        struct ResolveOkButQueryFails;
        impl ContextFetcher for ResolveOkButQueryFails {
            fn resolve_library(&self, name: &str) -> Option<String> { Some(format!("/lib/{name}")) }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> { None }
        }
        let deps = vec![Dependency { name: "react".into(), language: "javascript".into() }];
        let imports = vec!["react".into()];
        let frameworks = vec!["react".into()];
        let result = enrich_for_review(&deps, &frameworks, &imports, &ResolveOkButQueryFails);
        assert_eq!(result.metrics.context7_resolved, 1,
            "resolve must not double-count");
        assert_eq!(result.metrics.context7_query_failed, 1,
            "query_failed must not double-count");
    }

    #[test]
    fn enrich_telemetry_counts_resolves_resolve_fails_and_query_fails_separately() {
        use crate::dep_manifest::Dependency;
        struct PartialSpy;
        impl ContextFetcher for PartialSpy {
            fn resolve_library(&self, name: &str) -> Option<String> {
                if name == "good" { Some("/lib/good".into()) }
                else if name == "query_fails" { Some("/lib/qf".into()) }
                else { None }
            }
            fn query_docs(&self, lib: &str, _: &str, _: usize) -> Option<String> {
                if lib == "/lib/good" { Some("doc".into()) } else { None }
            }
        }
        let deps = vec![
            Dependency { name: "good".into(), language: "rust".into() },
            Dependency { name: "missing".into(), language: "rust".into() },
            Dependency { name: "query_fails".into(), language: "rust".into() },
        ];
        let imports = vec!["good".into(), "missing".into(), "query_fails".into()];
        let result = enrich_for_review(&deps, &[], &imports, &PartialSpy);
        assert_eq!(result.metrics.context7_resolved, 2);
        assert_eq!(result.metrics.context7_resolve_failed, 1);
        assert_eq!(result.metrics.context7_query_failed, 1);
    }

    #[test]
    fn build_code_aware_query_extracts_scope_for_scoped_packages() {
        // @nestjs/core should yield "nestjs" (the framework hint), not "core" (useless).
        let query = build_code_aware_query("base", &["@nestjs/core".into()]);
        assert!(query.contains("nestjs"), "got: {query}");
        assert!(!query.split_whitespace().any(|w| w == "core"), "got: {query}");
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
    fn format_context_section_uses_text_fence_language() {
        // N2: a bare ``` opening fence has no language tag, which causes
        // some Markdown renderers to highlight as Bash by default. The
        // fetched content is heterogeneous (HCL, YAML, prose, Rust); pick
        // `text` as a safe non-highlighting default that keeps the fence
        // syntactically explicit.
        let docs = vec![ContextDoc {
            library: "lib".into(),
            content: "body".into(),
        }];
        let section = format_context_section(&docs);
        assert!(
            section.contains("```text\n"),
            "expected ```text fence, got: {section}"
        );
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

    // Production hydration form: regression guard. The function must extract
    // useful keywords (the imported symbol) from the real
    // `"{symbol}: {use|from|import ...}"` shape, not garbage like `"Mutex;"`
    // or `"react'"` (which is what the old split-on-`[':','/','.']` produced).
    #[test]
    fn build_code_aware_query_handles_production_hydration_format() {
        let imports = vec![
            "Mutex: use tokio::sync::Mutex;".into(),
            "Deserialize: use serde::{Deserialize, Serialize};".into(),
            "useState: import { useState } from 'react'".into(),
            "join: from os.path import join".into(),
        ];
        let query = build_code_aware_query("base", &imports);
        // Imported-symbol keywords are present.
        assert!(query.contains("Mutex"), "missing Mutex: {query}");
        assert!(query.contains("Deserialize"), "missing Deserialize: {query}");
        assert!(query.contains("useState"), "missing useState: {query}");
        assert!(query.contains("join"), "missing join: {query}");
        // Garbage tokens from naive splitting are absent.
        assert!(!query.contains("Mutex;"), "leaked trailing semicolon: {query}");
        assert!(!query.contains("react'"), "leaked closing quote: {query}");
        // Trash tokens from the use-statement body must not leak in.
        for trash in [" import ", "use ", " from "] {
            assert!(!query.contains(trash), "leaked statement keyword {trash:?}: {query}");
        }
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
    fn cached_fetcher_evicts_lru_entry_at_capacity_not_whole_cache() {
        // CR4 regression: previous code did `cache.clear()` when len ==
        // max_entries, dropping every entry at once. With LRU semantics, only
        // the *least-recently-used* entry should be evicted. Most-recently-used
        // entries must still hit cache after the wrap.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct CountingResolver { calls: Arc<AtomicUsize> }
        impl ContextFetcher for CountingResolver {
            fn resolve_library(&self, name: &str) -> Option<String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Some(format!("/lib/{}", name))
            }
            fn query_docs(&self, _: &str, _: &str, _: usize) -> Option<String> { None }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let inner = CountingResolver { calls: calls.clone() };
        let cached = CachedContextFetcher::new(&inner, 3);

        // Fill cache to capacity.
        let _ = cached.resolve_library("a"); // 1 call
        let _ = cached.resolve_library("b"); // 2 calls
        let _ = cached.resolve_library("c"); // 3 calls
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        // Insert a 4th entry. With proper LRU eviction, "a" is dropped but
        // "b" and "c" remain. With the old `clear()`, "b" and "c" would also
        // be dropped, forcing them to be re-fetched below.
        let _ = cached.resolve_library("d"); // 4 calls (cold)
        assert_eq!(calls.load(Ordering::SeqCst), 4);

        // Re-query "b" and "c" — they MUST hit cache (not increment calls).
        let _ = cached.resolve_library("b");
        let _ = cached.resolve_library("c");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "LRU eviction must keep hot entries; old clear() would push this to 6"
        );

        // "a" was the LRU victim, so it does require a fresh call.
        let _ = cached.resolve_library("a");
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn cached_fetcher_query_docs_evicts_lru_entry_at_capacity() {
        // Same regression check for query_docs cache.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct CountingQuery { calls: Arc<AtomicUsize> }
        impl ContextFetcher for CountingQuery {
            fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
            fn query_docs(&self, lib: &str, _: &str, _: usize) -> Option<String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Some(format!("docs:{lib}"))
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let inner = CountingQuery { calls: calls.clone() };
        let cached = CachedContextFetcher::new(&inner, 2);

        let _ = cached.query_docs("a", "q", 5000);
        let _ = cached.query_docs("b", "q", 5000);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // 3rd entry — evicts "a" (LRU), keeps "b".
        let _ = cached.query_docs("c", "q", 5000);
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        // "b" must hit cache.
        let _ = cached.query_docs("b", "q", 5000);
        assert_eq!(calls.load(Ordering::SeqCst), 3,
            "LRU eviction must keep hot entries; old clear() would push this to 4");
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
