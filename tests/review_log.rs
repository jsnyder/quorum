//! Integration test: running `quorum review` writes a record to reviews.jsonl.

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn quorum(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("quorum").unwrap();
    cmd.env("HOME", home);
    // Suppress env-var detection so `invoked_from` is deterministic.
    cmd.env_remove("CLAUDE_CODE")
        .env_remove("CODEX_CI")
        .env_remove("GEMINI_CLI")
        .env_remove("AGENT");
    cmd
}

#[test]
fn review_writes_reviews_jsonl_record() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    quorum(home)
        .arg("review")
        .arg("tests/fixtures/rust/clean.rs")
        .assert()
        .code(0);

    let reviews_path = home.join(".quorum/reviews.jsonl");
    assert!(reviews_path.exists(), "reviews.jsonl not created");
    let content = std::fs::read_to_string(&reviews_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected exactly one review record");

    let rec: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(rec["files_reviewed"], 1);
    assert!(rec["run_id"].as_str().unwrap().len() == 26, "run_id must be 26-char ULID");
    assert!(rec["timestamp"].is_string());
    assert!(rec["quorum_version"].is_string());
    assert!(rec["findings_by_severity"].is_object());
    assert!(rec["flags"].is_object());
}

#[test]
fn caller_flag_overrides_invoked_from() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    quorum(home)
        .arg("review")
        .arg("--caller")
        .arg("my-ci-job")
        .arg("tests/fixtures/rust/clean.rs")
        .assert()
        .code(0);

    let content = std::fs::read_to_string(home.join(".quorum/reviews.jsonl")).unwrap();
    let rec: Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
    assert_eq!(rec["invoked_from"], "my-ci-job");
}

#[test]
fn second_review_appends() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    for _ in 0..2 {
        quorum(home)
            .arg("review")
            .arg("tests/fixtures/rust/clean.rs")
            .assert()
            .code(0);
    }

    let content = std::fs::read_to_string(home.join(".quorum/reviews.jsonl")).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "second run should append, not replace");

    let ids: Vec<String> = lines.iter()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["run_id"].as_str().unwrap().to_string())
        .collect();
    assert_ne!(ids[0], ids[1], "each run gets a unique ULID");
}
