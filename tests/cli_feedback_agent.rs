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
fn agent_model_alone_is_rejected_and_writes_nothing() {
    // Contract: --agent-model without --from-agent must fail. We assert on
    // both the exit status AND the side-effect (no entry written) so a
    // regression where clap silently accepts the flag and we fall through
    // to the Human path can't sneak by.
    let home = TempDir::new().unwrap();
    let fb_path = home.path().join(".quorum/feedback.jsonl");
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
            "--agent-model",
            "gpt-5.4",
        ],
    )
    .failure();
    assert!(
        !fb_path.exists() || std::fs::read_to_string(&fb_path).unwrap().is_empty(),
        "rejected invocation must not write any feedback entry: {}",
        std::fs::read_to_string(&fb_path).unwrap_or_default()
    );
}

#[test]
fn confidence_out_of_range_is_rejected_at_cli_boundary() {
    // clap value_parser must reject confidence outside [0,1] before it ever
    // reaches record_external. Both negative and >1 must fail.
    let home = TempDir::new().unwrap();
    for bad in &["-0.5", "1.5", "nan", "inf"] {
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
                "--from-agent",
                "pal",
                "--confidence",
                bad,
            ],
        )
        .failure();
    }
}

#[test]
fn from_agent_honors_explicit_json_flag_on_tty() {
    // CodeRabbit-flagged regression: --json must work even when stdout is
    // a TTY. Prior code only enabled JSON via TTY-detection. assert_cmd
    // doesn't hand us a real terminal, but it does pass --json; we just
    // verify the output parses as JSON and contains the expected fields.
    let home = TempDir::new().unwrap();
    let out = run_feedback(
        home.path(),
        &[
            "--file",
            "src/x.rs",
            "--finding",
            "Bug",
            "--verdict",
            "tp",
            "--reason",
            "r",
            "--json",
            "--from-agent",
            "pal",
            "--agent-model",
            "gpt-5.4",
            "--confidence",
            "0.5",
        ],
    )
    .success()
    .get_output()
    .stdout
    .clone();
    let stdout = String::from_utf8(out).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--json must emit valid JSON: {stdout:?} ({e})"));
    assert_eq!(v["provenance"], "external");
    assert_eq!(v["agent"], "pal");
    assert_eq!(v["verdict"], "tp");
}

#[test]
fn human_path_honors_explicit_json_flag() {
    // CodeRabbit-flagged regression (round 3): the Human CLI path must
    // honor --json even on a TTY. Prior code only switched to JSON via
    // pipe detection.
    let home = TempDir::new().unwrap();
    let out = run_feedback(
        home.path(),
        &[
            "--file",
            "src/x.rs",
            "--finding",
            "Bug",
            "--verdict",
            "tp",
            "--reason",
            "r",
            "--json",
        ],
    )
    .success()
    .get_output()
    .stdout
    .clone();
    let stdout = String::from_utf8(out).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("Human --json must emit valid JSON: {stdout:?} ({e})")
    });
    assert_eq!(v["verdict"], "tp");
    assert_eq!(v["file_path"], "src/x.rs");
}

#[test]
fn human_path_normalizes_blank_category() {
    // CodeRabbit-flagged regression: the Human CLI path must trim
    // whitespace and treat empty strings as missing — same rule
    // record_external and the MCP Human path follow.
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
            "--category",
            "   ",
        ],
    )
    .success();
    let fb = std::fs::read_to_string(home.path().join(".quorum/feedback.jsonl")).unwrap();
    assert!(
        fb.contains(r#""finding_category":"manual""#),
        "blank --category must collapse to manual default: {fb}"
    );
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
