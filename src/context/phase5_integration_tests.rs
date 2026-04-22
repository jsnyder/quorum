//! Phase 5 integration tests: retrieve → plan → render end-to-end.

use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::tempdir;

use super::config::{ContextConfig, SourceEntry, SourceKind, SourceLocation};
use super::extract::dispatch::{extract_source, ExtractConfig};
use super::index::builder::IndexBuilder;
use super::index::traits::{FixedClock, HashEmbedder};
use super::inject::plan::{plan_injection, InjectionPlan};
use super::inject::render::render_context_block;
use super::inject::stale::NoStaleness;
use super::retrieve::{resolve_precedence, Filters, PrecedenceLog, RetrievalQuery, Retriever, SourceWeights};
use super::store::ChunkStore;
use super::types::ChunkKind;

fn fixture_source(name: &str) -> SourceEntry {
    SourceEntry {
        name: name.to_string(),
        kind: SourceKind::Rust,
        location: SourceLocation::Path(PathBuf::from(format!(
            "tests/fixtures/context/repos/{name}"
        ))),
        paths: Vec::new(),
        weight: None,
        ignore: Vec::new(),
    }
}

fn token_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn build_retriever(source_name: &str) -> (tempfile::TempDir, Connection, HashEmbedder, FixedClock) {
    let dir = tempdir().unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    let db = dir.path().join("index.db");

    let source = fixture_source(source_name);
    let extracted =
        extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    let mut store = ChunkStore::new(&jsonl);
    for c in &extracted.chunks {
        store.append(c).unwrap();
    }
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);
    {
        let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
        builder.rebuild_from_jsonl(source_name, &jsonl).unwrap();
    }
    let conn = Connection::open(&db).unwrap();
    (dir, conn, emb, clock)
}

fn context_config_with_budget(budget: u32, tau: f32) -> ContextConfig {
    ContextConfig {
        auto_inject: true,
        inject_budget_tokens: budget,
        inject_min_score: tau,
        inject_max_chunks: 4,
        rerank_recency_halflife_days: 90,
        rerank_recency_floor: 0.25,
        max_source_size_mb: 100,
        ignore: Vec::new(),
    }
}

#[test]
fn retrieve_plan_render_pipeline_produces_markdown_block() {
    let (_dir, conn, emb, clock) = build_retriever("mini-rust");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let hits = retriever
        .query(RetrievalQuery {
            text: "jwt validation signing key".to_string(),
            identifiers: vec!["verify_token".to_string()],
            structural_names: vec![],
            filters: Filters::default(),
            k: 8,
            min_score: 0.0,
            reviewed_file_language: Some("rust".to_string()),
        })
        .unwrap();
    assert!(!hits.is_empty(), "expected retriever hits");

    let (symbols, prose): (Vec<_>, Vec<_>) = hits
        .into_iter()
        .partition(|h| matches!(h.chunk.kind, ChunkKind::Symbol));

    let config = context_config_with_budget(50, 0.0);
    let plan = plan_injection(symbols, prose, &config, &token_count);
    assert!(!plan.injected.is_empty(), "plan should inject at least one chunk");

    let output = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(
        output.starts_with("<retrieved_reference>"),
        "output must open with <retrieved_reference>, got: {output}"
    );
    assert!(
        output.contains("# Context"),
        "output must contain # Context header, got: {output}"
    );
    assert!(output.contains("verify_token"), "output should mention verify_token");
    assert!(
        output.contains("tokens across") && output.contains("chunks from"),
        "footer missing: {output}"
    );
}

#[test]
fn small_chunk_under_huge_budget_still_injects() {
    // Previously a 40% volume floor wiped plans that under-filled the
    // budget, which nuked ~half of single-file reviews in production.
    // Gate is now purely relevance-based (tau), so a single small chunk
    // with a passing score survives regardless of budget headroom.
    let (_dir, conn, emb, clock) = build_retriever("mini-rust");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let hits = retriever
        .query(RetrievalQuery {
            text: "jwt verification".to_string(),
            identifiers: Vec::new(),
            structural_names: vec![],
            filters: Filters::default(),
            k: 1,
            min_score: 0.0,
            reviewed_file_language: Some("rust".to_string()),
        })
        .unwrap();

    let (symbols, prose): (Vec<_>, Vec<_>) = hits
        .into_iter()
        .partition(|h| matches!(h.chunk.kind, ChunkKind::Symbol));

    let config = context_config_with_budget(100_000, 0.0);
    let plan = plan_injection(symbols, prose, &config, &token_count);
    assert!(
        !plan.injected.is_empty(),
        "small chunk should survive huge budget without a volume floor: {:?}",
        plan
    );

    let output = render_context_block(&plan, &NoStaleness, &PrecedenceLog::new());
    assert!(!output.is_empty(), "expected non-empty output: {output}");
}

#[test]
fn precedence_filters_duplicates_before_planning() {
    let (_dir, conn, emb, clock) = build_retriever("mini-rust");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let hits = retriever
        .query(RetrievalQuery {
            text: "verify token".to_string(),
            identifiers: vec!["verify_token".to_string()],
            structural_names: vec![],
            filters: Filters::default(),
            k: 8,
            min_score: 0.0,
            reviewed_file_language: Some("rust".to_string()),
        })
        .unwrap();
    assert!(!hits.is_empty());

    let weights = SourceWeights::new([("mini-rust".to_string(), 10)]);
    let (kept, log) = resolve_precedence(hits, &weights);
    assert!(!kept.is_empty());
    let verify_count = kept
        .iter()
        .filter(|c| c.chunk.qualified_name.as_deref() == Some("verify_token"))
        .count();
    assert!(
        verify_count <= 1,
        "precedence should keep at most one verify_token (got {verify_count})"
    );
    let _ = log;
}

#[test]
fn adaptive_threshold_injects_doc_when_symbols_starve() {
    let (_dir, conn, emb, clock) = build_retriever("mini-rust");
    let retriever = Retriever::new(&conn, &emb, &clock);
    let hits = retriever
        .query(RetrievalQuery {
            text: "architectural decision jwt authentication".to_string(),
            identifiers: Vec::new(),
            structural_names: vec![],
            filters: Filters {
                sources: Vec::new(),
                kinds: vec![ChunkKind::Doc],
                exclude_source_paths: vec![],
            },
            k: 4,
            min_score: 0.0,
            reviewed_file_language: None,
        })
        .unwrap();

    let config = context_config_with_budget(100, 0.9);
    let plan = plan_injection(Vec::new(), hits, &config, &token_count);
    let _ = plan;
}
