//! Dep manifest parsers: extract project dependencies for Context7 enrichment.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub language: String,
}

fn parse_cargo(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Cargo.toml parse failed");
            return Vec::new();
        }
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

fn parse_package_json(path: &Path, has_tsconfig: bool) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "package.json parse failed");
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

fn strip_python_dep_spec(raw: &str) -> Option<String> {
    let no_extras = raw.split('[').next()?.trim();
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
            tracing::warn!(error = %e, "pyproject.toml parse failed");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    let pep621_array = parsed
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array());
    if let Some(arr) = pep621_array {
        for v in arr {
            if let Some(s) = v.as_str() {
                let trimmed = s.trim();
                // Skip PEP 508 direct-reference URLs without a leading `name @`.
                // (The `name @ url` form survives because strip_python_dep_spec
                // stops at the space after `name`.)
                if trimmed.starts_with("git+")
                    || trimmed.starts_with("http://")
                    || trimmed.starts_with("https://")
                    || trimmed.starts_with("file://")
                {
                    continue;
                }
                if let Some(name) = strip_python_dep_spec(s) {
                    out.push(Dependency { name, language: "python".into() });
                }
            }
        }
        // PEP 621 section is present (possibly empty) → it wins over Poetry.
        // An explicit empty array means "this project has no deps" — do not
        // fall through to [tool.poetry.dependencies].
        return out;
    }
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

fn parse_requirements_txt(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("-r")
            || line.starts_with("-e")
        {
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

pub fn parse_dependencies(project_dir: &Path) -> Vec<Dependency> {
    let mut out = Vec::new();
    let cargo = project_dir.join("Cargo.toml");
    if cargo.exists() {
        out.extend(parse_cargo(&cargo));
    }
    let pkg = project_dir.join("package.json");
    if pkg.exists() {
        let has_tsconfig = project_dir.join("tsconfig.json").exists();
        out.extend(parse_package_json(&pkg, has_tsconfig));
    }
    let pyp = project_dir.join("pyproject.toml");
    let req = project_dir.join("requirements.txt");
    if pyp.exists() {
        out.extend(parse_pyproject(&pyp));
    } else if req.exists() {
        out.extend(parse_requirements_txt(&req));
    }
    out
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

    #[test]
    fn cargo_table_dep_is_parsed() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", "[dependencies]\ntokio = { version = \"1\", features = [\"full\"] }\n");
        let deps = parse_dependencies(dir.path());
        assert!(deps.iter().any(|d| d.name == "tokio"));
    }

    #[test]
    fn cargo_dev_and_build_deps_included() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", "[dev-dependencies]\ntempfile = \"3\"\n\n[build-dependencies]\ncc = \"1\"\n");
        let deps = parse_dependencies(dir.path());
        let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"tempfile"));
        assert!(names.contains(&"cc"));
    }

    #[test]
    fn cargo_workspace_true_extracts_name() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", "[dependencies]\ntokio = { workspace = true }\n");
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
    fn cargo_workspace_dependencies_section_is_not_parsed_in_v1() {
        // v1 decision: workspace.dependencies is NOT parsed (workspace member resolution
        // is an explicit accepted limitation in the design). Pin this so a future
        // change to broaden parsing is a deliberate decision, not a silent regression.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", "[workspace]\nmembers = [\"a\"]\n\n[workspace.dependencies]\ntokio = \"1\"\n");
        let deps = parse_dependencies(dir.path());
        assert!(!deps.iter().any(|d| d.name == "tokio"),
            "workspace.dependencies parsing is deferred; got {deps:?}");
    }

    #[test]
    fn cargo_dep_in_both_dependencies_and_dev_dependencies_appears() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", "[dependencies]\ntokio = \"1\"\n\n[dev-dependencies]\ntokio = \"1\"\n");
        let deps = parse_dependencies(dir.path());
        let count = deps.iter().filter(|d| d.name == "tokio").count();
        // Pin the choice: parse_cargo currently appends both. Downstream dedupe in
        // enrich_for_review handles uniqueness. If parse_cargo grows dedup, change to == 1.
        assert!(count >= 1, "tokio missing entirely: {deps:?}");
    }

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

    #[test]
    fn pyproject_empty_pep621_array_wins_over_poetry() {
        // [project.dependencies = []] means "this project explicitly has no deps".
        // Without an explicit "section was present" check, the previous logic
        // silently fell through to [tool.poetry.dependencies], merging two
        // dep-source-of-truth sections.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", r#"
[project]
dependencies = []

[tool.poetry.dependencies]
django = "*"
"#);
        let deps = parse_dependencies(dir.path());
        let names: Vec<_> = deps.iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, Vec::<String>::new(),
            "empty PEP 621 array must win, not fall through to Poetry: {names:?}");
    }

    #[test]
    fn pyproject_pep621_skips_pep508_direct_url_refs() {
        // PEP 508 direct refs in [project.dependencies] would otherwise parse to
        // garbage names like "git+https". parse_requirements_txt already filters
        // these; parse_pyproject must too.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", r#"
[project]
dependencies = [
  "fastapi",
  "git+https://github.com/x/y.git",
  "https://example.com/pkg.whl",
  "name @ git+https://github.com/a/b.git",
]
"#);
        let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
        // Only "fastapi" (a clean PEP 508 name) and "name" (the PEP 508 direct-ref
        // form: `name @ url` — the name half is legitimate) should survive.
        // Bare URLs without a leading `name @` are unusable as Context7 lookup keys.
        let mut sorted = names.clone();
        sorted.sort();
        assert!(sorted.contains(&"fastapi".to_string()), "fastapi missing: {names:?}");
        assert!(!sorted.iter().any(|n| n.contains("://") || n.starts_with("git+")),
            "URL-shaped names must be filtered: {names:?}");
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

    #[test]
    fn cargo_renamed_dep_uses_key_not_package_name() {
        // foo is the import-side name; "real-crate" is what's on crates.io.
        // We must surface "foo" so the import filter matches `use foo::...`.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", "[dependencies]\nfoo = { package = \"real-crate\", version = \"1\" }\n");
        let deps = parse_dependencies(dir.path());
        assert!(deps.iter().any(|d| d.name == "foo"),
            "renamed dep must surface key: {deps:?}");
        assert!(!deps.iter().any(|d| d.name == "real_crate"),
            "must not surface package name: {deps:?}");
    }
}
