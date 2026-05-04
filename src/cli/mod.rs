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
    /// Compute calibrator thresholds from feedback corpus
    Calibrate(CalibrateOpts),
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

/// Validate `--name` (and any other "becomes a single directory component"
/// argument). The value lands at `~/.quorum/sources/<name>/`, so it must be:
///
/// - 1..=64 ASCII chars from `[a-zA-Z0-9_-]`
/// - not start with `.` (no hidden dirs, no `.`/`..` traversal)
///
/// This validator is the single source of truth shared by clap's
/// `value_parser`, the `run_add` handler (defense-in-depth), and
/// `SourcesConfig::append_source` (config-write boundary). It returns the
/// owned `String` clap stores into the typed field.
pub fn validate_source_name(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("--name must not be empty".into());
    }
    if s.len() > 64 {
        return Err(format!(
            "--name length must be 1..=64 (got {})",
            s.len()
        ));
    }
    if s.starts_with('.') {
        return Err("--name must not start with '.'".into());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(
            "--name may only contain [a-zA-Z0-9_-] (no path separators, spaces, or unicode)"
                .into(),
        );
    }
    Ok(s.to_string())
}

/// Validate `--k` for `quorum context query`. clap 4.5's
/// `value_parser!(usize).range(...)` is unsupported (the ranged parser only
/// takes primitive integer types like u64/i64), so we hand-roll the
/// validator and keep the field type `Option<usize>` to avoid downstream
/// type churn.
pub fn validate_k(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|e| format!("--k must be a positive integer: {e}"))?;
    if !(1..=100).contains(&n) {
        return Err(format!("--k must be in 1..=100 (got {n})"));
    }
    Ok(n)
}

#[derive(Parser)]
pub struct ContextAddOpts {
    /// Short unique name for the source (used as a directory key).
    /// Must be 1..=64 chars from [a-zA-Z0-9_-] and not start with '.'.
    #[arg(long, value_parser = validate_source_name)]
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

    /// Return up to this many chunks. Range: 1..=100.
    #[arg(long, value_parser = validate_k)]
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
pub struct CalibrateOpts {
    /// Compute and print thresholds without writing the config file
    #[arg(long)]
    pub dry_run: bool,

    /// Target precision for suppress path (default: 0.95, range 0.0-1.0)
    #[arg(long, default_value = "0.95")]
    pub suppress_precision: f64,

    /// Target precision for boost path (default: 0.85, range 0.0-1.0)
    #[arg(long, default_value = "0.85")]
    pub boost_precision: f64,
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

/// CLI surface for `--fp-kind` (#123 Layer 1). Variants map onto
/// `feedback::FpKind` via `FeedbackOpts::into_fp_kind`. Only meaningful
/// when `--verdict fp`; ignored (with `tracing::warn`) on other verdicts.
#[derive(Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FpKindArg {
    /// LLM invented a defect that doesn't exist. Default semantics if the
    /// flag is omitted (None ↔ Hallucination).
    Hallucination,
    /// FP under the current trust model only. Calibrator decays 3× faster.
    TrustModel,
    /// Real defect, mitigated by a control elsewhere. Requires `--fp-reference`.
    CompensatingControl,
    /// Pattern fires correctly elsewhere; this instance is the exception.
    /// Optional `--fp-discriminator` surfaces a hint in the few-shot prompt.
    PatternOvergeneralization,
    /// Real defect tracked elsewhere (PR/issue). Optional `--fp-tracked-in`
    /// records the link. Excluded from the calibrator precedent pool.
    OutOfScope,
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

    // -- #123 Layer 1: FP discriminator vocabulary --
    //
    // `--fp-kind` is meaningful only when `--verdict fp`. On other verdicts
    // it's silently dropped with a `tracing::warn` (composability with
    // shell pipelines). Cross-field validation (compensating-control needs
    // a reference) lives in `into_fp_kind`, NOT in clap, because clap can't
    // see cross-arg requirements at parse time.
    /// Discriminate FP verdict by reason: hallucination | trust-model |
    /// compensating-control | pattern-overgeneralization | out-of-scope.
    /// See docs/plans/2026-04-29-issue-123-fpkind-design.md.
    #[arg(long, value_enum)]
    pub fp_kind: Option<FpKindArg>,

    /// REQUIRED with `--fp-kind compensating-control`: file:line, PR, or doc URL
    /// pointing at the control that mitigates this finding.
    #[arg(long, requires = "fp_kind")]
    pub fp_reference: Option<String>,

    /// Optional with `--fp-kind pattern-overgeneralization`: hint surfaced
    /// in the few-shot prompt so the LLM learns when the pattern IS a real bug.
    #[arg(long, requires = "fp_kind")]
    pub fp_discriminator: Option<String>,

    /// Optional with `--fp-kind out-of-scope`: PR/issue tracking the real
    /// follow-up fix. CLI emits a `tracing::warn` if omitted to discourage
    /// orphaned deferrals (still records the entry though).
    #[arg(long, requires = "fp_kind")]
    pub fp_tracked_in: Option<String>,
}

impl FeedbackOpts {
    /// Convert the parsed CLI flags into a concrete `FpKind` (or None if
    /// `--fp-kind` was omitted, or the verdict isn't `fp`).
    ///
    /// Validation rules (#123 Layer 1):
    /// - `--fp-kind` on a non-`fp` verdict → returns Ok(None) (silently dropped).
    /// - `--fp-kind compensating-control` without `--fp-reference` → Err.
    /// - `--fp-kind out-of-scope` without `--fp-tracked-in` → Ok(Some(..))
    ///   with `tracing::warn` (orphaned deferral discouraged but legal).
    pub fn into_fp_kind(&self) -> anyhow::Result<Option<crate::feedback::FpKind>> {
        use crate::feedback::FpKind;

        // Drop fp_kind silently when verdict != fp. Caller logs the warn
        // at the call site (it has the parsed Verdict in hand).
        let verdict_lower = self.verdict.trim().to_ascii_lowercase();
        if verdict_lower != "fp" {
            return Ok(None);
        }

        match self.fp_kind {
            None => Ok(None),
            Some(FpKindArg::Hallucination) => Ok(Some(FpKind::Hallucination)),
            Some(FpKindArg::TrustModel) => Ok(Some(FpKind::TrustModelAssumption)),
            Some(FpKindArg::CompensatingControl) => {
                let reference = self.fp_reference.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "--fp-kind compensating-control requires --fp-reference \
                         (a file:line, PR, or doc URL pointing at the control)"
                    )
                })?;
                Ok(Some(FpKind::CompensatingControl { reference }))
            }
            Some(FpKindArg::PatternOvergeneralization) => {
                Ok(Some(FpKind::PatternOvergeneralization {
                    discriminator_hint: self.fp_discriminator.clone(),
                }))
            }
            Some(FpKindArg::OutOfScope) => {
                let tracked_in = self.fp_tracked_in.clone();
                if tracked_in.is_none() {
                    tracing::warn!(
                        "--fp-kind out-of-scope recorded without --fp-tracked-in; \
                         the deferral has no tracking link and future findings \
                         matching this title will keep firing without suppression"
                    );
                }
                Ok(Some(FpKind::OutOfScope { tracked_in }))
            }
        }
    }
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

    // -----------------------------------------------------------------
    // #123 Layer 1 (Task 7) — CLI --fp-kind + suffix flags
    // -----------------------------------------------------------------

    fn parse_args(args: &[&str]) -> Result<FeedbackOpts, clap::Error> {
        // Need a leading binary name for clap; "feedback" is conventional here.
        let mut full = vec!["feedback"];
        full.extend_from_slice(args);
        FeedbackOpts::try_parse_from(full)
    }

    #[test]
    fn cli_fp_kind_trust_model_parses() {
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--fp-kind", "trust-model", "--reason", "r",
        ]).expect("parse");
        let kind = opts.into_fp_kind().expect("into_fp_kind");
        assert_eq!(kind, Some(crate::feedback::FpKind::TrustModelAssumption));
    }

    #[test]
    fn cli_fp_kind_compensating_control_requires_reference_in_into_fp_kind() {
        // PINNED: clap parses successfully because cross-field validation
        // can't happen at parse time. The error surfaces in into_fp_kind().
        // If a future refactor moves validation to clap, this test fails at
        // parse_args() — that's the signal to update the test, not to silently
        // rely on a different error path.
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--fp-kind", "compensating-control", "--reason", "r",
        ]).expect("clap parses; cross-field validation lives in into_fp_kind");
        let result = opts.into_fp_kind();
        assert!(result.is_err(), "compensating-control without --fp-reference must error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("--fp-reference"),
            "error message must name the missing flag; got: {}",
            err,
        );
    }

    #[test]
    fn cli_fp_kind_compensating_control_with_reference_parses() {
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--fp-kind", "compensating-control",
            "--fp-reference", "PR #99 line 42",
            "--reason", "r",
        ]).expect("parse");
        let kind = opts.into_fp_kind().expect("into_fp_kind").unwrap();
        match kind {
            crate::feedback::FpKind::CompensatingControl { reference } => {
                assert_eq!(reference, "PR #99 line 42");
            }
            other => panic!("expected CompensatingControl, got {:?}", other),
        }
    }

    #[test]
    fn cli_fp_kind_invalid_value_rejected_by_clap() {
        let result = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--fp-kind", "bogus-kind", "--reason", "r",
        ]);
        assert!(result.is_err(), "clap must reject unknown --fp-kind value");
    }

    #[test]
    fn cli_fp_kind_on_tp_verdict_silently_dropped() {
        // Composability: a shell pipeline can pipe `--verdict tp --fp-kind X`
        // without the CLI failing. into_fp_kind returns Ok(None); the call
        // site is responsible for emitting a tracing::warn. Test the dropped-
        // kind contract here.
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "tp",
            "--fp-kind", "trust-model", "--reason", "r",
        ]).expect("parse");
        let kind = opts.into_fp_kind().expect("must not fail on tp+kind");
        assert_eq!(kind, None, "fp_kind must be dropped when verdict != fp");
    }

    #[test]
    fn cli_fp_kind_pattern_overgeneralization_with_discriminator() {
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--fp-kind", "pattern-overgeneralization",
            "--fp-discriminator", "When in #[derive], ignore",
            "--reason", "r",
        ]).expect("parse");
        let kind = opts.into_fp_kind().expect("into_fp_kind").unwrap();
        match kind {
            crate::feedback::FpKind::PatternOvergeneralization { discriminator_hint } => {
                assert_eq!(discriminator_hint.as_deref(), Some("When in #[derive], ignore"));
            }
            other => panic!("expected PatternOvergeneralization, got {:?}", other),
        }
    }

    #[test]
    fn cli_fp_kind_out_of_scope_optional_tracked_in() {
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--fp-kind", "out-of-scope", "--reason", "r",
        ]).expect("parse");
        let kind = opts.into_fp_kind().expect("into_fp_kind").unwrap();
        assert!(matches!(
            kind,
            crate::feedback::FpKind::OutOfScope { tracked_in: None }
        ));
    }

    #[test]
    fn cli_fp_kind_out_of_scope_with_tracked_in() {
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--fp-kind", "out-of-scope",
            "--fp-tracked-in", "#456",
            "--reason", "r",
        ]).expect("parse");
        let kind = opts.into_fp_kind().expect("into_fp_kind").unwrap();
        match kind {
            crate::feedback::FpKind::OutOfScope { tracked_in } => {
                assert_eq!(tracked_in.as_deref(), Some("#456"));
            }
            other => panic!("expected OutOfScope, got {:?}", other),
        }
    }

    #[test]
    fn cli_fp_kind_omitted_means_none() {
        let opts = parse_args(&[
            "--file", "f.rs", "--finding", "x", "--verdict", "fp",
            "--reason", "r",
        ]).expect("parse");
        let kind = opts.into_fp_kind().expect("into_fp_kind");
        assert_eq!(kind, None, "no --fp-kind flag = no kind");
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

    // --- Issue #135: clap-layer validation for `--name` -----------------------
    //
    // The `--name` value becomes a single directory component under
    // `~/.quorum/sources/<name>/`. Path-traversal characters, leading dots,
    // and overlong values must be rejected at parse time; the handler also
    // re-validates as defense-in-depth (see `src/context/cli.rs::run_add`).

    #[test]
    fn context_add_name_rejects_dotdot() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", "../etc",
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_err(), "../etc must be rejected at parse time");
    }

    #[test]
    fn context_add_name_rejects_absolute() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", "/etc/passwd",
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn context_add_name_rejects_slash() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", "a/b",
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn context_add_name_rejects_backslash() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", r"a\b",
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn context_add_name_rejects_leading_dot() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", ".hidden",
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn context_add_name_rejects_overlong() {
        use clap::Parser;
        let long = "a".repeat(65);
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", &long,
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn context_add_name_rejects_empty() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", "",
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn context_add_name_accepts_simple() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", "my-source_1",
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_ok(), "simple alnum/dash/underscore must parse: {:?}", r.err().map(|e| e.to_string()));
    }

    #[test]
    fn context_add_name_accepts_64_chars() {
        use clap::Parser;
        let max_len = "a".repeat(64);
        let r = Args::try_parse_from([
            "quorum", "context", "add",
            "--name", &max_len,
            "--kind", "rust",
            "--path", "/tmp/x",
        ]);
        assert!(r.is_ok(), "64-char name (max allowed) must parse");
    }

    // --- Issue #136: clap-layer validation for `--k` --------------------------
    //
    // `clap::value_parser!(usize).range(...)` is NOT supported in clap 4.5
    // (the ranged parser only takes u64/i64-style integer types), so we use
    // a custom `validate_k` value_parser and keep the field type
    // `Option<usize>` to avoid downstream type churn.

    #[test]
    fn context_query_k_rejects_zero() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "query", "hello", "--k", "0",
        ]);
        assert!(r.is_err(), "--k 0 must be rejected (would produce empty results)");
    }

    #[test]
    fn context_query_k_rejects_above_cap() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "query", "hello", "--k", "101",
        ]);
        assert!(r.is_err(), "--k 101 must be rejected (above 100 cap)");
    }

    #[test]
    fn context_query_k_rejects_negative() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "query", "hello", "--k", "-1",
        ]);
        assert!(r.is_err(), "--k -1 must be rejected (not a usize)");
    }

    #[test]
    fn context_query_k_accepts_in_range() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "query", "hello", "--k", "50",
        ]);
        assert!(r.is_ok(), "--k 50 must parse");
    }

    #[test]
    fn context_query_k_accepts_one() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "query", "hello", "--k", "1",
        ]);
        assert!(r.is_ok(), "--k 1 must parse (lower bound)");
    }

    #[test]
    fn context_query_k_accepts_hundred() {
        use clap::Parser;
        let r = Args::try_parse_from([
            "quorum", "context", "query", "hello", "--k", "100",
        ]);
        assert!(r.is_ok(), "--k 100 must parse (upper bound)");
    }

    // --- PR3: calibrate subcommand -------------------------------------------

    #[test]
    fn calibrate_subcommand_parses() {
        use clap::Parser;
        let args = Args::try_parse_from(["quorum", "calibrate"]);
        assert!(args.is_ok(), "calibrate subcommand should parse");
    }

    #[test]
    fn calibrate_dry_run_parses() {
        use clap::Parser;
        let args = Args::try_parse_from(["quorum", "calibrate", "--dry-run"]);
        assert!(args.is_ok(), "calibrate --dry-run should parse");
    }

    #[test]
    fn calibrate_custom_precision_parses() {
        use clap::Parser;
        let args = Args::parse_from([
            "quorum", "calibrate",
            "--suppress-precision", "0.90",
            "--boost-precision", "0.80",
        ]);
        match args.command {
            Command::Calibrate(opts) => {
                assert!((opts.suppress_precision - 0.90).abs() < 1e-9);
                assert!((opts.boost_precision - 0.80).abs() < 1e-9);
            }
            _ => panic!("Expected Calibrate command"),
        }
    }

    #[test]
    fn calibrate_defaults() {
        use clap::Parser;
        let args = Args::parse_from(["quorum", "calibrate"]);
        match args.command {
            Command::Calibrate(opts) => {
                assert!(!opts.dry_run);
                assert!((opts.suppress_precision - 0.95).abs() < 1e-9);
                assert!((opts.boost_precision - 0.85).abs() < 1e-9);
            }
            _ => panic!("Expected Calibrate command"),
        }
    }
}
