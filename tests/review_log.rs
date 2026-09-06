//! Integration test: running `quorum review` writes a record to quorum.db.

mod support;

use rusqlite::Connection;
use support::quorum;
use tempfile::TempDir;

#[test]
fn review_writes_review_to_sqlite() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    quorum(home)
        .arg("review")
        .arg("tests/fixtures/rust/clean.rs")
        .assert()
        .code(0);

    let db_path = home.join(".quorum/quorum.db");
    assert!(db_path.exists(), "quorum.db not created");

    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM reviews", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "expected exactly one review record");

    let (run_id, files_reviewed, timestamp, quorum_version): (String, i64, String, String) = conn
        .query_row(
            "SELECT run_id, files_reviewed, timestamp, quorum_version FROM reviews",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(run_id.len(), 26, "run_id must be 26-char ULID");
    assert_eq!(files_reviewed, 1);
    assert!(!timestamp.is_empty());
    assert!(!quorum_version.is_empty());
}

#[test]
fn caller_flag_overrides_invoked_from() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    quorum(home)
        .arg("review")
        .arg("--caller")
        .arg("my-ci-job")
        .arg("tests/fixtures/rust/clean.rs")
        .assert()
        .code(0);

    let conn = Connection::open(home.join(".quorum/quorum.db")).unwrap();
    let invoked_from: String = conn
        .query_row("SELECT invoked_from FROM reviews", [], |r| r.get(0))
        .unwrap();
    assert_eq!(invoked_from, "my-ci-job");
}

#[test]
fn second_review_appends() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    for _ in 0..2 {
        quorum(home)
            .arg("review")
            .arg("tests/fixtures/rust/clean.rs")
            .assert()
            .code(0);
    }

    let conn = Connection::open(home.join(".quorum/quorum.db")).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM reviews", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "second run should append, not replace");

    let mut stmt = conn.prepare("SELECT run_id FROM reviews").unwrap();
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_ne!(ids[0], ids[1], "each run gets a unique ULID");
}
