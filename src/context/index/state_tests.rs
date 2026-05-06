use std::path::PathBuf;

use tempfile::tempdir;

use super::state::{CURRENT_SCHEMA_VERSION, IndexState, StateCheck, StateError};

fn sample(hash: &str) -> IndexState {
    IndexState::new(hash.to_string())
}

#[test]
fn load_returns_none_when_missing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let result = IndexState::load(&path).unwrap();
    assert!(result.is_none());
}

#[test]
fn save_and_reload_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = sample("h1");
    state.save(&path).unwrap();
    let loaded = IndexState::load(&path).unwrap().unwrap();
    assert_eq!(state, loaded);
}

#[test]
fn check_fresh_when_no_state() {
    let outcome = IndexState::check_against(None, "anyhash");
    assert_eq!(outcome, StateCheck::Fresh);
}

#[test]
fn check_ok_on_match() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    sample("h1").save(&path).unwrap();
    let loaded = IndexState::load(&path).unwrap();
    let outcome = IndexState::check_against(loaded.as_ref(), "h1");
    assert_eq!(outcome, StateCheck::Ok);
}

#[test]
fn check_reembed_required_on_hash_drift() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    sample("h1").save(&path).unwrap();
    let loaded = IndexState::load(&path).unwrap();
    let outcome = IndexState::check_against(loaded.as_ref(), "h2");
    assert_eq!(
        outcome,
        StateCheck::ReembedRequired {
            on_disk: "h1".to_string(),
            expected: "h2".to_string(),
        }
    );
}

#[test]
fn check_schema_migration_when_version_older() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let raw = r#"{
        "schema_version": 0,
        "embedder_model_hash": "h1",
        "quorum_version": "0.0.0"
    }"#;
    std::fs::write(&path, raw).unwrap();
    let loaded = IndexState::load(&path).unwrap();
    let outcome = IndexState::check_against(loaded.as_ref(), "h1");
    assert_eq!(
        outcome,
        StateCheck::SchemaMigrationRequired {
            on_disk: 0,
            expected: CURRENT_SCHEMA_VERSION,
        }
    );
}

#[test]
fn save_is_atomic_via_tempfile() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    sample("h1").save(&path).unwrap();
    let tmp: PathBuf = path.with_extension("json.tmp");
    assert!(!tmp.exists(), "tmp file should not exist after rename");
    assert!(path.exists());
}

#[test]
fn save_creates_parent_directories() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("a/b/c/state.json");
    sample("h1").save(&path).unwrap();
    let loaded = IndexState::load(&path).unwrap();
    assert!(loaded.is_some());
}

#[test]
fn load_returns_parse_error_on_garbage() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, b"not valid json {{{").unwrap();
    let result = IndexState::load(&path);
    match result {
        Err(StateError::Parse(_)) => {}
        other => panic!("expected Parse error, got {other:?}"),
    }
}
