#![allow(dead_code)]

// Library half of the bin/lib hybrid split: re-export `quorum::foo` modules
// at the binary crate root so existing `crate::foo` paths inside main.rs and
// its submodules continue to resolve unchanged. See `src/lib.rs` for the
// actual module declarations and rationale for the split.
pub use quorum::analysis;
pub use quorum::ast_grep;
pub use quorum::calibrator;
pub use quorum::calibrator_trace;
pub use quorum::category;
pub use quorum::domain;
pub use quorum::embeddings;
pub use quorum::feedback;
pub use quorum::feedback_index;
pub use quorum::finding;
pub use quorum::grounding;
pub use quorum::hydration;
pub use quorum::merge;
pub use quorum::parser;
pub use quorum::patterns;
pub use quorum::prompt_sanitize;
pub use quorum::redact;

mod agent;
mod analytics;
mod cache;
mod cli;
mod cli_io;
mod config;
mod context;
mod context_enrichment;
mod daemon;
mod dep_manifest;
mod dimensions;
mod formatting;
mod glyphs;
mod http_server;
mod linter;
mod llm_client;
mod mcp;
mod output;
mod pipeline;
mod progress;
mod review;
mod review_log;
mod stats;
mod suppress;
mod telemetry;
#[cfg(test)]
mod test_support;
mod tools;
mod trace_subscriber;

use clap::Parser;
use config::{Config, EnvConfigSource};
use pipeline::{LlmReviewer, PipelineConfig};

/// Resolve the quorum state directory, honoring the `QUORUM_HOME` env
/// override so integration tests can be hermetic. Falls back to
/// `$HOME/.quorum`. Returns None if neither can be resolved.
fn quorum_dir() -> Option<std::path::PathBuf> {
    if let Ok(override_path) = std::env::var("QUORUM_HOME") {
        if !override_path.is_empty() {
            return Some(std::path::PathBuf::from(override_path));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".quorum"))
}

/// Drain agent-contributed verdicts from `<quorum_dir>/inbox/` into the
/// feedback store before the caller loads feedback. Called at the top of the
/// `Review` and `Stats` command arms. Pipeline + stats modules stay IO-pure;
/// this is the application-boundary hook. See issue #32.
fn drain_agent_inbox() {
    let Some(home) = quorum_dir() else {
        return;
    };
    let inbox = home.join("inbox");
    let processed = inbox.join("processed");
    let feedback_path = home.join("feedback.jsonl");
    let store = crate::feedback::FeedbackStore::new(feedback_path);
    match store.drain_inbox(&inbox, &processed) {
        Ok(r) => {
            if r.drained_files > 0 {
                tracing::info!(
                    files = r.drained_files,
                    entries = r.entries,
                    errors = r.errors.len(),
                    "drained external feedback inbox"
                );
            }
            // Errors must surface regardless of `drained_files`: if every
            // claim/rename fails, no file is archived but stuck files still
            // accumulate under inbox/processing/. The previous arm-shaped
            // gate (`Ok(r) if r.drained_files > 0`) silenced that case.
            if !r.errors.is_empty() {
                for e in &r.errors {
                    tracing::warn!(
                        file = %e.file.display(),
                        line = e.line,
                        msg = %e.message,
                        "inbox drain error"
                    );
                }
                eprintln!(
                    "warning: {} external feedback line(s) failed to ingest; \
                     check {} for stuck files",
                    r.errors.len(),
                    inbox.join("processing").display(),
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "inbox drain failed");
            eprintln!("warning: external feedback inbox drain failed: {}", e);
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    match args.command {
        cli::Command::Review(opts) => {
            drain_agent_inbox();
            let exit_code = run_review(opts).await;
            std::process::exit(exit_code);
        }
        cli::Command::Stats(opts) => {
            drain_agent_inbox();
            // Resolve the quorum state dir honoring QUORUM_HOME (used by
            // hermetic tests and alternate installs). Falls back to
            // `$HOME/.quorum`, then to `./.quorum` as a last resort.
            let quorum_home = quorum_dir().unwrap_or_else(|| std::path::PathBuf::from(".quorum"));

            // Dimensional views read reviews.jsonl and aggregate via the
            // `dimensions` module. Classic dims: --by-repo/--by-caller/--rolling.
            // Context dims (Task 6.3): --by-source/--by-reviewed-repo/--misleading.
            // Context dims compose with --rolling by restricting aggregation to
            // the chronologically-last N records.
            let want_context_dim = opts.by_source || opts.by_reviewed_repo || opts.misleading;
            let want_classic_dim =
                !want_context_dim && (opts.by_repo || opts.by_caller || opts.rolling.is_some());

            if want_context_dim {
                let log = review_log::ReviewLog::new(quorum_home.join("reviews.jsonl"));
                let all_records = match log.load_all() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!(
                            "error: cannot read reviews log at {}: {e}",
                            log.path().display()
                        );
                        std::process::exit(3);
                    }
                };
                let records: Vec<_> = match opts.rolling {
                    Some(n) if n < all_records.len() => {
                        all_records[all_records.len() - n..].to_vec()
                    }
                    _ => all_records.clone(),
                };

                let (mode, slices) = if opts.by_source {
                    ("by-source", dimensions::aggregate_by_source(&records))
                } else if opts.by_reviewed_repo {
                    (
                        "by-reviewed-repo",
                        dimensions::aggregate_by_reviewed_repo(&records),
                    )
                } else {
                    ("misleading", dimensions::aggregate_misleading(&records))
                };

                let is_pipe = !std::io::IsTerminal::is_terminal(&std::io::stdout());
                let use_compact = output::should_use_compact(opts.compact);
                let use_json = opts.json || (is_pipe && !use_compact);

                if use_json {
                    let meta = serde_json::json!({
                        "min_sample": dimensions::MIN_SAMPLE,
                        "total_reviews": all_records.len(),
                        "windowed_reviews": records.len(),
                        "rolling": opts.rolling,
                    });
                    let payload = serde_json::json!({
                        "mode": mode,
                        "slices": slices,
                        "meta": meta,
                    });
                    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
                } else if use_compact {
                    println!("{}", stats::format_context_dimension_compact(mode, &slices));
                } else {
                    let style = output::Style::detect(false);
                    let unicode = unicode_ok();
                    print!(
                        "{}",
                        stats::format_context_dimension_table(mode, &slices, &style, unicode)
                    );
                }
                std::process::exit(0);
            }

            if want_classic_dim {
                let log = review_log::ReviewLog::new(quorum_home.join("reviews.jsonl"));
                let records = match log.load_all() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!(
                            "error: cannot read reviews log at {}: {e}",
                            log.path().display()
                        );
                        std::process::exit(3);
                    }
                };
                let (mode, slices) = if opts.by_repo {
                    ("by-repo", dimensions::group_by_repo(&records))
                } else if opts.by_caller {
                    ("by-caller", dimensions::group_by_caller(&records))
                } else {
                    let n = opts.rolling.unwrap();
                    ("rolling", dimensions::rolling_window(&records, n, 3))
                };

                let is_pipe = !std::io::IsTerminal::is_terminal(&std::io::stdout());
                let use_compact = output::should_use_compact(opts.compact);
                let use_json = opts.json || (is_pipe && !use_compact);

                if use_json {
                    let meta = serde_json::json!({
                        "min_sample": dimensions::MIN_SAMPLE,
                        "total_reviews": records.len(),
                    });
                    let payload = serde_json::json!({
                        "mode": mode,
                        "slices": slices,
                        "meta": meta,
                    });
                    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
                } else if use_compact {
                    println!("{}", stats::format_dimension_compact(mode, &slices));
                } else {
                    let style = output::Style::detect(false);
                    let unicode = unicode_ok();
                    print!(
                        "{}",
                        stats::format_dimension_table(mode, &slices, &style, unicode)
                    );
                }
                std::process::exit(0);
            }

            let feedback_store = feedback::FeedbackStore::new(quorum_home.join("feedback.jsonl"));
            let telemetry_store =
                telemetry::TelemetryStore::new(quorum_home.join("telemetry.jsonl"));
            let review_log = review_log::ReviewLog::new(quorum_home.join("reviews.jsonl"));

            match stats::compute_report(&feedback_store, &telemetry_store, &review_log) {
                Ok(report) => {
                    if opts.json {
                        match stats::format_json(&report) {
                            Ok(json) => println!("{}", json),
                            Err(e) => {
                                eprintln!("Error: {}", e);
                                std::process::exit(3);
                            }
                        }
                    } else if output::should_use_compact(opts.compact) {
                        print!("{}", stats::format_compact(&report));
                    } else {
                        let style = output::Style::detect(false);
                        if opts.minimal {
                            print!("{}", stats::format_human_minimal(&report, &style));
                        } else {
                            print!("{}", stats::format_human(&report, &style));
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(3);
                }
            }
        }
        cli::Command::Serve => {
            run_mcp_server().await?;
        }
        cli::Command::Daemon(opts) => {
            run_daemon(opts).await?;
        }
        cli::Command::Feedback(opts) => std::process::exit(run_feedback(opts)),
        cli::Command::Context(opts) => std::process::exit(run_context(opts)),
        cli::Command::Calibrate(opts) => std::process::exit(run_calibrate(opts)),
        cli::Command::Version => {
            println!("quorum {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(())
}

/// Translate clap args for `quorum context ...` into a `ContextCmd` and run
/// it against `ProdDeps`. Prints stdout to stdout, warnings (one per line)
/// to stderr. Exit code: 0 on success (unless `doctor` reports any failing
/// check, in which case 1), 1 on handler error.
fn run_context(opts: cli::ContextOpts) -> i32 {
    use context::cli::{
        AddArgs, AddLocation, ContextCmd, DoctorArgs, DoctorFormat, IndexArgs, ListArgs,
        ListFormat, ProdDeps, PruneArgs, QueryArgs, QueryFormat, RefreshArgs, SourceSelector,
        run_context_cmd,
    };

    // Map `--source X` / `--all` / neither into a SourceSelector. The default
    // when both are absent is `All` to match the handler's natural semantics
    // (bulk ops over every registered source).
    fn selector(source: Option<String>, all: bool) -> SourceSelector {
        if all {
            SourceSelector::All
        } else if let Some(name) = source {
            SourceSelector::Single(name)
        } else {
            SourceSelector::All
        }
    }

    let cmd = match opts.command {
        cli::ContextCommand::Init => ContextCmd::Init,
        cli::ContextCommand::Add(a) => {
            let location = match (a.path, a.git) {
                (Some(p), None) => AddLocation::Path(p),
                (None, Some(url)) => AddLocation::Git { url, rev: a.rev },
                (Some(_), Some(_)) => {
                    eprintln!("error: --path and --git are mutually exclusive");
                    return 1;
                }
                (None, None) => {
                    eprintln!("error: one of --path or --git is required");
                    return 1;
                }
            };
            ContextCmd::Add(AddArgs {
                name: a.name,
                kind: a.kind,
                location,
                weight: a.weight,
                ignore: a.ignore,
            })
        }
        cli::ContextCommand::List(l) => {
            let format = if l.json {
                ListFormat::Json
            } else if l.compact {
                ListFormat::Compact
            } else {
                ListFormat::Human
            };
            ContextCmd::List(ListArgs { format })
        }
        cli::ContextCommand::Index(i) => ContextCmd::Index(IndexArgs {
            selector: selector(i.source, i.all),
        }),
        cli::ContextCommand::Refresh(r) => ContextCmd::Refresh(RefreshArgs {
            selector: selector(r.source, r.all),
        }),
        cli::ContextCommand::Query(q) => {
            let format = if q.json {
                QueryFormat::Json
            } else if q.compact {
                QueryFormat::Compact
            } else {
                QueryFormat::Table
            };
            ContextCmd::Query(QueryArgs {
                text: q.text,
                source: q.source,
                k: q.k,
                explain: q.explain,
                format,
            })
        }
        cli::ContextCommand::Prune(p) => ContextCmd::Prune(PruneArgs { dry_run: p.dry_run }),
        cli::ContextCommand::Doctor(d) => {
            let format = if d.json {
                DoctorFormat::Json
            } else if d.compact {
                DoctorFormat::Compact
            } else {
                DoctorFormat::Table
            };
            ContextCmd::Doctor(DoctorArgs {
                format,
                repair: d.repair,
            })
        }
    };

    // `index` and `refresh` write to `chunks_vec`; if fastembed fell back to
    // HashEmbedder we'd rebuild the vector table with hashing-noise vectors
    // and silently degrade every subsequent retrieval. Use the strict
    // factory so a fastembed init failure surfaces as a clear error the
    // user can retry, rather than a corrupted index they have to discover.
    let needs_strict_embedder = matches!(cmd, ContextCmd::Index(_) | ContextCmd::Refresh(_));
    let deps_result = if needs_strict_embedder {
        ProdDeps::from_env_strict()
    } else {
        ProdDeps::from_env()
    };
    let deps = match deps_result {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}", e);
            return 1;
        }
    };

    match run_context_cmd(&cmd, &deps) {
        Ok(out) => {
            // Centralized stdout handling lives in `cli_io::write_cmd_output`:
            // BrokenPipe stays silent (downstream consumer closed early) but
            // EIO/ENOSPC etc. are surfaced to stderr with exit 1 (issue #84).
            // doctor_failed propagation preserved (issue #73).
            let stdout = std::io::stdout();
            let stderr = std::io::stderr();
            let mut out_handle = stdout.lock();
            let mut err_handle = stderr.lock();
            cli_io::write_cmd_output(&mut out_handle, &mut err_handle, &out)
        }
        Err(e) => {
            eprintln!("error: {}", e);
            1
        }
    }
}

async fn run_mcp_server() -> anyhow::Result<()> {
    use rust_mcp_sdk::mcp_server::{McpServerOptions, server_runtime};
    use rust_mcp_sdk::schema::{
        Implementation, InitializeResult, ProtocolVersion, ServerCapabilities,
        ServerCapabilitiesTools,
    };
    use rust_mcp_sdk::{McpServer, StdioTransport, ToMcpServerHandler, TransportOptions};

    let server_details = InitializeResult {
        server_info: Implementation {
            name: "quorum".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("Quorum Code Review".into()),
            description: Some("Multi-source code review: LLM ensemble + local AST analysis".into()),
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        protocol_version: ProtocolVersion::V2025_11_25.into(),
        instructions: None,
        meta: None,
    };

    let transport = StdioTransport::new(TransportOptions::default())
        .map_err(|e| anyhow::anyhow!("Failed to create stdio transport: {}", e))?;

    // Shared parse cache for the MCP server session
    let parse_cache = std::sync::Arc::new(cache::ParseCache::new(256));

    // Start file watcher in background (optional, non-fatal if it fails)
    let watch_dir = std::env::current_dir().unwrap_or_default();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _watcher = daemon::start_watcher(&watch_dir, tx).ok();
    let cache_for_watcher = parse_cache.clone();
    tokio::spawn(async move {
        daemon::run_event_loop(rx, cache_for_watcher).await;
    });

    let handler = mcp::handler::QuorumHandler::with_cache(parse_cache)?;

    let server = server_runtime::create_server(McpServerOptions {
        server_details,
        transport,
        handler: handler.to_mcp_server_handler(),
        task_store: None,
        client_task_store: None,
        message_observer: None,
    });

    server
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;
    Ok(())
}

/// Heuristic: whether the terminal can likely render block/sparkline glyphs.
/// Conservative — TERM=dumb or NO_UNICODE env disables; LANG without UTF-8 also disables.
fn unicode_ok() -> bool {
    if std::env::var_os("NO_UNICODE").is_some() {
        return false;
    }
    if let Some(term) = std::env::var_os("TERM") {
        if term == "dumb" {
            return false;
        }
    }
    if let Ok(lang) = std::env::var("LANG") {
        return lang.to_uppercase().contains("UTF-8") || lang.to_uppercase().contains("UTF8");
    }
    // No LANG set: default to unicode since we're likely on macOS or a modern terminal.
    true
}

/// Root directory for deep-review tools when reviewing `file_path`.
/// Uses the file's project root so agent tools don't escape into the
/// process CWD (previously a concrete scope-confusion issue when
/// `quorum review --deep /other/repo/f.rs` was run from $HOME).
fn deep_tool_root(file_path: &std::path::Path) -> std::path::PathBuf {
    pipeline::find_project_root(file_path)
}

async fn run_review(opts: cli::ReviewOpts) -> i32 {
    // The empty-files case is now rejected at the clap layer via
    // `#[arg(required = true, num_args = 1..)]` on `ReviewOpts.files`
    // (issue #89). The redundant handler-level guard was removed.

    // Initialize structured tracing if --trace flag or QUORUM_TRACE=1 env var
    let trace_enabled = opts.trace
        || std::env::var("QUORUM_TRACE")
            .map(|v| v == "1")
            .unwrap_or(false);
    let _trace_guard = if trace_enabled {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let trace_path = std::path::PathBuf::from(&home).join(".quorum/trace.jsonl");
        eprintln!("Tracing enabled: writing to {}", trace_path.display());
        trace_subscriber::init_trace_subscriber(Some(trace_path))
    } else {
        None
    };

    // If --daemon flag is set, send requests to running daemon
    if opts.daemon {
        return run_review_via_daemon(&opts);
    }

    // Load config
    let cfg = match Config::load(&EnvConfigSource) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            return 3;
        }
    };

    // Build LLM client if API key is available (implements both LlmReviewer and AgentReviewer)
    // Arc-wrapped for sharing across parallel file reviews
    let llm_client: Option<std::sync::Arc<llm_client::OpenAiClient>> =
        if let Ok(api_key) = cfg.require_api_key() {
            let effort = opts
                .reasoning_effort
                .clone()
                .or_else(|| {
                    std::env::var("QUORUM_REASONING_EFFORT")
                        .ok()
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| Some("low".into())); // Default: low reasoning is optimal for code review
            // Opt-in: tell the upstream proxy (e.g. LiteLLM) to skip its response
            // cache so each call reaches the underlying provider. Useful when
            // benchmarking, A/B comparing, or measuring upstream prompt-cache
            // hit rate. Default off — production reviews keep the proxy's fast
            // replay behavior.
            let bypass_proxy_cache = std::env::var("QUORUM_BYPASS_PROXY_CACHE")
                .ok()
                .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false);
            match llm_client::OpenAiClient::new(&cfg.base_url, api_key) {
                Ok(c) => Some(std::sync::Arc::new(
                    c.with_reasoning_effort(effort)
                        .with_bypass_proxy_cache(bypass_proxy_cache),
                )),
                Err(e) => {
                    eprintln!("error: cannot construct LLM client: {e}");
                    return 3;
                }
            }
        } else {
            None
        };
    let llm_reviewer: Option<&dyn LlmReviewer> = llm_client.as_deref().map(|c| c as _);

    // Build pipeline config
    let models = if opts.ensemble {
        // Ensemble: use QUORUM_ENSEMBLE_MODELS or default set
        std::env::var("QUORUM_ENSEMBLE_MODELS")
            .unwrap_or_else(|_| cfg.model.clone())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![cfg.model.clone()]
    };

    // Load feedback for calibration. Honor QUORUM_HOME via quorum_dir() so
    // hermetic tests and alternate installs route through the same dir as
    // stats/feedback. Without this, `review` could ingest from one inbox
    // and calibrate against a different feedback log (#95 review feedback).
    let qhome = quorum_dir().unwrap_or_else(|| std::path::PathBuf::from(".quorum"));
    let feedback_path = qhome.join("feedback.jsonl");
    let feedback_store = feedback::FeedbackStore::new(feedback_path.clone());
    let feedback_entries = feedback_store.load_all().unwrap_or_default();
    if !feedback_entries.is_empty() {
        tracing::debug!(entries = feedback_entries.len(), "loaded feedback entries");
    }

    // Parse diff file if provided for change-scoped review
    let diff_ranges = if let Some(ref diff_path) = opts.diff_file {
        match std::fs::read_to_string(diff_path) {
            Ok(diff_content) => {
                let ranges = hydration::parse_unified_diff(&diff_content);
                if !ranges.is_empty() {
                    eprintln!(
                        "Diff-aware: scoping hydration to {} changed file(s)",
                        ranges.len()
                    );
                    Some(ranges)
                } else {
                    None
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: Could not read diff file {}: {}",
                    diff_path.display(),
                    e
                );
                None
            }
        }
    } else {
        None
    };

    // Create semaphore for parallel LLM concurrency control
    let semaphore = if opts.parallel > 1 {
        Some(std::sync::Arc::new(tokio::sync::Semaphore::new(
            opts.parallel,
        )))
    } else if opts.parallel == 0 {
        Some(std::sync::Arc::new(tokio::sync::Semaphore::new(32)))
    } else {
        None // parallel=1, sequential
    };

    // Pre-build FeedbackIndex once for sharing across parallel tasks.
    // --fast skips fastembed model load and uses Jaccard-only matching.
    let shared_feedback_index = {
        let feedback_path_ref = qhome.join("feedback.jsonl");
        if feedback_path_ref.exists() {
            let store = feedback::FeedbackStore::new(feedback_path_ref);
            let build_result = if opts.fast {
                feedback_index::FeedbackIndex::build_bm25(&store)
            } else {
                feedback_index::FeedbackIndex::build(&store)
            };
            match build_result {
                Ok(idx) => {
                    tracing::debug!(
                        fast_mode = opts.fast,
                        "FeedbackIndex: pre-built for parallel calibration"
                    );
                    Some(std::sync::Arc::new(std::sync::Mutex::new(idx)))
                }
                Err(e) => {
                    eprintln!("Warning: Could not build feedback index: {}", e);
                    None
                }
            }
        } else {
            None
        }
    };

    // Build the production context injector from `<qhome>/sources.toml`
    // (if present and `auto_inject = true`). Returns `None` when context
    // isn't configured, so reviews without a sources file behave exactly
    // as before. Honors QUORUM_HOME via the same `qhome` used for
    // feedback/telemetry/reviews — without this, hermetic runs would
    // calibrate against one dir and source `sources.toml` from another.
    let context_injector = context::bootstrap::build_production_injector(&qhome, &feedback_entries);
    if context_injector.is_some() {
        tracing::info!(
            "context injector wired from ~/.quorum/sources.toml — auto-inject is active"
        );
    }

    // Build a single Context7 cache for the whole review so positive AND
    // negative resolves (with 24h TTL) are reused across every file in
    // multi-file reviews. Without this, each per-file enrich call would
    // build a fresh cache and re-hammer Context7 for the same deps.
    //
    // Box::leak is bounded to one allocation per process: `run_review` is
    // only ever called from the one-shot CLI dispatcher (`main()` calls
    // `std::process::exit` immediately after). The long-lived `daemon`
    // and `serve` paths use their own pipelines (run_daemon /
    // run_mcp_server) and never enter this function, so the leaked
    // memory is reclaimed when the CLI process exits. A future caller
    // that drives `run_review` in a long-lived loop should switch to
    // `OnceLock<&'static dyn ContextFetcher>` to make the once-per-process
    // guarantee explicit.
    // CR8: distinguish "not yet built" from "init failed". A None fetcher
    // alone would fall through to the per-file ad-hoc path in pipeline.rs,
    // which would re-fail Context7HttpFetcher::new() once per file and
    // abort each review. Carrying the failure forward as a sticky flag
    // lets the pipeline skip enrichment cleanly.
    let (context7_fetcher, context7_disabled): (
        Option<std::sync::Arc<dyn crate::context_enrichment::ContextFetcher>>,
        bool,
    ) = if opts.skip_context7 {
        (None, false)
    } else {
        match crate::context_enrichment::Context7HttpFetcher::new() {
            Ok(http) => {
                let leaked: &'static dyn crate::context_enrichment::ContextFetcher =
                    Box::leak(Box::new(http));
                let cached = crate::context_enrichment::CachedContextFetcher::new(leaked, 32);
                (
                    Some(std::sync::Arc::new(cached)
                        as std::sync::Arc<
                            dyn crate::context_enrichment::ContextFetcher,
                        >),
                    false,
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "Context7 HTTP fetcher init failed; disabling Context7 enrichment for this review");
                (None, true)
            }
        }
    };

    // Build calibrator config, loading data-driven thresholds if available.
    let mut calibrator_config = calibrator::CalibratorConfig::default();
    let thresholds_path = qhome.join("calibrator_thresholds.toml");
    if let Some(tc) =
        quorum::threshold_config::ThresholdConfig::load_from(&thresholds_path.to_string_lossy())
    {
        calibrator_config.suppress_threshold = tc.suppress.map(|p| p.threshold);
        calibrator_config.boost_threshold = tc.boost.map(|p| p.threshold);
        tracing::info!(
            suppress = ?calibrator_config.suppress_threshold,
            boost = ?calibrator_config.boost_threshold,
            "loaded data-driven calibrator thresholds"
        );
    }
    // QUORUM_FORCE_THRESHOLD overrides both suppress and boost.
    if let Ok(v) = std::env::var("QUORUM_FORCE_THRESHOLD") {
        match v.parse::<f64>() {
            Ok(t) if t.is_finite() && (0.0..=1.0).contains(&t) => {
                calibrator_config.force_threshold = Some(t);
                tracing::warn!(
                    threshold = t,
                    "QUORUM_FORCE_THRESHOLD active -- collapses neutral zone \
                     (suppress when score < {t}, boost when score >= {t})"
                );
            }
            Ok(t) => {
                tracing::warn!(
                    raw = %v,
                    parsed = t,
                    "ignoring QUORUM_FORCE_THRESHOLD: expected finite value in [0.0, 1.0]"
                );
            }
            Err(e) => {
                tracing::warn!(
                    raw = %v,
                    error = %e,
                    "ignoring QUORUM_FORCE_THRESHOLD: parse failed"
                );
            }
        }
    }

    let pipeline_cfg = PipelineConfig {
        models,
        feedback: feedback_entries,
        feedback_store: Some(feedback_path.clone()),
        diff_ranges,
        framework_overrides: opts.framework.clone(),
        skip_context7: opts.skip_context7,
        fast: opts.fast,
        semaphore,
        feedback_index: shared_feedback_index,
        context_injector,
        context7_fetcher,
        context7_disabled,
        calibrator_config,
        ..Default::default()
    };

    // Load project-level suppressions from target project root
    let project_root = if let Some(first_file) = opts.files.first() {
        pipeline::find_project_root(first_file)
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let suppress_path = project_root.join(".quorum/suppress.toml");
    let suppress_rules = suppress::load_project_suppressions(&suppress_path);
    if !suppress_rules.is_empty() {
        tracing::debug!(
            rules = suppress_rules.len(),
            path = %suppress_path.display(),
            "loaded project suppression rules"
        );
    }

    // Arc-wrap shared config for parallel access
    let mut pipeline_cfg = std::sync::Arc::new(pipeline_cfg);
    let suppress_rules = std::sync::Arc::new(suppress_rules);

    let review_start = std::time::Instant::now();

    let style = output::Style::detect(opts.no_color);
    let use_compact = output::should_use_compact(opts.compact);
    let use_json =
        !use_compact && (opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout()));
    let mut all_findings = Vec::new();
    let mut file_results: Vec<pipeline::FileReviewResult> = Vec::new();
    let mut had_errors = false;

    // Linter coverage discovery, scoped to whichever project the first
    // reviewed file lives in. Nothing here runs the linters -- only reports
    // what would or would not engage given current project config. Flows
    // into compact header, JSON _meta, and the human tail summary below.
    let project_root = opts
        .files
        .first()
        .map(|p| pipeline::find_project_root(p))
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let review_file_refs: Vec<&std::path::Path> = opts.files.iter().map(|p| p.as_path()).collect();
    let linter_hints = linter::detect_unconfigured_linters(&project_root, &review_file_refs);
    let enabled_linters: Vec<linter::LinterKind> = {
        let all_enabled = linter::detect_linters(&project_root);
        let exts: std::collections::HashSet<String> = review_file_refs
            .iter()
            .filter_map(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase)
            })
            .collect();
        all_enabled
            .into_iter()
            .filter(|k| linter_kind_is_relevant(k, &exts))
            .collect()
    };

    if use_compact {
        if let Some(header) = output::format_compact_linter_header(&enabled_linters, &linter_hints)
        {
            println!("{}", header);
        }
    } else if !use_json {
        // Human mode: one-line severity-symbol key so readers don't have to
        // infer from context. Always cheap; printed before any finding output.
        println!("{}", output::format_legend());
    }

    if opts.parallel == 1 || opts.files.len() <= 1 {
        // === SEQUENTIAL PATH ===
        // Clear semaphore: no concurrency control needed, and block_on panics
        // inside tokio runtime thread (only safe from spawn_blocking threads).
        {
            let cfg = std::sync::Arc::get_mut(&mut pipeline_cfg).expect("no other Arc refs yet");
            cfg.semaphore = None;
        }
        let parse_cache = cache::ParseCache::new(128);
        let progress = progress::ProgressReporter::detect();

        for file_path in &opts.files {
            if !file_path.exists() {
                eprintln!("Error: File not found: {}", file_path.display());
                had_errors = true;
                continue;
            }

            let lang = parser::Language::from_path(file_path);

            let source = match std::fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: Could not read {}: {}", file_path.display(), e);
                    had_errors = true;
                    continue;
                }
            };

            let file_display = file_path.to_string_lossy().to_string();
            progress.start_file(&file_display);

            // Deep review: agent loop with tool calling
            if opts.deep {
                if let Some(client) = llm_client.as_deref() {
                    let project_root = deep_tool_root(file_path);
                    let tool_reg = tools::ToolRegistry::new(&project_root);
                    let agent_cfg = agent::AgentConfig::default();
                    let model = pipeline_cfg
                        .models
                        .first()
                        .map(|s| s.as_str())
                        .unwrap_or("gpt-5.4");
                    match agent::agent_loop(
                        &source,
                        &file_path.to_string_lossy(),
                        client as &dyn agent::AgentReviewer,
                        model,
                        &tool_reg,
                        &agent_cfg,
                    ) {
                        Ok(findings) => {
                            // Apply project-level suppressions
                            let sup_result = suppress::apply_suppressions(
                                findings,
                                &suppress_rules,
                                &file_display,
                            );
                            if !sup_result.suppressed.is_empty() {
                                tracing::debug!(count = sup_result.suppressed.len(), file = %file_display, "project suppressions applied");
                            }
                            if opts.show_suppressed {
                                for (f, rule) in &sup_result.suppressed {
                                    eprint!("{}", suppress::format_suppressed_finding(f, rule));
                                }
                            }
                            let findings = sup_result.kept;
                            progress.finish_file(findings.len());
                            if use_compact {
                                println!(
                                    "{}",
                                    output::format_compact_review(&file_display, &findings)
                                );
                            } else if use_json {
                                // collected below
                            } else {
                                print!(
                                    "{}",
                                    output::format_review(&file_display, &findings, &style)
                                );
                            }
                            all_findings.extend(findings);
                            continue;
                        }
                        Err(e) => {
                            progress.clear_line();
                            eprintln!(
                                "Warning: Deep review failed for {}: {}. Falling back to standard review.",
                                file_path.display(),
                                e
                            );
                        }
                    }
                }
            }

            // Run pipeline: full (AST + LLM) for supported languages, LLM-only for others
            let review_result = if let Some(l) = lang {
                pipeline::review_source(
                    file_path,
                    &source,
                    l,
                    llm_reviewer,
                    &pipeline_cfg,
                    Some(&parse_cache),
                )
                .await
            } else {
                eprintln!(
                    "Note: No AST support for {}, using LLM-only review",
                    file_path.display()
                );
                pipeline::review_file_llm_only(file_path, &source, llm_reviewer, &pipeline_cfg)
                    .await
            };
            match review_result {
                Ok(mut result) => {
                    // Apply project-level suppressions
                    let file_display = result.file_path.clone();
                    let sup_result = suppress::apply_suppressions(
                        result.findings,
                        &suppress_rules,
                        &file_display,
                    );
                    if !sup_result.suppressed.is_empty() {
                        tracing::debug!(count = sup_result.suppressed.len(), file = %file_display, "project suppressions applied");
                    }
                    if opts.show_suppressed {
                        for (f, rule) in &sup_result.suppressed {
                            eprint!("{}", suppress::format_suppressed_finding(f, rule));
                        }
                    }
                    result.findings = sup_result.kept;
                    progress.finish_file(result.findings.len());
                    if use_compact {
                        println!(
                            "{}",
                            output::format_compact_review(&result.file_path, &result.findings)
                        );
                    } else if !use_json {
                        print!(
                            "{}",
                            output::format_review(&result.file_path, &result.findings, &style)
                        );
                    }
                    all_findings.extend(result.findings.clone());
                    file_results.push(result);
                }
                Err(e) => {
                    progress.clear_line();
                    eprintln!("Error: Review failed for {}: {}", file_path.display(), e);
                    had_errors = true;
                }
            }
        }
    } else {
        // === PARALLEL PATH ===
        let rt = tokio::runtime::Handle::current();
        let mut handles = Vec::new();

        for (idx, file_path) in opts.files.iter().enumerate() {
            let file_path = file_path.clone();
            let pipeline_cfg = pipeline_cfg.clone();
            let suppress_rules = suppress_rules.clone();
            let _show_suppressed = opts.show_suppressed;
            let deep = opts.deep;
            let llm_client = llm_client.clone();

            let handle = rt.spawn_blocking(move || {
                if !file_path.exists() {
                    return (
                        idx,
                        Err(anyhow::anyhow!("File not found: {}", file_path.display())),
                    );
                }
                let source = match std::fs::read_to_string(&file_path) {
                    Ok(s) => s,
                    Err(e) => {
                        return (
                            idx,
                            Err(anyhow::anyhow!(
                                "Could not read {}: {}",
                                file_path.display(),
                                e
                            )),
                        );
                    }
                };
                let lang = parser::Language::from_path(&file_path);
                let file_display = file_path.to_string_lossy().to_string();

                // Deep review path
                if deep {
                    if let Some(ref client) = llm_client {
                        let project_root = deep_tool_root(&file_path);
                        let tool_reg = tools::ToolRegistry::new(&project_root);
                        let agent_cfg = agent::AgentConfig::default();
                        let model = pipeline_cfg
                            .models
                            .first()
                            .map(|s| s.as_str())
                            .unwrap_or("gpt-5.4");
                        match agent::agent_loop(
                            &source,
                            &file_display,
                            &**client as &dyn agent::AgentReviewer,
                            model,
                            &tool_reg,
                            &agent_cfg,
                        ) {
                            Ok(findings) => {
                                let sup_result = suppress::apply_suppressions(
                                    findings,
                                    &suppress_rules,
                                    &file_display,
                                );
                                let result = pipeline::FileReviewResult {
                                    file_path: file_display,
                                    findings: sup_result.kept,
                                    usage: Default::default(),
                                    suppressed: sup_result.suppressed.len(),
                                    context_telemetry: None,
                                    enrichment_metrics: Default::default(),
                                };
                                return (idx, Ok((result, sup_result.suppressed)));
                            }
                            Err(e) => {
                                eprintln!(
                                    "[{}] Warning: Deep review failed: {}. Falling back.",
                                    file_path.display(),
                                    e
                                );
                            }
                        }
                    }
                }

                // Standard review path
                let llm_reviewer: Option<&dyn pipeline::LlmReviewer> =
                    llm_client.as_deref().map(|c| c as _);
                let parse_cache = cache::ParseCache::new(128);
                // `spawn_blocking` runs on Tokio's blocking pool (separate from
                // runtime workers), so `Handle::block_on` here is sound per
                // Tokio docs. We deliberately keep the parsing/AST CPU work
                // inside the spawn_blocking shell and just bridge into the
                // now-async review fns; issue #81.
                let handle = tokio::runtime::Handle::current();
                let review_result = handle.block_on(async {
                    if let Some(l) = lang {
                        pipeline::review_source(
                            &file_path,
                            &source,
                            l,
                            llm_reviewer,
                            &pipeline_cfg,
                            Some(&parse_cache),
                        )
                        .await
                    } else {
                        pipeline::review_file_llm_only(
                            &file_path,
                            &source,
                            llm_reviewer,
                            &pipeline_cfg,
                        )
                        .await
                    }
                });

                match review_result {
                    Ok(mut result) => {
                        let sup_result = suppress::apply_suppressions(
                            result.findings,
                            &suppress_rules,
                            &file_display,
                        );
                        result.findings = sup_result.kept;
                        result.suppressed = sup_result.suppressed.len();
                        (idx, Ok((result, sup_result.suppressed)))
                    }
                    Err(e) => (idx, Err(e)),
                }
            });
            handles.push(handle);
        }

        // Collect results in file order
        type ParResult = (
            pipeline::FileReviewResult,
            Vec<(crate::finding::Finding, suppress::SuppressionRule)>,
        );
        let mut indexed_results: Vec<Option<ParResult>> = vec![None; opts.files.len()];
        for handle in handles {
            match handle.await {
                Ok((idx, Ok(result))) => {
                    indexed_results[idx] = Some(result);
                }
                Ok((idx, Err(e))) => {
                    eprintln!(
                        "Error: Review failed for {}: {}",
                        opts.files[idx].display(),
                        e
                    );
                    had_errors = true;
                }
                Err(e) => {
                    eprintln!("Error: Task panicked: {}", e);
                    had_errors = true;
                }
            }
        }

        // Output in file order (sequential -- no interleaving)
        for result_opt in indexed_results.into_iter() {
            if let Some((result, suppressed_findings)) = result_opt {
                if !suppressed_findings.is_empty() {
                    eprintln!(
                        "Suppressed {} finding(s) in {}",
                        suppressed_findings.len(),
                        result.file_path
                    );
                }
                if opts.show_suppressed {
                    for (f, rule) in &suppressed_findings {
                        eprint!("{}", suppress::format_suppressed_finding(f, rule));
                    }
                }
                if use_compact {
                    println!(
                        "{}",
                        output::format_compact_review(&result.file_path, &result.findings)
                    );
                } else if !use_json {
                    print!(
                        "{}",
                        output::format_review(&result.file_path, &result.findings, &style)
                    );
                }
                all_findings.extend(result.findings.clone());
                file_results.push(result);
            }
        }

        if opts.parallel > 1 && file_results.len() > 1 {
            tracing::debug!(
                files = file_results.len(),
                parallel = opts.parallel,
                "parallel review complete"
            );
        }
    }

    let review_duration = review_start.elapsed();

    // Aggregated end-of-run summary (one line, always printed to stderr).
    {
        let total_suppressed: usize = file_results.iter().map(|r| r.suppressed).sum();
        let total_findings = all_findings.len();
        eprintln!(
            "Reviewed {} file(s) in {:.1}s: {} finding(s){}",
            file_results.len(),
            review_duration.as_secs_f64(),
            total_findings,
            if total_suppressed > 0 {
                format!(", {} suppressed", total_suppressed)
            } else {
                String::new()
            }
        );
    }

    // Record telemetry (best-effort, don't fail the review). Reuses the
    // outer-scope `qhome` resolved via quorum_dir() so review/telemetry/
    // reviews_jsonl all live in the same dir.
    {
        let telem_path = qhome.join("telemetry.jsonl");
        let telem_store = telemetry::TelemetryStore::new(telem_path);
        let mut finding_counts = std::collections::HashMap::new();
        for f in &all_findings {
            let sev = format!("{:?}", f.severity).to_lowercase();
            *finding_counts.entry(sev).or_insert(0usize) += 1;
        }
        let total_tokens_in: u64 = file_results.iter().map(|r| r.usage.prompt_tokens).sum();
        let total_tokens_out: u64 = file_results.iter().map(|r| r.usage.completion_tokens).sum();
        let total_tokens_cache_read: u64 = file_results.iter().map(|r| r.usage.cached_tokens).sum();
        let telem_entry = telemetry::TelemetryEntry {
            ts: chrono::Utc::now(),
            files: opts
                .files
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            findings: finding_counts,
            model: pipeline_cfg.models.first().cloned().unwrap_or_default(),
            tokens_in: total_tokens_in,
            tokens_out: total_tokens_out,
            duration_ms: review_duration.as_millis() as u64,
            suppressed: file_results.iter().map(|r| r.suppressed).sum(),
            context7_resolved: file_results
                .iter()
                .map(|r| r.enrichment_metrics.context7_resolved)
                .sum(),
            context7_resolve_failed: file_results
                .iter()
                .map(|r| r.enrichment_metrics.context7_resolve_failed)
                .sum(),
            context7_query_failed: file_results
                .iter()
                .map(|r| r.enrichment_metrics.context7_query_failed)
                .sum(),
            // #123 Layer 1 (Task 10): adoption telemetry for the FpKind
            // taxonomy. Computed over the loaded feedback store (same one
            // pipeline_cfg.feedback was built from). None when no FP
            // entries exist — utilization is undefined, not zero.
            fp_kind_utilization_rate: feedback::compute_fp_kind_utilization_rate(
                &pipeline_cfg.feedback,
            ),
        };
        let _ = telem_store.record(&telem_entry);

        // Per-review record for dimensional stats (by-repo, by-caller, rolling).
        let reviews_path = qhome.join("reviews.jsonl");
        let review_log = review_log::ReviewLog::new(reviews_path);
        let first_file = opts.files.first().map(|p| p.as_path());
        let repo = first_file.and_then(review_log::detect_repo);
        let invoked_from = review_log::detect_invoked_from(opts.caller.as_deref());
        let sev_iter = all_findings.iter().map(|f| &f.severity);
        // Merge per-file context telemetry into a single review-level
        // record. Counts/durations are summed; thresholds/flags take the
        // last populated value (all files share the same injector config,
        // so they're identical in practice); ID lists are concatenated.
        // When no file reported telemetry, default to semantic zeros
        // (injector_available=false).
        let mut context_telem = review_log::ContextTelemetry::default();
        let mut any_telem = false;
        for r in &file_results {
            if let Some(t) = &r.context_telemetry {
                any_telem = true;
                context_telem.auto_inject_enabled = t.auto_inject_enabled;
                context_telem.injector_available = t.injector_available;
                context_telem.retrieved_chunk_count = context_telem
                    .retrieved_chunk_count
                    .saturating_add(t.retrieved_chunk_count);
                context_telem.injected_chunk_count = context_telem
                    .injected_chunk_count
                    .saturating_add(t.injected_chunk_count);
                context_telem.injected_tokens = context_telem
                    .injected_tokens
                    .saturating_add(t.injected_tokens);
                context_telem.below_threshold_count = context_telem
                    .below_threshold_count
                    .saturating_add(t.below_threshold_count);
                context_telem.adaptive_threshold_applied =
                    context_telem.adaptive_threshold_applied || t.adaptive_threshold_applied;
                context_telem.effective_prose_threshold = t.effective_prose_threshold;
                context_telem
                    .injected_chunk_ids
                    .extend(t.injected_chunk_ids.iter().cloned());
                for s in &t.injected_sources {
                    if !context_telem.injected_sources.iter().any(|x| x == s) {
                        context_telem.injected_sources.push(s.clone());
                    }
                }
                context_telem.precedence_entries = context_telem
                    .precedence_entries
                    .saturating_add(t.precedence_entries);
                context_telem.render_duration_ms = context_telem
                    .render_duration_ms
                    .saturating_add(t.render_duration_ms);
                context_telem
                    .retrieved_by_leg
                    .saturating_add(&t.retrieved_by_leg);
                context_telem
                    .injected_by_leg
                    .saturating_add(&t.injected_by_leg);
                context_telem.nan_scores_dropped = context_telem
                    .nan_scores_dropped
                    .saturating_add(t.nan_scores_dropped);
                context_telem.suppressed_by_floor = context_telem
                    .suppressed_by_floor
                    .saturating_add(t.suppressed_by_floor);
                context_telem.suppressed_by_calibrator = context_telem
                    .suppressed_by_calibrator
                    .saturating_add(t.suppressed_by_calibrator);
                if context_telem.rerank_score_min.is_none() {
                    context_telem.rerank_score_min = t.rerank_score_min;
                }
                if context_telem.rerank_score_p10.is_none() {
                    context_telem.rerank_score_p10 = t.rerank_score_p10;
                }
                if context_telem.rerank_score_median.is_none() {
                    context_telem.rerank_score_median = t.rerank_score_median;
                }
                if context_telem.rerank_score_p90.is_none() {
                    context_telem.rerank_score_p90 = t.rerank_score_p90;
                }
                // Keep the first non-None hash; if any file rendered a
                // block, we have a representative hash. When multiple
                // files inject, they're distinct blocks — we expose the
                // first one as a sample.
                if context_telem.rendered_prompt_hash.is_none() && t.rendered_prompt_hash.is_some()
                {
                    context_telem.rendered_prompt_hash = t.rendered_prompt_hash.clone();
                }
            }
        }
        if !any_telem {
            context_telem = review_log::ContextTelemetry::default();
        }

        let record = review_log::ReviewRecord {
            run_id: review_log::ReviewRecord::new_ulid(),
            timestamp: chrono::Utc::now(),
            quorum_version: env!("CARGO_PKG_VERSION").to_string(),
            repo,
            invoked_from,
            model: pipeline_cfg.models.first().cloned().unwrap_or_default(),
            files_reviewed: opts.files.len() as u32,
            lines_added: None, // diff instrumentation: future work
            lines_removed: None,
            findings_by_severity: review_log::SeverityCounts::from_severities(sev_iter),
            suppressed_by_rule: std::collections::HashMap::new(), // per-rule breakdown: future work
            tokens_in: total_tokens_in,
            tokens_out: total_tokens_out,
            tokens_cache_read: total_tokens_cache_read,
            duration_ms: review_duration.as_millis() as u64,
            flags: review_log::Flags {
                deep: opts.deep,
                parallel_n: opts.parallel as u32,
                ensemble: opts.ensemble,
            },
            context: context_telem,
        };
        if let Err(e) = review_log.record(&record) {
            eprintln!("Warning: failed to write review log: {}", e);
        }
    }

    // If all files had errors and no findings, exit with tool error
    if had_errors && all_findings.is_empty() {
        if use_json {
            println!("[]");
        }
        return 3;
    }

    if use_json {
        match output::format_json_grouped_with_meta(&file_results, &enabled_linters, &linter_hints)
        {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("Error: JSON serialization failed: {}", e);
                return 3;
            }
        }
    } else if !use_compact {
        // Human mode: surface coverage gaps on stderr after the summary line.
        for line in output::format_hints_human(&linter_hints) {
            eprintln!("{}", line);
        }
    }

    output::compute_exit_code(&all_findings)
}

/// Relevance gate: a detected linter is only worth surfacing in this review's
/// status when its language is present in `exts`. Avoids dragging clippy into
/// a review of only Python files just because Cargo.toml exists at the root.
fn linter_kind_is_relevant(
    kind: &linter::LinterKind,
    exts: &std::collections::HashSet<String>,
) -> bool {
    use linter::LinterKind::*;
    match kind {
        Ruff => exts.contains("py"),
        Clippy => exts.contains("rs"),
        Eslint => ["ts", "tsx", "js", "jsx", "mjs", "cjs"]
            .iter()
            .any(|e| exts.contains(*e)),
        Yamllint => exts.contains("yaml") || exts.contains("yml"),
        Shellcheck => ["sh", "bash", "zsh", "bats"]
            .iter()
            .any(|e| exts.contains(*e)),
        Hadolint => exts.iter().any(|e| e == "dockerfile") || exts.contains(""),
        Tflint => exts.contains("tf") || exts.contains("tfvars"),
    }
}

async fn run_daemon(opts: cli::DaemonOpts) -> anyhow::Result<()> {
    use tokio::sync::mpsc;

    let watch_dir = opts
        .watch_dir
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    eprintln!("quorum daemon starting");
    eprintln!("  Port: {}", opts.port);
    eprintln!("  Watching: {}", watch_dir.display());
    eprintln!("  Cache capacity: {}", opts.cache_size);

    let state = http_server::create_daemon_state(opts.cache_size)?;

    // Start file watcher
    let (tx, rx) = mpsc::unbounded_channel();
    let _watcher = daemon::start_watcher(&watch_dir, tx).ok();
    let cache_for_watcher = state.parse_cache.clone();
    tokio::spawn(async move {
        daemon::run_event_loop(rx, cache_for_watcher).await;
    });

    // Build HTTP server
    let app = http_server::build_router(state.clone());
    let addr = format!("127.0.0.1:{}", opts.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("  Listening on http://{}", addr);
    eprintln!("  Ready. Press Ctrl+C to stop.");

    // Serve until Ctrl+C
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;

    let stats = state.parse_cache.stats();
    eprintln!(
        "Daemon stopped. Cache: {} hits, {} misses, {:.0}% hit rate",
        stats.hits,
        stats.misses,
        stats.hit_rate() * 100.0
    );
    Ok(())
}

fn run_review_via_daemon(opts: &cli::ReviewOpts) -> i32 {
    let client = reqwest::blocking::Client::new();
    let base = format!("http://127.0.0.1:{}", opts.daemon_port);

    // Check if daemon is running
    match client.get(format!("{}/health", base)).send() {
        Ok(resp) if resp.status().is_success() => {}
        _ => {
            eprintln!(
                "Error: Daemon not running on port {}. Start with: quorum daemon",
                opts.daemon_port
            );
            eprintln!("Falling back to local review.");
            // Fall through to local review by calling run_review without --daemon
            // For simplicity, just return 3 to indicate tool error
            return 3;
        }
    }

    let use_json = opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout());
    let style = output::Style::detect(opts.no_color);
    let mut all_findings = Vec::new();

    for file_path in &opts.files {
        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: Could not read {}: {}", file_path.display(), e);
                continue;
            }
        };

        let body = serde_json::json!({
            "file_path": file_path.to_string_lossy(),
            "code": source,
        });

        match client.post(format!("{}/review", base)).json(&body).send() {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(review) = resp.json::<http_server::ReviewResponse>() {
                    let cache_note = if review.cache_hit { " (cached)" } else { "" };
                    if !use_json {
                        let file_str = file_path.to_string_lossy();
                        eprint!("{}{}", file_str, cache_note);
                        eprintln!();
                        print!(
                            "{}",
                            output::format_review(&file_str, &review.findings, &style)
                        );
                    }
                    all_findings.extend(review.findings);
                }
            }
            Ok(resp) => {
                eprintln!("Error: Daemon returned {}", resp.status());
            }
            Err(e) => {
                eprintln!("Error: Failed to connect to daemon: {}", e);
                return 3;
            }
        }
    }

    if use_json {
        match output::format_json(&all_findings) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("Error: {}", e);
                return 3;
            }
        }
    }

    output::compute_exit_code(&all_findings)
}

/// Core feedback logic -- testable with custom feedback path.
/// `json`: explicit `--json` flag (when true, force JSON output even on a TTY).
#[allow(clippy::too_many_arguments)]
fn run_feedback_inner(
    file: &str,
    finding: &str,
    verdict_str: &str,
    reason: &str,
    model: Option<&str>,
    blamed_chunks: Option<&str>,
    category: Option<&str>,
    fp_kind: Option<feedback::FpKind>,
    json: bool,
    feedback_path: &std::path::Path,
) -> (i32, String) {
    let mut verdict = match cli::parse_verdict(verdict_str) {
        Ok(v) => v,
        Err(e) => {
            return (3, format!("Error: {}", e));
        }
    };

    // Merge --blamed-chunks into a ContextMisleading verdict. For other
    // verdicts, silently ignore the flag (the plan says we shouldn't error
    // on spurious use — existing validation behavior is unchanged).
    let parsed_chunks = match cli::parse_blamed_chunks(blamed_chunks) {
        Ok(v) => v,
        Err(e) => {
            return (3, format!("Error: {}", e));
        }
    };
    if let feedback::Verdict::ContextMisleading { blamed_chunk_ids } = &mut verdict {
        *blamed_chunk_ids = parsed_chunks;
    }

    let entry = feedback::FeedbackEntry {
        file_path: file.to_string(),
        finding_title: finding.to_string(),
        // Mirror record_external/MCP normalization: trim and treat blank as
        // missing so analytics buckets don't fragment by ingestion path.
        finding_category: category
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("manual")
            .to_string(),
        verdict: verdict.clone(),
        reason: reason.to_string(),
        model: model.map(|s| s.to_string()),
        timestamp: chrono::Utc::now(),
        provenance: feedback::Provenance::Human,
        fp_kind,
    };

    let store = feedback::FeedbackStore::new(feedback_path.to_path_buf());
    if let Err(e) = store.record(&entry) {
        return (3, format!("Error: Failed to write feedback: {}", e));
    }

    let total = store.count().unwrap_or(0);
    let verdict_label = match &entry.verdict {
        feedback::Verdict::Tp => "tp".to_string(),
        feedback::Verdict::Fp => "fp".to_string(),
        feedback::Verdict::Partial => "partial".to_string(),
        feedback::Verdict::Wontfix => "wontfix".to_string(),
        feedback::Verdict::ContextMisleading { .. } => "context_misleading".to_string(),
    };

    // Format output based on mode. Explicit `--json` wins over TTY
    // detection; otherwise we only fall into JSON when stdout is a pipe
    // and compact mode hasn't been requested.
    let use_compact = output::should_use_compact(false);
    let use_json = json || (!use_compact && !std::io::IsTerminal::is_terminal(&std::io::stdout()));

    let output = if use_json {
        let json_obj = serde_json::json!({
            "verdict": verdict_label,
            "file_path": entry.file_path,
            "finding_title": entry.finding_title,
            "total": total,
        });
        serde_json::to_string(&json_obj).unwrap_or_default()
    } else if use_compact {
        format!(
            "feedback:{}|{}|{}",
            verdict_label, entry.file_path, entry.finding_title
        )
    } else {
        format!(
            "Recorded: {} for \"{}\" in {} ({} entries)",
            verdict_label, entry.finding_title, entry.file_path, total,
        )
    };

    (0, output)
}

/// CLI entry point for `quorum feedback`.
fn run_feedback(opts: cli::FeedbackOpts) -> i32 {
    let feedback_path = quorum_dir()
        .map(|d| d.join("feedback.jsonl"))
        .unwrap_or_else(|| std::path::PathBuf::from(".quorum/feedback.jsonl"));

    // External-agent path: branch when --from-agent is provided. Uses
    // record_external so Provenance::External is serialized, bypassing the
    // default Human path.
    if let Some(agent) = opts.from_agent.as_deref() {
        let verdict = match cli::parse_verdict(&opts.verdict) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: {}", e);
                return 3;
            }
        };
        let input = feedback::ExternalVerdictInput {
            file_path: opts.file.clone(),
            finding_title: opts.finding.clone(),
            finding_category: opts.category.clone(),
            verdict,
            reason: opts.reason.clone(),
            agent: agent.to_string(),
            agent_model: opts.agent_model.clone(),
            confidence: opts.confidence,
        };
        if let Some(parent) = feedback_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store = feedback::FeedbackStore::new(feedback_path);
        match store.record_external(input) {
            Ok(_) => {
                // Match run_feedback_inner's output contract: compact when
                // CLAUDE_CODE / non-tty piped detection wants it; JSON when
                // piped without compact override; human text on a TTY.
                let total = store.count().unwrap_or(0);
                let use_compact = output::should_use_compact(false);
                // Honor explicit --json even on a TTY, matching the CLI
                // contract; fall back to TTY detection when the flag is off.
                let use_json = opts.json
                    || (!use_compact && !std::io::IsTerminal::is_terminal(&std::io::stdout()));
                let verdict_lower = opts.verdict.to_lowercase();
                let verdict_label: &str = match verdict_lower.as_str() {
                    "tp" => "tp",
                    "fp" => "fp",
                    "partial" => "partial",
                    "wontfix" => "wontfix",
                    "context_misleading" => "context_misleading",
                    _ => verdict_lower.as_str(),
                };
                if use_json {
                    let json_obj = serde_json::json!({
                        "verdict": verdict_label,
                        "file_path": opts.file,
                        "finding_title": opts.finding,
                        "agent": agent,
                        "provenance": "external",
                        "total": total,
                    });
                    println!("{}", serde_json::to_string(&json_obj).unwrap_or_default());
                } else if use_compact {
                    println!(
                        "feedback:{}|{}|{}|external:{}",
                        verdict_label, opts.file, opts.finding, agent
                    );
                } else {
                    println!(
                        "Recorded external verdict from agent {} ({} entries).",
                        agent, total
                    );
                }
                0
            }
            Err(e) => {
                eprintln!("Error: Failed to record external verdict: {}", e);
                3
            }
        }
    } else {
        if let Some(parent) = feedback_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Derive fp_kind from CLI flags. Errors only when verdict=fp and
        // a kind was specified that requires associated data (e.g.
        // compensating-control needs --fp-reference). Returns None silently
        // when verdict != fp; warn the user so the dropped flag is visible.
        let fp_kind = match opts.into_fp_kind() {
            Ok(k) => {
                if opts.fp_kind.is_some() && k.is_none() {
                    tracing::warn!(
                        "--fp-kind was provided but verdict is not fp; ignoring the flag"
                    );
                }
                k
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                return 3;
            }
        };
        let (exit_code, output) = run_feedback_inner(
            &opts.file,
            &opts.finding,
            &opts.verdict,
            &opts.reason,
            opts.model.as_deref(),
            opts.blamed_chunks.as_deref(),
            opts.category.as_deref(),
            fp_kind,
            opts.json,
            &feedback_path,
        );
        if exit_code != 0 {
            eprintln!("{}", output);
        } else {
            println!("{}", output);
        }
        exit_code
    }
}

/// Load a JSONL file line-by-line, skipping unparseable lines.
/// Returns `Ok(vec![])` for missing files, propagates other I/O errors.
fn load_jsonl(path: &std::path::Path) -> Result<Vec<serde_json::Value>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => entries.push(v),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            path = %path.display(),
            skipped,
            "skipped malformed JSONL lines"
        );
    }
    Ok(entries)
}

/// CLI entry point for `quorum calibrate`.
fn run_calibrate(opts: cli::CalibrateOpts) -> i32 {
    if !(0.0..=1.0).contains(&opts.suppress_precision)
        || !(0.0..=1.0).contains(&opts.boost_precision)
    {
        eprintln!("error: precision values must be between 0.0 and 1.0");
        return 3;
    }

    let Some(qhome) = quorum_dir() else {
        eprintln!("error: cannot determine quorum home directory");
        return 3;
    };

    let feedback_path = qhome.join("feedback.jsonl");
    let traces_path = qhome.join("calibrator_traces.jsonl");

    let feedback = match load_jsonl(&feedback_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    let traces = match load_jsonl(&traces_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 3;
        }
    };

    eprintln!(
        "Loaded {} feedback entries, {} trace entries",
        feedback.len(),
        traces.len(),
    );

    let samples = quorum::calibrate::join_feedback_and_traces(&feedback, &traces);
    let positives = samples.iter().filter(|(_, l)| *l).count();
    let negatives = samples.len() - positives;

    eprintln!(
        "Joined corpus: {} samples ({} TP/partial, {} FP)",
        samples.len(),
        positives,
        negatives,
    );

    let config = quorum::calibrate::compute_thresholds(
        &samples,
        opts.suppress_precision,
        opts.boost_precision,
    );

    // Print summary
    println!("--- Calibrator Threshold Report ---");
    println!("Corpus size:    {}", samples.len());
    println!("Class balance:  {} TP, {} FP", positives, negatives);
    println!(
        "Precision targets: suppress={:.2}, boost={:.2}",
        opts.suppress_precision, opts.boost_precision
    );
    println!();

    if let Some(ref s) = config.suppress {
        println!(
            "Suppress: threshold={:.4} (precision_target={:.2})",
            s.threshold, s.precision_target
        );
    } else {
        println!("Suppress: not computed (insufficient data or precision target unachievable)");
    }

    if let Some(ref b) = config.boost {
        println!(
            "Boost:    threshold={:.4} (precision_target={:.2})",
            b.threshold, b.precision_target
        );
    } else {
        println!("Boost:    not computed (insufficient data or precision target unachievable)");
    }

    if opts.dry_run {
        eprintln!("\n(dry run -- no file written)");
    } else if config.suppress.is_none() && config.boost.is_none() {
        eprintln!("\nNo thresholds computed (insufficient data). Existing config preserved.");
    } else {
        let toml_path = qhome.join("calibrator_thresholds.toml");
        let toml_str = config.to_toml();
        if let Err(e) = std::fs::create_dir_all(&qhome) {
            eprintln!("\nerror: failed to create {}: {e}", qhome.display());
            return 3;
        }
        match std::fs::write(&toml_path, &toml_str) {
            Ok(()) => {
                eprintln!("\nWrote {}", toml_path.display());
            }
            Err(e) => {
                eprintln!("\nerror: failed to write {}: {}", toml_path.display(), e);
                return 3;
            }
        }
    }

    0
}

#[cfg(test)]
mod threshold_loading_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_loads_thresholds_from_toml_into_calibrator() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calibrator_thresholds.toml");
        std::fs::write(
            &path,
            "[suppress]\nprecision_target = 0.95\nthreshold = 0.30\n\n[boost]\nprecision_target = 0.85\nthreshold = 0.78\n",
        )
        .unwrap();
        let tc = quorum::threshold_config::ThresholdConfig::load_from(path.to_str().unwrap());
        assert!(tc.is_some(), "TOML should load successfully");
        let tc = tc.unwrap();

        let mut calibrator_config = calibrator::CalibratorConfig::default();
        calibrator_config.suppress_threshold = tc.suppress.map(|p| p.threshold);
        calibrator_config.boost_threshold = tc.boost.map(|p| p.threshold);

        assert!(
            (calibrator_config.suppress_threshold.unwrap() - 0.30).abs() < 1e-9,
            "suppress_threshold should be loaded from TOML"
        );
        assert!(
            (calibrator_config.boost_threshold.unwrap() - 0.78).abs() < 1e-9,
            "boost_threshold should be loaded from TOML"
        );
    }

    #[test]
    fn missing_toml_leaves_config_at_defaults() {
        let tc =
            quorum::threshold_config::ThresholdConfig::load_from("/nonexistent/thresholds.toml");
        assert!(tc.is_none());

        let config = calibrator::CalibratorConfig::default();
        assert!(config.suppress_threshold.is_none());
        assert!(config.boost_threshold.is_none());
        assert!(config.force_threshold.is_none());
    }

    #[test]
    fn force_threshold_env_override_applies() {
        let mut config = calibrator::CalibratorConfig::default();
        // Simulate QUORUM_FORCE_THRESHOLD env var parsing
        let force_val = "0.65";
        if let Ok(t) = force_val.parse::<f64>() {
            config.force_threshold = Some(t);
        }
        assert!(
            (config.force_threshold.unwrap() - 0.65).abs() < 1e-9,
            "force_threshold should be set from parsed env value"
        );
    }
}

#[cfg(test)]
mod deep_tool_root_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn deep_tool_root_uses_files_project_root_not_cwd() {
        // Create a fake project with a Cargo.toml marker and a source file.
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("proj");
        let src = project.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let file = src.join("lib.rs");
        std::fs::write(&file, "").unwrap();

        // Helper must return project root, NOT current_dir.
        let root = deep_tool_root(&file);
        assert_eq!(root, project, "tool root should match file's project root");
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn feedback_records_tp_verdict() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _output) = run_feedback_inner(
            "src/auth.rs",
            "SQL injection",
            "tp",
            "Fixed with params",
            None,
            None,
            None,
            None,
            false,
            &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("SQL injection"));
        assert!(contents.contains("\"verdict\":\"tp\""));
        assert!(contents.contains("src/auth.rs"));
    }

    #[test]
    fn feedback_invalid_verdict_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, output) = run_feedback_inner(
            "src/auth.rs",
            "SQL injection",
            "maybe",
            "Not sure",
            None,
            None,
            None,
            None,
            false,
            &path,
        );
        assert_eq!(exit_code, 3);
        assert!(output.contains("Invalid verdict"));
    }

    #[test]
    fn feedback_provenance_is_human() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_inner(
            "src/auth.rs",
            "SQL injection",
            "tp",
            "Real issue",
            None,
            None,
            None,
            None,
            false,
            &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"provenance\":\"human\""));
    }

    #[test]
    fn feedback_category_is_manual() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_inner(
            "src/auth.rs",
            "Test finding",
            "fp",
            "Not real",
            None,
            None,
            None,
            None,
            false,
            &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"finding_category\":\"manual\""));
    }

    #[test]
    fn feedback_output_contains_key_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (_, output) = run_feedback_inner(
            "src/auth.rs",
            "SQL injection",
            "tp",
            "Fixed",
            None,
            None,
            None,
            None,
            false,
            &path,
        );
        assert!(output.contains("tp"));
        assert!(output.contains("src/auth.rs"));
        assert!(output.contains("SQL injection"));
    }

    #[test]
    fn feedback_json_output_parseable() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, output) = run_feedback_inner(
            "src/auth.rs",
            "SQL injection",
            "fp",
            "Not a real issue",
            None,
            None,
            None,
            None,
            false,
            &path,
        );
        assert_eq!(exit_code, 0);
        // In test environment stdout is not a TTY, so output should be JSON
        if output.starts_with('{') {
            let v: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(v["verdict"], "fp");
            assert_eq!(v["file_path"], "src/auth.rs");
            assert_eq!(v["finding_title"], "SQL injection");
            assert!(v["total"].is_number());
        }
    }

    #[test]
    fn feedback_with_model() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_inner(
            "src/auth.rs",
            "Test finding",
            "tp",
            "Real",
            Some("gpt-5.4"),
            None,
            None,
            None,
            false,
            &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("gpt-5.4"));
    }

    #[test]
    fn feedback_cli_records_context_misleading_with_blamed_chunks() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_inner(
            "src/auth.rs",
            "Missing null check",
            "context_misleading",
            "Injected context described v1 API, code uses v2",
            None,
            Some("chunk-abc,chunk-def"),
            None,
            None, // fp_kind
            false,
            &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        // Serialized struct variant: {"context_misleading":{"blamed_chunk_ids":[...]}}
        assert!(
            contents.contains("context_misleading"),
            "verdict tag missing; got: {}",
            contents
        );
        assert!(
            contents.contains("chunk-abc"),
            "first chunk id missing; got: {}",
            contents
        );
        assert!(
            contents.contains("chunk-def"),
            "second chunk id missing; got: {}",
            contents
        );

        // Round-trip through the store to assert exact structure.
        let store = feedback::FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].verdict {
            feedback::Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert_eq!(
                    blamed_chunk_ids,
                    &vec!["chunk-abc".to_string(), "chunk-def".to_string()]
                );
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
        }
    }

    #[test]
    fn feedback_cli_rejects_empty_entry_in_blamed_chunks() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, output) = run_feedback_inner(
            "src/auth.rs",
            "Missing null check",
            "context_misleading",
            "r",
            None,
            Some("a,,b"),
            None,
            None, // fp_kind
            false,
            &path,
        );
        assert_eq!(exit_code, 3, "expected tool error on malformed chunk list");
        assert!(
            output.to_lowercase().contains("empty"),
            "error must mention empty entry; got: {}",
            output
        );
        // Nothing should have been written.
        assert!(!path.exists() || std::fs::read_to_string(&path).unwrap().is_empty());
    }

    #[test]
    fn feedback_cli_context_misleading_with_no_blamed_chunks_uses_empty_vec() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_inner(
            "src/auth.rs",
            "Missing null check",
            "context_misleading",
            "No specific chunks identified",
            None,
            None, // user omitted --blamed-chunks entirely
            None,
            None, // fp_kind
            false,
            &path,
        );
        assert_eq!(
            exit_code, 0,
            "omitted --blamed-chunks must succeed with an empty default"
        );

        let store = feedback::FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].verdict {
            feedback::Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert!(
                    blamed_chunk_ids.is_empty(),
                    "absent flag must produce empty Vec, not populate with a placeholder"
                );
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
        }
    }
}
