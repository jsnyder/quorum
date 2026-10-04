//! Repo-defined review configuration: `.quorum/review.toml` (#632).
//!
//! A repository states what it wants reviewed, where, and what it already
//! knows about itself:
//!
//! ```toml
//! [project]
//! notes = ["This is a binary crate; the lib target has no out-of-tree consumers."]
//!
//! [[scope]]
//! paths = ["tests/**"]
//! axes = ["testing-antipatterns", "correctness"]
//!
//! [[scope]]
//! paths = ["eval/corpus/**"]
//! axes = []            # no LLM review; AST rules still run
//!
//! [[suppress]]
//! pattern = "struct literal"
//! reason = "every literal is in-tree"
//! ```
//!
//! Read once per run from the project root, next to `.quorum/suppress.toml`,
//! which keeps working. A missing file is an empty config; a file that does
//! not parse is a warning and an empty config, the same contract the
//! suppression file has, so a typo cannot fail a review.

use serde::Deserialize;
use std::path::Path;

use crate::suppress::SuppressionRule;

/// Project notes are prompt text the repository controls, which on a fork PR
/// is text the contributor controls. Capped so they cannot crowd out the
/// code; sandbox tags are defanged where the block is rendered.
pub const MAX_NOTES_BYTES: usize = 2048;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ReviewConfig {
    #[serde(default)]
    pub project: Project,
    #[serde(default, rename = "scope")]
    pub scopes: Vec<Scope>,
    #[serde(default)]
    pub suppress: Vec<SuppressionRule>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Project {
    /// Facts a reviewer cannot read from one file.
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scope {
    /// Globs relative to the project root; `*` does not cross `/`, `**` does.
    pub paths: Vec<String>,
    /// A set name (`"default"`, `"audit"`), a list of axis names, or `[]`
    /// for "no LLM review of these paths".
    pub axes: AxesSpec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AxesSpec {
    Named(String),
    List(Vec<String>),
}

impl AxesSpec {
    /// The names as `--axes` would receive them; set names are expanded by
    /// the same code that expands them on the command line.
    pub fn names(&self) -> Vec<String> {
        match self {
            AxesSpec::Named(n) => vec![n.clone()],
            AxesSpec::List(l) => l.clone(),
        }
    }
}

/// The scope chosen for a file, with the glob that chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeMatch {
    pub glob: String,
    pub axes: Vec<String>,
}

pub fn parse(toml_str: &str) -> anyhow::Result<ReviewConfig> {
    if toml_str.trim().is_empty() {
        return Ok(ReviewConfig::default());
    }
    Ok(toml::from_str(toml_str)?)
}

/// Load `<project_root>/.quorum/review.toml`.
pub fn load(project_root: &Path) -> ReviewConfig {
    let path = project_root.join(".quorum/review.toml");
    match std::fs::read_to_string(&path) {
        Ok(contents) => parse(&contents).unwrap_or_else(|e| {
            eprintln!("Warning: Failed to parse {}: {}", path.display(), e);
            ReviewConfig::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ReviewConfig::default(),
        Err(e) => {
            eprintln!("Warning: Could not read {}: {}", path.display(), e);
            ReviewConfig::default()
        }
    }
}

fn glob_matches(glob: &str, rel_path: &str) -> bool {
    // The same options the suppression file's `file` globs use.
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    match glob::Pattern::new(glob) {
        Ok(p) => p.matches_with(rel_path, opts),
        Err(_) => glob == rel_path,
    }
}

impl ReviewConfig {
    /// The scope that applies to `rel_path` (relative to the project root,
    /// `/`-separated), or `None` when no scope names it.
    ///
    /// Most specific wins: the longest matching glob, on the reasoning that
    /// `src/llm_client.rs` says more about a file than `src/**` does. Equal
    /// lengths go to the later scope, so a file can be re-scoped by adding a
    /// rule at the bottom.
    pub fn scope_for(&self, rel_path: &str) -> Option<ScopeMatch> {
        let rel = rel_path.replace('\\', "/");
        let mut best: Option<ScopeMatch> = None;
        for scope in &self.scopes {
            for g in &scope.paths {
                let g = g.replace('\\', "/");
                if !glob_matches(&g, &rel) {
                    continue;
                }
                if best.as_ref().is_none_or(|b| g.len() >= b.glob.len()) {
                    best = Some(ScopeMatch {
                        glob: g,
                        axes: scope.axes.names(),
                    });
                }
            }
        }
        best
    }

    /// The project notes as one block for `<review_context>`, or `None`
    /// when there are none. Truncated at `MAX_NOTES_BYTES` on a line
    /// boundary, with a marker, so an over-long list fails visibly.
    pub fn notes_block(&self) -> Option<String> {
        let notes: Vec<&str> = self
            .project
            .notes
            .iter()
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .collect();
        if notes.is_empty() {
            return None;
        }
        let mut out = String::new();
        for n in notes {
            let line = format!("- {}\n", n.replace('\n', " "));
            if out.len() + line.len() > MAX_NOTES_BYTES {
                out.push_str("- (further project notes omitted: over the 2 KiB limit)\n");
                break;
            }
            out.push_str(&line);
        }
        Some(out)
    }
}

/// `path` relative to `project_root`, `/`-separated; the path unchanged
/// when it is not under the root.
pub fn relative_to_root(path: &Path, project_root: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    abs.strip_prefix(&root)
        .unwrap_or(&abs)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[project]
notes = ["Binary crate; no out-of-tree consumers.", "eval/axes/corpus is deliberately defective."]

[[scope]]
paths = ["src/**"]
axes = "default"

[[scope]]
paths = ["tests/**", "src/**/tests.rs"]
axes = ["testing-antipatterns", "correctness"]

[[scope]]
paths = ["eval/corpus/**"]
axes = []

[[scope]]
paths = ["src/llm_client.rs"]
axes = "audit"

[[suppress]]
pattern = "struct literal"
reason = "in-tree only"
"#;

    #[test]
    fn parses_notes_scopes_and_suppressions() {
        let c = parse(SAMPLE).unwrap();
        assert_eq!(c.project.notes.len(), 2);
        assert_eq!(c.scopes.len(), 4);
        assert_eq!(c.suppress.len(), 1);
        assert_eq!(c.suppress[0].pattern, "struct literal");
    }

    #[test]
    fn the_longest_matching_glob_wins() {
        let c = parse(SAMPLE).unwrap();
        let m = c.scope_for("src/llm_client.rs").unwrap();
        assert_eq!(
            (m.glob.as_str(), m.axes),
            ("src/llm_client.rs", vec!["audit".to_string()])
        );
        // `src/**/tests.rs` (15) is longer than `src/**` (6).
        assert_eq!(
            c.scope_for("src/mcp/tests.rs").unwrap().axes,
            ["testing-antipatterns", "correctness"]
        );
        assert_eq!(c.scope_for("src/main.rs").unwrap().axes, ["default"]);
    }

    #[test]
    fn an_empty_axes_list_is_a_match_that_selects_nothing() {
        let c = parse(SAMPLE).unwrap();
        let m = c.scope_for("eval/corpus/python/client.py").unwrap();
        assert!(m.axes.is_empty(), "{m:?}");
    }

    #[test]
    fn a_path_no_scope_names_has_no_scope() {
        let c = parse(SAMPLE).unwrap();
        assert_eq!(c.scope_for("README.md"), None);
        // A glob is anchored at the project root: `tests/**` is not `docs/tests/..`.
        assert_eq!(c.scope_for("docs/tests/x.rs"), None);
    }

    #[test]
    fn a_single_star_does_not_cross_a_directory() {
        let c = parse("[[scope]]\npaths = [\"src/*.rs\"]\naxes = [\"security\"]\n").unwrap();
        assert!(c.scope_for("src/lib.rs").is_some());
        assert_eq!(c.scope_for("src/mcp/handler.rs"), None);
    }

    #[test]
    fn equal_length_globs_go_to_the_later_scope() {
        let c = parse(
            "[[scope]]\npaths = [\"a/**\"]\naxes = [\"security\"]\n\n[[scope]]\npaths = [\"a/**\"]\naxes = [\"correctness\"]\n",
        )
        .unwrap();
        assert_eq!(c.scope_for("a/x.rs").unwrap().axes, ["correctness"]);
    }

    #[test]
    fn notes_render_as_a_list_and_are_capped() {
        let c = parse(SAMPLE).unwrap();
        assert_eq!(
            c.notes_block().unwrap(),
            "- Binary crate; no out-of-tree consumers.\n- eval/axes/corpus is deliberately defective.\n"
        );
        assert_eq!(parse("").unwrap().notes_block(), None);
        assert_eq!(
            parse("[project]\nnotes = [\"  \"]\n")
                .unwrap()
                .notes_block(),
            None
        );

        let long = format!(
            "[project]\nnotes = [{}]\n",
            vec!["\"0123456789 0123456789 0123456789 0123456789 0123456789\""; 100].join(", ")
        );
        let block = parse(&long).unwrap().notes_block().unwrap();
        assert!(block.len() <= MAX_NOTES_BYTES + 80, "{}", block.len());
        assert!(block.ends_with("over the 2 KiB limit)\n"), "{block}");
    }

    #[test]
    fn a_missing_file_is_empty_and_a_broken_one_does_not_fail_the_review() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).scopes.is_empty());
        std::fs::create_dir_all(dir.path().join(".quorum")).unwrap();
        std::fs::write(
            dir.path().join(".quorum/review.toml"),
            "[[scope]\npaths = 3",
        )
        .unwrap();
        let c = load(dir.path());
        assert!(c.scopes.is_empty() && c.suppress.is_empty());
        std::fs::write(dir.path().join(".quorum/review.toml"), SAMPLE).unwrap();
        assert_eq!(load(dir.path()).scopes.len(), 4);
    }

    #[test]
    fn paths_are_made_relative_to_the_project_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let f = dir.path().join("src/lib.rs");
        std::fs::write(&f, "").unwrap();
        assert_eq!(relative_to_root(&f, dir.path()), "src/lib.rs");
    }
}
