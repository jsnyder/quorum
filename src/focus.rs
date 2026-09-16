//! Diff-first review input.
//!
//! With `--diff-file`, each axis used to receive the whole file (a 260 KB
//! `main.rs` cost ~64k tokens per axis per review) while the diff itself was
//! never shown: the parser keeps only new-side line ranges. This module
//! renders a *focused view*: the enclosing function of every changed range
//! (or the range plus context when no function contains it), with absolute
//! line numbers on every line and omitted regions marked, followed by the
//! file's unified-diff hunks so the model sees the actual edit, deletions
//! included. If the view would keep most of the file anyway, the caller
//! sends the whole file.

/// Lines of context kept around a changed range that no function contains.
pub const DEFAULT_CONTEXT_LINES: u32 = 20;
/// Above this fraction of the file kept, a focused view is not worth the
/// elision markers; send the whole file.
pub const MAX_KEPT_FRACTION: f64 = 0.6;
/// Hunk text is bounded so a mass rename cannot blow the prompt up: by
/// line count and, because one line can be a megabyte, by bytes.
pub const MAX_HUNK_LINES: usize = 400;
pub const MAX_HUNK_BYTES: usize = 64 * 1024;

/// A rendered focused view of one file.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusedView {
    /// The text to send in place of the file: numbered lines with omission
    /// markers, then the hunks.
    pub text: String,
    /// First and last absolute line present in the view.
    pub first_line: u32,
    pub last_line: u32,
    pub kept_lines: usize,
    pub total_lines: usize,
}

/// Expand each changed range to the function that contains it (or to
/// `context` lines around it), clamp to the file, and merge overlaps.
/// Ranges are 1-based inclusive.
pub fn kept_ranges(
    changed: &[(u32, u32)],
    function_spans: &[(u32, u32)],
    context: u32,
    total_lines: u32,
) -> Vec<(u32, u32)> {
    if total_lines == 0 {
        return Vec::new();
    }
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &(s, e) in changed {
        let (s, e) = (s.max(1).min(total_lines), e.max(s).min(total_lines));
        let mut lo = s;
        let mut hi = e;
        let mut covered = false;
        for &(fs, fe) in function_spans {
            // A span overlapping the change contains (part of) it.
            if fs <= e && fe >= s {
                lo = lo.min(fs);
                hi = hi.max(fe);
                covered = true;
            }
        }
        if !covered {
            lo = s.saturating_sub(context).max(1);
            hi = e.saturating_add(context).min(total_lines);
        }
        out.push((lo, hi));
    }
    out.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (s, e) in out {
        match merged.last_mut() {
            // Adjacent or overlapping: extend.
            Some(last) if s <= last.1.saturating_add(1) => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// Render the focused view, or `None` when the view would keep more than
/// [`MAX_KEPT_FRACTION`] of the file (send the whole file instead) or when
/// nothing is changed.
pub fn focus_source(
    source: &str,
    changed: &[(u32, u32)],
    function_spans: &[(u32, u32)],
    hunks: Option<&str>,
) -> Option<FocusedView> {
    let lines: Vec<&str> = source.lines().collect();
    let total = lines.len();
    if total == 0 || changed.is_empty() {
        return None;
    }
    let ranges = kept_ranges(changed, function_spans, DEFAULT_CONTEXT_LINES, total as u32);
    let kept: usize = ranges.iter().map(|(s, e)| (e - s + 1) as usize).sum();
    if kept == 0 || (kept as f64) > MAX_KEPT_FRACTION * (total as f64) {
        return None;
    }
    let width = total.to_string().len();
    let mut text = String::new();
    text.push_str(&format!(
        "[focused view: {kept} of {total} lines shown in {} region(s). Every line is prefixed \
         with its absolute line number; cite those numbers. Lines marked omitted are unchanged \
         code that compiles as-is. The unified diff for this file follows the code.]\n",
        ranges.len()
    ));
    let mut cursor: u32 = 1;
    for &(s, e) in &ranges {
        if s > cursor {
            text.push_str(&format!(
                "... lines {}-{} omitted (unchanged) ...\n",
                cursor,
                s - 1
            ));
        }
        for n in s..=e {
            text.push_str(&format!("{:>width$}| {}\n", n, lines[(n - 1) as usize]));
        }
        cursor = e + 1;
    }
    if (cursor as usize) <= total {
        text.push_str(&format!(
            "... lines {}-{} omitted (unchanged) ...\n",
            cursor, total
        ));
    }
    if let Some(h) = hunks {
        text.push_str("\n===== unified diff for this file (- removed, + added) =====\n");
        text.push_str(h);
        if !h.ends_with('\n') {
            text.push('\n');
        }
    }
    Some(FocusedView {
        text,
        first_line: ranges[0].0,
        last_line: ranges[ranges.len() - 1].1,
        kept_lines: kept,
        total_lines: total,
    })
}

/// The hunk lines (`@@` headers and `-`/`+`/` ` body lines) of every file in
/// `diff` whose `+++ b/` path satisfies `matches`, bounded by
/// [`MAX_HUNK_LINES`] and [`MAX_HUNK_BYTES`]. `None` when no matching file
/// contributed hunk text (a file absent from the diff, or present with a
/// binary or empty section).
///
/// Inside a hunk every body line starts with ` `, `+`, `-` or `\`, so a
/// body line whose content happens to start with `-- ` or `++ ` renders as
/// `--- ` / `+++ ` and must not be mistaken for a file header. A header is
/// only recognised where a hunk cannot continue: at a `diff --git` line, or
/// at a `--- ` line that is immediately followed by `+++ ` while no hunk is
/// open.
pub fn hunks_for_file(diff: &str, matches: &dyn Fn(&str) -> bool) -> Option<String> {
    let lines: Vec<&str> = diff.lines().collect();
    let mut out = String::new();
    let mut in_file = false;
    let mut in_hunk = false;
    let mut emitted = 0usize;
    let mut truncated = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("diff --git ") {
            in_hunk = false;
            i += 1;
            continue;
        }
        if !in_hunk {
            if let Some(path) = line.strip_prefix("+++ b/") {
                in_file = matches(path);
            }
            if line.starts_with("@@ ") {
                in_hunk = true;
            } else {
                i += 1;
                continue;
            }
        } else if !is_body(line) {
            // Anything that is not a body line ends the hunk (a new `@@`
            // starts the next one on the same pass).
            in_hunk = line.starts_with("@@ ");
            if !in_hunk {
                continue;
            }
        }
        if in_file {
            if emitted >= MAX_HUNK_LINES || out.len() + line.len() + 1 > MAX_HUNK_BYTES {
                truncated = true;
                break;
            }
            out.push_str(line);
            out.push('\n');
            emitted += 1;
        }
        i += 1;
    }
    if out.is_empty() {
        return None;
    }
    if truncated {
        out.push_str(&format!(
            "... diff truncated after {emitted} lines ({MAX_HUNK_LINES} lines / {MAX_HUNK_BYTES} bytes cap) ...\n"
        ));
    }
    Some(out)
}

/// A unified-diff hunk body line: context, addition, deletion, or the
/// "no newline at end of file" marker.
fn is_body(line: &str) -> bool {
    line.is_empty() || matches!(line.as_bytes()[0], b' ' | b'+' | b'-' | b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[test]
    fn kept_ranges_uses_enclosing_function_and_merges_neighbours() {
        // Change at 12 sits inside fn 10-30; change at 31 has no fn, gets context.
        let r = kept_ranges(&[(12, 12), (31, 31)], &[(10, 30), (50, 60)], 5, 100);
        // 10-30 and 26-36 overlap -> one merged region.
        assert_eq!(r, vec![(10, 36)]);
    }

    #[test]
    fn kept_ranges_context_is_clamped_to_the_file() {
        let r = kept_ranges(&[(2, 2), (99, 100)], &[], 20, 100);
        assert_eq!(r, vec![(1, 22), (79, 100)]);
    }

    #[test]
    fn focus_numbers_lines_absolutely_and_marks_omissions() {
        let src = numbered(100);
        let v = focus_source(&src, &[(50, 51)], &[(45, 55)], None).unwrap();
        assert!(v.text.contains("... lines 1-44 omitted (unchanged) ..."));
        assert!(v.text.contains(" 45| line 45"));
        assert!(v.text.contains(" 55| line 55"));
        assert!(!v.text.contains("| line 44"), "line 44 must be omitted");
        assert!(v.text.contains("... lines 56-100 omitted (unchanged) ..."));
        assert_eq!(
            (v.first_line, v.last_line, v.kept_lines, v.total_lines),
            (45, 55, 11, 100)
        );
    }

    #[test]
    fn focus_falls_back_to_whole_file_when_most_of_it_is_kept() {
        let src = numbered(100);
        // 70 of 100 lines would be kept: above the fraction, so no view.
        assert!(focus_source(&src, &[(1, 70)], &[], None).is_none());
        // Nothing changed: nothing to focus.
        assert!(focus_source(&src, &[], &[], None).is_none());
    }

    #[test]
    fn focus_appends_hunks_after_the_code() {
        let src = numbered(100);
        let v = focus_source(
            &src,
            &[(50, 50)],
            &[],
            Some("@@ -50,1 +50,1 @@\n-old\n+line 50\n"),
        )
        .unwrap();
        let code_end = v
            .text
            .find("===== unified diff")
            .expect("diff header present");
        let hunk = v.text.find("+line 50").expect("hunk body present");
        assert!(hunk > code_end, "hunks come after the code");
    }

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,3 @@\n fn a() {\n+    x();\n }\ndiff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -5,1 +5,1 @@\n-old\n+new\n";

    #[test]
    fn hunks_for_file_returns_only_that_files_hunks() {
        let h = hunks_for_file(DIFF, &|p| p == "src/b.rs").unwrap();
        assert_eq!(h, "@@ -5,1 +5,1 @@\n-old\n+new\n");
        assert!(hunks_for_file(DIFF, &|p| p == "src/zzz.rs").is_none());
    }

    #[test]
    fn hunks_for_file_is_bounded() {
        let mut big = String::from("--- a/x.rs\n+++ b/x.rs\n@@ -1,1000 +1,1000 @@\n");
        for i in 0..1000 {
            big.push_str(&format!("+l{i}\n"));
        }
        let h = hunks_for_file(&big, &|_| true).unwrap();
        assert_eq!(h.lines().count(), MAX_HUNK_LINES + 1);
        assert!(h.contains(&format!("... diff truncated after {MAX_HUNK_LINES} lines")));
    }

    #[test]
    fn hunks_for_file_is_bounded_by_bytes_too() {
        let big_line = "+".to_string() + &"x".repeat(MAX_HUNK_BYTES);
        let diff = format!("--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,2 @@\n+first\n{big_line}\n");
        let h = hunks_for_file(&diff, &|_| true).unwrap();
        assert!(
            h.len() < MAX_HUNK_BYTES + 200,
            "one oversized line must not blow the cap"
        );
        assert!(h.contains("... diff truncated after 2 lines"), "{h}");
    }

    /// A removed SQL comment `-- x` renders as `--- x`; an added `++ b/y`
    /// renders as `+++ b/y`. Neither is a header, and dropping the rest of
    /// the hunk on them lost the edit the model was meant to see.
    #[test]
    fn hunks_for_file_keeps_body_lines_that_look_like_headers() {
        let diff = "--- a/q.sql\n+++ b/q.sql\n@@ -1,3 +1,3 @@\n-- select\n--- old comment\n+++ b/not a header\n index_line_kept\n+select 1;\n";
        let h = hunks_for_file(diff, &|p| p == "q.sql").unwrap();
        assert!(h.contains("--- old comment"), "removed line kept: {h}");
        assert!(h.contains("+++ b/not a header"), "added line kept: {h}");
        assert!(h.contains("+select 1;"), "hunk continues to its end: {h}");
        // And a second file after a real header is still separated.
        let two = format!(
            "{diff}diff --git a/z.rs b/z.rs\n--- a/z.rs\n+++ b/z.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n"
        );
        assert!(
            !hunks_for_file(&two, &|p| p == "q.sql")
                .unwrap()
                .contains("+b")
        );
        assert_eq!(
            hunks_for_file(&two, &|p| p == "z.rs").unwrap(),
            "@@ -1,1 +1,1 @@\n-a\n+b\n"
        );
    }

    #[test]
    fn kept_ranges_does_not_overflow_near_u32_max() {
        let r = kept_ranges(&[(u32::MAX - 1, u32::MAX)], &[], 20, u32::MAX);
        assert_eq!(r, vec![(u32::MAX - 21, u32::MAX)]);
    }
}
