//! Issue #501: the recording script must never send `QUORUM_API_KEY` to a
//! host that is not https or genuine loopback.
//!
//! `scripts/record-llm-cassette.sh` is the one place in this repo that
//! deliberately spends money and sends a credential, so its base_url guard is
//! the highest-consequence check in the change. It has now needed a hostile
//! input twice to expose a hole:
//!
//! 1. The first version had no check at all -- a stale `QUORUM_BASE_URL` sent
//!    the key wherever it pointed.
//! 2. The replacement glob-matched the whole URL (`http://127.0.0.1:*`), which
//!    `*` made trivially bypassable: in `http://127.0.0.1:80@attacker.example`
//!    everything before the `@` is RFC 3986 userinfo, so the real host is
//!    `attacker.example`. The check meant to prevent cleartext key disclosure
//!    was admitting precisely that.
//!
//! Hence this file. A regression fixture is cheaper than a third round.
//!
//! # These tests never reach the network
//!
//! The guard runs before `curl`, so every rejected URL exits without a
//! request. The one accepted case points at a dead loopback port, so it fails
//! at connect. No test here can spend money.

use std::path::Path;
use std::process::Command;

/// Run the script with `base_url` and return (exit code, stderr).
fn run_with_base_url(base_url: &str) -> (Option<i32>, String) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/record-llm-cassette.sh");

    let tmp = tempfile::tempdir().unwrap();
    let prompt = tmp.path().join("prompt.txt");
    std::fs::write(&prompt, "review this\n").unwrap();

    let out = Command::new("bash")
        .arg(&script)
        .arg("guard-probe")
        .arg(&prompt)
        .env("QUORUM_API_KEY", "sk-not-a-real-key")
        .env("QUORUM_BASE_URL", base_url)
        .env("QUORUM_MODEL", "gpt-5.6")
        .current_dir(root)
        .output()
        .expect("bash runs the recording script");

    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The bypass that motivated this file.
///
/// Everything before the `@` is userinfo, so these all resolve to an
/// attacker-controlled host while *looking* like loopback.
#[test]
fn userinfo_cannot_smuggle_a_foreign_host_past_the_loopback_allowlist() {
    for url in [
        "http://127.0.0.1:80@attacker.example",
        "http://localhost:1@evil.example",
        "http://127.0.0.1@attacker.example/v1",
        "https://127.0.0.1:443@attacker.example",
    ] {
        let (code, stderr) = run_with_base_url(url);
        assert_eq!(
            code,
            Some(2),
            "{url} must be rejected -- its real host is after the `@`, so the \
             API key would go to an attacker in cleartext.\nstderr={stderr}"
        );
        assert!(
            stderr.contains("embedded credentials"),
            "{url} should be rejected for embedded credentials specifically; \
             got:\n{stderr}"
        );
    }
}

/// A path segment that looks like loopback must not admit a foreign host.
#[test]
fn a_loopback_looking_path_does_not_admit_a_foreign_host() {
    let (code, stderr) = run_with_base_url("http://evil.example/127.0.0.1:8080");
    assert_eq!(
        code,
        Some(2),
        "the host is evil.example; the path is not the authority.\nstderr={stderr}"
    );
}

/// Ordinary non-loopback plaintext http stays rejected.
#[test]
fn plaintext_http_to_a_public_host_is_rejected() {
    let (code, stderr) = run_with_base_url("http://api.openai.com/v1");
    assert_eq!(code, Some(2), "stderr={stderr}");
    assert!(
        stderr.contains("plaintext http"),
        "expected the plaintext-http rejection; got:\n{stderr}"
    );
}

/// Non-http schemes are rejected rather than silently handed to curl.
#[test]
fn non_http_schemes_are_rejected() {
    for url in ["file:///etc/passwd", "gopher://example.com"] {
        let (code, stderr) = run_with_base_url(url);
        assert_eq!(code, Some(2), "{url} must be rejected.\nstderr={stderr}");
    }
}

/// Genuine loopback still gets through the guard.
///
/// Guards that only ever reject are easy to write and useless. This pins that
/// the local-gateway workflow the guard exists to permit still works: the run
/// gets *past* the URL check and fails later at connect, against a port with
/// nothing on it. No network egress, no spend.
#[test]
fn genuine_loopback_is_still_allowed_through_the_guard() {
    for url in ["http://127.0.0.1:9", "http://localhost:9/v1"] {
        let (_code, stderr) = run_with_base_url(url);
        assert!(
            !stderr.contains("refusing to send")
                && !stderr.contains("embedded credentials")
                && !stderr.contains("must use http or https"),
            "{url} is genuine loopback and must pass the URL guard \
             (it may still fail later at connect).\nstderr={stderr}"
        );
    }
}

/// A missing key is refused before anything else happens.
#[test]
fn a_missing_api_key_stops_the_script() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tmp = tempfile::tempdir().unwrap();
    let prompt = tmp.path().join("prompt.txt");
    std::fs::write(&prompt, "review this\n").unwrap();

    let out = Command::new("bash")
        .arg(root.join("scripts/record-llm-cassette.sh"))
        .arg("guard-probe")
        .arg(&prompt)
        .env_remove("QUORUM_API_KEY")
        .current_dir(root)
        .output()
        .unwrap();

    assert_ne!(out.status.code(), Some(0), "must not proceed without a key");
}
