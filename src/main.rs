mod analytics;
mod analysis;
mod cache;
mod cli;
mod config;
mod domain;
mod feedback;
mod finding;
mod hydration;
mod linter;
mod llm_client;
mod mcp;
mod merge;
mod output;
mod parser;
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
    let handler = mcp::handler::QuorumHandler::new()?;

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
        Some(Box::new(llm_client::OpenAiClient::new(&cfg.base_url, api_key)))
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

    let pipeline_cfg = PipelineConfig {
        models,
        ..Default::default()
    };

    let style = output::Style::detect(opts.no_color);
    let use_json = opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout());
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

        let tree = match parser::parse(&source, lang) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error: Parse failed for {}: {}", file_path.display(), e);
                had_errors = true;
                continue;
            }
        };

        // Run the full pipeline
        match pipeline::review_file(
            file_path,
            &source,
            lang,
            &tree,
            llm_reviewer.as_deref(),
            &pipeline_cfg,
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
