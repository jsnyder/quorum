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
    pub fn load_all_with_stats(&self) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        use std::io::{BufRead, BufReader};

        if !self.path.exists() {
            return Ok((vec![], LoadStats::default()));
        }
        let file = std::fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries: Vec<TelemetryEntry> = Vec::new();
        let mut stats = LoadStats::default();

        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TelemetryEntry>(&line) {
                Ok(entry) => {
                    entries.push(entry);
                    stats.kept += 1;
                }
                Err(e) => {
                    stats.skipped += 1;
                    stats.errors.push(ParseError {
                        line_no: idx + 1,
                        snippet: line.chars().take(80).collect(),
                        error: e.to_string(),
                    });
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
