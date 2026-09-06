//! Issue #501: tests for the deny-by-default property itself.
//!
//! `tests/support/mod.rs` claims that no integration test can reach a paid
//! endpoint unless it explicitly asks. These tests hold that claim to account
//! rather than trusting the helper's own documentation -- the layer-2 tripwire
//! only works if every path to a paid call really does route through
//! `validate_base_url`, and that is an assumption about production code that
//! can regress without anyone touching this directory.

mod support;

use std::path::Path;

/// A file with a real finding, so the review has something to send.
const FIXTURE: &str = "tests/fixtures/rust/clean.rs";

fn write_fixture(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("subject.rs");
    std::fs::write(&path, "fn main() { let x: i32 = 1; println!(\"{x}\"); }\n").unwrap();
    path
}

/// Layer 1: the helper strips the key, so a review falls back to AST-only and
/// exits cleanly. This is the property that makes `cargo test` free.
#[test]
fn helper_strips_api_key_so_review_stays_ast_only() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_fixture(tmp.path());

    // On a machine with QUORUM_API_KEY exported -- the situation this whole
    // issue is about -- this exercises the strip directly, because the child
    // would otherwise inherit it. On a machine without one it degrades to a
    // plain AST-only smoke test. Injecting a key into the parent's own
    // environment to make it self-contained is not worth it: `set_var` is
    // unsafe in edition 2024 and would race the other tests in this binary.
    // `leaked_api_key_dies_in_the_base_url_validator` below covers the
    // adversarial case without needing that.
    let out = support::quorum(tmp.path())
        .arg("review")
        .arg(&subject)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean file should review clean via AST-only; stderr={stderr}"
    );
}

/// Layer 2: the tripwire fires *loudly*.
///
/// Leaks a key past layer 1 on purpose -- the situation a future bug or a
/// careless `.env()` would create -- and asserts the run dies with the
/// base_url validator's private-IP error. The failure mode this guards against
/// is not "the test fails"; it is the run quietly succeeding against a real
/// endpoint, or hanging on a dial to a dead port.
#[test]
fn leaked_api_key_dies_in_the_base_url_validator() {
    let tmp = tempfile::tempdir().unwrap();
    let subject = write_fixture(tmp.path());

    let out = support::quorum(tmp.path())
        .arg("review")
        .arg(&subject)
        .env("QUORUM_API_KEY", "sk-not-a-real-key")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !out.status.success(),
        "an un-opted-in LLM call must FAIL, not merely complain. A zero exit \
         here means the review carried on and the tripwire is decorative.\n\
         stderr={stderr}\nstdout={stdout}"
    );

    // Pins `actionable_error_for_private_ip` (src/llm_client.rs:592), the
    // exact branch the tripwire depends on -- not just "some error happened".
    assert!(
        stderr.contains("private/loopback/link-local")
            && stderr.contains("QUORUM_ALLOW_PRIVATE_BASE_URL"),
        "a leaked key must die in validate_base_url with an actionable message, \
         not silently reach a real endpoint.\n\
         The tripwire relies on every paid path routing through \
         `validate_base_url`; if that stopped being true, this is where you \
         find out.\nstatus={:?}\nstderr={stderr}\nstdout={stdout}",
        out.status.code()
    );
}

/// The opt-in is the *only* way to a live call. A present key is not enough.
///
/// Both branches assert something real. An earlier version faked the expected
/// panic when the opt-in happened to be set, which made the test pass without
/// exercising anything -- the exact "green for the wrong reason" failure this
/// file exists to prevent.
#[test]
fn live_helper_refuses_without_the_explicit_opt_in() {
    let opted_in = std::env::var("QUORUM_TEST_LIVE").is_ok_and(|v| v == "1");

    let tmp = tempfile::tempdir().unwrap();
    let attempt = std::panic::catch_unwind(|| {
        // Building the Command reaches no network; the refusal is a
        // precondition check inside the helper.
        let _cmd = support::quorum_live(tmp.path());
    });

    if opted_in {
        assert!(
            attempt.is_ok(),
            "with QUORUM_TEST_LIVE=1 the helper should hand back a Command, \
             not refuse"
        );
    } else {
        assert!(
            attempt.is_err(),
            "without QUORUM_TEST_LIVE=1 the helper MUST refuse, even though \
             QUORUM_API_KEY may be set -- present-key-means-live is issue #501"
        );
    }
}

/// The fixture the rest of the suite reviews must stay clean, or several
/// tests' exit-code assertions become meaningless.
#[test]
fn shared_fixture_still_exists() {
    assert!(
        Path::new(FIXTURE).exists(),
        "{FIXTURE} is referenced by review_log.rs and stats_dimensions.rs"
    );
}
