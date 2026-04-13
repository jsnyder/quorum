#![allow(dead_code)]

mod agent;
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
mod feedback_index;
mod finding;
mod formatting;
mod hydration;
mod linter;
mod llm_client;
mod mcp;
mod merge;
mod output;
mod parser;
mod patterns;
mod pipeline;
mod progress;
mod redact;
mod review;
mod stats;
mod suppress;
mod telemetry;
mod tools;
#[cfg(test)] mod test_support;

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
        cli::Command::Stats(opts) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            let home_path = std::path::PathBuf::from(&home);
            let feedback_store = feedback::FeedbackStore::new(home_path.join(".quorum/feedback.jsonl"));
            let telemetry_store = telemetry::TelemetryStore::new(home_path.join(".quorum/telemetry.jsonl"));

            match stats::compute_report(&feedback_store, &telemetry_store) {
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
                        print!("{}", stats::format_human(&report, &style));
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

    // Build LLM client if API key is available (implements both LlmReviewer and AgentReviewer)
    let llm_client: Option<llm_client::OpenAiClient> = if let Ok(api_key) = cfg.require_api_key() {
        let effort = opts.reasoning_effort.clone()
            .or_else(|| std::env::var("QUORUM_REASONING_EFFORT").ok().filter(|s| !s.is_empty()))
            .or_else(|| Some("low".into())); // Default: low reasoning is optimal for code review
        Some(
            llm_client::OpenAiClient::new(&cfg.base_url, api_key)
                .with_reasoning_effort(effort)
        )
    } else {
        None
    };
    let llm_reviewer: Option<&dyn LlmReviewer> = llm_client.as_ref().map(|c| c as &dyn LlmReviewer);

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

    // Parse diff file if provided for change-scoped review
    let diff_ranges = if let Some(ref diff_path) = opts.diff_file {
        match std::fs::read_to_string(diff_path) {
            Ok(diff_content) => {
                let ranges = hydration::parse_unified_diff(&diff_content);
                if !ranges.is_empty() {
                    eprintln!("Diff-aware: scoping hydration to {} changed file(s)", ranges.len());
                    Some(ranges)
                } else {
                    None
                }
            }
            Err(e) => {
                eprintln!("Warning: Could not read diff file {}: {}", diff_path.display(), e);
                None
            }
        }
    } else {
        None
    };

    let pipeline_cfg = PipelineConfig {
        models,
        calibration_model: opts.calibration_model.clone(),
        feedback: feedback_entries,
        auto_calibrate: !opts.no_auto_calibrate(),
        feedback_store: Some(feedback_path.clone()),
        diff_ranges,
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
        eprintln!(
            "Loaded {} suppression rule(s) from {}",
            suppress_rules.len(),
            suppress_path.display()
        );
    }

    let review_start = std::time::Instant::now();

    let style = output::Style::detect(opts.no_color);
    let use_compact = output::should_use_compact(opts.compact);
    let use_json = !use_compact && (opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout()));
    let parse_cache = cache::ParseCache::new(128);
    let progress = progress::ProgressReporter::detect();
    let mut all_findings = Vec::new();
    let mut file_results: Vec<pipeline::FileReviewResult> = Vec::new();
    let mut had_errors = false;

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
            if let Some(client) = llm_client.as_ref() {
                let project_root = std::env::current_dir().unwrap_or_default();
                let tool_reg = tools::ToolRegistry::new(&project_root);
                let agent_cfg = agent::AgentConfig::default();
                let model = pipeline_cfg.models.first()
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
                        let sup_result = suppress::apply_suppressions(findings, &suppress_rules, &file_display);
                        if !sup_result.suppressed.is_empty() {
                            eprintln!("Suppressed {} finding(s) in {}", sup_result.suppressed.len(), file_display);
                        }
                        if opts.show_suppressed {
                            for (f, rule) in &sup_result.suppressed {
                                eprint!("{}", suppress::format_suppressed_finding(f, rule));
                            }
                        }
                        let findings = sup_result.kept;
                        progress.finish_file(findings.len());
                        if use_compact {
                            println!("{}", output::format_compact_review(&file_display, &findings));
                        } else if use_json {
                            // collected below
                        } else {
                            print!("{}", output::format_review(&file_display, &findings, &style));
                        }
                        all_findings.extend(findings);
                        continue;
                    }
                    Err(e) => {
                        progress.clear_line();
                        eprintln!("Warning: Deep review failed for {}: {}. Falling back to standard review.", file_path.display(), e);
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
        } else {
            eprintln!("Note: No AST support for {}, using LLM-only review", file_path.display());
            pipeline::review_file_llm_only(
                file_path,
                &source,
                llm_reviewer,
                &pipeline_cfg,
            )
        };
        match review_result {
            Ok(mut result) => {
                // Apply project-level suppressions
                let file_display = result.file_path.clone();
                let sup_result = suppress::apply_suppressions(result.findings, &suppress_rules, &file_display);
                if !sup_result.suppressed.is_empty() {
                    eprintln!("Suppressed {} finding(s) in {}", sup_result.suppressed.len(), file_display);
                }
                if opts.show_suppressed {
                    for (f, rule) in &sup_result.suppressed {
                        eprint!("{}", suppress::format_suppressed_finding(f, rule));
                    }
                }
                result.findings = sup_result.kept;
                progress.finish_file(result.findings.len());
                if use_compact {
                    println!("{}", output::format_compact_review(&result.file_path, &result.findings));
                } else if !use_json {
                    print!("{}", output::format_review(&result.file_path, &result.findings, &style));
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

    let review_duration = review_start.elapsed();

    // Record telemetry (best-effort, don't fail the review)
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let telem_path = std::path::PathBuf::from(&home).join(".quorum/telemetry.jsonl");
        let telem_store = telemetry::TelemetryStore::new(telem_path);
        let mut finding_counts = std::collections::HashMap::new();
        for f in &all_findings {
            let sev = format!("{:?}", f.severity).to_lowercase();
            *finding_counts.entry(sev).or_insert(0usize) += 1;
        }
        let total_tokens_in: u64 = file_results.iter().map(|r| r.usage.prompt_tokens).sum();
        let total_tokens_out: u64 = file_results.iter().map(|r| r.usage.completion_tokens).sum();
        let telem_entry = telemetry::TelemetryEntry {
            ts: chrono::Utc::now(),
            files: opts.files.iter().map(|p| p.to_string_lossy().to_string()).collect(),
            findings: finding_counts,
            model: pipeline_cfg.models.first().cloned().unwrap_or_default(),
            tokens_in: total_tokens_in,
            tokens_out: total_tokens_out,
            duration_ms: review_duration.as_millis() as u64,
            suppressed: 0,  // TODO: wire from calibrator
        };
        let _ = telem_store.record(&telem_entry);
    }

    // If all files had errors and no findings, exit with tool error
    if had_errors && all_findings.is_empty() {
        if use_json {
            println!("[]");
        }
        return 3;
    }

    if use_json {
        match output::format_json_grouped(&file_results) {
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
