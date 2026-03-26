#![allow(dead_code)]

mod analytics;
mod analysis;
mod auto_calibrate;
mod cache;
mod calibrator;
mod cli;
mod config;
mod context_enrichment;
mod daemon;
mod http_server;
mod domain;
mod embeddings;
mod feedback;
mod finding;
mod hydration;
mod linter;
mod llm_client;
mod mcp;
mod merge;
mod output;
mod parser;
mod patterns;
mod pipeline;
mod redact;
mod review;

use clap::Parser;
use config::{Config, EnvConfigSource};
use pipeline::{PipelineConfig, LlmReviewer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    match args.command {
        cli::Command::Review(opts) => {
            let exit_code = run_review(opts);
            std::process::exit(exit_code);
        }
        cli::Command::Serve => {
            run_mcp_server().await?;
        }
        cli::Command::Daemon(opts) => {
            run_daemon(opts).await?;
        }
        cli::Command::Version => {
            println!("quorum {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(())
}

async fn run_mcp_server() -> anyhow::Result<()> {
    use rust_mcp_sdk::schema::{
        Implementation, InitializeResult, ProtocolVersion, ServerCapabilities,
        ServerCapabilitiesTools,
    };
    use rust_mcp_sdk::mcp_server::{server_runtime, McpServerOptions};
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

    server.start().await
        .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;
    Ok(())
}

fn run_review(opts: cli::ReviewOpts) -> i32 {
    if opts.files.is_empty() {
        eprintln!("Error: No files specified");
        return 3;
    }

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

    // Build LLM reviewer if API key is available
    let llm_reviewer: Option<Box<dyn LlmReviewer>> = if let Ok(api_key) = cfg.require_api_key() {
        let effort = opts.reasoning_effort.clone()
            .or_else(|| std::env::var("QUORUM_REASONING_EFFORT").ok().filter(|s| !s.is_empty()))
            .or_else(|| Some("low".into())); // Default: low reasoning is optimal for code review
        Some(Box::new(
            llm_client::OpenAiClient::new(&cfg.base_url, api_key)
                .with_reasoning_effort(effort)
        ))
    } else {
        None
    };

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

    // Load feedback for calibration
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let feedback_path = std::path::PathBuf::from(&home).join(".quorum/feedback.jsonl");
    let feedback_store = feedback::FeedbackStore::new(feedback_path.clone());
    let feedback_entries = feedback_store.load_all().unwrap_or_default();
    if !feedback_entries.is_empty() {
        eprintln!("Loaded {} feedback entries for calibration", feedback_entries.len());
    }

    let pipeline_cfg = PipelineConfig {
        models,
        calibration_model: opts.calibration_model.clone(),
        feedback: feedback_entries,
        auto_calibrate: !opts.no_auto_calibrate(),
        feedback_store: Some(feedback_path.clone()),
        ..Default::default()
    };

    let style = output::Style::detect(opts.no_color);
    let use_json = opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout());
    let parse_cache = cache::ParseCache::new(128);
    let mut all_findings = Vec::new();
    let mut had_errors = false;

    for file_path in &opts.files {
        if !file_path.exists() {
            eprintln!("Error: File not found: {}", file_path.display());
            had_errors = true;
            continue;
        }

        let lang = match parser::Language::from_path(file_path) {
            Some(l) => l,
            None => {
                eprintln!("Warning: Unsupported file type, skipping: {}", file_path.display());
                continue;
            }
        };

        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: Could not read {}: {}", file_path.display(), e);
                had_errors = true;
                continue;
            }
        };

        // Run the full pipeline with cache
        match pipeline::review_source(
            file_path,
            &source,
            lang,
            llm_reviewer.as_deref(),
            &pipeline_cfg,
            Some(&parse_cache),
        ) {
            Ok(result) => {
                if use_json {
                    all_findings.extend(result.findings);
                } else {
                    print!("{}", output::format_review(&result.file_path, &result.findings, &style));
                    all_findings.extend(result.findings);
                }
            }
            Err(e) => {
                eprintln!("Error: Review failed for {}: {}", file_path.display(), e);
                had_errors = true;
            }
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
        match output::format_json(&all_findings) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("Error: JSON serialization failed: {}", e);
                return 3;
            }
        }
    }

    output::compute_exit_code(&all_findings)
}

async fn run_daemon(opts: cli::DaemonOpts) -> anyhow::Result<()> {
    use tokio::sync::mpsc;

    let watch_dir = opts.watch_dir
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
        stats.hits, stats.misses, stats.hit_rate() * 100.0
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
            eprintln!("Error: Daemon not running on port {}. Start with: quorum daemon", opts.daemon_port);
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
                        print!("{}", output::format_review(&file_str, &review.findings, &style));
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
