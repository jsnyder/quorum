//! High-level context injection facade: compose retrieve → precedence → plan
//! → render into a single `Option<String>` suitable for splicing into an LLM
//! review prompt.
//!
//! The facade is intentionally decoupled from the real fastembed / rusqlite
//! retriever stack: callers pass a boxed closure that produces
//! `Vec<ScoredChunk>` so tests can inject fakes.

use std::sync::Arc;

use crate::context::config::{ContextConfig, SourcesConfig};
use crate::context::inject::plan::plan_injection;
use crate::context::inject::render::render_context_block;
use crate::context::inject::stale::NoStaleness;
use crate::context::retrieve::{
    resolve_precedence, Filters, PrecedenceLog, RetrievalQuery, ScoredChunk, SourceWeights,
};
use crate::context::types::ChunkKind;

/// Request shape that the review pipeline passes to the injector.
#[derive(Debug, Clone)]
pub struct InjectionRequest {
    pub file_path: String,
    pub language: Option<String>,
    /// Identifiers harvested from the file under review (callee names, type
    /// names, etc.). Used for FTS MATCH.
    pub identifiers: Vec<String>,
    /// Free-text query (e.g., trimmed code slice or import targets joined).
    pub text: String,
}

/// Retrieval hook — trait object avoids leaking the `Connection` / `Embedder`
/// lifetimes into `PipelineConfig`. Callers typically build this as a closure
/// over an owned retriever + connection.
pub type RetrieverFn = dyn Fn(&RetrievalQuery) -> anyhow::Result<Vec<ScoredChunk>> + Send + Sync;

/// Trait that the pipeline calls. A `None` result means "no context to inject".
pub trait ContextInjectionSource: Send + Sync {
    fn inject(&self, req: &InjectionRequest) -> Option<String>;
}

/// Concrete injector that composes retrieve → precedence → plan → render.
pub struct ContextInjector {
    context: ContextConfig,
    weights: SourceWeights,
    retriever: Arc<RetrieverFn>,
    k: usize,
}

impl ContextInjector {
    /// Build from a `SourcesConfig` (weights derived from source entries) and
    /// a retrieval closure.
    pub fn new(sources: &SourcesConfig, retriever: Arc<RetrieverFn>) -> Self {
        let weights = SourceWeights::new(
            sources
                .sources
                .iter()
                .filter_map(|s| s.weight.map(|w| (s.name.clone(), w))),
        );
        Self {
            context: sources.context.clone(),
            weights,
            retriever,
            k: 8,
        }
    }

    /// Override the retrieval top-K (default 8).
    #[must_use]
    pub fn with_k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }
}

impl ContextInjectionSource for ContextInjector {
    fn inject(&self, req: &InjectionRequest) -> Option<String> {
        if !self.context.auto_inject {
            return None;
        }

        let query = RetrievalQuery {
            text: req.text.clone(),
            identifiers: req.identifiers.clone(),
            filters: Filters::default(),
            k: self.k,
            min_score: 0.0,
            reviewed_file_language: req.language.clone(),
        };

        let hits = match (self.retriever)(&query) {
            Ok(h) => h,
            Err(_) => return None,
        };
        if hits.is_empty() {
            return None;
        }

        // Precedence pass (dedupes duplicated qualified_names across sources).
        let (kept, precedence_log) = if self.weights.is_empty() {
            (hits, PrecedenceLog::new())
        } else {
            resolve_precedence(hits, &self.weights)
        };

        let (symbols, prose): (Vec<_>, Vec<_>) = kept
            .into_iter()
            .partition(|h| matches!(h.chunk.kind, ChunkKind::Symbol));

        let token_counter = |s: &str| s.split_whitespace().count();
        let plan = plan_injection(symbols, prose, &self.context, &token_counter);
        if plan.injected.is_empty() {
            return None;
        }

        let rendered = render_context_block(&plan, &NoStaleness, &precedence_log);
        if rendered.trim().is_empty() {
            None
        } else {
            Some(rendered)
        }
    }
}

impl std::fmt::Debug for ContextInjector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextInjector")
            .field("auto_inject", &self.context.auto_inject)
            .field("k", &self.k)
            .finish_non_exhaustive()
    }
}
