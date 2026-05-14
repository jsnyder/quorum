use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
/// Review telemetry: append-only JSONL recording review metadata.
/// No file contents, no finding text, no code snippets. Just counts and metadata.
use std::path::{Path, PathBuf};

use crate::storage::StorageHandle;

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
    #[serde(default)]
    pub context7_resolved: u32,
    #[serde(default)]
    pub context7_resolve_failed: u32,
    #[serde(default)]
    pub context7_query_failed: u32,
    #[serde(default)]
    pub context7_skipped_popular: u32,
    #[serde(default)]
    pub context7_budget_reduced: u32,
    /// #123 Layer 1 (Task 10): fraction of `Verdict::Fp` feedback entries
    /// that carry a `fp_kind` discriminator. Range [0.0, 1.0]. `None` when
    /// the loaded feedback store has no FP entries (denominator zero).
    /// Informs Layer 3 prioritization. `serde(default)` for back-compat
    /// with pre-bump rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp_kind_utilization_rate: Option<f32>,
    #[serde(default)]
    pub judge_calls: u32,
    #[serde(default)]
    pub judge_approved: u32,
    #[serde(default)]
    pub judge_rejected: u32,
    #[serde(default)]
    pub judge_uncertain: u32,
    #[serde(default)]
    pub judge_skipped: u32,
    #[serde(default)]
    pub judge_cache_hits: u32,
    #[serde(default)]
    pub judge_latency_ms: u64,
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

/// Internal backend discriminator: JSONL (legacy) or SQLite.
enum Backend {
    Jsonl(PathBuf),
    Sqlite(StorageHandle),
}

/// Intermediate row extracted from SQLite. All columns are pulled inside the
/// `query_map` closure (where the `Row` borrow lives), then converted to
/// `TelemetryEntry` after the statement is dropped.
struct RawTelemetryRow {
    ts_str: String,
    files_json: String,
    findings_json: String,
    model: String,
    tokens_in: i64,
    tokens_out: i64,
    duration_ms: i64,
    suppressed: i64,
    context7_resolved: i64,
    context7_resolve_failed: i64,
    context7_query_failed: i64,
    context7_skipped_popular: i64,
    context7_budget_reduced: i64,
    fp_kind_utilization_rate: Option<f64>,
    judge_calls: i64,
    judge_approved: i64,
    judge_rejected: i64,
    judge_uncertain: i64,
    judge_skipped: i64,
    judge_cache_hits: i64,
    judge_latency_ms: i64,
}

impl RawTelemetryRow {
    /// Convert a raw SQLite row into a `TelemetryEntry`.
    fn into_entry(self) -> anyhow::Result<TelemetryEntry> {
        use anyhow::Context;

        let ts = DateTime::parse_from_rfc3339(&self.ts_str)
            .with_context(|| format!("invalid timestamp in telemetry row: {}", self.ts_str))?
            .with_timezone(&Utc);

        let files: Vec<String> = serde_json::from_str(&self.files_json)
            .with_context(|| {
                format!(
                    "invalid files JSON in telemetry row: {}",
                    self.files_json
                )
            })?;

        let findings: HashMap<String, usize> = serde_json::from_str(&self.findings_json)
            .with_context(|| {
                format!(
                    "invalid findings JSON in telemetry row: {}",
                    self.findings_json
                )
            })?;

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let entry = TelemetryEntry {
            ts,
            files,
            findings,
            model: self.model,
            tokens_in: self.tokens_in as u64,
            tokens_out: self.tokens_out as u64,
            duration_ms: self.duration_ms as u64,
            suppressed: self.suppressed as usize,
            context7_resolved: self.context7_resolved as u32,
            context7_resolve_failed: self.context7_resolve_failed as u32,
            context7_query_failed: self.context7_query_failed as u32,
            context7_skipped_popular: self.context7_skipped_popular as u32,
            context7_budget_reduced: self.context7_budget_reduced as u32,
            fp_kind_utilization_rate: self.fp_kind_utilization_rate.map(|v| v as f32),
            judge_calls: self.judge_calls as u32,
            judge_approved: self.judge_approved as u32,
            judge_rejected: self.judge_rejected as u32,
            judge_uncertain: self.judge_uncertain as u32,
            judge_skipped: self.judge_skipped as u32,
            judge_cache_hits: self.judge_cache_hits as u32,
            judge_latency_ms: self.judge_latency_ms as u64,
        };
        Ok(entry)
    }
}

pub struct TelemetryStore {
    backend: Backend,
}

impl TelemetryStore {
    /// Create a TelemetryStore backed by a JSONL file.
    /// Retained for backward compatibility, migration support, and tests.
    /// New callers should use `with_storage()`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            backend: Backend::Jsonl(path),
        }
    }

    /// Create a SQLite-backed telemetry store.
    pub fn with_storage(handle: StorageHandle) -> Self {
        Self {
            backend: Backend::Sqlite(handle),
        }
    }

    /// Path to the underlying JSONL file. Returns an empty path for
    /// the SQLite backend (only used for display purposes).
    pub fn path(&self) -> &Path {
        match &self.backend {
            Backend::Jsonl(p) => p,
            Backend::Sqlite(_) => Path::new(""),
        }
    }

    pub fn record(&self, entry: &TelemetryEntry) -> anyhow::Result<()> {
        match &self.backend {
            Backend::Jsonl(path) => Self::record_jsonl(path, entry),
            Backend::Sqlite(handle) => Self::record_sqlite(handle, entry),
        }
    }

    pub fn load_all_with_stats(&self) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        match &self.backend {
            Backend::Jsonl(path) => Self::load_all_with_stats_jsonl(path),
            Backend::Sqlite(handle) => Self::load_all_with_stats_sqlite(handle),
        }
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

    // ── JSONL backend ──────────────────────────────────────────────────

    fn record_jsonl(path: &Path, entry: &TelemetryEntry) -> anyhow::Result<()> {
        use anyhow::Context;
        use std::io::Write;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open telemetry file: {}", path.display()))?;
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
    fn load_all_with_stats_jsonl(path: &Path) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        use std::io::{BufRead, BufReader, Read};

        // Bounded per-line allocation. JSONL telemetry rows are tiny
        // (a few hundred bytes); 1 MiB is a generous ceiling.
        const MAX_JSONL_LINE_BYTES: usize = 1 << 20;
        // Bounded error retention. A heavily-corrupted file cannot
        // accumulate unbounded ParseError entries — `skipped` keeps
        // counting beyond the cap so totals stay accurate.
        const MAX_PARSE_ERRORS: usize = 1000;

        if !path.exists() {
            return Ok((vec![], LoadStats::default()));
        }
        let file = std::fs::File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut entries: Vec<TelemetryEntry> = Vec::new();
        let mut stats = LoadStats::default();
        let mut buf = Vec::with_capacity(4096);
        let mut line_no: usize = 0;

        loop {
            buf.clear();
            // read_until lets us bound how many bytes we'll allocate per
            // line: we read up to MAX_JSONL_LINE_BYTES + 2 so that a
            // payload of exactly MAX bytes followed by \r\n is still
            // captured as a non-oversized line, while a payload of
            // MAX+1 (with or without newline) is unambiguously oversized.
            let mut limited = (&mut reader).take((MAX_JSONL_LINE_BYTES + 2) as u64);
            let n = limited.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            line_no += 1;

            // Detect oversized line. The cap applies to the JSONL *payload*
            // (i.e. the bytes excluding the trailing `\n` / `\r\n`), so a
            // record of exactly MAX_JSONL_LINE_BYTES bytes is valid whether
            // or not it carries a trailing newline. Since Read::take is
            // configured for MAX + 1 bytes, we have at most one extra
            // payload byte to disambiguate: payload_len > MAX means the
            // real line truly exceeded the cap.
            let payload_len = if buf.ends_with(b"\n") {
                let n = buf.len() - 1;
                if n > 0 && buf[n - 1] == b'\r' {
                    n - 1
                } else {
                    n
                }
            } else {
                buf.len()
            };
            let oversized = payload_len > MAX_JSONL_LINE_BYTES;
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
                    let snippet: String = String::from_utf8_lossy(&buf).chars().take(80).collect();
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
                let end = if end > 0 && buf[end - 1] == b'\r' {
                    end - 1
                } else {
                    end
                };
                &buf[..end]
            } else {
                &buf[..]
            };
            // Empty / whitespace-only lines are quietly elided.
            if line_bytes.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            // Strict UTF-8 validation. `from_utf8_lossy` would silently
            // replace bad bytes with U+FFFD and could let a corrupted
            // row "successfully" deserialize with mutated string fields.
            // We treat invalid UTF-8 as a parse error and use a lossy
            // snippet only for the diagnostic message.
            let line = match std::str::from_utf8(line_bytes) {
                Ok(s) => s,
                Err(e) => {
                    stats.skipped += 1;
                    if stats.errors.len() < MAX_PARSE_ERRORS {
                        let snippet: String = String::from_utf8_lossy(line_bytes)
                            .chars()
                            .take(80)
                            .collect();
                        stats.errors.push(ParseError {
                            line_no,
                            snippet,
                            error: format!("invalid UTF-8: {e}"),
                        });
                    }
                    continue;
                }
            };
            match serde_json::from_str::<TelemetryEntry>(line) {
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

    // ── SQLite backend ─────────────────────────────────────────────────

    fn record_sqlite(handle: &StorageHandle, entry: &TelemetryEntry) -> anyhow::Result<()> {
        use rusqlite::params;

        let files_json = serde_json::to_string(&entry.files)?;
        let findings_json = serde_json::to_string(&entry.findings)?;
        let ts = entry.ts.to_rfc3339();

        let conn = handle.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        // u64 -> i64 casts are intentional: SQLite INTEGER is signed 64-bit.
        // Token counts and durations are well within i64 range in practice.
        #[allow(clippy::cast_possible_wrap)]
        conn.execute(
            "INSERT INTO telemetry (
                ts, files, findings, model,
                tokens_in, tokens_out, duration_ms, suppressed,
                context7_resolved, context7_resolve_failed, context7_query_failed,
                context7_skipped_popular, context7_budget_reduced,
                fp_kind_utilization_rate,
                judge_calls, judge_approved, judge_rejected,
                judge_uncertain, judge_skipped, judge_cache_hits,
                judge_latency_ms
            ) VALUES (
                ?1, ?2, ?3, ?4,
                ?5, ?6, ?7, ?8,
                ?9, ?10, ?11,
                ?12, ?13,
                ?14,
                ?15, ?16, ?17,
                ?18, ?19, ?20,
                ?21
            )",
            params![
                ts,
                files_json,
                findings_json,
                entry.model,
                entry.tokens_in as i64,
                entry.tokens_out as i64,
                entry.duration_ms as i64,
                entry.suppressed as i64,
                i64::from(entry.context7_resolved),
                i64::from(entry.context7_resolve_failed),
                i64::from(entry.context7_query_failed),
                i64::from(entry.context7_skipped_popular),
                i64::from(entry.context7_budget_reduced),
                entry.fp_kind_utilization_rate.map(f64::from),
                i64::from(entry.judge_calls),
                i64::from(entry.judge_approved),
                i64::from(entry.judge_rejected),
                i64::from(entry.judge_uncertain),
                i64::from(entry.judge_skipped),
                i64::from(entry.judge_cache_hits),
                entry.judge_latency_ms as i64,
            ],
        )?;

        Ok(())
    }

    fn load_all_with_stats_sqlite(
        handle: &StorageHandle,
    ) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        let conn = handle.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        let raw_rows = Self::query_raw_rows(&conn)?;

        let mut entries = Vec::with_capacity(raw_rows.len());
        for r in raw_rows {
            entries.push(r.into_entry()?);
        }

        let stats = LoadStats {
            kept: entries.len(),
            skipped: 0,
            errors: vec![],
        };

        Ok((entries, stats))
    }

    /// Execute the SELECT query and extract all columns into `RawTelemetryRow`
    /// structs, keeping the rusqlite `Row` borrow confined to the closure.
    fn query_raw_rows(conn: &rusqlite::Connection) -> anyhow::Result<Vec<RawTelemetryRow>> {
        let mut stmt = conn.prepare(
            "SELECT
                ts, files, findings, model,
                tokens_in, tokens_out, duration_ms, suppressed,
                context7_resolved, context7_resolve_failed, context7_query_failed,
                context7_skipped_popular, context7_budget_reduced,
                fp_kind_utilization_rate,
                judge_calls, judge_approved, judge_rejected,
                judge_uncertain, judge_skipped, judge_cache_hits,
                judge_latency_ms
            FROM telemetry
            ORDER BY ts ASC",
        )?;

        let raw_rows: Vec<RawTelemetryRow> = stmt
            .query_map([], |row| {
                Ok(RawTelemetryRow {
                    ts_str: row.get(0)?,
                    files_json: row.get(1)?,
                    findings_json: row.get(2)?,
                    model: row.get(3)?,
                    tokens_in: row.get(4)?,
                    tokens_out: row.get(5)?,
                    duration_ms: row.get(6)?,
                    suppressed: row.get(7)?,
                    context7_resolved: row.get(8)?,
                    context7_resolve_failed: row.get(9)?,
                    context7_query_failed: row.get(10)?,
                    context7_skipped_popular: row.get(11)?,
                    context7_budget_reduced: row.get(12)?,
                    fp_kind_utilization_rate: row.get(13)?,
                    judge_calls: row.get(14)?,
                    judge_approved: row.get(15)?,
                    judge_rejected: row.get(16)?,
                    judge_uncertain: row.get(17)?,
                    judge_skipped: row.get(18)?,
                    judge_cache_hits: row.get(19)?,
                    judge_latency_ms: row.get(20)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(raw_rows)
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
            context7_skipped_popular: 0,
            context7_budget_reduced: 0,
            fp_kind_utilization_rate: None,
            judge_calls: 0,
            judge_approved: 0,
            judge_rejected: 0,
            judge_uncertain: 0,
            judge_skipped: 0,
            judge_cache_hits: 0,
            judge_latency_ms: 0,
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
        let entry: TelemetryEntry =
            serde_json::from_str(old).expect("old JSONL rows must deserialize after schema bump");
        assert_eq!(entry.context7_resolved, 0);
        assert_eq!(entry.context7_resolve_failed, 0);
        assert_eq!(entry.context7_query_failed, 0);
        assert_eq!(entry.context7_skipped_popular, 0);
        assert_eq!(entry.context7_budget_reduced, 0);
        assert_eq!(entry.judge_calls, 0);
        assert_eq!(entry.judge_approved, 0);
        assert_eq!(entry.judge_rejected, 0);
        assert_eq!(entry.judge_uncertain, 0);
        assert_eq!(entry.judge_skipped, 0);
        assert_eq!(entry.judge_cache_hits, 0);
        assert_eq!(entry.judge_latency_ms, 0);
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
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
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
    fn newline_terminated_line_at_cap_payload_is_not_oversized() {
        // Quorum re-review #2 (gpt-5.4, severity=high): the cap applies
        // to the JSONL *payload*, not to the raw buffer that includes
        // the trailing newline. A payload of exactly MAX bytes followed
        // by '\n' (so buf.len() = MAX + 1) must NOT be flagged oversized.
        // We exercise the boundary at a smaller, tractable size — the
        // logic is identical regardless of the absolute cap.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        // 64 KiB of garbage (well under the 1 MiB cap) followed by '\n'.
        // The point is the trailing-newline boundary, not the cap value.
        let body = format!("{}\n", "y".repeat(64 * 1024));
        std::fs::write(&path, body).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert!(entries.is_empty());
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.errors.len(), 1);
        assert!(
            !stats.errors[0].error.contains("exceeds"),
            "newline-terminated line at-or-under cap must not be flagged oversized: {}",
            stats.errors[0].error
        );
    }

    #[test]
    fn invalid_utf8_is_rejected_as_parse_error_not_silently_rewritten() {
        // Quorum re-review #3 (gpt-5.4, severity=high): the loader must
        // not run JSONL bytes through `from_utf8_lossy` before parsing,
        // because U+FFFD replacements could mutate string fields and
        // make a corrupted row "successfully" deserialize. Strict
        // UTF-8 validation is required at this trust boundary.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let good = serde_json::to_string(&sample_entry()).unwrap();
        // Build a single line with an invalid UTF-8 byte sequence
        // (lone continuation byte 0xFF) embedded inside it.
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        // Second line: an opening brace + lone 0xFF + closing brace +
        // newline. Looks JSON-shaped but is not valid UTF-8.
        bytes.extend_from_slice(b"{\"model\":\"");
        bytes.push(0xFF);
        bytes.extend_from_slice(b"\"}\n");
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).unwrap();
        let store = TelemetryStore::new(path);
        let (entries, stats) = store.load_all_with_stats().unwrap();
        // Two valid rows, one rejected for invalid UTF-8.
        assert_eq!(entries.len(), 2, "valid rows must still parse");
        assert_eq!(stats.kept, 2);
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.errors.len(), 1);
        assert_eq!(stats.errors[0].line_no, 2);
        assert!(
            stats.errors[0].error.to_lowercase().contains("utf-8")
                || stats.errors[0].error.to_lowercase().contains("utf8"),
            "expected UTF-8 error, got: {}",
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
            .unwrap()
            .with_timezone(&Utc);
        store.record(&old).unwrap();

        let recent = sample_entry(); // ts = now
        store.record(&recent).unwrap();

        let since = chrono::DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let entries = store.load_since(since).unwrap();
        assert_eq!(entries.len(), 1);
    }

    // ── SQLite backend tests ──────────────────────────────────────────

    /// Fixture builder for test `TelemetryEntry`s. Reduces duplication
    /// across SQLite tests -- callers override only what they need.
    fn test_telemetry_entry() -> TelemetryEntry {
        TelemetryEntry {
            ts: chrono::Utc::now(),
            files: vec![],
            findings: HashMap::new(),
            model: "test-model".to_string(),
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
            suppressed: 0,
            context7_resolved: 0,
            context7_resolve_failed: 0,
            context7_query_failed: 0,
            context7_skipped_popular: 0,
            context7_budget_reduced: 0,
            fp_kind_utilization_rate: None,
            judge_calls: 0,
            judge_approved: 0,
            judge_rejected: 0,
            judge_uncertain: 0,
            judge_skipped: 0,
            judge_cache_hits: 0,
            judge_latency_ms: 0,
        }
    }

    /// Helper: create a SQLite-backed TelemetryStore in a temp directory.
    fn sqlite_telemetry_store(dir: &TempDir) -> TelemetryStore {
        let handle = crate::storage::initialize(dir.path()).expect("storage init");
        TelemetryStore::with_storage(handle)
    }

    #[test]
    fn sqlite_record_round_trip() {
        // Full entry with all fields populated; verify every field
        // survives a write-then-read cycle through SQLite.
        let dir = TempDir::new().unwrap();
        let store = sqlite_telemetry_store(&dir);

        let mut findings = HashMap::new();
        findings.insert("critical".to_string(), 3_usize);
        findings.insert("warning".to_string(), 7_usize);
        findings.insert("info".to_string(), 1_usize);

        let entry = TelemetryEntry {
            ts: chrono::DateTime::parse_from_rfc3339("2026-05-10T14:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            files: vec![
                "src/main.rs".to_string(),
                "src/lib.rs".to_string(),
                "tests/integration.rs".to_string(),
            ],
            findings,
            model: "gpt-5.4".to_string(),
            tokens_in: 12_345,
            tokens_out: 678,
            duration_ms: 4_200,
            suppressed: 5,
            context7_resolved: 3,
            context7_resolve_failed: 1,
            context7_query_failed: 2,
            context7_skipped_popular: 4,
            context7_budget_reduced: 6,
            fp_kind_utilization_rate: Some(0.42),
            judge_calls: 10,
            judge_approved: 7,
            judge_rejected: 2,
            judge_uncertain: 1,
            judge_skipped: 3,
            judge_cache_hits: 5,
            judge_latency_ms: 850,
        };

        store.record(&entry).unwrap();
        let (loaded, stats) = store.load_all_with_stats().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(stats.kept, 1);
        assert_eq!(stats.skipped, 0);
        assert!(stats.errors.is_empty());

        let got = &loaded[0];
        assert_eq!(got.ts, entry.ts);
        assert_eq!(got.files, entry.files);
        assert_eq!(got.findings, entry.findings);
        assert_eq!(got.model, entry.model);
        assert_eq!(got.tokens_in, entry.tokens_in);
        assert_eq!(got.tokens_out, entry.tokens_out);
        assert_eq!(got.duration_ms, entry.duration_ms);
        assert_eq!(got.suppressed, entry.suppressed);
        assert_eq!(got.context7_resolved, entry.context7_resolved);
        assert_eq!(got.context7_resolve_failed, entry.context7_resolve_failed);
        assert_eq!(got.context7_query_failed, entry.context7_query_failed);
        assert_eq!(got.context7_skipped_popular, entry.context7_skipped_popular);
        assert_eq!(got.context7_budget_reduced, entry.context7_budget_reduced);
        assert_eq!(
            got.fp_kind_utilization_rate,
            entry.fp_kind_utilization_rate
        );
        assert_eq!(got.judge_calls, entry.judge_calls);
        assert_eq!(got.judge_approved, entry.judge_approved);
        assert_eq!(got.judge_rejected, entry.judge_rejected);
        assert_eq!(got.judge_uncertain, entry.judge_uncertain);
        assert_eq!(got.judge_skipped, entry.judge_skipped);
        assert_eq!(got.judge_cache_hits, entry.judge_cache_hits);
        assert_eq!(got.judge_latency_ms, entry.judge_latency_ms);
    }

    #[test]
    fn sqlite_null_fp_kind_rate() {
        // None fp_kind_utilization_rate round-trips as None through SQLite.
        let dir = TempDir::new().unwrap();
        let store = sqlite_telemetry_store(&dir);

        let entry = test_telemetry_entry();
        assert!(entry.fp_kind_utilization_rate.is_none());

        store.record(&entry).unwrap();
        let loaded = store.load_all().unwrap();

        assert_eq!(loaded.len(), 1);
        assert!(
            loaded[0].fp_kind_utilization_rate.is_none(),
            "None must round-trip as None, got {:?}",
            loaded[0].fp_kind_utilization_rate
        );
    }

    #[test]
    fn sqlite_files_and_findings_json_round_trip() {
        // Verify Vec<String> and HashMap<String, usize> survive the JSON
        // column serialization/deserialization cycle.
        let dir = TempDir::new().unwrap();
        let store = sqlite_telemetry_store(&dir);

        let mut findings = HashMap::new();
        findings.insert("sql-injection".to_string(), 2_usize);
        findings.insert("xss".to_string(), 5_usize);
        findings.insert("secrets".to_string(), 0_usize);

        let mut entry = test_telemetry_entry();
        entry.files = vec![
            "src/handler.rs".to_string(),
            "src/routes/auth.rs".to_string(),
        ];
        entry.findings = findings.clone();

        store.record(&entry).unwrap();
        let loaded = store.load_all().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].files,
            vec!["src/handler.rs", "src/routes/auth.rs"]
        );
        assert_eq!(loaded[0].findings, findings);
    }

    #[test]
    fn sqlite_load_all_with_stats_returns_correct_counts() {
        // stats.kept must match actual count; stats.skipped must be 0;
        // stats.errors must be empty (SQLite has no parse errors).
        let dir = TempDir::new().unwrap();
        let store = sqlite_telemetry_store(&dir);

        for i in 0..5 {
            let mut entry = test_telemetry_entry();
            entry.model = format!("model-{i}");
            store.record(&entry).unwrap();
        }

        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(stats.kept, 5);
        assert_eq!(stats.skipped, 0);
        assert!(stats.errors.is_empty());
    }

    #[test]
    fn sqlite_empty_db_returns_empty() {
        // load_all on a fresh SQLite DB returns an empty vec.
        let dir = TempDir::new().unwrap();
        let store = sqlite_telemetry_store(&dir);
        let loaded = store.load_all().unwrap();
        assert!(loaded.is_empty());

        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert!(entries.is_empty());
        assert_eq!(stats.kept, 0);
        assert_eq!(stats.skipped, 0);
        assert!(stats.errors.is_empty());
    }
}
