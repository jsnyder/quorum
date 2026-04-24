// Task 6 (issue #32): verify `quorum stats` and `quorum review` drain the
// external-feedback inbox before loading the feedback store.

use assert_cmd::Command;
use tempfile::TempDir;

fn quorum_home(qhome: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("quorum").unwrap();
    cmd.env("QUORUM_HOME", qhome);
    // Never make a real LLM call from tests.
    cmd.env_remove("QUORUM_API_KEY");
    cmd
}

#[test]
fn stats_drains_inbox_before_loading_feedback() {
    let home = TempDir::new().unwrap();
    let qhome = home.path().to_path_buf();
    let inbox = qhome.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let line = r#"{"file_path":"x.rs","finding_title":"Bug","finding_category":"security","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
    std::fs::write(inbox.join("drop.jsonl"), format!("{line}\n")).unwrap();

    quorum_home(&qhome).args(["stats"]).assert().success();

    // Inbox should have no *.jsonl files (only the processing/ and processed/ subdirs).
    let remaining: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|x| x == "jsonl")
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        remaining.len(),
        0,
        "inbox should have no *.jsonl files after drain, found {:?}",
        remaining.iter().map(|e| e.path()).collect::<Vec<_>>()
    );

    // processed/ should contain the archived file.
    let processed = inbox.join("processed");
    assert!(processed.exists(), "processed/ must exist after drain");
    let archived: Vec<_> = std::fs::read_dir(&processed)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(archived.len(), 1, "expected exactly one archived file");

    // feedback.jsonl should contain the External entry.
    let fb = std::fs::read_to_string(qhome.join("feedback.jsonl")).unwrap();
    assert!(
        fb.contains("\"external\""),
        "feedback.jsonl should contain External entry: {fb}"
    );
}

#[test]
fn empty_inbox_does_not_error_stats() {
    // Sanity: stats must work even when no inbox exists at all.
    let home = TempDir::new().unwrap();
    quorum_home(home.path()).args(["stats"]).assert().success();
}
