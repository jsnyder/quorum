use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Symbol,
    Doc,
    Schema,
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
    pub source_path: PathBuf,
    pub line_range: (u32, u32),
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
    pub confidence: f32,
    pub source_uri: String,
}
