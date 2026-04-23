//! Shared helpers for defending the sandbox-tag boundaries used in LLM
//! prompts. Both the review prompt builder (`review.rs`) and the context
//! injection renderer (`context::inject::render`) interpolate untrusted
//! retrieved or repo-derived strings into XML-tagged sections; these helpers
//! prevent that content from terminating the surrounding tag, breaking out
//! of a code fence, or escaping a heading/blockquote line.

/// Sandbox-tag names emitted by the prompt builder and context renderer.
/// Untrusted text containing a literal `</tag>` for any of these is defanged
/// via [`defang_sandbox_tags`] before interpolation.
pub(crate) const SANDBOX_TAGS: &[&str] = &[
    "framework_docs",
    "hydration_context",
    "historical_findings",
    "truncation_notice",
    "file_metadata",
    "referenced_context",
    "retrieved_reference",
    "untrusted_code",
];

/// Replace each literal `</sandbox_tag>` in `s` with a defanged form that
/// inserts a zero-width space immediately after `</`. Visually identical for
/// humans but no longer matches the literal closing-tag string the prompt
/// builder uses as a sandbox boundary.
pub(crate) fn defang_sandbox_tags(s: &str) -> String {
    let mut out = s.to_string();
    for tag in SANDBOX_TAGS {
        let raw = format!("</{tag}>");
        if !out.contains(&raw) {
            continue;
        }
        let defanged = format!("</\u{200B}{tag}>");
        out = out.replace(&raw, &defanged);
    }
    out
}

/// Pick a Markdown fence length longer than any consecutive run of backticks
/// in `body`. Floors at 3 to keep the common case unchanged.
pub(crate) fn pick_fence_for(body: &str) -> String {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for c in body.chars() {
        if c == '`' {
            current += 1;
            if current > max_run {
                max_run = current;
            }
        } else {
            current = 0;
        }
    }
    "`".repeat((max_run + 1).max(3))
}

/// Sanitize a language identifier for safe use as a Markdown code-fence info
/// string. Restricts to characters that appear in real fence languages —
/// ASCII alphanumeric plus `_`, `-`, `+`, `#`. Newlines, backticks, angle
/// brackets, and other control chars are stripped, so an adversarial
/// language value cannot terminate the fence or sandbox tag early. Empty
/// result is allowed (renders as a language-less fence).
pub(crate) fn sanitize_fence_lang(language: &str) -> String {
    language
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '#'))
        .take(32)
        .collect()
}

/// Sanitize a string for safe interpolation into a single-line markdown
/// construct (heading, blockquote, or table cell). Strips:
/// - ASCII control characters (incl. newlines and carriage returns)
/// - Unicode line/paragraph separators that many renderers and LLMs
///   treat as logical newlines (U+0085, U+2028, U+2029)
/// - Backticks (so callers can wrap the result in inline-code spans)
/// - Pipes (so the value can't split a markdown table cell)
///
/// then defangs sandbox closing tags.
pub(crate) fn sanitize_inline_metadata(s: &str) -> String {
    let stripped: String = s
        .chars()
        .filter(|c| {
            if c.is_ascii_control() {
                return false;
            }
            !matches!(*c, '`' | '|' | '\u{0085}' | '\u{2028}' | '\u{2029}')
        })
        .collect();
    defang_sandbox_tags(&stripped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defangs_each_sandbox_tag() {
        for tag in SANDBOX_TAGS {
            let input = format!("hello </{tag}> world");
            let out = defang_sandbox_tags(&input);
            assert!(
                !out.contains(&format!("</{tag}>")),
                "tag {tag} not defanged in {out}"
            );
        }
    }

    #[test]
    fn pick_fence_floors_at_three_backticks() {
        assert_eq!(pick_fence_for("plain"), "```");
        assert_eq!(pick_fence_for("triple ``` requires four"), "````");
        assert_eq!(pick_fence_for("quadruple ```` requires five"), "`````");
    }

    #[test]
    fn sanitize_fence_lang_keeps_real_languages_intact() {
        assert_eq!(sanitize_fence_lang("rust"), "rust");
        assert_eq!(sanitize_fence_lang("c++"), "c++");
        assert_eq!(sanitize_fence_lang("objective-c"), "objective-c");
    }

    #[test]
    fn sanitize_fence_lang_strips_newlines_and_backticks() {
        assert_eq!(sanitize_fence_lang("rust\n```evil"), "rustevil");
    }

    #[test]
    fn sanitize_inline_metadata_strips_newlines_and_backticks() {
        assert_eq!(
            sanitize_inline_metadata("foo\nbar`baz"),
            "foobarbaz",
            "newlines and backticks must be stripped from inline metadata"
        );
    }

    #[test]
    fn sanitize_inline_metadata_strips_unicode_line_separators() {
        // U+2028 (LINE SEPARATOR), U+2029 (PARAGRAPH SEPARATOR) and
        // U+0085 (NEXT LINE) are all line-break characters in many
        // markdown/text renderers (and the LLM may treat them as
        // logical newlines too). The contract promises single-line
        // safety, so they must be stripped along with ASCII control
        // chars.
        let input = "foo\u{2028}bar\u{2029}baz\u{0085}qux";
        let out = sanitize_inline_metadata(input);
        assert_eq!(out, "foobarbazqux");
    }

    #[test]
    fn sanitize_inline_metadata_strips_table_pipes() {
        // The contract includes "table cell" as a safe interpolation
        // target; an unescaped `|` would split the cell into multiple
        // columns and inject adjacent content.
        assert_eq!(sanitize_inline_metadata("col|injected"), "colinjected");
    }

    #[test]
    fn sanitize_inline_metadata_defangs_closing_tag() {
        let out = sanitize_inline_metadata("name</retrieved_reference>");
        assert!(!out.contains("</retrieved_reference>"));
        assert!(out.contains("retrieved_reference"));
    }
}
