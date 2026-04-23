# Issue #29 Implementation Plan — Context7 dep-based enrichment + Rust support

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the hardcoded 11-entry framework allow-list with manifest-parsed dep enrichment for Rust + JS/TS + Python, so every Rust review gets Context7 docs and long-tail libraries (sqlx, httpx, zod, etc.) are covered automatically.

**Architecture:** Pure dep-manifest parsers (new `src/dep_manifest.rs`) feed a refactored `enrich_for_review` orchestrator (in `src/context_enrichment.rs`) that filters by file imports (K=5), looks up curated queries with language-aware generic fallback, and returns docs + telemetry metrics. `CachedContextFetcher` gains 24h negative-result LRU.

**Tech Stack:** Rust, `toml` 1.x, `serde_json`, `lru` 0.17, `tracing`, existing Context7 HTTP client.

**Design doc:** `docs/plans/2026-04-22-issue-29-context7-deps-design.md` (read this first if you didn't author it).

---

## Pre-flight

- Branch off `main` in a worktree (handled by Phase 2 of `/dev:start`).
- Confirm `cargo test --bin quorum` is green at branch start.
- Copy/commit both the design doc and this plan as the first commit on the branch.

---

## Task 1 — Module skeleton + `Dependency` type

**Files:**
- Create: `src/dep_manifest.rs`
- Modify: `src/lib.rs` (add `pub mod dep_manifest;`)
- Test: `src/dep_manifest.rs` (inline `#[cfg(test)]` mod)

**Step 1: Write failing test**

Add to `src/dep_manifest.rs`:
```rust
//! Dep manifest parsers: extract project dependencies for Context7 enrichment.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub language: String,
}

pub fn parse_dependencies(_project_dir: &Path) -> Vec<Dependency> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn empty_project_returns_empty_vec() {
        let dir = TempDir::new().unwrap();
        let deps = parse_dependencies(dir.path());
        assert!(deps.is_empty());
    }
}
```

Add to `src/lib.rs`: `pub mod dep_manifest;`

**Step 2: Run test**
```
cargo test --bin quorum dep_manifest::tests::empty_project_returns_empty_vec -- --nocapture
```
Expected: PASS (stub returns empty Vec; test validates the entry point compiles).

**Step 3: Commit**
```bash
git add src/dep_manifest.rs src/lib.rs
git commit -m "feat(dep_manifest): add module skeleton + Dependency type (#29)"
```

---

## Task 2 — Cargo.toml parser (string + table deps)

**Files:**
- Modify: `src/dep_manifest.rs`

**Step 1: Write failing tests**

Append to `tests` mod:
```rust
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

#[test]
fn cargo_string_dep_is_parsed() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
tokio = "1"
serde = "1.0"
"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "tokio" && d.language == "rust"));
    assert!(deps.iter().any(|d| d.name == "serde" && d.language == "rust"));
}

#[test]
fn cargo_table_dep_is_parsed() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dependencies]
tokio = { version = "1", features = ["full"] }
"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "tokio"));
}

#[test]
fn cargo_dev_and_build_deps_included() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dev-dependencies]
tempfile = "3"

[build-dependencies]
cc = "1"
"#);
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"tempfile"));
    assert!(names.contains(&"cc"));
}

#[test]
fn cargo_workspace_true_extracts_name() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dependencies]
tokio = { workspace = true }
"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "tokio"));
}

#[test]
fn cargo_hyphen_normalized_to_underscore() {
    // serde-json in manifest becomes serde_json in code.
    // Without normalization, the imports-filter would never match.
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dependencies]
serde-json = "1"
tokio-stream = "0.1"
"#);
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"serde_json"), "got {:?}", names);
    assert!(names.contains(&"tokio_stream"), "got {:?}", names);
}

#[test]
fn cargo_workspace_root_no_dependencies_returns_empty() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[workspace]
members = ["a", "b"]
"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.is_empty());
}
```

**Step 2: Run, verify all 6 fail**
```
cargo test --bin quorum dep_manifest::tests::cargo
```
Expected: 6 FAILS (parser returns empty Vec).

**Step 3: Implement `parse_cargo`**

```rust
fn parse_cargo(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to parse Cargo.toml");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = parsed.get(*section).and_then(|v| v.as_table()) {
            for name in table.keys() {
                let normalized = name.replace('-', "_");
                out.push(Dependency { name: normalized, language: "rust".into() });
            }
        }
    }
    out
}
```

Update `parse_dependencies`:
```rust
pub fn parse_dependencies(project_dir: &Path) -> Vec<Dependency> {
    let mut out = Vec::new();
    let cargo = project_dir.join("Cargo.toml");
    if cargo.exists() {
        out.extend(parse_cargo(&cargo));
    }
    out
}
```

**Step 4: Run, verify all 6 pass**
```
cargo test --bin quorum dep_manifest::tests::cargo
```

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): parse Cargo.toml deps with hyphen normalization (#29)"
```

---

## Task 3 — package.json parser

**Files:** Modify `src/dep_manifest.rs`.

**Step 1: Write failing tests**
```rust
#[test]
fn package_json_all_dep_kinds_included() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "package.json", r#"{
        "dependencies": {"react": "^18"},
        "devDependencies": {"vitest": "^1"},
        "peerDependencies": {"@types/react": "^18"},
        "optionalDependencies": {"fsevents": "*"}
    }"#);
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    for n in ["react", "vitest", "@types/react", "fsevents"] {
        assert!(names.contains(&n), "missing {n} in {:?}", names);
    }
}

#[test]
fn package_json_scoped_packages_kept_verbatim() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "package.json", r#"{
        "dependencies": {"@nestjs/core": "^10"}
    }"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "@nestjs/core"));
}

#[test]
fn package_json_language_typescript_when_tsconfig_sibling() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "package.json", r#"{"dependencies": {"react": "^18"}}"#);
    write(dir.path(), "tsconfig.json", "{}");
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().all(|d| d.language == "typescript"));
}

#[test]
fn package_json_language_javascript_when_no_tsconfig() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "package.json", r#"{"dependencies": {"react": "^18"}}"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().all(|d| d.language == "javascript"));
}

#[test]
fn package_json_malformed_returns_empty() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "package.json", "{not json");
    let deps = parse_dependencies(dir.path());
    assert!(deps.is_empty());
}
```

**Step 2: Run, verify fails**
```
cargo test --bin quorum dep_manifest::tests::package_json
```

**Step 3: Implement**
```rust
fn parse_package_json(path: &Path, has_tsconfig: bool) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to parse package.json");
            return Vec::new();
        }
    };
    let lang = if has_tsconfig { "typescript" } else { "javascript" };
    let mut out = Vec::new();
    for key in &["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"] {
        if let Some(obj) = parsed.get(*key).and_then(|v| v.as_object()) {
            for name in obj.keys() {
                out.push(Dependency { name: name.clone(), language: lang.into() });
            }
        }
    }
    out
}
```

Wire into `parse_dependencies`:
```rust
let pkg = project_dir.join("package.json");
if pkg.exists() {
    let has_tsconfig = project_dir.join("tsconfig.json").exists();
    out.extend(parse_package_json(&pkg, has_tsconfig));
}
```

**Step 4: Run all dep_manifest tests, verify pass**
```
cargo test --bin quorum dep_manifest::
```

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): parse package.json with TS/JS detection + scoped pkgs (#29)"
```

---

## Task 4 — pyproject.toml parser (PEP 621 + Poetry)

**Files:** Modify `src/dep_manifest.rs`.

**Step 1: Write failing tests**
```rust
#[test]
fn pyproject_pep621_deps_parsed() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", r#"
[project]
name = "x"
dependencies = ["fastapi>=0.100", "pydantic[email]>=2", "httpx"]
"#);
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"fastapi"));
    assert!(names.contains(&"pydantic"));  // [email] stripped
    assert!(names.contains(&"httpx"));
    assert!(deps.iter().all(|d| d.language == "python"));
}

#[test]
fn pyproject_poetry_deps_parsed() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", r#"
[tool.poetry.dependencies]
python = "^3.11"
fastapi = "^0.100"
httpx = { version = "*" }
"#);
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(!names.contains(&"python"), "python key must be excluded");
    assert!(names.contains(&"fastapi"));
    assert!(names.contains(&"httpx"));
}

#[test]
fn pyproject_pep621_wins_when_both_present() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", r#"
[project]
dependencies = ["fastapi"]

[tool.poetry.dependencies]
django = "*"
"#);
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"fastapi"));
    assert!(!names.contains(&"django"));
}
```

**Step 2: Run, verify fails**
```
cargo test --bin quorum dep_manifest::tests::pyproject
```

**Step 3: Implement**
```rust
fn strip_python_dep_spec(raw: &str) -> Option<String> {
    // Strip extras [foo,bar], then strip version operators.
    let no_extras = raw.split('[').next()?.trim();
    // First non-name char is one of: <>=!~ space
    let name_end = no_extras
        .find(|c: char| matches!(c, '<' | '>' | '=' | '!' | '~' | ' ' | ';'))
        .unwrap_or(no_extras.len());
    let name = no_extras[..name_end].trim();
    if name.is_empty() { None } else { Some(name.to_string()) }
}

fn parse_pyproject(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to parse pyproject.toml");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    // PEP 621 first
    if let Some(arr) = parsed
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for v in arr {
            if let Some(s) = v.as_str() {
                if let Some(name) = strip_python_dep_spec(s) {
                    out.push(Dependency { name, language: "python".into() });
                }
            }
        }
        if !out.is_empty() {
            return out;  // PEP 621 wins
        }
    }
    // Poetry fallback
    if let Some(table) = parsed
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for name in table.keys() {
            if name == "python" { continue; }
            out.push(Dependency { name: name.clone(), language: "python".into() });
        }
    }
    out
}
```

Wire into `parse_dependencies`:
```rust
let pyp = project_dir.join("pyproject.toml");
if pyp.exists() {
    out.extend(parse_pyproject(&pyp));
}
```

**Step 4: Run, verify pass**
```
cargo test --bin quorum dep_manifest::tests::pyproject
```

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): parse pyproject.toml (PEP 621 + Poetry) (#29)"
```

---

## Task 5 — requirements.txt parser (with pyproject gating)

**Files:** Modify `src/dep_manifest.rs`.

**Step 1: Write failing tests**
```rust
#[test]
fn requirements_txt_parsed_when_no_pyproject() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt", "fastapi>=0.100\n# comment\n\nrequests\n-r dev.txt\ngit+https://x/y.git\n-e .\n");
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"fastapi"));
    assert!(names.contains(&"requests"));
    assert_eq!(names.len(), 2, "must skip comments/blanks/-r/-e/git+ {:?}", names);
}

#[test]
fn requirements_txt_skipped_when_pyproject_present() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", r#"[project]
dependencies = ["fastapi"]
"#);
    write(dir.path(), "requirements.txt", "django\n");
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"fastapi"));
    assert!(!names.contains(&"django"), "requirements.txt must be ignored when pyproject.toml present");
}
```

**Step 2: Run, verify fails.**

**Step 3: Implement**
```rust
fn parse_requirements_txt(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("-r") || line.starts_with("-e") {
            continue;
        }
        if line.contains("://") || line.starts_with("git+") {
            continue;
        }
        if let Some(name) = strip_python_dep_spec(line) {
            out.push(Dependency { name, language: "python".into() });
        }
    }
    out
}
```

Update `parse_dependencies` Python branch:
```rust
let pyp = project_dir.join("pyproject.toml");
let req = project_dir.join("requirements.txt");
if pyp.exists() {
    out.extend(parse_pyproject(&pyp));
} else if req.exists() {
    out.extend(parse_requirements_txt(&req));
}
```

**Step 4: Run, verify pass.**

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): parse requirements.txt as pyproject fallback (#29)"
```

---

## Task 6 — Refactor `framework_queries` → `curated_query_for`

**Files:** Modify `src/context_enrichment.rs`. Update all callers + tests.

**Step 1: Write failing test**
```rust
#[test]
fn curated_query_for_known_returns_some() {
    assert!(curated_query_for("react").is_some());
    assert!(curated_query_for("home-assistant").is_some());
}

#[test]
fn curated_query_for_unknown_returns_none() {
    assert!(curated_query_for("tokio").is_none());
    assert!(curated_query_for("xyz").is_none());
}
```

**Step 2: Run, verify fails (function does not exist).**

**Step 3: Implement**

Replace `framework_queries` body (keep the function but mark deprecated, OR remove and migrate callers in this task — choose remove):

```rust
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
```

Delete `framework_queries` and migrate `fetch_framework_docs` and existing tests to use `curated_query_for` (kept temporarily; will be replaced by `enrich_for_review` in Task 11).

**Step 4: Run full suite**
```
cargo test --bin quorum
```
Expected: PASS (existing tests migrated; behavior preserved for curated names).

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "refactor(context_enrichment): framework_queries -> curated_query_for lookup (#29)"
```

---

## Task 7 — `generic_query_for_language`

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Write failing tests**
```rust
#[test]
fn generic_query_per_language() {
    assert_eq!(generic_query_for_language("rust"), "common pitfalls async safety error handling");
    assert_eq!(generic_query_for_language("python"), "common pitfalls security type safety");
    assert_eq!(generic_query_for_language("typescript"), "common pitfalls security type safety async");
    assert_eq!(generic_query_for_language("javascript"), "common pitfalls security type safety async");
}

#[test]
fn generic_query_unknown_language_falls_back_to_minimal() {
    assert_eq!(generic_query_for_language("unknown"), "common pitfalls security");
}
```

**Step 2: Run, verify fails.**

**Step 3: Implement**
```rust
pub fn generic_query_for_language(lang: &str) -> &'static str {
    match lang {
        "rust" => "common pitfalls async safety error handling",
        "python" => "common pitfalls security type safety",
        "typescript" | "javascript" => "common pitfalls security type safety async",
        _ => "common pitfalls security",
    }
}
```

**Step 4: Run, verify pass.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): add generic_query_for_language (#29)"
```

---

## Task 8 — `build_code_aware_query` scoped-package fix

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Write failing test**
```rust
#[test]
fn build_code_aware_query_extracts_scope_for_scoped_packages() {
    // @nestjs/core should yield "nestjs" (the framework hint), not "core" (useless).
    let query = build_code_aware_query("base", &["@nestjs/core".into()]);
    assert!(query.contains("nestjs"), "got: {query}");
    assert!(!query.split_whitespace().any(|w| w == "core"), "got: {query}");
}
```

**Step 2: Run, verify fails (current logic returns `core`).**

**Step 3: Modify keyword-extraction in `build_code_aware_query`**

Replace the `keywords` line:
```rust
let keywords: Vec<String> = import_targets.iter()
    .filter_map(|imp| {
        if let Some(stripped) = imp.strip_prefix('@') {
            // @scope/pkg → scope
            stripped.split('/').next().map(|s| s.to_string())
        } else {
            imp.split(&['.', '/', ':'][..]).last().map(|s| s.to_string())
        }
    })
    .filter(|s| s.len() > 2)
    .take(10)
    .collect();
```

(Adjust `keywords.join(" ")` accordingly — type changed from `Vec<&str>` to `Vec<String>`.)

**Step 4: Run all `build_code_aware_query` tests, verify all pass.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "fix(context_enrichment): preserve @scope keyword for scoped npm packages (#29)"
```

---

## Task 9 — `EnrichmentResult` types + `enrich_for_review` skeleton

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Write failing tests**
```rust
#[test]
fn enrich_for_review_empty_inputs_returns_empty() {
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, _: &str) -> Option<String> { None }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { None }
    }
    let result = enrich_for_review(&[], &[], &[], &Spy);
    assert!(result.docs.is_empty());
    assert_eq!(result.metrics.context7_resolved, 0);
    assert_eq!(result.metrics.context7_resolve_failed, 0);
    assert_eq!(result.metrics.context7_query_failed, 0);
}
```

**Step 2: Run, verify fails (types don't exist).**

**Step 3: Add types + skeleton**
```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentMetrics {
    pub context7_resolved: u32,
    pub context7_resolve_failed: u32,
    pub context7_query_failed: u32,
}

#[derive(Debug, Default)]
pub struct EnrichmentResult {
    pub docs: Vec<ContextDoc>,
    pub metrics: EnrichmentMetrics,
}

pub fn enrich_for_review(
    _deps: &[crate::dep_manifest::Dependency],
    _curated_frameworks: &[String],
    _imports: &[String],
    _fetcher: &dyn ContextFetcher,
) -> EnrichmentResult {
    EnrichmentResult::default()
}
```

**Step 4: Run, verify pass.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): add EnrichmentResult types + skeleton (#29)"
```

---

## Task 10 — `enrich_for_review` imports-filter + curated/generic decision

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Write failing tests**
```rust
#[test]
fn enrich_filters_to_import_matched_deps() {
    use crate::dep_manifest::Dependency;
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, name: &str) -> Option<String> {
            Some(format!("/lib/{name}"))
        }
        fn query_docs(&self, lib: &str, _: &str, _: u32) -> Option<String> {
            Some(format!("docs for {lib}"))
        }
    }
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
fn enrich_curated_query_used_when_available() {
    use crate::dep_manifest::Dependency;
    use std::sync::Mutex;
    struct CapturingSpy { queries: Mutex<Vec<String>> }
    impl ContextFetcher for CapturingSpy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
        fn query_docs(&self, _: &str, query: &str, _: u32) -> Option<String> {
            self.queries.lock().unwrap().push(query.into());
            Some("doc".into())
        }
    }
    let spy = CapturingSpy { queries: Mutex::new(Vec::new()) };
    let deps = vec![Dependency { name: "react".into(), language: "javascript".into() }];
    let imports = vec!["react".into()];
    let _ = enrich_for_review(&deps, &[], &imports, &spy);
    let captured = spy.queries.lock().unwrap();
    assert!(captured[0].contains("hooks"), "curated query expected, got {:?}", *captured);
}

#[test]
fn enrich_generic_query_used_when_no_curated() {
    use crate::dep_manifest::Dependency;
    use std::sync::Mutex;
    struct CapturingSpy { queries: Mutex<Vec<String>> }
    impl ContextFetcher for CapturingSpy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
        fn query_docs(&self, _: &str, query: &str, _: u32) -> Option<String> {
            self.queries.lock().unwrap().push(query.into());
            Some("doc".into())
        }
    }
    let spy = CapturingSpy { queries: Mutex::new(Vec::new()) };
    let deps = vec![Dependency { name: "tokio".into(), language: "rust".into() }];
    let imports = vec!["tokio::spawn".into()];
    let _ = enrich_for_review(&deps, &[], &imports, &spy);
    let captured = spy.queries.lock().unwrap();
    assert!(captured[0].contains("async safety"), "rust generic query expected, got {:?}", *captured);
}
```

**Step 2: Run, verify all 3 fail.**

**Step 3: Implement**

Replace `enrich_for_review` body:
```rust
pub fn enrich_for_review(
    deps: &[crate::dep_manifest::Dependency],
    curated_frameworks: &[String],
    imports: &[String],
    fetcher: &dyn ContextFetcher,
) -> EnrichmentResult {
    const K: usize = 5;
    let mut metrics = EnrichmentMetrics::default();
    let mut docs: Vec<ContextDoc> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Build the imports-occurrence ordering: deps in import order, deduped.
    let import_set: std::collections::HashSet<String> =
        imports.iter().flat_map(normalize_import_to_dep_names).collect();

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

    // Use the import_set to silence dead_code on its construction:
    let _ = import_set;

    // Cap at K.
    for dep in import_matched.into_iter().take(K) {
        if seen.contains(&dep.name) { continue; }
        let query = curated_query_for(&dep.name)
            .unwrap_or_else(|| generic_query_for_language(&dep.language).into());
        try_fetch_one(&dep.name, &query, imports, fetcher, &mut docs, &mut metrics, &mut seen);
    }

    // HA/ESPHome and other directory-detected curated frameworks (additive).
    for fw in curated_frameworks {
        if seen.contains(fw) { continue; }
        if let Some(query) = curated_query_for(fw) {
            try_fetch_one(fw, &query, imports, fetcher, &mut docs, &mut metrics, &mut seen);
        }
    }

    EnrichmentResult { docs, metrics }
}

fn normalize_import_to_dep_names(imp: &str) -> Vec<String> {
    // For Rust "tokio::sync::Mutex" → ["tokio"]
    // For Python "fastapi.routing" → ["fastapi"]
    // For JS "@nestjs/core" → ["@nestjs/core"] (verbatim, scoped pkg)
    // For JS "react" → ["react"]
    if imp.starts_with('@') {
        // Scoped — keep first two segments verbatim.
        let parts: Vec<&str> = imp.splitn(3, '/').collect();
        if parts.len() >= 2 {
            return vec![format!("{}/{}", parts[0], parts[1])];
        }
        return vec![imp.to_string()];
    }
    let head = imp
        .split(&['.', '/', ':'][..])
        .next()
        .unwrap_or(imp)
        .to_string();
    vec![head]
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
    match fetcher.resolve_library(name) {
        Some(lib_id) => {
            metrics.context7_resolved += 1;
            let enriched = build_code_aware_query(query, imports);
            if let Some(content) = fetcher.query_docs(&lib_id, &enriched, 5000) {
                docs.push(ContextDoc { library: name.into(), content });
                seen.insert(name.into());
            } else {
                metrics.context7_query_failed += 1;
            }
        }
        None => {
            metrics.context7_resolve_failed += 1;
        }
    }
}
```

**Step 4: Run all `enrich_for_review` tests + full context_enrichment suite, verify pass.**
```
cargo test --bin quorum context_enrichment::
```

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): enrich_for_review with imports-filter + K=5 (#29)"
```

---

## Task 11 — `enrich_for_review` K=5 cap, dedupe, HA path tests

**Files:** Modify `src/context_enrichment.rs` (test mod only — implementation done in Task 10).

**Step 1: Write tests**
```rust
#[test]
fn enrich_caps_at_five_docs() {
    use crate::dep_manifest::Dependency;
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
        fn query_docs(&self, lib: &str, _: &str, _: u32) -> Option<String> { Some(format!("doc {lib}")) }
    }
    let deps: Vec<_> = (0..10).map(|i| Dependency {
        name: format!("dep{i}"), language: "rust".into()
    }).collect();
    let imports: Vec<_> = (0..10).map(|i| format!("dep{i}::x")).collect();
    let result = enrich_for_review(&deps, &[], &imports, &Spy);
    assert_eq!(result.docs.len(), 5);
}

#[test]
fn enrich_dedupes_curated_framework_already_in_deps() {
    use crate::dep_manifest::Dependency;
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
        fn query_docs(&self, lib: &str, _: &str, _: u32) -> Option<String> { Some(format!("doc {lib}")) }
    }
    let deps = vec![Dependency { name: "react".into(), language: "javascript".into() }];
    let imports = vec!["react".into()];
    let frameworks = vec!["react".into()];
    let result = enrich_for_review(&deps, &frameworks, &imports, &Spy);
    let count = result.docs.iter().filter(|d| d.library == "react").count();
    assert_eq!(count, 1, "react must appear exactly once");
}

#[test]
fn enrich_ha_framework_path_runs_without_manifest_match() {
    struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
        fn query_docs(&self, lib: &str, _: &str, _: u32) -> Option<String> { Some(format!("doc {lib}")) }
    }
    let frameworks = vec!["home-assistant".into()];
    let result = enrich_for_review(&[], &frameworks, &[], &Spy);
    assert!(result.docs.iter().any(|d| d.library == "home-assistant"));
}

#[test]
fn enrich_telemetry_counts_correctly() {
    use crate::dep_manifest::Dependency;
    struct PartialSpy;
    impl ContextFetcher for PartialSpy {
        fn resolve_library(&self, name: &str) -> Option<String> {
            if name == "good" { Some("/lib/good".into()) }
            else if name == "query_fails" { Some("/lib/qf".into()) }
            else { None }
        }
        fn query_docs(&self, lib: &str, _: &str, _: u32) -> Option<String> {
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
```

**Step 2: Run, verify pass (logic from Task 10 already supports these).** If any fail, refine `enrich_for_review` accordingly.

**Step 3: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "test(context_enrichment): cover K-cap, dedupe, HA path, telemetry counts (#29)"
```

---

## Task 12 — Negative-result LRU cache in `CachedContextFetcher`

**Files:** Modify `src/context_enrichment.rs`. Cargo.toml already has `lru` 0.17 per MEMORY.md.

**Step 1: Write failing tests**
```rust
#[test]
fn cached_fetcher_negative_result_is_cached() {
    use std::sync::Mutex;
    struct CountingSpy { calls: Mutex<u32> }
    impl ContextFetcher for CountingSpy {
        fn resolve_library(&self, _: &str) -> Option<String> {
            *self.calls.lock().unwrap() += 1;
            None
        }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { None }
    }
    let inner = CountingSpy { calls: Mutex::new(0) };
    let cached = CachedContextFetcher::new(&inner);
    assert!(cached.resolve_library("missing").is_none());
    assert!(cached.resolve_library("missing").is_none());
    assert!(cached.resolve_library("missing").is_none());
    assert_eq!(*inner.calls.lock().unwrap(), 1, "second+ calls must hit negative cache");
}

#[test]
fn cached_fetcher_positive_result_still_cached() {
    use std::sync::Mutex;
    struct CountingSpy { calls: Mutex<u32> }
    impl ContextFetcher for CountingSpy {
        fn resolve_library(&self, name: &str) -> Option<String> {
            *self.calls.lock().unwrap() += 1;
            Some(format!("/lib/{name}"))
        }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { None }
    }
    let inner = CountingSpy { calls: Mutex::new(0) };
    let cached = CachedContextFetcher::new(&inner);
    assert_eq!(cached.resolve_library("react"), Some("/lib/react".into()));
    assert_eq!(cached.resolve_library("react"), Some("/lib/react".into()));
    assert_eq!(*inner.calls.lock().unwrap(), 1);
}
```

**Step 2: Run, verify fails (current `CachedContextFetcher` only caches `query_docs`).**

**Step 3: Implement**

Add to `CachedContextFetcher`:
```rust
struct ResolveCacheEntry {
    result: Option<String>,
    cached_at: std::time::Instant,
}

pub struct CachedContextFetcher<'a> {
    inner: &'a dyn ContextFetcher,
    query_cache: Mutex<LruCache<(String, String, u32), Option<String>>>,
    resolve_cache: Mutex<LruCache<String, ResolveCacheEntry>>,
}

const RESOLVE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
const RESOLVE_CACHE_CAP: usize = 256;
```

Update `resolve_library`:
```rust
fn resolve_library(&self, name: &str) -> Option<String> {
    {
        let mut cache = self.resolve_cache.lock().unwrap();
        if let Some(entry) = cache.get(name) {
            if entry.cached_at.elapsed() < RESOLVE_CACHE_TTL {
                return entry.result.clone();
            }
        }
    }
    let result = self.inner.resolve_library(name);
    let mut cache = self.resolve_cache.lock().unwrap();
    cache.put(name.into(), ResolveCacheEntry {
        result: result.clone(),
        cached_at: std::time::Instant::now(),
    });
    result
}
```

Update `CachedContextFetcher::new` to initialize `resolve_cache`.

**Step 4: Run, verify pass.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): negative-result LRU cache in CachedContextFetcher (#29)"
```

---

## Task 13 — `ReviewTelemetry` field additions

**Files:** Modify `src/review.rs` (find `pub struct ReviewTelemetry`).

**Step 1: Write failing test**
```rust
#[test]
fn review_telemetry_context7_fields_default_to_zero() {
    let t = ReviewTelemetry::default();
    assert_eq!(t.context7_resolved, 0);
    assert_eq!(t.context7_resolve_failed, 0);
    assert_eq!(t.context7_query_failed, 0);
}

#[test]
fn review_telemetry_context7_fields_deserialize_with_defaults() {
    // Old records without these fields must still deserialize.
    let json = r#"{"run_id":"x","timestamp":"2026-01-01T00:00:00Z"}"#;
    // Use whatever the existing minimal valid telemetry JSON shape is in the codebase;
    // adjust this test to match. The point: serde(default) on the new fields keeps
    // backward compat with reviews.jsonl.
}
```
(Adjust the second test to the actual minimal `ReviewTelemetry` shape.)

**Step 2: Run, verify fails.**

**Step 3: Implement** — add to `ReviewTelemetry`:
```rust
#[serde(default)] pub context7_resolved: u32,
#[serde(default)] pub context7_resolve_failed: u32,
#[serde(default)] pub context7_query_failed: u32,
```

**Step 4: Run, verify pass.**

**Step 5: Commit**
```bash
git add src/review.rs
git commit -m "feat(review): add Context7 enrichment counters to ReviewTelemetry (#29)"
```

---

## Task 14 — Wire `enrich_for_review` into `pipeline.rs`

**Files:** Modify `src/pipeline.rs:362` and `:689`.

**Step 1: Write failing integration-style test** (in `pipeline.rs` test mod or new `tests/context7_integration.rs`):
- Build a tempdir with Cargo.toml + a Rust source file
- Inject a spy ContextFetcher
- Run the relevant pipeline entry (or extract a `enrich_for_review_in_project(&Path, ...)` helper if needed for testability)
- Assert telemetry fields populated

**Step 2: Run, verify fails.**

**Step 3: Replace both call sites:**
```rust
let deps = crate::dep_manifest::parse_dependencies(&project_root);
let result = crate::context_enrichment::enrich_for_review(
    &deps,
    &domain.frameworks,
    &redacted_ctx.import_targets,  // or &[] at the second site
    &cached_fetcher,
);
let docs = result.docs;
telemetry.context7_resolved += result.metrics.context7_resolved;
telemetry.context7_resolve_failed += result.metrics.context7_resolve_failed;
telemetry.context7_query_failed += result.metrics.context7_query_failed;
```

**Step 4: Run full suite + new integration test, verify pass.**
```
cargo test --bin quorum
cargo test
```

**Step 5: Commit**
```bash
git add src/pipeline.rs tests/
git commit -m "feat(pipeline): use enrich_for_review with manifest deps + telemetry (#29)"
```

---

## Task 15 — Remove dead `fetch_framework_docs`

**Files:** Modify `src/context_enrichment.rs`.

**Step 1:** Verify no callers remain:
```
rg "fetch_framework_docs" src/ tests/
```

**Step 2: Delete `fetch_framework_docs` and any tests that referenced it (those should already be migrated/removed in earlier tasks).**

**Step 3: Run full suite.**
```
cargo test --bin quorum && cargo clippy -- -D warnings
```

**Step 4: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "refactor(context_enrichment): remove dead fetch_framework_docs (#29)"
```

---

## Task 16 — Docs update

**Files:**
- Modify: `CLAUDE.md` (add note under "Context Injection" or new "Context7 enrichment" section about manifest-based dep parsing)
- Modify: `docs/ARCHITECTURE.md` if it references `framework_queries`

**Step 1:** Search docs for stale references:
```
rg "framework_queries|allow-list|hardcoded.*framework" docs/ CLAUDE.md
```

**Step 2:** Update affected sections to describe the new flow: manifest parsing → imports filter → curated/generic query → Context7 fetch → LLM prompt.

**Step 3: Commit**
```bash
git add CLAUDE.md docs/
git commit -m "docs: update for dep-based Context7 enrichment (#29)"
```

---

## Final verification

```
cargo test --bin quorum
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Expected: all green; binary builds. Move to Phase 5 of `/dev:start` (verification with evidence) → Phase 6 (quorum review on changed files).

---

## Execution choice

After plan approval, two execution options:

1. **Subagent-Driven (this session)** — fresh subagent per task, code review between tasks, fast iteration. Default for single-session work.
2. **Parallel Session (separate)** — open new session in the worktree with `superpowers:executing-plans`, batch execution with checkpoints.

Recommend **Subagent-Driven** for this plan — 16 tasks, mostly mechanical TDD, benefits from quick review cadence.
