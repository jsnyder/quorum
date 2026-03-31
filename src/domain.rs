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

    // Home Assistant detection
    if project_dir.join(".HA_VERSION").exists() {
        frameworks.insert("home-assistant".into());
    }
    if let Ok(content) = std::fs::read_to_string(project_dir.join("configuration.yaml")) {
        if content.contains("homeassistant:") {
            frameworks.insert("home-assistant".into());
        }
    }
    // HA marker files: automations, scripts, scenes
    for ha_file in &["automations.yaml", "scripts.yaml", "scenes.yaml"] {
        if project_dir.join(ha_file).exists() {
            frameworks.insert("home-assistant".into());
            break;
        }
    }

    // ESPHome detection: any .yaml file with top-level `esphome:` key
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if content.starts_with("esphome:")
                        || content.contains("\nesphome:")
                    {
                        frameworks.insert("esphome".into());
                        break;
                    }
                }
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
    fn detect_ha_from_configuration_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("configuration.yaml"),
            "homeassistant:\n  name: Home\n  unit_system: metric\n",
        )
        .unwrap();
        let domain = detect_domain(dir.path());
        assert!(
            domain.frameworks.iter().any(|f| f.contains("home-assistant")),
            "Should detect Home Assistant. Got: {:?}",
            domain.frameworks
        );
    }

    #[test]
    fn detect_ha_from_ha_version_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".HA_VERSION"), "2024.1.0\n").unwrap();
        let domain = detect_domain(dir.path());
        assert!(
            domain.frameworks.iter().any(|f| f.contains("home-assistant")),
            "Should detect HA from .HA_VERSION. Got: {:?}",
            domain.frameworks
        );
    }

    #[test]
    fn detect_ha_from_automations_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("automations.yaml"),
            "- id: test\n  alias: Test\n",
        )
        .unwrap();
        let domain = detect_domain(dir.path());
        assert!(
            domain.frameworks.iter().any(|f| f.contains("home-assistant")),
            "Should detect HA from automations.yaml. Got: {:?}",
            domain.frameworks
        );
    }

    #[test]
    fn no_ha_in_generic_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), "server:\n  port: 8080\n").unwrap();
        let domain = detect_domain(dir.path());
        assert!(
            !domain
                .frameworks
                .iter()
                .any(|f| f.contains("home-assistant")),
            "Generic YAML project should NOT be detected as HA"
        );
    }

    #[test]
    fn detect_esphome_from_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("device.yaml"),
            "esphome:\n  name: my-device\n  platform: ESP32\n",
        )
        .unwrap();
        let domain = detect_domain(dir.path());
        assert!(
            domain.frameworks.iter().any(|f| f.contains("esphome")),
            "Should detect ESPHome. Got: {:?}",
            domain.frameworks
        );
    }

    #[test]
    fn no_esphome_in_generic_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.yaml"), "server:\n  port: 3000\n").unwrap();
        let domain = detect_domain(dir.path());
        assert!(
            !domain.frameworks.iter().any(|f| f.contains("esphome")),
            "Generic YAML should NOT be detected as ESPHome"
        );
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
