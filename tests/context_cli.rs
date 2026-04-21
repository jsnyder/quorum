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
    let tmp = TempDir::new().unwrap();
    quorum(tmp.path())
        .args(["context", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("index"))
        .stdout(predicate::str::contains("refresh"))
        .stdout(predicate::str::contains("query"))
        .stdout(predicate::str::contains("prune"))
        .stdout(predicate::str::contains("doctor"));
}
