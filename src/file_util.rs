//! Shared file-path utilities for same-file precedent matching.

/// Additive similarity boost for candidates whose file path matches the
/// file currently under review.
pub const SAME_FILE_BOOST: f32 = 0.05;

/// Normalize a file path for comparison: strip leading `./` and trailing `/`.
pub fn normalize_file_path(path: &str) -> &str {
    let p = path.strip_prefix("./").unwrap_or(path);
    p.strip_suffix('/').unwrap_or(p)
}

/// Deep path normalization: strip `.`, `..`, root, and trailing `/` to produce
/// a clean relative path for cross-source matching (issue #307).
///
/// Unlike [`normalize_file_path`] (which only handles `./` prefix), this
/// handles `../../../x`, `/absolute/paths/x`, and mixed cases.
pub fn normalize_file_path_deep(raw: &str) -> String {
    let mut components: Vec<&str> = Vec::new();
    for seg in raw.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if !components.is_empty() {
                    components.pop();
                }
            }
            _ => components.push(seg),
        }
    }
    components.join("/")
}

/// Check if the shorter (normalized) path is a complete suffix of the longer
/// path at a `/` boundary. Requires at least 2 components in the shorter path
/// to avoid false matches on filename alone.
pub fn path_suffix_eq(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if !shorter.contains('/') {
        return false;
    }
    longer.ends_with(shorter)
        && longer.len() > shorter.len()
        && longer.as_bytes()[longer.len() - shorter.len() - 1] == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_dot_slash() {
        assert_eq!(normalize_file_path("./src/main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(normalize_file_path("src/dir/"), "src/dir");
    }

    #[test]
    fn normalize_noop_clean_path() {
        assert_eq!(normalize_file_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_strips_both() {
        assert_eq!(normalize_file_path("./src/dir/"), "src/dir");
    }

    #[test]
    fn normalize_empty_string() {
        assert_eq!(normalize_file_path(""), "");
    }

    // --- normalize_file_path_deep tests ---

    #[test]
    fn deep_strips_dot_dot_prefix() {
        assert_eq!(
            normalize_file_path_deep("../../../samples/rust/patterns.rs"),
            "samples/rust/patterns.rs"
        );
    }

    #[test]
    fn deep_strips_dot_prefix() {
        assert_eq!(normalize_file_path_deep("./src/main.rs"), "src/main.rs");
    }

    #[test]
    fn deep_strips_root() {
        assert_eq!(
            normalize_file_path_deep("/Users/jsnyder/Sources/repo/src/main.rs"),
            "Users/jsnyder/Sources/repo/src/main.rs"
        );
    }

    #[test]
    fn deep_clean_relative_unchanged() {
        assert_eq!(normalize_file_path_deep("src/calibrate.rs"), "src/calibrate.rs");
    }

    #[test]
    fn deep_empty() {
        assert_eq!(normalize_file_path_deep(""), "");
        assert_eq!(normalize_file_path_deep(".."), "");
        assert_eq!(normalize_file_path_deep("./"), "");
    }

    #[test]
    fn deep_collapses_double_slashes() {
        assert_eq!(normalize_file_path_deep("src//main.rs"), "src/main.rs");
    }

    #[test]
    fn deep_resolves_interior_dotdot() {
        assert_eq!(normalize_file_path_deep("a/../b.rs"), "b.rs");
        assert_eq!(normalize_file_path_deep("src/sub/../../main.rs"), "main.rs");
    }

    // --- path_suffix_eq tests ---

    #[test]
    fn suffix_eq_exact_match() {
        assert!(path_suffix_eq("src/main.rs", "src/main.rs"));
    }

    #[test]
    fn suffix_eq_absolute_vs_relative() {
        assert!(path_suffix_eq(
            "Users/jsnyder/Sources/repo/src/main.rs",
            "src/main.rs"
        ));
    }

    #[test]
    fn suffix_eq_symmetric() {
        assert!(path_suffix_eq(
            "src/main.rs",
            "Users/jsnyder/Sources/repo/src/main.rs"
        ));
    }

    #[test]
    fn suffix_eq_rejects_filename_only() {
        assert!(!path_suffix_eq("main.rs", "Users/repo/src/main.rs"));
    }

    #[test]
    fn suffix_eq_rejects_partial_component() {
        assert!(!path_suffix_eq("rc/main.rs", "src/main.rs"));
    }

    #[test]
    fn suffix_eq_rejects_empty() {
        assert!(!path_suffix_eq("", "src/main.rs"));
        assert!(!path_suffix_eq("src/main.rs", ""));
    }

    #[test]
    fn suffix_eq_different_repos_same_relative() {
        assert!(!path_suffix_eq(
            "project-a/src/main.rs",
            "project-b/src/main.rs"
        ));
    }
}
