//! Stdio shim for command handlers.
//!
//! `write_cmd_output` is the centralized exit-code rule for `quorum context …`
//! (and any future subcommand that returns a `CmdOutput`). It exists so the
//! pipe-handling logic is testable without touching real stdio.
//!
//! Issue #84: previous inline implementation in `run_context` swallowed every
//! `write_all`/`flush` error with `let _ = …`. That was correct for `BrokenPipe`
//! (the user piped to `head` and closed the reader early — exit 0 is fine) but
//! silently hid `EIO` / `ENOSPC`, so a redirect to a failing filesystem looked
//! successful. We now distinguish the two.

use std::io::{self, Write};

use crate::context::cli::CmdOutput;

/// Write a command's rendered stdout + warnings, returning the process exit
/// code.
///
/// Contract:
/// - `BrokenPipe` on `out` (write or flush) → exit 0, no diagnostic. The
///   downstream consumer chose to stop reading; that is not a failure.
/// - Any other I/O error on `out` → write `error: failed to write output: {e}`
///   to `err` and return 1. This surfaces `EIO`/`ENOSPC`/etc.
/// - On success, return 1 if `cmd.doctor_failed == Some(true)`, else 0.
///
/// Errors writing to `err` (the diagnostic channel) are themselves swallowed
/// — there is no useful recovery if both stdout and stderr are broken.
pub fn write_cmd_output<W: Write, E: Write>(
    out: &mut W,
    err: &mut E,
    cmd: &CmdOutput,
) -> i32 {
    if !cmd.stdout.is_empty() {
        if let Err(e) = out.write_all(cmd.stdout.as_bytes()) {
            return classify(&e, err);
        }
        if !cmd.stdout.ends_with('\n')
            && let Err(e) = out.write_all(b"\n")
        {
            return classify(&e, err);
        }
    }
    if let Err(e) = out.flush() {
        return classify(&e, err);
    }
    for w in &cmd.warnings {
        // err is the diagnostic channel; if it itself fails there's nothing
        // useful to do.
        let _ = writeln!(err, "{}", w);
    }
    if cmd.doctor_failed.unwrap_or(false) {
        1
    } else {
        0
    }
}

/// Map an I/O error from the stdout channel into an exit code, emitting a
/// diagnostic to `err` for non-`BrokenPipe` cases.
fn classify<E: Write>(e: &io::Error, err: &mut E) -> i32 {
    if e.kind() == io::ErrorKind::BrokenPipe {
        0
    } else {
        let _ = writeln!(err, "error: failed to write output: {}", e);
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// Test double: returns the configured `ErrorKind` from every `write` and
    /// reports `flush` as success. Models EIO/ENOSPC/BrokenPipe on the write
    /// path.
    struct WriteFailingWriter {
        kind: io::ErrorKind,
    }
    impl Write for WriteFailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.kind, "simulated write failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Test double: writes succeed; flush returns the configured `ErrorKind`.
    /// Models `tee`-style pipelines that fail only on flush.
    struct FlushFailingWriter {
        kind: io::ErrorKind,
    }
    impl Write for FlushFailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(self.kind, "simulated flush failure"))
        }
    }

    fn cmd_with_stdout(s: &str) -> CmdOutput {
        CmdOutput {
            stdout: s.into(),
            ..Default::default()
        }
    }

    #[test]
    fn broken_pipe_on_write_returns_zero_and_emits_no_error() {
        let mut out = WriteFailingWriter {
            kind: io::ErrorKind::BrokenPipe,
        };
        let mut err: Vec<u8> = Vec::new();
        let cmd = cmd_with_stdout("hello");
        let code = write_cmd_output(&mut out, &mut err, &cmd);
        assert_eq!(code, 0, "BrokenPipe must yield exit 0");
        assert!(
            err.is_empty(),
            "BrokenPipe must not emit a diagnostic; got: {:?}",
            String::from_utf8_lossy(&err)
        );
    }

    #[test]
    fn non_broken_pipe_write_error_returns_one_and_prints_to_stderr() {
        // Issue #84: this is the bug being fixed. Pre-fix, the Ok arm of
        // run_context silently dropped EIO and exited 0. Now we expect 1.
        let mut out = WriteFailingWriter {
            kind: io::ErrorKind::Other,
        };
        let mut err: Vec<u8> = Vec::new();
        let cmd = cmd_with_stdout("hello");
        let code = write_cmd_output(&mut out, &mut err, &cmd);
        assert_eq!(code, 1, "non-BrokenPipe I/O error must yield exit 1");
        let s = String::from_utf8_lossy(&err);
        assert!(
            s.contains("failed to write"),
            "stderr must explain the failure to the user; got: {s}"
        );
    }

    #[test]
    fn flush_error_non_broken_pipe_returns_one() {
        // Mirror of the write-path EIO test for flush. Pre-fix, flush errors
        // were silently dropped via `let _ = handle.flush();`. Now we expect
        // the same surface as a write failure.
        let mut out = FlushFailingWriter {
            kind: io::ErrorKind::Other,
        };
        let mut err: Vec<u8> = Vec::new();
        let cmd = cmd_with_stdout("hello");
        let code = write_cmd_output(&mut out, &mut err, &cmd);
        assert_eq!(code, 1, "flush EIO must yield exit 1");
        let s = String::from_utf8_lossy(&err);
        assert!(
            s.contains("failed to write"),
            "flush failure stderr must explain the failure; got: {s}"
        );
    }

    #[test]
    fn flush_error_broken_pipe_returns_zero() {
        let mut out = FlushFailingWriter {
            kind: io::ErrorKind::BrokenPipe,
        };
        let mut err: Vec<u8> = Vec::new();
        let cmd = cmd_with_stdout("hello");
        let code = write_cmd_output(&mut out, &mut err, &cmd);
        assert_eq!(code, 0, "BrokenPipe on flush must still be silent");
        assert!(err.is_empty(), "BrokenPipe must not emit a diagnostic");
    }

    #[test]
    fn successful_write_with_doctor_failed_true_returns_one() {
        // Pins the exit-code interaction with issue #73 (typed doctor result).
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let cmd = CmdOutput {
            stdout: "ok\n".into(),
            doctor_failed: Some(true),
            ..Default::default()
        };
        assert_eq!(write_cmd_output(&mut out, &mut err, &cmd), 1);
        assert!(err.is_empty(), "doctor-failed alone must not emit stderr");
    }

    #[test]
    fn successful_write_with_no_doctor_status_returns_zero() {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let cmd = CmdOutput {
            stdout: "ok\n".into(),
            doctor_failed: None,
            ..Default::default()
        };
        assert_eq!(write_cmd_output(&mut out, &mut err, &cmd), 0);
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "ok\n",
            "rendered stdout must be written verbatim"
        );
        assert!(err.is_empty());
    }

    #[test]
    fn appends_trailing_newline_when_stdout_lacks_one() {
        // run_context's prior contract: write a trailing \n if non-empty stdout
        // does not end with one. Pin this so the helper extraction doesn't drop
        // it.
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let cmd = cmd_with_stdout("no-newline");
        write_cmd_output(&mut out, &mut err, &cmd);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.ends_with('\n'),
            "trailing newline must be appended; got: {s:?}"
        );
        assert_eq!(s, "no-newline\n");
    }

    #[test]
    fn does_not_append_extra_newline_when_present() {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let cmd = cmd_with_stdout("has-newline\n");
        write_cmd_output(&mut out, &mut err, &cmd);
        assert_eq!(std::str::from_utf8(&out).unwrap(), "has-newline\n");
    }

    #[test]
    fn warnings_are_written_to_err_one_per_line() {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let cmd = CmdOutput {
            stdout: "ok\n".into(),
            warnings: vec!["first".into(), "second".into()],
            ..Default::default()
        };
        write_cmd_output(&mut out, &mut err, &cmd);
        let s = std::str::from_utf8(&err).unwrap();
        assert_eq!(
            s, "first\nsecond\n",
            "warnings must be one-per-line on stderr; got: {s:?}"
        );
    }

    #[test]
    fn empty_stdout_writes_nothing_no_trailing_newline() {
        // Match prior contract: `if !out.stdout.is_empty() && !ends_with('\n')`.
        // An empty stdout produces no output at all.
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let cmd = cmd_with_stdout("");
        write_cmd_output(&mut out, &mut err, &cmd);
        assert!(out.is_empty(), "empty stdout must produce no bytes");
    }
}
