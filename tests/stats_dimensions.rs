//! Integration test: `quorum stats --by-repo / --by-caller / --rolling N` produce dimensional output.

mod support;

use serde_json::Value;
use std::path::Path;
use support::quorum;
use tempfile::TempDir;

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
    assert!(
        out.status.success(),
        "stats --by-caller --json should succeed"
    );

    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON: {}\n{}",
            e,
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(v["mode"], "by-caller");
    let slices = v["slices"].as_array().expect("slices array");
    let a = slices
        .iter()
        .find(|s| s["key"] == "script-a")
        .unwrap_or_else(|| {
            panic!(
                "expected script-a slice, got {:?}",
                slices.iter().map(|s| s["key"].as_str()).collect::<Vec<_>>()
            )
        });
    let b = slices.iter().find(|s| s["key"] == "script-b").unwrap();
    assert_eq!(
        a["n_reviews"].as_u64().unwrap(),
        3,
        "script-a seeded 3 reviews"
    );
    assert_eq!(
        b["n_reviews"].as_u64().unwrap(),
        2,
        "script-b seeded 2 reviews"
    );
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
    let repo_slice = slices
        .iter()
        .find(|s| s["key"] == repo.as_str())
        .unwrap_or_else(|| {
            panic!(
                "expected a '{}' repo slice, got {:?}",
                repo,
                slices.iter().map(|s| s["key"].as_str()).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        repo_slice["n_reviews"].as_u64().unwrap(),
        5,
        "all 5 reviews of fixtures in this repo should group into one slice"
    );
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
    assert!(
        !body.contains('\n'),
        "compact output must be single-line, got:\n{:?}",
        body
    );
    assert!(
        body.starts_with("by-caller:"),
        "expected by-caller prefix, got: {:?}",
        body
    );
    assert!(
        !body.contains('█'),
        "compact mode must not contain block glyphs"
    );
}

#[test]
fn stats_by_file_json_returns_hotspots() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let quorum_dir = home.join(".quorum");
    std::fs::create_dir_all(&quorum_dir).unwrap();

    let entries = [
        serde_json::json!({"file_path":"src/a.rs","finding_title":"buf overflow","finding_category":"security","verdict":"tp","timestamp":"2026-01-01T00:00:00Z","provenance":"human","reason":"real bug"}),
        serde_json::json!({"file_path":"src/a.rs","finding_title":"sql inject","finding_category":"security","verdict":"tp","timestamp":"2026-01-02T00:00:00Z","provenance":"human","reason":"another"}),
        serde_json::json!({"file_path":"src/b.rs","finding_title":"xss","finding_category":"security","verdict":"fp","timestamp":"2026-01-01T00:00:00Z","provenance":"human","reason":"false alarm"}),
        serde_json::json!({"file_path":"src/a.rs","finding_title":"eval","finding_category":"security","verdict":"fp","timestamp":"2026-01-03T00:00:00Z","provenance":"human","reason":"nah"}),
    ];
    let content: String = entries.iter().map(|e| e.to_string() + "\n").collect();
    std::fs::write(quorum_dir.join("feedback.jsonl"), content).unwrap();

    let out = quorum(home)
        .args(["stats", "--by-file", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stats --by-file --json should succeed, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON: {}\n{}",
            e,
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(v["mode"], "by-file");
    let rows = v["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 2, "expected 2 file hotspots");
    assert_eq!(rows[0]["file_path"], "src/a.rs", "src/a.rs has more TPs");
    assert_eq!(rows[0]["tp_count"], 2);
    assert_eq!(rows[0]["fp_count"], 1);
    assert_eq!(rows[1]["file_path"], "src/b.rs");
    assert_eq!(rows[1]["fp_count"], 1);
}

#[test]
fn stats_by_file_top_limits_output() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let quorum_dir = home.join(".quorum");
    std::fs::create_dir_all(&quorum_dir).unwrap();

    let entries = [
        serde_json::json!({"file_path":"src/a.rs","finding_title":"a","finding_category":"bug","verdict":"tp","timestamp":"2026-01-01T00:00:00Z","provenance":"human","reason":"r"}),
        serde_json::json!({"file_path":"src/b.rs","finding_title":"b","finding_category":"bug","verdict":"tp","timestamp":"2026-01-01T00:00:00Z","provenance":"human","reason":"r"}),
        serde_json::json!({"file_path":"src/c.rs","finding_title":"c","finding_category":"bug","verdict":"tp","timestamp":"2026-01-01T00:00:00Z","provenance":"human","reason":"r"}),
    ];
    let content: String = entries.iter().map(|e| e.to_string() + "\n").collect();
    std::fs::write(quorum_dir.join("feedback.jsonl"), content).unwrap();

    let out = quorum(home)
        .args(["stats", "--by-file", "--top", "1", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = v["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 1, "--top 1 should limit to 1 row");
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
