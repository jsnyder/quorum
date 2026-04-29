# Test Plan: `FeedbackStore::drain_inbox` symlink/FIFO/size hardening

Branch: `fix/drain-inbox-symlink-cap` · Target: `src/feedback.rs` (~L371) · Sibling precedent: issue #120 (`src/ast_grep.rs`)

## 1. Acceptance Criteria

### Security (must)
1. **AC-S1 Symlink rejection.** Any inbox path whose `symlink_metadata` reports a symlink MUST be skipped without opening, regardless of where the link points. The original symlink MUST NOT be renamed into `processing/`.
2. **AC-S2 Non-regular rejection.** FIFOs, sockets, block/char devices, and directories MUST be skipped before any read attempt. Skip is logged at `warn` with path + file_type.
3. **AC-S3 No-follow open.** Files that pass the type filter MUST be opened with `O_NOFOLLOW | O_NONBLOCK | O_RDONLY` on Unix. A TOCTOU race where the file is swapped to a symlink between stat and open MUST cause `ELOOP` and a clean skip (not a panic, not an unwrap).
4. **AC-S4 Size cap.** Reads MUST be bounded to `MAX_INBOX_FILE_BYTES = 1 MiB`. A file at exactly the cap is accepted; a file at cap+1 byte is rejected with a `warn` log, archived to `processed/` (or a new `rejected/` subdir — see §5), and counted as 0 ingested.
5. **AC-S5 Defensive `take`.** Even after stat says size <= cap, the read MUST use `Read::take(MAX + 1)` so that a file growing between stat and read still cannot exceed the cap.

### Behavioral preservation (must)
6. **AC-B1 Happy path unchanged.** A well-formed `<name>.jsonl` of <= 1 MiB containing valid `ExternalFeedbackRecord` lines is still: claimed via atomic rename to `processing/<name>.jsonl`, parsed, each record passed through `record_external` (trust-boundary preserved), archived to `processed/<name>.jsonl`. Return value `(claimed, ingested)` matches pre-hardening behavior.
7. **AC-B2 Empty inbox.** Missing or empty `~/.quorum/inbox/` returns `Ok((0, 0))`.
8. **AC-B3 Partial parse.** A file with mixed valid + malformed lines still ingests valid lines and archives (existing behavior — do not regress).
9. **AC-B4 Concurrent drain.** Two simultaneous `drain_inbox` calls do not double-ingest any record (claim-then-ingest atomicity preserved).
10. **AC-B5 Non-`.jsonl` files.** Files without `.jsonl` extension (e.g. `README.md` someone dropped in) MUST be ignored (existing behavior).

### Cross-platform (must)
11. **AC-X1 Unix gating.** `O_NOFOLLOW`/`O_NONBLOCK` open path is `#[cfg(unix)]`. macOS and Linux MUST behave identically for symlink rejection, FIFO rejection, and size cap. Windows fallback (best effort) uses `symlink_metadata` + size cap; FIFO test is skipped.
12. **AC-X2 No new dependencies.** Use `std::os::unix::fs::{OpenOptionsExt, FileTypeExt}` and `nix`-free `libc::{O_NOFOLLOW, O_NONBLOCK}` flags as in `src/ast_grep.rs` precedent.

## 2. Test Inventory

All tests live in `src/feedback.rs` `#[cfg(test)] mod tests` near existing `drain_inbox_*` tests (~L1190+). Naming follows `drain_inbox_<scenario>_<expected>` convention.

| # | Name | Intent | Setup (1 line) |
|---|------|--------|----------------|
| 1 | `drain_inbox_skips_symlink_to_outside_file` | AC-S1: symlink to `/etc/passwd` (or tempfile outside inbox) is filtered | `symlink(outside_file, inbox.join("evil.jsonl"))` then drain |
| 2 | `drain_inbox_skips_symlink_to_inbox_sibling` | AC-S1: symlink within inbox also rejected (no special case) | `symlink(inbox.join("real.jsonl"), inbox.join("link.jsonl"))` |
| 3 | `drain_inbox_skips_dangling_symlink` | AC-S1: broken symlink does not panic | `symlink("/nonexistent", inbox.join("dead.jsonl"))` |
| 4 | `drain_inbox_skips_fifo` (#[cfg(unix)]) | AC-S2: FIFO does not hang daemon | `nix::unistd::mkfifo` or `libc::mkfifo` at `inbox/pipe.jsonl` |
| 5 | `drain_inbox_skips_unix_socket` (#[cfg(unix)]) | AC-S2: AF_UNIX socket rejected | `UnixListener::bind(inbox.join("sock.jsonl"))` |
| 6 | `drain_inbox_skips_directory_named_jsonl` | AC-S2: `inbox/foo.jsonl/` (a dir) rejected | `fs::create_dir(inbox.join("dir.jsonl"))` |
| 7 | `drain_inbox_rejects_oversized_file` | AC-S4: 1 MiB + 1 byte is rejected | write `vec![b'x'; MAX + 1]` |
| 8 | `drain_inbox_accepts_file_at_exact_cap` | AC-S4 boundary: exactly MAX bytes accepted | write a payload that totals exactly `MAX` bytes of valid JSONL+padding-line |
| 9 | `drain_inbox_take_guards_against_growing_file` | AC-S5: stat-says-small, read-finds-large still bounded | mock or test-only hook; if hard, fall back to documenting via code review |
| 10 | `drain_inbox_nofollow_open_handles_toctou_swap` (#[cfg(unix)]) | AC-S3: regular file replaced by symlink between stat & open | rename real file out, symlink in, then drain — expect skip not error |
| 11 | `drain_inbox_valid_file_still_works` | AC-B1 regression guard | one valid 2-line jsonl, drain twice, second call returns (0,0) |
| 12 | `drain_inbox_partial_parse_preserved` | AC-B3 regression | mix one valid + one `not json` line, expect 1 ingested |
| 13 | `drain_inbox_concurrent_calls_no_double_ingest` | AC-B4 atomicity | spawn 2 threads draining same inbox, sum of ingested == record count |
| 14 | `drain_inbox_ignores_non_jsonl_extension` | AC-B5 | drop `README.md` and `evil.txt` in inbox |
| 15 | `drain_inbox_oversized_file_logged_and_quarantined` | AC-S4 disposition: confirm file does not loop forever in `inbox/` | assert post-drain that oversized file is no longer in `inbox/*.jsonl` glob |
| 16 | `drain_inbox_symlink_not_renamed_into_processing` | AC-S1 ops detail | after drain, `processing/` is empty, original symlink still in inbox (or removed — pin the choice) |

## 3. Test Data Builders / Helpers Needed

Add to `#[cfg(test)] mod tests` (or a new `mod test_helpers`):

```rust
/// One valid ExternalFeedbackRecord JSONL line (TP verdict, agent="pal").
fn valid_external_jsonl_line() -> String { ... }

/// Build a temp inbox dir + return (root_dir, inbox_path).
fn inbox_fixture() -> (TempDir, PathBuf) { ... }

/// Write `n` valid JSONL records to `path`.
fn write_valid_jsonl(path: &Path, n: usize) { ... }

#[cfg(unix)]
fn make_fifo(path: &Path) {
    use std::ffi::CString;
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
}

#[cfg(unix)]
fn make_unix_socket(path: &Path) -> UnixListener {
    UnixListener::bind(path).expect("bind unix socket fixture")
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

const MAX_INBOX_FILE_BYTES: u64 = 1024 * 1024;
```

Reuse existing `tempfile::TempDir` pattern from current `drain_inbox_*` tests; do not introduce new test crates.

## 4. Edge Cases Worth Testing (Beyond Plan Doc)

- **EC-1 Zero-byte file.** Empty `.jsonl` should drain cleanly (claim, archive, 0 ingested) — guards against `read_to_string` returning empty being misclassified as error.
- **EC-2 BOM / non-UTF8.** Invalid UTF-8 bytes in an otherwise small file — current behavior is parse-fail, archive; verify still holds and does not propagate `Utf8Error` as drain failure.
- **EC-3 Symlink to oversized file.** Even if size cap would catch it, AC-S1 should reject *before* size check. Test ordering: symlink filter wins.
- **EC-4 Symlink to FIFO.** Compound trap — symlink filter must catch, otherwise opening would block.
- **EC-5 `inbox/` itself a symlink.** Out of scope per §5 below — note explicitly so reviewers don't expect it.
- **EC-6 Read-only filesystem under inbox.** Atomic rename to `processing/` fails — should be `warn` + continue, not abort the whole drain. (Existing behavior — regression guard only.)
- **EC-7 File with NUL bytes mid-record.** JSON parsers handle this; ensure no `CString::new` style helper added in hardening introduces a new failure mode.
- **EC-8 Hard link into inbox.** Hardlinks pass `is_file()` and are not symlinks — these are accepted by design. Document so a future reader doesn't add unnecessary inode-comparison logic.
- **EC-9 File mode 000 (unreadable).** Open fails with `EACCES` — should skip + warn, not abort.

## 5. What NOT to Test (Out of Scope)

- **OS-1** `inbox/` directory itself being a symlink. The hardening is per-file; directory-level lockdown is a separate concern (filed for follow-up).
- **OS-2** Disk-full on archive rename. Generic IO error path, covered by existing error-tolerant drain loop.
- **OS-3** Windows symlink semantics. Windows is best-effort (`symlink_metadata` + size cap); we do not test `O_NOFOLLOW` because it does not exist on Windows. CI runs Unix only for these tests.
- **OS-4** Adversarial inputs to `record_external` itself (trust boundary tested in feedback ingestion tests already).
- **OS-5** Performance benchmarks of cap enforcement. The `take(MAX+1)` path is O(MAX) worst-case; that is the acceptable bound, no perf regression test needed.
- **OS-6** Fuzzing of JSONL parser. Already covered indirectly by existing `drain_inbox_partial_parse` tests; not part of this hardening.
- **OS-7** Behavior when `processed/` archive collides (same filename re-dropped). Existing behavior; out of scope unless hardening changes it.

## 6. Suggested Disposition Decision (needs author confirmation)

Oversized + symlink files: should they be (a) left in `inbox/` (re-warn each drain), (b) moved to `inbox/rejected/`, or (c) deleted? Recommendation: **(b) `inbox/rejected/`** — auditable, idempotent, doesn't re-spam logs. Pin this in the plan doc before writing tests #15/#16, since assertion targets depend on it.
