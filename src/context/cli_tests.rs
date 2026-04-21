//! Unit tests for `src/context/cli.rs`.

use super::cli::{
    run_context_cmd, AddArgs, AddLocation, ContextCmd, ContextDeps, ListArgs, ListFormat, TestDeps,
};
use super::config::{SourceLocation, SourcesConfig};
use std::path::PathBuf;

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

    let expected = deps.home_dir().join("sources.toml");
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
fn run_context_cmd_init_is_idempotent_and_preserves_existing_config() {
    let deps = TestDeps::new();
    let _ = run_context_cmd(&ContextCmd::Init, &deps).expect("first init succeeds");
    let sources_path = deps.home_dir().join("sources.toml");
    // Mutate the file so we can prove re-init does NOT overwrite it.
    let sentinel = "# sentinel: hand-edited after init\n[context]\nauto_inject = false\n";
    std::fs::write(&sources_path, sentinel).unwrap();

    let output = run_context_cmd(&ContextCmd::Init, &deps).expect("re-init succeeds");

    assert!(
        output.created_paths.is_empty(),
        "re-init must not report created paths: {:?}",
        output.created_paths
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
    let after = std::fs::read_to_string(&sources_path).unwrap();
    assert_eq!(after, sentinel, "re-init must not clobber existing config");
}

#[test]
fn run_context_cmd_init_writes_directly_under_home_dir() {
    // Regression for the bug where ProdDeps (home_dir = ~/.quorum) plus the
    // handler joining ".quorum" again produced ~/.quorum/.quorum/sources.toml.
    // With home_dir treated as the state root, the file must land directly
    // at <home>/sources.toml — no extra ".quorum" component.
    let deps = TestDeps::new();
    let output = run_context_cmd(&ContextCmd::Init, &deps).unwrap();
    let expected = deps.home_dir().join("sources.toml");
    assert!(expected.exists(), "sources.toml must land at <home>/sources.toml");
    assert_eq!(output.created_paths, vec![expected]);
    let doubled = deps.home_dir().join(".quorum").join("sources.toml");
    assert!(
        !doubled.exists(),
        "must NOT create <home>/.quorum/sources.toml (double-join)"
    );
}

// --- add -------------------------------------------------------------------

fn path_add_args(name: &str, kind: &str, path: &str) -> AddArgs {
    AddArgs {
        name: name.to_string(),
        kind: kind.to_string(),
        location: AddLocation::Path(PathBuf::from(path)),
        weight: None,
        ignore: Vec::new(),
    }
}

fn git_add_args(name: &str, kind: &str, url: &str, rev: Option<&str>) -> AddArgs {
    AddArgs {
        name: name.to_string(),
        kind: kind.to_string(),
        location: AddLocation::Git {
            url: url.to_string(),
            rev: rev.map(str::to_string),
        },
        weight: None,
        ignore: Vec::new(),
    }
}

#[test]
fn add_appends_path_source_to_sources_toml() {
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");

    let mut args = path_add_args("core", "rust", "/tmp/core");
    args.weight = Some(3);
    args.ignore = vec!["target/**".to_string(), "**/*.snap".to_string()];

    let out = run_context_cmd(&ContextCmd::Add(args), &deps).expect("add path");
    assert!(
        out.created_paths.is_empty(),
        "add mutates sources.toml; no new paths: {:?}",
        out.created_paths
    );
    assert!(
        out.stdout.contains("added source") && out.stdout.contains("core"),
        "stdout must announce added source: {:?}",
        out.stdout
    );

    let cfg = SourcesConfig::load(&deps.home_dir().join("sources.toml"))
        .expect("re-load after add");
    assert_eq!(cfg.sources.len(), 1);
    let e = &cfg.sources[0];
    assert_eq!(e.name, "core");
    assert_eq!(e.weight, Some(3));
    assert_eq!(e.ignore, vec!["target/**".to_string(), "**/*.snap".to_string()]);
    assert_eq!(
        e.location,
        SourceLocation::Path(PathBuf::from("/tmp/core"))
    );
}

#[test]
fn add_appends_git_source_with_optional_rev() {
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");

    let args = git_add_args(
        "stdlib",
        "rust",
        "https://github.com/rust-lang/rust",
        Some("1.80.0"),
    );
    run_context_cmd(&ContextCmd::Add(args), &deps).expect("add git w/ rev");

    let args_no_rev = git_add_args("ha", "python", "https://github.com/home-assistant/core", None);
    run_context_cmd(&ContextCmd::Add(args_no_rev), &deps).expect("add git no rev");

    let cfg = SourcesConfig::load(&deps.home_dir().join("sources.toml")).expect("load");
    assert_eq!(cfg.sources.len(), 2);
    assert_eq!(
        cfg.sources[0].location,
        SourceLocation::Git {
            url: "https://github.com/rust-lang/rust".to_string(),
            rev: Some("1.80.0".to_string()),
        }
    );
    assert_eq!(
        cfg.sources[1].location,
        SourceLocation::Git {
            url: "https://github.com/home-assistant/core".to_string(),
            rev: None,
        }
    );
}

#[test]
fn add_rejects_duplicate_name() {
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");
    run_context_cmd(
        &ContextCmd::Add(path_add_args("core", "rust", "/tmp/core")),
        &deps,
    )
    .expect("first add");

    let err = run_context_cmd(
        &ContextCmd::Add(path_add_args("core", "rust", "/tmp/other")),
        &deps,
    )
    .expect_err("duplicate must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("core") && msg.to_lowercase().contains("duplicate"),
        "error should call out duplicate name: {msg:?}"
    );

    // On-disk file must still have exactly one entry.
    let cfg = SourcesConfig::load(&deps.home_dir().join("sources.toml")).expect("load");
    assert_eq!(cfg.sources.len(), 1);
}

#[test]
fn add_rejects_empty_name_or_empty_path_or_empty_url() {
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");

    let e1 = run_context_cmd(
        &ContextCmd::Add(path_add_args("", "rust", "/tmp/x")),
        &deps,
    )
    .expect_err("empty name");
    assert!(format!("{e1}").to_lowercase().contains("name"));

    let e2 = run_context_cmd(
        &ContextCmd::Add(path_add_args("x", "rust", "   ")),
        &deps,
    )
    .expect_err("empty path");
    assert!(format!("{e2}").to_lowercase().contains("path"));

    let e3 = run_context_cmd(
        &ContextCmd::Add(git_add_args("y", "rust", "   ", None)),
        &deps,
    )
    .expect_err("empty url");
    assert!(format!("{e3}").to_lowercase().contains("url") || format!("{e3}").to_lowercase().contains("git"));

    // None of the failed adds should have mutated the file.
    let cfg = SourcesConfig::load(&deps.home_dir().join("sources.toml")).expect("load");
    assert_eq!(cfg.sources.len(), 0);
}

#[test]
fn add_rejects_unknown_kind() {
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");
    let err = run_context_cmd(
        &ContextCmd::Add(path_add_args("x", "cobol", "/tmp/x")),
        &deps,
    )
    .expect_err("unknown kind");
    let msg = format!("{err}").to_lowercase();
    assert!(msg.contains("kind") && msg.contains("cobol"), "{msg:?}");
}

#[test]
fn add_is_atomic_on_write_failure() {
    // Pre-existing sentinel file that must NOT be partially clobbered.
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");
    let sources_path = deps.home_dir().join("sources.toml");
    run_context_cmd(
        &ContextCmd::Add(path_add_args("core", "rust", "/tmp/core")),
        &deps,
    )
    .expect("seed add");
    let before = std::fs::read_to_string(&sources_path).expect("read before");

    // Trigger a failure: duplicate names bail *before* writing. The file
    // must be byte-identical afterwards (no tmp file left dangling).
    let err = run_context_cmd(
        &ContextCmd::Add(path_add_args("core", "rust", "/tmp/other")),
        &deps,
    )
    .expect_err("duplicate fails");
    drop(err);

    let after = std::fs::read_to_string(&sources_path).expect("read after");
    assert_eq!(
        before, after,
        "sources.toml must be byte-identical after failed add"
    );

    // No stray temp artefacts left behind.
    let home = deps.home_dir();
    for entry in std::fs::read_dir(home).expect("readdir") {
        let entry = entry.expect("entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            name == "sources.toml",
            "unexpected file left in home dir: {name}"
        );
    }
}

// --- list ------------------------------------------------------------------

#[test]
fn list_on_uninitialized_repo_returns_friendly_message_not_error() {
    let deps = TestDeps::new();
    let out = run_context_cmd(&ContextCmd::List(ListArgs::default()), &deps)
        .expect("list on uninit is NOT an error");
    assert!(
        out.warnings.iter().any(|w| w.contains("no sources")
            || w.contains("context init")),
        "should warn and suggest init: {:?}",
        out.warnings
    );
    assert!(
        out.stdout.to_lowercase().contains("no sources")
            || out.stdout.to_lowercase().contains("context init"),
        "stdout should guide the user: {:?}",
        out.stdout
    );
}

#[test]
fn list_renders_table_of_registered_sources() {
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");
    let mut a = path_add_args("core", "rust", "/tmp/core");
    a.weight = Some(2);
    a.ignore = vec!["target/**".to_string()];
    run_context_cmd(&ContextCmd::Add(a), &deps).expect("add 1");
    run_context_cmd(
        &ContextCmd::Add(git_add_args(
            "ha",
            "python",
            "https://github.com/home-assistant/core",
            Some("dev"),
        )),
        &deps,
    )
    .expect("add 2");

    let out = run_context_cmd(&ContextCmd::List(ListArgs::default()), &deps).expect("list");

    // Table must mention each name, kind, location summary, weight, and
    // ignore count. We check for substrings rather than exact layout to
    // avoid brittle whitespace assertions.
    let s = &out.stdout;
    assert!(s.contains("core"), "{s}");
    assert!(s.contains("rust"), "{s}");
    assert!(s.contains("/tmp/core"), "{s}");
    assert!(s.contains("ha"), "{s}");
    assert!(s.contains("python"), "{s}");
    assert!(s.contains("home-assistant/core"), "{s}");
    assert!(s.contains("dev"), "{s}");
    // ignore count "1" for core, "0" for ha
    assert!(s.contains("NAME") || s.contains("name"), "header expected: {s}");
}

#[test]
fn list_json_output_has_stable_field_names() {
    let deps = TestDeps::new();
    run_context_cmd(&ContextCmd::Init, &deps).expect("init");
    run_context_cmd(
        &ContextCmd::Add(path_add_args("core", "rust", "/tmp/core")),
        &deps,
    )
    .expect("add");

    let args = ListArgs {
        format: ListFormat::Json,
    };
    let out = run_context_cmd(&ContextCmd::List(args), &deps).expect("list json");

    let v: serde_json::Value =
        serde_json::from_str(&out.stdout).expect("list --json must emit valid JSON");
    let arr = v
        .get("sources")
        .and_then(|x| x.as_array())
        .expect("top-level {sources: [...]}");
    assert_eq!(arr.len(), 1);
    let s0 = &arr[0];
    // Stable field names — pin the schema.
    for key in ["name", "kind", "location", "weight", "ignore"] {
        assert!(
            s0.get(key).is_some(),
            "missing stable field '{key}' in JSON: {s0}"
        );
    }
    // location is a tagged object: {"path": "..."} or {"git": {...}}
    let loc = s0.get("location").unwrap();
    assert!(
        loc.get("path").is_some() || loc.get("git").is_some(),
        "location must be {{path}} or {{git}}: {loc}"
    );
    assert_eq!(s0.get("name").and_then(|x| x.as_str()), Some("core"));
    assert_eq!(s0.get("kind").and_then(|x| x.as_str()), Some("rust"));
}
