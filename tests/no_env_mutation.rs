//! Issue #497: no test may mutate the process environment.
//!
//! `std::env::set_var` and `remove_var` are `unsafe` in edition 2024 because a
//! concurrent `getenv` during `setenv` can fault -- the environ block may be
//! reallocated under the reader. The hazard does **not** depend on the two
//! threads touching the same variable, which is why the four per-module
//! `ENV_LOCK` mutexes this replaced did not make anything sound: each
//! serialised its own module's writes while other modules' tests read env
//! freely, in the same process, on other threads.
//!
//! The fix was not a shared lock. Every function that read the environment now
//! has a pure form taking the values explicitly, with one thin env-reading
//! wrapper, so tests exercise the logic without touching the environment at
//! all. This guard keeps it that way: a new test that reaches for `set_var`
//! fails here rather than reintroducing undefined behaviour that shows up as
//! an unattributable crash months later.
//!
//! Scope: this scans source text under `src/`, so it catches the ordinary
//! mistake, not a determined evasion. Same ceiling as
//! `tests/spawn_helper_guard.rs`, and acceptable for the same reason.

use std::path::Path;

/// Direct environment mutation. Both are `unsafe` in edition 2024.
const MUTATORS: &[&str] = &["std::env::set_var", "std::env::remove_var"];

/// Files allowed to contain the marker strings, relative to `src/`.
///
/// Deliberately empty. If an entry is ever needed, the reason belongs next to
/// it -- a guard whose allowlist grows without explanation is a guard that has
/// stopped guarding.
const EXEMPT: &[&str] = &[];

fn rust_sources(dir: &Path, root: &Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("readable dir entry").path();
        if path.is_dir() {
            rust_sources(&path, root, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
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
fn no_source_file_mutates_the_process_environment() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    rust_sources(&src, &src, &mut sources);

    assert!(
        !sources.is_empty(),
        "scanned {} and found no .rs files -- the guard would pass vacuously",
        src.display()
    );

    let mut offenders = Vec::new();
    for (rel, path) in sources {
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for (i, line) in text.lines().enumerate() {
            // Comments may name the functions; prose about the rule is not a
            // violation of it.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if let Some(m) = MUTATORS.iter().find(|m| line.contains(*m)) {
                offenders.push(format!("{rel}:{} uses `{m}`", i + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Source must not mutate the process environment (#497).\n\n\
         `set_var`/`remove_var` are unsafe in edition 2024: a concurrent \
         `getenv` during `setenv` can fault, and that is true whichever \
         variables the two threads touch. Tests run as threads in one \
         process, so one env-mutating test makes every concurrently-running \
         test unsound -- not just the ones reading the same variable.\n\n\
         Offending sites:\n  {}\n\n\
         Fix: give the function a pure form that takes the values as \
         arguments and keep one thin env-reading wrapper, then test the pure \
         form. See `BaseUrlPolicy::from_values`, `invoked_from`, \
         `grounding_disabled_by` or `ProdDeps::quorum_root_from`.",
        offenders.join("\n  ")
    );
}
