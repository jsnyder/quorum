//! Production bootstrap for the context injector.
//!
//! Builds an `Arc<dyn ContextInjectionSource>` from `~/.quorum/sources.toml`
//! plus the on-disk per-source indexes. Returns `None` (rather than erroring)
//! whenever context injection cannot be safely enabled — missing config,
//! `auto_inject = false`, no indexed source — so reviews degrade to the
//! pre-context behavior instead of failing.
//!
//! When `context.multi_source.enabled = true` (the default), collects ALL
//! sources with a valid index and fans out retrieval across them. Per-source
//! results are min-max normalized and re-ranked with multiplicative boosts
//! (current-repo, dep-manifest, language-match) before diversity constraints
//! enforce per-source caps and current-repo reserved slots.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
use crate::context::retrieve::multi_source::{BoostContext, SourceBatch, merge_and_rerank};
use crate::context::retrieve::{Filters, RetrievalQuery, Retriever, ScoredChunk};
use crate::feedback::FeedbackEntry;

fn build_retrieval_embedder() -> crate::context::cli::ProdEmbedder {
    crate::context::cli::new_prod_embedder()
}

/// A validated source with its db path, ready for retrieval.
#[derive(Debug, Clone)]
struct ValidSource {
    name: String,
    db_path: PathBuf,
}

/// Build a production `ContextInjectionSource` from `<home>/sources.toml` and
/// the associated per-source indexes.
///
/// Returns `None` when:
/// - `<home>/sources.toml` is missing or unparseable
/// - `context.auto_inject = false` in the config
/// - No registered source has an `index.db` on disk
pub fn build_production_injector(
    home: &Path,
    feedback: &[FeedbackEntry],
) -> Option<Arc<dyn ContextInjectionSource>> {
    build_production_injector_with_project(home, feedback, None)
}

/// Like [`build_production_injector`] but accepts an optional project root
/// for current-repo detection and dep-manifest matching.
pub fn build_production_injector_with_project(
    home: &Path,
    feedback: &[FeedbackEntry],
    project_root: Option<&Path>,
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

    let multi_enabled = cfg.context.multi_source.enabled;
    let max_sources = cfg.context.multi_source.max_sources_queried as usize;

    let project_name = project_root
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());
    let valid_sources = collect_valid_sources_for_project(
        home,
        &cfg,
        if multi_enabled { max_sources } else { 1 },
        project_name.as_deref(),
    );
    if valid_sources.is_empty() {
        tracing::info!(
            "context bootstrap: no registered source has a usable index; run `quorum context index` to enable auto-injection"
        );
        return None;
    }

    let embedder = Arc::new(build_retrieval_embedder());
    let multi_source_config = cfg.context.multi_source.clone();

    let boost_ctx = build_boost_context(project_root, &cfg, &valid_sources);

    let retriever: Arc<RetrieverFn> = if valid_sources.len() == 1 && !multi_enabled {
        build_single_source_retriever(valid_sources.into_iter().next().unwrap(), embedder)
    } else {
        build_multi_source_retriever(valid_sources, embedder, multi_source_config, boost_ctx)
    };

    let calibrator = Calibrator::from_feedback(cfg.context.inject_min_score, feedback);
    let injector = ContextInjector::new(&cfg, retriever).with_calibrator(Arc::new(calibrator));
    Some(Arc::new(injector))
}

fn collect_valid_sources(home: &Path, cfg: &SourcesConfig, limit: usize) -> Vec<ValidSource> {
    collect_valid_sources_for_project(home, cfg, limit, None)
}

fn collect_valid_sources_for_project(
    home: &Path,
    cfg: &SourcesConfig,
    limit: usize,
    project_name: Option<&str>,
) -> Vec<ValidSource> {
    let mut valid = Vec::new();
    for s in &cfg.sources {
        if valid.len() >= limit {
            break;
        }
        if let Some(proj) = project_name {
            if !s.include_for.is_empty() && !s.include_for.iter().any(|p| p == proj) {
                continue;
            }
            if s.exclude_for.iter().any(|p| p == proj) {
                continue;
            }
        }
        let layout = SourceLayout::for_source(home, &s.name);
        if !layout.db.exists() {
            continue;
        }
        ensure_vec_loaded();
        match Connection::open_with_flags(&layout.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => {
                let probe = conn.query_row::<u32, _, _>(
                    "SELECT (SELECT COUNT(*) FROM chunks) \
                          + (SELECT COUNT(*) FROM chunks_fts) \
                          + (SELECT COUNT(*) FROM chunks_vec)",
                    [],
                    |r| r.get(0),
                );
                match probe {
                    Ok(_) => valid.push(ValidSource {
                        name: s.name.clone(),
                        db_path: layout.db,
                    }),
                    Err(e) => {
                        tracing::warn!(
                            source = %s.name,
                            path = %layout.db.display(),
                            error = %e,
                            "context bootstrap: index.db present but unusable (missing table?); skipping"
                        );
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
            }
        }
    }
    valid
}

fn build_boost_context(
    project_root: Option<&Path>,
    cfg: &SourcesConfig,
    valid_sources: &[ValidSource],
) -> BoostContext {
    let current_repo_source = detect_current_repo(project_root, cfg, valid_sources);

    let dep_manifest_sources = match project_root {
        Some(root) => match_dep_manifest(root, cfg),
        None => HashSet::new(),
    };

    BoostContext {
        current_repo_source,
        dep_manifest_sources,
        reviewed_language: None, // set per-file at query time via RetrievalQuery
    }
}

fn detect_current_repo(
    project_root: Option<&Path>,
    cfg: &SourcesConfig,
    valid_sources: &[ValidSource],
) -> Option<String> {
    let root = project_root?;
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let valid_names: HashSet<&str> = valid_sources.iter().map(|v| v.name.as_str()).collect();

    let mut best: Option<(usize, String)> = None;
    for s in &cfg.sources {
        if !valid_names.contains(s.name.as_str()) {
            continue;
        }
        let crate::context::config::SourceLocation::Path(ref p) = s.location else {
            continue;
        };
        let Ok(canonical_src) = std::fs::canonicalize(p) else {
            continue;
        };
        if canonical_root.starts_with(&canonical_src) {
            let depth = canonical_src.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, s.name.clone()));
            }
        }
    }
    best.map(|(_, name)| name)
}

fn match_dep_manifest(project_root: &Path, cfg: &SourcesConfig) -> HashSet<String> {
    let deps = crate::dep_manifest::parse_dependencies(project_root);
    let dep_names: HashSet<String> = deps.iter().map(|d| d.name.clone()).collect();
    let mut matched = HashSet::new();
    for s in &cfg.sources {
        if dep_names.contains(&s.name) {
            matched.insert(s.name.clone());
            continue;
        }
        for alias in &s.provides {
            if dep_names.contains(alias) {
                matched.insert(s.name.clone());
                break;
            }
        }
    }
    matched
}

fn build_single_source_retriever(
    source: ValidSource,
    embedder: Arc<crate::context::cli::ProdEmbedder>,
) -> Arc<RetrieverFn> {
    let src_name = source.name;
    let db_path = source.db_path;
    Arc::new(
        move |q: &RetrievalQuery| -> anyhow::Result<Vec<ScoredChunk>> {
            ensure_vec_loaded();
            let conn = Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let clock = SystemClock;
            let retriever = Retriever::new(&conn, embedder.as_ref(), &clock);
            let mut q = q.clone();
            q.filters = Filters {
                sources: vec![src_name.clone()],
                kinds: q.filters.kinds,
                exclude_source_paths: q.filters.exclude_source_paths,
            };
            retriever.query(q)
        },
    )
}

fn build_multi_source_retriever(
    sources: Vec<ValidSource>,
    embedder: Arc<crate::context::cli::ProdEmbedder>,
    ms_config: crate::context::config::MultiSourceConfig,
    boost_ctx: BoostContext,
) -> Arc<RetrieverFn> {
    Arc::new(
        move |q: &RetrievalQuery| -> anyhow::Result<Vec<ScoredChunk>> {
            let per_source_k = q.k.saturating_mul(2).max(10);
            let mut batches = Vec::with_capacity(sources.len());

            for src in &sources {
                ensure_vec_loaded();
                let conn = match Connection::open_with_flags(
                    &src.db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            source = %src.name,
                            error = %e,
                            "multi-source retriever: skipping source (db open failed)"
                        );
                        continue;
                    }
                };
                let clock = SystemClock;
                let retriever = Retriever::new(&conn, embedder.as_ref(), &clock);
                let mut local_q = q.clone();
                local_q.k = per_source_k;
                local_q.filters = Filters {
                    sources: vec![src.name.clone()],
                    kinds: q.filters.kinds.clone(),
                    exclude_source_paths: q.filters.exclude_source_paths.clone(),
                };
                match retriever.query(local_q) {
                    Ok(chunks) if !chunks.is_empty() => {
                        batches.push(SourceBatch {
                            source_name: src.name.clone(),
                            chunks,
                        });
                    }
                    Ok(_) => {} // empty — skip
                    Err(e) => {
                        tracing::warn!(
                            source = %src.name,
                            error = %e,
                            "multi-source retriever: query failed for source; skipping"
                        );
                    }
                }
            }

            if batches.is_empty() {
                return Ok(Vec::new());
            }

            let mut ctx = boost_ctx.clone();
            ctx.reviewed_language = q.reviewed_file_language.clone();

            let k = u32::try_from(q.k).unwrap_or(u32::MAX);
            Ok(merge_and_rerank(&batches, &ms_config, &ctx, k))
        },
    )
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
        use crate::context::extract::dispatch::{ExtractConfig, extract_source};
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
            provides: vec![],
            include_for: vec![],
            exclude_for: vec![],
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
            structural_names: vec![],
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
