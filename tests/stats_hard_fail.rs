//! Issue #69: `quorum stats` dimensional commands previously swallowed
//! `ReviewLog::load_all` errors via `unwrap_or_default()`, silently producing
//! empty stats when the reviews log was unreadable. These tests pin the
//! contract that file-level read failures hard-fail with exit code 3 and a
//! diagnostic naming the failing path, while preserving the existing
//! "missing file -> Ok(empty)" semantic at the line-parse layer.

use assert_cmd::Command;

/// Build a HOME directory whose `.quorum/reviews.jsonl` is a *directory*,
/// causing `File::open` to fail with "Is a directory" on Unix. Portable
/// across macOS/Linux without relying on chmod or root-only tricks.
fn unreadable_log_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let quorum_dir = tmp.path().join(".quorum");
    std::fs::create_dir_all(&quorum_dir).unwrap();
    std::fs::create_dir(quorum_dir.join("reviews.jsonl")).unwrap();
    tmp
}

#[test]
fn stats_by_repo_fails_loudly_on_unreadable_log() {
    let tmp = unreadable_log_dir();
    let output = Command::cargo_bin("quorum")
        .unwrap()
        .arg("stats")
        .arg("--by-repo")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "expected exit code 3 (tool error per CLAUDE.md); got: {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected_path = tmp.path().join(".quorum/reviews.jsonl");
    assert!(
        stderr.contains(expected_path.to_string_lossy().as_ref()),
        "stderr should name the failing path; got: {stderr}"
    );
}

/// Helper for the three near-identical `stats <flag>` hard-fail tests.
/// `stats_by_repo_fails_loudly_on_unreadable_log` is intentionally NOT
/// collapsed into this — it carries the verbose assertion messages that
/// surface useful context the first time the contract regresses.
fn assert_stats_flag_fails_loudly(flag_args: &[&str]) {
    let tmp = unreadable_log_dir();
    let mut cmd = Command::cargo_bin("quorum").unwrap();
    cmd.arg("stats");
    for arg in flag_args {
        cmd.arg(arg);
    }
    let output = cmd.env("HOME", tmp.path()).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(".quorum/reviews.jsonl"));
}

#[test]
fn stats_by_caller_fails_loudly_on_unreadable_log() {
    assert_stats_flag_fails_loudly(&["--by-caller"]);
}

#[test]
fn stats_by_rolling_fails_loudly_on_unreadable_log() {
    assert_stats_flag_fails_loudly(&["--rolling", "5"]);
}

#[test]
fn stats_by_source_fails_loudly_on_unreadable_log() {
    // Context-dim branch (main.rs:72) shares the same load_all + exit 3
    // pattern as the classic-dim branch (main.rs:126). Both code paths
    // were fixed in #69; this test pins the context-dim path so a
    // future refactor can't regress only one of them.
    assert_stats_flag_fails_loudly(&["--by-source"]);
}

#[test]
fn stats_succeeds_when_log_missing() {
    // Guard against an over-fix that promotes "missing file" to error.
    // load_all on a non-existent file currently returns Ok(empty vec) via
    // iter() — the fix MUST NOT change that semantic.
    let tmp = tempfile::tempdir().unwrap();
    // Note: NO ~/.quorum/reviews.jsonl created.
    let output = Command::cargo_bin("quorum")
        .unwrap()
        .arg("stats")
        .arg("--by-repo")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "missing log should still exit 0 (empty stats); got: {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
