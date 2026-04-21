use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Symbol,
    Doc,
    Schema,
}

/// 1-indexed inclusive line range in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub source: String,
    pub kind: ChunkKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    pub content: String,
    pub metadata: ChunkMeta,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMeta {
    /// Path relative to the source root, using forward slashes for
    /// cross-platform stability in the on-disk JSONL schema.
    pub source_path: String,
    pub line_range: LineRange,
    pub commit_sha: String,
    pub indexed_at: DateTime<Utc>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    pub is_exported: bool,
    pub neighboring_symbols: Vec<String>,
}

// Note: confidence is f32, so Provenance derives PartialEq but NOT Eq.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub extractor: String,
    /// Confidence score in the range [0.0, 1.0]. f32 is used (vs f64) to keep
    /// JSONL rows compact; precision beyond 6 digits is not meaningful here.
    pub confidence: f32,
    pub source_uri: String,
}
