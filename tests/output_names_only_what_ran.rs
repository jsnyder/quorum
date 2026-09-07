//! #531: a component is named in output only if it actually executed.
//!
//! Two instances, both misleading in the same direction -- toward the reader
//! believing more analysis happened than did:
//!
//! 1. `Reviewed 1 file(s) in 0.1s using gpt-5.6` on a run with no API key.
//!    The model named was `QUORUM_MODEL`'s value: what *would* have run,
//!    reported as what *did*.
//! 2. `"enabled": ["clippy"]` in `_meta`, and `clippy=on` in the compact
//!    header, on every review. `run_linter` has no production caller and never
//!    has, so no external linter output has ever reached a finding.
//!
//! These tests spawn through `support`, so they are AST-only by construction
//! (#501) -- which is exactly the condition under test.

mod support;

use std::fs;

fn subject(dir: &std::path::Path) -> std::path::PathBuf {
    let f = dir.join("subject.rs");
    fs::write(
        &f,
        "fn preview(s: &str) -> &str {\n    &s[..10]\n}\n\nfn main() {\n    println!(\"{}\", preview(\"hello world\"));\n}\n",
    )
    .unwrap();
    f
}

#[test]
fn ast_only_review_does_not_name_a_model() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let file = subject(work.path());

    let out = support::quorum(home.path())
        .arg("review")
        .arg(&file)
        .output()
        .expect("spawn quorum");
    let stderr = String::from_utf8_lossy(&out.stderr);

    let summary = stderr
        .lines()
        .find(|l| l.starts_with("Reviewed "))
        .unwrap_or_else(|| panic!("no summary line in stderr:\n{stderr}"));

    assert!(
        !summary.contains(" using "),
        "an AST-only review named a model it never called: {summary}"
    );
    assert!(
        summary.contains("AST-only"),
        "the summary should say what actually ran: {summary}"
    );
}

#[test]
fn linter_meta_does_not_claim_a_linter_ran() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let file = subject(work.path());

    let out = support::quorum(home.path())
        .arg("review")
        .arg(&file)
        .arg("--json")
        .output()
        .expect("spawn quorum");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Only assert when a _meta block is actually emitted -- whether any linter
    // is installed depends on the machine, and the claim under test is about
    // wording, not presence.
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout) else {
        return;
    };
    let Some(meta) = parsed
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.get("_meta"))
        .and_then(|m| m.get("linters"))
    else {
        return;
    };

    assert!(
        meta.get("enabled").is_none(),
        "`enabled` reads as `ran`; nothing invokes these linters: {meta}"
    );
    assert!(
        meta.get("installed_and_configured").is_some(),
        "the key should say what was actually determined: {meta}"
    );
}

#[test]
fn compact_linter_header_does_not_claim_a_linter_ran() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let file = subject(work.path());

    let out = support::quorum(home.path())
        .arg("review")
        .arg(&file)
        .arg("--compact")
        .output()
        .expect("spawn quorum");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    for line in combined.lines().filter(|l| l.starts_with("# linters:")) {
        assert!(
            !line.contains("=on"),
            "`=on` reads as `ran`; nothing invokes these linters: {line}"
        );
    }
}
