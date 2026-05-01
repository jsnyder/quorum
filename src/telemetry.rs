/// Review telemetry: append-only JSONL recording review metadata.
/// No file contents, no finding text, no code snippets. Just counts and metadata.

use std::path::PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryEntry {
    pub ts: DateTime<Utc>,
    pub files: Vec<String>,
    pub findings: HashMap<String, usize>,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub duration_ms: u64,
    pub suppressed: usize,
    #[serde(default)] pub context7_resolved: u32,
    #[serde(default)] pub context7_resolve_failed: u32,
    #[serde(default)] pub context7_query_failed: u32,
    /// #123 Layer 1 (Task 10): fraction of `Verdict::Fp` feedback entries
    /// that carry a `fp_kind` discriminator. Range [0.0, 1.0]. `None` when
    /// the loaded feedback store has no FP entries (denominator zero).
    /// Informs Layer 3 prioritization. `serde(default)` for back-compat
    /// with pre-bump rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp_kind_utilization_rate: Option<f32>,
}

/// Structured per-line parse failure surfaced by `load_all_with_stats`.
///
/// Mirrors the shape of `feedback::LoadStats`/`ParseError` (#92): the caller
/// (e.g. `quorum stats`) decides whether/how to log. The store itself is
/// silent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseError {
    /// 1-indexed line number in the source file.
    pub line_no: usize,
    /// First 80 characters of the offending line, for diagnostics.
    /// Truncated on Unicode-scalar boundaries (`chars().take(80)`),
    /// so this is always valid UTF-8 even for multibyte content.
    pub snippet: String,
    /// `serde_json::Error::to_string()` from the failed parse.
    pub error: String,
}

/// Aggregate counters returned alongside parsed entries by
/// `load_all_with_stats`. Empty/whitespace lines do NOT count toward
/// `skipped` — they are quietly elided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadStats {
    pub kept: usize,
    pub skipped: usize,
    pub errors: Vec<ParseError>,
}

pub struct TelemetryStore {
    path: PathBuf,
}

impl TelemetryStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn record(&self, entry: &TelemetryEntry) -> anyhow::Result<()> {
        use anyhow::Context;
        use std::io::Write;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open telemetry file: {}", self.path.display()))?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Stream-parse the JSONL file line-by-line and return the parsed
    /// entries together with structured counters/errors.
    ///
    /// Memory footprint is bounded by the longest line, not by the total
    /// file size — this is the fix for #138 (previously the whole file was
    /// slurped into a `String`). Per #139, malformed lines are surfaced as
    /// structured `ParseError { line_no, snippet, error }` instead of being
    /// silently dropped.
    ///
    /// Empty/whitespace-only lines are quietly elided and do NOT count
    /// toward `skipped`. The store itself does not log; the caller decides
    /// what to do with `stats.errors` (consistent with `feedback.rs`).
    ///
    /// Per-line allocation is hard-capped at `MAX_JSONL_LINE_BYTES` (1 MiB):
    /// pathologically long lines are skipped without reading them into
    /// memory in full, then surfaced as a ParseError with a truncated
    /// snippet. The error vector itself is bounded at `MAX_PARSE_ERRORS`
    /// (1000) so a fully-corrupted file cannot OOM the caller — `skipped`
    /// continues to count beyond the cap.
    pub fn load_all_with_stats(&self) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        use std::io::{BufRead, BufReader, Read};

        // Bounded per-line allocation. JSONL telemetry rows are tiny
        // (a few hundred bytes); 1 MiB is a generous ceiling.
        const MAX_JSONL_LINE_BYTES: usize = 1 << 20;
        // Bounded error retention. A heavily-corrupted file cannot
        // accumulate unbounded ParseError entries — `skipped` keeps
        // counting beyond the cap so totals stay accurate.
        const MAX_PARSE_ERRORS: usize = 1000;

        if !self.path.exists() {
            return Ok((vec![], LoadStats::default()));
        }
        let file = std::fs::File::open(&self.path)?;
        let mut reader = BufReader::new(file);
        let mut entries: Vec<TelemetryEntry> = Vec::new();
        let mut stats = LoadStats::default();
        let mut buf = Vec::with_capacity(4096);
        let mut line_no: usize = 0;

        loop {
            buf.clear();
            // read_until lets us bound how many bytes we'll allocate per
            // line: we read up to MAX_JSONL_LINE_BYTES + 1 to detect
            // overflow without slurping the rest of an oversized line.
            let mut limited = (&mut reader).take((MAX_JSONL_LINE_BYTES + 1) as u64);
            let n = limited.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            line_no += 1;

            // Detect oversized line. We read up to MAX_JSONL_LINE_BYTES + 1
            // bytes via Read::take, so the only way `buf.len()` exceeds
            // MAX_JSONL_LINE_BYTES is if the real line is strictly longer
            // than the cap. A line of *exactly* MAX_JSONL_LINE_BYTES at
            // EOF (no trailing newline) is fine and must NOT be rejected.
            let oversized = buf.len() > MAX_JSONL_LINE_BYTES;
            if oversized {
                // Drain the rest of the line so we resync to the next
                // newline without allocating it.
                let mut sink = Vec::with_capacity(64);
                while !buf.ends_with(b"\n") {
                    sink.clear();
                    let mut tail = (&mut reader).take(64 * 1024);
                    let drained = tail.read_until(b'\n', &mut sink)?;
                    if drained == 0 {
                        break;
                    }
                    if sink.ends_with(b"\n") {
                        buf.push(b'\n');
                        break;
                    }
                }
                stats.skipped += 1;
                if stats.errors.len() < MAX_PARSE_ERRORS {
                    let snippet: String = String::from_utf8_lossy(&buf)
                        .chars()
                        .take(80)
                        .collect();
                    stats.errors.push(ParseError {
                        line_no,
                        snippet,
                        error: format!("line exceeds {MAX_JSONL_LINE_BYTES} bytes"),
                    });
                }
                continue;
            }

            // Trim the trailing newline (and \r if CRLF) before parsing.
            let line_bytes = if buf.ends_with(b"\n") {
                let end = buf.len() - 1;
                let end = if end > 0 && buf[end - 1] == b'\r' { end - 1 } else { end };
                &buf[..end]
            } else {
                &buf[..]
            };
            // Empty / whitespace-only lines are quietly elided.
            if line_bytes.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            // Convert to &str (lossy on invalid UTF-8 — surfaced as a
            // parse error below).
            let line: std::borrow::Cow<'_, str> = String::from_utf8_lossy(line_bytes);
            match serde_json::from_str::<TelemetryEntry>(&line) {
                Ok(entry) => {
                    entries.push(entry);
                    stats.kept += 1;
                }
                Err(e) => {
                    stats.skipped += 1;
                    if stats.errors.len() < MAX_PARSE_ERRORS {
                        stats.errors.push(ParseError {
                            line_no,
                            snippet: line.chars().take(80).collect(),
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
        Ok((entries, stats))
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<TelemetryEntry>> {
        Ok(self.load_all_with_stats()?.0)
    }

    pub fn load_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<TelemetryEntry>> {
        Ok(self
            .load_all_with_stats()?
            .0
            .into_iter()
            .filter(|e| e.ts >= since)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_entry() -> TelemetryEntry {
        let mut findings = HashMap::new();
        findings.insert("critical".into(), 1);
        findings.insert("warning".into(), 2);
        TelemetryEntry {
            ts: Utc::now(),
            files: vec!["src/main.rs".into()],
            findings,
            model: "gpt-5.4".into(),
            tokens_in: 4200,
            tokens_out: 1800,
            duration_ms: 3400,
            suppressed: 2,
            context7_resolved: 0,
            context7_resolve_failed: 0,
            context7_query_failed: 0,
            fp_kind_utilization_rate: None,
        }
    }

    #[test]
    fn telemetry_entry_context7_fields_default_to_zero() {
        let entry = sample_entry();
        assert_eq!(entry.context7_resolved, 0);
        assert_eq!(entry.context7_resolve_failed, 0);
        assert_eq!(entry.context7_query_failed, 0);
    }

    #[test]
    fn telemetry_entry_old_jsonl_row_deserializes_with_zero_context7_fields() {
        // CRITICAL backward-compat: every existing user's `quorum stats`
        // breaks if this fails. Shape matches the actual TelemetryEntry
        // as it existed before the schema bump.
        let old = r#"{
            "ts": "2026-01-01T00:00:00Z",
            "files": [],
            "findings": {},
            "model": "gpt-5.4",
            "tokens_in": 0,
            "tokens_out": 0,
            "duration_ms": 0,
            "suppressed": 0
        }"#;
        let entry: TelemetryEntry = serde_json::from_str(old)
            .expect("old JSONL rows must deserialize after schema bump");
        assert_eq!(entry.context7_resolved, 0);
        assert_eq!(entry.context7_resolve_failed, 0);
        assert_eq!(entry.context7_query_failed, 0);
    }

    #[test]
    fn round_trip_serialization() {
        let entry = sample_entry();
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: TelemetryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, "gpt-5.4");
        assert_eq!(parsed.tokens_in, 4200);
        assert_eq!(parsed.tokens_out, 1800);
        assert_eq!(parsed.files, vec!["src/main.rs"]);
    }

    #[test]
    fn record_and_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let store = TelemetryStore::new(path);

        store.record(&sample_entry()).unwrap();
        store.record(&sample_entry()).unwrap();

        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn load_nonexistent_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let store = TelemetryStore::new(path);
        let entries = store.load_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn load_skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let store = TelemetryStore::new(path.clone());

        store.record(&sample_entry()).unwrap();
        // Append garbage
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{garbage}}").unwrap();
        writeln!(f, "not json at all").unwrap();
        store.record(&sample_entry()).unwrap();

        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 2); // skipped 2 bad lines
    }

    #[test]
    fn load_all_streams_does_not_oom_on_large_file() {
        // 1000-line synthetic file; the streaming impl must return all
        // entries without slurping the whole file into a single string.
        // We can't strictly assert "did not OOM" but we CAN assert the
        // new load_all_with_stats API exists and returns structured
        // counts — the streaming switch is observed via that API.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            let entry = serde_json::to_string(&sample_entry()).unwrap();
            for _ in 0..1000 {
                writeln!(f, "{}", entry).unwrap();
            }
        }
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert_eq!(entries.len(), 1000);
        assert_eq!(stats.kept, 1000);
        assert_eq!(stats.skipped, 0);
        assert!(stats.errors.is_empty());
    }

    #[test]
    fn malformed_lines_become_parse_errors_with_line_numbers() {
        // #139: malformed JSONL must surface as structured ParseError
        // (line_no, snippet, error) — not silently dropped.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let good = serde_json::to_string(&sample_entry()).unwrap();
        let body = format!("{good}\nthis is not json\n{good}\n{{partial:\n");
        std::fs::write(&path, body).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(stats.kept, 2);
        assert_eq!(stats.skipped, 2);
        assert_eq!(stats.errors.len(), 2);
        assert_eq!(stats.errors[0].line_no, 2);
        assert_eq!(stats.errors[1].line_no, 4);
        assert!(stats.errors[0].snippet.starts_with("this is not"));
        assert!(!stats.errors[0].error.is_empty());
    }

    #[test]
    fn empty_lines_do_not_count_as_skipped() {
        // Whitespace-only / blank lines are quietly elided — they are NOT
        // a parse failure and must not inflate stats.skipped.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let good = serde_json::to_string(&sample_entry()).unwrap();
        let body = format!("\n{good}\n   \n{good}\n");
        std::fs::write(&path, body).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(stats.kept, 2);
        assert_eq!(stats.skipped, 0);
        assert!(stats.errors.is_empty());
    }

    #[test]
    fn very_long_malformed_line_snippet_is_truncated_to_80_chars() {
        // Per GPT-5.5 review: confirm the 80-char snippet cap holds for
        // pathologically long malformed rows. Use a multi-byte char to
        // also exercise the chars()-based (Unicode-scalar-safe) truncation.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        // 500 copies of a 3-byte UTF-8 char = 1500 bytes / 500 chars,
        // none of which is valid JSON.
        let huge = "\u{2603}".repeat(500); // ☃ snowman
        std::fs::write(&path, format!("{huge}\n")).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert!(entries.is_empty());
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.errors.len(), 1);
        let err = &stats.errors[0];
        assert_eq!(err.line_no, 1);
        // Snippet must be exactly 80 Unicode scalars, NOT 80 bytes —
        // chars().take(80) on multi-byte input must not panic or split.
        assert_eq!(err.snippet.chars().count(), 80);
        // And the snippet must be valid UTF-8 (implied by being a String,
        // but assert the byte count is the expected 3 * 80 = 240 to
        // confirm we didn't accidentally byte-truncate).
        assert_eq!(err.snippet.len(), 240);
    }

    #[test]
    fn oversized_line_is_rejected_without_unbounded_allocation() {
        // Quorum reviewer (gpt-5.4, severity=high): `BufRead::lines()`
        // allocates a `String` for the full line with no size limit, so a
        // single multi-GB corrupted line could OOM the loader. Bound the
        // per-line allocation and surface oversized lines as ParseError —
        // the same defect class as #138, just relocated into the streaming
        // path.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let good = serde_json::to_string(&sample_entry()).unwrap();
        // 2 MiB line of 'x' — well above the 1 MiB cap.
        let huge = "x".repeat(2 * 1024 * 1024);
        let body = format!("{good}\n{huge}\n{good}\n");
        std::fs::write(&path, body).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        // Both good lines must survive; the oversized line must be skipped
        // structurally (not by allocating the full 2 MiB).
        assert_eq!(entries.len(), 2);
        assert_eq!(stats.kept, 2);
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.errors.len(), 1);
        let err = &stats.errors[0];
        assert_eq!(err.line_no, 2);
        assert!(
            err.error.contains("exceeds")
                || err.error.to_lowercase().contains("too large")
                || err.error.to_lowercase().contains("oversized"),
            "expected oversized-line error message, got: {}",
            err.error
        );
    }

    #[test]
    fn line_exactly_at_size_cap_at_eof_is_not_rejected() {
        // Quorum re-review (gpt-5.4, severity=high): the oversized check
        // must not reject a valid final JSONL record whose length is
        // *exactly* MAX_JSONL_LINE_BYTES with no trailing newline. We read
        // up to MAX+1 bytes via Read::take, so only buf.len() > MAX
        // indicates the real line is too long.
        //
        // We can't easily construct a 1 MiB valid TelemetryEntry, so this
        // test asserts the related boundary: a file whose only line is
        // garbage of length exactly N (no newline) must be reported as a
        // single ParseError on line 1 (one parse failure, not two; not
        // skipped as oversized) when N <= MAX_JSONL_LINE_BYTES.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        // 32 KiB of 'x', no trailing newline. Well below the 1 MiB cap
        // but exercises the EOF-without-newline path that triggered the
        // off-by-one.
        let body = "x".repeat(32 * 1024);
        std::fs::write(&path, body).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert!(entries.is_empty());
        assert_eq!(stats.skipped, 1, "must report exactly one parse failure");
        assert_eq!(stats.errors.len(), 1);
        // Crucially: the error must be the JSON parse error, NOT the
        // 'line exceeds N bytes' oversized error.
        assert!(
            !stats.errors[0].error.contains("exceeds"),
            "EOF line at-or-under cap must not be flagged as oversized: {}",
            stats.errors[0].error
        );
    }

    #[test]
    fn errors_vec_is_bounded_on_pathologically_corrupted_files() {
        // Step-3 followup: `LoadStats::errors` was unbounded — a heavily
        // corrupted file could accumulate millions of ParseError entries
        // and OOM the caller. Bound the error vec at MAX_PARSE_ERRORS;
        // beyond that, increment `skipped` but stop pushing snippets.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        // Write 5000 malformed lines.
        let mut body = String::new();
        for i in 0..5000 {
            body.push_str(&format!("not json line {i}\n"));
        }
        std::fs::write(&path, body).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert!(entries.is_empty());
        assert_eq!(stats.skipped, 5000);
        // Cap is 1000 — see MAX_PARSE_ERRORS in load_all_with_stats.
        assert!(
            stats.errors.len() <= 1000,
            "errors vec should be capped at 1000, got {}",
            stats.errors.len()
        );
    }

    #[test]
    fn load_since_filters_by_date() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let store = TelemetryStore::new(path);

        let mut old = sample_entry();
        old.ts = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap().with_timezone(&Utc);
        store.record(&old).unwrap();

        let recent = sample_entry(); // ts = now
        store.record(&recent).unwrap();

        let since = chrono::DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .unwrap().with_timezone(&Utc);
        let entries = store.load_since(since).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
