//! Integration tests for `run_context`'s stdout handling (issue #84).
//!
//! These prove the binary actually invokes `cli_io::write_cmd_output`. The
//! pure unit tests in `src/cli_io.rs` cover the helper's branching logic;
//! this file kills the "forgot to wire helper" mutation by running the real
//! binary and observing end-to-end behavior.

use std::io::Read;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn quorum_bin() -> std::path::PathBuf {
    // Same env var assert_cmd uses; cargo sets it for the integration target.
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_quorum"));
    if !path.exists() {
        // Fall back to assert_cmd's lookup if the env var isn't honored on
        // some toolchains.
        path = assert_cmd::cargo::cargo_bin("quorum");
    }
    path
}

/// Run `quorum context list` with stdout piped to a reader that closes
/// immediately. This forces a `BrokenPipe` on the child's stdout, exercising
/// the real `write_cmd_output` path. The child must exit 0 (not panic, not 1).
#[test]
fn context_list_with_closed_stdout_exits_zero() {
    let home = TempDir::new().expect("failed to create temp dir for QUORUM_HOME");
    let qhome = home.path().join(".quorum");
    std::fs::create_dir_all(&qhome).expect("failed to create .quorum directory");

    let mut child = Command::new(quorum_bin())
        .env("QUORUM_HOME", qhome.as_os_str())
        .env_remove("QUORUM_API_KEY")
        .args(["context", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn quorum binary");

    // Drop the stdout pipe immediately. The child's first write_all should
    // see EPIPE -> the helper translates to exit 0 silently.
    drop(child.stdout.take());

    // Drain stderr so the child can flush + exit; also lets us assert that
    // the BrokenPipe path does NOT emit the "failed to write output:"
    // diagnostic (which would be a regression of the helper's classify rule).
    // CodeRabbit PR #106 noted the original test didn't catch a regression
    // where classify() mistakenly wrote on BrokenPipe.
    let mut stderr_buf = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr_buf)
        .expect("failed to drain quorum stderr");
    let stderr = String::from_utf8_lossy(&stderr_buf);
    assert!(
        !stderr.contains("failed to write"),
        "BrokenPipe must not emit the write-error diagnostic; got stderr: {stderr}"
    );

    let status = child.wait().expect("failed to wait for quorum child");
    assert!(
        status.success(),
        "BrokenPipe on stdout must yield exit 0; got {status:?}"
    );
}

/// Sanity: with stdout fully consumed, `context list` exits 0 and does not
/// emit a write-error diagnostic on stderr. (stderr may legitimately contain
/// a warning on a fresh QUORUM_HOME — e.g. "sources.toml not found" — which
/// the helper routes there per contract; the assertion below only forbids
/// the specific "failed to write" diagnostic.) This is the "happy path"
/// companion to the BrokenPipe test — proves the helper is not over-eagerly
/// translating success into errors.
#[test]
fn context_list_with_open_stdout_exits_zero_and_writes_to_stdout() {
    let home = TempDir::new().expect("failed to create temp dir for QUORUM_HOME");
    let qhome = home.path().join(".quorum");
    std::fs::create_dir_all(&qhome).expect("failed to create .quorum directory");

    let mut child = Command::new(quorum_bin())
        .env("QUORUM_HOME", qhome.as_os_str())
        .env_remove("QUORUM_API_KEY")
        .args(["context", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn quorum binary");

    // Drain stdout to EOF so the child can flush successfully.
    let mut stdout_buf = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout_buf)
        .expect("failed to drain quorum stdout");
    let mut stderr_buf = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr_buf)
        .expect("failed to drain quorum stderr");
    let status = child.wait().expect("failed to wait for quorum child");

    assert!(
        status.success(),
        "successful list must exit 0; got {status:?}"
    );
    let stderr = String::from_utf8_lossy(&stderr_buf);
    assert!(
        !stderr.contains("failed to write"),
        "happy path must not emit write-error diagnostic; got: {stderr}"
    );
    // Pin that the helper actually wrote something (CodeRabbit PR #106). A
    // mutation that drops the `out.write_all(cmd.stdout.as_bytes())` call
    // would still pass an exit-code-only assertion.
    assert!(
        !stdout_buf.is_empty(),
        "context list must produce stdout output; got empty buffer"
    );
}
