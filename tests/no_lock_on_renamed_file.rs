//! Issue #494 / #549: an advisory lock must never be taken on a file that is
//! also replaced by `rename`.
//!
//! POSIX advisory locks attach to the **inode**, not the path. A writer that
//! replaces a file by rename therefore defeats a lock held on the file itself:
//! a blocked appender wakes holding a lock on an inode with no directory
//! entry, its `write_all` returns `Ok`, and the data is gone. Every party
//! behaved correctly and the write succeeded, which is what makes it silent.
//!
//! This happened twice, on the two append-only logs that matter most:
//! `feedback.jsonl` (#494, human verdicts, irreplaceable) and
//! `calibrator_traces.jsonl` (#549). Both were fixed the same way, by moving
//! the lock to a sidecar that is never renamed. Per the Quality gates section
//! of CLAUDE.md, a property that has now been fixed at two paths gets a
//! source-scanning guard rather than a third per-path fix.
//!
//! Scope and ceiling, stated plainly: this scans source text and pairs
//! *within a file*. It catches the ordinary mistake — someone adds a rewrite
//! next to an existing lock — not a determined evasion, and not a lock and a
//! rename split across two modules. Same ceiling as
//! `tests/no_env_mutation.rs` and `tests/spawn_helper_guard.rs`, and
//! acceptable for the same reason: the mistake it catches is the one that
//! actually happened, twice.

use std::path::Path;

/// Advisory lock acquisition via `fs2`.
const LOCKS: &[&str] = &[
    "lock_exclusive",
    "lock_shared",
    "try_lock_exclusive",
    "try_lock_shared",
];

/// The chokepoint. A file that both locks and renames must route its lock
/// through this helper, which appends `.lock` to the data path.
const SIDECAR_HELPER: &str = "sidecar_lock_path";

/// Files that lock and rename but are safe, each with the reason.
///
/// An allowlist that grows without explanation is a guard that has stopped
/// guarding, so every entry states why the pairing is benign.
const ALLOWED: &[(&str, &str)] = &[(
    "skill_audit.rs",
    "The locked path and the renamed path are different files: the advisory \
     lock is on the append-only audit JSONL, while the rename replaces \
     skills.lock (TOML). No lock is ever held on the renamed path.",
)];

fn rust_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src/") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_source_file_locks_and_renames_without_a_sidecar() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(!files.is_empty(), "found no sources to scan under {src:?}");

    let mut offenders = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).expect("read source");
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        if !text.contains("fs::rename") {
            continue;
        }
        if ALLOWED.iter().any(|(f, _)| *f == name) {
            continue;
        }

        // Proximity, not file granularity. `main.rs` is 5k lines with several
        // lock sites; asking only whether the *file* mentions the helper lets
        // a new lock-and-rename pair hide behind an unrelated correct one.
        // Each lock call must have the helper within the preceding window
        // that opens the file it locks.
        // Wide enough to span an OpenOptions builder chain plus the
        // surrounding error handling, which is what sits between the helper
        // call and the lock in every current call site.
        const WINDOW: usize = 45;
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !LOCKS.iter().any(|m| line.contains(m)) {
                continue;
            }
            let lo = i.saturating_sub(WINDOW);
            let routed = lines[lo..=i].iter().any(|l| l.contains(SIDECAR_HELPER));
            if !routed {
                offenders.push(format!(
                    "{}:{}  {}",
                    path.strip_prefix(&src).unwrap_or(path).display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these files take an advisory lock and also rename, without routing the \
         lock through `{SIDECAR_HELPER}`:\n  {}\n\n\
         A lock on a renamed file protects nothing: rename changes which inode \
         the path names, so a blocked writer appends to an orphaned inode and \
         the write silently succeeds into nowhere (#494, #549). Lock a sidecar \
         that is never renamed, or add an allowlist entry stating why the \
         locked path and the renamed path cannot be the same file.",
        offenders.join("\n  ")
    );
}

/// The allowlist must not outlive its subject.
#[test]
fn allowlist_entries_still_exist_and_still_lock_and_rename() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);

    for (allowed, reason) in ALLOWED {
        assert!(!reason.trim().is_empty(), "{allowed} needs a stated reason");
        let found = files.iter().find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy() == *allowed)
        });
        let path = found.unwrap_or_else(|| {
            panic!("allowlisted file {allowed} no longer exists; drop the entry")
        });
        let text = std::fs::read_to_string(path).expect("read source");
        assert!(
            LOCKS.iter().any(|m| text.contains(m)) && text.contains("fs::rename"),
            "{allowed} no longer both locks and renames; drop the allowlist entry \
             rather than leaving a permanent exemption behind"
        );
    }
}
