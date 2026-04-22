//! Smoke integration test for the `quorum context` subcommand surface.
//!
//! The full handler matrix is covered by unit tests in
//! `src/context/cli_tests.rs` against `TestDeps`. This file verifies the
//! argparse + `ProdDeps` wiring end-to-end by driving the real binary:
//!
//! * `context init` creates `$HOME/.quorum/sources.toml` and exits 0, and
//! * `context --help` lists every subcommand (guards against a forgotten
//!   variant in `ContextCommand`).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn quorum(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("quorum").unwrap();
    // Pin HOME so `ProdDeps::from_env()` resolves against the tempdir and
    // never pollutes the developer's real `~/.quorum`.
    cmd.env("HOME", home);
    cmd.env_remove("USERPROFILE");
    cmd.env_remove("QUORUM_API_KEY");
    cmd
}

#[test]
fn context_init_creates_sources_toml() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    quorum(home)
        .args(["context", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized context"));

    let sources_toml = home.join(".quorum").join("sources.toml");
    assert!(
        sources_toml.is_file(),
        "expected {} to be a regular file",
        sources_toml.display()
    );
}

#[test]
fn context_help_lists_all_subcommands() {
    // Assert each subcommand appears as a *command-row* entry (clap indents
    // subcommand rows with two spaces before the name) rather than a loose
    // substring that could match any doc sentence. Renaming or removing a
    // variant in `ContextCommand` will make this test fail.
    let tmp = TempDir::new().unwrap();
    let out = quorum(tmp.path())
        .args(["context", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).unwrap();
    for cmd in [
        "init", "add", "list", "index", "refresh", "query", "prune", "doctor",
    ] {
        let needle = format!("  {cmd}  ");
        assert!(
            stdout.contains(&needle),
            "expected subcommand row `{needle}` in help output:\n{stdout}"
        );
    }
}

#[test]
fn context_add_without_path_or_git_is_rejected() {
    // clap-level validation: `add` must have exactly one of --path / --git.
    let tmp = TempDir::new().unwrap();
    quorum(tmp.path())
        .args(["context", "add", "--name", "core", "--kind", "rust"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}
