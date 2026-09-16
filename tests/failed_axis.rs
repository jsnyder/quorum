//! A review whose skill axes failed must say so everywhere a consumer
//! looks: the summary line, the `_meta.incomplete` entry of `--json`, and
//! the exit code. Before, a fully failed review printed `[]` and "0
//! finding(s)" and CI read it as clean.

mod support;

#[test]
fn failed_axes_are_visible_in_summary_json_and_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = tmp.path().join("subject.rs");
    std::fs::write(
        &subject,
        "fn changed(text: &str) -> i32 {\n    text.len() as i32\n}\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();

    // The cassette answers every axis with prose instead of JSON.
    let (out, sent) = support::with_cassette(home.path(), "malformed_response", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg("--skip-context7")
            .arg("--axes")
            .arg("correctness,security")
            .arg(&subject)
            .output()
            .unwrap()
    });
    assert_eq!(sent.len(), 2, "one call per axis");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("2 of 2 skill axes failed (review incomplete)"),
        "summary line must say the review is incomplete:\n{stderr}"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let entries: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    let incomplete = entries
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|e| e.get("_meta").and_then(|m| m.get("incomplete")).cloned())
        .expect("_meta.incomplete must be present when an axis failed");
    assert_eq!(incomplete["axes_failed"], 2, "{stdout}");
    assert_eq!(incomplete["axes_total"], 2, "{stdout}");
    let cells = incomplete["cells"].as_array().expect("cells listed");
    assert_eq!(cells.len(), 2);
    assert!(
        cells[0].as_str().unwrap_or("").contains("not_json"),
        "each cell names its failure class: {cells:?}"
    );

    assert_eq!(
        out.status.code(),
        Some(1),
        "an incomplete review with no findings is not exit 0"
    );
}

/// The same file with a well-formed answer has no `_meta.incomplete` and
/// exits by its findings alone, so the entry means something when present.
#[test]
fn complete_review_carries_no_incomplete_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = tmp.path().join("subject.rs");
    std::fs::write(
        &subject,
        "fn changed(text: &str) -> i32 {\n    text.len() as i32\n}\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let (out, _sent) = support::with_cassette(home.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg("--skip-context7")
            .arg("--axes")
            .arg("correctness,security")
            .arg(&subject)
            .output()
            .unwrap()
    });
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("\"incomplete\""), "{stdout}");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("review incomplete"));
}
