/// Domain/framework detection: scan project for framework markers.
/// Detected domains enrich review prompts with framework-specific context.

use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainInfo {
    pub frameworks: Vec<String>,
    pub languages: Vec<String>,
}

/// Detect frameworks and languages from project directory structure and config files.
pub fn detect_domain(project_dir: &Path) -> DomainInfo {
    let mut languages: HashSet<String> = HashSet::new();
    let mut frameworks: HashSet<String> = HashSet::new();

    // Language detection
    if project_dir.join("Cargo.toml").exists() {
        languages.insert("rust".into());
    }
    if project_dir.join("pyproject.toml").exists()
        || project_dir.join("setup.py").exists()
        || project_dir.join("requirements.txt").exists()
    {
        languages.insert("python".into());
    }
    if project_dir.join("tsconfig.json").exists() {
        languages.insert("typescript".into());
    }
    // JS only if package.json exists AND not already detected as TS
    if project_dir.join("package.json").exists() && !languages.contains("typescript") {
        languages.insert("javascript".into());
    }
    if project_dir.join("go.mod").exists() {
        languages.insert("go".into());
    }

    // Framework detection from package.json — exact key matching
    if let Ok(content) = std::fs::read_to_string(project_dir.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            let deps = collect_dep_keys(&pkg);
            if deps.contains("next") {
                frameworks.insert("nextjs".into());
            }
            if deps.contains("react") && !frameworks.contains("nextjs") {
                frameworks.insert("react".into());
            }
            if deps.contains("vue") {
                frameworks.insert("vue".into());
            }
            if deps.contains("express") {
                frameworks.insert("express".into());
            }
            if deps.contains("fastify") {
                frameworks.insert("fastify".into());
            }
        }
    }

    // Framework detection from pyproject.toml
    if let Ok(content) = std::fs::read_to_string(project_dir.join("pyproject.toml")) {
        let lower = content.to_lowercase();
        if lower.contains("fastapi") {
            frameworks.insert("fastapi".into());
        }
        if lower.contains("django") {
            frameworks.insert("django".into());
        }
        if lower.contains("flask") {
            frameworks.insert("flask".into());
        }
    }

    // Django detection from manage.py
    if project_dir.join("manage.py").exists() {
        if let Ok(content) = std::fs::read_to_string(project_dir.join("manage.py")) {
            if content.contains("django") {
                frameworks.insert("django".into());
            }
        }
    }

    let mut langs: Vec<String> = languages.into_iter().collect();
    let mut fws: Vec<String> = frameworks.into_iter().collect();
    langs.sort();
    fws.sort();

    DomainInfo {
        frameworks: fws,
        languages: langs,
    }
}

/// Collect exact dependency key names from package.json
fn collect_dep_keys(pkg: &serde_json::Value) -> HashSet<String> {
    let mut keys = HashSet::new();
    for section in &["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(deps) = pkg[section].as_object() {
            for k in deps.keys() {
                keys.insert(k.clone());
            }
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_project(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (path, content) in files {
            let full_path = dir.path().join(path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full_path, content).unwrap();
        }
        dir
    }

    #[test]
    fn detect_rust_project() {
        let dir = create_project(&[("Cargo.toml", "[package]\nname = \"test\"\n")]);
        let info = detect_domain(dir.path());
        assert!(info.languages.contains(&"rust".to_string()));
    }

    #[test]
    fn detect_python_project() {
        let dir = create_project(&[("pyproject.toml", "[project]\nname = \"test\"\n")]);
        let info = detect_domain(dir.path());
        assert!(info.languages.contains(&"python".to_string()));
    }

    #[test]
    fn detect_typescript_project() {
        let dir = create_project(&[("tsconfig.json", "{}")]);
        let info = detect_domain(dir.path());
        assert!(info.languages.contains(&"typescript".to_string()));
    }

    #[test]
    fn detect_react_framework() {
        let dir = create_project(&[
            ("package.json", r#"{"dependencies":{"react":"^18.0.0"}}"#),
        ]);
        let info = detect_domain(dir.path());
        assert!(info.frameworks.contains(&"react".to_string()));
    }

    #[test]
    fn detect_django_framework() {
        let dir = create_project(&[
            ("manage.py", "#!/usr/bin/env python\nimport django\n"),
            ("settings.py", "INSTALLED_APPS = []\n"),
        ]);
        let info = detect_domain(dir.path());
        assert!(info.frameworks.contains(&"django".to_string()));
    }

    #[test]
    fn detect_nextjs_framework() {
        let dir = create_project(&[
            ("package.json", r#"{"dependencies":{"next":"^14.0.0"}}"#),
        ]);
        let info = detect_domain(dir.path());
        assert!(info.frameworks.contains(&"nextjs".to_string()));
    }

    #[test]
    fn detect_fastapi_framework() {
        let dir = create_project(&[
            ("pyproject.toml", "[project]\ndependencies = [\"fastapi\"]\n"),
        ]);
        let info = detect_domain(dir.path());
        assert!(info.frameworks.contains(&"fastapi".to_string()));
    }

    #[test]
    fn detect_empty_project() {
        let dir = TempDir::new().unwrap();
        let info = detect_domain(dir.path());
        assert!(info.frameworks.is_empty());
        assert!(info.languages.is_empty());
    }

    #[test]
    fn detect_multi_language_project() {
        let dir = create_project(&[
            ("Cargo.toml", "[package]\n"),
            ("package.json", r#"{"dependencies":{}}"#),
        ]);
        let info = detect_domain(dir.path());
        assert!(info.languages.contains(&"rust".to_string()));
        assert!(info.languages.contains(&"javascript".to_string()));
    }
}
