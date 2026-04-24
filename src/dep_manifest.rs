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
    let mut seen = std::collections::HashSet::new();
    let push_table = |table: &toml::value::Table, out: &mut Vec<Dependency>, seen: &mut std::collections::HashSet<String>| {
        for name in table.keys() {
            let normalized = name.replace('-', "_");
            if seen.insert(normalized.clone()) {
                out.push(Dependency { name: normalized, language: "rust".into() });
            }
        }
    };
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = parsed.get(*section).and_then(|v| v.as_table()) {
            push_table(table, &mut out, &mut seen);
        }
    }
    // `[target.<spec>.dependencies]` (and dev/build variants). Common in real
    // crates for cfg-gated platform deps (winapi, nix, inotify). Iterate every
    // child of [target.*] and pull the same three sub-tables.
    if let Some(targets) = parsed.get("target").and_then(|v| v.as_table()) {
        for spec_table in targets.values().filter_map(|v| v.as_table()) {
            for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = spec_table.get(*section).and_then(|v| v.as_table()) {
                    push_table(table, &mut out, &mut seen);
                }
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

/// Parse `pyproject.toml`.
///
/// Returns:
/// * `None` — pyproject is unreadable, malformed, or has *neither* a
///   `[project]` PEP 621 section nor a `[tool.poetry.dependencies]` section.
///   Caller should fall through to `requirements.txt`.
/// * `Some(deps)` — at least one recognized dep section was present (PEP 621
///   wins over Poetry; an explicit `dependencies = []` returns `Some(vec![])`
///   and is the project's source of truth — no fallthrough).
fn parse_pyproject(path: &Path) -> Option<Vec<Dependency>> {
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "pyproject.toml parse failed");
            return None;
        }
    };
    // Detect a [project] table that *declares* dependencies (regardless of TOML
    // type). A wrong-type value (e.g. `dependencies = "stringy"`) means the
    // user tried to declare deps and got it wrong — do NOT fall through to
    // requirements.txt and surface stale deps. Treat malformed-but-present as
    // "explicitly declared empty".
    let project_dependencies_value = parsed
        .get("project")
        .and_then(|p| p.get("dependencies"));
    let pep621_array = project_dependencies_value.and_then(|d| d.as_array());
    if let Some(arr) = pep621_array {
        let mut out = Vec::new();
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
        // PEP 621 section is present (possibly empty) → it wins over Poetry
        // AND over requirements.txt. An explicit empty array means "this
        // project has no deps" — do not fall through.
        return Some(out);
    }
    // Wrong-type `dependencies` (string, table, etc.) is also a present-but-
    // malformed declaration. Same rule as the empty-array branch: stop here,
    // do not fall through. Logs a warning so the user can fix the manifest.
    if let Some(v) = project_dependencies_value {
        tracing::warn!(
            kind = ?v.type_str(),
            "pyproject.toml: [project].dependencies has wrong TOML type (expected array of strings); treating as explicitly empty"
        );
        return Some(Vec::new());
    }
    let poetry_table = parsed
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table());
    let Some(table) = poetry_table else {
        // Neither PEP 621 nor Poetry section recognized — let requirements.txt
        // handle this project.
        return None;
    };
    let mut out = Vec::new();
    for name in table.keys() {
        if name == "python" { continue; }
        out.push(Dependency { name: name.clone(), language: "python".into() });
    }
    Some(out)
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
            || line.starts_with('-')   // pip options: -r, -e, --find-links, --no-binary, -c, --pre, etc.
        {
            continue;
        }
        // PEP 508 named direct ref: `mypkg[extras] @ git+https://...` or
        // `mypkg @ https://example.com/pkg.whl`. Extract the name before `@`
        // BEFORE the URL skip below — but only when the LHS is a plausible
        // package name, not URL-shaped text. Bare URLs like
        // `https://user@example.com/...` and `git+ssh://git@github.com/...`
        // also contain `@` (in the authority); those must NOT be treated as
        // named requirements.
        if let Some((lhs, _url)) = line.split_once('@') {
            let lhs_trim = lhs.trim();
            let looks_like_url = lhs_trim.contains("://") || lhs_trim.starts_with("git+");
            if !lhs_trim.is_empty() && !looks_like_url {
                if let Some(name) = strip_python_dep_spec(lhs_trim) {
                    out.push(Dependency { name, language: "python".into() });
                }
                continue;
            }
        }
        if line.contains("://") || line.starts_with("git+") {
            continue;
        }
        // Skip local path references (./dist/pkg.whl, ../lib, /opt/pkg.tar.gz)
        // and bare local artifact filenames in cwd (mypkg-1.0.0-py3-none-any.whl,
        // package.tar.gz, archive.zip, plugin.egg). pip accepts both shapes
        // but neither is a package name.
        if line.starts_with('.') || line.starts_with('/') || line.contains('/') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.ends_with(".whl")
            || lower.ends_with(".tar.gz")
            || lower.ends_with(".tar.bz2")
            || lower.ends_with(".tar.xz")
            || lower.ends_with(".zip")
            || lower.ends_with(".egg")
        {
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
    let pyproject_deps = if pyp.exists() { parse_pyproject(&pyp) } else { None };
    match pyproject_deps {
        Some(deps) => out.extend(deps),
        None => if req.exists() {
            out.extend(parse_requirements_txt(&req));
        },
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
    fn cargo_target_specific_dependencies_collected() {
        // Quorum MED: `[target.<spec>.dependencies]` (and dev/build variants)
        // are common in real Rust projects. Skipping them silently drops deps
        // like winapi/nix from enrichment.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", r#"
[target.'cfg(unix)'.dependencies]
nix = "0.27"

[target.'cfg(windows)'.dependencies]
winapi = "0.3"

[target.x86_64-unknown-linux-gnu.dev-dependencies]
inotify = "0.10"

[target.'cfg(target_os = "macos")'.build-dependencies]
cc = "1"
"#);
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        for expected in ["nix", "winapi", "inotify", "cc"] {
            assert!(names.contains(&expected.to_string()),
                "missing {expected} in {names:?}");
        }
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

    // --- Bug 3: PEP 508 named direct-URL refs (`name @ url`) were dropped ---
    // The previous filter dropped any line containing "://" or starting with
    // "git+", losing the usable name in `mypkg @ git+https://...`. Fix must
    // extract the name before `@` while still skipping unnamed bare URLs.

    #[test]
    fn requirements_txt_keeps_pep508_named_git_url() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "fastapi\nmypkg @ git+https://github.com/foo/bar.git\n");
        let mut names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["fastapi", "mypkg"]);
    }

    #[test]
    fn requirements_txt_keeps_pep508_named_https_wheel() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "wheelpkg @ https://example.com/pkg.whl\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["wheelpkg"]);
    }

    #[test]
    fn requirements_txt_keeps_pep508_named_url_with_extras() {
        // `mypkg[extra1,extra2] @ git+https://...` -> name `mypkg`, extras dropped.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "mypkg[email,async] @ git+https://github.com/foo/bar.git\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["mypkg"]);
    }

    #[test]
    fn requirements_txt_still_skips_unnamed_vcs_urls() {
        // Regression guard (antipatterns reviewer MUST-FIX 3.1):
        // bare `git+https://...` and `https://...` with no `name @` prefix
        // must still be skipped (no valid dep name to extract).
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "fastapi\ngit+https://github.com/x/y.git\nhttps://example.com/pkg.whl\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn requirements_txt_skips_at_with_empty_name() {
        // ` @ git+https://...` (no name before @) must NOT yield an empty-string dep.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            " @ git+https://github.com/x/y.git\nfastapi\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn requirements_txt_keeps_pep508_named_url_with_extras_and_whitespace() {
        // PEP 508 grammar allows whitespace between name and `[extras]`.
        // strip_python_dep_spec splits on whitespace first, so it handles this
        // — pin it so a future tightening doesn't regress.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "mypkg [email,async] @ git+https://github.com/foo/bar.git\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["mypkg"]);
    }

    #[test]
    fn requirements_txt_skips_unnamed_url_with_at_in_authority() {
        // Bug 4 (regression from Bug 3 fix): a bare URL like
        // `https://user@example.com/pkg.whl` or `git+ssh://user@host/repo.git`
        // contains `@` in the authority, but is NOT a PEP 508 `name @ url`
        // form. Earlier naive split_once('@') yielded bogus deps like
        // "https://user" or "git+ssh://user".
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "fastapi\n\
             https://user@example.com/pkg.whl\n\
             git+ssh://git@github.com/foo/bar.git\n\
             git+https://token:x-oauth-basic@github.com/foo/bar.git\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn requirements_txt_skips_local_paths() {
        // pre-existing bug exposed by re-review: vendored wheels, editable
        // local installs, and absolute paths in requirements.txt would
        // previously be emitted as fake deps named "./dist/pkg.whl" etc.
        // Path-shaped tokens are NOT package names; skip them.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "fastapi\n\
             ./dist/pkg.whl\n\
             ../lib\n\
             /opt/pkg.tar.gz\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn requirements_txt_skips_pip_options() {
        // In-branch HIGH from #29: pip directives starting with `-` (other than
        // `-r` and `-e` which are already skipped) must not become fake deps.
        // `--find-links`, `--no-binary`, `--index-url`, `-c constraints.txt`
        // all currently fell through to strip_python_dep_spec and were emitted.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "--find-links ./wheels\n\
             --no-binary :all:\n\
             --index-url https://pypi.example.com/simple\n\
             -c constraints.txt\n\
             --pre\n\
             fastapi\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn pyproject_pep621_keeps_pep508_named_direct_url() {
        // Mirror Bug 3 in pyproject [project.dependencies]: the `name @ url`
        // form already survives via strip_python_dep_spec stopping at the space,
        // but pin it explicitly so a future tighter URL filter doesn't regress it.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", r#"
[project]
dependencies = [
  "fastapi",
  "name @ git+https://github.com/a/b.git",
]
"#);
        let mut names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["fastapi", "name"]);
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

    // --- Bug 2: pyproject without recognized section silently dropped requirements.txt ---
    // The fix must distinguish "PEP 621 declared empty" (existing semantic above —
    // explicit `dependencies = []` returns []) from "no recognized section at all"
    // (should fall through to requirements.txt).

    #[test]
    fn pyproject_without_known_sections_falls_through_to_requirements() {
        // Build-system-only pyproject: no [project] AND no [tool.poetry] →
        // pyproject yields nothing useful → requirements.txt should win.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", "[build-system]\nrequires = [\"setuptools\"]\n");
        write(dir.path(), "requirements.txt", "fastapi\n");
        let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn pyproject_unparseable_falls_through_to_requirements() {
        // Syntactically broken pyproject must NOT poison the parse path —
        // requirements.txt is still a valid source of truth.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", "this is not valid toml ===\n");
        write(dir.path(), "requirements.txt", "django\n");
        let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["django"]);
    }

    #[test]
    fn pyproject_with_wrong_type_dependencies_does_not_fall_through() {
        // Quorum re-review HIGH: a `[project]` table whose `dependencies`
        // is the wrong TOML type (string, table, etc.) means the user
        // *tried* to declare deps. Falling through to requirements.txt
        // would mask the bug AND surface stale/wrong deps. Treat
        // `[project]` with a malformed dependencies key as "explicitly
        // declared zero deps" and stop there.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", r#"
[project]
name = "broken"
dependencies = "this should be an array"
"#);
        write(dir.path(), "requirements.txt", "fastapi\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, Vec::<String>::new(),
            "wrong-type dependencies must NOT fall through to requirements.txt");
    }

    #[test]
    fn pyproject_with_only_project_table_no_deps_key_still_falls_through() {
        // Regression guard: `[project]` with only metadata (name, version)
        // and NO `dependencies` key at all is "no PEP 621 deps section" —
        // should still fall through to requirements.txt or Poetry.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", r#"
[project]
name = "myproj"
version = "0.1.0"
"#);
        write(dir.path(), "requirements.txt", "fastapi\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn requirements_txt_skips_bare_archive_filenames() {
        // Quorum re-review HIGH: `pkg.whl` / `package.tar.gz` (no path
        // separator) are local pip artifact filenames, not package names.
        // The previous path heuristic only caught lines with `/` or leading
        // `.`/`/`. Bare filenames slipped through.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "requirements.txt",
            "fastapi\n\
             mypkg-1.0.0-py3-none-any.whl\n\
             package.tar.gz\n\
             archive.zip\n\
             plugin.egg\n");
        let names: Vec<_> = parse_dependencies(dir.path())
            .iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["fastapi"]);
    }

    #[test]
    fn pyproject_empty_pep621_with_requirements_present_still_wins() {
        // Antipatterns reviewer MUST-FIX 2.2 regression guard:
        // explicit `dependencies = []` declares zero deps and must NOT
        // silently fall through to a stray requirements.txt.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", "[project]\ndependencies = []\n");
        write(dir.path(), "requirements.txt", "fastapi\n");
        let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, Vec::<String>::new());
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
