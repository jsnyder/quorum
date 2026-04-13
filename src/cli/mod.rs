use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "quorum", about = "Multi-source code review")]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Review files for issues
    Review(ReviewOpts),
    /// Show feedback and review statistics
    Stats(StatsOpts),
    /// Start MCP server (stdio transport)
    Serve,
    /// Run as daemon with file watching and warm caches
    Daemon(DaemonOpts),
    /// Print version
    Version,
}

#[derive(Parser)]
pub struct StatsOpts {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Token-efficient output for LLM consumption
    #[arg(long)]
    pub compact: bool,

    /// Show stats since this date (YYYY-MM-DD, default: all time)
    #[arg(long)]
    pub since: Option<String>,
}

#[derive(Parser)]
pub struct DaemonOpts {
    /// Directory to watch for file changes (default: current directory)
    #[arg(long)]
    pub watch_dir: Option<PathBuf>,

    /// Parse cache capacity
    #[arg(long, default_value = "256")]
    pub cache_size: usize,

    /// Port to listen on
    #[arg(long, default_value = "7842")]
    pub port: u16,
}

#[derive(Parser)]
pub struct ReviewOpts {
    /// Files to review
    pub files: Vec<PathBuf>,

    /// Output as JSON (auto-detected when piped)
    #[arg(long)]
    pub json: bool,

    /// Use ensemble mode (multiple model families)
    #[arg(long)]
    pub ensemble: bool,

    /// Model for auto-calibration triage (default: same as review model)
    #[arg(long)]
    pub calibration_model: Option<String>,

    /// Reasoning effort: none, minimal, low, medium, high, xhigh
    #[arg(long)]
    pub reasoning_effort: Option<String>,

    /// Disable color output
    #[arg(long)]
    pub no_color: bool,

    /// Show finding provenance
    #[arg(long)]
    pub provenance: bool,

    /// Disable auto-calibration (second LLM pass that triages findings)
    #[arg(long)]
    pub no_auto_calibrate: bool,

    /// Send review request to running daemon instead of parsing locally
    #[arg(long)]
    pub daemon: bool,

    /// Daemon port (default: 7842)
    #[arg(long, default_value = "7842")]
    pub daemon_port: u16,

    /// Enable deep review with tool calling (reads additional files for context)
    #[arg(long)]
    pub deep: bool,

    /// Unified diff file for change-scoped review
    #[arg(long)]
    pub diff_file: Option<PathBuf>,

    /// Token-efficient output for LLM consumption
    #[arg(long)]
    pub compact: bool,

    /// Show findings that were suppressed by project rules
    #[arg(long)]
    pub show_suppressed: bool,
}

impl ReviewOpts {
    pub fn no_auto_calibrate(&self) -> bool {
        self.no_auto_calibrate
    }
}
