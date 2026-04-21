use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Versioning + model tracking state written alongside `index.db`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexState {
    pub schema_version: u32,
    pub embedder_model_hash: String,
    pub quorum_version: String,
}

impl IndexState {
    pub fn new(embedder_model_hash: String) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            embedder_model_hash,
            quorum_version: env!("CARGO_PKG_VERSION").to_string(),
        }
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
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|e| e.to_str()).unwrap_or("")
        ));
        std::fs::write(&tmp, json.as_bytes()).map_err(|source| StateError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| StateError::Io {
            path: tmp,
            source,
        })?;
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
            Some(s) if s.embedder_model_hash != expected_model_hash => StateCheck::ReembedRequired {
                on_disk: s.embedder_model_hash.clone(),
                expected: expected_model_hash.to_string(),
            },
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
