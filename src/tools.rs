/// Tool registry for deep review agent.
/// Provides read_file, grep, list_files — all confined to a repo root for safety.

use std::path::{Path, PathBuf};
use serde::Serialize;

const MAX_OUTPUT_CHARS: usize = 8000;
const MAX_GREP_RESULTS: usize = 20;

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

pub struct ToolRegistry {
    root: PathBuf,
}

impl ToolRegistry {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }

    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "read_file".into(),
                description: "Read file contents. Use start_line/end_line to read a specific range.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path" },
                        "start_line": { "type": "integer", "description": "Start line (1-indexed, optional)" },
                        "end_line": { "type": "integer", "description": "End line (inclusive, optional)" }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "grep".into(),
                description: "Search for a pattern across files. Returns matching lines with file paths.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Search pattern (substring match)" },
                        "path_glob": { "type": "string", "description": "File glob pattern (e.g. '*.py', optional)" },
                        "max_results": { "type": "integer", "description": "Max matches to return (default 20)" }
                    },
                    "required": ["pattern"]
                }),
            },
            ToolDefinition {
                name: "list_files".into(),
                description: "List files in the project directory.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Subdirectory to list (optional, default root)" },
                        "pattern": { "type": "string", "description": "Glob pattern filter (e.g. '*.py', optional)" }
                    }
                }),
            },
        ]
    }

    pub fn execute(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        max_output_bytes: usize,
    ) -> anyhow::Result<String> {
        let result = match tool_name {
            "read_file" => self.exec_read_file(args, max_output_bytes)?,
            "grep" => self.exec_grep(args, max_output_bytes)?,
            "list_files" => self.exec_list_files(args, max_output_bytes)?,
            _ => anyhow::bail!("Unknown tool: {}", tool_name),
        };
        // Safety net: truncate in case a tool overshoots its internal budget.
        Ok(truncate(&result, max_output_bytes))
    }

    fn resolve_path(&self, relative: &str) -> anyhow::Result<PathBuf> {
        // Block absolute paths
        if relative.starts_with('/') || relative.starts_with('\\') {
            anyhow::bail!("Absolute paths not allowed");
        }
        let resolved = self.root.join(relative).canonicalize()
            .map_err(|e| anyhow::anyhow!("Cannot resolve path '{}': {}", relative, e))?;
        // Ensure resolved path is within root
        let canon_root = self.root.canonicalize()
            .map_err(|e| anyhow::anyhow!("Cannot resolve root: {}", e))?;
        if !resolved.starts_with(&canon_root) {
            anyhow::bail!("Path traversal blocked: '{}' escapes project root", relative);
        }
        Ok(resolved)
    }

    fn exec_read_file(&self, args: &serde_json::Value, max_output_bytes: usize) -> anyhow::Result<String> {
        let path_str = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?;
        let resolved = self.resolve_path(path_str)?;

        // Read only up to budget + 1 byte to detect truncation without
        // allocating the full file.
        let file = std::fs::File::open(&resolved)?;
        let read_limit = (max_output_bytes as u64).saturating_add(1);
        let mut limited = String::new();
        use std::io::Read;
        file.take(read_limit).read_to_string(&mut limited)?;

        let start = args["start_line"].as_u64().map(|n| n as usize).unwrap_or(1);
        let end = args["end_line"].as_u64().map(|n| n as usize);

        let lines: Vec<&str> = limited.lines().collect();
        let start_idx = start.saturating_sub(1).min(lines.len());
        let end_idx = end.unwrap_or(lines.len()).min(lines.len()).max(start_idx);

        let selected: String = lines[start_idx..end_idx]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4} | {}", start_idx + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(truncate(&selected, max_output_bytes))
    }

    fn exec_grep(&self, args: &serde_json::Value, max_output_bytes: usize) -> anyhow::Result<String> {
        let pattern = args["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern required"))?;
        let max = args["max_results"].as_u64().unwrap_or(MAX_GREP_RESULTS as u64) as usize;
        let path_glob = args["path_glob"].as_str();

        let mut results = Vec::new();
        let mut total_bytes = 0usize;
        self.grep_recursive(&self.root, pattern, path_glob, &mut results, max, &mut total_bytes, max_output_bytes)?;

        if results.is_empty() {
            Ok("No matches found.".into())
        } else {
            Ok(truncate(&results.join("\n"), max_output_bytes))
        }
    }

    fn grep_recursive(
        &self,
        dir: &Path,
        pattern: &str,
        glob: Option<&str>,
        results: &mut Vec<String>,
        max: usize,
        total_bytes: &mut usize,
        byte_budget: usize,
    ) -> anyhow::Result<()> {
        if results.len() >= max || *total_bytes >= byte_budget { return Ok(()); }
        let entries = std::fs::read_dir(dir)?;
        for entry in entries.flatten() {
            if results.len() >= max || *total_bytes >= byte_budget { break; }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden dirs and common non-source dirs
            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || name == "__pycache__"
                || name == "venv"
            {
                continue;
            }
            // Skip symlinks to prevent escaping repo root
            if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                self.grep_recursive(&path, pattern, glob, results, max, total_bytes, byte_budget)?;
            } else if path.is_file() {
                if let Some(g) = glob {
                    if g != "*" {
                        let ext_match = g.trim_start_matches("*.");
                        if let Some(ext) = path.extension() {
                            if ext.to_string_lossy() != ext_match { continue; }
                        } else {
                            continue;
                        }
                    }
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let rel = path.strip_prefix(&self.root).unwrap_or(&path);
                    for (i, line) in content.lines().enumerate() {
                        if results.len() >= max || *total_bytes >= byte_budget { break; }
                        if line.contains(pattern) {
                            let entry = format!("{}:{}: {}", rel.display(), i + 1, line.trim());
                            // +1 accounts for the newline when results are joined
                            *total_bytes += entry.len() + 1;
                            results.push(entry);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn exec_list_files(&self, args: &serde_json::Value, max_output_bytes: usize) -> anyhow::Result<String> {
        let subdir = args["path"].as_str().unwrap_or("");
        let pattern = args["pattern"].as_str();
        let dir = if subdir.is_empty() { self.root.clone() } else { self.resolve_path(subdir)? };

        let mut files = Vec::new();
        let mut total_bytes = 0usize;
        self.list_recursive(&dir, pattern, &mut files, 200, &mut total_bytes, max_output_bytes)?;

        if files.is_empty() {
            Ok("No files found.".into())
        } else {
            Ok(truncate(&files.join("\n"), max_output_bytes))
        }
    }

    fn list_recursive(
        &self,
        dir: &Path,
        glob: Option<&str>,
        files: &mut Vec<String>,
        max: usize,
        total_bytes: &mut usize,
        byte_budget: usize,
    ) -> anyhow::Result<()> {
        if files.len() >= max || *total_bytes >= byte_budget { return Ok(()); }
        let entries = std::fs::read_dir(dir)?;
        for entry in entries.flatten() {
            if files.len() >= max || *total_bytes >= byte_budget { break; }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || name == "__pycache__"
                || name == "venv"
            {
                continue;
            }
            // Skip symlinks to prevent escaping repo root
            if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                self.list_recursive(&path, glob, files, max, total_bytes, byte_budget)?;
            } else {
                let rel = path.strip_prefix(&self.root).unwrap_or(&path);
                if let Some(g) = glob {
                    if g != "*" {
                        let ext = g.trim_start_matches("*.");
                        if let Some(file_ext) = path.extension() {
                            if file_ext.to_string_lossy() != ext { continue; }
                        } else {
                            continue;
                        }
                    }
                }
                let entry_str = rel.display().to_string();
                // +1 accounts for the newline when files are joined
                *total_bytes += entry_str.len() + 1;
                files.push(entry_str);
            }
        }
        Ok(())
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        const MARKER: &str = "\n... (truncated)";
        // Reserve space for the marker within the budget so the total
        // output (body + marker) never exceeds `max`.
        if max < MARKER.len() {
            // Budget too small to fit the marker; just hard-truncate.
            let safe_end = s.floor_char_boundary(max);
            s[..safe_end].to_string()
        } else {
            let body_budget = max - MARKER.len();
            let safe_end = s.floor_char_boundary(body_budget);
            format!("{}{}", &s[..safe_end], MARKER)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("main.py"),
            "def hello():\n    print('hi')\n\ndef world():\n    pass\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/auth.py"),
            "SECRET = 'abc'\ndef login(): pass\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/db.py"),
            "import sqlite3\ndef query(): pass\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn registry_lists_tools() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let defs = reg.tool_definitions();
        assert!(defs.len() >= 3);
        assert!(defs.iter().any(|t| t.name == "read_file"));
        assert!(defs.iter().any(|t| t.name == "grep"));
        assert!(defs.iter().any(|t| t.name == "list_files"));
    }

    #[test]
    fn read_file_returns_content() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("read_file", &serde_json::json!({"path": "main.py"}), usize::MAX)
            .unwrap();
        assert!(result.contains("def hello"));
    }

    #[test]
    fn read_file_with_line_range() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute(
                "read_file",
                &serde_json::json!({"path": "main.py", "start_line": 1, "end_line": 2}),
                usize::MAX,
            )
            .unwrap();
        assert!(result.contains("def hello"));
        assert!(!result.contains("def world"));
    }

    #[test]
    fn read_file_blocks_path_traversal() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg.execute(
            "read_file",
            &serde_json::json!({"path": "../../etc/passwd"}),
            usize::MAX,
        );
        assert!(result.is_err());
    }

    #[test]
    fn read_file_blocks_absolute_path() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg.execute(
            "read_file",
            &serde_json::json!({"path": "/etc/passwd"}),
            usize::MAX,
        );
        assert!(result.is_err());
    }

    #[test]
    fn read_file_truncates_large_output() {
        let dir = TempDir::new().unwrap();
        let big = "x\n".repeat(10000);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        let reg = ToolRegistry::new(dir.path());
        // Use a concrete budget — with usize::MAX there is no truncation.
        let result = reg
            .execute("read_file", &serde_json::json!({"path": "big.txt"}), MAX_OUTPUT_CHARS)
            .unwrap();
        assert!(result.len() <= MAX_OUTPUT_CHARS, "Should truncate large files");
    }

    #[test]
    fn grep_finds_matches() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("grep", &serde_json::json!({"pattern": "SECRET"}), usize::MAX)
            .unwrap();
        assert!(result.contains("SECRET"));
        assert!(result.contains("auth.py"));
    }

    #[test]
    fn grep_respects_max_results() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute(
                "grep",
                &serde_json::json!({"pattern": "def", "max_results": 2}),
                usize::MAX,
            )
            .unwrap();
        let matches: Vec<&str> = result.lines().filter(|l| l.contains("def")).collect();
        assert!(matches.len() <= 2);
    }

    #[test]
    fn list_files_returns_tree() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("list_files", &serde_json::json!({}), usize::MAX)
            .unwrap();
        assert!(result.contains("main.py"));
        assert!(result.contains("auth.py"));
    }

    #[test]
    fn list_files_with_pattern() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("list_files", &serde_json::json!({"pattern": "*.py"}), usize::MAX)
            .unwrap();
        assert!(result.contains("main.py"));
    }

    #[test]
    fn list_files_with_star_glob_returns_all() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        // Model sends "*" meaning "all files" — should not filter everything out
        let result = reg
            .execute("list_files", &serde_json::json!({"path": "src", "pattern": "*"}), usize::MAX)
            .unwrap();
        assert!(result.contains("auth.py"), "Star glob should match all files, got: {}", result);
    }

    #[test]
    fn list_files_subdir_returns_files() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("list_files", &serde_json::json!({"path": "src"}), usize::MAX)
            .unwrap();
        assert!(result.contains("auth.py"));
        assert!(result.contains("db.py"));
    }

    #[test]
    fn execute_unknown_tool_errors() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        assert!(reg.execute("nonexistent", &serde_json::json!({}), usize::MAX).is_err());
    }

    #[test]
    fn execute_read_file_respects_max_output_bytes() {
        let dir = setup_repo();
        std::fs::write(dir.path().join("big.txt"), "x".repeat(10_000)).unwrap();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("read_file", &serde_json::json!({"path": "big.txt"}), 200)
            .unwrap();
        assert!(
            result.len() <= 200,
            "read_file output {} exceeded max_output_bytes 200",
            result.len()
        );
    }

    #[test]
    fn execute_grep_respects_max_output_bytes() {
        let dir = setup_repo();
        let content: String = (0..500).map(|i| format!("match line {}\n", i)).collect();
        std::fs::write(dir.path().join("many.txt"), &content).unwrap();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("grep", &serde_json::json!({"pattern": "match"}), 300)
            .unwrap();
        assert!(
            result.len() <= 300,
            "grep output {} exceeded max_output_bytes 300",
            result.len()
        );
    }

    #[test]
    fn execute_with_large_budget_returns_full_output() {
        let dir = setup_repo();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("read_file", &serde_json::json!({"path": "main.py"}), usize::MAX)
            .unwrap();
        assert!(result.contains("print"), "full output should include file content");
    }

    #[test]
    fn read_file_does_not_allocate_beyond_budget() {
        let dir = setup_repo();
        std::fs::write(dir.path().join("huge.txt"), "y".repeat(1_000_000)).unwrap();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("read_file", &serde_json::json!({"path": "huge.txt"}), 500)
            .unwrap();
        assert!(result.len() <= 500);
    }

    #[test]
    fn grep_stops_accumulating_at_byte_budget() {
        let dir = setup_repo();
        let content: String = (0..10_000).map(|i| format!("pattern {}\n", i)).collect();
        std::fs::write(dir.path().join("huge.txt"), &content).unwrap();
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("grep", &serde_json::json!({"pattern": "pattern"}), 500)
            .unwrap();
        assert!(result.len() <= 500);
    }

    #[test]
    fn list_files_stops_accumulating_at_byte_budget() {
        let dir = setup_repo();
        for i in 0..200 {
            std::fs::write(dir.path().join(format!("file_{:04}.txt", i)), "x").unwrap();
        }
        let reg = ToolRegistry::new(dir.path());
        let result = reg
            .execute("list_files", &serde_json::json!({}), 300)
            .unwrap();
        assert!(result.len() <= 300);
    }
}
