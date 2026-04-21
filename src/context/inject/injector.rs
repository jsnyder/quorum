//! High-level context injection facade: compose retrieve → precedence → plan
//! → render into a single `Option<String>` suitable for splicing into an LLM
//! review prompt.
//!
//! The facade is intentionally decoupled from the real fastembed / rusqlite
//! retriever stack: callers pass a boxed closure that produces
//! `Vec<ScoredChunk>` so tests can inject fakes.

use std::sync::Arc;
use std::time::Instant;

use crate::calibrator::Calibrator;
use crate::context::config::{ContextConfig, SourcesConfig};
use crate::context::inject::plan::plan_injection;
use crate::context::inject::render::render_context_block;
use crate::context::inject::stale::NoStaleness;
use crate::context::retrieve::{
    resolve_precedence, Filters, PrecedenceLog, RetrievalQuery, ScoredChunk, SourceWeights,
};
use crate::context::types::ChunkKind;
use crate::review_log::{hash_rendered_block, ContextTelemetry};

/// Request shape that the review pipeline passes to the injector.
///
/// `file_path` is captured today for diagnostic tracing and for future
/// filters (e.g. excluding chunks from the file being reviewed); it is not
/// yet routed into the retrieval query by design.
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

/// Result of an injection attempt: the optional rendered block plus
/// telemetry describing the retrieve→plan→render pass (always populated,
/// even when `rendered` is `None`).
#[derive(Debug, Clone, Default)]
pub struct InjectionOutcome {
    pub rendered: Option<String>,
    pub telemetry: ContextTelemetry,
}

impl InjectionOutcome {
    /// Outcome representing "auto_inject was disabled". Telemetry records
    /// injector_available=true so dashboards can tell this apart from
    /// "no injector wired at all".
    pub fn disabled() -> Self {
        Self {
            rendered: None,
            telemetry: ContextTelemetry {
                auto_inject_enabled: false,
                injector_available: true,
                ..ContextTelemetry::default()
            },
        }
    }
}

/// Trait that the pipeline calls. A `rendered = None` result means
/// "no context to inject" — the telemetry is still populated.
pub trait ContextInjectionSource: Send + Sync {
    fn inject(&self, req: &InjectionRequest) -> InjectionOutcome;
}

/// Concrete injector that composes retrieve → precedence → plan → render.
pub struct ContextInjector {
    context: ContextConfig,
    weights: SourceWeights,
    retriever: Arc<RetrieverFn>,
    k: usize,
    /// Optional per-chunk threshold oracle. When `Some`, the injector gates
    /// post-retrieve hits by `max(inject_min_score,
    /// calibrator.injection_threshold_for(chunk_id))` and drops chunks whose
    /// score falls below that. `None` preserves the pre-calibrator behavior
    /// (no per-chunk suppression).
    calibrator: Option<Arc<Calibrator>>,
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
            calibrator: None,
        }
    }

    /// Override the retrieval top-K (default 8). Values of 0 are silently
    /// clamped to 1 so an accidental config never makes the injector a
    /// permanent no-op.
    #[must_use]
    pub fn with_k(mut self, k: usize) -> Self {
        self.k = k.max(1);
        self
    }

    /// Attach a calibrator so the injector can apply per-chunk injection
    /// thresholds (from `Verdict::ContextMisleading` feedback). A chunk
    /// that has been flagged `inject_suppress_after` times returns
    /// `f32::INFINITY` from the calibrator and is dropped unconditionally.
    #[must_use]
    pub fn with_calibrator(mut self, calibrator: Arc<Calibrator>) -> Self {
        self.calibrator = Some(calibrator);
        self
    }
}

impl ContextInjectionSource for ContextInjector {
    fn inject(&self, req: &InjectionRequest) -> InjectionOutcome {
        // Start the clock at the very top so retrieve+plan+render cost
        // is measured end-to-end (including the auto_inject guard, which
        // is negligible but consistent).
        let started = Instant::now();

        // Seed telemetry now; every early return mutates and ships it.
        let mut tele = ContextTelemetry {
            auto_inject_enabled: self.context.auto_inject,
            injector_available: true,
            effective_prose_threshold: self.context.inject_min_score,
            ..ContextTelemetry::default()
        };

        if !self.context.auto_inject {
            tele.render_duration_ms = started.elapsed().as_millis() as u64;
            return InjectionOutcome {
                rendered: None,
                telemetry: tele,
            };
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
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    file_path = %req.file_path,
                    "context injection retriever failed; skipping block"
                );
                tele.retriever_errored = true;
                tele.render_duration_ms = started.elapsed().as_millis() as u64;
                return InjectionOutcome {
                    rendered: None,
                    telemetry: tele,
                };
            }
        };
        tele.retrieved_chunk_count = hits.len() as u32;
        if hits.is_empty() {
            tele.render_duration_ms = started.elapsed().as_millis() as u64;
            return InjectionOutcome {
                rendered: None,
                telemetry: tele,
            };
        }

        // Calibrator gate (Task 8.4): drop chunks whose score falls below
        // their per-chunk injection threshold. `f32::INFINITY` from the
        // calibrator (N+ ContextMisleading confirmations) is a hard seal.
        //
        // This is a post-retrieve concern — the raw retriever contract is
        // unchanged — and runs before precedence/plan so neither sees
        // chunks the operator has already flagged as misleading.
        let hits = if let Some(cal) = self.calibrator.as_ref() {
            let before = hits.len();
            let kept: Vec<ScoredChunk> = hits
                .into_iter()
                .filter(|h| h.score >= cal.injection_threshold_for(&h.chunk.id))
                .collect();
            tele.suppressed_by_calibrator = (before - kept.len()) as u32;
            kept
        } else {
            hits
        };

        // Precedence pass (dedupes duplicated qualified_names across sources).
        let (kept, precedence_log) = if self.weights.is_empty() {
            (hits, PrecedenceLog::new())
        } else {
            resolve_precedence(hits, &self.weights)
        };
        tele.precedence_entries = precedence_log.entries().len() as u32;

        let (symbols, prose): (Vec<_>, Vec<_>) = kept
            .into_iter()
            .partition(|h| matches!(h.chunk.kind, ChunkKind::Symbol));

        let token_counter = |s: &str| s.split_whitespace().count();
        let plan = plan_injection(symbols, prose, &self.context, &token_counter);
        tele.below_threshold_count = plan.below_threshold_count as u32;
        tele.effective_prose_threshold = plan.effective_prose_threshold;
        tele.adaptive_threshold_applied = plan.adaptive_threshold_applied;

        if plan.injected.is_empty() {
            tele.render_duration_ms = started.elapsed().as_millis() as u64;
            return InjectionOutcome {
                rendered: None,
                telemetry: tele,
            };
        }

        // Capture injected IDs/sources BEFORE the move into the renderer.
        tele.injected_chunk_count = plan.injected.len() as u32;
        tele.injected_tokens = plan.token_count as u32;
        tele.injected_chunk_ids = plan
            .injected
            .iter()
            .map(|c| c.chunk.id.clone())
            .collect();
        let mut uniq_sources: Vec<String> = Vec::new();
        for c in &plan.injected {
            if !uniq_sources.iter().any(|s| s == &c.chunk.source) {
                uniq_sources.push(c.chunk.source.clone());
            }
        }
        tele.injected_sources = uniq_sources;

        let rendered = render_context_block(&plan, &NoStaleness, &precedence_log);
        let rendered_opt = if rendered.trim().is_empty() {
            // Plan produced chunks but render yielded nothing usable — reset
            // the injection counters so telemetry consumers see "0 delivered"
            // instead of "N planned, but no hash / no block" inconsistency.
            tele.injected_chunk_count = 0;
            tele.injected_tokens = 0;
            tele.injected_chunk_ids.clear();
            tele.injected_sources.clear();
            None
        } else {
            tele.rendered_prompt_hash = Some(hash_rendered_block(&rendered));
            Some(rendered)
        };

        tele.render_duration_ms = started.elapsed().as_millis() as u64;
        InjectionOutcome {
            rendered: rendered_opt,
            telemetry: tele,
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
