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
//!
//! Every test here asserts the process succeeded and that the output it
//! inspects was actually produced. The first draft returned early when stdout
//! would not parse or the `_meta` block was absent, which meant a crash, a CLI
//! error, or the wholesale removal of the metadata all made these tests pass.
//! Quorum's review of that draft caught it -- the same vacuous-assertion class
//! these very changes are about (#536), aimed back at their own tests.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

/// A project whose linters are detectable: `detect_linters` keys on manifests,
/// so a bare temp dir produces no `_meta` block at all and the linter
/// assertions below would have nothing to inspect.
fn cargo_project(dir: &Path) -> std::path::PathBuf {
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"subject\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let f = dir.join("subject.rs");
    fs::write(
        &f,
        "fn preview(s: &str) -> &str {\n    &s[..10]\n}\n\nfn main() {\n    println!(\"{}\", preview(\"hello world\"));\n}\n",
    )
    .unwrap();
    f
}

/// Exit codes 0/1/2 are review verdicts (clean / warnings / critical); 3 is a
/// tool error. Anything else is a crash.
fn assert_ran(out: &Output, what: &str) {
    let code = out.status.code();
    assert!(
        matches!(code, Some(0) | Some(1) | Some(2)),
        "{what}: quorum did not complete a review (exit {code:?})\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ast_only_review_does_not_name_a_model() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let file = cargo_project(work.path());

    let out = support::quorum(home.path())
        .arg("review")
        .arg(&file)
        .output()
        .expect("spawn quorum");
    assert_ran(&out, "ast_only_review_does_not_name_a_model");
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
    let file = cargo_project(work.path());

    let out = support::quorum(home.path())
        .arg("review")
        .arg(&file)
        .arg("--json")
        .output()
        .expect("spawn quorum");
    assert_ran(&out, "linter_meta_does_not_claim_a_linter_ran");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json did not emit valid JSON ({e}):\n{stdout}"));
    let linters = parsed
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.get("_meta"))
        .and_then(|m| m.get("linters"))
        .unwrap_or_else(|| panic!("no _meta.linters block in --json output:\n{stdout}"));

    assert!(
        linters.get("enabled").is_none(),
        "`enabled` reads as `ran`; nothing invokes these linters: {linters}"
    );
    assert!(
        linters.get("installed_and_configured").is_some(),
        "the key should say what was actually determined: {linters}"
    );
}

#[test]
fn compact_linter_header_does_not_claim_a_linter_ran() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let file = cargo_project(work.path());

    let out = support::quorum(home.path())
        .arg("review")
        .arg(&file)
        .arg("--compact")
        .output()
        .expect("spawn quorum");
    assert_ran(&out, "compact_linter_header_does_not_claim_a_linter_ran");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let header = combined
        .lines()
        .find(|l| l.starts_with("# linters:"))
        .unwrap_or_else(|| panic!("no linter header emitted:\n{combined}"));
    assert!(
        !header.contains("=on"),
        "`=on` reads as `ran`; nothing invokes these linters: {header}"
    );
    assert!(
        header.contains("=configured") || header.contains("=off"),
        "header should state configuration, not execution: {header}"
    );
}
