# Issue #29 Implementation Plan — Context7 dep-based enrichment + Rust support

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the hardcoded 11-entry framework allow-list with manifest-parsed dep enrichment for Rust + JS/TS + Python, so every Rust review gets Context7 docs and long-tail libraries (sqlx, httpx, zod, etc.) are covered automatically.

**Architecture:** Pure dep-manifest parsers (new `src/dep_manifest.rs`) feed a refactored `enrich_for_review` orchestrator (in `src/context_enrichment.rs`) that filters by file imports (K=5), looks up curated queries with language-aware generic fallback, and returns docs + telemetry metrics. `CachedContextFetcher` gains 24h negative-result LRU with injectable clock for testability.

**Tech Stack:** Rust, `toml` 1.x, `serde_json`, `lru` 0.17, `tracing`, existing Context7 HTTP client.

**Design doc:** `docs/plans/2026-04-22-issue-29-context7-deps-design.md`
**Test reviews:**
- `docs/plans/2026-04-22-issue-29-test-coverage-review.md` (test-planning agent)
- `docs/plans/reviews/2026-04-22-issue-29-antipatterns.md` (antipatterns agent)

This v2 of the plan folds in MUST-FIX + SHOULD-FIX items from both reviews.

---

## Pre-flight

- Branch off `main` in a worktree (handled by Phase 2 of `/dev:start`).
- Confirm `cargo test --bin quorum` is green at branch start.
- Both design doc and this plan committed as the first commit on the branch.

---

## Task 1 — Module skeleton + first real Cargo parser test (MERGED 1+2)

Skeleton-only commits are Liar Tests. We register the module AND drive in the first real failing test together.

**Files:**
- Create: `src/dep_manifest.rs`
- Modify: `src/lib.rs` (add `pub mod dep_manifest;`)

**Step 1: Write failing test + minimal type**

Create `src/dep_manifest.rs`:
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

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    /// Test helper: write a Cargo.toml with the given (name, version) dep pairs in [dependencies].
    fn cargo_with(deps: &[(&str, &str)]) -> String {
        let mut s = String::from("[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\n");
        for (n, v) in deps {
            s.push_str(&format!("{n} = \"{v}\"\n"));
        }
        s
    }

    #[test]
    fn cargo_string_dep_is_parsed() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", &cargo_with(&[("tokio", "1"), ("serde", "1.0")]));
        let deps = parse_dependencies(dir.path());
        assert!(deps.iter().any(|d| d.name == "tokio" && d.language == "rust"));
        assert!(deps.iter().any(|d| d.name == "serde" && d.language == "rust"));
    }
}
```

Add to `src/lib.rs`: `pub mod dep_manifest;`

**Step 2: Run, verify FAIL**
```
cargo test --bin quorum dep_manifest::tests::cargo_string_dep_is_parsed
```
Expected: FAIL (parser returns empty Vec).

**Step 3: Implement minimal `parse_cargo`** (defer hyphen normalization + table form to Task 2):
```rust
fn parse_cargo(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => return Vec::new() };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => { tracing::warn!(error = %e, "Cargo.toml parse failed"); return Vec::new(); }
    };
    let mut out = Vec::new();
    if let Some(table) = parsed.get("dependencies").and_then(|v| v.as_table()) {
        for name in table.keys() {
            out.push(Dependency { name: name.clone(), language: "rust".into() });
        }
    }
    out
}

pub fn parse_dependencies(project_dir: &Path) -> Vec<Dependency> {
    let mut out = Vec::new();
    let cargo = project_dir.join("Cargo.toml");
    if cargo.exists() { out.extend(parse_cargo(&cargo)); }
    out
}
```

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/dep_manifest.rs src/lib.rs
git commit -m "feat(dep_manifest): add module + minimal Cargo string-dep parser (#29)"
```

---

## Task 2 — Cargo.toml: table form, dev/build deps, hyphen normalization, edge cases

**Files:** Modify `src/dep_manifest.rs`.

**Step 1: Write failing tests** (build on `cargo_with` helper from Task 1):
```rust
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
    write(dir.path(), "Cargo.toml", &cargo_with(&[("serde-json", "1"), ("tokio-stream", "0.1")]));
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"serde_json"), "got {names:?}");
    assert!(names.contains(&"tokio_stream"), "got {names:?}");
}

#[test]
fn cargo_workspace_root_with_only_members_returns_empty() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n");
    let deps = parse_dependencies(dir.path());
    assert!(deps.is_empty());
}

#[test]
fn cargo_workspace_dependencies_section_is_parsed() {
    // Locks down the silent-broadening risk: if someone adds [workspace.dependencies]
    // semantics later, we want a test that catches the change.
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[workspace]
members = ["a"]

[workspace.dependencies]
tokio = "1"
"#);
    let deps = parse_dependencies(dir.path());
    // v1 decision: workspace.dependencies is NOT parsed (workspace member resolution is
    // an explicit accepted limitation in the design). Pin this so a future change is
    // a deliberate decision.
    assert!(!deps.iter().any(|d| d.name == "tokio"),
        "workspace.dependencies parsing is deferred; got {deps:?}");
}

#[test]
fn cargo_dep_in_both_dependencies_and_dev_dependencies_appears() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dependencies]
tokio = "1"

[dev-dependencies]
tokio = "1"
"#);
    let deps = parse_dependencies(dir.path());
    let count = deps.iter().filter(|d| d.name == "tokio").count();
    // Pin the implementation choice: we currently push twice. Downstream dedupe in
    // enrich_for_review handles this. If parse_cargo grows dedup logic, change to == 1.
    assert!(count >= 1, "tokio missing entirely: {deps:?}");
}

#[test]
fn cargo_renamed_dep_uses_key_not_package_name() {
    // foo is the import-side name; "real-crate" is what's on crates.io.
    // We must surface "foo" so the import filter matches `use foo::...`.
    let dir = TempDir::new().unwrap();
    write(dir.path(), "Cargo.toml", r#"
[dependencies]
foo = { package = "real-crate", version = "1" }
"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "foo"),
        "renamed dep must surface key: {deps:?}");
    assert!(!deps.iter().any(|d| d.name == "real_crate"),
        "must not surface package name: {deps:?}");
}
```

**Step 2: Run, verify FAILS.**

**Step 3: Implement** — flesh out `parse_cargo`:
```rust
fn parse_cargo(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => return Vec::new() };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => { tracing::warn!(error = %e, "Cargo.toml parse failed"); return Vec::new(); }
    };
    let mut out = Vec::new();
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = parsed.get(*section).and_then(|v| v.as_table()) {
            for name in table.keys() {
                out.push(Dependency {
                    name: name.replace('-', "_"),
                    language: "rust".into(),
                });
            }
        }
    }
    out
}
```

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): full Cargo parser with hyphen norm + edge cases (#29)"
```

---

## Task 3 — package.json parser

**Files:** Modify `src/dep_manifest.rs`.

**Step 1: Failing tests**
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
        assert!(names.contains(&n), "missing {n} in {names:?}");
    }
}

#[test]
fn package_json_scoped_packages_kept_verbatim() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "package.json", r#"{"dependencies": {"@nestjs/core": "^10"}}"#);
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "@nestjs/core"));
}

#[test]
fn package_json_dependencies_get_typescript_language_when_project_is_typescript() {
    // The signal (tsconfig.json sibling) is incidental — what matters is the language
    // assigned to the deps. Naming the test by the behavior, not the mechanism.
    let dir = TempDir::new().unwrap();
    write(dir.path(), "package.json", r#"{"dependencies": {"react": "^18"}}"#);
    write(dir.path(), "tsconfig.json", "{}");
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().all(|d| d.language == "typescript"));
}

#[test]
fn package_json_dependencies_get_javascript_language_when_project_is_not_typescript() {
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

**Step 2: Run, verify FAILS.**

**Step 3: Implement**
```rust
fn parse_package_json(path: &Path, has_tsconfig: bool) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => return Vec::new() };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => { tracing::warn!(error = %e, "package.json parse failed"); return Vec::new(); }
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

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): parse package.json with TS/JS detection + scoped pkgs (#29)"
```

---

## Task 4 — pyproject.toml parser (PEP 621 + Poetry)

**Files:** Modify `src/dep_manifest.rs`.

**Step 1: Failing tests** (with exact-set assertions to catch over-inclusion):
```rust
#[test]
fn pyproject_pep621_deps_parsed_with_extras_and_versions_stripped() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", r#"
[project]
name = "x"
dependencies = ["fastapi>=0.100", "pydantic[email]>=2", "httpx"]
"#);
    let deps = parse_dependencies(dir.path());
    let mut names: Vec<_> = deps.iter().map(|d| d.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["fastapi", "httpx", "pydantic"]);
    assert!(deps.iter().all(|d| d.language == "python"));
}

#[test]
fn pyproject_poetry_deps_parsed_excluding_python_key() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", r#"
[tool.poetry.dependencies]
python = "^3.11"
fastapi = "^0.100"
httpx = { version = "*" }
"#);
    let deps = parse_dependencies(dir.path());
    let mut names: Vec<_> = deps.iter().map(|d| d.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["fastapi", "httpx"]);
}

#[test]
fn pyproject_pep621_wins_when_both_sections_present() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", r#"
[project]
dependencies = ["fastapi"]

[tool.poetry.dependencies]
django = "*"
"#);
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["fastapi"], "PEP 621 must win, not be merged with Poetry: {names:?}");
}
```

**Step 2: Run, verify FAILS.**

**Step 3: Implement**
```rust
fn strip_python_dep_spec(raw: &str) -> Option<String> {
    let no_extras = raw.split('[').next()?.trim();
    let name_end = no_extras
        .find(|c: char| matches!(c, '<' | '>' | '=' | '!' | '~' | ' ' | ';'))
        .unwrap_or(no_extras.len());
    let name = no_extras[..name_end].trim();
    if name.is_empty() { None } else { Some(name.to_string()) }
}

fn parse_pyproject(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => return Vec::new() };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => { tracing::warn!(error = %e, "pyproject.toml parse failed"); return Vec::new(); }
    };
    let mut out = Vec::new();
    if let Some(arr) = parsed.get("project")
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
        if !out.is_empty() { return out; }  // PEP 621 wins
    }
    if let Some(table) = parsed.get("tool")
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
if pyp.exists() { out.extend(parse_pyproject(&pyp)); }
```

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): parse pyproject.toml (PEP 621 + Poetry) (#29)"
```

---

## Task 5 — requirements.txt parser (split tests by skip-rule)

**Files:** Modify `src/dep_manifest.rs`.

**Step 1: Failing tests** (split for diagnosability):
```rust
#[test]
fn requirements_txt_skips_comments_and_blanks() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt", "# top comment\n\nfastapi\n# inline comment after\n");
    let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["fastapi"]);
}

#[test]
fn requirements_txt_skips_includes_and_editable() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt", "fastapi\n-r dev.txt\n-e .\n");
    let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["fastapi"]);
}

#[test]
fn requirements_txt_skips_vcs_urls() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt", "fastapi\ngit+https://github.com/x/y.git\nhttps://example.com/pkg.whl\n");
    let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["fastapi"]);
}

#[test]
fn requirements_txt_strips_version_specifiers() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt", "fastapi>=0.100\nrequests==2.31.0\n");
    let mut names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["fastapi", "requests"]);
}

#[test]
fn requirements_txt_skipped_when_pyproject_present() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", "[project]\ndependencies = [\"fastapi\"]\n");
    write(dir.path(), "requirements.txt", "django\n");
    let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["fastapi"]);
}
```

**Step 2: Run, verify FAILS.**

**Step 3: Implement**
```rust
fn parse_requirements_txt(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => return Vec::new() };
    let mut out = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#')
            || line.starts_with("-r") || line.starts_with("-e") {
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

Update `parse_dependencies` Python branch (gating):
```rust
let pyp = project_dir.join("pyproject.toml");
let req = project_dir.join("requirements.txt");
if pyp.exists() {
    out.extend(parse_pyproject(&pyp));
} else if req.exists() {
    out.extend(parse_requirements_txt(&req));
}
```

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/dep_manifest.rs
git commit -m "feat(dep_manifest): parse requirements.txt as pyproject fallback (#29)"
```

---

## Task 6 — Refactor `framework_queries` → `curated_query_for` (semantic-marker tests)

**Files:** Modify `src/context_enrichment.rs`. Migrate existing callers + tests.

**Step 1: Failing tests** (semantic markers, NOT exact strings):
```rust
#[test]
fn curated_query_for_known_frameworks_contains_semantic_markers() {
    // Each framework's curated query MUST contain a load-bearing keyword.
    // This catches the "silently changed to empty string" failure mode without
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
        let q = curated_query_for(name).unwrap_or_else(|| panic!("missing curated query for {name}"));
        assert!(q.contains(marker),
            "curated query for {name} missing marker '{marker}': got {q:?}");
    }
}

#[test]
fn curated_query_for_unknown_returns_none() {
    assert!(curated_query_for("tokio").is_none());
    assert!(curated_query_for("xyz-does-not-exist").is_none());
}
```

**Step 2: Run, verify FAILS (function does not exist).**

**Step 3: Implement** + delete `framework_queries`:
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

Migrate `fetch_framework_docs` to use `curated_query_for` (kept temporarily; replaced in Task 11). Migrate all existing tests that referenced `framework_queries`.

**Step 4: Run full suite, verify PASS.**
```
cargo test --bin quorum
```

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "refactor(context_enrichment): framework_queries -> curated_query_for (#29)"
```

---

## Task 7 — `generic_query_for_language` (keyword tests, not exact strings)

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Failing tests**
```rust
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
```

**Step 2: Run, verify FAILS.**

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

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): add generic_query_for_language (#29)"
```

---

## Task 8 — `build_code_aware_query` scoped-package fix

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Failing test**
```rust
#[test]
fn build_code_aware_query_extracts_scope_for_scoped_packages() {
    // @nestjs/core should yield "nestjs" (the framework hint), not "core" (useless).
    let query = build_code_aware_query("base", &["@nestjs/core".into()]);
    assert!(query.contains("nestjs"), "got: {query}");
    assert!(!query.split_whitespace().any(|w| w == "core"), "got: {query}");
}
```

**Step 2: Run, verify FAILS.**

**Step 3: Update keyword extraction in `build_code_aware_query`:**
```rust
let keywords: Vec<String> = import_targets.iter()
    .filter_map(|imp| {
        if let Some(stripped) = imp.strip_prefix('@') {
            stripped.split('/').next().map(|s| s.to_string())
        } else {
            imp.split(&['.', '/', ':'][..]).last().map(|s| s.to_string())
        }
    })
    .filter(|s| s.len() > 2)
    .take(10)
    .collect();
```

(Update `keywords.join(" ")` for `Vec<String>`.)

**Step 4: Run all `build_code_aware_query` tests, verify PASS.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "fix(context_enrichment): preserve @scope keyword for scoped npm packages (#29)"
```

---

## Task 9 — `EnrichmentResult` types + `enrich_for_review` skeleton

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Failing test** (boundary, not assertion-free):
```rust
#[test]
fn enrich_for_review_with_empty_inputs_returns_no_docs_and_zero_metrics() {
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

**Step 2: Run, verify FAILS (types don't exist).**

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

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): add EnrichmentResult types + skeleton (#29)"
```

---

## Task 10 — `enrich_for_review` body + `normalize_import_to_dep_names` (with direct tests)

**Files:** Modify `src/context_enrichment.rs`.

**Step 1: Failing tests** (with hoisted spy + normalize unit tests + order-independent assertions)

First, hoist a shared test support module:
```rust
#[cfg(test)]
mod test_support {
    use super::*;
    use std::sync::Mutex;

    pub struct Spy;
    impl ContextFetcher for Spy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(format!("/lib/{name}")) }
        fn query_docs(&self, lib: &str, _: &str, _: u32) -> Option<String> {
            Some(format!("docs for {lib}"))
        }
    }

    pub struct CapturingSpy { pub queries: Mutex<Vec<(String, String)>> }  // (lib, query)
    impl CapturingSpy {
        pub fn new() -> Self { Self { queries: Mutex::new(Vec::new()) } }
    }
    impl ContextFetcher for CapturingSpy {
        fn resolve_library(&self, name: &str) -> Option<String> { Some(name.into()) }
        fn query_docs(&self, lib: &str, query: &str, _: u32) -> Option<String> {
            self.queries.lock().unwrap().push((lib.into(), query.into()));
            Some("doc".into())
        }
    }
}
```

Then the tests:
```rust
// --- normalize_import_to_dep_names: direct unit tests ---
// Requires `pub(crate) fn normalize_import_to_dep_names(...)` for testability.

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
```

**Step 2: Run, verify all FAIL.**

**Step 3: Implement** — `normalize_import_to_dep_names` is `pub(crate)`; `enrich_for_review` body has NO dead code:
```rust
pub(crate) fn normalize_import_to_dep_names(imp: &str) -> Vec<String> {
    if let Some(stripped) = imp.strip_prefix('@') {
        let parts: Vec<&str> = stripped.splitn(3, '/').collect();
        if parts.len() >= 2 {
            return vec![format!("@{}/{}", parts[0], parts[1])];
        }
        return vec![imp.to_string()];
    }
    let head = imp
        .split(&['.', '/', ':'][..])
        .find(|s| !s.is_empty())  // skip empty heads from leading "::"
        .unwrap_or(imp)
        .to_string();
    vec![head]
}

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

    // Build import-occurrence-ordered list of dep names that appear in imports.
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

    // Cap at K (drops tail in import order, NOT random).
    for dep in import_matched.into_iter().take(K) {
        if seen.contains(&dep.name) { continue; }
        let query = curated_query_for(&dep.name)
            .unwrap_or_else(|| generic_query_for_language(&dep.language).into());
        try_fetch_one(&dep.name, &query, imports, fetcher, &mut docs, &mut metrics, &mut seen);
    }

    // Curated frameworks (HA/ESPHome) — additive path for directory-detected frameworks.
    for fw in curated_frameworks {
        if seen.contains(fw) { continue; }
        if let Some(query) = curated_query_for(fw) {
            try_fetch_one(fw, &query, imports, fetcher, &mut docs, &mut metrics, &mut seen);
        }
    }

    EnrichmentResult { docs, metrics }
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
        None => { metrics.context7_resolve_failed += 1; }
    }
}
```

**Step 4: Run all tests, verify PASS.**
```
cargo test --bin quorum context_enrichment::
cargo test --bin quorum dep_manifest::
```

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): enrich_for_review + normalize_import_to_dep_names (#29)"
```

---

## Task 11 — Coverage tests: K boundaries, import-order priority, dedupe, HA path, telemetry

**Files:** Modify `src/context_enrichment.rs` (test mod only — implementation already done in Task 10).

**Step 1: Tests**
```rust
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
fn enrich_telemetry_counts_resolves_resolve_fails_and_query_fails_separately() {
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

**Step 2: Run, verify PASS** (logic from Task 10 should already support these). If any fail, refine `enrich_for_review` accordingly.

**Step 3: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "test(context_enrichment): K boundaries, import-order, dedupe, HA, telemetry (#29)"
```

---

## Task 12 — Negative-result LRU cache with injectable clock

**Files:** Modify `src/context_enrichment.rs`. `lru` 0.17 already in Cargo.toml.

**Step 1: Failing tests**
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
    assert_eq!(*inner.calls.lock().unwrap(), 1);
}

#[test]
fn cached_fetcher_positive_result_is_cached() {
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

#[test]
fn cached_fetcher_negative_cache_expires_after_ttl() {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    struct CountingSpy { calls: Mutex<u32> }
    impl ContextFetcher for CountingSpy {
        fn resolve_library(&self, _: &str) -> Option<String> {
            *self.calls.lock().unwrap() += 1;
            None
        }
        fn query_docs(&self, _: &str, _: &str, _: u32) -> Option<String> { None }
    }
    let inner = CountingSpy { calls: Mutex::new(0) };
    // Use injectable clock to advance past TTL without thread::sleep(24h).
    let now = Instant::now();
    let mut current = now;
    let clock = move || current;  // closure-based clock; advance via outer var below
    // Simpler: use Arc<Mutex<Instant>> as the time source.
    let time = std::sync::Arc::new(Mutex::new(now));
    let time_clone = time.clone();
    let cached = CachedContextFetcher::new_with_clock(&inner, Duration::from_secs(60), move || *time_clone.lock().unwrap());
    let _ = cached.resolve_library("missing");
    *time.lock().unwrap() = now + Duration::from_secs(120);
    let _ = cached.resolve_library("missing");
    assert_eq!(*inner.calls.lock().unwrap(), 2,
        "expired entry must trigger fresh inner call");
}
```

**Step 2: Run, verify FAILS.**

**Step 3: Implement** — make TTL + clock injectable:
```rust
struct ResolveCacheEntry {
    result: Option<String>,
    cached_at: std::time::Instant,
}

type Clock = Box<dyn Fn() -> std::time::Instant + Send + Sync>;

pub struct CachedContextFetcher<'a> {
    inner: &'a dyn ContextFetcher,
    query_cache: Mutex<LruCache<(String, String, u32), Option<String>>>,
    resolve_cache: Mutex<LruCache<String, ResolveCacheEntry>>,
    resolve_ttl: std::time::Duration,
    now: Clock,
}

const DEFAULT_RESOLVE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
const RESOLVE_CACHE_CAP: usize = 256;

impl<'a> CachedContextFetcher<'a> {
    pub fn new(inner: &'a dyn ContextFetcher) -> Self {
        Self::new_with_clock(inner, DEFAULT_RESOLVE_CACHE_TTL, || std::time::Instant::now())
    }

    pub fn new_with_clock(
        inner: &'a dyn ContextFetcher,
        resolve_ttl: std::time::Duration,
        now: impl Fn() -> std::time::Instant + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            query_cache: Mutex::new(LruCache::new(/* existing cap */)),
            resolve_cache: Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(RESOLVE_CACHE_CAP).unwrap(),
            )),
            resolve_ttl,
            now: Box::new(now),
        }
    }
}
```

Update `resolve_library`:
```rust
fn resolve_library(&self, name: &str) -> Option<String> {
    let now = (self.now)();
    {
        let mut cache = self.resolve_cache.lock().unwrap();
        if let Some(entry) = cache.get(name) {
            if now.duration_since(entry.cached_at) < self.resolve_ttl {
                return entry.result.clone();
            }
        }
    }
    let result = self.inner.resolve_library(name);
    let mut cache = self.resolve_cache.lock().unwrap();
    cache.put(name.into(), ResolveCacheEntry {
        result: result.clone(),
        cached_at: now,
    });
    result
}
```

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "feat(context_enrichment): negative-result LRU cache with injectable clock (#29)"
```

---

## Task 13 — Add Context7 counters to `TelemetryEntry` (correct struct, in `src/telemetry.rs`)

**Files:** Modify `src/telemetry.rs`. **NOT** `src/review.rs` — that struct does not exist.

**Step 1: Failing tests**
```rust
#[test]
fn telemetry_entry_context7_fields_default_to_zero() {
    let t = TelemetryEntry {
        ts: chrono::Utc::now(),
        files: vec![],
        findings: HashMap::new(),
        model: "x".into(),
        tokens_in: 0,
        tokens_out: 0,
        duration_ms: 0,
        suppressed: 0,
        ..Default::default()  // requires Default impl, OR set new fields explicitly to 0
    };
    assert_eq!(t.context7_resolved, 0);
    assert_eq!(t.context7_resolve_failed, 0);
    assert_eq!(t.context7_query_failed, 0);
}

#[test]
fn telemetry_entry_old_jsonl_row_deserializes_with_zero_context7_fields() {
    // CRITICAL: every existing user's `quorum stats` breaks if this fails.
    // Shape matches the actual TelemetryEntry as of branch start.
    let old = r#"{
        "ts": "2026-01-01T00:00:00Z",
        "files": [],
        "findings": {},
        "model": "gpt-5.4",
        "tokens_in": 0,
        "tokens_out": 0,
        "duration_ms": 0,
        "suppressed": 0
    }"#;
    let entry: TelemetryEntry = serde_json::from_str(old)
        .expect("old JSONL rows must deserialize after schema bump");
    assert_eq!(entry.context7_resolved, 0);
    assert_eq!(entry.context7_resolve_failed, 0);
    assert_eq!(entry.context7_query_failed, 0);
}
```

**Step 2: Run, verify FAILS.**

**Step 3: Implement** — add to `TelemetryEntry`:
```rust
#[serde(default)] pub context7_resolved: u32,
#[serde(default)] pub context7_resolve_failed: u32,
#[serde(default)] pub context7_query_failed: u32,
```

If `TelemetryEntry` doesn't yet derive `Default`, derive it (or write the construction explicitly in the test instead of `..Default::default()`).

**Step 4: Run, verify PASS.**

**Step 5: Commit**
```bash
git add src/telemetry.rs
git commit -m "feat(telemetry): add Context7 enrichment counters with serde(default) (#29)"
```

---

## Task 14 — Wire `enrich_for_review` into `pipeline.rs` (with explicit integration assertions)

**Files:** Modify `src/pipeline.rs:362` and `:689`. Add `tests/context7_integration.rs`.

**Step 1: Failing integration test** (pin assertions explicitly):
```rust
// tests/context7_integration.rs

use std::path::Path;
// ... imports

#[test]
fn pipeline_review_writes_context7_counters_to_telemetry() {
    // Setup: tempdir with Cargo.toml containing tokio + serde,
    //        plus src/main.rs that `use tokio::sync;` and `use serde::Serialize;`.
    // Inject a spy ContextFetcher that resolves both deps and returns docs.
    // Run the pipeline entry that flushes telemetry.
    // Assert:
    //   - The latest TelemetryEntry has context7_resolved == 2
    //   - context7_resolve_failed == 0, context7_query_failed == 0
    //   - The spy received resolve_library calls only for "tokio" and "serde",
    //     NOT for any other Cargo dep that wasn't imported.
    //
    // Implementation note: this likely requires either:
    //   (a) extracting an `enrich_for_review_in_project(&Path, &dyn ContextFetcher) -> ...`
    //       helper that the test can call directly, OR
    //   (b) wiring a #[cfg(test)] hook to inject the fetcher.
    // (a) is preferred — it's a real public seam, not a test backdoor.
    todo!("flesh out per implementation seam chosen during Task 14");
}
```

**Step 2: Run, verify FAILS (todo! panics).**

**Step 3: Implement**

Replace both pipeline call sites:
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

Then complete the integration test per the chosen seam.

**Step 4: Run full suite + new integration test, verify PASS.**
```
cargo test --bin quorum
cargo test
```

**Step 5: Commit**
```bash
git add src/pipeline.rs tests/context7_integration.rs
git commit -m "feat(pipeline): wire enrich_for_review with manifest deps + telemetry (#29)"
```

---

## Task 15 — Remove dead `fetch_framework_docs`

**Files:** Modify `src/context_enrichment.rs`.

**Step 1:** Verify no callers remain:
```
rg "fetch_framework_docs" src/ tests/
```

**Step 2:** Delete `fetch_framework_docs` and any tests that referenced it.

**Step 3: Run full suite + clippy.**
```
cargo test --bin quorum && cargo clippy --all-targets -- -D warnings
```

**Step 4: Commit**
```bash
git add src/context_enrichment.rs
git commit -m "refactor(context_enrichment): remove dead fetch_framework_docs (#29)"
```

---

## Task 16 — Docs update

**Files:**
- Modify: `CLAUDE.md` (update "Review Telemetry" to mention Context7 fields; add "Context7 enrichment" sub-section under Context Injection)
- Modify: `docs/ARCHITECTURE.md` if it references `framework_queries` or the framework allow-list

**Step 1:** Search for stale references:
```
rg "framework_queries|hardcoded.*framework|allow-list.*framework" docs/ CLAUDE.md
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
