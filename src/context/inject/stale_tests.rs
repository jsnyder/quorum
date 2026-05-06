use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};

use super::stale::*;
use crate::context::index::traits::{Clock, FixedClock};
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn chunk_with(source: &str, indexed_at: DateTime<Utc>) -> Chunk {
    Chunk {
        id: format!("{source}:x:y"),
        source: source.into(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: Some("y".into()),
        signature: None,
        content: "content".into(),
        metadata: ChunkMeta {
            source_path: "x.rs".into(),
            line_range: LineRange::new(1, 1).unwrap(),
            commit_sha: "c".into(),
            indexed_at,
            source_version: None,
            language: Some("rust".into()),
            is_exported: true,
            neighboring_symbols: vec![],
        },
        provenance: Provenance::new("t", 0.9, "x.rs").unwrap(),
    }
}

fn now_fixed() -> FixedClock {
    FixedClock::from_rfc3339("2026-04-21T12:00:00Z")
}

#[test]
fn no_staleness_returns_none() {
    let chunk = chunk_with("local", Utc::now());
    assert!(NoStaleness.annotate(&chunk).is_none());
}

#[test]
fn timestamp_returns_none_for_clean_repo() {
    let clock = now_fixed();
    let git = FakeGit::with_dirty(false);
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);
    let chunk = chunk_with("local", clock.now() - Duration::hours(1));
    assert!(annotator.annotate(&chunk).is_none());
}

#[test]
fn timestamp_returns_some_for_dirty_local_chunk() {
    let clock = now_fixed();
    let git = FakeGit::with_dirty(true);
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);
    let chunk = chunk_with("local", clock.now() - Duration::hours(2));
    let msg = annotator.annotate(&chunk).expect("expected staleness");
    assert!(msg.contains("source has edits"), "msg: {msg}");
    assert!(msg.contains("2h"), "msg: {msg}");
}

#[test]
fn timestamp_returns_none_for_non_current_source() {
    let clock = now_fixed();
    let git = FakeGit::with_dirty(true);
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);
    let chunk = chunk_with("registry-foo", clock.now() - Duration::hours(2));
    assert!(annotator.annotate(&chunk).is_none());
}

#[test]
fn timestamp_returns_none_when_no_current_source() {
    let clock = now_fixed();
    let git = FakeGit::with_dirty(true);
    let annotator = TimestampStaleness::new(None, None, &git, &clock);
    let chunk = chunk_with("local", clock.now() - Duration::hours(2));
    assert!(annotator.annotate(&chunk).is_none());
}

#[test]
fn format_age_renders_days() {
    let clock = now_fixed();
    let git = FakeGit::with_dirty(true);
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);
    let chunk = chunk_with("local", clock.now() - Duration::days(3));
    let msg = annotator.annotate(&chunk).unwrap();
    assert!(msg.contains("3d"), "msg: {msg}");
}

#[test]
fn format_age_renders_just_now_under_one_minute() {
    let clock = now_fixed();
    let git = FakeGit::with_dirty(true);
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);
    let chunk = chunk_with("local", clock.now() - Duration::seconds(30));
    let msg = annotator.annotate(&chunk).unwrap();
    assert!(msg.contains("just now"), "msg: {msg}");
}

#[test]
fn format_age_renders_singular_units() {
    let clock = now_fixed();
    let git = FakeGit::with_dirty(true);
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);

    let chunk_hr = chunk_with("local", clock.now() - Duration::hours(1));
    let msg_hr = annotator.annotate(&chunk_hr).unwrap();
    assert!(msg_hr.contains("1h"), "msg: {msg_hr}");
    assert!(!msg_hr.contains("1d"), "msg: {msg_hr}");

    let chunk_day = chunk_with("local", clock.now() - Duration::days(1));
    let msg_day = annotator.annotate(&chunk_day).unwrap();
    assert!(msg_day.contains("1d"), "msg: {msg_day}");
}

struct FailingGit;
impl GitOps for FailingGit {
    fn has_local_changes(&self, _: &Path) -> std::io::Result<bool> {
        Err(std::io::Error::other("fake error"))
    }
    fn head_sha(&self, _: &Path) -> std::io::Result<Option<String>> {
        Err(std::io::Error::other("fake error"))
    }
}

#[test]
fn git_error_is_swallowed_returning_none() {
    let clock = now_fixed();
    let git = FailingGit;
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);
    let chunk = chunk_with("local", clock.now() - Duration::hours(2));
    assert!(annotator.annotate(&chunk).is_none());
}

#[test]
#[ignore = "requires `git` binary in PATH; trait contract covered by FakeGit tests"]
fn system_git_implements_trait_without_compile_errors() {
    let tmp = std::env::temp_dir().join("quorum-stale-sysgit-test");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(&tmp)
        .arg("init")
        .output();
    let git = SystemGit;
    let _ = git.has_local_changes(&tmp);
    let _ = std::fs::remove_dir_all(&tmp);
}

struct CountingGit {
    dirty: bool,
    calls: std::cell::Cell<usize>,
}

impl GitOps for CountingGit {
    fn has_local_changes(&self, _: &Path) -> std::io::Result<bool> {
        self.calls.set(self.calls.get() + 1);
        Ok(self.dirty)
    }
    fn head_sha(&self, _: &Path) -> std::io::Result<Option<String>> {
        Ok(None)
    }
}

#[test]
fn timestamp_calls_git_status_only_once_per_annotator() {
    let clock = now_fixed();
    let git = CountingGit {
        dirty: true,
        calls: std::cell::Cell::new(0),
    };
    let root = PathBuf::from("/tmp/repo");
    let annotator = TimestampStaleness::new(Some("local"), Some(root.as_path()), &git, &clock);
    for _ in 0..5 {
        let chunk = chunk_with("local", clock.now() - Duration::hours(1));
        assert!(annotator.annotate(&chunk).is_some());
    }
    assert_eq!(
        git.calls.get(),
        1,
        "has_local_changes must be memoized per annotator instance"
    );
}
