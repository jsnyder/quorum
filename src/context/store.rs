//! JSONL-backed append-only chunk store.
//!
//! MVP design: one line per [`Chunk`], append-only, no persistent file handle.
//! Source-level rebuilds are handled by the `IndexBuilder` layer (Task 3).

use super::types::Chunk;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Append-only JSONL writer/reader for chunks.
pub struct ChunkStore {
    path: PathBuf,
}

/// Lenient-load result: valid chunks plus per-line parse errors.
pub struct LoadReport {
    pub chunks: Vec<Chunk>,
    pub errors: Vec<LoadError>,
}

/// A single parse failure, with 1-indexed line number matching editor view.
#[derive(Debug)]
pub struct LoadError {
    pub line_number: usize,
    pub message: String,
}

/// Result of structural validation (pure, no I/O).
pub struct ValidationReport {
    pub errors: Vec<String>,
}

impl ValidationReport {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

impl ChunkStore {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// Append a chunk as a single JSONL line. Creates the file and any missing
    /// parent directories on first call. Opens in append mode per call — no
    /// persistent file handle is held.
    pub fn append(&mut self, chunk: &Chunk) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let line = serde_json::to_string(chunk)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    /// Strict load: the first malformed line aborts with an error.
    /// A missing file returns an empty Vec (not an error).
    pub fn load_all(path: &Path) -> io::Result<Vec<Chunk>> {
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };

        let mut chunks = Vec::new();
        for (idx, raw) in contents.split('\n').enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let chunk: Chunk = serde_json::from_str(line).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("line {}: {e}", idx + 1),
                )
            })?;
            chunks.push(chunk);
        }
        Ok(chunks)
    }

    /// Lenient load: collect malformed lines into `report.errors`, keep valid
    /// chunks in `report.chunks`. Blank lines are silently skipped. A missing
    /// file yields an empty report.
    pub fn load_all_lenient(path: &Path) -> io::Result<LoadReport> {
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadReport {
                    chunks: Vec::new(),
                    errors: Vec::new(),
                });
            }
            Err(e) => return Err(e),
        };

        let mut chunks = Vec::new();
        let mut errors = Vec::new();
        for (idx, raw) in contents.split('\n').enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Chunk>(line) {
                Ok(chunk) => chunks.push(chunk),
                Err(e) => errors.push(LoadError {
                    line_number: idx + 1,
                    message: e.to_string(),
                }),
            }
        }
        Ok(LoadReport { chunks, errors })
    }

    /// Validate structural invariants: no duplicate ids, no empty id/source/content.
    /// Pure function; does no I/O.
    pub fn validate(chunks: &[Chunk]) -> ValidationReport {
        let mut errors = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();

        for (idx, chunk) in chunks.iter().enumerate() {
            if chunk.id.is_empty() {
                errors.push(format!("chunk at index {idx}: empty id"));
            } else if !seen.insert(chunk.id.as_str()) {
                errors.push(format!("duplicate id: {}", chunk.id));
            }
            if chunk.source.is_empty() {
                errors.push(format!("chunk '{}': empty source", chunk.id));
            }
            if chunk.content.is_empty() {
                errors.push(format!("chunk '{}': empty content", chunk.id));
            }
        }

        ValidationReport { errors }
    }
}
