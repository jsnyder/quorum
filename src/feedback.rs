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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Human,
    PostFix,                // verdict recorded after applying a fix (strongest signal)
    AutoCalibrate(String),  // model name used for auto-calibration
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
}
