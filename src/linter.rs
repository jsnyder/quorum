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
    Yamllint,
    Shellcheck,
    Hadolint,
    Tflint,
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
            .filter(|p| ext_of(p).as_deref().map_or(false, matches))
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
        LinterKind::Ruff => {
            runner.run("ruff", &["check", "--output-format=json", &file_str], cwd)?
        }
        LinterKind::Clippy => runner.run(
            "cargo",
            &["clippy", "--message-format=json", "--", "-W", "clippy::all"],
            cwd,
        )?,
        LinterKind::Eslint => runner.run("eslint", &["--format=json", &file_str], cwd)?,
        LinterKind::Yamllint => runner.run("yamllint", &["-f", "parsable", &file_str], cwd)?,
        LinterKind::Shellcheck => runner.run("shellcheck", &["--format=json1", &file_str], cwd)?,
        LinterKind::Hadolint => runner.run("hadolint", &["--format", "tty", &file_str], cwd)?,
        LinterKind::Tflint => {
            runner.run("tflint", &["--format=json", "--force", &file_str], cwd)?
        }
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
        LinterKind::Yamllint => normalize_yamllint_output(&output.stdout),
        LinterKind::Shellcheck => normalize_shellcheck_output(&output.stdout),
        LinterKind::Hadolint => normalize_hadolint_output(&output.stdout),
        LinterKind::Tflint => normalize_tflint_output(&output.stdout),
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
            category: category.into(),
            source: Source::Linter("ruff".into()),
            line_start: row,
            line_end: end_row,
            evidence: vec![format!("ruff {}", code)],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
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
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
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
                suggested_fix: None,
                based_on_excerpt: None,
                reasoning: None,
                confidence: None,
                cited_lines: None,
                grounding_status: None,
            });
        }
    }

    Ok(findings)
}

pub fn normalize_yamllint_output(output: &str) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    // yamllint parsable format: file:line:col: [level] message (rule)
    // Find the level marker to reliably split, then extract line number
    for line in output.lines() {
        let level_idx = line.find(" [error]").or_else(|| line.find(" [warning]"));
        let (line_num, rest) = if let Some(idx) = level_idx {
            // Everything before marker is "file:line:col"
            let prefix = &line[..idx];
            // Split prefix by ':' and take the second-to-last as line number
            let colon_parts: Vec<&str> = prefix.split(':').collect();
            // Parts: [file, line, col] -- line is second-to-last before col
            let line_n = if colon_parts.len() >= 3 {
                colon_parts[colon_parts.len() - 3]
                    .trim()
                    .parse::<u32>()
                    .unwrap_or(1)
            } else {
                1
            };
            (line_n, line[idx + 1..].trim()) // skip the space before [level]
        } else {
            continue; // skip lines without a recognized level marker
        };

        let (severity, message) = if rest.starts_with("[error]") {
            (Severity::High, rest.trim_start_matches("[error]").trim())
        } else if rest.starts_with("[warning]") {
            (
                Severity::Medium,
                rest.trim_start_matches("[warning]").trim(),
            )
        } else {
            (Severity::Low, rest)
        };

        findings.push(Finding {
            title: format!("yamllint: {}", message),
            description: message.to_string(),
            severity,
            category: "lint".into(),
            source: Source::Linter("yamllint".into()),
            line_start: line_num,
            line_end: line_num,
            evidence: vec!["yamllint".into()],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
        });
    }
    Ok(findings)
}

pub fn normalize_shellcheck_output(json_output: &str) -> anyhow::Result<Vec<Finding>> {
    // shellcheck --format=json1 outputs: {"comments": [{file, line, endLine, column, endColumn, level, code, message}]}
    let wrapper: serde_json::Value = serde_json::from_str(json_output)?;
    let comments = wrapper.get("comments").and_then(|c| c.as_array());
    let mut findings = Vec::new();
    if let Some(items) = comments {
        for item in items {
            let code = item["code"].as_u64().unwrap_or(0);
            let message = item["message"].as_str().unwrap_or("");
            let line = item["line"].as_u64().unwrap_or(1) as u32;
            let end_line = item["endLine"].as_u64().unwrap_or(line as u64) as u32;
            let level = item["level"].as_str().unwrap_or("warning");

            let severity = match level {
                "error" => Severity::High,
                "warning" => Severity::Medium,
                "info" => Severity::Low,
                "style" => Severity::Info,
                _ => Severity::Low,
            };

            findings.push(Finding {
                title: format!("SC{}: {}", code, message),
                description: message.to_string(),
                severity,
                category: "lint".into(),
                source: Source::Linter("shellcheck".into()),
                line_start: line,
                line_end: end_line,
                evidence: vec![format!("shellcheck SC{}", code)],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
                suggested_fix: None,
                based_on_excerpt: None,
                reasoning: None,
                confidence: None,
                cited_lines: None,
                grounding_status: None,
            });
        }
    }
    Ok(findings)
}

pub fn normalize_hadolint_output(output: &str) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    // hadolint tty format: file:line rule level: message
    // Example: "Dockerfile:3 DL3008 warning: Pin versions in apt get install"
    for line in output.lines() {
        // Split on first space after file:line
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() < 2 {
            continue;
        }

        // Extract line number from "file:line"
        let loc_parts: Vec<&str> = parts[0].rsplitn(2, ':').collect();
        let line_num = if loc_parts.len() >= 2 {
            loc_parts[0].trim().parse::<u32>().unwrap_or(1)
        } else {
            1
        };

        let rest = parts[1];
        // rest format: "DL3008 warning: Pin versions..."
        let rest_parts: Vec<&str> = rest.splitn(3, ' ').collect();
        if rest_parts.len() < 3 {
            continue;
        }

        let rule = rest_parts[0];
        let level_raw = rest_parts[1].trim_end_matches(':');
        let message = rest_parts[2];

        let severity = match level_raw {
            "error" => Severity::High,
            "warning" => Severity::Medium,
            "info" => Severity::Low,
            _ => Severity::Low,
        };

        findings.push(Finding {
            title: format!("{}: {}", rule, message),
            description: message.to_string(),
            severity,
            category: "lint".into(),
            source: Source::Linter("hadolint".into()),
            line_start: line_num,
            line_end: line_num,
            evidence: vec![format!("hadolint {}", rule)],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
        });
    }
    Ok(findings)
}

pub fn normalize_tflint_output(json_output: &str) -> anyhow::Result<Vec<Finding>> {
    let wrapper: serde_json::Value = serde_json::from_str(json_output)?;
    let issues = wrapper.get("issues").and_then(|i| i.as_array());
    let mut findings = Vec::new();

    if let Some(items) = issues {
        for item in items {
            let rule_name = item["rule"]["name"].as_str().unwrap_or("unknown");
            let severity_str = item["rule"]["severity"].as_str().unwrap_or("warning");
            let message = item["message"].as_str().unwrap_or("");
            let line_start = item["range"]["start"]["line"].as_u64().unwrap_or(1) as u32;
            let line_end = item["range"]["end"]["line"]
                .as_u64()
                .unwrap_or(line_start as u64) as u32;

            let severity = match severity_str {
                "error" => Severity::High,
                "warning" => Severity::Medium,
                "notice" => Severity::Low,
                _ => Severity::Low,
            };

            findings.push(Finding {
                title: format!("{}: {}", rule_name, message),
                description: message.to_string(),
                severity,
                category: "lint".into(),
                source: Source::Linter("tflint".into()),
                line_start,
                line_end,
                evidence: vec![format!("tflint {}", rule_name)],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
                suggested_fix: None,
                based_on_excerpt: None,
                reasoning: None,
                confidence: None,
                cited_lines: None,
                grounding_status: None,
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
        assert_eq!(findings[0].category, "maintainability");
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

    #[test]
    fn normalize_yamllint_valid_output() {
        let output = "config.yaml:3:1: [error] duplication of key \"api_key\" in mapping (key-duplicates)\nconfig.yaml:5:1: [warning] line too long (120 > 80 characters) (line-length)\n";
        let findings = normalize_yamllint_output(output).unwrap();
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].line_start, 3);
        assert_eq!(findings[1].severity, Severity::Medium);
        assert_eq!(findings[1].line_start, 5);
        assert_eq!(findings[0].source, Source::Linter("yamllint".into()));
    }

    #[test]
    fn normalize_yamllint_empty_output() {
        let findings = normalize_yamllint_output("").unwrap();
        assert!(findings.is_empty());
    }

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

    #[test]
    fn normalize_shellcheck_valid_output() {
        let json = r#"{"comments":[{"file":"test.sh","line":3,"endLine":3,"column":1,"endColumn":6,"level":"warning","code":2086,"message":"Double quote to prevent globbing and word splitting."}]}"#;
        let findings = normalize_shellcheck_output(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line_start, 3);
        assert!(findings[0].title.contains("SC2086"));
        assert_eq!(findings[0].severity, Severity::Medium);
        assert_eq!(findings[0].source, Source::Linter("shellcheck".into()));
    }

    #[test]
    fn normalize_shellcheck_empty() {
        let json = r#"{"comments":[]}"#;
        let findings = normalize_shellcheck_output(json).unwrap();
        assert!(findings.is_empty());
    }

    // -- Hadolint output normalization --

    #[test]
    fn normalize_hadolint_valid_output() {
        let output = "Dockerfile:3 DL3008 warning: Pin versions in apt get install\nDockerfile:1 DL3006 warning: Always tag the version of an image explicitly\n";
        let findings = normalize_hadolint_output(output).unwrap();
        assert_eq!(findings.len(), 2);
        assert!(findings[0].title.contains("DL3008"));
        assert_eq!(findings[0].line_start, 3);
        assert_eq!(findings[1].line_start, 1);
        assert_eq!(findings[0].source, Source::Linter("hadolint".into()));
    }

    #[test]
    fn normalize_hadolint_empty() {
        let findings = normalize_hadolint_output("").unwrap();
        assert!(findings.is_empty());
    }

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

    #[test]
    fn normalize_tflint_valid_output() {
        let json = r#"{"issues":[{"rule":{"name":"aws_instance_invalid_type","severity":"error","link":"https://example.com"},"message":"\"t2.nano\" is an invalid instance type.","range":{"filename":"main.tf","start":{"line":3,"column":17},"end":{"line":3,"column":29}},"callers":[]}],"errors":[]}"#;
        let findings = normalize_tflint_output(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].title.contains("aws_instance_invalid_type"));
        assert_eq!(findings[0].line_start, 3);
        assert_eq!(findings[0].line_end, 3);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].source, Source::Linter("tflint".into()));
    }

    #[test]
    fn normalize_tflint_warning_severity() {
        let json = r#"{"issues":[{"rule":{"name":"terraform_deprecated_interpolation","severity":"warning","link":""},"message":"Interpolation-only expressions are deprecated.","range":{"filename":"main.tf","start":{"line":5,"column":10},"end":{"line":5,"column":30}},"callers":[]}],"errors":[]}"#;
        let findings = normalize_tflint_output(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Medium);
    }

    #[test]
    fn normalize_tflint_notice_severity() {
        let json = r#"{"issues":[{"rule":{"name":"terraform_naming_convention","severity":"notice","link":""},"message":"resource name should be snake_case","range":{"filename":"main.tf","start":{"line":1,"column":1},"end":{"line":1,"column":20}},"callers":[]}],"errors":[]}"#;
        let findings = normalize_tflint_output(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Low);
    }

    #[test]
    fn normalize_tflint_empty_issues() {
        let json = r#"{"issues":[],"errors":[]}"#;
        let findings = normalize_tflint_output(json).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn normalize_tflint_malformed_json() {
        assert!(normalize_tflint_output("not json").is_err());
    }

    #[test]
    fn run_tflint_via_runner() {
        let json = r#"{"issues":[{"rule":{"name":"test_rule","severity":"warning","link":""},"message":"test message","range":{"filename":"main.tf","start":{"line":1,"column":1},"end":{"line":1,"column":10}},"callers":[]}],"errors":[]}"#;
        let runner = FakeCommandRunner::with_exit_code(json, 2);
        let file = PathBuf::from("main.tf");
        let cwd = PathBuf::from(".");
        let findings = run_linter(&LinterKind::Tflint, &file, &cwd, &runner).unwrap();
        assert_eq!(findings.len(), 1);
    }
}
