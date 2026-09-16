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
/// The whole prompt per request, system then user: the file-stable part
/// (context, code) is the system message and the axis is the user message.
fn user_message_per_request(sent: &[serde_json::Value]) -> Vec<String> {
    sent.iter()
        .map(|req| {
            req["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m["role"] == "system" || m["role"] == "user")
                .map(|m| m["content"].as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

/// Every prompt the binary sent, concatenated.
fn user_messages(sent: &[serde_json::Value]) -> String {
    user_message_per_request(sent).join("\n")
}

/// The `filename` the `<code_to_review>` metadata line names, per request.
fn filename_of(body: &str) -> String {
    let start = body
        .find("<code_to_review>\n")
        .map(|i| i + "<code_to_review>\n".len());
    let line = start.and_then(|i| body[i..].lines().next()).unwrap_or("");
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v["filename"].as_str().map(str::to_string))
        .unwrap_or_default()
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
            body.contains("\"view\":{\"diff_follows\":true,\"kind\":\"focused\""),
            "the metadata must state the focused view:\n{body}"
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
        !body.contains("\"kind\":\"focused\""),
        "no focus without a diff:\n{body}"
    );
}

/// A deletion-only hunk (`+N,0`) leaves no new-side lines to focus on; the
/// whole file goes, as it does when the diff names some other file entirely.
#[test]
fn deletion_only_and_unmentioned_files_fall_back_to_the_whole_file() {
    for (name, diff) in [
        (
            "deletion-only",
            "diff --git a/src/subject.rs b/src/subject.rs\n--- a/src/subject.rs\n+++ b/src/subject.rs\n@@ -10,3 +9,0 @@\n-gone\n-gone\n-gone\n",
        ),
        (
            "unmentioned",
            "diff --git a/src/other.rs b/src/other.rs\n--- a/src/other.rs\n+++ b/src/other.rs\n@@ -1,1 +1,2 @@\n fn x() {}\n+fn y() {}\n",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (subject_path, diff_path) = write_fixture(tmp.path());
        std::fs::write(&diff_path, diff).unwrap();
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
        let body = user_messages(&sent);
        assert!(
            !sent.is_empty(),
            "{name}: the binary must have called the LLM"
        );
        assert!(
            body.contains("SENTINEL_FAR_AWAY_BODY"),
            "{name}: whole file expected:\n{body}"
        );
        assert!(
            !body.contains("\"kind\":\"focused\""),
            "{name}: no focused view expected:\n{body}"
        );
    }
}

/// Two files with `--parallel 2` take the parallel path, which assembles the
/// axis input separately from the sequential loop. Each request is
/// attributed by the filename in its metadata and must carry its own file's
/// focused view.
#[test]
fn parallel_path_sends_a_focused_view_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (subject_path, diff_path) = write_fixture(tmp.path());
    let second_path = tmp.path().join("src").join("second.rs");
    let mut second = String::from("fn other_changed(v: Option<u8>) -> u8 {\n    v.unwrap()\n}\n\n");
    for i in 0..40 {
        second.push_str(&format!("fn pad_{i}() -> u32 {{\n    {i}\n}}\n\n"));
    }
    second.push_str("fn second_far_away() -> &'static str {\n    \"SENTINEL_SECOND_FAR\"\n}\n");
    std::fs::write(&second_path, second).unwrap();
    let mut diff = std::fs::read_to_string(&diff_path).unwrap();
    diff.push_str("diff --git a/src/second.rs b/src/second.rs\n--- a/src/second.rs\n+++ b/src/second.rs\n@@ -1,3 +1,3 @@\n fn other_changed(v: Option<u8>) -> u8 {\n-    v.unwrap_or(0)\n+    v.unwrap()\n }\n");
    std::fs::write(&diff_path, &diff).unwrap();
    let home = tempfile::tempdir().unwrap();

    let (_out, sent) = support::with_cassette(home.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg("--skip-context7")
            .arg("--parallel")
            .arg("2")
            .arg("--diff-file")
            .arg(&diff_path)
            .arg(&subject_path)
            .arg(&second_path)
            .output()
            .unwrap()
    });
    let bodies = user_message_per_request(&sent);
    let mut seen_subject = 0;
    let mut seen_second = 0;
    for body in &bodies {
        let name = filename_of(body);
        assert!(
            body.contains("\"kind\":\"focused\""),
            "{name}: every request must carry a focused view:\n{body}"
        );
        if name.ends_with("subject.rs") {
            seen_subject += 1;
            assert!(
                body.contains("2|     text.parse::<i32>().unwrap()"),
                "{body}"
            );
            assert!(!body.contains("SENTINEL_FAR_AWAY_BODY"), "{body}");
            assert!(
                !body.contains("other_changed"),
                "{name} must not carry the other file:\n{body}"
            );
        } else if name.ends_with("second.rs") {
            seen_second += 1;
            assert!(body.contains("2|     v.unwrap()"), "{body}");
            assert!(!body.contains("SENTINEL_SECOND_FAR"), "{body}");
            assert!(
                !body.contains("text.parse"),
                "{name} must not carry the other file:\n{body}"
            );
        } else {
            panic!("request for an unexpected file {name:?}");
        }
    }
    assert!(
        seen_subject >= 1 && seen_second >= 1,
        "both files must be reviewed: {bodies:?}"
    );
}

/// The axes receive what the pipeline knows about the file: here the
/// changed function calls a helper defined further down, and its signature
/// must reach every axis inside `<review_context>`, ahead of the code.
#[test]
fn axes_receive_the_file_context_before_the_code() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    let mut src =
        String::from("fn changed(text: &str) -> i32 {\n    helper_target(text).unwrap()\n}\n\n");
    for i in 0..40 {
        src.push_str(&format!("fn filler_{i}() -> u32 {{\n    {i}\n}}\n\n"));
    }
    src.push_str("fn helper_target(s: &str) -> Option<i32> {\n    s.parse().ok()\n}\n");
    let subject_path = tmp.path().join("src").join("subject.rs");
    std::fs::write(&subject_path, src).unwrap();
    let diff_path = tmp.path().join("change.patch");
    std::fs::write(
        &diff_path,
        "diff --git a/src/subject.rs b/src/subject.rs\n--- a/src/subject.rs\n+++ b/src/subject.rs\n@@ -1,3 +1,3 @@\n fn changed(text: &str) -> i32 {\n-    helper_target(text).unwrap_or(0)\n+    helper_target(text).unwrap()\n }\n",
    )
    .unwrap();
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
    let bodies = user_message_per_request(&sent);
    assert!(
        !bodies.is_empty(),
        "the binary must have called the LLM with a user message"
    );
    for body in bodies {
        let ctx = body
            .rfind("<review_context>\n")
            .unwrap_or_else(|| panic!("no review_context:\n{body}"));
        let code = body.rfind("<code_to_review>\n").expect("no code block");
        assert!(ctx < code, "context must precede the code:\n{body}");
        assert!(
            body.contains("fn helper_target(s: &str) -> Option<i32>"),
            "the callee signature must reach the axis:\n{body}"
        );
        assert!(
            !body[ctx..code].contains("s.parse().ok()"),
            "the context carries signatures, not the helper's body:\n{body}"
        );
        assert!(
            body[ctx..code].contains("</hydration_context>"),
            "the inner sandbox closers must survive intact:\n{body}"
        );
    }
}
