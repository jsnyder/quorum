//! Issue #501: integration coverage of the LLM review path, replayed from a
//! recorded response instead of a paid call.
//!
//! Stripping the env surface makes the suite free and hermetic, but on its own
//! it means nothing exercises the LLM path end-to-end -- the binary's prompt
//! assembly, HTTP call, response parsing and finding rendering had no
//! integration test at any price. These do, at no price.
//!
//! The cassettes are real provider response bodies under
//! `tests/fixtures/llm/`, served off a loopback wiremock server. Re-record
//! with `scripts/record-llm-cassette.sh`.
//!
//! # What these tests do and do not catch
//!
//! The mock replies to any `POST /chat/completions` without inspecting the
//! request, so replay alone proves nothing about what the binary *sent*.
//! `with_cassette` therefore hands back the captured request bodies, and
//! `request_carries_the_reviewed_source_and_model` pins the invariants that
//! matter. Those invariants are deliberately few: enough to fail loudly if
//! prompt assembly regresses, loose enough that rewording the prompt does not
//! force a re-record with real money.
//!
//! Outside those invariants these tests do NOT detect prompt drift. A changed
//! system prompt, a reordered section, or a dropped few-shot example keeps
//! them green. Treat replay as coverage of the transport and parsing path plus
//! the pinned invariants -- not as a prompt regression suite.

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

    let (output, _sent) = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
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

/// What the binary *sent* -- the half a replay test does not otherwise cover.
///
/// Without this, the cassette replies to any POST and these tests would stay
/// green against a truncated prompt, a missing system message, or no prompt at
/// all. Each assertion below is chosen to survive rewording while failing on a
/// real assembly regression.
#[test]
fn request_carries_the_reviewed_source_and_model() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_subject(tmp.path());

    let (output, sent) = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--json")
            .arg(&subject)
            .env("QUORUM_MODEL", "gpt-5.6")
            .output()
            .unwrap()
    });

    assert!(
        !sent.is_empty(),
        "the binary sent no request at all -- replay proved nothing.\nstderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let body = &sent[0];

    assert_eq!(
        body["model"].as_str(),
        Some("gpt-5.6"),
        "the model the binary asked for should be the one configured; body={body}"
    );

    let messages = body["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("request has no messages array; body={body}"));
    assert!(
        messages.len() >= 2,
        "expected at least a system and a user message, got {}",
        messages.len()
    );

    let system = messages
        .iter()
        .find(|m| m["role"] == "system")
        .unwrap_or_else(|| panic!("no system message in {body}"));
    assert!(
        !system["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "the system message must not be empty -- an empty one silently \
         degrades review quality with no error anywhere"
    );

    let user = messages
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap_or_else(|| panic!("no user message in {body}"));
    let user_text = user["content"].as_str().unwrap_or_default();
    assert!(
        user_text.contains("text.parse::<i32>().unwrap()"),
        "the reviewed source must reach the prompt. If this fails, the \
         reviewer is being asked to review code it was never shown.\n\
         user message was:\n{user_text}"
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

    let (output, _sent) = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
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

    let (output, _sent) = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
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
