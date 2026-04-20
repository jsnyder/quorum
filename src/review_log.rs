//! Per-review telemetry log: one JSON line per `quorum review` invocation.
//!
//! Recorded to `~/.quorum/reviews.jsonl` to enable dimensional analytics
//! (by-repo, by-caller, rolling trend) in `quorum stats`. Joins to
//! `feedback.jsonl` / `calibrator_traces.jsonl` via `run_id` (ULID).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::finding::Severity;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeverityCounts {
    #[serde(default)]
    pub critical: u32,
    #[serde(default)]
    pub high: u32,
    #[serde(default)]
    pub medium: u32,
    #[serde(default)]
    pub low: u32,
    #[serde(default)]
    pub info: u32,
}

impl SeverityCounts {
    pub fn from_severities<'a, I: IntoIterator<Item = &'a Severity>>(iter: I) -> Self {
        let mut s = Self::default();
        for sev in iter {
            match sev {
                Severity::Critical => s.critical += 1,
                Severity::High => s.high += 1,
                Severity::Medium => s.medium += 1,
                Severity::Low => s.low += 1,
                Severity::Info => s.info += 1,
            }
        }
        s
    }

    pub fn total(&self) -> u32 {
        self.critical + self.high + self.medium + self.low + self.info
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Flags {
    #[serde(default)]
    pub deep: bool,
    #[serde(default)]
    pub parallel_n: u32,
    #[serde(default)]
    pub ensemble: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub quorum_version: String,
    pub repo: Option<String>,
    pub invoked_from: String,
    pub model: String,
    pub files_reviewed: u32,
    pub lines_added: Option<u32>,
    pub lines_removed: Option<u32>,
    pub findings_by_severity: SeverityCounts,
    #[serde(default)]
    pub suppressed_by_rule: HashMap<String, u32>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(default)]
    pub tokens_cache_read: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub flags: Flags,
}

impl ReviewRecord {
    pub fn new_ulid() -> String {
        Ulid::new().to_string()
    }
}

pub struct ReviewLog {
    path: PathBuf,
}

impl ReviewLog {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stream records line-by-line from the log, skipping malformed lines.
    /// Returns an empty iterator if the file does not exist.
    pub fn iter(&self) -> anyhow::Result<ReviewLogIter> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};
        if !self.path.exists() {
            return Ok(ReviewLogIter { inner: None });
        }
        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open review log: {}", self.path.display()))?;
        let reader: Box<dyn BufRead> = Box::new(BufReader::new(file));
        Ok(ReviewLogIter { inner: Some(reader.lines()) })
    }

    /// Convenience: collect all records (suitable for small logs and tests).
    pub fn load_all(&self) -> anyhow::Result<Vec<ReviewRecord>> {
        self.iter()?.collect()
    }

    /// Append one record as a JSON line. Creates the file (and parent dir) if missing.
    pub fn record(&self, entry: &ReviewRecord) -> anyhow::Result<()> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create review log dir: {}", parent.display())
                })?;
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open review log: {}", self.path.display()))?;
        let mut buf = serde_json::to_string(entry)?;
        buf.push('\n');
        file.write_all(buf.as_bytes())?;
        Ok(())
    }
}

/// Streaming iterator over a reviews.jsonl file.
/// Malformed lines are logged to stderr and skipped — parity with FeedbackStore.
pub struct ReviewLogIter {
    inner: Option<std::io::Lines<Box<dyn std::io::BufRead>>>,
}

impl Iterator for ReviewLogIter {
    type Item = anyhow::Result<ReviewRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        let lines = self.inner.as_mut()?;
        loop {
            match lines.next()? {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => match serde_json::from_str::<ReviewRecord>(&line) {
                    Ok(rec) => return Some(Ok(rec)),
                    Err(e) => {
                        eprintln!("warning: skipping malformed review record: {}", e);
                        continue;
                    }
                },
                Err(e) => return Some(Err(anyhow::anyhow!("read error: {}", e))),
            }
        }
    }
}

/// Detect invocation context from env vars. Mirrors the detection used for
/// compact-mode sniffing in telemetry.rs. Priority order matters: more specific
/// signals beat generic `AGENT`.
pub fn detect_invoked_from(caller_override: Option<&str>) -> String {
    if let Some(name) = caller_override {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if std::env::var_os("CLAUDE_CODE").is_some() {
        return "claude_code".to_string();
    }
    if std::env::var_os("CODEX_CI").is_some() {
        return "codex_ci".to_string();
    }
    if std::env::var_os("GEMINI_CLI").is_some() {
        return "gemini_cli".to_string();
    }
    if let Some(v) = std::env::var_os("AGENT") {
        if let Some(s) = v.to_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        "tty".to_string()
    } else {
        "pipe".to_string()
    }
}

/// Walk parents of the given path looking for a `.git` directory.
/// Returns the basename of the repo root, or None if not in a git repo.
pub fn detect_repo(start: &Path) -> Option<String> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    let mut cur: &Path = &start;
    loop {
        if cur.join(".git").exists() {
            return cur.file_name().and_then(|s| s.to_str()).map(String::from);
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_record() -> ReviewRecord {
        let mut suppressed = HashMap::new();
        suppressed.insert("tautological-length".into(), 2);
        ReviewRecord {
            run_id: ReviewRecord::new_ulid(),
            timestamp: Utc::now(),
            quorum_version: env!("CARGO_PKG_VERSION").to_string(),
            repo: Some("quorum".into()),
            invoked_from: "tty".into(),
            model: "gpt-5.4".into(),
            files_reviewed: 3,
            lines_added: Some(120),
            lines_removed: Some(40),
            findings_by_severity: SeverityCounts {
                critical: 1,
                high: 2,
                medium: 0,
                low: 0,
                info: 4,
            },
            suppressed_by_rule: suppressed,
            tokens_in: 12_345,
            tokens_out: 678,
            tokens_cache_read: 8_000,
            duration_ms: 4_200,
            flags: Flags { deep: false, parallel_n: 4, ensemble: false },
        }
    }

    #[test]
    fn record_round_trips_through_json() {
        let rec = sample_record();
        let json = serde_json::to_string(&rec).unwrap();
        let back: ReviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn ulid_is_26_chars_and_unique() {
        let a = ReviewRecord::new_ulid();
        let b = ReviewRecord::new_ulid();
        assert_eq!(a.len(), 26);
        assert_eq!(b.len(), 26);
        assert_ne!(a, b);
    }

    #[test]
    fn writer_creates_and_appends() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reviews.jsonl");
        let log = ReviewLog::new(path.clone());
        log.record(&sample_record()).unwrap();
        log.record(&sample_record()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let _: ReviewRecord = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn writer_creates_missing_parent_dir() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/sub/reviews.jsonl");
        let log = ReviewLog::new(path.clone());
        log.record(&sample_record()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn severity_counts_from_iter() {
        let sevs = [
            Severity::Critical,
            Severity::High,
            Severity::High,
            Severity::Info,
            Severity::Info,
            Severity::Info,
        ];
        let sc = SeverityCounts::from_severities(sevs.iter());
        assert_eq!(sc.critical, 1);
        assert_eq!(sc.high, 2);
        assert_eq!(sc.info, 3);
        assert_eq!(sc.total(), 6);
    }

    #[test]
    fn invoked_from_override_wins() {
        let got = detect_invoked_from(Some("my-script"));
        assert_eq!(got, "my-script");
    }

    #[test]
    fn invoked_from_claude_code_env() {
        // Serialize env-var tests so concurrent tests don't race on env state.
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CLAUDE_CODE");
        unsafe { std::env::set_var("CLAUDE_CODE", "1") };
        let got = detect_invoked_from(None);
        match prev {
            Some(v) => unsafe { std::env::set_var("CLAUDE_CODE", v) },
            None => unsafe { std::env::remove_var("CLAUDE_CODE") },
        }
        assert_eq!(got, "claude_code");
    }

    #[test]
    fn invoked_from_agent_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_claude = std::env::var_os("CLAUDE_CODE");
        let prev_codex = std::env::var_os("CODEX_CI");
        let prev_gemini = std::env::var_os("GEMINI_CLI");
        let prev_agent = std::env::var_os("AGENT");
        unsafe {
            std::env::remove_var("CLAUDE_CODE");
            std::env::remove_var("CODEX_CI");
            std::env::remove_var("GEMINI_CLI");
            std::env::set_var("AGENT", "cursor");
        }
        let got = detect_invoked_from(None);
        // Restore
        unsafe {
            if let Some(v) = prev_claude { std::env::set_var("CLAUDE_CODE", v); }
            if let Some(v) = prev_codex { std::env::set_var("CODEX_CI", v); }
            if let Some(v) = prev_gemini { std::env::set_var("GEMINI_CLI", v); }
            match prev_agent {
                Some(v) => std::env::set_var("AGENT", v),
                None => std::env::remove_var("AGENT"),
            }
        }
        assert_eq!(got, "cursor");
    }

    #[test]
    fn detect_repo_finds_git_root() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let sub = dir.path().join("src/nested");
        std::fs::create_dir_all(&sub).unwrap();
        let got = detect_repo(&sub).unwrap();
        let expected = dir.path().file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(got, expected);
    }

    #[test]
    fn detect_repo_returns_none_for_filesystem_root() {
        // Root has no parent with a .git directory (on any reasonable system).
        // Using "/" guarantees we exhaust the parent chain without a match.
        let got = detect_repo(Path::new("/"));
        assert!(got.is_none(), "filesystem root should yield None, got {:?}", got);
    }

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn iter_over_empty_path_yields_nothing() {
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("absent.jsonl"));
        let n = log.iter().unwrap().count();
        assert_eq!(n, 0);
    }

    #[test]
    fn iter_preserves_insertion_order() {
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("reviews.jsonl"));
        let mut ids = Vec::new();
        for _ in 0..5 {
            let mut r = sample_record();
            r.run_id = ReviewRecord::new_ulid();
            ids.push(r.run_id.clone());
            log.record(&r).unwrap();
        }
        let got: Vec<String> = log.iter().unwrap()
            .map(|r| r.unwrap().run_id)
            .collect();
        assert_eq!(got, ids);
    }

    #[test]
    fn iter_skips_malformed_lines() {
        use std::io::Write;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reviews.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "{}", serde_json::to_string(&sample_record()).unwrap()).unwrap();
            writeln!(f, "{{ this is not json").unwrap();
            writeln!(f, "").unwrap();
            writeln!(f, "{}", serde_json::to_string(&sample_record()).unwrap()).unwrap();
        }
        let log = ReviewLog::new(path);
        let records: Vec<_> = log.iter().unwrap().filter_map(|r| r.ok()).collect();
        assert_eq!(records.len(), 2, "should skip malformed + blank, keep valid");
    }

    #[test]
    fn load_all_round_trips_many_records() {
        // Smoke test that many records can be written and re-read.
        // Verifies streaming path works for larger inputs.
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("reviews.jsonl"));
        for _ in 0..1_000 {
            log.record(&sample_record()).unwrap();
        }
        let loaded = log.load_all().unwrap();
        assert_eq!(loaded.len(), 1_000);
    }
}
