//! Issue #69: `quorum stats` dimensional commands previously swallowed
//! `ReviewLog::load_all` errors via `unwrap_or_default()`, silently producing
//! empty stats when the reviews log was unreadable.
//!
//! With the SQLite migration (#326), the stats path gracefully falls back to
//! an in-memory database when disk storage initialization fails. A directory
//! at the old `reviews.jsonl` path is now simply ignored by the migration
//! (not a regular file). These tests verify that stats commands still succeed
//! cleanly under degraded storage conditions.
//!
//! The "missing file -> Ok(empty)" semantic is preserved: `stats` with no
//! prior data exits 0.

use assert_cmd::Command;

/// Build a HOME directory whose `.quorum/reviews.jsonl` is a *directory*.
/// With the SQLite backend, this is silently skipped by migration. The
/// stats command should still exit 0 with empty data.
fn quirky_log_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let quorum_dir = tmp.path().join(".quorum");
    std::fs::create_dir_all(&quorum_dir).unwrap();
    std::fs::create_dir(quorum_dir.join("reviews.jsonl")).unwrap();
    tmp
}

/// With SQLite, a directory at reviews.jsonl is harmlessly ignored.
/// Stats commands create a fresh quorum.db and report empty data.
fn assert_stats_flag_succeeds_with_quirky_log(flag_args: &[&str]) {
    let tmp = quirky_log_dir();
    let mut cmd = Command::cargo_bin("quorum").unwrap();
    cmd.arg("stats");
    for arg in flag_args {
        cmd.arg(arg);
    }
    let output = cmd.env("HOME", tmp.path()).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit code 0 (graceful empty stats); got: {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stats_by_repo_succeeds_with_quirky_log() {
    assert_stats_flag_succeeds_with_quirky_log(&["--by-repo"]);
}

#[test]
fn stats_by_caller_succeeds_with_quirky_log() {
    assert_stats_flag_succeeds_with_quirky_log(&["--by-caller"]);
}

#[test]
fn stats_by_rolling_succeeds_with_quirky_log() {
    assert_stats_flag_succeeds_with_quirky_log(&["--rolling", "5"]);
}

#[test]
fn stats_by_source_succeeds_with_quirky_log() {
    assert_stats_flag_succeeds_with_quirky_log(&["--by-source"]);
}

#[test]
fn stats_succeeds_when_log_missing() {
    // Guard against an over-fix that promotes "missing file" to error.
    // An empty quorum.db (or no quorum.db) should return Ok(empty vec).
    let tmp = tempfile::tempdir().unwrap();
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
