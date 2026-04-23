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
    if cargo.exists() {
        out.extend(parse_cargo(&cargo));
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
}
