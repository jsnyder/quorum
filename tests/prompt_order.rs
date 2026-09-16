//! For one file, every skill axis must send the same prefix: the system
//! message, then the user message up to and including `</code_to_review>`.
//! Provider prompt caching keys on the message list from the start, so the
//! axis-specific `<skill_instructions>` have to follow the code, not lead it.

mod support;

fn messages(req: &serde_json::Value, role: &str) -> String {
    req["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == role)
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn axes_reviewing_one_file_share_the_prefix_through_the_code() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = tmp.path().join("subject.rs");
    std::fs::write(
        &subject,
        "fn changed(text: &str) -> i32 {\n    text.parse::<i32>().unwrap()\n}\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();

    let (_out, sent) = support::with_cassette(home.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg("--skip-context7")
            .arg("--axes")
            .arg("correctness,security")
            .arg(&subject)
            .output()
            .unwrap()
    });
    assert_eq!(sent.len(), 2, "one call per axis; got {}", sent.len());
    // The cache key is the message list itself, so pin its shape before
    // comparing contents: exactly one system message then one user message.
    for req in &sent {
        let roles: Vec<&str> = req["messages"]
            .as_array()
            .expect("messages array")
            .iter()
            .map(|m| m["role"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(roles, ["system", "user"], "message list shape");
    }

    assert_eq!(
        messages(&sent[0], "system"),
        messages(&sent[1], "system"),
        "the system message must not vary by axis"
    );
    assert!(
        messages(&sent[0], "system").contains("Read the code as a reviewer"),
        "the shared system prompt must prime the read, since the axis instructions now follow the code"
    );

    let prefix = |req: &serde_json::Value| {
        let user = messages(req, "user");
        let end = user
            .find("</code_to_review>")
            .expect("user message carries the code")
            + "</code_to_review>".len();
        let skill = user
            .find("<skill_instructions>")
            .expect("user message carries the axis instructions");
        assert!(
            skill > end,
            "axis instructions must follow the code, not lead it:\n{user}"
        );
        user[..end].to_string()
    };
    assert_eq!(
        prefix(&sent[0]),
        prefix(&sent[1]),
        "both axes must send an identical prefix through the code"
    );
    assert_ne!(
        messages(&sent[0], "user"),
        messages(&sent[1], "user"),
        "the axes must still differ after the code"
    );
}
