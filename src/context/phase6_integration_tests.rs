//! Phase 6 integration tests: injector facade wires retrieve → plan → render
//! end-to-end and gates on `auto_inject`.

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;
use tempfile::tempdir;

use super::config::{ContextConfig, SourceEntry, SourceKind, SourceLocation, SourcesConfig};
use super::extract::dispatch::{extract_source, ExtractConfig};
use super::index::builder::IndexBuilder;
use super::index::traits::{FixedClock, HashEmbedder};
use super::inject::{ContextInjectionSource, ContextInjector, InjectionRequest, RetrieverFn};
use super::retrieve::{RetrievalQuery, Retriever, ScoredChunk};

fn fixture_source(name: &str) -> SourceEntry {
    SourceEntry {
        name: name.to_string(),
        kind: SourceKind::Rust,
        location: SourceLocation::Path(PathBuf::from(format!(
            "tests/fixtures/context/repos/{name}"
        ))),
        paths: Vec::new(),
        weight: Some(10),
        ignore: Vec::new(),
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    conn: Connection,
    embedder: HashEmbedder,
    clock: FixedClock,
}

fn build_harness(source_name: &str) -> Harness {
    let dir = tempdir().unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    let db = dir.path().join("index.db");

    let source = fixture_source(source_name);
    let extracted =
        extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    let mut store = super::store::ChunkStore::new(&jsonl);
    for c in &extracted.chunks {
        store.append(c).unwrap();
    }
    let clock = FixedClock::epoch();
    let embedder = HashEmbedder::new(384);
    {
        let mut builder = IndexBuilder::new(&db, &clock, &embedder).unwrap();
        builder.rebuild_from_jsonl(source_name, &jsonl).unwrap();
    }
    let conn = Connection::open(&db).unwrap();
    Harness {
        _dir: dir,
        conn,
        embedder,
        clock,
    }
}

fn context_config(budget: u32) -> ContextConfig {
    ContextConfig {
        auto_inject: true,
        inject_budget_tokens: budget,
        inject_min_score: 0.0,
        inject_max_chunks: 4,
        rerank_recency_halflife_days: 90,
        rerank_recency_floor: 0.25,
        max_source_size_mb: 100,
        ignore: Vec::new(),
    }
}

fn sources_config(name: &str, budget: u32, auto_inject: bool) -> SourcesConfig {
    let mut ctx = context_config(budget);
    ctx.auto_inject = auto_inject;
    SourcesConfig {
        sources: vec![fixture_source(name)],
        context: ctx,
    }
}

/// Build a `RetrieverFn` closure that owns its own connection + embedder.
/// We reopen the SQLite file so the closure can be `Send + Sync + 'static`.
fn retriever_closure(harness: &Harness) -> Arc<RetrieverFn> {
    // We can't move the harness's conn into the closure while keeping the
    // harness alive for other assertions. Instead, reopen by getting the
    // path via sqlite pragma. FixedClock isn't Clone, so construct a fresh
    // `epoch()` clock inside the closure — the retriever is pure over `now()`
    // for a given query, and the fixture is epoch-indexed.
    let path: String = harness
        .conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .expect("database path");
    Arc::new(move |q: &RetrievalQuery| -> anyhow::Result<Vec<ScoredChunk>> {
        let conn = Connection::open(&path)?;
        let embedder = HashEmbedder::new(384);
        let clock = FixedClock::epoch();
        let retriever = Retriever::new(&conn, &embedder, &clock);
        retriever.query(q.clone())
    })
}

#[test]
fn injector_produces_context_block_when_auto_inject_enabled() {
    let harness = build_harness("mini-rust");
    let sources = sources_config("mini-rust", 50, true);
    let injector = ContextInjector::new(&sources, retriever_closure(&harness));

    let req = InjectionRequest {
        file_path: "src/auth.rs".to_string(),
        language: Some("rust".to_string()),
        identifiers: vec!["verify_token".to_string()],
        text: "jwt validation signing key".to_string(),
    };

    let outcome = injector.inject(&req);
    let out = outcome
        .rendered
        .expect("auto_inject=true should produce a block when hits exist");
    assert!(
        out.starts_with("# Context"),
        "block must start with '# Context' header, got: {out}"
    );
    assert!(
        out.contains("verify_token"),
        "block should mention verify_token: {out}"
    );
    assert!(outcome.telemetry.auto_inject_enabled);
    assert!(outcome.telemetry.injector_available);
    assert!(outcome.telemetry.injected_chunk_count > 0);
    assert!(outcome.telemetry.rendered_prompt_hash.is_some());
}

#[test]
fn injector_returns_none_when_auto_inject_disabled() {
    let harness = build_harness("mini-rust");
    let sources = sources_config("mini-rust", 50, /* auto_inject = */ false);
    let injector = ContextInjector::new(&sources, retriever_closure(&harness));

    let req = InjectionRequest {
        file_path: "src/auth.rs".to_string(),
        language: Some("rust".to_string()),
        identifiers: vec!["verify_token".to_string()],
        text: "jwt validation signing key".to_string(),
    };

    let outcome = injector.inject(&req);
    assert!(
        outcome.rendered.is_none(),
        "auto_inject=false must produce None"
    );
    assert!(!outcome.telemetry.auto_inject_enabled);
    assert!(outcome.telemetry.injector_available);
    assert_eq!(outcome.telemetry.injected_chunk_count, 0);
}

#[test]
fn injector_returns_none_when_query_yields_no_hits() {
    let harness = build_harness("mini-rust");
    let sources = sources_config("mini-rust", 50, true);
    let injector = ContextInjector::new(&sources, retriever_closure(&harness));

    let req = InjectionRequest {
        file_path: "src/auth.rs".to_string(),
        language: Some("rust".to_string()),
        identifiers: Vec::new(),
        text: String::new(),
    };

    assert!(
        injector.inject(&req).rendered.is_none(),
        "empty query yields no hits -> None"
    );
}

#[test]
fn injector_returns_none_when_retriever_closure_returns_empty_vec() {
    use std::sync::Arc;

    use crate::context::inject::injector::RetrieverFn;

    let sources = sources_config("mini-rust", 50, true);
    let empty_retriever: Arc<RetrieverFn> = Arc::new(|_q| Ok(Vec::new()));
    let injector = ContextInjector::new(&sources, empty_retriever);

    let req = InjectionRequest {
        file_path: "src/auth.rs".to_string(),
        language: Some("rust".to_string()),
        identifiers: vec!["verify_token".to_string()],
        text: "jwt validation".to_string(),
    };

    assert!(
        injector.inject(&req).rendered.is_none(),
        "retriever returning empty Vec -> None"
    );
}

#[test]
fn injector_returns_none_when_retriever_errors() {
    use std::sync::Arc;

    use crate::context::inject::injector::RetrieverFn;

    let sources = sources_config("mini-rust", 50, true);
    let erroring: Arc<RetrieverFn> =
        Arc::new(|_q| Err(anyhow::anyhow!("simulated retriever failure")));
    let injector = ContextInjector::new(&sources, erroring);

    let req = InjectionRequest {
        file_path: "src/auth.rs".to_string(),
        language: Some("rust".to_string()),
        identifiers: vec!["verify_token".to_string()],
        text: "jwt validation".to_string(),
    };

    let outcome = injector.inject(&req);
    assert!(
        outcome.rendered.is_none(),
        "retriever error must yield None (fail-open, tracing::warn only)"
    );
    assert!(
        outcome.telemetry.retriever_errored,
        "retriever error must be distinguishable in telemetry from 'no hits'"
    );
    assert_eq!(
        outcome.telemetry.retrieved_chunk_count, 0,
        "retriever error -> no retrieved chunks counted"
    );
}

#[test]
fn retriever_errored_flag_is_false_when_retriever_returns_zero_hits() {
    use std::sync::Arc;

    use crate::context::inject::injector::RetrieverFn;

    let sources = sources_config("mini-rust", 50, true);
    let empty_retriever: Arc<RetrieverFn> = Arc::new(|_q| Ok(Vec::new()));
    let injector = ContextInjector::new(&sources, empty_retriever);

    let req = InjectionRequest {
        file_path: "src/auth.rs".to_string(),
        language: Some("rust".to_string()),
        identifiers: vec!["verify_token".to_string()],
        text: "jwt validation".to_string(),
    };

    let outcome = injector.inject(&req);
    assert!(outcome.rendered.is_none());
    assert!(
        !outcome.telemetry.retriever_errored,
        "healthy-but-empty retriever must NOT set retriever_errored"
    );
}
