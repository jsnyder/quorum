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

/// Outcome of the `quorum context` retrieve→plan→render pipeline for a
/// single review invocation.
///
/// When no injector was wired into the pipeline (the default), callers
/// still record a default-constructed [`ContextTelemetry`] (all zeros /
/// `false` / empty) so dashboards can distinguish "no injector" from
/// "injector produced nothing".
///
/// Backwards-compatibility: the `context` field on [`ReviewRecord`] uses
/// `#[serde(default)]`, so legacy records written before this block
/// existed still deserialize cleanly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextTelemetry {
    /// True iff `sources.context.auto_inject` was enabled for this review.
    #[serde(default)]
    pub auto_inject_enabled: bool,
    /// True iff an injector was wired into the pipeline at all.
    #[serde(default)]
    pub injector_available: bool,
    /// True iff the retriever closure returned an error for this review
    /// (dashboards can distinguish "retriever crashed" from "no hits").
    #[serde(default)]
    pub retriever_errored: bool,
    /// Total chunks returned by the retriever (pre-plan filtering).
    #[serde(default)]
    pub retrieved_chunk_count: u32,
    /// Chunks that ended up in the rendered block.
    #[serde(default)]
    pub injected_chunk_count: u32,
    /// Token cost of the injected chunks (as counted by the planner).
    #[serde(default)]
    pub injected_tokens: u32,
    /// Candidates whose score fell below the effective threshold.
    #[serde(default)]
    pub below_threshold_count: u32,
    /// True iff the planner lowered the prose threshold adaptively.
    #[serde(default)]
    pub adaptive_threshold_applied: bool,
    /// Prose threshold actually applied (may differ from `inject_min_score`
    /// when adaptive lowering kicked in).
    #[serde(default)]
    pub effective_prose_threshold: f32,
    /// Chunk IDs in emission order.
    #[serde(default)]
    pub injected_chunk_ids: Vec<String>,
    /// Unique source names represented in the injected chunks.
    #[serde(default)]
    pub injected_sources: Vec<String>,
    /// Count of precedence winner-records (deduped qualified_names).
    #[serde(default)]
    pub precedence_entries: u32,
    /// Cumulative retrieve+plan+render wall time for this invocation.
    #[serde(default)]
    pub render_duration_ms: u64,
    /// sha256 of the rendered context block. `None` when no block was
    /// injected.
    #[serde(default)]
    pub rendered_prompt_hash: Option<String>,
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
    /// Context-injection telemetry for this invocation. Defaults to a
    /// semantic-zero [`ContextTelemetry`] when no injector was wired.
    /// Marked `#[serde(default)]` for backwards-compat with records
    /// written before this field existed.
    #[serde(default)]
    pub context: ContextTelemetry,
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

/// Deterministic sha256 hex digest of a rendered context block. Used as
/// the `rendered_prompt_hash` field in [`ContextTelemetry`] so dashboards
/// can group reruns that produced byte-identical blocks.
pub fn hash_rendered_block(rendered: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(rendered.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
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
            context: ContextTelemetry::default(),
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

    // ---- ContextTelemetry (Task 6.2) ------------------------------------

    fn populated_context_telemetry() -> ContextTelemetry {
        ContextTelemetry {
            auto_inject_enabled: true,
            injector_available: true,
            retriever_errored: false,
            retrieved_chunk_count: 5,
            injected_chunk_count: 2,
            injected_tokens: 180,
            below_threshold_count: 3,
            adaptive_threshold_applied: false,
            effective_prose_threshold: 0.65,
            injected_chunk_ids: vec!["chunk-a".into(), "chunk-b".into()],
            injected_sources: vec!["mini-rust".into()],
            precedence_entries: 1,
            render_duration_ms: 42,
            rendered_prompt_hash: Some("deadbeef".into()),
        }
    }

    #[test]
    fn context_telemetry_populated_after_successful_injection() {
        // Simulates the pipeline handing the review-log a non-default
        // ContextTelemetry after a successful injection pass. The record
        // must serialize with every telemetry field present.
        let mut rec = sample_record();
        rec.context = populated_context_telemetry();

        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("\"auto_inject_enabled\":true"), "json: {json}");
        assert!(json.contains("\"injector_available\":true"));
        assert!(json.contains("\"retrieved_chunk_count\":5"));
        assert!(json.contains("\"injected_chunk_count\":2"));
        assert!(json.contains("\"injected_tokens\":180"));
        assert!(json.contains("\"below_threshold_count\":3"));
        assert!(json.contains("\"adaptive_threshold_applied\":false"));
        assert!(json.contains("\"effective_prose_threshold\":0.65"));
        assert!(json.contains("\"chunk-a\""));
        assert!(json.contains("\"chunk-b\""));
        assert!(json.contains("\"mini-rust\""));
        assert!(json.contains("\"precedence_entries\":1"));
        assert!(json.contains("\"render_duration_ms\":42"));
        assert!(json.contains("\"rendered_prompt_hash\":\"deadbeef\""));

        let back: ReviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.context, populated_context_telemetry());
    }

    #[test]
    fn context_telemetry_is_semantic_zeros_when_no_injector_wired() {
        // When no injector is wired, the pipeline writes a default
        // ContextTelemetry: auto_inject_enabled=false, everything else
        // 0/empty/false. The record round-trips cleanly.
        let rec = sample_record();
        assert!(!rec.context.auto_inject_enabled);
        assert!(!rec.context.injector_available);
        assert_eq!(rec.context.retrieved_chunk_count, 0);
        assert_eq!(rec.context.injected_chunk_count, 0);
        assert_eq!(rec.context.injected_tokens, 0);
        assert_eq!(rec.context.below_threshold_count, 0);
        assert!(!rec.context.adaptive_threshold_applied);
        assert_eq!(rec.context.effective_prose_threshold, 0.0);
        assert!(rec.context.injected_chunk_ids.is_empty());
        assert!(rec.context.injected_sources.is_empty());
        assert_eq!(rec.context.precedence_entries, 0);
        assert_eq!(rec.context.render_duration_ms, 0);
        assert!(rec.context.rendered_prompt_hash.is_none());

        // End-to-end: record must write + reload identically.
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("reviews.jsonl"));
        log.record(&rec).unwrap();
        let back = log.load_all().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].context, ContextTelemetry::default());
    }

    #[test]
    fn legacy_review_record_without_context_field_deserializes() {
        // Legacy JSON line written by quorum <= v0.15.x (no `context`
        // field). Must deserialize, with `context` defaulted to zeros.
        let legacy = r#"{
            "run_id":"01HX000000000000000000000X",
            "timestamp":"2026-04-20T12:00:00Z",
            "quorum_version":"0.15.0",
            "repo":"legacy-repo",
            "invoked_from":"tty",
            "model":"gpt-5.4",
            "files_reviewed":1,
            "lines_added":null,
            "lines_removed":null,
            "findings_by_severity":{"critical":0,"high":0,"medium":0,"low":0,"info":0},
            "tokens_in":100,
            "tokens_out":20,
            "duration_ms":500
        }"#;
        let rec: ReviewRecord = serde_json::from_str(legacy)
            .expect("legacy record without `context` field must deserialize");
        assert_eq!(rec.context, ContextTelemetry::default());
        assert_eq!(rec.run_id, "01HX000000000000000000000X");
        assert_eq!(rec.repo.as_deref(), Some("legacy-repo"));
    }

    #[test]
    fn context_telemetry_hash_is_stable_across_reruns() {
        // Same rendered string → same hash. Different string → different
        // hash. Guards against accidental use of a non-deterministic
        // hasher or per-run salt.
        use super::hash_rendered_block;
        let a = hash_rendered_block("# Context\n\n## mini-rust\n\nfoo");
        let b = hash_rendered_block("# Context\n\n## mini-rust\n\nfoo");
        let c = hash_rendered_block("# Context\n\n## mini-rust\n\nbar");
        assert_eq!(a, b, "deterministic hasher must agree across calls");
        assert_ne!(a, c, "distinct inputs must produce distinct hashes");
        assert_eq!(a.len(), 64, "sha256 hex digest is 64 chars");
    }
}
