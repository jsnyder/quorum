/// Feedback storage: JSONL file for recording TP/FP verdicts on findings.

use std::path::PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Tp,
    Fp,
    Partial,
    Wontfix,
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
}
