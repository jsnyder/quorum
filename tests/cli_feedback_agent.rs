// Task 7 (issue #32): `quorum feedback --from-agent <name>` writes an External
// provenance entry; the default path still writes Human.

use assert_cmd::Command;
use tempfile::TempDir;

fn run_feedback(home: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let qhome = home.join(".quorum");
    std::fs::create_dir_all(&qhome).unwrap();
    Command::cargo_bin("quorum")
        .unwrap()
        .env("QUORUM_HOME", qhome.as_os_str())
        .env_remove("QUORUM_API_KEY")
        .args(["feedback"])
        .args(args)
        .assert()
}

#[test]
fn from_agent_writes_external_provenance() {
    let home = TempDir::new().unwrap();
    run_feedback(
        home.path(),
        &[
            "--file",
            "src/a.rs",
            "--finding",
            "SQL injection",
            "--verdict",
            "tp",
            "--reason",
            "confirmed",
            "--from-agent",
            "pal",
            "--agent-model",
            "gemini-3-pro-preview",
            "--confidence",
            "0.9",
        ],
    )
    .success();

    let fb = std::fs::read_to_string(home.path().join(".quorum/feedback.jsonl")).unwrap();
    assert!(
        fb.contains("\"external\""),
        "feedback must contain external-tagged entry: {fb}"
    );
    assert!(fb.contains("\"pal\""), "agent name must appear: {fb}");
    assert!(
        fb.contains("\"gemini-3-pro-preview\""),
        "model must appear: {fb}"
    );
}

#[test]
fn agent_model_alone_does_not_write_external_entry() {
    // Behavior test: --agent-model without --from-agent must NOT produce an
    // External entry. We don't couple to clap's error wording — only the
    // side-effect contract.
    let home = TempDir::new().unwrap();
    let fb_path = home.path().join(".quorum/feedback.jsonl");
    let _ = run_feedback(
        home.path(),
        &[
            "--file",
            "a.rs",
            "--finding",
            "X",
            "--verdict",
            "tp",
            "--reason",
            "r",
            "--agent-model",
            "gpt-5.4",
        ],
    );
    if fb_path.exists() {
        let fb = std::fs::read_to_string(&fb_path).unwrap();
        assert!(
            !fb.contains("\"external\""),
            "agent-model alone must NOT produce External entry: {fb}"
        );
    }
}

#[test]
fn feedback_without_from_agent_still_writes_human() {
    let home = TempDir::new().unwrap();
    run_feedback(
        home.path(),
        &[
            "--file",
            "a.rs",
            "--finding",
            "X",
            "--verdict",
            "tp",
            "--reason",
            "r",
        ],
    )
    .success();
    let fb = std::fs::read_to_string(home.path().join(".quorum/feedback.jsonl")).unwrap();
    assert!(
        fb.contains("\"provenance\":\"human\""),
        "default path must be Human: {fb}"
    );
}
