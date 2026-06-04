//! Audit log infrastructure for the skills framework.
//!
//! Provides shared append-only JSONL infrastructure and schema definitions for
//! the skills framework audit trail. Other issues (#410, #411) will write rows
//! to these files; this module defines the schemas, the writer, the reader,
//! and the `skills.lock` mechanism.
//!
//! ## Design
//!
//! - `AuditWriter<T: Serialize>`: cross-process safe JSONL appender using
//!   `fs2` exclusive advisory locks (mirrors `src/feedback.rs` pattern).
//! - `AuditReader<T: DeserializeOwned>`: bounded-line JSONL reader using
//!   `fs2` shared locks (per #233, no `BufRead::lines()`).
//! - `SkillInvocationRecord`: one row per (skill x model x file x review) cell.
//! - `IntegratorDecisionRecord`: one row per integrator merge/suppress decision.
//! - `SkillsLock`: TOML-based tamper-detection lock file.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::skill_manifest::LoadedSkill;

// ---------------------------------------------------------------------------
// Default file paths (relative to ~/.quorum/)
// ---------------------------------------------------------------------------

/// Default basename for the skill invocation audit log.
pub const SKILL_INVOCATIONS_FILE: &str = "skill_invocations.jsonl";

/// Default basename for the integrator decision audit log.
pub const INTEGRATOR_DECISIONS_FILE: &str = "integrator_decisions.jsonl";

/// Default basename for the skills lock file.
pub const SKILLS_LOCK_FILE: &str = "skills.lock";

// ---------------------------------------------------------------------------
// Bounded-read constants
// ---------------------------------------------------------------------------

/// Maximum bytes per JSONL line before the line is considered oversized and
/// skipped. Skill invocation and integrator records are typically a few
/// hundred bytes; 1 MiB is a generous ceiling.
const MAX_JSONL_LINE_BYTES: usize = 1 << 20;

// ---------------------------------------------------------------------------
// AuditReadStats
// ---------------------------------------------------------------------------

/// Statistics returned alongside parsed records from `AuditReader::load_all`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AuditReadStats {
    /// Total non-empty lines encountered.
    pub total_lines: usize,
    /// Lines that parsed successfully.
    pub parsed_ok: usize,
    /// Lines that failed to parse (malformed JSON, schema mismatch, oversized).
    pub parse_errors: usize,
}

// ---------------------------------------------------------------------------
// AuditWriter<T>
// ---------------------------------------------------------------------------

/// Cross-process-safe append-only JSONL writer.
///
/// Opens a file with `O_APPEND | O_CREAT`, takes an exclusive `fs2` lock
/// before writing, serializes `T` to JSON + newline, and unlocks after write
/// (even on error). Creates parent directories if needed.
pub struct AuditWriter<T: Serialize> {
    path: PathBuf,
    _marker: PhantomData<T>,
}

impl<T: Serialize> AuditWriter<T> {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            _marker: PhantomData,
        }
    }

    /// Append a single record to the JSONL file.
    pub fn write(&self, record: &T) -> anyhow::Result<()> {
        use fs2::FileExt;
        use std::io::Write;

        // Create parent directories if needed (mirrors feedback.rs #100).
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create audit log parent dir: {}",
                    parent.display()
                )
            })?;
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open audit log file: {}", self.path.display()))?;

        // Exclusive advisory lock (issue #185: cross-process append safety).
        FileExt::lock_exclusive(&file)
            .with_context(|| format!("Failed to lock audit log file: {}", self.path.display()))?;

        let mut buf = serde_json::to_string(record)?;
        buf.push('\n');
        let write_result = file.write_all(buf.as_bytes());

        // Always attempt unlock, even if write failed.
        let unlock_result = FileExt::unlock(&file);
        write_result?;
        unlock_result
            .with_context(|| format!("Failed to unlock audit log file: {}", self.path.display()))?;

        Ok(())
    }

    /// Read-only access to the underlying path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// AuditReader<T>
// ---------------------------------------------------------------------------

/// Bounded-line JSONL reader with shared locking.
///
/// Uses `BufRead::read_until` with a per-line byte cap (per #233 — no
/// `BufRead::lines()` which is unbounded). Malformed rows are skipped with
/// a warning counter rather than aborting.
pub struct AuditReader<T: DeserializeOwned> {
    path: PathBuf,
    _marker: PhantomData<T>,
}

impl<T: DeserializeOwned> AuditReader<T> {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            _marker: PhantomData,
        }
    }

    /// Load all records from the JSONL file.
    ///
    /// Returns the successfully parsed records together with structured
    /// read statistics. Malformed or oversized lines increment
    /// `stats.parse_errors` rather than aborting.
    pub fn load_all(&self) -> anyhow::Result<(Vec<T>, AuditReadStats)> {
        use fs2::FileExt;
        use std::io::{BufRead, BufReader, Read};

        let file = match std::fs::OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((vec![], AuditReadStats::default()));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Failed to open audit log file: {}", self.path.display())
                });
            }
        };

        // Shared advisory lock (pairs with exclusive lock in AuditWriter).
        FileExt::lock_shared(&file).with_context(|| {
            format!(
                "Failed to lock audit log file for read: {}",
                self.path.display()
            )
        })?;

        let mut reader = BufReader::new(&file);
        let mut entries: Vec<T> = Vec::new();
        let mut stats = AuditReadStats::default();
        let mut buf = Vec::with_capacity(4096);

        loop {
            buf.clear();
            // Bounded read: take at most MAX + 2 bytes (to accommodate
            // trailing \r\n on a line of exactly MAX payload bytes).
            let mut limited = (&mut reader).take((MAX_JSONL_LINE_BYTES + 2) as u64);
            let n = limited.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }

            // Detect oversized line.
            let payload_len = if buf.ends_with(b"\n") {
                let end = buf.len() - 1;
                if end > 0 && buf[end - 1] == b'\r' {
                    end - 1
                } else {
                    end
                }
            } else {
                buf.len()
            };

            let oversized = payload_len > MAX_JSONL_LINE_BYTES;
            if oversized {
                // Drain the rest of the oversized line so we resync to the
                // next newline without unbounded allocation.
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
                stats.total_lines += 1;
                stats.parse_errors += 1;
                tracing::warn!(
                    target: "quorum::skill_audit",
                    path = %self.path.display(),
                    "oversized line ({payload_len} bytes) skipped in audit log"
                );
                continue;
            }

            // Trim trailing newline / CRLF.
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

            // Skip empty / whitespace-only lines.
            if line_bytes.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }

            stats.total_lines += 1;

            // Strict UTF-8 validation.
            let line = match std::str::from_utf8(line_bytes) {
                Ok(s) => s,
                Err(_) => {
                    stats.parse_errors += 1;
                    tracing::warn!(
                        target: "quorum::skill_audit",
                        path = %self.path.display(),
                        "invalid UTF-8 in audit log line; skipping"
                    );
                    continue;
                }
            };

            match serde_json::from_str(line) {
                Ok(entry) => {
                    entries.push(entry);
                    stats.parsed_ok += 1;
                }
                Err(_) => {
                    stats.parse_errors += 1;
                    tracing::warn!(
                        target: "quorum::skill_audit",
                        path = %self.path.display(),
                        "malformed JSON in audit log line; skipping"
                    );
                }
            }
        }

        // Unlock after reading.
        let unlock_result = FileExt::unlock(&file);
        unlock_result
            .with_context(|| format!("Failed to unlock audit log file: {}", self.path.display()))?;

        Ok((entries, stats))
    }

    /// Read-only access to the underlying path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Schema enums
// ---------------------------------------------------------------------------

/// How the review axis set was selected for this invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisSelectionSource {
    ExplicitAxes,
    ModeMacro,
    Default,
    AutoDiscovery,
}

/// Terminal exit status for a skill invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitStatus {
    Ok,
    Error,
}

/// Reason a skill invocation failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    ModelTimeout,
    ModelRateLimit,
    BudgetCapHit,
    CapabilityDenied,
    NetworkError,
    Other,
}

/// Integrator decision for a finding cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegratorDecision {
    Merged,
    Suppressed,
    PassThrough,
}

// ---------------------------------------------------------------------------
// SkillInvocationRecord
// ---------------------------------------------------------------------------

/// One record per (skill x model x file x review) cell.
///
/// See design doc section 5.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillInvocationRecord {
    /// ULID for this specific skill run.
    pub skill_run_id: String,
    /// Parent review ULID.
    pub run_id: String,
    pub ts: DateTime<Utc>,
    pub skill_name: String,
    pub skill_version: String,
    pub manifest_sha256: String,
    pub prompt_family: String,
    pub prompt_sha256: String,
    pub model: String,
    pub model_was_fallback: bool,
    pub axis_selection_source: AxisSelectionSource,
    /// "pure" in v1.
    pub capability_mode: String,
    /// "bundled" | "user" | "untrusted".
    pub trust_tier: String,
    pub file_path: String,
    pub file_sha256: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(default)]
    pub tokens_cache_read: u64,
    #[serde(default)]
    pub llm_cache_hit: bool,
    pub duration_ms: u64,
    pub findings_emitted: u32,
    #[serde(default)]
    pub findings_clamped: u32,
    #[serde(default)]
    pub findings_dropped_invalid_json: u32,
    /// From #407's `ParseErrorClass`, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_error_class: Option<String>,
    pub exit_status: ExitStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<FailureReason>,
    #[serde(default)]
    pub calibrator_suppressions: u32,
    #[serde(default)]
    pub calibrator_precedents_matched: u32,
}

// ---------------------------------------------------------------------------
// ClusterKey
// ---------------------------------------------------------------------------

/// Identifies a finding cluster by file location and kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterKey {
    pub file_path: String,
    pub line_range: (u32, u32),
    pub finding_kind: String,
}

// ---------------------------------------------------------------------------
// IntegratorDecisionRecord
// ---------------------------------------------------------------------------

/// One record per integrator merge/suppress/pass-through decision.
///
/// See design doc section 5.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegratorDecisionRecord {
    pub run_id: String,
    pub ts: DateTime<Utc>,
    pub decision: IntegratorDecision,
    pub cluster_key: ClusterKey,
    pub input_finding_ids: Vec<String>,
    pub input_confidences: Vec<f64>,
    pub input_severities: Vec<String>,
    pub calibrator_weights: HashMap<String, f64>,
    pub confidence_floor: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_finding_id: Option<String>,
    pub output_confidence: f64,
    pub severity_pre_clamp: String,
    pub severity_post_clamp: String,
    pub reason: String,
    pub originating_skills: Vec<String>,
}

// ---------------------------------------------------------------------------
// SkillsLock
// ---------------------------------------------------------------------------

/// A single entry in the skills lock file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    pub name: String,
    pub source: String,
    pub manifest_sha256: String,
    pub version: String,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_manifest_sha256: Option<String>,
}

/// Warning emitted when the lock detects a silent manifest edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockWarning {
    /// Name of the skill whose hash changed without a version bump.
    pub skill_name: String,
    /// The hash recorded in the lock before this update.
    pub previous_sha256: String,
    /// The new hash from the loaded skill.
    pub current_sha256: String,
    /// The version that stayed the same.
    pub version: String,
}

/// TOML-serializable wrapper for the lock file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsLock {
    #[serde(default)]
    pub skills: Vec<LockEntry>,
}

impl SkillsLock {
    /// Load an existing lock file, or return an empty lock if the file does
    /// not exist.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let lock: SkillsLock = toml::from_str(&content).with_context(|| {
                    format!("Failed to parse skills lock file: {}", path.display())
                })?;
                Ok(lock)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e)
                .with_context(|| format!("Failed to read skills lock file: {}", path.display())),
        }
    }

    /// Save the lock file to disk atomically (write-to-temp, then rename).
    /// Creates parent directories if needed.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create lock file parent dir: {}",
                    parent.display()
                )
            })?;
        }
        let content =
            toml::to_string_pretty(self).context("Failed to serialize skills lock to TOML")?;

        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, &content).with_context(|| {
            format!(
                "Failed to write temporary lock file: {}",
                tmp_path.display()
            )
        })?;
        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Failed to rename temporary lock file to: {}",
                path.display()
            )
        })?;
        Ok(())
    }

    /// Compare currently loaded skills against the lock state.
    ///
    /// Returns warnings for silent edits: same version but different
    /// manifest hash. Updates the lock in place:
    /// - New skills are added.
    /// - Existing skills are updated (`last_seen_at`, and version/hash
    ///   if changed).
    /// - Skills present in the lock but absent from `loaded` are preserved
    ///   (they may be temporarily absent if a user directory is unavailable).
    pub fn update(&mut self, loaded: &[LoadedSkill]) -> Vec<LockWarning> {
        let now = Utc::now();
        let mut warnings = Vec::new();

        // Index existing lock entries by name for O(1) lookup.
        let mut by_name: HashMap<String, usize> = self
            .skills
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i))
            .collect();

        for skill in loaded {
            let name = &skill.manifest.name;
            let new_sha = &skill.manifest_sha256;
            let new_version = &skill.manifest.version;
            let source = skill.source_path.display().to_string();

            if let Some(&idx) = by_name.get(name) {
                let entry = &mut self.skills[idx];
                entry.last_seen_at = now;
                entry.source = source;

                if entry.version == *new_version && entry.manifest_sha256 != *new_sha {
                    // Silent edit: same version, different hash.
                    warnings.push(LockWarning {
                        skill_name: name.clone(),
                        previous_sha256: entry.manifest_sha256.clone(),
                        current_sha256: new_sha.clone(),
                        version: new_version.clone(),
                    });
                    entry.previous_manifest_sha256 = Some(entry.manifest_sha256.clone());
                    entry.manifest_sha256 = new_sha.clone();
                } else if entry.version != *new_version {
                    // Version change: clean update, no warning.
                    entry.previous_manifest_sha256 = Some(entry.manifest_sha256.clone());
                    entry.manifest_sha256 = new_sha.clone();
                    entry.version = new_version.clone();
                }
                // If both version and hash match, only last_seen_at is bumped.
            } else {
                // New skill, not previously in the lock.
                let entry = LockEntry {
                    name: name.clone(),
                    source,
                    manifest_sha256: new_sha.clone(),
                    version: new_version.clone(),
                    first_seen_at: now,
                    last_seen_at: now,
                    previous_manifest_sha256: None,
                };
                by_name.insert(name.clone(), self.skills.len());
                self.skills.push(entry);
            }
        }

        // Skills present in the lock but absent from `loaded` are preserved
        // (not removed) — they may be temporarily absent.

        warnings
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── Helpers ──────────────────────────────────────────────────────────

    fn sample_invocation_record() -> SkillInvocationRecord {
        SkillInvocationRecord {
            skill_run_id: "01HZ0000000000000000000001".into(),
            run_id: "01HZ0000000000000000000000".into(),
            ts: Utc::now(),
            skill_name: "security".into(),
            skill_version: "1.0.0".into(),
            manifest_sha256: "abcd1234".repeat(8),
            prompt_family: "default".into(),
            prompt_sha256: "ef567890".repeat(8),
            model: "gpt-5.4".into(),
            model_was_fallback: false,
            axis_selection_source: AxisSelectionSource::Default,
            capability_mode: "pure".into(),
            trust_tier: "bundled".into(),
            file_path: "src/main.rs".into(),
            file_sha256: "1111aaaa".repeat(8),
            tokens_in: 1500,
            tokens_out: 300,
            tokens_cache_read: 100,
            llm_cache_hit: false,
            duration_ms: 2500,
            findings_emitted: 3,
            findings_clamped: 1,
            findings_dropped_invalid_json: 0,
            parse_error_class: None,
            exit_status: ExitStatus::Ok,
            failure_reason: None,
            calibrator_suppressions: 0,
            calibrator_precedents_matched: 2,
        }
    }

    fn sample_integrator_record() -> IntegratorDecisionRecord {
        IntegratorDecisionRecord {
            run_id: "01HZ0000000000000000000000".into(),
            ts: Utc::now(),
            decision: IntegratorDecision::Merged,
            cluster_key: ClusterKey {
                file_path: "src/lib.rs".into(),
                line_range: (10, 15),
                finding_kind: "sql-injection".into(),
            },
            input_finding_ids: vec!["f1".into(), "f2".into()],
            input_confidences: vec![0.85, 0.72],
            input_severities: vec!["high".into(), "medium".into()],
            calibrator_weights: HashMap::from([
                ("security".into(), 1.2),
                ("correctness".into(), 0.8),
            ]),
            confidence_floor: 0.5,
            output_finding_id: Some("merged-f1".into()),
            output_confidence: 0.9,
            severity_pre_clamp: "high".into(),
            severity_post_clamp: "high".into(),
            reason: "Two skills agree on SQL injection at same location".into(),
            originating_skills: vec!["security".into(), "correctness".into()],
        }
    }

    fn sample_loaded_skill(name: &str, version: &str, sha: &str) -> LoadedSkill {
        use crate::skill_manifest::*;
        LoadedSkill {
            manifest: SkillManifest {
                name: name.into(),
                version: version.into(),
                display_name: format!("Test {name}"),
                description: "A test skill.".into(),
                preferred_model: None,
                fallback_models: None,
                calibration_namespace: None,
                axis: Axis::Security,
                max_severity: crate::finding::Severity::Critical,
                target_findings: None,
                capability: Capability {
                    mode: CapabilityMode::Pure,
                },
                prompts: Prompts {
                    primary: "Review for issues.".into(),
                    anthropic: None,
                    openai: None,
                    google: None,
                },
                checklist: vec![],
                ast_rules: vec![],
            },
            trust_tier: TrustTier::Bundled,
            source_path: PathBuf::from(format!("/skills/{name}.toml")),
            manifest_sha256: sha.into(),
        }
    }

    // ── AuditWriter / AuditReader tests ─────────────────────────────────

    #[test]
    fn write_single_record_read_back() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        let writer = AuditWriter::<SkillInvocationRecord>::new(path.clone());
        let record = sample_invocation_record();
        writer.write(&record).unwrap();

        let reader = AuditReader::<SkillInvocationRecord>::new(path);
        let (entries, stats) = reader.load_all().unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].skill_name, "security");
        assert_eq!(stats.total_lines, 1);
        assert_eq!(stats.parsed_ok, 1);
        assert_eq!(stats.parse_errors, 0);
    }

    #[test]
    fn write_multiple_records_read_all_back() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.jsonl");

        let writer = AuditWriter::<SkillInvocationRecord>::new(path.clone());
        for i in 0..5 {
            let mut record = sample_invocation_record();
            record.skill_name = format!("skill-{i}");
            writer.write(&record).unwrap();
        }

        let reader = AuditReader::<SkillInvocationRecord>::new(path);
        let (entries, stats) = reader.load_all().unwrap();

        assert_eq!(entries.len(), 5);
        assert_eq!(stats.total_lines, 5);
        assert_eq!(stats.parsed_ok, 5);
        assert_eq!(stats.parse_errors, 0);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.skill_name, format!("skill-{i}"));
        }
    }

    #[test]
    fn malformed_row_skipped_with_error_count() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("malformed.jsonl");

        // Write a valid record, then a malformed line, then another valid one.
        let writer = AuditWriter::<SkillInvocationRecord>::new(path.clone());
        writer.write(&sample_invocation_record()).unwrap();

        // Manually append a malformed line.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "{{this is not valid json}}")
            })
            .unwrap();

        let mut record2 = sample_invocation_record();
        record2.skill_name = "second".into();
        writer.write(&record2).unwrap();

        let reader = AuditReader::<SkillInvocationRecord>::new(path);
        let (entries, stats) = reader.load_all().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(stats.total_lines, 3);
        assert_eq!(stats.parsed_ok, 2);
        assert_eq!(stats.parse_errors, 1);
    }

    #[test]
    fn empty_file_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();

        let reader = AuditReader::<SkillInvocationRecord>::new(path);
        let (entries, stats) = reader.load_all().unwrap();

        assert!(entries.is_empty());
        assert_eq!(stats, AuditReadStats::default());
    }

    #[test]
    fn nonexistent_file_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");

        let reader = AuditReader::<SkillInvocationRecord>::new(path);
        let (entries, stats) = reader.load_all().unwrap();

        assert!(entries.is_empty());
        assert_eq!(stats, AuditReadStats::default());
    }

    #[test]
    fn audit_read_stats_counts_correct() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stats.jsonl");

        // 3 valid, 2 malformed, 1 blank line.
        let writer = AuditWriter::<SkillInvocationRecord>::new(path.clone());
        writer.write(&sample_invocation_record()).unwrap();
        writer.write(&sample_invocation_record()).unwrap();

        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "bad line 1").unwrap();
            writeln!(f).unwrap(); // blank line
            writeln!(f, "bad line 2").unwrap();
        }

        writer.write(&sample_invocation_record()).unwrap();

        let reader = AuditReader::<SkillInvocationRecord>::new(path);
        let (entries, stats) = reader.load_all().unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(stats.total_lines, 5); // 3 valid + 2 malformed; blank skipped
        assert_eq!(stats.parsed_ok, 3);
        assert_eq!(stats.parse_errors, 2);
    }

    #[test]
    fn writer_creates_parent_directories() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("deep").join("nested").join("audit.jsonl");

        let writer = AuditWriter::<SkillInvocationRecord>::new(path.clone());
        writer.write(&sample_invocation_record()).unwrap();

        assert!(path.exists());
        let reader = AuditReader::<SkillInvocationRecord>::new(path);
        let (entries, _) = reader.load_all().unwrap();
        assert_eq!(entries.len(), 1);
    }

    // ── Schema roundtrip tests ──────────────────────────────────────────

    #[test]
    fn skill_invocation_record_full_roundtrip() {
        let record = sample_invocation_record();
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: SkillInvocationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, deserialized);
    }

    #[test]
    fn skill_invocation_record_with_failure() {
        let mut record = sample_invocation_record();
        record.exit_status = ExitStatus::Error;
        record.failure_reason = Some(FailureReason::ModelTimeout);
        record.parse_error_class = Some("truncated_json".into());

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: SkillInvocationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.exit_status, ExitStatus::Error);
        assert_eq!(
            deserialized.failure_reason,
            Some(FailureReason::ModelTimeout)
        );
        assert_eq!(
            deserialized.parse_error_class.as_deref(),
            Some("truncated_json")
        );
    }

    #[test]
    fn integrator_decision_merged_roundtrip() {
        let record = sample_integrator_record();
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: IntegratorDecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, deserialized);
    }

    #[test]
    fn integrator_decision_suppressed_roundtrip() {
        let mut record = sample_integrator_record();
        record.decision = IntegratorDecision::Suppressed;
        record.output_finding_id = None;
        record.output_confidence = 0.0;
        record.reason = "Below confidence floor".into();

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: IntegratorDecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.decision, IntegratorDecision::Suppressed);
        assert!(deserialized.output_finding_id.is_none());
    }

    #[test]
    fn integrator_decision_pass_through_roundtrip() {
        let mut record = sample_integrator_record();
        record.decision = IntegratorDecision::PassThrough;
        record.input_finding_ids = vec!["solo-f1".into()];
        record.input_confidences = vec![0.95];
        record.input_severities = vec!["critical".into()];
        record.originating_skills = vec!["security".into()];
        record.reason = "Single skill, no merge needed".into();

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: IntegratorDecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.decision, IntegratorDecision::PassThrough);
        assert_eq!(deserialized.input_finding_ids.len(), 1);
    }

    #[test]
    fn cluster_key_roundtrip() {
        let key = ClusterKey {
            file_path: "src/main.rs".into(),
            line_range: (42, 50),
            finding_kind: "buffer-overflow".into(),
        };
        let json = serde_json::to_string(&key).unwrap();
        let deserialized: ClusterKey = serde_json::from_str(&json).unwrap();
        assert_eq!(key, deserialized);
    }

    // ── Enum variant serde roundtrips ───────────────────────────────────

    #[test]
    fn axis_selection_source_all_variants_roundtrip() {
        let variants = [
            AxisSelectionSource::ExplicitAxes,
            AxisSelectionSource::ModeMacro,
            AxisSelectionSource::Default,
            AxisSelectionSource::AutoDiscovery,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: AxisSelectionSource = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized, "failed roundtrip for {json}");
        }
    }

    #[test]
    fn exit_status_all_variants_roundtrip() {
        let variants = [ExitStatus::Ok, ExitStatus::Error];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: ExitStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized, "failed roundtrip for {json}");
        }
    }

    #[test]
    fn failure_reason_all_variants_roundtrip() {
        let variants = [
            FailureReason::ModelTimeout,
            FailureReason::ModelRateLimit,
            FailureReason::BudgetCapHit,
            FailureReason::CapabilityDenied,
            FailureReason::NetworkError,
            FailureReason::Other,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: FailureReason = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized, "failed roundtrip for {json}");
        }
    }

    #[test]
    fn integrator_decision_all_variants_roundtrip() {
        let variants = [
            IntegratorDecision::Merged,
            IntegratorDecision::Suppressed,
            IntegratorDecision::PassThrough,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: IntegratorDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized, "failed roundtrip for {json}");
        }
    }

    // ── Integrator record writer/reader roundtrip ───────────────────────

    #[test]
    fn integrator_record_write_read_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("integrator.jsonl");

        let writer = AuditWriter::<IntegratorDecisionRecord>::new(path.clone());
        let record = sample_integrator_record();
        writer.write(&record).unwrap();

        let reader = AuditReader::<IntegratorDecisionRecord>::new(path);
        let (entries, stats) = reader.load_all().unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], record);
        assert_eq!(stats.parsed_ok, 1);
    }

    // ── SkillsLock tests ────────────────────────────────────────────────

    #[test]
    fn fresh_load_from_nonexistent_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.lock");
        let lock = SkillsLock::load(&path).unwrap();
        assert!(lock.skills.is_empty());
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("skills.lock");

        let mut lock = SkillsLock::default();
        lock.skills.push(LockEntry {
            name: "security".into(),
            source: "/skills/security.toml".into(),
            manifest_sha256: "aaaa".repeat(16),
            version: "1.0.0".into(),
            first_seen_at: Utc::now(),
            last_seen_at: Utc::now(),
            previous_manifest_sha256: None,
        });

        lock.save(&path).unwrap();
        let reloaded = SkillsLock::load(&path).unwrap();

        assert_eq!(reloaded.skills.len(), 1);
        assert_eq!(reloaded.skills[0].name, "security");
        assert_eq!(reloaded.skills[0].version, "1.0.0");
    }

    #[test]
    fn silent_edit_detection_same_version_different_hash() {
        let mut lock = SkillsLock {
            skills: vec![LockEntry {
                name: "security".into(),
                source: "/skills/security.toml".into(),
                manifest_sha256: "old_hash_aaa".into(),
                version: "1.0.0".into(),
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
                previous_manifest_sha256: None,
            }],
        };

        let loaded = [sample_loaded_skill("security", "1.0.0", "new_hash_bbb")];
        let warnings = lock.update(&loaded);

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].skill_name, "security");
        assert_eq!(warnings[0].previous_sha256, "old_hash_aaa");
        assert_eq!(warnings[0].current_sha256, "new_hash_bbb");
        assert_eq!(warnings[0].version, "1.0.0");
    }

    #[test]
    fn version_change_no_warning() {
        let mut lock = SkillsLock {
            skills: vec![LockEntry {
                name: "security".into(),
                source: "/skills/security.toml".into(),
                manifest_sha256: "old_hash".into(),
                version: "1.0.0".into(),
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
                previous_manifest_sha256: None,
            }],
        };

        let loaded = [sample_loaded_skill("security", "2.0.0", "new_hash")];
        let warnings = lock.update(&loaded);

        assert!(
            warnings.is_empty(),
            "version change should not produce a warning"
        );
        assert_eq!(lock.skills[0].version, "2.0.0");
        assert_eq!(lock.skills[0].manifest_sha256, "new_hash");
        assert_eq!(
            lock.skills[0].previous_manifest_sha256.as_deref(),
            Some("old_hash")
        );
    }

    #[test]
    fn new_skill_added_no_warning() {
        let mut lock = SkillsLock::default();

        let loaded = [sample_loaded_skill("security", "1.0.0", "sha_aaa")];
        let warnings = lock.update(&loaded);

        assert!(
            warnings.is_empty(),
            "new skill should not produce a warning"
        );
        assert_eq!(lock.skills.len(), 1);
        assert_eq!(lock.skills[0].name, "security");
        assert!(lock.skills[0].previous_manifest_sha256.is_none());
    }

    #[test]
    fn removed_skill_preserved_in_lock() {
        let mut lock = SkillsLock {
            skills: vec![
                LockEntry {
                    name: "security".into(),
                    source: "/skills/security.toml".into(),
                    manifest_sha256: "sha_sec".into(),
                    version: "1.0.0".into(),
                    first_seen_at: Utc::now(),
                    last_seen_at: Utc::now(),
                    previous_manifest_sha256: None,
                },
                LockEntry {
                    name: "performance".into(),
                    source: "/skills/performance.toml".into(),
                    manifest_sha256: "sha_perf".into(),
                    version: "1.0.0".into(),
                    first_seen_at: Utc::now(),
                    last_seen_at: Utc::now(),
                    previous_manifest_sha256: None,
                },
            ],
        };

        // Only "security" is loaded; "performance" is absent.
        let loaded = [sample_loaded_skill("security", "1.0.0", "sha_sec")];
        let warnings = lock.update(&loaded);

        assert!(warnings.is_empty());
        assert_eq!(
            lock.skills.len(),
            2,
            "absent skill should be preserved in lock"
        );
        assert!(lock.skills.iter().any(|e| e.name == "performance"));
    }

    #[test]
    fn previous_manifest_sha256_preserved_on_edit() {
        let mut lock = SkillsLock {
            skills: vec![LockEntry {
                name: "security".into(),
                source: "/skills/security.toml".into(),
                manifest_sha256: "hash_v1".into(),
                version: "1.0.0".into(),
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
                previous_manifest_sha256: None,
            }],
        };

        // First edit: version bump.
        let loaded_v2 = [sample_loaded_skill("security", "2.0.0", "hash_v2")];
        lock.update(&loaded_v2);
        assert_eq!(
            lock.skills[0].previous_manifest_sha256.as_deref(),
            Some("hash_v1")
        );

        // Second edit: another version bump.
        let loaded_v3 = [sample_loaded_skill("security", "3.0.0", "hash_v3")];
        lock.update(&loaded_v3);
        assert_eq!(
            lock.skills[0].previous_manifest_sha256.as_deref(),
            Some("hash_v2"),
            "previous_manifest_sha256 should track the immediately prior hash"
        );
    }

    #[test]
    fn unchanged_skill_no_warning_no_previous_hash_change() {
        let mut lock = SkillsLock {
            skills: vec![LockEntry {
                name: "security".into(),
                source: "/skills/security.toml".into(),
                manifest_sha256: "same_hash".into(),
                version: "1.0.0".into(),
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
                previous_manifest_sha256: None,
            }],
        };

        let loaded = [sample_loaded_skill("security", "1.0.0", "same_hash")];
        let warnings = lock.update(&loaded);

        assert!(
            warnings.is_empty(),
            "no change should not produce a warning"
        );
        assert!(
            lock.skills[0].previous_manifest_sha256.is_none(),
            "unchanged skill should not set previous_manifest_sha256"
        );
    }

    #[test]
    fn lock_save_creates_parent_directories() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("deep").join("nested").join("skills.lock");

        let lock = SkillsLock::default();
        lock.save(&path).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn lock_multiple_skills_update() {
        let mut lock = SkillsLock::default();

        let loaded = [
            sample_loaded_skill("security", "1.0.0", "sha_sec"),
            sample_loaded_skill("performance", "1.0.0", "sha_perf"),
            sample_loaded_skill("correctness", "1.0.0", "sha_corr"),
        ];
        let warnings = lock.update(&loaded);

        assert!(warnings.is_empty());
        assert_eq!(lock.skills.len(), 3);
    }

    // ── Serde snake_case verification ───────────────────────────────────

    #[test]
    fn enum_serde_uses_snake_case() {
        // Verify the serde representation matches the spec.
        assert_eq!(
            serde_json::to_string(&AxisSelectionSource::ExplicitAxes).unwrap(),
            "\"explicit_axes\""
        );
        assert_eq!(
            serde_json::to_string(&AxisSelectionSource::AutoDiscovery).unwrap(),
            "\"auto_discovery\""
        );
        assert_eq!(serde_json::to_string(&ExitStatus::Ok).unwrap(), "\"ok\"");
        assert_eq!(
            serde_json::to_string(&FailureReason::ModelRateLimit).unwrap(),
            "\"model_rate_limit\""
        );
        assert_eq!(
            serde_json::to_string(&IntegratorDecision::PassThrough).unwrap(),
            "\"pass_through\""
        );
    }
}
