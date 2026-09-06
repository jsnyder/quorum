//! Issue #501: structural guard against integration tests that spawn the
//! `quorum` binary without going through `support::quorum()`.
//!
//! Five test files reached the binary directly and never cleared
//! `QUORUM_API_KEY`, so a developer with a working key paid for a full LLM
//! review on every `cargo test`. `tests/parallel_review.rs` measured 694s
//! against `tests/stats_dimensions.rs`'s 32s at an identical test count --
//! the only structural difference between them was env isolation.
//!
//! Fixing those five files does not stop the sixth. Convention ("remember to
//! call `.env_remove`") is the discipline that already failed; this test is
//! the enforcement. A new integration test that spawns the binary its own way
//! goes red on its first run.
//!
//! Same shape as `every_telemetry_field_is_consumed_or_allowlisted` (#491):
//! the guard fails until someone makes a deliberate decision.

use std::path::Path;

/// Source-level markers for reaching the `quorum` binary directly.
///
/// `cargo_bin` covers both `assert_cmd::Command::cargo_bin("quorum")` and the
/// path-only `assert_cmd::cargo::cargo_bin("quorum")`; `CARGO_BIN_EXE_quorum`
/// covers the `std::process::Command::new(env!(..))` form that
/// `calibrate_backfill.rs` used.
const RAW_SPAWN_MARKERS: &[&str] = &["cargo_bin", "CARGO_BIN_EXE_quorum"];

/// Files exempt from the guard.
///
/// `support/mod.rs` is the sanctioned helper and lives in a subdirectory, so
/// it is never scanned; this list only needs the guard itself, whose source
/// necessarily contains the marker strings.
const EXEMPT: &[&str] = &["spawn_helper_guard.rs"];

#[test]
fn no_integration_test_spawns_quorum_outside_the_support_helper() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");

    let mut offenders: Vec<String> = Vec::new();

    let entries = std::fs::read_dir(&tests_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", tests_dir.display()));

    for entry in entries {
        let path = entry.expect("readable dir entry").path();

        // Subdirectories (support/, fixtures/) are not integration test
        // targets and are not scanned.
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }

        let name = path
            .file_name()
            .expect("dir entry has a file name")
            .to_string_lossy()
            .into_owned();

        if EXEMPT.contains(&name.as_str()) {
            continue;
        }

        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        for (i, line) in src.lines().enumerate() {
            if let Some(marker) = RAW_SPAWN_MARKERS.iter().find(|m| line.contains(*m)) {
                offenders.push(format!("{name}:{} uses `{marker}`", i + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Integration tests must spawn the binary via `support::quorum()` (or a \
         documented sibling in tests/support/mod.rs), which strips the LLM env \
         surface so no test can make a paid call unless it explicitly asks.\n\n\
         Spawning directly re-opens issue #501: a developer with QUORUM_API_KEY \
         set pays for a real LLM review on every `cargo test`, while CI -- which \
         has no key -- never sees it.\n\n\
         Offending sites:\n  {}\n\n\
         Fix: add `mod support;` and call `support::quorum(home)`. If you need a \
         real endpoint, use `support::quorum_with_cassette()` (replays a recorded \
         response, costs nothing) or `support::quorum_live()` (requires \
         QUORUM_TEST_LIVE=1 and spends real money).",
        offenders.join("\n  ")
    );
}
