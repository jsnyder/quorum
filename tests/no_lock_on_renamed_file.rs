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
//! Two properties are checked at every advisory-lock call site:
//!
//! 1. **Routed.** The chokepoint `file_util::sidecar_lock_path` appears in the
//!    window above the lock.
//! 2. **Ordered.** No data file is opened before the lock is taken. Swapping
//!    *which* file is locked is not sufficient on its own: if the open comes
//!    first, a rewriter can rename in between and the handle is already on an
//!    orphaned inode when the lock is granted. That bug survived inside its
//!    own fix at two of three call sites and was caught by the tool's review,
//!    not by the first version of this guard.
//!
//! Scope and ceiling, stated plainly. This scans source text with a window,
//! so it catches the two mistakes that actually happened — locking the data
//! file, and locking after opening — and not a determined evasion. Known to
//! escape: assigning the data path to a variable *named* `lock_path` while a
//! correct helper call sits elsewhere in the window. That is a perverse edit
//! rather than a plausible one, and chasing it made the guard fragile enough
//! to produce false positives on every correct site, which is worse. Same
//! ceiling as `tests/no_env_mutation.rs` and `tests/spawn_helper_guard.rs`.
//!
//! Deliberately not gated on the scanned file containing `fs::rename`: in both
//! #494 and #549 the appender and the rewriter live in different modules, so a
//! per-file pairing check excludes the exact shape it exists to catch. An
//! earlier version had that gate and let the ordering bug in `pipeline.rs`
//! through, because `pipeline.rs` renames nothing.

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
const ALLOWED: &[(&str, &str)] = &[
    (
        "skill_audit.rs",
        "Append-only audit JSONL with no rewriter anywhere in the tree: \
         nothing renames over skill_invocations.jsonl or \
         integrator_decisions.jsonl, so there is no inode to orphan. The \
         rename in this file replaces skills.lock (TOML), which is never \
         locked. Revisit if a compaction or backfill command is added for \
         these logs.",
    ),
    (
        "judge.rs",
        "Locks ~/.quorum/judge_cache.jsonl, which is append-only and has no \
         rewriter. Same reasoning as skill_audit.rs, and the contents are a \
         regenerable cache rather than ground truth.",
    ),
];

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

        // Deliberately NOT gated on this file containing `fs::rename`. The
        // hazard is cross-module by nature: in both #494 and #549 the
        // appender and the rewriter live in different files, so a per-file
        // pairing check excludes the exact shape it exists to catch. An
        // earlier version of this guard had that gate and let the ordering
        // bug in `pipeline.rs` through, because `pipeline.rs` renames
        // nothing. Every advisory lock is in scope; exceptions are named.
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
            let window = &lines[lo..=i];

            let routed = window.iter().any(|l| l.contains(SIDECAR_HELPER));
            if !routed {
                offenders.push(format!(
                    "{}:{}  {}  (lock not routed through {SIDECAR_HELPER})",
                    path.strip_prefix(&src).unwrap_or(path).display(),
                    i + 1,
                    line.trim()
                ));
                continue;
            }

            // Ordering. Swapping *which* file is locked is not sufficient: if
            // the data file is opened before the lock is granted, a rewriter
            // can rename in between and the handle is already on an orphaned
            // inode. So any `.open(` before the lock must be the sidecar's
            // own. Found by the quorum review of #549 -- the bug survived
            // inside its own fix at two of three call sites.
            if let Some(bad) = window
                .iter()
                .find(|l| l.contains(".open(") && !l.contains("lock_path"))
            {
                offenders.push(format!(
                    "{}:{}  {}  (data file opened before the lock: {})",
                    path.strip_prefix(&src).unwrap_or(path).display(),
                    i + 1,
                    line.trim(),
                    bad.trim()
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
fn allowlist_entries_still_exist_and_still_lock() {
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
            LOCKS.iter().any(|m| text.contains(m)),
            "{allowed} no longer takes an advisory lock; drop the allowlist entry \
             rather than leaving a permanent exemption behind"
        );
    }
}
