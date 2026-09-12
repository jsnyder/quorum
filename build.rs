use std::process::Command;

/// Emitted when the working-tree state could not be determined. Must match the
/// constant of the same meaning in `src/main.rs`.
const TREE_STATE_UNKNOWN: &str = "unknown";

/// Run a git command, returning its trimmed stdout, distinguishing a failed
/// probe (`None`) from one that succeeded with nothing to say (`Some("")`).
///
/// The distinction matters for exactly one caller: `git status --porcelain`
/// prints nothing for a clean tree, so collapsing the two cases would report a
/// *failed* status probe as a clean tree -- a provenance claim the build
/// cannot actually support, and the same silent fallback #517 exists to
/// remove.
fn git_output(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// As `git_output`, but for probes where an empty answer is not an answer: a
/// SHA or a date that came back blank is a failure, not a value.
///
/// Every failure mode is the same result: no provenance. `.git` absent (a
/// `cargo install` from a crates.io tarball), git not on PATH, a shallow or
/// broken checkout -- none of them should fail the build.
fn git(args: &[&str]) -> Option<String> {
    git_output(args).filter(|s| !s.is_empty())
}

/// Bake the commit, its date, and the working-tree state into the binary (#517).
///
/// Without this, a seven-day-old install and a fresh one both say `0.31.0` and
/// nothing distinguishes them -- which is how a stale binary gets blamed on the
/// tool rather than on itself.
fn emit_build_provenance() {
    // Re-run when HEAD moves, or this whole exercise bakes one SHA on the
    // first build and reports it forever -- #517's bug, reintroduced by its
    // own fix. `--git-path` resolves correctly inside a worktree, where `.git`
    // is a file rather than a directory.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    // HEAD on a branch points at a ref file; that file is what changes on
    // commit, so watch it too.
    if let Some(refname) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(refpath) = git(&["rev-parse", "--git-path", &refname])
    {
        println!("cargo:rerun-if-changed={refpath}");
    }

    // The dirty flag is only as fresh as the last build-script run, and this
    // script already opts into a narrow rerun set by printing any
    // `rerun-if-changed` at all. Without these, editing a file and rebuilding
    // produces a binary that reports the tree as clean -- measured, not
    // assumed. `src` and the manifests are what actually change the binary;
    // a change confined to `tests/` or `docs/` still reports clean, which is
    // the known ceiling here.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");

    match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(sha) => {
            // Three states, not two. A status probe that fails tells us
            // nothing about the tree, and saying "clean" there would be a
            // claim rather than a measurement.
            let dirty = match git_output(&["status", "--porcelain"]) {
                Some(out) if out.is_empty() => "0",
                Some(_) => "1",
                None => TREE_STATE_UNKNOWN,
            };
            println!("cargo:rustc-env=QUORUM_BUILD_SHA={sha}");
            println!("cargo:rustc-env=QUORUM_BUILD_DIRTY={dirty}");
            println!(
                "cargo:rustc-env=QUORUM_COMMIT_DATE={}",
                git(&["log", "-1", "--format=%cd", "--date=short"])
                    .unwrap_or_else(|| "unknown".into())
            );
        }
        None => {
            // Deliberately not silent. An empty or absent SHA that prints as
            // nothing is indistinguishable from a tool that never had the
            // feature; `unavailable` says the build could not determine it.
            println!("cargo:rustc-env=QUORUM_BUILD_SHA=unavailable");
            println!("cargo:rustc-env=QUORUM_BUILD_DIRTY={TREE_STATE_UNKNOWN}");
            println!("cargo:rustc-env=QUORUM_COMMIT_DATE=unavailable");
        }
    }
}

fn main() {
    // The build date is independent of git: a tarball build still knows when it
    // was compiled, even when it cannot know what it was compiled from.
    println!(
        "cargo:rustc-env=QUORUM_BUILD_DATE={}",
        chrono::Utc::now().format("%Y-%m-%d")
    );
    emit_build_provenance();

    // Compile the tree-sitter-dockerfile grammar from vendored C sources.
    // We vendor this instead of using the tree-sitter-dockerfile crate because
    // that crate depends on tree-sitter 0.20, which conflicts with our 0.25.
    let src_dir = std::path::Path::new("grammars/tree-sitter-dockerfile/src");

    cc::Build::new()
        .include(src_dir)
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .file(src_dir.join("parser.c"))
        .file(src_dir.join("scanner.c"))
        .compile("tree_sitter_dockerfile");

    println!("cargo:rerun-if-changed=grammars/tree-sitter-dockerfile/src/parser.c");
    println!("cargo:rerun-if-changed=grammars/tree-sitter-dockerfile/src/scanner.c");
}
