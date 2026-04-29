# drain_inbox symlink + size-cap hardening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Harden `FeedbackStore::drain_inbox` (src/feedback.rs:371) against symlink-redirect, FIFO-hang, and unbounded-allocation attacks. Mirror the #120 architecture (just shipped on src/ast_grep.rs).

**Architecture:** Three layered guards on the inbox ingestion path:
1. **At iteration**: replace `!p.is_dir()` (which follows symlinks) with `symlink_metadata` regular-file check.
2. **At read**: a `read_inbox_file` helper using `OpenOptions` with `libc::O_NOFOLLOW | libc::O_NONBLOCK`, validates regular-file via fstat after open, caps size at 1 MiB, and reads via `.take(MAX_INBOX_FILE_BYTES + 1)` defensive bound.
3. **Fail closed**: any rejected file becomes a `DrainError` and stays in `inbox/` (NOT moved to processing/) so an operator notices.

**Tech Stack:** Rust, std::os::unix::fs::OpenOptionsExt, libc crate (already a dep after #120).

---

## Threat model

External agents drop JSONL feedback into `~/.quorum/inbox/`. The directory is process-readable, but a compromised dependency, IDE plugin, or local-write attacker can:
- Place a symlink `inbox/evil.jsonl -> /etc/passwd`. `!p.is_dir()` returns true. `fs::rename` moves the symlink. `read_to_string(&claimed)` reads `/etc/passwd` and tries to parse it (no exfil because we don't echo content, but DoS-by-spam errors and the parse-attempt is unwanted I/O).
- Place a symlink `inbox/evil.jsonl -> /dev/zero` (or any large file). `read_to_string` allocates until OOM.
- Place a FIFO at `inbox/evil.jsonl`. `read_to_string` blocks indefinitely (kills daemon mode).
- Place a 10 GiB file. No size cap → OOM.

Same architectural class as #120 ast_grep symlink/YAML-DoS. Same fix.

---

## Task 1: Plan + test names committed (this doc)

Done by writing this file.

---

## Task 2: Add `MAX_INBOX_FILE_BYTES` constant + `read_inbox_file` helper

**Files:**
- Modify: `src/feedback.rs` — add helper above `drain_inbox`

**Step 1: Write the failing tests** — three guard tests in the existing test module (~line 1190 area):

```rust
#[test]
#[cfg(unix)]
fn drain_inbox_skips_symlinked_inbox_file() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    // Real file outside inbox holding valid feedback
    let outside = dir.path().join("outside.jsonl");
    std::fs::write(&outside, valid_external_jsonl_line()).unwrap();

    // Symlink inside inbox pointing at it
    symlink(&outside, inbox.join("evil.jsonl")).unwrap();

    let report = store.drain_inbox(&inbox, &processed).unwrap();

    // The symlink must be skipped, not ingested.
    assert_eq!(report.entries, 0, "symlinked inbox file must not be ingested");
    assert!(
        report.errors.iter().any(|e| e.message.starts_with("rejected:") && e.message.contains("symlink")),
        "expected 'rejected: symlink ...' error, got: {:?}",
        report.errors
    );
    // Fail-closed: symlink must remain in inbox/, never reach processing/ or processed/.
    assert!(inbox.join("evil.jsonl").exists(), "symlink must remain in inbox/ (fail-closed)");
    assert!(!inbox.join("processing").join("evil.jsonl").exists());
    assert!(
        !processed.exists() || std::fs::read_dir(&processed).unwrap().next().is_none(),
        "rejected symlink must not flow into processed/"
    );
}

#[test]
#[cfg(unix)]
fn drain_inbox_rejects_oversized_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    // Write 2 MiB (over the 1 MiB cap)
    let huge = "x".repeat(2 * 1024 * 1024);
    std::fs::write(inbox.join("huge.jsonl"), huge).unwrap();

    let report = store.drain_inbox(&inbox, &processed).unwrap();

    assert_eq!(report.entries, 0);
    assert!(
        report.errors.iter().any(|e| e.message.starts_with("rejected:") && e.message.contains("size")),
        "expected 'rejected: ... size ...' error, got: {:?}",
        report.errors
    );
    assert!(inbox.join("huge.jsonl").exists(), "oversized file must remain in inbox/ (fail-closed)");
}

#[test]
#[cfg(unix)]
fn drain_inbox_rejects_non_regular_file() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    // Bind a Unix socket inside inbox with a .jsonl name. Non-regular file.
    let sock = inbox.join("evil.jsonl");
    let _listener = UnixListener::bind(&sock).unwrap();

    let report = store.drain_inbox(&inbox, &processed).unwrap();

    assert_eq!(report.entries, 0);
    assert!(
        report.errors.iter().any(|e| e.message.starts_with("rejected:")),
        "expected 'rejected: ...' error for non-regular file, got: {:?}",
        report.errors
    );
    assert!(inbox.join("evil.jsonl").exists(), "non-regular file must remain in inbox/ (fail-closed)");
}
```

**Helper for tests:** `fn valid_external_jsonl_line() -> &'static str` returning a single well-formed `ExternalVerdictInputWire` JSON line. Place near the other test helpers in the file.

**Step 2: Run tests** — confirm all three FAIL (with the current vulnerable code).

**Step 3: Implement the helper**

```rust
/// Maximum bytes read from a single inbox file. External agents have no
/// reason to drop multi-MB feedback; cap protects against symlink-to-/dev/zero
/// and runaway file growth.
const MAX_INBOX_FILE_BYTES: u64 = 1024 * 1024;

/// Open an inbox file with O_NOFOLLOW (refuse symlinks at the syscall
/// boundary) + O_NONBLOCK (so a FIFO at this path errors EWOULDBLOCK
/// instead of hanging the drain loop). Validate regular-file via fstat
/// after open, cap size, read via `.take(MAX+1)` to defend against
/// inodes that lie about size (proc/sysfs/network FS).
#[cfg(unix)]
fn read_inbox_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;

    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if meta.size() > MAX_INBOX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("exceeds size cap of {MAX_INBOX_FILE_BYTES} bytes"),
        ));
    }
    let mut buf = String::new();
    file.take(MAX_INBOX_FILE_BYTES + 1).read_to_string(&mut buf)?;
    if buf.len() as u64 > MAX_INBOX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "exceeds size cap during read",
        ));
    }
    Ok(buf)
}
```

**Step 4: Wire into drain_inbox** — replace the iteration filter and the `read_to_string` call:

At ~line 386:
```rust
.filter(|p| !p.is_dir())
```
becomes:
```rust
.filter(|p| {
    // Reject symlinks and non-regular files at iteration time.
    // O_NOFOLLOW in read_inbox_file is the load-bearing guard, but
    // pre-filtering avoids creating spurious processing/ entries.
    match std::fs::symlink_metadata(p) {
        Ok(m) => m.file_type().is_file(),
        Err(_) => false,
    }
})
```

And at ~line 432, replace `std::fs::read_to_string(&claimed)` with a call sequence that:
1. Calls `read_inbox_file(&claimed)`.
2. On error, logs to `report.errors` and **leaves the file in processing/** (existing behavior preserved — operator inspection).

For non-regular / oversized / symlink rejections caught at the iteration filter, file never enters processing/ to begin with.

**Step 5: Run tests** — confirm GREEN.

**Step 6: Commit.**

---

## Task 2.5: Boundary + FIFO tests (added from antipattern review 2026-04-29)

**Files:** `src/feedback.rs` test module

```rust
#[test]
#[cfg(unix)]
fn drain_inbox_rejects_fifo_file() {
    use std::ffi::CString;
    let dir = tempfile::tempdir().unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    let fifo_path = inbox.join("evil.jsonl");
    let cstr = CString::new(fifo_path.to_str().unwrap()).unwrap();
    let rc = unsafe { libc::mkfifo(cstr.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo failed");

    let report = store.drain_inbox(&inbox, &processed).unwrap();

    assert_eq!(report.entries, 0);
    assert!(report.errors.iter().any(|e| e.message.starts_with("rejected:")));
    // Fail-closed: file remains in inbox/, NOT in processing/ or processed/.
    assert!(fifo_path.exists());
    assert!(!inbox.join("processing").join("evil.jsonl").exists());
}

#[test]
#[cfg(unix)]
fn drain_inbox_accepts_file_at_size_cap() {
    let dir = tempfile::tempdir().unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    // One valid JSONL line padded with whitespace lines to reach exactly 1 MiB.
    let line = valid_external_jsonl_line();
    let mut content = String::with_capacity(MAX_INBOX_FILE_BYTES as usize);
    content.push_str(line);
    content.push('\n');
    while content.len() < MAX_INBOX_FILE_BYTES as usize {
        content.push('\n');
    }
    content.truncate(MAX_INBOX_FILE_BYTES as usize);
    std::fs::write(inbox.join("at_cap.jsonl"), &content).unwrap();

    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.entries, 1, "exactly-at-cap must be accepted");
}

#[test]
#[cfg(unix)]
fn drain_inbox_rejects_file_one_byte_over_cap() {
    let dir = tempfile::tempdir().unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();

    let huge = "x".repeat(MAX_INBOX_FILE_BYTES as usize + 1);
    std::fs::write(inbox.join("over.jsonl"), huge).unwrap();

    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.entries, 0);
    assert!(report.errors.iter().any(|e| e.message.starts_with("rejected:")));
}
```

---

## Task 3: Regression guard for normal flow

**Files:** `src/feedback.rs` test module

```rust
#[test]
fn drain_inbox_happy_path_unaffected_by_nofollow_helper() {
    // Validates the existing happy-path tests still pass with the new helper.
    // Distinct from drain_inbox_valid_file_appends_and_moves (l.1222) so a
    // future regression that breaks normal ingestion lights up two tests, not one.
    let dir = tempfile::tempdir().unwrap();
    let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
    let inbox = dir.path().join("inbox");
    let processed = dir.path().join("processed");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("ok.jsonl"), valid_external_jsonl_line()).unwrap();

    let report = store.drain_inbox(&inbox, &processed).unwrap();
    assert_eq!(report.entries, 1);
    assert!(report.errors.is_empty(), "no errors expected, got: {:?}", report.errors);
}
```

This test should pass immediately once Task 2 is GREEN.

---

## Task 4: CHANGELOG + plan reference

**Files:** `CHANGELOG.md`

Add under `[Unreleased]` -> Fixed:
- "drain_inbox no longer follows symlinks, hangs on FIFOs, or reads unbounded inbox files (#124-related; closes drain_inbox attack surface flagged in 5-file panel 2026-04-29). Mirrors #120 architecture for src/ast_grep.rs."

---

## Verification gates

- `cargo test --bin quorum drain_inbox` — all green
- `cargo test --bin quorum` — full suite green
- `cargo clippy --bin quorum -- -W clippy::all` — no new warnings on touched lines
- `cargo build --release` — clean

## Out of scope

- Changes to record_external trust-boundary policy (rejected verdicts, confidence clamping). Already correct.
- Calibrator changes. (Tracked in #122/#123/#124.)
- macOS-specific `O_SYMLINK` flag. O_NOFOLLOW alone is sufficient on Linux + macOS for this surface.
