//! Issue #73: `quorum context doctor` exit code derives from typed
//! `CmdOutput.doctor_failed`, not from re-parsing rendered stdout. Tests
//! pin that the exit code is decoupled from rendering format choice.
//!
//! These tests serve as both end-to-end regression guards (they should
//! stay GREEN through the refactor that removes the `doctor_reports_fail`
//! substring matcher) and as proof that the typed signal flows correctly
//! all the way to `process::exit`.

use assert_cmd::Command;

fn home_with_no_sources_toml() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".quorum")).unwrap();
    tmp
}

#[test]
fn doctor_exits_1_when_checks_fail_json_format() {
    let tmp = home_with_no_sources_toml();
    let output = Command::cargo_bin("quorum")
        .unwrap()
        .arg("context")
        .arg("doctor")
        .arg("--json")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 (json format); stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Pin typed signal and rendered text agree: the JSON output should still
    // carry "ok": false. Catches a regression where typed signal flips to
    // Some(true) but rendering silently drops the fail marker.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"ok\": false") || stdout.contains("\"ok\":false"),
        "typed signal and rendered text out of sync (json); stdout: {stdout}"
    );
}

#[test]
fn doctor_exits_1_when_checks_fail_table_format() {
    let tmp = home_with_no_sources_toml();
    let output = Command::cargo_bin("quorum")
        .unwrap()
        .arg("context")
        .arg("doctor") // table is default
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 (table format); stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fail") || stdout.contains("overall: fail"),
        "table output should still contain a fail indicator; stdout: {stdout}"
    );
}

#[test]
fn doctor_exits_1_when_checks_fail_compact_format() {
    let tmp = home_with_no_sources_toml();
    let output = Command::cargo_bin("quorum")
        .unwrap()
        .arg("context")
        .arg("doctor")
        .arg("--compact")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 (compact format); stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fail"),
        "compact output should still contain a fail row; stdout: {stdout}"
    );
}
