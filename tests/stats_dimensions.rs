//! Integration test: `quorum stats --by-repo / --by-caller / --rolling N` produce dimensional output.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn quorum(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("quorum").unwrap();
    cmd.env("HOME", home);
    cmd.env_remove("CLAUDE_CODE")
        .env_remove("CODEX_CI")
        .env_remove("GEMINI_CLI")
        .env_remove("AGENT")
        // Isolate from developer shell so tests don't invoke real LLM (see issue #23)
        .env_remove("QUORUM_API_KEY");
    cmd
}

/// Basename of the repo that contains the test fixtures — derived at test time
/// so checkouts with a different directory name still work.
fn current_repo_basename() -> String {
    let cwd = std::env::current_dir().unwrap();
    let mut cur: &Path = &cwd;
    loop {
        if cur.join(".git").exists() {
            return cur.file_name().unwrap().to_string_lossy().into_owned();
        }
        cur = cur.parent().expect("not in a git repo?");
    }
}

fn seed_reviews(home: &Path) {
    // Seed reviews.jsonl by actually running quorum review a few times.
    // Assert success on every seed run — a silently-failing seed would
    // mask real bugs in the tests below.
    for _ in 0..3 {
        quorum(home)
            .arg("review")
            .arg("--caller")
            .arg("script-a")
            .arg("tests/fixtures/rust/clean.rs")
            .assert()
            .code(0);
    }
    for _ in 0..2 {
        quorum(home)
            .arg("review")
            .arg("--caller")
            .arg("script-b")
            .arg("tests/fixtures/rust/clean.rs")
            .assert()
            .code(0);
    }
}

#[test]
fn stats_by_caller_json_returns_slices() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    seed_reviews(home);

    let out = quorum(home)
        .args(["stats", "--by-caller", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stats --by-caller --json should succeed");

    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("stdout not JSON: {}\n{}", e, String::from_utf8_lossy(&out.stdout))
    });
    assert_eq!(v["mode"], "by-caller");
    let slices = v["slices"].as_array().expect("slices array");
    let a = slices.iter().find(|s| s["key"] == "script-a")
        .unwrap_or_else(|| panic!("expected script-a slice, got {:?}",
            slices.iter().map(|s| s["key"].as_str()).collect::<Vec<_>>()));
    let b = slices.iter().find(|s| s["key"] == "script-b").unwrap();
    assert_eq!(a["n_reviews"].as_u64().unwrap(), 3, "script-a seeded 3 reviews");
    assert_eq!(b["n_reviews"].as_u64().unwrap(), 2, "script-b seeded 2 reviews");
}

#[test]
fn stats_by_repo_json_returns_slices() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    seed_reviews(home);

    let out = quorum(home)
        .args(["stats", "--by-repo", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["mode"], "by-repo");
    let slices = v["slices"].as_array().expect("slices array");
    assert!(!slices.is_empty(), "expected at least one repo slice");

    // All seed reviews targeted tests/fixtures/rust/clean.rs which is inside the
    // containing git repo — so we expect exactly one slice carrying all 5 reviews.
    // Repo name is derived from the test's CWD so a forked checkout still works.
    let repo = current_repo_basename();
    let repo_slice = slices.iter().find(|s| s["key"] == repo.as_str())
        .unwrap_or_else(|| panic!("expected a '{}' repo slice, got {:?}", repo,
            slices.iter().map(|s| s["key"].as_str()).collect::<Vec<_>>()));
    assert_eq!(repo_slice["n_reviews"].as_u64().unwrap(), 5,
        "all 5 reviews of fixtures in this repo should group into one slice");
    assert_eq!(v["meta"]["total_reviews"].as_u64().unwrap(), 5);
}

#[test]
fn stats_by_caller_compact_is_single_line_no_glyphs() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    seed_reviews(home);

    let out = quorum(home)
        .args(["stats", "--by-caller", "--compact"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    // At most one trailing newline; body must be a single line.
    let body = s.trim_end_matches('\n');
    assert!(!body.contains('\n'), "compact output must be single-line, got:\n{:?}", body);
    assert!(body.starts_with("by-caller:"), "expected by-caller prefix, got: {:?}", body);
    assert!(!body.contains('█'), "compact mode must not contain block glyphs");
}

#[test]
fn stats_rolling_json_returns_windows() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    seed_reviews(home);

    let out = quorum(home)
        .args(["stats", "--rolling", "2", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["mode"], "rolling");
    let slices = v["slices"].as_array().unwrap();
    let keys: Vec<&str> = slices.iter().filter_map(|s| s["key"].as_str()).collect();
    assert_eq!(keys.first(), Some(&"last 2"), "got keys {:?}", keys);
}
