//! Shared file-path utilities for same-file precedent matching.

/// Additive similarity boost for candidates whose file path matches the
/// file currently under review.
pub const SAME_FILE_BOOST: f32 = 0.05;

/// Normalize a file path for comparison: strip leading `./` and trailing `/`.
pub fn normalize_file_path(path: &str) -> &str {
    let p = path.strip_prefix("./").unwrap_or(path);
    p.strip_suffix('/').unwrap_or(p)
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
}
