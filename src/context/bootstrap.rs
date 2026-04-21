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
use crate::context::index::traits::{HashEmbedder, SystemClock};
use crate::context::inject::{ContextInjectionSource, ContextInjector, RetrieverFn};
use crate::context::retrieve::{Filters, RetrievalQuery, Retriever, ScoredChunk};
use crate::feedback::FeedbackEntry;

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

    let (src_name, db_path) = cfg.sources.iter().find_map(|s| {
        let layout = SourceLayout::for_source(home, &s.name);
        if layout.db.exists() {
            Some((s.name.clone(), layout.db))
        } else {
            None
        }
    })?;

    // Stringify the db path for the closure — `&Path` isn't `'static`.
    let db_str = db_path.to_string_lossy().into_owned();
    let src_for_filter = src_name.clone();

    let retriever: Arc<RetrieverFn> =
        Arc::new(move |q: &RetrievalQuery| -> anyhow::Result<Vec<ScoredChunk>> {
            // Every invocation is a fresh process-safe open; the vec0 hook
            // must be registered before `Connection::open*` or the vector
            // leg of retrieval errors with `no such module: vec0`.
            ensure_vec_loaded();
            let conn = Connection::open_with_flags(
                &db_str,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI,
            )?;
            let embedder = HashEmbedder::new(384);
            let clock = SystemClock;
            let retriever = Retriever::new(&conn, &embedder, &clock);
            // Constrain to the specific source we picked so multi-source
            // layouts don't accidentally leak hits from other indexes.
            let mut q = q.clone();
            q.filters = Filters {
                sources: vec![src_for_filter.clone()],
                kinds: q.filters.kinds,
            };
            retriever.query(q)
        });

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
}
