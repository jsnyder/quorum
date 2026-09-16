mod support;

use assert_cmd::Command;
use predicates::prelude::*;

/// A private, empty quorum home per test, so the calibrator has no data to
/// load and no test shares state with another.
///
/// Returns the `TempDir` alongside the command: it must outlive the spawned
/// process, so callers bind it (`let (_home, cmd) = quorum();`).
///
/// Issue #23 -- `review_unknown_extension_llm_only_fallback` asserting exit 0
/// on the assumption that no LLM is configured -- was the first symptom of
/// #501. That assumption is enforced by `support::quorum` rather than by this
/// file remembering one `env_remove`.
///
/// #503: these tests previously all shared the fixed path
/// `/tmp/quorum-test-home`, so every spawned process opened the same
/// `.quorum/quorum.db`. That is shared mutable state across concurrently
/// spawned tests, it persisted between runs so a stale database could break a
/// later one, and a fixed path in a world-writable directory is the
/// `predictable-tmp` pattern quorum's own bash rules flag.
fn quorum() -> (tempfile::TempDir, Command) {
    let home = tempfile::tempdir().expect("create per-test quorum home");
    let cmd = support::quorum(home.path());
    (home, cmd)
}

#[test]
fn version_exits_zero() {
    let (_home, mut cmd) = quorum();
    cmd.arg("version")
        .assert()
        .success()
        .stdout(predicate::str::contains("quorum"));
}

#[test]
fn review_clean_file_exits_zero() {
    // When piped (assert_cmd), output is JSON auto-detected
    let (_home, mut cmd) = quorum();
    cmd.arg("review")
        .arg("tests/fixtures/rust/clean.rs")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("[]"));
}

#[test]
fn review_complex_file_exits_nonzero() {
    let (_home, mut cmd) = quorum();
    cmd.arg("review")
        .arg("--complexity-threshold")
        .arg("10")
        .arg("tests/fixtures/rust/complex.rs")
        .assert()
        .code(predicate::gt(0))
        .stdout(predicate::str::contains("complexity"));
}

/// Complexity is opt-in: without the flag the same file is clean, and the
/// exit code says so. Corpus precision on complexity is flat and low, and it
/// was most of the in-diff noise on every PR review.
#[test]
fn review_complex_file_is_clean_without_the_threshold_flag() {
    let (_home, mut cmd) = quorum();
    cmd.arg("review")
        .arg("tests/fixtures/rust/complex.rs")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("complexity").not());
}

#[test]
fn review_insecure_python_finds_eval() {
    let (_home, mut cmd) = quorum();
    cmd.arg("review")
        .arg("tests/fixtures/python/insecure.py")
        .assert()
        .code(2) // critical finding = exit 2
        .stdout(predicate::str::contains("eval"));
}

#[test]
fn review_json_flag_outputs_valid_json() {
    let (_home, mut cmd) = quorum();
    let output = cmd
        .arg("review")
        .arg("--json")
        .arg("tests/fixtures/rust/clean.rs")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    // JSON output is a stream containing a `_meta` envelope element plus one
    // element per reviewed file. clean.rs must produce no `findings`.
    let total_findings: usize = parsed
        .iter()
        .filter_map(|el| el.get("findings").and_then(|f| f.as_array()))
        .map(|arr| arr.len())
        .sum();
    assert_eq!(total_findings, 0, "clean.rs produced findings: {stdout}");
}

#[test]
fn review_json_output_no_ansi() {
    let (_home, mut cmd) = quorum();
    let output = cmd
        .arg("review")
        .arg("--json")
        .arg("tests/fixtures/rust/complex.rs")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("\x1b["));
}

#[test]
fn review_nonexistent_file_exits_three() {
    let (_home, mut cmd) = quorum();
    cmd.arg("review")
        .arg("nonexistent_file.rs")
        .assert()
        .code(3);
}

#[test]
fn review_unknown_extension_llm_only_fallback() {
    // Unknown extensions use LLM-only review; without LLM configured, returns 0 findings
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("example.go");
    std::fs::write(
        &file,
        "package main\nfunc main() { fmt.Println(\"hello\") }\n",
    )
    .unwrap();
    let (_home, mut cmd) = quorum();
    cmd.arg("review")
        .arg(file.to_str().unwrap())
        .assert()
        .code(0);
}

#[test]
fn review_multiple_files() {
    // JSON output when piped; should contain complexity findings
    let (_home, mut cmd) = quorum();
    cmd.arg("review")
        .arg("--complexity-threshold")
        .arg("10")
        .arg("tests/fixtures/rust/clean.rs")
        .arg("tests/fixtures/rust/complex.rs")
        .assert()
        .code(predicate::gt(0))
        .stdout(predicate::str::contains("complexity"));
}

/// #517: the binary must be able to say what it was built from, so a stale
/// install diagnoses itself instead of looking identical to a fresh one.
///
/// Either shape is acceptable -- a git build names its commit, a build from a
/// release tarball says it cannot -- but a bare `quorum <version>` is not,
/// because that is exactly the ambiguity the issue is about.
#[test]
fn version_reports_build_provenance() {
    let (_home, mut cmd) = quorum();
    let assert = cmd.arg("version").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let line = stdout.trim();

    assert!(line.starts_with("quorum "), "unexpected shape: {line:?}");
    assert!(line.contains("built "), "no build date: {line:?}");

    let names_a_commit = line.split(['(', ',']).any(|field| {
        let field = field.trim();
        field.len() == 12 && field.chars().all(|c| c.is_ascii_hexdigit())
    });
    assert!(
        names_a_commit || line.contains("commit unknown"),
        "version line neither names a commit nor admits it cannot: {line:?}"
    );
}

/// With `--diff-file`, findings outside the changed lines are hidden by
/// default and the summary line says how many; `--show-out-of-diff` restores
/// them. Corpus precision on out-of-diff findings is 7% against 88% inside,
/// and CI has no feedback corpus to suppress them with.
#[test]
fn diff_file_hides_out_of_diff_findings_unless_asked() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(proj.path().join("src")).unwrap();
    // Line 2 (`unwrap`) is added by the diff; `untouched` is not in the diff
    // and trips the complexity threshold.
    let mut source = String::from("pub fn changed(x: Option<u32>) -> u32 {\n    x.unwrap()\n}\n\n");
    source.push_str("pub fn untouched(n: u32) -> u32 {\n");
    for i in 0..12 {
        source.push_str(&format!(
            "    if n == {i} {{\n        return {i};\n    }}\n"
        ));
    }
    source.push_str("    0\n}\n");
    let lib = proj.path().join("src").join("lib.rs");
    std::fs::write(&lib, source).unwrap();
    let diff = proj.path().join("change.patch");
    std::fs::write(
        &diff,
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub fn changed(x: Option<u32>) -> u32 {\n+    x.unwrap()\n }\n",
    )
    .unwrap();

    let titles = |stdout: &str| -> Vec<String> {
        let parsed: Vec<serde_json::Value> = serde_json::from_str(stdout).unwrap();
        parsed
            .iter()
            .filter_map(|el| el.get("findings").and_then(|f| f.as_array()))
            .flatten()
            .map(|f| f["title"].as_str().unwrap_or("").to_string())
            .collect()
    };

    let (_home, mut cmd) = quorum();
    let output = cmd
        .arg("review")
        .arg("--json")
        .arg("--complexity-threshold")
        .arg("10")
        .arg("--diff-file")
        .arg(&diff)
        .arg(&lib)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let shown = titles(&stdout);
    assert!(
        shown.iter().any(|t| t.contains("unwrap")),
        "the in-diff unwrap finding must survive: {shown:?}\n{stderr}"
    );
    assert!(
        !shown.iter().any(|t| t.contains("cyclomatic")),
        "the out-of-diff complexity finding must be hidden: {shown:?}"
    );
    assert!(
        stderr.contains("outside the diff hidden (run with --show-out-of-diff"),
        "the summary must say findings were hidden:\n{stderr}"
    );

    let (_home, mut cmd) = quorum();
    let output = cmd
        .arg("review")
        .arg("--json")
        .arg("--complexity-threshold")
        .arg("10")
        .arg("--diff-file")
        .arg(&diff)
        .arg("--show-out-of-diff")
        .arg(&lib)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let shown = titles(&stdout);
    assert!(
        shown.iter().any(|t| t.contains("cyclomatic")),
        "--show-out-of-diff must restore the complexity finding: {shown:?}\n{stderr}"
    );
    assert!(
        !stderr.contains("outside the diff hidden"),
        "nothing hidden, nothing to announce:\n{stderr}"
    );
}

/// An outside-hunk finding inside the changed function is shown by default;
/// one in an untouched function stays hidden. Both `unwrap()` lines are far
/// enough from the hunk to be outside the diff ranges.
#[test]
fn diff_file_rescues_findings_inside_the_changed_function() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(proj.path().join("src")).unwrap();
    let mut source = String::from(
        "pub fn changed(x: Option<u32>, y: Option<u32>) -> u32 {\n    let a = x.unwrap();\n",
    );
    for i in 0..12 {
        source.push_str(&format!("    let _pad{i} = {i};\n"));
    }
    source.push_str("    let b = y.map(|v| v + 1).unwrap_or(0);\n    a + b\n}\n\n");
    // A different rule in the untouched function, so the two findings cannot
    // be merged by title.
    source.push_str("pub fn untouched(p: *const u32) -> u32 {\n    unsafe { *p }\n}\n");
    let lib = proj.path().join("src").join("lib.rs");
    std::fs::write(&lib, source).unwrap();
    // The diff touches only the `let b` line, deep in `changed`.
    let diff = proj.path().join("change.patch");
    std::fs::write(
        &diff,
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -15,1 +15,1 @@\n-    let b = y.map(|v| v + 1).unwrap_or(1);\n+    let b = y.map(|v| v + 1).unwrap_or(0);\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let out = support::quorum(home.path())
        .arg("review")
        .arg("--json")
        .arg("--diff-file")
        .arg(&diff)
        .arg(&lib)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let files: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    let lines: Vec<u64> = files
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|f| f["findings"].as_array().into_iter().flatten())
        .map(|f| f["line_start"].as_u64().unwrap())
        .collect();
    assert_eq!(
        lines,
        [2],
        "the unwrap on line 2 is in the changed function (rescued); the unsafe block in `untouched` is not:\n{stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("1 outside the diff hidden"),
        "the untouched function's finding is the one hidden:\n{stderr}"
    );
}

/// A hidden finding is recorded with the review, so a verdict recorded
/// against it later resolves to its finding id instead of dangling.
#[test]
fn hidden_finding_can_still_receive_a_linked_verdict() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(proj.path().join("src")).unwrap();
    let mut source = String::from("pub fn changed(x: Option<u32>) -> u32 {\n    x.unwrap()\n}\n\n");
    source.push_str("pub fn untouched(p: *const u32) -> u32 {\n    unsafe { *p }\n}\n");
    let lib = proj.path().join("src").join("lib.rs");
    std::fs::write(&lib, source).unwrap();
    let diff = proj.path().join("change.patch");
    std::fs::write(
        &diff,
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub fn changed(x: Option<u32>) -> u32 {\n+    x.unwrap()\n }\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let out = support::quorum(home.path())
        .arg("review")
        .arg("--json")
        .arg("--diff-file")
        .arg(&diff)
        .arg(&lib)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("1 outside the diff hidden"),
        "precondition: the `untouched` unwrap is hidden:\n{stderr}"
    );
    // Record a verdict on the hidden finding by title; it must link.
    let fb = support::quorum(home.path())
        .arg("feedback")
        .arg("--file")
        .arg(&lib)
        .arg("--finding")
        .arg("Use of `unsafe` block")
        .arg("--verdict")
        .arg("tp")
        .arg("--in-diff")
        .arg("false")
        .arg("--reason")
        .arg("hidden but real")
        .output()
        .unwrap();
    let fb_out = String::from_utf8_lossy(&fb.stdout);
    assert!(
        fb_out.contains("\"linked\":\"01"),
        "the verdict must resolve to a recorded finding id:\n{fb_out}\n{}",
        String::from_utf8_lossy(&fb.stderr)
    );
}
