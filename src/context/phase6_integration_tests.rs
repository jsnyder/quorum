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

#[test]
fn end_to_end_review_with_context_injection_logs_telemetry() {
    // End-to-end wiring check: a pipeline configured with a real
    // `ContextInjector` (backed by the mini-rust fixture index) must produce
    // a `FileReviewResult` whose `context_telemetry` is non-default, and
    // that telemetry must round-trip through a `ReviewRecord` written to
    // reviews.jsonl in a tempdir.
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    use crate::parser::{self, Language};
    use crate::pipeline::{review_file, PipelineConfig};
    use crate::review_log::{Flags, ReviewLog, ReviewRecord, SeverityCounts};
    use crate::test_support::fakes::FakeReviewer;

    // 1. Index the mini-rust fixture and wire a real ContextInjector.
    let harness = build_harness("mini-rust");
    // Budget 50 mirrors the working phase6 injector test — with a small
    // budget the 40% floor is easy to clear on the mini-rust fixture.
    let sources = sources_config("mini-rust", 50, true);
    let injector = ContextInjector::new(&sources, retriever_closure(&harness));

    // 2. Build the pipeline with a fake LLM that returns an empty findings
    //    list (well-formed JSON) and the injector wired in.
    let llm = FakeReviewer::always("[]");
    let config = PipelineConfig {
        models: vec!["test-model".into()],
        auto_calibrate: false,
        skip_context7: true,
        context_injector: Some(Arc::new(injector)),
        ..Default::default()
    };

    // 3. Review a synthetic rust source that mentions `verify_token` — a
    //    symbol known to live in the fixture — so retrieval returns hits.
    //    The `verify_token` call surfaces as a hydrated callee signature,
    //    which the pipeline turns into a retrieval identifier.
    let source = "fn verify_token(t: &str) -> bool { !t.is_empty() }\n\
                  pub fn check(t: &str) -> bool { verify_token(t) }\n";
    let tree = parser::parse(source, Language::Rust).unwrap();
    let result = review_file(
        Path::new("src/auth.rs"),
        source,
        Language::Rust,
        &tree,
        Some(&llm),
        &config,
    )
    .unwrap();

    let tele = result
        .context_telemetry
        .expect("context_injector wired -> FileReviewResult must carry telemetry");
    assert!(tele.auto_inject_enabled, "auto_inject=true in config");
    assert!(tele.injector_available);
    assert!(!tele.retriever_errored);
    assert!(tele.injected_chunk_count > 0, "retrieve+plan delivered chunks");
    assert!(tele.rendered_prompt_hash.is_some(), "render hash present");
    assert!(!tele.injected_chunk_ids.is_empty(), "chunk ids recorded");
    assert!(
        tele.injected_chunk_ids
            .iter()
            .any(|id| id.contains("verify_token") || id.contains("token")),
        "expected at least one injected chunk id to mention the indexed symbol, got {:?}",
        tele.injected_chunk_ids,
    );

    // 4. Build a ReviewRecord carrying this telemetry and persist it to a
    //    tempdir reviews.jsonl (avoids touching ~/.quorum/).
    let dir = TempDir::new().unwrap();
    let log = ReviewLog::new(dir.path().join("reviews.jsonl"));
    let record = ReviewRecord {
        run_id: ReviewRecord::new_ulid(),
        timestamp: chrono::Utc::now(),
        quorum_version: env!("CARGO_PKG_VERSION").to_string(),
        repo: Some("phase6-e2e".into()),
        invoked_from: "test".into(),
        model: "test-model".into(),
        files_reviewed: 1,
        lines_added: None,
        lines_removed: None,
        findings_by_severity: SeverityCounts::default(),
        suppressed_by_rule: Default::default(),
        tokens_in: 0,
        tokens_out: 0,
        tokens_cache_read: 0,
        duration_ms: 0,
        flags: Flags::default(),
        context: tele.clone(),
    };
    log.record(&record).unwrap();

    // 5. Read back: exactly one record, context block round-trips intact.
    let loaded = log.load_all().unwrap();
    assert_eq!(loaded.len(), 1, "exactly one record written");
    let back = &loaded[0].context;
    assert!(back.auto_inject_enabled);
    assert!(back.injector_available);
    assert!(!back.retriever_errored);
    assert!(back.injected_chunk_count > 0);
    assert!(back.rendered_prompt_hash.is_some());
    assert_eq!(back.injected_chunk_ids, tele.injected_chunk_ids);
}
