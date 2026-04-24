/// Feedback storage: JSONL file for recording TP/FP verdicts on findings.
///
/// Verdict on-disk schema (backward-compatible):
/// - Unit variants serialize as bare strings: "tp", "fp", "partial", "wontfix".
/// - Struct variant `ContextMisleading` serializes as an externally-tagged
///   object: `{"context_misleading": {"blamed_chunk_ids": ["c1", "c2"]}}`.
///
/// Legacy entries without the struct variant continue to deserialize unchanged.

use std::path::PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Tp,
    Fp,
    Partial,
    Wontfix,
    /// Recorded when the injected retrieval context misled the reviewer.
    /// `blamed_chunk_ids` may be empty (defaults to "last-injected" downstream).
    ContextMisleading { blamed_chunk_ids: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Human,
    PostFix,                // verdict recorded after applying a fix (strongest signal)
    AutoCalibrate(String),  // model name used for auto-calibration
    /// Verdict from another review agent (pal, third-opinion, gemini, reviewdog, ...).
    /// Calibrator weight: 0.7x (see calibrator.rs). `confidence` is stored but
    /// IGNORED by the calibrator in v1 — reserved for future confidence-aware weighting.
    External {
        agent: String,
        model: Option<String>,
        confidence: Option<f32>,
    },
    Unknown,
}

impl Default for Provenance {
    fn default() -> Self {
        Provenance::Unknown
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub file_path: String,
    pub finding_title: String,
    pub finding_category: String,
    pub verdict: Verdict,
    pub reason: String,
    pub model: Option<String>,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub provenance: Provenance,
}

/// Input for recording a verdict from an external review agent.
///
/// Use `FeedbackStore::record_external` instead of constructing a `FeedbackEntry`
/// directly — it handles agent-name normalization, confidence clamping, and
/// timestamp assignment. See issue #32.
#[derive(Debug, Clone)]
pub struct ExternalVerdictInput {
    pub file_path: String,
    pub finding_title: String,
    pub finding_category: Option<String>,
    pub verdict: Verdict,
    pub reason: String,
    pub agent: String,
    pub agent_model: Option<String>,
    pub confidence: Option<f32>,
}

/// Clamp confidence to [0,1], mapping NaN/±Inf to None.
/// `f32::clamp` is NOT NaN-safe — this wraps it with an `is_finite` gate
/// (quorum self-review 2026-04-24).
pub(crate) fn clamp_confidence(c: Option<f32>) -> Option<f32> {
    c.filter(|x| x.is_finite()).map(|x| x.clamp(0.0, 1.0))
}

/// Normalize an agent name: trim + lowercase. Returns Err for empty-after-trim.
pub(crate) fn normalize_agent(raw: &str) -> anyhow::Result<String> {
    let t = raw.trim();
    if t.is_empty() {
        anyhow::bail!("agent name cannot be empty after normalization");
    }
    Ok(t.to_lowercase())
}

/// Summary of a single `drain_inbox` call. `processed_dir_total_bytes` is
/// the CUMULATIVE size of `processed_dir` after drain (drives the 50MB warning),
/// NOT the per-run size (quorum self-review 2026-04-24).
#[derive(Debug, Clone, Default)]
pub struct DrainReport {
    pub drained_files: usize,
    pub entries: usize,
    pub errors: Vec<DrainError>,
    pub processed_dir_total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct DrainError {
    pub file: PathBuf,
    /// Line number (1-indexed). `0` = file-level error (read/rename failure).
    pub line: usize,
    pub message: String,
}

/// Wire format that agents drop into `~/.quorum/inbox/*.jsonl`.
/// Structurally mirrors `ExternalVerdictInput` — kept as a separate type so
/// the on-disk schema can evolve independently of the in-memory DTO.
#[derive(Debug, Deserialize)]
struct ExternalVerdictInputWire {
    file_path: String,
    finding_title: String,
    finding_category: Option<String>,
    verdict: Verdict,
    reason: String,
    agent: String,
    agent_model: Option<String>,
    confidence: Option<f32>,
}

impl From<ExternalVerdictInputWire> for ExternalVerdictInput {
    fn from(w: ExternalVerdictInputWire) -> Self {
        Self {
            file_path: w.file_path,
            finding_title: w.finding_title,
            finding_category: w.finding_category,
            verdict: w.verdict,
            reason: w.reason,
            agent: w.agent,
            agent_model: w.agent_model,
            confidence: w.confidence,
        }
    }
}

/// Rename `src` to `dst`. Returns `Ok(true)` on success, `Ok(false)` if the
/// source disappeared between enumeration and rename (benign multi-process
/// race — another process claimed it first). Any other IO error propagates.
/// Extracted so the ENOENT arm is directly unit-testable.
pub(crate) fn rename_or_tolerate_race(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<bool> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub struct FeedbackStore {
    path: PathBuf,
}

impl FeedbackStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn record(&self, entry: &FeedbackEntry) -> anyhow::Result<()> {
        use anyhow::Context;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open feedback file: {}", self.path.display()))?;
        let mut buf = serde_json::to_string(entry)?;
        buf.push('\n');
        file.write_all(buf.as_bytes())?;
        Ok(())
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<FeedbackEntry>> {
        use anyhow::Context;
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read feedback file: {}", self.path.display()))?;
        let mut entries = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(entry) => entries.push(entry),
                Err(_) => continue, // skip malformed entries (e.g. from older formats)
            }
        }
        Ok(entries)
    }

    pub fn query_by_verdict(&self, verdict: &Verdict) -> anyhow::Result<Vec<FeedbackEntry>> {
        Ok(self
            .load_all()?
            .into_iter()
            .filter(|e| e.verdict == *verdict)
            .collect())
    }

    pub fn count(&self) -> anyhow::Result<usize> {
        Ok(self.load_all()?.len())
    }

    /// Drain all `*.jsonl` files from `inbox_dir` into this store as External
    /// verdicts. Claim-then-ingest: atomically rename each file into an
    /// `<inbox>/processing/` claim-path FIRST (making ownership exclusive so
    /// concurrent quorum processes don't double-ingest), then parse and
    /// `record_external` each line, then archive to `<processed_dir>/` with
    /// a ULID suffix. Non-ENOENT errors leave the file in `processing/` for
    /// operator inspection — never silent duplicate writes (quorum self-review
    /// 2026-04-24).
    ///
    /// Malformed lines land in `DrainReport.errors` and are skipped; the rest
    /// of the file still drains. ENOENT on any rename is treated as a benign
    /// race (another process got it first).
    ///
    /// `processed_dir_total_bytes` in the returned report is cumulative
    /// (drives the 50MB warning threshold).
    pub fn drain_inbox(
        &self,
        inbox_dir: &std::path::Path,
        processed_dir: &std::path::Path,
    ) -> anyhow::Result<DrainReport> {
        use std::io::ErrorKind;
        let mut report = DrainReport::default();

        // Fast path: ENOENT → zero work. Empty dir yields an empty iterator.
        let read = match std::fs::read_dir(inbox_dir) {
            Ok(r) => r,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(e.into()),
        };

        let mut files: Vec<PathBuf> = read
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .filter(|p| !p.is_dir())
            .collect();
        files.sort();

        if files.is_empty() {
            return Ok(report);
        }

        // Claim-then-ingest: create processing/ (sibling of files in inbox)
        // and processed/ lazily.
        let processing_dir = inbox_dir.join("processing");
        std::fs::create_dir_all(&processing_dir)?;
        std::fs::create_dir_all(processed_dir)?;

        for file in files {
            // STEP A: CLAIM. Atomic rename into processing/. ENOENT → another
            // process already claimed it; skip.
            let fname = file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown.jsonl");
            let claim_ulid = ulid::Ulid::new().to_string();
            let claimed = processing_dir.join(format!("{fname}.{claim_ulid}.jsonl"));
            match rename_or_tolerate_race(&file, &claimed) {
                Ok(true) => { /* we exclusively own the file now */ }
                Ok(false) => continue,
                Err(e) => {
                    report.errors.push(DrainError {
                        file: file.clone(),
                        line: 0,
                        message: format!("claim rename failed: {e}"),
                    });
                    continue;
                }
            }

            // STEP B: INGEST from the claimed path.
            let content = match std::fs::read_to_string(&claimed) {
                Ok(c) => c,
                Err(e) => {
                    report.errors.push(DrainError {
                        file: claimed.clone(),
                        line: 0,
                        message: format!("read failed: {e}"),
                    });
                    // Leave in processing/ for operator inspection.
                    continue;
                }
            };
            for (idx, line) in content.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<ExternalVerdictInputWire>(line) {
                    Ok(wire) => {
                        let input: ExternalVerdictInput = wire.into();
                        if let Err(e) = self.record_external(input) {
                            report.errors.push(DrainError {
                                file: claimed.clone(),
                                line: idx + 1,
                                message: format!("record failed: {e}"),
                            });
                        } else {
                            report.entries += 1;
                        }
                    }
                    Err(e) => {
                        report.errors.push(DrainError {
                            file: claimed.clone(),
                            line: idx + 1,
                            message: format!("parse failed: {e}"),
                        });
                    }
                }
            }

            // STEP C: ARCHIVE. Move from processing/ to processed/.
            let archive_ulid = ulid::Ulid::new().to_string();
            let target = processed_dir.join(format!("{fname}.{archive_ulid}.jsonl"));
            match rename_or_tolerate_race(&claimed, &target) {
                Ok(true) => report.drained_files += 1,
                Ok(false) => { /* unlikely — someone unlinked processing/ under us */ }
                Err(e) => {
                    report.errors.push(DrainError {
                        file: claimed.clone(),
                        line: 0,
                        message: format!(
                            "archive rename failed: {e}; file left in processing/"
                        ),
                    });
                    // File stays in processing/ — operator resolves manually.
                }
            }
        }

        // Size-warning threshold (cumulative total of processed_dir).
        const WARN_BYTES: u64 = 50 * 1024 * 1024;
        if let Ok(entries) = std::fs::read_dir(processed_dir) {
            let total: u64 = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum();
            report.processed_dir_total_bytes = total;
            if total > WARN_BYTES {
                tracing::warn!(
                    processed_dir = %processed_dir.display(),
                    total_mb = total / 1024 / 1024,
                    "quorum inbox processed/ is large; consider manual cleanup"
                );
            }
        }

        Ok(report)
    }

    /// Record a verdict from an external review agent (pal, third-opinion, etc.).
    /// Normalizes `agent` (trim + lowercase), NaN-safe clamps `confidence` to
    /// [0,1], defaults missing `finding_category` to `"unknown"`, rejects
    /// `Verdict::ContextMisleading` (retrieval signals need blamed_chunk_ids
    /// an external agent can't credibly produce), and sets
    /// `provenance = Provenance::External {..}`. See issue #32.
    pub fn record_external(&self, input: ExternalVerdictInput) -> anyhow::Result<()> {
        if matches!(input.verdict, Verdict::ContextMisleading { .. }) {
            anyhow::bail!(
                "context_misleading verdicts are not accepted from External agents \
                 (they cannot identify blamed chunks in our injected context)"
            );
        }
        let agent = normalize_agent(&input.agent)?;
        let confidence = clamp_confidence(input.confidence);
        let category = input
            .finding_category
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let entry = FeedbackEntry {
            file_path: input.file_path,
            finding_title: input.finding_title,
            finding_category: category,
            verdict: input.verdict,
            reason: input.reason,
            model: None,
            timestamp: Utc::now(),
            provenance: Provenance::External {
                agent,
                model: input.agent_model,
                confidence,
            },
        };
        self.record(&entry)
    }

    /// Record a `ContextMisleading` verdict — reviewer determined the injected
    /// retrieval context was wrong or misleading. `blamed_chunk_ids` may be
    /// empty; callers (e.g. the CLI in task 8.2) supply a sensible default.
    pub fn record_context_misleading(
        &self,
        file: impl Into<String>,
        finding_title: impl Into<String>,
        finding_category: impl Into<String>,
        blamed_chunk_ids: Vec<String>,
        reason: impl Into<String>,
    ) -> anyhow::Result<()> {
        let entry = FeedbackEntry {
            file_path: file.into(),
            finding_title: finding_title.into(),
            finding_category: finding_category.into(),
            verdict: Verdict::ContextMisleading { blamed_chunk_ids },
            reason: reason.into(),
            model: None,
            timestamp: Utc::now(),
            provenance: Provenance::Human,
        };
        self.record(&entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (FeedbackStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        (FeedbackStore::new(path), dir)
    }

    fn sample_entry(verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "src/auth.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict,
            reason: "Fixed with parameterized query".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: Provenance::Unknown,
        }
    }

    #[test]
    fn record_and_load_single_entry() {
        let (store, _dir) = test_store();
        let entry = sample_entry(Verdict::Tp);
        store.record(&entry).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].finding_title, "SQL injection");
        assert_eq!(all[0].verdict, Verdict::Tp);
    }

    #[test]
    fn record_multiple_entries() {
        let (store, _dir) = test_store();
        store.record(&sample_entry(Verdict::Tp)).unwrap();
        store.record(&sample_entry(Verdict::Fp)).unwrap();
        store.record(&sample_entry(Verdict::Partial)).unwrap();
        assert_eq!(store.count().unwrap(), 3);
    }

    #[test]
    fn query_by_verdict() {
        let (store, _dir) = test_store();
        store.record(&sample_entry(Verdict::Tp)).unwrap();
        store.record(&sample_entry(Verdict::Fp)).unwrap();
        store.record(&sample_entry(Verdict::Tp)).unwrap();
        let tps = store.query_by_verdict(&Verdict::Tp).unwrap();
        assert_eq!(tps.len(), 2);
        let fps = store.query_by_verdict(&Verdict::Fp).unwrap();
        assert_eq!(fps.len(), 1);
    }

    #[test]
    fn load_empty_file() {
        let (store, _dir) = test_store();
        let all = store.load_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn count_empty() {
        let (store, _dir) = test_store();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn entries_persist_across_instances() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");

        let store1 = FeedbackStore::new(path.clone());
        store1.record(&sample_entry(Verdict::Tp)).unwrap();
        drop(store1);

        let store2 = FeedbackStore::new(path);
        assert_eq!(store2.count().unwrap(), 1);
    }

    #[test]
    fn feedback_entry_has_provenance_field() {
        let entry = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "Real bug".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: Provenance::Human,
        };
        assert_eq!(entry.provenance, Provenance::Human);
    }

    #[test]
    fn legacy_entries_without_provenance_default_to_unknown() {
        let json = r#"{"file_path":"test.rs","finding_title":"Bug","finding_category":"security","verdict":"tp","reason":"test","model":"gpt-5.4","timestamp":"2026-01-01T00:00:00Z"}"#;
        let entry: FeedbackEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.provenance, Provenance::Unknown);
    }

    #[test]
    fn provenance_serializes_correctly() {
        assert_eq!(serde_json::to_value(&Provenance::Human).unwrap(), "human");
        assert_eq!(serde_json::to_value(&Provenance::PostFix).unwrap(), "post_fix");
    }

    #[test]
    fn verdict_serializes_as_lowercase() {
        assert_eq!(serde_json::to_value(&Verdict::Tp).unwrap(), "tp");
        assert_eq!(serde_json::to_value(&Verdict::Fp).unwrap(), "fp");
        assert_eq!(serde_json::to_value(&Verdict::Partial).unwrap(), "partial");
        assert_eq!(serde_json::to_value(&Verdict::Wontfix).unwrap(), "wontfix");
    }

    #[test]
    fn feedback_records_context_misleading_with_chunk_ids() {
        let (store, _dir) = test_store();
        store
            .record_context_misleading(
                "src/retriever.rs",
                "Stale API reference",
                "correctness",
                vec!["chunk-abc".into(), "chunk-def".into()],
                "Injected docs described v1, code uses v2",
            )
            .unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].verdict {
            Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert_eq!(blamed_chunk_ids, &vec!["chunk-abc".to_string(), "chunk-def".to_string()]);
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
        }
        assert_eq!(all[0].file_path, "src/retriever.rs");
        assert_eq!(all[0].finding_title, "Stale API reference");
        assert_eq!(
            all[0].finding_category, "correctness",
            "finding_category must round-trip, not be hardcoded empty"
        );
        assert_eq!(all[0].provenance, Provenance::Human);
    }

    #[test]
    fn legacy_verdicts_still_load_after_adding_context_misleading() {
        // Entries written before the ContextMisleading variant existed must still parse.
        let legacy = r#"{"file_path":"a.rs","finding_title":"X","finding_category":"security","verdict":"tp","reason":"r","model":"m","timestamp":"2026-01-01T00:00:00Z","provenance":"human"}
{"file_path":"b.rs","finding_title":"Y","finding_category":"style","verdict":"fp","reason":"r","model":"m","timestamp":"2026-01-02T00:00:00Z"}
{"file_path":"c.rs","finding_title":"Z","finding_category":"security","verdict":"partial","reason":"r","model":"m","timestamp":"2026-01-03T00:00:00Z","provenance":"post_fix"}
{"file_path":"d.rs","finding_title":"W","finding_category":"style","verdict":"wontfix","reason":"r","model":"m","timestamp":"2026-01-04T00:00:00Z","provenance":"human"}
"#;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        std::fs::write(&path, legacy).unwrap();
        let store = FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].verdict, Verdict::Tp);
        assert_eq!(all[1].verdict, Verdict::Fp);
        assert_eq!(all[2].verdict, Verdict::Partial);
        assert_eq!(all[3].verdict, Verdict::Wontfix);
    }

    #[test]
    fn context_misleading_with_empty_chunk_ids_roundtrips() {
        let (store, _dir) = test_store();
        store
            .record_context_misleading(
                "src/foo.rs",
                "No chunks blamed",
                "",
                vec![],
                "Reviewer did not identify specific chunks",
            )
            .unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].verdict {
            Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert!(blamed_chunk_ids.is_empty());
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
        }
    }

    // --- Task 1: External provenance variant (issue #32) ---

    #[test]
    fn external_variant_roundtrips_through_jsonl() {
        let (store, _dir) = test_store();
        let entry = FeedbackEntry {
            file_path: "src/auth.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "Confirmed".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: Provenance::External {
                agent: "pal".into(),
                model: Some("gemini-3-pro-preview".into()),
                confidence: Some(0.9),
            },
        };
        store.record(&entry).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].provenance {
            Provenance::External { agent, model, confidence } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gemini-3-pro-preview"));
                assert_eq!(*confidence, Some(0.9));
            }
            other => panic!("expected External, got {:?}", other),
        }
    }

    #[test]
    fn external_serializes_with_external_tag() {
        let p = Provenance::External {
            agent: "pal".into(),
            model: Some("gpt-5.4".into()),
            confidence: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        // Externally tagged: {"external": {...}}
        assert!(v.get("external").is_some(), "expected 'external' tag, got {v}");
        let inner = v.get("external").unwrap();
        assert_eq!(inner.get("agent").and_then(|x| x.as_str()), Some("pal"));
        // `confidence: None` may serialize as `null` OR be absent (if serde is
        // later configured with skip_serializing_if). Both are valid wire forms.
        assert!(inner.get("confidence").map_or(true, |c| c.is_null()),
            "confidence must be null or absent, got {:?}", inner.get("confidence"));
    }

    #[test]
    fn external_deserializes_when_confidence_key_absent() {
        // Agents may omit the confidence key entirely. Must round-trip to
        // Provenance::External { confidence: None, .. }.
        let json = r#"{"external":{"agent":"pal","model":"gpt-5.4"}}"#;
        let p: Provenance = serde_json::from_str(json).unwrap();
        match p {
            Provenance::External { agent, model, confidence } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gpt-5.4"));
                assert_eq!(confidence, None);
            }
            o => panic!("{o:?}"),
        }
    }

    // --- Task 4: ExternalVerdictInput + record_external (issue #32) ---

    #[test]
    fn clamp_confidence_maps_values() {
        assert_eq!(clamp_confidence(None), None);
        assert_eq!(clamp_confidence(Some(0.42)), Some(0.42));
        assert_eq!(clamp_confidence(Some(1.5)), Some(1.0));
        assert_eq!(clamp_confidence(Some(-0.2)), Some(0.0));
        assert_eq!(clamp_confidence(Some(0.0)), Some(0.0));
        assert_eq!(clamp_confidence(Some(1.0)), Some(1.0));
    }

    #[test]
    fn clamp_confidence_rejects_nan_inf() {
        // f32::clamp is NOT NaN-safe — it returns NaN for NaN input.
        // clamp_confidence must detect non-finite values explicitly.
        assert_eq!(clamp_confidence(Some(f32::NAN)), None, "NaN must become None");
        assert_eq!(clamp_confidence(Some(f32::INFINITY)), None, "+inf must become None");
        assert_eq!(clamp_confidence(Some(f32::NEG_INFINITY)), None, "-inf must become None");
    }

    #[test]
    fn record_external_writes_external_provenance() {
        let (store, _dir) = test_store();
        let input = ExternalVerdictInput {
            file_path: "src/a.rs".into(),
            finding_title: "Bug".into(),
            finding_category: Some("security".into()),
            verdict: Verdict::Tp,
            reason: "confirmed".into(),
            agent: "pal".into(),
            agent_model: Some("gemini-3-pro-preview".into()),
            confidence: Some(0.85),
        };
        store.record_external(input).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].provenance {
            Provenance::External { agent, model, confidence } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gemini-3-pro-preview"));
                assert_eq!(*confidence, Some(0.85));
            }
            o => panic!("expected External, got {o:?}"),
        }
        assert!(all[0].model.is_none(), "entry.model should be None (reviewer model, not agent model)");
    }

    #[test]
    fn record_external_normalizes_agent_name() {
        let (store, _dir) = test_store();
        store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "  PaL  ".into(),
            agent_model: None,
            confidence: None,
        }).unwrap();
        let all = store.load_all().unwrap();
        match &all[0].provenance {
            Provenance::External { agent, .. } => assert_eq!(agent, "pal"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn record_external_rejects_empty_agent() {
        let (store, _dir) = test_store();
        let err = store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "   ".into(),
            agent_model: None,
            confidence: None,
        }).expect_err("empty agent must be rejected");
        assert!(err.to_string().to_lowercase().contains("agent"),
            "error message should mention agent: {err}");
    }

    #[test]
    fn record_external_defaults_missing_category_to_unknown() {
        let (store, _dir) = test_store();
        store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "pal".into(),
            agent_model: None,
            confidence: None,
        }).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all[0].finding_category, "unknown");
    }

    #[test]
    fn record_external_applies_clamp_confidence() {
        let (store, _dir) = test_store();
        store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "pal".into(),
            agent_model: None,
            confidence: Some(1.5),
        }).unwrap();
        let all = store.load_all().unwrap();
        match &all[0].provenance {
            Provenance::External { confidence, .. } => {
                assert_eq!(*confidence, Some(1.0), "1.5 must clamp to 1.0");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn record_external_rejects_context_misleading_verdict() {
        // ContextMisleading needs blamed_chunk_ids that the reviewer sees in our
        // injected context — an external agent can't credibly produce them.
        // Reject at ingest to avoid polluting the calibrator's retrieval signal.
        let (store, _dir) = test_store();
        let err = store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::ContextMisleading { blamed_chunk_ids: vec!["c1".into()] },
            reason: "r".into(),
            agent: "pal".into(),
            agent_model: None,
            confidence: None,
        }).expect_err("ContextMisleading must be rejected for External provenance");
        assert!(
            err.to_string().to_lowercase().contains("context"),
            "error message must mention context_misleading: {err}"
        );
    }

    #[test]
    fn normalize_agent_idempotent_on_typical_input() {
        assert_eq!(normalize_agent("pal").unwrap(), "pal");
        assert_eq!(normalize_agent("  Pal  ").unwrap(), "pal");
        assert_eq!(normalize_agent("THIRD-OPINION").unwrap(), "third-opinion");
        assert!(normalize_agent("").is_err());
        assert!(normalize_agent("   ").is_err());
        // Idempotence: normalize(normalize(s)) == normalize(s)
        let once = normalize_agent("  MixedCase  ").unwrap();
        let twice = normalize_agent(&once).unwrap();
        assert_eq!(once, twice);
    }

    // --- Task 5: drain_inbox with claim-then-ingest (issue #32) ---

    #[test]
    fn drain_inbox_missing_dir_returns_zero_work() {
        // Inbox dir doesn't exist — first-run scenario. Must NOT error.
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("nonexistent-inbox");
        let processed = dir.path().join("processed");
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 0);
        assert_eq!(report.entries, 0);
        assert!(report.errors.is_empty());
        assert!(!processed.exists(), "processed/ must not be created when inbox is absent");
    }

    #[test]
    fn drain_inbox_empty_returns_zero_work() {
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 0);
        assert_eq!(report.entries, 0);
        assert!(report.errors.is_empty());
        assert_eq!(report.processed_dir_total_bytes, 0);
        assert!(!processed.exists(), "processed/ should not be created when inbox is empty");
    }

    #[test]
    fn drain_inbox_valid_file_appends_and_moves() {
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let line = serde_json::to_string(&serde_json::json!({
            "file_path": "src/a.rs",
            "finding_title": "Bug",
            "finding_category": "security",
            "verdict": "tp",
            "reason": "confirmed",
            "agent": "pal",
            "agent_model": "gemini-3-pro-preview",
            "confidence": 0.9
        })).unwrap();
        let inbox_file = inbox.join("pal-run-1.jsonl");
        std::fs::write(&inbox_file, format!("{line}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 1);
        assert_eq!(report.entries, 1);
        assert!(report.errors.is_empty());

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(matches!(all[0].provenance, Provenance::External { .. }));

        assert!(!inbox_file.exists(), "inbox file should be moved after drain");

        let processed_files: Vec<_> = std::fs::read_dir(&processed).unwrap().collect::<Result<_,_>>().unwrap();
        assert_eq!(processed_files.len(), 1);
        let name = processed_files[0].file_name().into_string().unwrap();
        assert!(name.starts_with("pal-run-1.jsonl."), "expected ulid-suffixed name, got {name}");
        assert!(name.ends_with(".jsonl"));

        // Claim-then-ingest invariant: processing/ must be empty on success.
        let processing = inbox.join("processing");
        if processing.exists() {
            let leftover: Vec<_> = std::fs::read_dir(&processing).unwrap().collect::<Result<_,_>>().unwrap();
            assert!(leftover.is_empty(),
                "processing/ must be empty after successful drain, found {:?}",
                leftover.iter().map(|e| e.path()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn drain_inbox_malformed_line_skipped_rest_drained() {
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let good = serde_json::to_string(&serde_json::json!({
            "file_path": "src/a.rs",
            "finding_title": "Bug",
            "finding_category": "security",
            "verdict": "tp",
            "reason": "r",
            "agent": "pal",
            "agent_model": null,
            "confidence": null
        })).unwrap();
        let bad = "{not json";
        std::fs::write(inbox.join("mix.jsonl"), format!("{good}\n{bad}\n{good}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 1);
        assert_eq!(report.entries, 2, "2 good + 1 bad = 2 appended");
        assert_eq!(report.errors.len(), 1);

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn drain_inbox_is_idempotent_on_empty_second_call() {
        // Honest name: this tests idempotency, not multi-process ENOENT races.
        // ENOENT tolerance is tested directly via rename_or_tolerate_race below.
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let line = r#"{"file_path":"a.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
        std::fs::write(inbox.join("a.jsonl"), format!("{line}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let r1 = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(r1.drained_files, 1);
        let r2 = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(r2.drained_files, 0, "second drain is a no-op, not an error");
    }

    #[test]
    fn rename_or_tolerate_race_swallows_nonexistent_source() {
        // Directly tests ENOENT-tolerance by calling the extracted seam
        // with a source path that doesn't exist. Proves the multi-process
        // race arm of drain_inbox without requiring actual concurrency.
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not-there.jsonl");
        std::fs::create_dir_all(dir.path().join("processed")).unwrap();
        let dst = dir.path().join("processed").join("moved.jsonl");
        let renamed = rename_or_tolerate_race(&missing, &dst).unwrap();
        assert!(!renamed, "missing source must return Ok(false), not Err");
        assert!(!dst.exists(), "destination must not be created");
    }

    #[test]
    fn drain_inbox_rejects_uppercase_verdict_string() {
        // Verdict must round-trip through #[serde(rename_all="snake_case")].
        // "TP" is not valid; the line lands in errors, valid line still drains.
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let bad = r#"{"file_path":"a.rs","finding_title":"t","finding_category":"c","verdict":"TP","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
        let good = r#"{"file_path":"b.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
        std::fs::write(inbox.join("mix.jsonl"), format!("{bad}\n{good}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 1);
        assert_eq!(report.entries, 1, "only the valid line was appended");
        assert_eq!(report.errors.len(), 1, "uppercase TP must land in errors");
        let msg = report.errors[0].message.to_lowercase();
        assert!(msg.contains("tp") || msg.contains("verdict") || msg.contains("unknown variant"),
            "error must reference the bad verdict: {}", report.errors[0].message);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn clamp_always_finite_and_in_unit_interval(c in any::<f32>()) {
            match clamp_confidence(Some(c)) {
                Some(out) => {
                    prop_assert!(out.is_finite(), "clamp output must be finite, got {out}");
                    prop_assert!((0.0..=1.0).contains(&out), "out={out} not in [0,1]");
                }
                None => prop_assert!(!c.is_finite(), "None only allowed for non-finite input, got {c}"),
            }
        }

        #[test]
        fn normalize_agent_is_idempotent(s in "[[:print:]]{0,64}") {
            let first = normalize_agent(&s);
            if let Ok(first_val) = first {
                let second = normalize_agent(&first_val)
                    .expect("normalized output should itself be valid input");
                prop_assert_eq!(first_val, second);
            }
        }

        #[test]
        fn normalize_agent_err_iff_trim_empty(s in "[[:print:]]{0,64}") {
            let is_err = normalize_agent(&s).is_err();
            let trimmed_empty = s.trim().is_empty();
            prop_assert_eq!(is_err, trimmed_empty,
                "err iff trim empty for input {:?}", s);
        }
    }
}
