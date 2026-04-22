use crate::context::types::ChunkKind;

/// Canonical retrieval filters shared across BM25, vector, and the
/// `Retriever` facade.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// If empty, no source restriction.
    pub sources: Vec<String>,
    /// If empty, no kind restriction.
    pub kinds: Vec<ChunkKind>,
    /// If non-empty, chunks whose `source_path` is in this list are
    /// excluded from retrieval. Used today to keep the file-under-review
    /// out of its own context block — otherwise similarity retrieval
    /// collapses the review target and the reference material. Under
    /// diff-scoped review (follow-up) the correct filter will drop to the
    /// chunk / qualified-name level so in-file callees remain valid
    /// context.
    pub exclude_source_paths: Vec<String>,
}

pub mod bm25;
pub mod identifiers;
pub mod precedence;
pub mod rerank;
pub mod retriever;
pub mod structural;
pub mod vector;

// Public re-exports for consumers of the retrieve module. Clippy's unused
// analysis treats these as dead in a binary crate; suppress that noise.
#[allow(unused_imports)]
pub use precedence::{resolve_precedence, PrecedenceLog, SourceWeights};
#[allow(unused_imports)]
pub use rerank::{RerankConfig, ScoreBreakdown};
#[allow(unused_imports)]
pub use retriever::{RetrievalQuery, Retriever, ScoredChunk};

#[cfg(test)]
mod bm25_tests;
#[cfg(test)]
mod identifiers_tests;
#[cfg(test)]
mod precedence_tests;
#[cfg(test)]
mod rerank_tests;
#[cfg(test)]
mod retriever_tests;
#[cfg(test)]
mod structural_tests;
#[cfg(test)]
mod vector_tests;
