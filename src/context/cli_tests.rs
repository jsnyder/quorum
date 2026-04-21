//! Unit tests for `src/context/cli.rs`.

use super::cli::{run_context_cmd, AddArgs, ContextCmd, ContextDeps, TestDeps};
use super::config::SourcesConfig;

#[test]
fn test_deps_home_dir_is_stable_across_calls() {
    let deps = TestDeps::new();
    let first = deps.home_dir().to_path_buf();
    let second = deps.home_dir().to_path_buf();
    assert_eq!(
        first, second,
        "home_dir must return the same path across calls"
    );
    assert!(
        first.exists(),
        "TestDeps should own a tempdir that exists for its lifetime"
    );
}

#[test]
fn run_context_cmd_init_creates_sources_toml() {
    let deps = TestDeps::new();

    let output = run_context_cmd(&ContextCmd::Init, &deps).expect("init succeeds");

    let expected = deps.home_dir().join(".quorum").join("sources.toml");
    assert!(
        expected.exists(),
        "init must create {}",
        expected.display()
    );
    assert_eq!(output.created_paths, vec![expected.clone()]);
    assert!(output.warnings.is_empty(), "first init emits no warnings");
    assert!(
        output.stdout.contains("initialized context"),
        "stdout should announce init: got {:?}",
        output.stdout
    );

    // The written file must parse back through the real loader. This catches
    // any formatting drift (e.g. forgetting a decimal point on a float).
    let parsed = SourcesConfig::load(&expected).expect("written toml must load");
    assert!(
        parsed.sources.is_empty(),
        "fresh sources.toml has no sources yet"
    );
    assert_eq!(parsed.context.inject_max_chunks, 4);
    assert!(parsed.context.auto_inject);
}

#[test]
fn run_context_cmd_init_is_idempotent() {
    let deps = TestDeps::new();
    let _ = run_context_cmd(&ContextCmd::Init, &deps).expect("first init succeeds");

    let output = run_context_cmd(&ContextCmd::Init, &deps).expect("re-init succeeds");

    assert!(
        output.created_paths.is_empty(),
        "re-init must not report created paths: {:?}",
        output.created_paths
    );
    assert!(
        !output.warnings.is_empty(),
        "re-init must surface an 'already initialized' warning"
    );
    assert!(
        output
            .warnings
            .iter()
            .any(|w| w.contains("already exists")),
        "warning should mention the existing file: {:?}",
        output.warnings
    );
    assert!(output.stdout.contains("already initialized"));
}

#[test]
fn run_context_cmd_add_returns_unimplemented_error() {
    // Locks in the stub contract for not-yet-implemented variants: they must
    // return an error rather than panic, so later tasks replacing the body
    // can't regress into a crash.
    let deps = TestDeps::new();
    let err = run_context_cmd(&ContextCmd::Add(AddArgs::default()), &deps)
        .expect_err("add is not implemented yet");
    let msg = format!("{err}");
    assert!(
        msg.contains("not yet implemented"),
        "error should announce stub: got {msg:?}"
    );
    assert!(
        msg.contains("add"),
        "error should name the command variant: got {msg:?}"
    );
}
