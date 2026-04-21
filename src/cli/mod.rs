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
    /// Record feedback on a finding
    Feedback(FeedbackOpts),
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
    #[arg(long, value_parser = parse_since_date)]
    pub since: Option<String>,

    /// Group stats by repository
    #[arg(long)]
    pub by_repo: bool,

    /// Group stats by invocation caller (CLAUDE_CODE, CODEX_CI, tty, etc.)
    #[arg(long)]
    pub by_caller: bool,

    /// Show rolling N-review windows (e.g. --rolling 50)
    #[arg(long, value_parser = parse_rolling_n)]
    pub rolling: Option<usize>,

    /// Group context-injection stats by injected source name.
    /// Flattens `context.injected_sources`; reviews listing two sources
    /// contribute to both rows.
    #[arg(long)]
    pub by_source: bool,

    /// Group context-injection stats by repo, restricted to reviews
    /// where an injector was wired (`injector_available = true`).
    #[arg(long)]
    pub by_reviewed_repo: bool,

    /// Count reviews with misleading context telemetry: retriever
    /// errors and "phantom" injections (rendered block recorded but
    /// zero chunks). Produces a 3-row breakdown.
    #[arg(long)]
    pub misleading: bool,

    /// Hide dimensional highlights (top repos/callers/rolling) from the
    /// default dashboard. Restores the pre-highlights output shape.
    #[arg(long)]
    pub minimal: bool,
}

fn parse_rolling_n(s: &str) -> Result<usize, String> {
    match s.parse::<usize>() {
        Ok(0) => Err("--rolling must be >= 1 (0 would produce no output)".into()),
        Ok(n) => Ok(n),
        Err(e) => Err(format!("invalid number '{}': {}", s, e)),
    }
}

/// Validate `--since` as a YYYY-MM-DD calendar date. Returns the original
/// string on success (stats.rs still expects a String today).
fn parse_since_date(s: &str) -> Result<String, String> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map(|_| s.to_string())
        .map_err(|e| format!("invalid date '{}' (expected YYYY-MM-DD): {}", s, e))
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

    /// Model for auto-calibration triage (disabled -- auto-calibrate is off)
    #[arg(long, hide = true)]
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

    /// Disable auto-calibration (no-op: auto-calibrate is off by default)
    #[arg(long, hide = true)]
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

    /// Override framework detection (e.g., --framework home-assistant)
    #[arg(long)]
    pub framework: Vec<String>,

    /// Skip Context7 framework doc enrichment (default: fail if frameworks detected but docs unavailable)
    #[arg(long)]
    pub skip_context7: bool,

    /// Max concurrent LLM calls (default: 4, 0 = unlimited, 1 = sequential)
    #[arg(long, default_value = "4")]
    pub parallel: usize,

    /// Enable structured tracing to ~/.quorum/trace.jsonl (also: QUORUM_TRACE=1)
    #[arg(long)]
    pub trace: bool,

    /// Skip fastembed model (saves ~1.5 GB RAM, ~15 s startup). Calibrator
    /// falls back to Jaccard word-overlap matching on feedback titles.
    #[arg(long)]
    pub fast: bool,

    /// Label this invocation in reviews.jsonl (overrides env-based detection).
    #[arg(long)]
    pub caller: Option<String>,
}

#[derive(Parser)]
pub struct FeedbackOpts {
    /// File path the finding was about
    #[arg(long)]
    pub file: String,

    /// Finding title or substring to match
    #[arg(long)]
    pub finding: String,

    /// Verdict: tp, fp, partial, wontfix
    #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(["tp", "fp", "partial", "wontfix"]))]
    pub verdict: String,

    /// Reason for the verdict
    #[arg(long)]
    pub reason: String,

    /// Model that produced the finding (optional)
    #[arg(long)]
    pub model: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Parse a verdict string into a Verdict enum.
pub fn parse_verdict(s: &str) -> anyhow::Result<crate::feedback::Verdict> {
    match s.trim().to_lowercase().as_str() {
        "tp" => Ok(crate::feedback::Verdict::Tp),
        "fp" => Ok(crate::feedback::Verdict::Fp),
        "partial" => Ok(crate::feedback::Verdict::Partial),
        "wontfix" => Ok(crate::feedback::Verdict::Wontfix),
        other => anyhow::bail!("Invalid verdict '{}'. Must be: tp, fp, partial, wontfix", other),
    }
}

impl ReviewOpts {
    pub fn no_auto_calibrate(&self) -> bool {
        self.no_auto_calibrate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_valid() {
        assert_eq!(parse_verdict("tp").unwrap(), crate::feedback::Verdict::Tp);
        assert_eq!(parse_verdict("fp").unwrap(), crate::feedback::Verdict::Fp);
        assert_eq!(parse_verdict("partial").unwrap(), crate::feedback::Verdict::Partial);
        assert_eq!(parse_verdict("wontfix").unwrap(), crate::feedback::Verdict::Wontfix);
    }

    #[test]
    fn parse_verdict_case_insensitive() {
        assert_eq!(parse_verdict("TP").unwrap(), crate::feedback::Verdict::Tp);
        assert_eq!(parse_verdict("Fp").unwrap(), crate::feedback::Verdict::Fp);
    }

    #[test]
    fn parse_verdict_invalid() {
        assert!(parse_verdict("maybe").is_err());
        assert!(parse_verdict("").is_err());
    }

    #[test]
    fn parse_verdict_trims_whitespace() {
        assert_eq!(parse_verdict(" tp ").unwrap(), crate::feedback::Verdict::Tp);
        assert_eq!(parse_verdict("fp\n").unwrap(), crate::feedback::Verdict::Fp);
    }

    #[test]
    fn parse_parallel_flag() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "review", "--parallel", "8", "file.rs"]);
        match args.command {
            Command::Review(opts) => assert_eq!(opts.parallel, 8),
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn stats_by_repo_flag_parses() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats", "--by-repo"]);
        match args.command {
            Command::Stats(opts) => assert!(opts.by_repo),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn stats_by_caller_flag_parses() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats", "--by-caller"]);
        match args.command {
            Command::Stats(opts) => assert!(opts.by_caller),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn stats_rejects_rolling_zero() {
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "stats", "--rolling", "0"]);
        assert!(res.is_err(), "parser should reject --rolling 0");
    }

    #[test]
    fn stats_rolling_flag_parses_with_value() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats", "--rolling", "50"]);
        match args.command {
            Command::Stats(opts) => assert_eq!(opts.rolling, Some(50)),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn stats_rolling_defaults_to_none() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats"]);
        match args.command {
            Command::Stats(opts) => assert_eq!(opts.rolling, None),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn stats_rejects_malformed_since_date() {
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "stats", "--since", "not-a-date"]);
        assert!(res.is_err(), "parser should reject non-YYYY-MM-DD --since");
    }

    #[test]
    fn stats_accepts_valid_since_date() {
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "stats", "--since", "2026-04-19"]);
        assert!(res.is_ok(), "parser should accept valid YYYY-MM-DD");
    }

    #[test]
    fn feedback_rejects_invalid_verdict_at_parse_time() {
        use clap::Parser;
        let res = Args::try_parse_from([
            "quorum", "feedback",
            "--file", "x.rs",
            "--finding", "t",
            "--verdict", "maybe",
            "--reason", "r",
        ]);
        assert!(res.is_err(), "parser should reject verdict='maybe'");
    }

    #[test]
    fn feedback_accepts_valid_verdicts_at_parse_time() {
        use clap::Parser;
        for v in ["tp", "fp", "partial", "wontfix"] {
            let res = Args::try_parse_from([
                "quorum", "feedback",
                "--file", "x.rs",
                "--finding", "t",
                "--verdict", v,
                "--reason", "r",
            ]);
            assert!(res.is_ok(), "verdict {} should parse", v);
        }
    }

    #[test]
    fn parse_parallel_default() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "review", "file.rs"]);
        match args.command {
            Command::Review(opts) => assert_eq!(opts.parallel, 4),
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn stats_by_source_flag_parses() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats", "--by-source"]);
        match args.command {
            Command::Stats(opts) => assert!(opts.by_source),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn stats_by_reviewed_repo_flag_parses() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats", "--by-reviewed-repo"]);
        match args.command {
            Command::Stats(opts) => assert!(opts.by_reviewed_repo),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn stats_misleading_flag_parses() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats", "--misleading"]);
        match args.command {
            Command::Stats(opts) => assert!(opts.misleading),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn stats_by_source_composes_with_rolling() {
        // `--by-source --rolling 50` must parse — the two flags are
        // intentionally compatible (rolling slices are applied within
        // the source group).
        use clap::Parser;
        let args = Args::parse_from(["quorum", "stats", "--by-source", "--rolling", "50"]);
        match args.command {
            Command::Stats(opts) => {
                assert!(opts.by_source);
                assert_eq!(opts.rolling, Some(50));
            }
            _ => panic!("Expected Stats command"),
        }
    }
}
