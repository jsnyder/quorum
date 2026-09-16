//! The axes ask the model for a confidence; it must reach the integrator
//! and come out on the wire. Both axes return the cassette's 0.9, so the
//! merged finding is the noisy-or 1 - 0.1 * 0.1 = 0.99, not a fabricated
//! 0.5 (or 0.75 for two of them).

mod support;

#[test]
fn model_reported_confidence_is_merged_and_emitted() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = tmp.path().join("subject.rs");
    std::fs::write(
        &subject,
        "fn changed(text: &str) -> i32 {\n    text.parse::<i32>().unwrap()\n}\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();

    let (out, sent) = support::with_cassette(home.path(), "rust_unwrap_finding", |mut cmd| {
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    let files: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    let findings: Vec<&serde_json::Value> = files
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|f| f["findings"].as_array().into_iter().flatten())
        // The local AST pass reports its own unwrap finding with a computed
        // confidence; the model's finding is the one with llm_confidence.
        .filter(|f| f["source"] != "local-ast")
        .collect();
    assert_eq!(
        findings.len(),
        1,
        "the two axes' copies merge into one:\n{stdout}"
    );
    assert!(
        findings[0]["description"]
            .as_str()
            .unwrap_or("")
            .contains("Also flagged by"),
        "the merged finding names the other axis:\n{stdout}"
    );
    let conf = findings[0]["confidence"]
        .as_f64()
        .expect("merged finding carries the model-reported confidence");
    assert!(
        (conf - 0.99).abs() < 1e-3,
        "noisy-or of two 0.9s is 0.99; got {conf}"
    );
}
