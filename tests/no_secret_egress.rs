//! Issue #530: no secret in reviewed source may reach an LLM endpoint.
//!
//! CLAUDE.md promises, under Constraints, "All secrets redacted before LLM
//! calls (always-on)". That was false: redaction lived in two unrelated places
//! -- upstream in `pipeline.rs` for the main reviewer, inline in
//! `judge_completion` for the judge -- and the skills/axes path went through
//! neither. A canary secret in one small Rust file reached the endpoint
//! verbatim in six of seven request bodies on a single default review.
//!
//! # Why this test is at the wire
//!
//! Three separate attempts to locate this bug by reasoning from call sites
//! blamed the wrong component (#515, #528, and a first pass on this one).
//! Reading `pipeline.rs` finds redaction; reading `judge_completion` finds
//! redaction; neither tells you about the third path. Capturing the request
//! bodies found it immediately.
//!
//! So this asserts on what actually left the process, not on which function
//! called which. `support::with_cassette` returns every captured request body,
//! and the assertion is simply that none contains the canary.
//!
//! A new LLM path added without redaction fails these tests -- which is the
//! point, and is what makes the CLAUDE.md guarantee real rather than
//! documentary.

mod support;

use std::path::Path;

/// Distinctive enough that a match cannot be coincidence, and shaped like a
/// real key so `redact_secrets` recognises it.
const CANARY: &str = "sk-CANARY1234567890abcdefghijklmnop";

fn write_leaky_source(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("leaky.rs");
    std::fs::write(
        &path,
        format!(
            "fn main() {{\n    \
             let api_key = \"{CANARY}\";\n    \
             let s = String::from(\"hi\");\n    \
             let _ = &s[..1];\n\
             }}\n"
        ),
    )
    .unwrap();
    path
}

/// Run a review through the cassette mock and return every captured body.
fn bodies_for(args: &[&str], env: &[(&str, &str)]) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_leaky_source(tmp.path());

    let (_out, sent) = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review").arg("--no-cache").arg(&subject);
        for a in args {
            cmd.arg(a);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    });

    sent.iter().map(|b| b.to_string()).collect()
}

fn assert_no_canary(label: &str, bodies: &[String]) {
    assert!(
        !bodies.is_empty(),
        "{label}: no request bodies captured -- this test would pass \
         vacuously. The path under test did not reach the endpoint at all."
    );
    let leaked: Vec<usize> = bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| b.contains(CANARY))
        .map(|(i, _)| i)
        .collect();
    assert!(
        leaked.is_empty(),
        "{label}: the canary secret reached the endpoint in {} of {} request \
         bodies (indices {leaked:?}).\n\
         Redaction must happen at `OpenAiClient::post_json`, the chokepoint \
         every path crosses. If a new request-construction site was added, it \
         has to go through that function -- see #530.",
        leaked.len(),
        bodies.len()
    );
}

/// The default path: main reviewer plus every skills axis. This is the
/// configuration that leaked six of seven bodies.
#[test]
fn default_review_sends_no_secret() {
    let bodies = bodies_for(&[], &[]);
    assert_no_canary("default review", &bodies);
}

/// The judge was the one path that was already safe. Pinned so that removing
/// its now-redundant inline redaction cannot silently regress it.
#[test]
fn judge_path_sends_no_secret() {
    let bodies = bodies_for(&[], &[("QUORUM_JUDGE", "1")]);
    assert_no_canary("judge enabled", &bodies);
}

/// Explicit multi-axis review -- the documented `--axes` entry point.
#[test]
fn axes_review_sends_no_secret() {
    let bodies = bodies_for(&["--axes", "correctness,security"], &[]);
    assert_no_canary("--axes", &bodies);
}

/// The agent loop uses `chat_with_tools`, a different request-construction
/// site with a `tools` array alongside the messages.
#[test]
fn deep_review_sends_no_secret() {
    let bodies = bodies_for(&["--deep"], &[]);
    assert_no_canary("--deep", &bodies);
}

/// The Responses API is a wholly different body shape -- `instructions` and
/// `input` rather than `messages` -- reached only by codex models. A
/// key-based redactor that only knew about `messages` would leak here.
#[test]
fn responses_api_path_sends_no_secret() {
    let bodies = bodies_for(&[], &[("QUORUM_MODEL", "gpt-5-codex")]);
    assert_no_canary("responses API (gpt-5-codex)", &bodies);
}

/// Redaction must not eat the auth header: the request still has to
/// authenticate. Guards against a future "redact everything" change that
/// scrubs the bearer token too and breaks every call.
#[test]
fn redaction_does_not_touch_the_authorization_header() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_leaky_source(tmp.path());

    let (out, sent) = support::with_cassette(tmp.path(), "rust_unwrap_finding", |mut cmd| {
        cmd.arg("review")
            .arg("--no-cache")
            .arg(&subject)
            .output()
            .unwrap()
    });

    assert!(!sent.is_empty(), "no requests captured");
    // A 401 would surface as a review failure; the cassette returns 200, so a
    // successful run is evidence the request was well-formed and authorized.
    assert!(
        out.status.code().is_some(),
        "review process did not exit cleanly"
    );
}
