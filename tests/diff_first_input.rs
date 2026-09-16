//! With `--diff-file`, the axes receive a focused view of each file (the
//! enclosing function of every changed range, numbered absolutely, plus the
//! file's hunks) instead of the whole file. Pinned at the wire: the request
//! body the binary sends must carry the changed function and the hunks, and
//! must not carry an untouched function far from the change. Without a
//! diff, the whole file still goes.

mod support;

use std::path::Path;

/// A changed function at the top and an untouched one ~150 lines below,
/// with filler between so the focused view is well under the whole-file
/// fallback fraction.
fn subject() -> String {
    let mut s =
        String::from("fn changed(text: &str) -> i32 {\n    text.parse::<i32>().unwrap()\n}\n\n");
    for i in 0..40 {
        s.push_str(&format!("fn filler_{i}() -> u32 {{\n    {i}\n}}\n\n"));
    }
    s.push_str("fn far_away_untouched() -> &'static str {\n    \"SENTINEL_FAR_AWAY_BODY\"\n}\n");
    s
}

fn write_fixture(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let subject_path = dir.join("src").join("subject.rs");
    std::fs::write(&subject_path, subject()).unwrap();
    let diff_path = dir.join("change.patch");
    std::fs::write(
        &diff_path,
        "diff --git a/src/subject.rs b/src/subject.rs\n--- a/src/subject.rs\n+++ b/src/subject.rs\n@@ -1,3 +1,3 @@\n fn changed(text: &str) -> i32 {\n-    text.parse::<i32>().unwrap_or(0)\n+    text.parse::<i32>().unwrap()\n }\n",
    )
    .unwrap();
    (subject_path, diff_path)
}

/// The user message of each request the binary sent, one string per request.
fn user_message_per_request(sent: &[serde_json::Value]) -> Vec<String> {
    sent.iter()
        .map(|req| {
            req["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m["role"] == "user")
                .map(|m| m["content"].as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

/// Every user message the binary sent, concatenated.
fn user_messages(sent: &[serde_json::Value]) -> String {
    sent.iter()
        .flat_map(|req| {
            req["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m["role"] == "user")
                .map(|m| m["content"].as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn diff_file_sends_the_focused_view_not_the_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (subject_path, diff_path) = write_fixture(tmp.path());
    let home = tempfile::tempdir().unwrap();

    let (_out, sent) = support::with_cassette(home.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg("--skip-context7")
            .arg("--diff-file")
            .arg(&diff_path)
            .arg(&subject_path)
            .output()
            .unwrap()
    });
    assert!(!sent.is_empty(), "the binary must have called the LLM");
    // Every axis is a separate request; each one must carry the complete
    // focused payload, not just the union of them.
    for body in user_message_per_request(&sent) {
        assert!(
            body.contains("[focused view:"),
            "the axes must receive the focused view:\n{body}"
        );
        assert!(
            body.contains("2|     text.parse::<i32>().unwrap()"),
            "the changed function must be present with its absolute line number:\n{body}"
        );
        assert!(
            body.contains("===== unified diff for this file")
                && body.contains("-    text.parse::<i32>().unwrap_or(0)"),
            "the hunks, deleted line included, must follow the code:\n{body}"
        );
        assert!(
            !body.contains("SENTINEL_FAR_AWAY_BODY"),
            "an untouched function far from the change must be omitted:\n{body}"
        );
    }
}

#[test]
fn without_a_diff_the_whole_file_still_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let (subject_path, _diff_path) = write_fixture(tmp.path());
    let home = tempfile::tempdir().unwrap();

    let (_out, sent) = support::with_cassette(home.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg("--skip-context7")
            .arg(&subject_path)
            .output()
            .unwrap()
    });
    let body = user_messages(&sent);
    assert!(
        body.contains("SENTINEL_FAR_AWAY_BODY"),
        "whole file expected:\n{body}"
    );
    assert!(
        !body.contains("[focused view:"),
        "no focus without a diff:\n{body}"
    );
}
