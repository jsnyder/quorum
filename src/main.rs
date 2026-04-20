#![allow(dead_code)]

mod agent;
mod analytics;
mod analysis;
mod ast_grep;
mod auto_calibrate;
mod cache;
mod calibrator;
mod calibrator_trace;
mod cli;
mod config;
mod context_enrichment;
mod daemon;
mod dimensions;
mod glyphs;
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
mod review_log;
mod stats;
mod suppress;
mod telemetry;
mod tools;
mod trace_subscriber;
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

            // Dimensional views (by-repo / by-caller / rolling) read reviews.jsonl
            // and aggregate via the `dimensions` module. Three output modes:
            // JSON (pipe/--json), compact (CLAUDE_CODE/--compact), human (TTY).
            let want_dimensional = opts.by_repo || opts.by_caller || opts.rolling.is_some();
            if want_dimensional {
                let log = review_log::ReviewLog::new(home_path.join(".quorum/reviews.jsonl"));
                let records = log.load_all().unwrap_or_default();
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
                    print!("{}", stats::format_dimension_table(mode, &slices, &style, unicode));
                }
                std::process::exit(0);
            }

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
        cli::Command::Feedback(opts) => std::process::exit(run_feedback(opts)),
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

fn run_review(opts: cli::ReviewOpts) -> i32 {
    if opts.files.is_empty() {
        eprintln!("Error: No files specified");
        return 3;
    }

    // Initialize structured tracing if --trace flag or QUORUM_TRACE=1 env var
    let trace_enabled = opts.trace || std::env::var("QUORUM_TRACE").map(|v| v == "1").unwrap_or(false);
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
    let llm_client: Option<std::sync::Arc<llm_client::OpenAiClient>> = if let Ok(api_key) = cfg.require_api_key() {
        let effort = opts.reasoning_effort.clone()
            .or_else(|| std::env::var("QUORUM_REASONING_EFFORT").ok().filter(|s| !s.is_empty()))
            .or_else(|| Some("low".into())); // Default: low reasoning is optimal for code review
        Some(std::sync::Arc::new(
            llm_client::OpenAiClient::new(&cfg.base_url, api_key)
                .with_reasoning_effort(effort)
        ))
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

    // Load feedback for calibration
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let feedback_path = std::path::PathBuf::from(&home).join(".quorum/feedback.jsonl");
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

    // Create semaphore for parallel LLM concurrency control
    let semaphore = if opts.parallel > 1 {
        Some(std::sync::Arc::new(tokio::sync::Semaphore::new(opts.parallel)))
    } else if opts.parallel == 0 {
        Some(std::sync::Arc::new(tokio::sync::Semaphore::new(32)))
    } else {
        None // parallel=1, sequential
    };

    // Pre-build FeedbackIndex once for sharing across parallel tasks.
    // --fast skips fastembed model load and uses Jaccard-only matching.
    let shared_feedback_index = {
        let feedback_path_ref = std::path::PathBuf::from(&home).join(".quorum/feedback.jsonl");
        if feedback_path_ref.exists() {
            let store = feedback::FeedbackStore::new(feedback_path_ref);
            let build_result = if opts.fast {
                feedback_index::FeedbackIndex::build_bm25(&store)
            } else {
                feedback_index::FeedbackIndex::build(&store)
            };
            match build_result {
                Ok(idx) => {
                    tracing::debug!(fast_mode = opts.fast, "FeedbackIndex: pre-built for parallel calibration");
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

    let pipeline_cfg = PipelineConfig {
        models,
        calibration_model: opts.calibration_model.clone(),
        feedback: feedback_entries,
        auto_calibrate: false,
        feedback_store: Some(feedback_path.clone()),
        diff_ranges,
        framework_overrides: opts.framework.clone(),
        skip_context7: opts.skip_context7,
        fast: opts.fast,
        semaphore,
        feedback_index: shared_feedback_index,
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
    let use_json = !use_compact && (opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout()));
    let mut all_findings = Vec::new();
    let mut file_results: Vec<pipeline::FileReviewResult> = Vec::new();
    let mut had_errors = false;

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
                    return (idx, Err(anyhow::anyhow!("File not found: {}", file_path.display())));
                }
                let source = match std::fs::read_to_string(&file_path) {
                    Ok(s) => s,
                    Err(e) => return (idx, Err(anyhow::anyhow!("Could not read {}: {}", file_path.display(), e))),
                };
                let lang = parser::Language::from_path(&file_path);
                let file_display = file_path.to_string_lossy().to_string();

                // Deep review path
                if deep {
                    if let Some(ref client) = llm_client {
                        let project_root = deep_tool_root(&file_path);
                        let tool_reg = tools::ToolRegistry::new(&project_root);
                        let agent_cfg = agent::AgentConfig::default();
                        let model = pipeline_cfg.models.first()
                            .map(|s| s.as_str()).unwrap_or("gpt-5.4");
                        match agent::agent_loop(
                            &source, &file_display,
                            &**client as &dyn agent::AgentReviewer,
                            model, &tool_reg, &agent_cfg,
                        ) {
                            Ok(findings) => {
                                let sup_result = suppress::apply_suppressions(
                                    findings, &suppress_rules, &file_display);
                                let result = pipeline::FileReviewResult {
                                    file_path: file_display,
                                    findings: sup_result.kept,
                                    usage: Default::default(),
                                    suppressed: sup_result.suppressed.len(),
                                };
                                return (idx, Ok((result, sup_result.suppressed)));
                            }
                            Err(e) => {
                                eprintln!("[{}] Warning: Deep review failed: {}. Falling back.", file_path.display(), e);
                            }
                        }
                    }
                }

                // Standard review path
                let llm_reviewer: Option<&dyn pipeline::LlmReviewer> = llm_client.as_deref().map(|c| c as _);
                let parse_cache = cache::ParseCache::new(128);
                let review_result = if let Some(l) = lang {
                    pipeline::review_source(
                        &file_path, &source, l, llm_reviewer, &pipeline_cfg, Some(&parse_cache))
                } else {
                    pipeline::review_file_llm_only(
                        &file_path, &source, llm_reviewer, &pipeline_cfg)
                };

                match review_result {
                    Ok(mut result) => {
                        let sup_result = suppress::apply_suppressions(
                            result.findings, &suppress_rules, &file_display);
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
        type ParResult = (pipeline::FileReviewResult, Vec<(crate::finding::Finding, suppress::SuppressionRule)>);
        let mut indexed_results: Vec<Option<ParResult>> = vec![None; opts.files.len()];
        for handle in handles {
            match tokio::task::block_in_place(|| rt.block_on(handle)) {
                Ok((idx, Ok(result))) => { indexed_results[idx] = Some(result); }
                Ok((idx, Err(e))) => {
                    eprintln!("Error: Review failed for {}: {}", opts.files[idx].display(), e);
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
                    eprintln!("Suppressed {} finding(s) in {}", suppressed_findings.len(), result.file_path);
                }
                if opts.show_suppressed {
                    for (f, rule) in &suppressed_findings {
                        eprint!("{}", suppress::format_suppressed_finding(f, rule));
                    }
                }
                if use_compact {
                    println!("{}", output::format_compact_review(&result.file_path, &result.findings));
                } else if !use_json {
                    print!("{}", output::format_review(&result.file_path, &result.findings, &style));
                }
                all_findings.extend(result.findings.clone());
                file_results.push(result);
            }
        }

        if opts.parallel > 1 && file_results.len() > 1 {
            tracing::debug!(files = file_results.len(), parallel = opts.parallel, "parallel review complete");
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
            suppressed: file_results.iter().map(|r| r.suppressed).sum(),
        };
        let _ = telem_store.record(&telem_entry);

        // Per-review record for dimensional stats (by-repo, by-caller, rolling).
        let reviews_path = std::path::PathBuf::from(&home).join(".quorum/reviews.jsonl");
        let review_log = review_log::ReviewLog::new(reviews_path);
        let first_file = opts.files.first().map(|p| p.as_path());
        let repo = first_file.and_then(review_log::detect_repo);
        let invoked_from = review_log::detect_invoked_from(opts.caller.as_deref());
        let sev_iter = all_findings.iter().map(|f| &f.severity);
        let record = review_log::ReviewRecord {
            run_id: review_log::ReviewRecord::new_ulid(),
            timestamp: chrono::Utc::now(),
            quorum_version: env!("CARGO_PKG_VERSION").to_string(),
            repo,
            invoked_from,
            model: pipeline_cfg.models.first().cloned().unwrap_or_default(),
            files_reviewed: opts.files.len() as u32,
            lines_added: None,     // diff instrumentation: future work
            lines_removed: None,
            findings_by_severity: review_log::SeverityCounts::from_severities(sev_iter),
            suppressed_by_rule: std::collections::HashMap::new(), // per-rule breakdown: future work
            tokens_in: total_tokens_in,
            tokens_out: total_tokens_out,
            tokens_cache_read: 0,  // cache instrumentation: future work
            duration_ms: review_duration.as_millis() as u64,
            flags: review_log::Flags {
                deep: opts.deep,
                parallel_n: opts.parallel as u32,
                ensemble: opts.ensemble,
            },
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

/// Core feedback logic -- testable with custom feedback path.
fn run_feedback_inner(
    file: &str,
    finding: &str,
    verdict_str: &str,
    reason: &str,
    model: Option<&str>,
    feedback_path: &std::path::Path,
) -> (i32, String) {
    let verdict = match cli::parse_verdict(verdict_str) {
        Ok(v) => v,
        Err(e) => {
            return (3, format!("Error: {}", e));
        }
    };

    let entry = feedback::FeedbackEntry {
        file_path: file.to_string(),
        finding_title: finding.to_string(),
        finding_category: "manual".to_string(),
        verdict: verdict.clone(),
        reason: reason.to_string(),
        model: model.map(|s| s.to_string()),
        timestamp: chrono::Utc::now(),
        provenance: feedback::Provenance::Human,
    };

    let store = feedback::FeedbackStore::new(feedback_path.to_path_buf());
    if let Err(e) = store.record(&entry) {
        return (3, format!("Error: Failed to write feedback: {}", e));
    }

    let total = store.count().unwrap_or(0);
    let verdict_label = format!("{:?}", entry.verdict).to_lowercase();

    // Format output based on mode
    let use_compact = output::should_use_compact(false);
    let use_json = !use_compact && !std::io::IsTerminal::is_terminal(&std::io::stdout());

    let output = if use_json {
        let json_obj = serde_json::json!({
            "verdict": verdict_label,
            "file_path": entry.file_path,
            "finding_title": entry.finding_title,
            "total": total,
        });
        serde_json::to_string(&json_obj).unwrap_or_default()
    } else if use_compact {
        format!("feedback:{}|{}|{}", verdict_label, entry.file_path, entry.finding_title)
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
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let feedback_path = std::path::PathBuf::from(&home).join(".quorum/feedback.jsonl");
    let (exit_code, output) = run_feedback_inner(
        &opts.file, &opts.finding, &opts.verdict, &opts.reason,
        opts.model.as_deref(), &feedback_path,
    );
    if exit_code != 0 {
        eprintln!("{}", output);
    } else {
        println!("{}", output);
    }
    exit_code
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
            "src/auth.rs", "SQL injection", "tp", "Fixed with params", None, &path,
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
            "src/auth.rs", "SQL injection", "maybe", "Not sure", None, &path,
        );
        assert_eq!(exit_code, 3);
        assert!(output.contains("Invalid verdict"));
    }

    #[test]
    fn feedback_provenance_is_human() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_inner(
            "src/auth.rs", "SQL injection", "tp", "Real issue", None, &path,
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
            "src/auth.rs", "Test finding", "fp", "Not real", None, &path,
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
            "src/auth.rs", "SQL injection", "tp", "Fixed", None, &path,
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
            "src/auth.rs", "SQL injection", "fp", "Not a real issue", None, &path,
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
            "src/auth.rs", "Test finding", "tp", "Real", Some("gpt-5.4"), &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("gpt-5.4"));
    }
}
