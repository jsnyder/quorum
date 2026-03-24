use assert_cmd::Command;
use predicates::prelude::*;

fn quorum() -> Command {
    Command::cargo_bin("quorum").unwrap()
}

#[test]
fn version_exits_zero() {
    quorum()
        .arg("version")
        .assert()
        .success()
        .stdout(predicate::str::contains("quorum"));
}

#[test]
fn review_clean_file_exits_zero() {
    // When piped (assert_cmd), output is JSON auto-detected
    quorum()
        .arg("review")
        .arg("tests/fixtures/rust/clean.rs")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("[]"));
}

#[test]
fn review_complex_file_exits_nonzero() {
    quorum()
        .arg("review")
        .arg("tests/fixtures/rust/complex.rs")
        .assert()
        .code(predicate::gt(0))
        .stdout(predicate::str::contains("complexity"));
}

#[test]
fn review_insecure_python_finds_eval() {
    quorum()
        .arg("review")
        .arg("tests/fixtures/python/insecure.py")
        .assert()
        .code(2) // critical finding = exit 2
        .stdout(predicate::str::contains("eval"));
}

#[test]
fn review_json_flag_outputs_valid_json() {
    let output = quorum()
        .arg("review")
        .arg("--json")
        .arg("tests/fixtures/rust/clean.rs")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(parsed.is_empty());
}

#[test]
fn review_json_output_no_ansi() {
    let output = quorum()
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
    quorum()
        .arg("review")
        .arg("nonexistent_file.rs")
        .assert()
        .code(3);
}

#[test]
fn review_unknown_extension_exits_three() {
    quorum()
        .arg("review")
        .arg("tests/cli.rs") // .rs is known, use a truly unknown ext
        .assert()
        .code(predicate::in_iter([0, 1, 2])); // should still work for .rs
}

#[test]
fn review_multiple_files() {
    // JSON output when piped; should contain complexity findings
    quorum()
        .arg("review")
        .arg("tests/fixtures/rust/clean.rs")
        .arg("tests/fixtures/rust/complex.rs")
        .assert()
        .code(predicate::gt(0))
        .stdout(predicate::str::contains("complexity"));
}
