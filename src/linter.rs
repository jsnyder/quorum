use std::path::Path;

use crate::finding::{Finding, Severity, Source};

pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> anyhow::Result<CommandOutput>;
}

pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinterKind {
    Ruff,
    Clippy,
    Eslint,
}

impl LinterKind {
    pub fn name(&self) -> &'static str {
        match self {
            LinterKind::Ruff => "ruff",
            LinterKind::Clippy => "clippy",
            LinterKind::Eslint => "eslint",
        }
    }
}

pub fn detect_linters(project_dir: &Path) -> Vec<LinterKind> {
    let mut linters = Vec::new();

    // Ruff: pyproject.toml with [tool.ruff] or ruff.toml
    if project_dir.join("ruff.toml").exists() {
        linters.push(LinterKind::Ruff);
    } else if let Ok(content) = std::fs::read_to_string(project_dir.join("pyproject.toml")) {
        if content.contains("[tool.ruff]") {
            linters.push(LinterKind::Ruff);
        }
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

    linters
}

pub fn run_linter(
    kind: &LinterKind,
    file: &Path,
    cwd: &Path,
    runner: &dyn CommandRunner,
) -> anyhow::Result<Vec<Finding>> {
    let file_str = file.to_string_lossy();
    let output = match kind {
        LinterKind::Ruff => runner.run("ruff", &["check", "--output-format=json", &file_str], cwd)?,
        LinterKind::Clippy => runner.run(
            "cargo",
            &["clippy", "--message-format=json", "--", "-W", "clippy::all"],
            cwd,
        )?,
        LinterKind::Eslint => runner.run("eslint", &["--format=json", &file_str], cwd)?,
    };

    // Linters typically exit 1 when they find issues — that's normal, not an error.
    // Exit code 2+ with empty stdout indicates a tool failure.
    if output.exit_code >= 2 && output.stdout.trim().is_empty() {
        anyhow::bail!(
            "{} failed (exit {}): {}",
            kind.name(),
            output.exit_code,
            output.stderr.lines().next().unwrap_or("unknown error")
        );
    }

    match kind {
        LinterKind::Ruff => normalize_ruff_output(&output.stdout),
        LinterKind::Clippy => normalize_clippy_output(&output.stdout),
        LinterKind::Eslint => normalize_eslint_output(&output.stdout),
    }
}

pub fn normalize_ruff_output(json_output: &str) -> anyhow::Result<Vec<Finding>> {
    let items: Vec<serde_json::Value> = serde_json::from_str(json_output)?;
    let mut findings = Vec::new();

    for item in items {
        let code = item["code"].as_str().unwrap_or("unknown");
        let message = item["message"].as_str().unwrap_or("");
        let row = item["location"]["row"].as_u64().unwrap_or(1) as u32;
        let end_row = item["end_location"]["row"].as_u64().unwrap_or(row as u64) as u32;

        let category = ruff_code_to_category(code);
        let severity = ruff_code_to_severity(code);

        findings.push(Finding {
            title: format!("{}: {}", code, message),
            description: message.to_string(),
            severity,
            category,
            source: Source::Linter("ruff".into()),
            line_start: row,
            line_end: end_row,
            evidence: vec![format!("ruff {}", code)],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }

    Ok(findings)
}

pub fn normalize_clippy_output(json_output: &str) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();

    for line in json_output.lines() {
        if line.is_empty() {
            continue;
        }
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if val["reason"].as_str() != Some("compiler-message") {
            continue;
        }
        let msg = &val["message"];
        let level = msg["level"].as_str().unwrap_or("warning");
        let message = msg["message"].as_str().unwrap_or("");
        let code = msg["code"]["code"].as_str().unwrap_or("unknown");

        let spans = &msg["spans"];
        let (line_start, line_end) = if let Some(span) = spans.as_array().and_then(|a| a.first()) {
            (
                span["line_start"].as_u64().unwrap_or(1) as u32,
                span["line_end"].as_u64().unwrap_or(1) as u32,
            )
        } else {
            (1, 1)
        };

        let severity = match level {
            "error" => Severity::High,
            "warning" => Severity::Medium,
            _ => Severity::Low,
        };

        findings.push(Finding {
            title: format!("{}: {}", code, message),
            description: message.to_string(),
            severity,
            category: "lint".into(),
            source: Source::Linter("clippy".into()),
            line_start,
            line_end,
            evidence: vec![format!("clippy {}", code)],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }

    Ok(findings)
}

pub fn normalize_eslint_output(json_output: &str) -> anyhow::Result<Vec<Finding>> {
    let files: Vec<serde_json::Value> = serde_json::from_str(json_output)?;
    let mut findings = Vec::new();

    for file in files {
        let messages = match file["messages"].as_array() {
            Some(m) => m,
            None => continue,
        };

        for msg in messages {
            let rule_id = msg["ruleId"].as_str().unwrap_or("unknown");
            let message = msg["message"].as_str().unwrap_or("");
            let line = msg["line"].as_u64().unwrap_or(1) as u32;
            let end_line = msg["endLine"].as_u64().unwrap_or(line as u64) as u32;
            let eslint_severity = msg["severity"].as_u64().unwrap_or(1);

            let severity = match eslint_severity {
                2 => Severity::High,
                1 => Severity::Medium,
                _ => Severity::Low,
            };

            findings.push(Finding {
                title: format!("{}: {}", rule_id, message),
                description: message.to_string(),
                severity,
                category: "lint".into(),
                source: Source::Linter("eslint".into()),
                line_start: line,
                line_end: end_line,
                evidence: vec![format!("eslint {}", rule_id)],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
            });
        }
    }

    Ok(findings)
}

fn ruff_code_to_category(code: &str) -> String {
    match code.chars().next() {
        Some('F') => "import".into(),
        Some('E') => "style".into(),
        Some('W') => "style".into(),
        Some('S') => "security".into(),
        Some('B') => "bug".into(),
        Some('C') => "complexity".into(),
        _ => "lint".into(),
    }
}

fn ruff_code_to_severity(code: &str) -> Severity {
    match code.chars().next() {
        Some('S') => Severity::High,
        Some('B') => Severity::Medium,
        Some('F') => Severity::Medium,
        Some('E') => Severity::Low,
        Some('W') => Severity::Low,
        _ => Severity::Info,
    }
}

#[cfg(test)]
struct FakeCommandRunner {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

#[cfg(test)]
impl FakeCommandRunner {
    fn success(stdout: &str) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    fn with_exit_code(stdout: &str, code: i32) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: code,
        }
    }

    fn not_found() -> Self {
        Self {
            stdout: String::new(),
            stderr: "command not found".into(),
            exit_code: 127,
        }
    }
}

#[cfg(test)]
impl CommandRunner for FakeCommandRunner {
    fn run(&self, _program: &str, _args: &[&str], _cwd: &Path) -> anyhow::Result<CommandOutput> {
        if self.exit_code == 127 {
            anyhow::bail!("command not found");
        }
        Ok(CommandOutput {
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
            exit_code: self.exit_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Severity, Source};
    use std::path::PathBuf;

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

    #[test]
    fn normalize_ruff_valid_output() {
        let json = r#"[
            {
                "code": "F401",
                "message": "os imported but unused",
                "filename": "test.py",
                "location": {"row": 1, "column": 1},
                "end_location": {"row": 1, "column": 10}
            }
        ]"#;
        let findings = normalize_ruff_output(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, "import");
        assert!(findings[0].title.contains("F401"));
        assert_eq!(findings[0].line_start, 1);
        assert_eq!(findings[0].source, Source::Linter("ruff".into()));
    }

    #[test]
    fn normalize_ruff_empty_output() {
        let findings = normalize_ruff_output("[]").unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn normalize_ruff_malformed_json() {
        assert!(normalize_ruff_output("not json").is_err());
    }

    // -- Clippy output normalization --

    #[test]
    fn normalize_clippy_valid_output() {
        let json = r#"{"reason":"compiler-message","message":{"code":{"code":"clippy::unwrap_used"},"level":"warning","message":"used `unwrap()` on a `Result` value","spans":[{"file_name":"src/main.rs","line_start":10,"line_end":10,"column_start":5,"column_end":20}]}}"#;
        let findings = normalize_clippy_output(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].title.contains("unwrap"));
        assert_eq!(findings[0].line_start, 10);
        assert_eq!(findings[0].source, Source::Linter("clippy".into()));
    }

    #[test]
    fn normalize_clippy_empty_output() {
        let findings = normalize_clippy_output("").unwrap();
        assert!(findings.is_empty());
    }

    // -- ESLint output normalization --

    #[test]
    fn normalize_eslint_valid_output() {
        let json = r#"[{
            "filePath": "test.ts",
            "messages": [{
                "ruleId": "no-eval",
                "severity": 2,
                "message": "eval can be harmful.",
                "line": 5,
                "endLine": 5
            }]
        }]"#;
        let findings = normalize_eslint_output(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].title.contains("no-eval"));
        assert_eq!(findings[0].line_start, 5);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].source, Source::Linter("eslint".into()));
    }

    #[test]
    fn normalize_eslint_empty_output() {
        let findings = normalize_eslint_output("[]").unwrap();
        assert!(findings.is_empty());
    }

    // -- CommandRunner integration --

    #[test]
    fn run_linter_success_returns_findings() {
        let ruff_output = r#"[{"code":"F401","message":"os imported but unused","filename":"test.py","location":{"row":1,"column":1},"end_location":{"row":1,"column":10}}]"#;
        let runner = FakeCommandRunner::with_exit_code(ruff_output, 1);
        let file = PathBuf::from("test.py");
        let cwd = PathBuf::from(".");
        let findings = run_linter(&LinterKind::Ruff, &file, &cwd, &runner).unwrap();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn run_linter_not_found_returns_error() {
        let runner = FakeCommandRunner::not_found();
        let file = PathBuf::from("test.py");
        let cwd = PathBuf::from(".");
        assert!(run_linter(&LinterKind::Ruff, &file, &cwd, &runner).is_err());
    }

    #[test]
    fn run_linter_empty_output_returns_empty() {
        let runner = FakeCommandRunner::success("[]");
        let file = PathBuf::from("test.py");
        let cwd = PathBuf::from(".");
        let findings = run_linter(&LinterKind::Ruff, &file, &cwd, &runner).unwrap();
        assert!(findings.is_empty());
    }

    // -- Linter source tags --

    #[test]
    fn findings_tagged_with_correct_source() {
        let ruff_json = r#"[{"code":"E501","message":"Line too long","filename":"t.py","location":{"row":1,"column":1},"end_location":{"row":1,"column":100}}]"#;
        let findings = normalize_ruff_output(ruff_json).unwrap();
        assert_eq!(findings[0].source, Source::Linter("ruff".into()));
    }
}
