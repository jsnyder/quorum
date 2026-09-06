//! Issue #501: integration coverage of the LLM review path, replayed from a
//! recorded response instead of a paid call.
//!
//! Stripping the env surface makes the suite free and hermetic, but on its own
//! it means nothing ever exercises the LLM path end-to-end -- the binary's
//! prompt assembly, HTTP call, response parsing, and finding rendering had no
//! integration test at any price. These do, at no price.
//!
//! The cassettes are real provider response bodies under
//! `tests/fixtures/llm/`, served off a loopback wiremock server. Re-record
//! with `scripts/record-llm-cassette.sh`.

mod support;

use std::path::Path;

/// Source with the defect the cassette reports, so line numbers line up.
const SUBJECT: &str = "fn parse(text: &str) -> i32 {\n    text.parse::<i32>().unwrap()\n}\n";

fn write_subject(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("subject.rs");
    std::fs::write(&path, SUBJECT).unwrap();
    path
}

/// The recorded finding survives the whole pipeline and reaches JSON output.
///
/// Kills the "review never actually parses an LLM response" mutation: before
/// this, no integration test sent the binary a provider payload at all.
#[test]
fn replayed_llm_finding_reaches_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_subject(tmp.path());

    let output = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg(&subject)
            .output()
            .unwrap()
    });

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("unwrap() on a fallible parse can panic"),
        "the cassette's finding should reach JSON output.\n\
         If this fails with a base_url error, the three env vars \
         `with_cassette` sets have drifted apart -- see its docs.\n\
         status={:?}\nstdout={stdout}\nstderr={stderr}",
        output.status.code()
    );
}

/// A replayed high-severity finding drives the documented exit code.
///
/// Exit codes are the CLI's contract (0 clean, 1 warnings, 2 critical), and
/// until now nothing checked that an LLM-sourced finding moved them at all.
#[test]
fn replayed_llm_finding_is_reflected_in_the_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_subject(tmp.path());

    let output = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review").arg(&subject).output().unwrap()
    });

    let code = output.status.code();
    assert!(
        matches!(code, Some(1) | Some(2)),
        "a high-severity finding should exit 1 or 2, not {code:?}.\nstderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The cassette path is genuinely offline: it works with no ambient
/// credentials whatsoever, which is what makes it safe in CI.
#[test]
fn cassette_replay_needs_no_ambient_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_subject(tmp.path());

    let output = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
        // Belt and braces: even the vars the helper already strips are
        // re-removed here, so this test states its own precondition.
        for var in support::stripped_vars() {
            if var != "QUORUM_BASE_URL" {
                cmd.env_remove(var);
            }
        }
        cmd.env("QUORUM_ALLOW_PRIVATE_BASE_URL", "1")
            .env("QUORUM_API_KEY", "sk-cassette-not-a-real-key")
            .arg("review")
            .arg("--json")
            .arg(&subject)
            .output()
            .unwrap()
    });

    assert!(
        String::from_utf8_lossy(&output.stdout).contains("fallible parse"),
        "replay must not depend on anything in the developer's environment"
    );
}
