use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinterKind {
    Ruff,
    Clippy,
    Eslint,
    Yamllint,
    Shellcheck,
    Hadolint,
    Tflint,
    Golangcilint,
}

impl LinterKind {
    pub fn name(&self) -> &'static str {
        match self {
            LinterKind::Ruff => "ruff",
            LinterKind::Clippy => "clippy",
            LinterKind::Eslint => "eslint",
            LinterKind::Yamllint => "yamllint",
            LinterKind::Shellcheck => "shellcheck",
            LinterKind::Hadolint => "hadolint",
            LinterKind::Tflint => "tflint",
            LinterKind::Golangcilint => "golangci-lint",
        }
    }
}

/// A linter that *would* run for the languages in this review but is missing
/// the project-level config needed to turn it on. Emitted so operators and
/// agents know coverage they are leaving on the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinterHint {
    pub linter: LinterKind,
    pub language: &'static str,
    pub file_count: usize,
    pub enable_instruction: &'static str,
}

/// For each language present in `files`, report linters that integrate with
/// quorum but are not configured in `project_dir`. Only returns hints for
/// linters that actually need config to auto-enable (ruff/eslint/yamllint);
/// clippy/shellcheck/hadolint/tflint auto-enable from file presence alone.
pub fn detect_unconfigured_linters(project_dir: &Path, files: &[&Path]) -> Vec<LinterHint> {
    let mut hints = Vec::new();

    let ext_of = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
    };

    let count_by = |matches: fn(&str) -> bool| -> usize {
        files
            .iter()
            .filter(|p| ext_of(p).as_deref().is_some_and(matches))
            .count()
    };

    let py_count = count_by(|e| e == "py");
    if py_count > 0 && !has_ruff_config(project_dir) {
        hints.push(LinterHint {
            linter: LinterKind::Ruff,
            language: "Python",
            file_count: py_count,
            enable_instruction: "add [tool.ruff] to pyproject.toml or create ruff.toml",
        });
    }

    let jsts_count = count_by(|e| matches!(e, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"));
    if jsts_count > 0 && !has_eslint_config(project_dir) {
        hints.push(LinterHint {
            linter: LinterKind::Eslint,
            language: "JavaScript/TypeScript",
            file_count: jsts_count,
            enable_instruction: "add eslint.config.js or .eslintrc.* to the project root",
        });
    }

    let yaml_count = count_by(|e| e == "yaml" || e == "yml");
    if yaml_count > 0 && !has_yamllint_config(project_dir) {
        hints.push(LinterHint {
            linter: LinterKind::Yamllint,
            language: "YAML",
            file_count: yaml_count,
            enable_instruction: "add .yamllint (or .yamllint.yaml) to the project root",
        });
    }

    hints
}

fn has_ruff_config(dir: &Path) -> bool {
    if dir.join("ruff.toml").exists() {
        return true;
    }
    std::fs::read_to_string(dir.join("pyproject.toml"))
        .map(|c| c.contains("[tool.ruff]"))
        .unwrap_or(false)
}

fn has_eslint_config(dir: &Path) -> bool {
    const ESLINT_CONFIGS: &[&str] = &[
        ".eslintrc.json",
        ".eslintrc.js",
        ".eslintrc.yaml",
        ".eslintrc.yml",
        ".eslintrc",
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.cjs",
    ];
    ESLINT_CONFIGS.iter().any(|c| dir.join(c).exists())
}

fn has_yamllint_config(dir: &Path) -> bool {
    [".yamllint", ".yamllint.yaml", ".yamllint.yml"]
        .iter()
        .any(|c| dir.join(c).exists())
}

pub fn detect_linters(project_dir: &Path) -> Vec<LinterKind> {
    let mut linters = Vec::new();

    // Ruff: pyproject.toml with [tool.ruff] or ruff.toml
    if project_dir.join("ruff.toml").exists() {
        linters.push(LinterKind::Ruff);
    } else if let Ok(content) = std::fs::read_to_string(project_dir.join("pyproject.toml"))
        && content.contains("[tool.ruff]")
    {
        linters.push(LinterKind::Ruff);
    }

    // Clippy: Cargo.toml present
    if project_dir.join("Cargo.toml").exists() {
        linters.push(LinterKind::Clippy);
    }

    // ESLint: .eslintrc.* or eslint.config.*
    let eslint_configs = [
        ".eslintrc.json",
        ".eslintrc.js",
        ".eslintrc.yaml",
        ".eslintrc.yml",
        ".eslintrc",
        "eslint.config.js",
        "eslint.config.mjs",
    ];
    for config in &eslint_configs {
        if project_dir.join(config).exists() {
            linters.push(LinterKind::Eslint);
            break;
        }
    }

    // Yamllint: .yamllint, .yamllint.yaml, .yamllint.yml
    let yamllint_configs = [".yamllint", ".yamllint.yaml", ".yamllint.yml"];
    for config in &yamllint_configs {
        if project_dir.join(config).exists() {
            linters.push(LinterKind::Yamllint);
            break;
        }
    }

    // Shellcheck: detect if any .sh files exist in project root
    if std::fs::read_dir(project_dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("sh"))
        })
        .unwrap_or(false)
    {
        linters.push(LinterKind::Shellcheck);
    }

    // Hadolint: .hadolint.yaml/.hadolint.yml or Dockerfile exists
    let hadolint_configs = [".hadolint.yaml", ".hadolint.yml"];
    let has_hadolint_config = hadolint_configs
        .iter()
        .any(|c| project_dir.join(c).exists());
    let has_dockerfile = project_dir.join("Dockerfile").exists();
    if has_hadolint_config || has_dockerfile {
        linters.push(LinterKind::Hadolint);
    }

    // Tflint: .tflint.hcl config or .tf files in project root
    let has_tflint_config = project_dir.join(".tflint.hcl").exists();
    let has_tf_files = std::fs::read_dir(project_dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("tf"))
        })
        .unwrap_or(false);
    if has_tflint_config || has_tf_files {
        linters.push(LinterKind::Tflint);
    }

    // golangci-lint: .golangci.yml/.yaml/.toml/.json or go.mod
    let golangci_configs = [
        ".golangci.yml",
        ".golangci.yaml",
        ".golangci.toml",
        ".golangci.json",
    ];
    let has_golangci_config = golangci_configs
        .iter()
        .any(|c| project_dir.join(c).exists());
    if has_golangci_config || project_dir.join("go.mod").exists() {
        linters.push(LinterKind::Golangcilint);
    }

    linters
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // -- Unconfigured linter hints --

    fn pbuf(parts: &[&str]) -> Vec<PathBuf> {
        parts.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn hint_ruff_when_python_present_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["src/main.py"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].linter, LinterKind::Ruff);
        assert_eq!(hints[0].file_count, 1);
    }

    #[test]
    fn no_hint_when_ruff_toml_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ruff.toml"), "").unwrap();
        let files = pbuf(&["src/main.py"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        assert!(hints.iter().all(|h| h.linter != LinterKind::Ruff));
    }

    #[test]
    fn no_hint_when_pyproject_has_ruff_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.ruff]\n").unwrap();
        let files = pbuf(&["x.py"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        assert!(hints.iter().all(|h| h.linter != LinterKind::Ruff));
    }

    #[test]
    fn no_ruff_hint_without_python_files() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["src/main.rs"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        assert!(hints.iter().all(|h| h.linter != LinterKind::Ruff));
    }

    #[test]
    fn hint_eslint_when_ts_present_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["app.ts"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        assert!(hints.iter().any(|h| h.linter == LinterKind::Eslint));
    }

    #[test]
    fn hint_eslint_applies_to_js_tsx_jsx_mjs() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["a.js", "b.tsx", "c.jsx", "d.mjs"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        let eslint = hints
            .iter()
            .find(|h| h.linter == LinterKind::Eslint)
            .unwrap();
        assert_eq!(eslint.file_count, 4);
    }

    #[test]
    fn no_eslint_hint_when_flat_config_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("eslint.config.js"), "").unwrap();
        let files = pbuf(&["app.ts"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        assert!(hints.iter().all(|h| h.linter != LinterKind::Eslint));
    }

    #[test]
    fn hint_yamllint_when_yaml_present_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["ci.yaml", "x.yml"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        let yh = hints
            .iter()
            .find(|h| h.linter == LinterKind::Yamllint)
            .unwrap();
        assert_eq!(yh.file_count, 2);
    }

    #[test]
    fn multiple_hints_when_multiple_languages_unconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["a.py", "b.ts", "c.yaml"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        assert_eq!(hints.len(), 3);
    }

    #[test]
    fn hint_ordering_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["c.yaml", "a.py", "b.ts"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        let names: Vec<_> = hints.iter().map(|h| h.linter.name()).collect();
        assert_eq!(names, vec!["ruff", "eslint", "yamllint"]);
    }

    #[test]
    fn each_hint_carries_enable_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let files = pbuf(&["x.py", "y.ts", "z.yaml"]);
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let hints = detect_unconfigured_linters(dir.path(), &refs);
        for h in &hints {
            assert!(!h.enable_instruction.is_empty());
        }
    }

    // -- Linter detection --

    #[test]
    fn detect_ruff_from_pyproject_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.ruff]\nline-length = 88\n",
        )
        .unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Ruff));
    }

    #[test]
    fn detect_eslint_from_eslintrc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".eslintrc.json"), "{}").unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Eslint));
    }

    #[test]
    fn detect_clippy_from_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Clippy));
    }

    #[test]
    fn detect_no_linters_in_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.is_empty());
    }

    #[test]
    fn detect_multiple_linters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.ruff]\n").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Ruff));
        assert!(linters.contains(&LinterKind::Clippy));
    }

    // -- Ruff output normalization --

    // -- Clippy output normalization --

    // -- ESLint output normalization --

    // -- CommandRunner integration --

    // -- Linter source tags --

    // -- Yamllint detection --

    #[test]
    fn detect_yamllint_from_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".yamllint"), "extends: default\n").unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Yamllint));
    }

    #[test]
    fn detect_yamllint_from_yaml_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".yamllint.yaml"), "extends: default\n").unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Yamllint));
    }

    // -- Yamllint output normalization --

    // -- Shellcheck detection --

    #[test]
    fn detect_shellcheck_from_sh_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("deploy.sh"), "#!/bin/bash\necho hi\n").unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Shellcheck));
    }

    // -- Hadolint detection --

    #[test]
    fn detect_hadolint_from_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hadolint.yaml"), "ignored: [DL3008]\n").unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Hadolint));
    }

    #[test]
    fn detect_hadolint_from_dockerfile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM node:18\n").unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Hadolint));
    }

    // -- Shellcheck output normalization --

    // -- Hadolint output normalization --

    // -- Tflint detection --

    #[test]
    fn detect_tflint_from_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".tflint.hcl"),
            "plugin \"terraform\" {\n  enabled = true\n}\n",
        )
        .unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Tflint));
    }

    #[test]
    fn detect_tflint_from_tf_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            "resource \"aws_instance\" \"web\" {}\n",
        )
        .unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Tflint));
    }

    // -- Tflint output normalization --

    // -- golangci-lint --

    #[test]
    fn detect_golangcilint_from_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".golangci.yml"),
            "linters:\n  enable:\n    - errcheck\n",
        )
        .unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Golangcilint));
    }

    #[test]
    fn detect_golangcilint_from_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/app\n\ngo 1.21\n",
        )
        .unwrap();
        let linters = detect_linters(dir.path());
        assert!(linters.contains(&LinterKind::Golangcilint));
    }
}
