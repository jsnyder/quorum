//! Quorum library crate.
//!
//! This is the library half of the bin/lib hybrid split. The binary at
//! `src/main.rs` depends on this library and re-exports its modules through
//! `pub use quorum::foo` aliases at the crate root so existing `crate::foo`
//! paths continue to resolve.
//!
//! Integration tests under `tests/` import internal types directly from this
//! crate (e.g. `use quorum::calibrator::CalibratorConfig;`).
//!
//! # Scope (PR0)
//!
//! Intentionally conservative: only modules that the planned PR1 integration
//! tests need (`calibrator_parity`, `calibrator_trace_snapshot`,
//! `feedback_backward_compat`, `prompt_format`) plus their tight transitive
//! dependencies. Specifically excluded for now:
//!
//! - `pipeline`, `review`: depend on binary-only modules (`llm_client`,
//!   `context_enrichment`, `context`, `cache`, `review_log`). Moving these
//!   would cascade ~6 more modules into the lib. The `llm_compliance.rs`
//!   integration test slot will need a follow-up PR0.5 that either moves
//!   those modules too, or uses a thin shim.
//!
//! Tests that only need `calibrate(...)` over hand-built `FeedbackEntry`
//! lists, plus `Finding`/`CalibratorTraceEntry` snapshot assertions, work
//! against this scope without modification.

// Core finding/severity/source types — leaf module, no internal deps.
pub mod finding;

// Local embedding model (fastembed, gated on the `embeddings` feature).
// Leaf module; pulled in here because feedback_index references it.
pub mod embeddings;

// Feedback store + entries (serde-only deps, no embeddings).
pub mod feedback;

// Vector/Jaccard similarity index over the feedback store. Calibrator
// references this directly, so it has to live in the lib.
pub mod feedback_index;

// Calibrator: post-hoc finding adjustment using feedback precedents.
pub mod calibrator;

// Calibrator tracing: structured per-finding decision trace.
pub mod calibrator_trace;

// Calibrator fingerprint: stable projection over CalibratorTraceEntry for
// refactor parity testing (PR1 Phase 0).
pub mod calibrator_fingerprint;

// Strict Category enum (PR1a) — replaces Finding.category: String.
pub mod category;

// Project domain detection (HA, ESPHome, etc.) — leaf module.
pub mod domain;

// Source parsing wrappers (tree-sitter) — leaf module.
pub mod parser;

// Local AST analysis (per-language pattern matching).
pub mod analysis;

// ast-grep rule scanning.
pub mod ast_grep;

// Code hydration: turning raw findings into rich, navigable findings.
pub mod hydration;

// Finding merge / deduplication across sources.
pub mod merge;

// Secret redaction prior to LLM calls.
pub mod redact;

// Shared pattern utilities.
pub mod patterns;

// Prompt sanitization helpers (sandbox tags, fence picking).
pub mod prompt_sanitize;

// AST grounding: identifier extraction from LLM findings.
pub mod grounding;
