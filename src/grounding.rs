use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static BACKTICK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").unwrap());

static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // Rust
        "self", "Self", "super", "crate", "true", "false", "None", "Some",
        "unwrap", "expect", "clone", "iter", "into", "from", "default",
        "push", "is_empty", "map", "filter", "collect", "Result", "Option",
        "String", "Vec", "Box", "Arc", "Mutex",
        // Python
        "True", "False", "print", "list", "dict", "str", "int", "float",
        "bool", "type", "init",
        // TypeScript/JS
        "this", "null", "undefined", "console", "log",
        "length", "toString", "Promise",
    ]
    .into_iter()
    .collect()
});

const MIN_IDENTIFIER_LEN: usize = 4;

/// Extract backtick-delimited identifiers from text, filtering stopwords
/// and short tokens.
pub fn extract_identifiers(text: &str) -> Vec<&str> {
    BACKTICK_RE
        .captures_iter(text)
        .filter_map(|cap| {
            let id = cap.get(1).unwrap().as_str().trim();
            if id.len() >= MIN_IDENTIFIER_LEN && !STOPWORDS.contains(id) {
                Some(id)
            } else {
                None
            }
        })
        .collect()
}

/// Extract identifiers from finding title first; fall back to description
/// if the title yields nothing.
pub fn extract_identifiers_from_finding_text<'a>(title: &'a str, description: &'a str) -> Vec<&'a str> {
    let mut ids = extract_identifiers(title);
    if ids.is_empty() {
        ids = extract_identifiers(description);
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_backtick_identifiers_from_title() {
        let ids = extract_identifiers("Function `parse_unified_diff` panics on single-line hunks");
        assert_eq!(ids, vec!["parse_unified_diff"]);
    }

    #[test]
    fn extracts_multiple_identifiers() {
        let ids = extract_identifiers("`foo_bar` and `bar_baz` are both wrong");
        assert_eq!(ids, vec!["foo_bar", "bar_baz"]);
    }

    #[test]
    fn returns_empty_for_no_backticks() {
        let ids = extract_identifiers("Missing null check on return value");
        assert!(ids.is_empty());
    }

    #[test]
    fn filters_short_identifiers() {
        let ids = extract_identifiers("`fn` and `Ok` and `parse_diff` are mentioned");
        assert_eq!(ids, vec!["parse_diff"]);
    }

    #[test]
    fn filters_language_stopwords() {
        let ids = extract_identifiers("`self` calls `unwrap` on `parse_config`");
        assert_eq!(ids, vec!["parse_config"]);
    }

    #[test]
    fn empty_backtick_content_ignored() {
        let ids = extract_identifiers("some `` empty backticks");
        assert!(ids.is_empty());
    }

    #[test]
    fn backtick_with_whitespace_only() {
        let ids = extract_identifiers("some `   ` whitespace");
        assert!(ids.is_empty());
    }

    #[test]
    fn stoplist_entry_exact_match_only() {
        // "unwrap_or" should NOT be filtered even though "unwrap" is on the stoplist
        let ids = extract_identifiers("`unwrap_or` should pass");
        assert_eq!(ids, vec!["unwrap_or"]);
    }

    #[test]
    fn extracts_from_description_too() {
        let ids = extract_identifiers_from_finding_text(
            "Missing error handling",
            "The function `process_data` at line 42 swallows the error",
        );
        assert_eq!(ids, vec!["process_data"]);
    }

    #[test]
    fn multibyte_utf8_identifier() {
        let ids = extract_identifiers("`some_func` and `\u{65e5}\u{672c}\u{8a9e}\u{30c6}\u{30b9}\u{30c8}` both present");
        assert_eq!(ids.len(), 2);
    }
}
