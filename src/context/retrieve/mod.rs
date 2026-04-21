use crate::context::types::ChunkKind;

/// Canonical retrieval filters shared across BM25, vector, and the
/// `Retriever` facade.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// If empty, no source restriction.
    pub sources: Vec<String>,
    /// If empty, no kind restriction.
    pub kinds: Vec<ChunkKind>,
}

pub mod bm25;
pub mod identifiers;
pub mod rerank;
pub mod retriever;
pub mod vector;

// Public re-exports for consumers of the retrieve module. Clippy's unused
// analysis treats these as dead in a binary crate; suppress that noise.
#[allow(unused_imports)]
pub use rerank::{RerankConfig, ScoreBreakdown};
#[allow(unused_imports)]
pub use retriever::{RetrievalQuery, Retriever, ScoredChunk};

#[cfg(test)]
mod bm25_tests;
#[cfg(test)]
mod identifiers_tests;
#[cfg(test)]
mod rerank_tests;
#[cfg(test)]
mod retriever_tests;
#[cfg(test)]
mod vector_tests;
