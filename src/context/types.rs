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
///
/// Construct via [`LineRange::new`]; fields are `pub(crate)` so existing
/// in-crate readers continue to work while external struct-literal
/// construction is forbidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawLineRange")]
pub struct LineRange {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum LineRangeError {
    #[error("line_range.start must be >= 1 (got {0})")]
    InvalidStart(u32),
    #[error("line_range.end must be >= 1 (got {0})")]
    InvalidEnd(u32),
    #[error("line_range.end ({end}) must be >= start ({start})")]
    EndBeforeStart { start: u32, end: u32 },
}

impl LineRange {
    /// Construct a validated 1-indexed inclusive line range.
    pub fn new(start: u32, end: u32) -> Result<Self, LineRangeError> {
        if start == 0 {
            return Err(LineRangeError::InvalidStart(start));
        }
        if end == 0 {
            return Err(LineRangeError::InvalidEnd(end));
        }
        if start > end {
            return Err(LineRangeError::EndBeforeStart { start, end });
        }
        Ok(Self { start, end })
    }

    #[inline]
    pub fn start(&self) -> u32 {
        self.start
    }

    #[inline]
    pub fn end(&self) -> u32 {
        self.end
    }
}

#[derive(Deserialize)]
struct RawLineRange {
    start: u32,
    end: u32,
}

impl TryFrom<RawLineRange> for LineRange {
    type Error = String;

    fn try_from(raw: RawLineRange) -> Result<Self, Self::Error> {
        if raw.start == 0 || raw.end == 0 {
            return Err("line ranges must be 1-indexed (got 0)".into());
        }
        if raw.start > raw.end {
            return Err(format!(
                "line range start ({}) must be <= end ({})",
                raw.start, raw.end
            ));
        }
        Ok(Self {
            start: raw.start,
            end: raw.end,
        })
    }
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
//
// Construct via [`Provenance::new`]; fields are `pub(crate)` so existing
// in-crate readers continue to work while external struct-literal
// construction is forbidden.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawProvenance")]
pub struct Provenance {
    pub(crate) extractor: String,
    /// Confidence score in the range [0.0, 1.0]. f32 is used (vs f64) to keep
    /// JSONL rows compact; precision beyond 6 digits is not meaningful here.
    pub(crate) confidence: f32,
    pub(crate) source_uri: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    #[error("provenance.confidence must be finite (got {0})")]
    NotFinite(f32),
    #[error("provenance.confidence must be in [0.0, 1.0] (got {0})")]
    OutOfRange(f32),
}

impl Provenance {
    /// Construct a validated provenance record.
    ///
    /// `confidence` must be finite and within `[0.0, 1.0]`.
    pub fn new(
        extractor: impl Into<String>,
        confidence: f32,
        source_uri: impl Into<String>,
    ) -> Result<Self, ProvenanceError> {
        if !confidence.is_finite() {
            return Err(ProvenanceError::NotFinite(confidence));
        }
        if !(0.0..=1.0).contains(&confidence) {
            return Err(ProvenanceError::OutOfRange(confidence));
        }
        Ok(Self {
            extractor: extractor.into(),
            confidence,
            source_uri: source_uri.into(),
        })
    }

    #[inline]
    pub fn extractor(&self) -> &str {
        &self.extractor
    }

    #[inline]
    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    #[inline]
    pub fn source_uri(&self) -> &str {
        &self.source_uri
    }
}

#[derive(Deserialize)]
struct RawProvenance {
    extractor: String,
    confidence: f32,
    source_uri: String,
}

impl TryFrom<RawProvenance> for Provenance {
    type Error = String;

    fn try_from(raw: RawProvenance) -> Result<Self, Self::Error> {
        if !raw.confidence.is_finite() {
            return Err(format!(
                "confidence must be finite (got {})",
                raw.confidence
            ));
        }
        if !(0.0..=1.0).contains(&raw.confidence) {
            return Err(format!(
                "confidence must be in [0.0, 1.0] (got {})",
                raw.confidence
            ));
        }
        Ok(Self {
            extractor: raw.extractor,
            confidence: raw.confidence,
            source_uri: raw.source_uri,
        })
    }
}
