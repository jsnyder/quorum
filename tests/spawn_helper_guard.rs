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
//!
//! # Scope
//!
//! This is a tripwire, not a sandbox. It matches source text, so a launch form
//! it does not recognise -- a path built at runtime, a shell indirection, a
//! future `assert_cmd` spelling -- passes unnoticed. That is inherent to
//! scanning source rather than intercepting execution, and it is an acceptable
//! ceiling: the job is to catch the honest mistake of a test author who does
//! not know the helper exists, not to stop someone determined to evade it.
//! Layer 1 (stripping in the constructor) and layer 2 (the tripwire base_url)
//! are what hold if this is bypassed.

use std::path::Path;

/// Source-level markers for reaching the `quorum` binary directly.
///
/// `cargo_bin` covers both `assert_cmd::Command::cargo_bin("quorum")` and the
/// path-only `assert_cmd::cargo::cargo_bin("quorum")`; `CARGO_BIN_EXE_quorum`
/// covers the `std::process::Command::new(env!(..))` form that
/// `calibrate_backfill.rs` used.
const RAW_SPAWN_MARKERS: &[&str] = &["cargo_bin", "CARGO_BIN_EXE_quorum"];

/// Paths exempt from the guard, relative to `tests/`.
///
/// `support/mod.rs` IS the sanctioned helper, so it necessarily spawns the
/// binary. The guard itself necessarily contains the marker strings.
/// Deliberately short: a guard with many exceptions is not a guard.
const EXEMPT: &[&str] = &["spawn_helper_guard.rs", "support/mod.rs"];

/// Every `.rs` file under `tests/`, recursively.
///
/// Recursing matters. A helper module in a subdirectory (`tests/support/`,
/// or a new `tests/helpers/`) is not itself a test target, but it *is*
/// compiled into the targets that declare it -- so a spawn hidden there
/// reaches the binary just as directly while a top-level-only scan waves it
/// through. `fixtures/` is skipped because it holds test data, not code.
fn rust_sources_under(dir: &Path, root: &Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));

    for entry in entries {
        let path = entry.expect("readable dir entry").path();

        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "fixtures") {
                continue;
            }
            rust_sources_under(&path, root, out);
            continue;
        }

        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push((rel, path));
    }
}

#[test]
fn no_integration_test_spawns_quorum_outside_the_support_helper() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");

    let mut sources = Vec::new();
    rust_sources_under(&tests_dir, &tests_dir, &mut sources);

    assert!(
        !sources.is_empty(),
        "scanned {} and found no .rs files -- the guard would pass vacuously",
        tests_dir.display()
    );

    let mut offenders: Vec<String> = Vec::new();

    for (rel, path) in sources {
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }

        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        for (i, line) in src.lines().enumerate() {
            // Comments are skipped so that *writing about* the guard does not
            // trip it. Deliberately naive: this is a tripwire, and erring
            // toward a false positive is the safe direction for one.
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(marker) = RAW_SPAWN_MARKERS.iter().find(|m| line.contains(*m)) {
                offenders.push(format!("{rel}:{} uses `{marker}`", i + 1));
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
         real endpoint, use `support::with_cassette()` (replays a recorded \
         response, costs nothing) or `support::quorum_live()` (requires \
         QUORUM_TEST_LIVE=1 and spends real money).",
        offenders.join("\n  ")
    );
}
