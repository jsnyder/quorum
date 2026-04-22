//! Production bootstrap for the context injector.
//!
//! Builds an `Arc<dyn ContextInjectionSource>` from `~/.quorum/sources.toml`
//! plus the on-disk per-source indexes. Returns `None` (rather than erroring)
//! whenever context injection cannot be safely enabled — missing config,
//! `auto_inject = false`, no indexed source — so reviews degrade to the
//! pre-context behavior instead of failing.
//!
//! The retriever closure opens the SQLite db read-only on every call and
//! owns a fresh `HashEmbedder` / `SystemClock`. This keeps it `Send + Sync
//! + 'static` without threading a shared connection through the pipeline;
//! review workloads query once per file so the per-call open is negligible
//! relative to the LLM round-trip.
//!
//! NOTE: the current wiring picks the first registered source that has an
//! index on disk. Cross-source fanout (merging hits across multiple dbs) is
//! out of scope for the initial integration — the injector already filters
//! to that source via `RetrievalQuery.filters.sources`, so multi-source
//! support is a closure-level change, not a pipeline change.

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use crate::calibrator::Calibrator;
use crate::context::cli::SourceLayout;
use crate::context::config::SourcesConfig;
use crate::context::index::builder::ensure_vec_loaded;
#[cfg(test)]
use crate::context::index::traits::HashEmbedder;
use crate::context::index::traits::SystemClock;
use crate::context::inject::{ContextInjectionSource, ContextInjector, RetrieverFn};
use crate::context::retrieve::{Filters, RetrievalQuery, Retriever, ScoredChunk};
use crate::feedback::FeedbackEntry;

/// Construct the production retrieval embedder. Delegates to the shared
/// factory in `cli::new_prod_embedder` so reviews and `quorum context
/// query` agree on the same fastembed-with-HashEmbedder-fallback policy
/// — using two different factories would drift the two code paths and
/// make a first-run `context query` succeed while reviews panic.
fn build_retrieval_embedder() -> crate::context::cli::ProdEmbedder {
    crate::context::cli::new_prod_embedder()
}

/// Build a production `ContextInjectionSource` from `<home>/sources.toml` and
/// the associated per-source indexes.
///
/// Returns `None` when:
/// - `<home>/sources.toml` is missing or unparseable (don't fail the whole
///   review just because the user hasn't set up context yet).
/// - `context.auto_inject = false` in the config.
/// - No registered source has an `index.db` on disk.
///
/// The returned injector is wired with a [`Calibrator`] seeded from the
/// caller-supplied feedback entries so `ContextMisleading` verdicts raise
/// per-chunk thresholds in real reviews.
pub fn build_production_injector(
    home: &Path,
    feedback: &[FeedbackEntry],
) -> Option<Arc<dyn ContextInjectionSource>> {
    let sources_path = home.join("sources.toml");
    if !sources_path.exists() {
        return None;
    }
    let cfg = match SourcesConfig::load(&sources_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %sources_path.display(),
                "context bootstrap: sources.toml present but failed to parse; skipping injection"
            );
            return None;
        }
    };
    if !cfg.context.auto_inject || cfg.sources.is_empty() {
        return None;
    }

    // Walk registered sources and pick the first one whose `index.db` is
    // actually openable and queryable. Existence alone isn't enough — a
    // stale tempfile or truncated db would hand a dead connection to the
    // retriever and fail on the first real review. Skip those and fall
    // through so the caller degrades to pre-context behavior.
    let picked = cfg.sources.iter().find_map(|s| {
        let layout = SourceLayout::for_source(home, &s.name);
        if !layout.db.exists() {
            return None;
        }
        ensure_vec_loaded();
        match Connection::open_with_flags(&layout.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => {
                // Probe all three tables the retriever actually uses. A db
                // with only `chunks` would pass a naive check but fail the
                // BM25 leg of the first query; likewise a missing
                // `chunks_vec` would fail the vector leg. Using one
                // compound query keeps the cost minimal.
                let probe = conn.query_row::<u32, _, _>(
                    "SELECT (SELECT COUNT(*) FROM chunks) \
                          + (SELECT COUNT(*) FROM chunks_fts) \
                          + (SELECT COUNT(*) FROM chunks_vec)",
                    [],
                    |r| r.get(0),
                );
                match probe {
                    Ok(_) => Some((s.name.clone(), layout.db)),
                    Err(e) => {
                        tracing::warn!(
                            source = %s.name,
                            path = %layout.db.display(),
                            error = %e,
                            "context bootstrap: index.db present but unusable (missing table?); skipping"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    source = %s.name,
                    path = %layout.db.display(),
                    error = %e,
                    "context bootstrap: index.db present but cannot be opened; skipping"
                );
                None
            }
        }
    });
    let (src_name, db_path) = match picked {
        Some(v) => v,
        None => {
            tracing::info!(
                "context bootstrap: no registered source has a usable index; run `quorum context index` to enable auto-injection"
            );
            return None;
        }
    };

    // Own the db path directly so non-UTF-8 bytes (rare on macOS/Linux but
    // legal on ext4/APFS) survive the hand-off into the `'static` closure
    // without going through `to_string_lossy`.
    let db_path_owned = db_path;
    let src_for_filter = src_name.clone();

    // Initialize fastembed once; share the `Arc` across every retriever
    // call. Model init is expensive (~1s) and per-review cost would
    // dominate otherwise. Mutex inside `FastEmbedEmbedder` serializes
    // actual inference, which is fine because reviews are sequential per
    // file anyway.
    let embedder = std::sync::Arc::new(build_retrieval_embedder());

    let retriever: Arc<RetrieverFn> = {
        let embedder = std::sync::Arc::clone(&embedder);
        Arc::new(move |q: &RetrievalQuery| -> anyhow::Result<Vec<ScoredChunk>> {
            // Every invocation is a fresh process-safe open; the vec0 hook
            // must be registered before `Connection::open*` or the vector
            // leg of retrieval errors with `no such module: vec0`.
            ensure_vec_loaded();
            let conn = Connection::open_with_flags(
                &db_path_owned,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let clock = SystemClock;
            let retriever = Retriever::new(&conn, embedder.as_ref(), &clock);
            // Constrain to the specific source we picked so multi-source
            // layouts don't accidentally leak hits from other indexes.
            let mut q = q.clone();
            q.filters = Filters {
                sources: vec![src_for_filter.clone()],
                kinds: q.filters.kinds,
            };
            retriever.query(q)
        })
    };

    let calibrator = Calibrator::from_feedback(cfg.context.inject_min_score, feedback);
    let injector = ContextInjector::new(&cfg, retriever).with_calibrator(Arc::new(calibrator));
    Some(Arc::new(injector))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn returns_none_when_sources_toml_missing() {
        let dir = tempdir().unwrap();
        assert!(build_production_injector(dir.path(), &[]).is_none());
    }

    #[test]
    fn returns_none_when_auto_inject_disabled() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("sources.toml"),
            r#"
[context]
auto_inject = false

[[source]]
name = "demo"
kind = "rust"
path = "/tmp/demo"
"#,
        )
        .unwrap();
        assert!(build_production_injector(dir.path(), &[]).is_none());
    }

    #[test]
    fn returns_none_when_no_source_has_index() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("sources.toml"),
            r#"
[context]
auto_inject = true

[[source]]
name = "demo"
kind = "rust"
path = "/tmp/demo"
"#,
        )
        .unwrap();
        // No `<home>/sources/demo/index.db` exists.
        assert!(build_production_injector(dir.path(), &[]).is_none());
    }

    #[test]
    fn returns_some_when_one_source_has_index() {
        // Build a real minimal index so the bootstrap finds `index.db` and
        // can open it. We don't care whether the retriever returns hits for
        // an empty query — only that the injector is wired and dispatchable.
        use crate::context::extract::dispatch::{extract_source, ExtractConfig};
        use crate::context::index::builder::IndexBuilder;
        use crate::context::index::traits::FixedClock;
        use crate::context::store::ChunkStore;
        use std::path::PathBuf;

        let dir = tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("sources/mini-rust")).unwrap();

        // Extract + index the mini-rust fixture so index.db exists with real
        // data. This mirrors what `quorum context index` does.
        let source = crate::context::config::SourceEntry {
            name: "mini-rust".into(),
            kind: crate::context::config::SourceKind::Rust,
            location: crate::context::config::SourceLocation::Path(PathBuf::from(
                "tests/fixtures/context/repos/mini-rust",
            )),
            paths: vec![],
            weight: Some(10),
            ignore: vec![],
        };
        let clock = FixedClock::epoch();
        let extracted = extract_source(&source, &ExtractConfig::default(), &clock).unwrap();
        let jsonl = home.join("sources/mini-rust/chunks.jsonl");
        let mut store = ChunkStore::new(&jsonl);
        for c in &extracted.chunks {
            store.append(c).unwrap();
        }
        let embedder = HashEmbedder::new(384);
        let db = home.join("sources/mini-rust/index.db");
        let mut builder = IndexBuilder::new(&db, &clock, &embedder).unwrap();
        builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();

        std::fs::write(
            home.join("sources.toml"),
            r#"
[context]
auto_inject = true

[[source]]
name = "mini-rust"
kind = "rust"
path = "tests/fixtures/context/repos/mini-rust"
weight = 10
"#,
        )
        .unwrap();

        let injector = build_production_injector(home, &[]).expect("injector wired");

        // Dispatch through the injector to prove retriever+calibrator wiring
        // are live. Empty text + identifiers still runs the pipeline.
        let req = crate::context::inject::InjectionRequest {
            file_path: "x.rs".into(),
            language: Some("rust".into()),
            identifiers: vec!["verify_token".into()],
            text: "jwt signing".into(),
        };
        let out = injector.inject(&req);
        assert!(out.telemetry.auto_inject_enabled);
        assert!(out.telemetry.injector_available);
    }

    #[test]
    fn returns_none_when_index_is_missing_fts_table() {
        // A db that only has `chunks` but no `chunks_fts` would pass the
        // old single-table probe but fail the BM25 leg of every real
        // retrieval. Force the smoke test to cover all three tables so
        // partially-built indexes also fall through to None.
        let dir = tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("sources/partial")).unwrap();
        let db_path = home.join("sources/partial/index.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE chunks (id TEXT PRIMARY KEY);")
            .unwrap();
        // Intentionally omit chunks_fts and chunks_vec.
        drop(conn);
        std::fs::write(
            home.join("sources.toml"),
            r#"
[context]
auto_inject = true

[[source]]
name = "partial"
kind = "rust"
path = "/tmp/partial"
"#,
        )
        .unwrap();
        assert!(build_production_injector(home, &[]).is_none());
    }

    #[test]
    fn returns_none_when_only_index_is_corrupt() {
        // A file named `index.db` that isn't a valid SQLite database should
        // not be picked as a usable source. Before the validation was added
        // bootstrap would hand a dead connection to the retriever and each
        // query inside a real review would fail, instead of degrading to the
        // pre-context behavior as the contract states.
        let dir = tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("sources/broken")).unwrap();
        std::fs::write(
            home.join("sources/broken/index.db"),
            b"this is not a sqlite database",
        )
        .unwrap();
        std::fs::write(
            home.join("sources.toml"),
            r#"
[context]
auto_inject = true

[[source]]
name = "broken"
kind = "rust"
path = "/tmp/broken"
"#,
        )
        .unwrap();
        assert!(build_production_injector(home, &[]).is_none());
    }
}
