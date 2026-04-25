// Task 8.5 (issue #32): same logical input through the inbox and CLI paths
// must land JSON-equivalent External entries (sans timestamp). Parsed into
// serde_json::Value for the comparison, so whitespace and key ordering
// differences are intentionally ignored — we only pin the data contract.
// MCP equivalence is pinned separately in
// src/mcp/handler.rs::tests::mcp_from_agent_writes_external_provenance.

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn entry_without_timestamp(e: &Value) -> Value {
    let mut e = e.clone();
    if let Some(obj) = e.as_object_mut() {
        obj.remove("timestamp");
    }
    e
}

#[test]
fn inbox_and_cli_paths_produce_equivalent_entries() {
    let payload_verdict = "tp";
    let payload_agent = "pal";
    let payload_model = "gemini-3-pro-preview";
    let payload_conf = "0.9";
    let payload_category = "security";

    // -------- Path A: inbox drain --------
    let home_a = TempDir::new().unwrap();
    let qhome_a = home_a.path().to_path_buf();
    std::fs::create_dir_all(qhome_a.join("inbox")).unwrap();
    let line = format!(
        r#"{{"file_path":"src/a.rs","finding_title":"Bug","finding_category":"{payload_category}","verdict":"{payload_verdict}","reason":"r","agent":"{payload_agent}","agent_model":"{payload_model}","confidence":{payload_conf}}}"#
    );
    std::fs::write(
        qhome_a.join("inbox").join("drop.jsonl"),
        format!("{line}\n"),
    )
    .unwrap();
    Command::cargo_bin("quorum")
        .unwrap()
        .env("QUORUM_HOME", &qhome_a)
        .env_remove("QUORUM_API_KEY")
        .args(["stats"])
        .assert()
        .success();
    let fb_a = std::fs::read_to_string(qhome_a.join("feedback.jsonl")).unwrap();
    let entry_a: Value = serde_json::from_str(fb_a.lines().next().unwrap()).unwrap();

    // -------- Path B: CLI --from-agent --------
    let home_b = TempDir::new().unwrap();
    let qhome_b = home_b.path().to_path_buf();
    std::fs::create_dir_all(&qhome_b).unwrap();
    Command::cargo_bin("quorum")
        .unwrap()
        .env("QUORUM_HOME", &qhome_b)
        .env_remove("QUORUM_API_KEY")
        .args([
            "feedback",
            "--file",
            "src/a.rs",
            "--finding",
            "Bug",
            "--verdict",
            payload_verdict,
            "--reason",
            "r",
            "--category",
            payload_category,
            "--from-agent",
            payload_agent,
            "--agent-model",
            payload_model,
            "--confidence",
            payload_conf,
        ])
        .assert()
        .success();
    let fb_b = std::fs::read_to_string(qhome_b.join("feedback.jsonl")).unwrap();
    let entry_b: Value = serde_json::from_str(fb_b.lines().next().unwrap()).unwrap();

    // Path C (MCP) equivalence is covered by the Provenance::External
    // assertion in src/mcp/handler.rs::tests::mcp_from_agent_writes_external_provenance.

    let a = entry_without_timestamp(&entry_a);
    let b = entry_without_timestamp(&entry_b);
    assert_eq!(
        a, b,
        "inbox and CLI paths produced divergent entries (JSON structural compare):\n  inbox: {a:#}\n  CLI  : {b:#}"
    );
}
