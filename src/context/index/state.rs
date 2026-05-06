use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a per-call unique suffix for the atomic-write tempfile so two
/// concurrent `IndexState::save` calls on the same path don't race on a
/// shared `*.tmp` file.
fn unique_tmp_suffix() -> String {
    let pid = process::id();
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{pid}-{nanos}-{n}.tmp")
}

/// Versioning + model tracking state written alongside `index.db`.
///
/// The optional `head_sha` and `indexed_at` fields are populated by the
/// `quorum context index` / `refresh` handlers so they can detect whether a
/// source has moved since the last build. They default to `None` when absent
/// (older state files without these fields still load cleanly).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexState {
    pub schema_version: u32,
    pub embedder_model_hash: String,
    pub quorum_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<DateTime<Utc>>,
}

impl IndexState {
    pub fn new(embedder_model_hash: String) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            embedder_model_hash,
            quorum_version: env!("CARGO_PKG_VERSION").to_string(),
            head_sha: None,
            indexed_at: None,
        }
    }

    /// Chainable setter for the current git HEAD sha (None for path sources).
    #[must_use]
    pub fn with_head_sha(mut self, head_sha: Option<String>) -> Self {
        self.head_sha = head_sha;
        self
    }

    /// Chainable setter for the indexed-at timestamp.
    #[must_use]
    pub fn with_indexed_at(mut self, ts: DateTime<Utc>) -> Self {
        self.indexed_at = Some(ts);
        self
    }

    /// Load from the given path. Returns `Ok(None)` if the file doesn't exist,
    /// an error if the file exists but can't be parsed.
    pub fn load(path: &Path) -> Result<Option<Self>, StateError> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let state: Self = serde_json::from_slice(&bytes)?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StateError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Write the state atomically (write to temp file, rename).
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| StateError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = {
            let mut t = path.as_os_str().to_os_string();
            t.push(".");
            t.push(unique_tmp_suffix());
            PathBuf::from(t)
        };
        std::fs::write(&tmp, json.as_bytes()).map_err(|source| StateError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| StateError::Io { path: tmp, source })?;
        Ok(())
    }

    pub fn check_against(on_disk: Option<&Self>, expected_model_hash: &str) -> StateCheck {
        match on_disk {
            None => StateCheck::Fresh,
            Some(s) if s.schema_version != CURRENT_SCHEMA_VERSION => {
                StateCheck::SchemaMigrationRequired {
                    on_disk: s.schema_version,
                    expected: CURRENT_SCHEMA_VERSION,
                }
            }
            Some(s) if s.embedder_model_hash != expected_model_hash => {
                StateCheck::ReembedRequired {
                    on_disk: s.embedder_model_hash.clone(),
                    expected: expected_model_hash.to_string(),
                }
            }
            Some(_) => StateCheck::Ok,
        }
    }
}

/// Outcome of comparing the on-disk state against current runtime expectations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateCheck {
    Fresh,
    Ok,
    SchemaMigrationRequired { on_disk: u32, expected: u32 },
    ReembedRequired { on_disk: String, expected: String },
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("failed to read state at {path}: {source}", path = path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse state: {0}")]
    Parse(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn concurrent_saves_dont_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("state.json"));

        let mut handles = Vec::new();
        let mut expected_hashes = Vec::new();
        for i in 0..8 {
            let hash = format!("model-hash-{i}");
            expected_hashes.push(hash.clone());
            let p = Arc::clone(&path);
            handles.push(thread::spawn(move || {
                let state = IndexState::new(hash);
                state.save(&p).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let loaded = IndexState::load(&path)
            .expect("load must not see a corrupted/partial write")
            .expect("file exists");
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(
            expected_hashes.contains(&loaded.embedder_model_hash),
            "loaded hash {:?} not one of the writes {:?}",
            loaded.embedder_model_hash,
            expected_hashes,
        );

        // No leftover *.tmp siblings (all renames must have consumed them).
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("state.json."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "unexpected leftover tempfiles: {:?}",
            leftovers.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let s = IndexState::new("abc123".into());
        s.save(&path).unwrap();
        let loaded = IndexState::load(&path).unwrap().unwrap();
        assert_eq!(loaded, s);
    }
}
