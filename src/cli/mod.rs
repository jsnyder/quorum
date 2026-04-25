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
    /// Manage the review-context store (sources, index, retrieval)
    Context(ContextOpts),
    /// Print version
    Version,
}

/// Top-level `quorum context` command. Wraps the nested subcommand so that
/// argparse can keep the context surface area in a single enum.
#[derive(Parser)]
#[command(
    about = "Manage the review-context store",
    long_about = "Register source repositories or docs, index them for semantic retrieval, \
                  and surface relevant chunks during review.\n\n\
                  Examples:\n  \
                    quorum context init\n  \
                    quorum context add --name my-app --kind rust --path ./src\n  \
                    quorum context index --all\n  \
                    quorum context query \"how is auth done?\" --k 5"
)]
pub struct ContextOpts {
    #[command(subcommand)]
    pub command: ContextCommand,
}

#[derive(Subcommand)]
pub enum ContextCommand {
    /// Create ~/.quorum/sources.toml with a default empty config.
    ///
    /// Example: quorum context init
    Init,

    /// Register a new source (local path or git repo).
    ///
    /// Example: quorum context add --name myapp --kind rust --path ./src
    Add(ContextAddOpts),

    /// List registered sources.
    ///
    /// Example: quorum context list --json
    List(ContextListOpts),

    /// Extract + embed one or all registered sources.
    ///
    /// Example: quorum context index --source myapp
    Index(ContextIndexOpts),

    /// Re-index sources that have drifted since last extract.
    ///
    /// Example: quorum context refresh --all
    Refresh(ContextRefreshOpts),

    /// Semantic search across indexed chunks.
    ///
    /// Example: quorum context query "token refresh" --k 5
    Query(ContextQueryOpts),

    /// Remove per-source artefacts for sources no longer in sources.toml.
    ///
    /// Example: quorum context prune --dry-run
    Prune(ContextPruneOpts),

    /// Run health checks on the context store.
    ///
    /// Example: quorum context doctor --json
    Doctor(ContextDoctorOpts),
}

#[derive(Parser)]
pub struct ContextAddOpts {
    /// Short unique name for the source (used as a directory key).
    #[arg(long)]
    pub name: String,

    /// Source kind: rust, typescript, javascript, python, go, terraform, service, docs.
    #[arg(long)]
    pub kind: String,

    /// Local filesystem path to the source. Mutually exclusive with --git.
    /// Exactly one of --path or --git is required.
    #[arg(long, conflicts_with = "git", required_unless_present = "git")]
    pub path: Option<PathBuf>,

    /// Git URL for a remote source. Mutually exclusive with --path.
    /// Exactly one of --path or --git is required.
    #[arg(long, conflicts_with = "path", required_unless_present = "path")]
    pub git: Option<String>,

    /// Optional git rev (branch, tag, sha) to pin when --git is set.
    #[arg(long, requires = "git")]
    pub rev: Option<String>,

    /// Relative weight (higher floats further in retrieval).
    #[arg(long)]
    pub weight: Option<i32>,

    /// Glob to exclude from extraction (repeatable).
    #[arg(long = "ignore")]
    pub ignore: Vec<String>,
}

#[derive(Parser)]
pub struct ContextListOpts {
    /// Output as JSON.
    #[arg(long, conflicts_with = "compact")]
    pub json: bool,

    /// Compact single-line-per-source output.
    #[arg(long, conflicts_with = "json")]
    pub compact: bool,
}

#[derive(Parser)]
pub struct ContextIndexOpts {
    /// Index a single named source. Mutually exclusive with --all.
    #[arg(long, conflicts_with = "all")]
    pub source: Option<String>,

    /// Index every registered source.
    #[arg(long, conflicts_with = "source")]
    pub all: bool,
}

#[derive(Parser)]
pub struct ContextRefreshOpts {
    /// Refresh a single named source. Mutually exclusive with --all.
    #[arg(long, conflicts_with = "all")]
    pub source: Option<String>,

    /// Refresh every registered source.
    #[arg(long, conflicts_with = "source")]
    pub all: bool,
}

#[derive(Parser)]
pub struct ContextQueryOpts {
    /// Natural-language query text.
    pub text: String,

    /// Restrict results to a single source.
    #[arg(long)]
    pub source: Option<String>,

    /// Return up to this many chunks.
    #[arg(long)]
    pub k: Option<usize>,

    /// Include per-chunk scoring details.
    #[arg(long)]
    pub explain: bool,

    /// Output as JSON.
    #[arg(long, conflicts_with = "compact")]
    pub json: bool,

    /// Compact token-efficient output.
    #[arg(long, conflicts_with = "json")]
    pub compact: bool,
}

#[derive(Parser)]
pub struct ContextPruneOpts {
    /// Report what would be removed without touching the filesystem.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Parser)]
pub struct ContextDoctorOpts {
    /// Output as JSON.
    #[arg(long, conflicts_with = "compact")]
    pub json: bool,

    /// Compact tab-separated output.
    #[arg(long, conflicts_with = "json")]
    pub compact: bool,

    /// Apply best-effort fixes for any fixable failures.
    #[arg(long)]
    pub repair: bool,
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
    // Issue #89: at least one file is required. Without this, clap accepted
    // an empty list and the handler later short-circuited with exit 3 — but
    // that's a usage error (exit 2) and the contract belongs at the parsing
    // layer so the user gets clap's standard "required arguments" message
    // and `--help` hint.
    #[arg(required = true, num_args = 1..)]
    pub files: Vec<PathBuf>,

    /// Output as JSON (auto-detected when piped)
    #[arg(long)]
    pub json: bool,

    /// Use ensemble mode (multiple model families)
    #[arg(long)]
    pub ensemble: bool,

    /// Reasoning effort: none, minimal, low, medium, high, xhigh
    #[arg(long)]
    pub reasoning_effort: Option<String>,

    /// Disable color output
    #[arg(long)]
    pub no_color: bool,

    /// Show finding provenance
    #[arg(long)]
    pub provenance: bool,

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

    /// Verdict: tp, fp, partial, wontfix, context_misleading
    // Issue #90: validation delegated to `parse_verdict` (called in main.rs).
    // A previous `PossibleValuesParser` rejected case/whitespace variants
    // before `parse_verdict` (which trims + lowercases) could normalize them —
    // inconsistent with our own contract.
    #[arg(long)]
    pub verdict: String,

    /// Reason for the verdict
    #[arg(long)]
    pub reason: String,

    /// Model that produced the finding (optional)
    #[arg(long)]
    pub model: Option<String>,

    /// Comma-separated chunk IDs blamed for misleading context
    /// (only meaningful with `--verdict context_misleading`).
    /// Whitespace is trimmed per entry; empty entries like "a,,b" are rejected.
    #[arg(long)]
    pub blamed_chunks: Option<String>,

    /// Finding category (e.g. "security", "correctness"). If omitted, the
    /// Human path records "manual"; the External path records "unknown".
    #[arg(long)]
    pub category: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Record the verdict as coming from an external review agent (pal, third-opinion, etc.).
    /// Triggers External provenance instead of the default Human path.
    #[arg(long)]
    pub from_agent: Option<String>,

    /// Optional: the LLM model the external agent used (only meaningful with --from-agent).
    #[arg(long, requires = "from_agent")]
    pub agent_model: Option<String>,

    /// Optional: agent-reported confidence in [0,1]. Rejected at the CLI
    /// boundary if outside that range (clap `value_parser`); record_external
    /// re-clamps for safety. Ignored by calibrator in v1.
    #[arg(long, requires = "from_agent", value_parser = parse_confidence)]
    pub confidence: Option<f32>,
}

/// Validate `--confidence` at the CLI boundary. Accepts finite values in
/// [0,1] only. NaN/Inf and out-of-range values are rejected with a clear
/// error rather than silently clamped.
pub fn parse_confidence(s: &str) -> Result<f32, String> {
    let v: f32 = s
        .parse()
        .map_err(|e| format!("--confidence must be a number: {e}"))?;
    if !v.is_finite() {
        return Err(format!("--confidence must be finite, got {v}"));
    }
    if !(0.0..=1.0).contains(&v) {
        return Err(format!(
            "--confidence must be in [0.0, 1.0], got {v}"
        ));
    }
    Ok(v)
}

/// Parse a comma-separated list of chunk IDs, trimming whitespace per entry.
/// Rejects empty entries (e.g. "a,,b" or a trailing comma).
/// Returns `Ok(Vec::new())` for `None` input; callers may supply an empty
/// default when the verdict is `context_misleading` and no chunks were given.
pub fn parse_blamed_chunks(raw: Option<&str>) -> anyhow::Result<Vec<String>> {
    let Some(s) = raw else {
        return Ok(Vec::new());
    };
    // Explicit empty / whitespace-only --blamed-chunks "" is rejected: the
    // flag was present, so the user meant something. If the intent is "no
    // chunks blamed", omit the flag entirely (parse_blamed_chunks(None)
    // returns Ok(vec![])).
    if s.trim().is_empty() {
        anyhow::bail!(
            "--blamed-chunks was provided but is empty; omit the flag if no chunks are blamed"
        );
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            anyhow::bail!(
                "--blamed-chunks contains an empty entry (check for leading, trailing, or doubled commas): {:?}",
                s
            );
        }
        out.push(trimmed.to_string());
    }
    Ok(out)
}

/// Parse a verdict string into a Verdict enum.
///
/// For `context_misleading`, this returns a variant with an empty
/// `blamed_chunk_ids` list. Callers that accept `--blamed-chunks` should
/// override via [`parse_blamed_chunks`] before storing the verdict.
pub fn parse_verdict(s: &str) -> anyhow::Result<crate::feedback::Verdict> {
    match s.trim().to_lowercase().as_str() {
        "tp" => Ok(crate::feedback::Verdict::Tp),
        "fp" => Ok(crate::feedback::Verdict::Fp),
        "partial" => Ok(crate::feedback::Verdict::Partial),
        "wontfix" => Ok(crate::feedback::Verdict::Wontfix),
        "context_misleading" => Ok(crate::feedback::Verdict::ContextMisleading {
            blamed_chunk_ids: Vec::new(),
        }),
        other => anyhow::bail!(
            "Invalid verdict '{}'. Must be: tp, fp, partial, wontfix, context_misleading",
            other
        ),
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

    // Issue #90: removed `feedback_rejects_invalid_verdict_at_parse_time`.
    // Clap no longer validates the verdict (the old PossibleValuesParser
    // rejected case/whitespace variants that `parse_verdict` would otherwise
    // normalize). Runtime validation happens via `parse_verdict` in main.rs;
    // end-to-end coverage lives in `tests/cli_feedback_agent.rs`
    // (`cli_feedback_rejects_unknown_verdict`).

    #[test]
    fn feedback_accepts_valid_verdicts_at_parse_time() {
        use clap::Parser;
        for v in ["tp", "fp", "partial", "wontfix", "context_misleading"] {
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
    fn feedback_parses_blamed_chunks_flag() {
        use clap::Parser;
        let args = Args::parse_from([
            "quorum", "feedback",
            "--file", "src/x.rs",
            "--finding", "t",
            "--verdict", "context_misleading",
            "--reason", "r",
            "--blamed-chunks", "chunk-abc,chunk-def",
        ]);
        match args.command {
            Command::Feedback(opts) => {
                assert_eq!(opts.blamed_chunks.as_deref(), Some("chunk-abc,chunk-def"));
            }
            _ => panic!("Expected Feedback command"),
        }
    }

    #[test]
    fn feedback_blamed_chunks_optional_for_context_misleading() {
        // Plan is explicit: missing --blamed-chunks must NOT error at parse time.
        use clap::Parser;
        let res = Args::try_parse_from([
            "quorum", "feedback",
            "--file", "src/x.rs",
            "--finding", "t",
            "--verdict", "context_misleading",
            "--reason", "r",
        ]);
        assert!(res.is_ok(), "--blamed-chunks must remain optional");
        match res.unwrap().command {
            Command::Feedback(opts) => assert!(opts.blamed_chunks.is_none()),
            _ => panic!("Expected Feedback command"),
        }
    }

    #[test]
    fn parse_blamed_chunks_splits_and_trims() {
        let got = parse_blamed_chunks(Some(" a, b ,c")).unwrap();
        assert_eq!(got, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn parse_blamed_chunks_rejects_empty_entries() {
        assert!(parse_blamed_chunks(Some("a,,b")).is_err());
        assert!(parse_blamed_chunks(Some("a,")).is_err());
        assert!(parse_blamed_chunks(Some(",a")).is_err());
        assert!(parse_blamed_chunks(Some("a, ,b")).is_err());
    }

    #[test]
    fn parse_blamed_chunks_none_returns_empty_vec() {
        assert_eq!(parse_blamed_chunks(None).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn parse_blamed_chunks_blank_string_is_rejected() {
        // Explicit `--blamed-chunks ""` is a user mistake, not shorthand for
        // "no chunks" — omit the flag entirely for that semantic.
        assert!(parse_blamed_chunks(Some("")).is_err());
        assert!(parse_blamed_chunks(Some("   ")).is_err());
    }

    #[test]
    fn parse_verdict_accepts_context_misleading() {
        let v = parse_verdict("context_misleading").unwrap();
        match v {
            crate::feedback::Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert!(blamed_chunk_ids.is_empty(),
                    "parse_verdict alone never fills chunks; caller merges --blamed-chunks");
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
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

    // --- Issue #89: review subcommand requires at least one file -----------
    //
    // Pre-fix, `Vec<PathBuf>` parsed an empty list silently and the handler
    // short-circuited with exit 3 — but the contract belongs at the clap
    // layer (standard usage error, exit 2, clap-formatted message). Asserting
    // on the typed `ErrorKind` is robust to clap's wording changes.

    #[test]
    fn review_with_no_files_yields_missing_required_argument() {
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "review"]);
        let err = res.err().expect("review with zero files must fail to parse");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "expected MissingRequiredArgument; got {:?}",
            err.kind()
        );
    }

    #[test]
    fn review_with_json_flag_and_no_files_still_requires_files() {
        // --json must NOT bypass the required-files rule. This guards against
        // an over-aggressive `required_unless_present_any = ["json"]` style
        // fix that would re-introduce the silent-no-op surface.
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "review", "--json"]);
        let err = res.err().expect("--json without files must still fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn review_with_one_file_parses_successfully() {
        // Positive control: pins that the constraint only fires on the truly
        // empty case, not on every invocation.
        use clap::Parser;
        let args = Args::parse_from(["quorum", "review", "/tmp/x.rs"]);
        match args.command {
            Command::Review(opts) => assert_eq!(opts.files.len(), 1),
            _ => panic!("Expected Review command"),
        }
    }

    // --- Issue #79 regression guards ---------------------------------------
    //
    // ContextIndexOpts and ContextRefreshOpts already declare
    // `conflicts_with` for --source/--all (src/cli/mod.rs:135-156). These
    // tests exist to prevent silent regression of that fix; they pass on
    // current main and would fail if the annotation is dropped.

    #[test]
    fn regression_guard_79_index_rejects_source_and_all() {
        use clap::Parser;
        let res = Args::try_parse_from([
            "quorum", "context", "index", "--source", "foo", "--all",
        ]);
        let err = res.err().expect("index with both --source and --all must fail");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "expected ArgumentConflict; got {:?}",
            err.kind()
        );
    }

    #[test]
    fn regression_guard_79_refresh_rejects_source_and_all() {
        use clap::Parser;
        let res = Args::try_parse_from([
            "quorum", "context", "refresh", "--source", "foo", "--all",
        ]);
        let err = res.err().expect("refresh with both --source and --all must fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn regression_guard_79_index_with_only_source_parses() {
        // Positive control: --source alone must NOT trip the conflict rule.
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "context", "index", "--source", "foo"]);
        assert!(
            res.is_ok(),
            "single-flag invocation must parse; got {:?}",
            res.err().map(|e| e.kind())
        );
    }

    #[test]
    fn regression_guard_79_index_with_only_all_parses() {
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "context", "index", "--all"]);
        assert!(
            res.is_ok(),
            "single-flag invocation must parse; got {:?}",
            res.err().map(|e| e.kind())
        );
    }

    #[test]
    fn regression_guard_79_refresh_with_only_source_parses() {
        // ContextRefreshOpts is a separate clap surface from ContextIndexOpts;
        // mirror the index positive-controls so a future over-tightening of
        // refresh's flags is caught (CodeRabbit PR #106).
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "context", "refresh", "--source", "foo"]);
        assert!(
            res.is_ok(),
            "single-flag invocation must parse; got {:?}",
            res.err().map(|e| e.kind())
        );
    }

    #[test]
    fn regression_guard_79_refresh_with_only_all_parses() {
        use clap::Parser;
        let res = Args::try_parse_from(["quorum", "context", "refresh", "--all"]);
        assert!(
            res.is_ok(),
            "single-flag invocation must parse; got {:?}",
            res.err().map(|e| e.kind())
        );
    }
}
