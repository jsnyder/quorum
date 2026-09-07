// Library half of the bin/lib hybrid split: re-export `quorum::foo` modules
// at the binary crate root so existing `crate::foo` paths inside main.rs and
// its submodules continue to resolve unchanged. See `src/lib.rs` for the
// actual module declarations and rationale for the split.
pub use quorum::analysis;
pub use quorum::ast_grep;
pub use quorum::calibrator;
pub use quorum::calibrator_model;
pub use quorum::calibrator_trace;
pub use quorum::category;
pub use quorum::domain;
pub use quorum::embeddings;
pub use quorum::feedback;
pub use quorum::feedback_index;
pub use quorum::file_util;
pub use quorum::finding;
pub use quorum::grounding;
pub use quorum::hydration;
pub use quorum::logistic;
pub use quorum::merge;
pub use quorum::parser;
pub use quorum::patterns;
pub use quorum::prompt_sanitize;
pub use quorum::prose_prompts;
pub use quorum::redact;
pub use quorum::review_mode;
pub use quorum::skill_audit;
pub use quorum::skill_manifest;
pub use quorum::skill_prompt_defense;
pub use quorum::storage;

#[allow(dead_code)]
mod agent;
#[allow(dead_code)]
mod analytics;
#[allow(dead_code)]
mod cache;
mod cli;
mod cli_io;
mod config;
#[allow(dead_code)]
mod context;
#[allow(dead_code)]
mod context_enrichment;
#[allow(dead_code)]
mod daemon;
mod dep_manifest;
mod dimensions;
#[allow(dead_code)]
mod enrichment_policy;
#[allow(dead_code)]
mod formatting;
#[allow(dead_code)]
mod github_report;
mod glyphs;
#[allow(dead_code)]
mod http_server;
mod judge;
#[allow(dead_code)]
mod linter;
#[allow(dead_code)]
mod llm_client;
#[allow(dead_code)]
mod mcp;
#[allow(dead_code)]
mod output;
#[allow(dead_code)]
mod pipeline;
#[allow(dead_code)]
mod progress;
mod review;
#[allow(dead_code)]
mod review_log;
#[allow(dead_code)]
mod stats;
mod stats_math;
mod suppress;
#[allow(dead_code)]
mod telemetry;
#[cfg(test)]
mod test_support;
#[allow(dead_code)]
mod tools;
mod trace_subscriber;

use clap::Parser;
use config::{Config, EnvConfigSource};
use pipeline::{LlmReviewer, PipelineConfig};

/// Resolve the quorum state directory, honoring the `QUORUM_HOME` env
/// override so integration tests can be hermetic. Falls back to
/// `$HOME/.quorum`. Returns None if neither can be resolved.
fn quorum_dir() -> Option<std::path::PathBuf> {
    if let Ok(override_path) = std::env::var("QUORUM_HOME")
        && !override_path.is_empty()
    {
        return Some(std::path::PathBuf::from(override_path));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".quorum"))
}

/// Opportunistically re-link feedback entries that have `finding_id: None`
/// against review metadata. Runs silently — no output, swallows errors.
/// Called alongside `drain_agent_inbox` so linkage improves automatically.
fn backfill_linkage_opportunistic() {
    let Some(home) = quorum_dir() else {
        return;
    };
    let (linked, _) = backfill_linkage_inner(&home);
    if linked > 0 {
        tracing::info!(linked, "auto-backfilled feedback finding_id linkage");
    }
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

/// #491: `stats --skills` -- per-axis rollup of `skill_invocations.jsonl`.
/// `AuditReader::load_all` shipped fully tested with zero production callers,
/// which is how the axis reviewer stayed silent for 440 invocations. This is
/// the caller. Exits the process, like the other `stats` dimensional views.
fn run_skills_view(quorum_home: &std::path::Path, opts: &cli::StatsOpts) -> ! {
    let reader: skill_audit::AuditReader<skill_audit::SkillInvocationRecord> =
        skill_audit::AuditReader::new(quorum_home.join(skill_audit::SKILL_INVOCATIONS_FILE));
    // A missing log is a fresh install, not a failure.
    let (records, read_stats) = match reader.load_all() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot read skill invocation log: {e}");
            std::process::exit(3);
        }
    };
    let rows = dimensions::group_by_skill(&records);

    let is_terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
    match output::resolve_output_mode(opts.json, opts.compact, is_terminal) {
        output::OutputMode::Json => {
            let payload = serde_json::json!({
                "mode": "skills",
                "rows": rows,
                "meta": {
                    "total_lines": read_stats.total_lines,
                    "parsed_ok": read_stats.parsed_ok,
                    "parse_errors": read_stats.parse_errors,
                },
            });
            match serde_json::to_string_pretty(&payload) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("error: failed to serialize --skills output: {e}");
                    std::process::exit(3);
                }
            }
        }
        output::OutputMode::Compact => {
            println!("{}", stats::format_skill_compact(&rows, &read_stats));
        }
        output::OutputMode::Human => {
            let style = output::Style::detect(false);
            print!(
                "{}",
                stats::format_skill_table(&rows, &read_stats, &style, unicode_ok())
            );
        }
    }
    std::process::exit(0);
}

/// #491: `stats --integrator` -- decision-kind rollup of
/// `integrator_decisions.jsonl`. Merge and clamp behaviour is where the
/// v0.28.0 severity collapse hid for two months.
fn run_integrator_view(quorum_home: &std::path::Path, opts: &cli::StatsOpts) -> ! {
    let reader: skill_audit::AuditReader<skill_audit::IntegratorDecisionRecord> =
        skill_audit::AuditReader::new(quorum_home.join(skill_audit::INTEGRATOR_DECISIONS_FILE));
    let (records, read_stats) = match reader.load_all() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot read integrator decision log: {e}");
            std::process::exit(3);
        }
    };
    let (rows, transitions) = dimensions::group_by_integrator_decision(&records);

    let is_terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
    match output::resolve_output_mode(opts.json, opts.compact, is_terminal) {
        output::OutputMode::Json => {
            let payload = serde_json::json!({
                "mode": "integrator",
                "rows": rows,
                "severity_transitions": transitions,
                "meta": {
                    "total_lines": read_stats.total_lines,
                    "parsed_ok": read_stats.parsed_ok,
                    "parse_errors": read_stats.parse_errors,
                },
            });
            match serde_json::to_string_pretty(&payload) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("error: failed to serialize --integrator output: {e}");
                    std::process::exit(3);
                }
            }
        }
        output::OutputMode::Compact => {
            println!(
                "{}",
                stats::format_integrator_compact(&rows, &transitions, &read_stats)
            );
        }
        output::OutputMode::Human => {
            let style = output::Style::detect(false);
            print!(
                "{}",
                stats::format_integrator_table(&rows, &transitions, &read_stats, &style)
            );
        }
    }
    std::process::exit(0);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    match args.command {
        cli::Command::Review(opts) => {
            drain_agent_inbox();
            backfill_linkage_opportunistic();
            let exit_code = run_review(opts).await;
            std::process::exit(exit_code);
        }
        cli::Command::Stats(opts) => {
            drain_agent_inbox();
            backfill_linkage_opportunistic();
            // Resolve the quorum state dir honoring QUORUM_HOME (used by
            // hermetic tests and alternate installs). Falls back to
            // `$HOME/.quorum`, then to `./.quorum` as a last resort.
            let quorum_home = quorum_dir().unwrap_or_else(|| std::path::PathBuf::from(".quorum"));
            let storage_handle = quorum::storage::initialize(&quorum_home).unwrap_or_else(|e| {
                eprintln!(
                    "warning: failed to initialize storage: {}. Using in-memory database.",
                    e
                );
                quorum::storage::in_memory_handle()
            });

            // --join-health diagnostic: short-circuit normal dashboard, just
            // report reviews↔feedback finding_id linkage rate. Used to assess
            // whether per-finding precision math (Phase A) is trustworthy or
            // should fall back to entry-level with a banner.
            if opts.join_health {
                let exit_code = run_join_health(&quorum_home);
                std::process::exit(exit_code);
            }

            // #491: audit-log views over skill_invocations.jsonl /
            // integrator_decisions.jsonl. Bodies live in helpers so main()
            // does not grow another two 55-line arms.
            if opts.integrator {
                run_integrator_view(&quorum_home, &opts);
            }
            if opts.skills {
                run_skills_view(&quorum_home, &opts);
            }

            // Dimensional views read reviews.jsonl and aggregate via the
            // `dimensions` module. Classic dims: --by-repo/--by-caller/--rolling.
            // Context dims (Task 6.3): --by-source/--by-reviewed-repo/--misleading.
            // Context dims compose with --rolling by restricting aggregation to
            // the chronologically-last N records.
            if opts.by_rule {
                let fb_store = feedback::FeedbackStore::new(quorum_home.join("feedback.jsonl"));
                let entries = match fb_store.load_all() {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("error: cannot read feedback store: {e}");
                        std::process::exit(3);
                    }
                };
                let slices = dimensions::group_by_rule(&entries, opts.rule.as_deref());

                let is_pipe = !std::io::IsTerminal::is_terminal(&std::io::stdout());
                let use_compact = output::should_use_compact(opts.compact);
                let use_json = opts.json || (is_pipe && !use_compact);

                if use_json {
                    let payload = serde_json::json!({
                        "mode": "by-rule",
                        "rows": slices,
                        "meta": {
                            "total_feedback_entries": entries.len(),
                            "filter": opts.rule,
                        },
                    });
                    match serde_json::to_string_pretty(&payload) {
                        Ok(json) => println!("{json}"),
                        Err(e) => {
                            eprintln!("error: failed to serialize --by-rule output: {e}");
                            std::process::exit(3);
                        }
                    }
                } else {
                    println!(
                        "{:<55} {:>4} {:>4} {:>4} {:>4} {:>6} {:>5}",
                        "Rule", "TP", "FP", "Part", "Won't", "Prec%", "Total"
                    );
                    println!("{}", "-".repeat(85));
                    for s in &slices {
                        println!(
                            "{:<55} {:>4} {:>4} {:>4} {:>4} {:>5.1}% {:>5}{}",
                            s.key,
                            s.tp,
                            s.fp,
                            s.partial,
                            s.wontfix,
                            s.precision * 100.0,
                            s.total,
                            if s.low_sample { " *" } else { "" }
                        );
                    }
                    if slices.iter().any(|s| s.low_sample) {
                        println!("\n* = low sample (<{} entries)", dimensions::MIN_SAMPLE);
                    }
                }
                std::process::exit(0);
            }

            if opts.by_file {
                let fb_store = feedback::FeedbackStore::new(quorum_home.join("feedback.jsonl"));
                let entries = match fb_store.load_all() {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("error: cannot read feedback store: {e}");
                        std::process::exit(3);
                    }
                };
                let rows = dimensions::group_by_file(&entries, opts.top);

                let is_pipe = !std::io::IsTerminal::is_terminal(&std::io::stdout());
                let use_compact = output::should_use_compact(opts.compact);
                let use_json = opts.json || (is_pipe && !use_compact);

                if use_json {
                    let payload = serde_json::json!({
                        "mode": "by-file",
                        "rows": rows,
                        "meta": {
                            "total_feedback_entries": entries.len(),
                            "top": opts.top,
                        },
                    });
                    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
                } else if use_compact {
                    println!("{}", stats::format_file_hotspots_compact(&rows));
                } else {
                    let style = output::Style::detect(false);
                    let unicode = unicode_ok();
                    print!("{}", stats::format_file_hotspots(&rows, &style, unicode));
                }
                std::process::exit(0);
            }

            let want_context_dim = opts.by_source || opts.by_reviewed_repo || opts.misleading;
            let want_classic_dim = !want_context_dim
                && (opts.by_repo || opts.by_caller || opts.by_version || opts.rolling.is_some());

            if want_context_dim {
                let log = review_log::ReviewLog::with_storage(storage_handle.clone());
                let all_records = match log.load_all() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("error: cannot read reviews log: {e}");
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

                let out_mode = output::resolve_output_mode(
                    opts.json,
                    opts.compact,
                    std::io::IsTerminal::is_terminal(&std::io::stdout()),
                );

                if out_mode == output::OutputMode::Json {
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
                } else if out_mode == output::OutputMode::Compact {
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
                let log = review_log::ReviewLog::with_storage(storage_handle.clone());
                let (mode, records, slices) = if opts.by_repo {
                    let records = match log.load_all() {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: cannot read reviews log: {e}");
                            std::process::exit(3);
                        }
                    };
                    let slices = dimensions::group_by_repo(&records);
                    ("by-repo", records, slices)
                } else if opts.by_caller {
                    let records = match log.load_all() {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: cannot read reviews log: {e}");
                            std::process::exit(3);
                        }
                    };
                    let slices = dimensions::group_by_caller(&records);
                    ("by-caller", records, slices)
                } else if opts.by_version {
                    let records = match log.load_all() {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: cannot read reviews log: {e}");
                            std::process::exit(3);
                        }
                    };
                    let slices = dimensions::group_by_version(&records);
                    ("by-version", records, slices)
                } else {
                    let n = opts.rolling.unwrap();
                    let window_count = 3usize;
                    let needed = n.saturating_mul(window_count);
                    let records = match log.load_recent(needed) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: cannot read reviews log: {e}");
                            std::process::exit(3);
                        }
                    };
                    let slices = dimensions::rolling_window(&records, n, window_count);
                    ("rolling", records, slices)
                };

                let out_mode = output::resolve_output_mode(
                    opts.json,
                    opts.compact,
                    std::io::IsTerminal::is_terminal(&std::io::stdout()),
                );

                if out_mode == output::OutputMode::Json {
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
                } else if out_mode == output::OutputMode::Compact {
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
            let telemetry_store = telemetry::TelemetryStore::with_storage(storage_handle.clone());
            let review_log = review_log::ReviewLog::with_storage(storage_handle.clone());

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
                            print!(
                                "{}",
                                stats::format_human_with_full(&report, &style, opts.full)
                            );
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
        cli::Command::Feedback(opts) => {
            backfill_linkage_opportunistic();
            std::process::exit(run_feedback(opts))
        }
        cli::Command::Context(opts) => std::process::exit(run_context(opts)),
        cli::Command::Calibrate(opts) => std::process::exit(run_calibrate(opts)),
        cli::Command::BackfillLinkage(opts) => std::process::exit(run_backfill_linkage(opts)),
        cli::Command::Report(opts) => {
            let exit_code = run_report(opts).await;
            std::process::exit(exit_code);
        }
        cli::Command::Version => {
            println!("quorum {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(())
}

/// Build the rendered `--join-health` diagnostic from a quorum state dir.
///
/// Pure function — reads the JSONL files but produces a String rather than
/// printing, so unit tests can pin the contract on a tempdir fixture.
fn format_join_health(quorum_home: &std::path::Path) -> String {
    use std::fmt::Write;

    let storage_handle = match quorum::storage::initialize(quorum_home) {
        Ok(h) => h,
        Err(e) => {
            let mut out = String::new();
            writeln!(out, "Linkage health").unwrap();
            writeln!(out, "  ERROR: failed to read reviews: {e}").unwrap();
            return out;
        }
    };
    let log = review_log::ReviewLog::with_storage(storage_handle);
    let review_count = match log.count() {
        Ok(n) => n,
        Err(e) => {
            let mut out = String::new();
            writeln!(out, "Linkage health").unwrap();
            writeln!(out, "  ERROR: failed to read reviews: {e}").unwrap();
            return out;
        }
    };
    let finding_ids = match log.load_all_finding_ids() {
        Ok(ids) => ids,
        Err(e) => {
            // Surface the failure rather than rendering a misleading
            // "0 reviews" line. The diagnostic exists to assess data
            // health — silent zeros would defeat the point.
            let mut out = String::new();
            writeln!(out, "Linkage health").unwrap();
            writeln!(out, "  ERROR: failed to read reviews: {e}").unwrap();
            return out;
        }
    };

    let store = feedback::FeedbackStore::new(quorum_home.join("feedback.jsonl"));
    let feedback = match store.load_all() {
        Ok(f) => f,
        Err(e) => {
            let mut out = String::new();
            writeln!(out, "Linkage health").unwrap();
            writeln!(out, "  ERROR: failed to read feedback.jsonl: {e}").unwrap();
            return out;
        }
    };

    let stats = analytics::linkage_stats_from_ids(&finding_ids, &feedback);
    let total_findings = finding_ids.len();
    let rate = stats.rate();
    let rate_pct = (rate * 100.0).round() as u32;

    let mut out = String::new();
    writeln!(out, "Linkage health").unwrap();
    writeln!(
        out,
        "  Reviews: {} with {} findings",
        review_count, total_findings
    )
    .unwrap();
    writeln!(
        out,
        "  Feedback: {} entries ({} linked, {} unlinked legacy)",
        feedback.len(),
        stats.linked,
        stats.unlinked
    )
    .unwrap();
    if stats.linked + stats.unlinked == 0 {
        writeln!(out, "  Linkage rate: — (no feedback entries)").unwrap();
    } else if rate < 0.85 {
        writeln!(
            out,
            "  Linkage rate: {}%   ← below 85% threshold; per-finding precision falls back to entry-level",
            rate_pct
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "  Linkage rate: {}%   ← per-finding precision math active",
            rate_pct
        )
        .unwrap();
    }
    out
}

/// `quorum stats --join-health` — diagnostic that reports the
/// reviews↔feedback `finding_id` linkage rate. Used to assess whether
/// per-finding precision math (Phase A headline trend) is trustworthy or
/// should fall back to entry-level with a banner.
///
/// Exit code is always 0 — this is a diagnostic, not a check. A low
/// linkage rate is a real-world condition during rollout, not a failure.
fn run_join_health(quorum_home: &std::path::Path) -> i32 {
    print!("{}", format_join_health(quorum_home));
    0
}

/// Translate clap args for `quorum context ...` into a `ContextCmd` and run
/// it against `ProdDeps`. Prints stdout to stdout, warnings (one per line)
/// to stderr. Exit code: 0 on success (unless `doctor` reports any failing
/// check, in which case 1), 1 on handler error.
fn run_context(opts: cli::ContextOpts) -> i32 {
    use context::cli::{
        AddArgs, AddLocation, ContextCmd, DoctorArgs, DoctorFormat, IndexArgs, ListArgs,
        ListFormat, ProdDeps, PruneArgs, QueryArgs, QueryFormat, SourceSelector, run_context_cmd,
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
                (Some(_), None) if a.rev.is_some() => {
                    eprintln!("error: --rev may only be used with --git, not --path");
                    return 1;
                }
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
            force: i.force,
        }),
        cli::ContextCommand::Refresh(r) => {
            eprintln!("warning: `refresh` is deprecated, use `index` instead");
            ContextCmd::Index(IndexArgs {
                selector: selector(r.source, r.all),
                force: r.force,
            })
        }
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

    // `index` writes to `chunks_vec`; if fastembed fell back to HashEmbedder
    // we'd rebuild the vector table with hashing-noise vectors and silently
    // degrade every subsequent retrieval. Use the strict factory so a
    // fastembed init failure surfaces as a clear error the user can retry,
    // rather than a corrupted index they have to discover.
    let needs_strict_embedder = matches!(cmd, ContextCmd::Index(_));
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
    if let Some(term) = std::env::var_os("TERM")
        && term == "dumb"
    {
        return false;
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

async fn run_report(opts: cli::ReportOpts) -> i32 {
    let json_str = if opts.findings_file == "-" {
        use std::io::Read;
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("Error: failed to read stdin: {}", e);
            return 3;
        }
        buf
    } else {
        match std::fs::read_to_string(&opts.findings_file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: failed to read {}: {}", opts.findings_file, e);
                return 3;
            }
        }
    };

    let findings: Vec<finding::Finding> = match serde_json::from_str(&json_str) {
        Ok(f) => f,
        Err(_) => match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(v) => {
                let mut merged = Vec::new();
                if let Some(files) = v.get("files").and_then(|x| x.as_array()) {
                    for file_entry in files {
                        if let Some(findings_val) = file_entry.get("findings") {
                            match serde_json::from_value::<Vec<finding::Finding>>(
                                findings_val.clone(),
                            ) {
                                Ok(mut ff) => merged.append(&mut ff),
                                Err(e) => {
                                    eprintln!("Error: invalid grouped findings payload: {}", e);
                                    return 3;
                                }
                            }
                        }
                    }
                    merged
                } else {
                    eprintln!("Error: unsupported findings JSON format");
                    return 3;
                }
            }
            Err(e) => {
                eprintln!("Error: failed to parse findings JSON: {}", e);
                return 3;
            }
        },
    };

    let ctx = match github_report::resolve_github_context(
        opts.github_token.as_deref(),
        opts.github_repo.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            return 3;
        }
    };

    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: failed to initialize HTTP client: {}", e);
            return 3;
        }
    };

    let diff_text = if let Some(ref diff_path) = opts.diff_file {
        match std::fs::read_to_string(diff_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: failed to read diff file: {}", e);
                return 3;
            }
        }
    } else {
        match github_report::fetch_pr_diff(
            &client, &ctx.owner, &ctx.repo, opts.pr, &ctx.token, None,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: failed to fetch PR diff: {}", e);
                return 3;
            }
        }
    };

    let commit_sha = match github_report::fetch_pr_head_sha(
        &client, &ctx.owner, &ctx.repo, opts.pr, &ctx.token, None,
    )
    .await
    {
        Ok(sha) => sha,
        Err(e) => {
            eprintln!("Error: failed to fetch PR head SHA: {}", e);
            return 3;
        }
    };

    let run_id = ulid::Ulid::new().to_string();
    let version = env!("CARGO_PKG_VERSION").to_string();

    let req = github_report::PostReviewRequest {
        owner: ctx.owner,
        repo: ctx.repo,
        pr_number: opts.pr,
        token: ctx.token,
        findings,
        diff_text,
        version,
        run_id,
        commit_sha,
        api_base_url: None,
    };

    eprint!(
        "Posting {} findings to PR #{}...",
        req.findings.len(),
        req.pr_number
    );

    match github_report::post_review(&client, &req).await {
        Ok(result) => {
            if let Some(dismissed) = result.dismissed_previous {
                eprint!(" dismissed review {}...", dismissed);
            }
            eprintln!(
                " done ({} inline, {} in summary)",
                result.inline_count, result.body_count
            );
            0
        }
        Err(e) => {
            eprintln!("\nError: GitHub post failed: {}", e);
            3
        }
    }
}

// ---------------------------------------------------------------------------
// Axis resolution: maps (--axes, --mode, --deep/--daemon/--ensemble) to a
// skill set or legacy fallback. Pure logic, no I/O.
// ---------------------------------------------------------------------------

/// The resolved set of skill axes for a review invocation.
#[derive(Debug)]
struct ResolvedAxes {
    skills: Vec<crate::skill_manifest::LoadedSkill>,
    source: crate::skill_audit::AxisSelectionSource,
}

/// The default code-mode macro axes, applied when no `--axes` flag is given
/// and the mode is `Code` with no legacy flags active.
const CODE_MODE_MACRO_AXES: &[&str] = &[
    "correctness",
    "security",
    "testing-antipatterns",
    "simplicity",
    "performance",
    "architecture",
];

/// Bridges the binary-side `OpenAiClient` (which implements `pipeline::LlmReviewer`)
/// to the lib-side `skill_executor::LlmReviewer` trait.
struct SkillLlmAdapter(std::sync::Arc<llm_client::OpenAiClient>);

impl quorum::skill_executor::LlmReviewer for SkillLlmAdapter {
    fn review(
        &self,
        prompt: &str,
        model: &str,
        system_prompt: &str,
    ) -> anyhow::Result<quorum::skill_executor::LlmResponse> {
        let resp = pipeline::LlmReviewer::review(&*self.0, prompt, model, system_prompt)?;
        let usage = resp.usage.map(|u| quorum::skill_executor::TokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            cached_tokens: u.cached_tokens,
        });
        Ok(quorum::skill_executor::LlmResponse {
            content: resp.content,
            usage,
        })
    }
}

/// Report skill cells that failed, and return how many.
///
/// Cells expand to skills x models x files, so identity comes from the cell
/// itself -- never from position in the skills list. A parse failure that is
/// only written to the audit log looks identical to a clean file: that is how
/// the axis reviewer emitted zero findings for 440 invocations without anyone
/// noticing. Callers must fold the count into the exit status.
fn report_failed_skill_cells(
    cell_results: &[quorum::skill_executor::CellResult],
    file: &str,
) -> usize {
    let failed: Vec<String> = cell_results
        .iter()
        .filter(|c| c.parse_error_class.is_some() || c.failure_reason.is_some())
        .map(|c| match &c.parse_error_class {
            Some(class) => format!("{}/{} ({class})", c.skill_name, c.actual_model),
            None => format!("{}/{}", c.skill_name, c.actual_model),
        })
        .collect();
    if !failed.is_empty() {
        eprintln!(
            "Warning: {} of {} skill axes failed on {}: {}",
            failed.len(),
            cell_results.len(),
            file,
            failed.join(", "),
        );
    }
    failed.len()
}

/// Resolve the review axis set from CLI flags and available skills.
///
/// Returns:
/// - `Ok(Some(ResolvedAxes))` — a concrete skill set to execute.
/// - `Ok(None)`               — fall back to the legacy single-prompt pipeline.
/// - `Err(String)`            — user-facing error message (reserved mode, conflict, unknown axis).
fn resolve_axes(
    axes: &[String],
    mode: crate::review_mode::ReviewMode,
    deep: bool,
    daemon: bool,
    ensemble: bool,
    available_skills: &[crate::skill_manifest::LoadedSkill],
) -> Result<Option<ResolvedAxes>, String> {
    // -----------------------------------------------------------------------
    // (a) Reserved mode — hard error with placeholder skill names
    // -----------------------------------------------------------------------
    if mode.is_reserved() {
        let placeholder = match mode {
            crate::review_mode::ReviewMode::Tests => "test-coverage, test-quality",
            crate::review_mode::ReviewMode::Release => "release-readiness",
            _ => unreachable!(),
        };
        return Err(format!(
            "mode '{}' requires axes not installed in this version: [{}]",
            mode, placeholder,
        ));
    }

    // -----------------------------------------------------------------------
    // Normalize --axes: filter empties, lowercase, deduplicate (preserving
    // first-occurrence order).
    // -----------------------------------------------------------------------
    let normalized: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        axes.iter()
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .filter(|a| seen.insert(a.clone()))
            .collect()
    };

    let has_explicit_axes = !normalized.is_empty();

    // -----------------------------------------------------------------------
    // (b) Explicit --axes + legacy flag → hard error
    // -----------------------------------------------------------------------
    if has_explicit_axes {
        let legacy_flag = if deep {
            Some("--deep")
        } else if daemon {
            Some("--daemon")
        } else if ensemble {
            Some("--ensemble")
        } else {
            None
        };
        if let Some(flag) = legacy_flag {
            return Err(format!(
                "multi-axis review is not supported with {} yet",
                flag,
            ));
        }
    }

    // -----------------------------------------------------------------------
    // (c) Explicit --axes → validate against available skills
    // -----------------------------------------------------------------------
    if has_explicit_axes {
        let available_names: Vec<String> = available_skills
            .iter()
            .map(|s| s.manifest.name.to_ascii_lowercase())
            .collect();

        let mut matched = Vec::new();
        for axis in &normalized {
            let idx = available_names
                .iter()
                .position(|n| n == axis)
                .ok_or_else(|| {
                    format!(
                        "unknown skill axis '{}'; available: [{}]",
                        axis,
                        available_names.join(", "),
                    )
                })?;
            matched.push(available_skills[idx].clone());
        }
        return Ok(Some(ResolvedAxes {
            skills: matched,
            source: crate::skill_audit::AxisSelectionSource::ExplicitAxes,
        }));
    }

    // -----------------------------------------------------------------------
    // (d) Legacy flag without explicit --axes → legacy fallback
    // -----------------------------------------------------------------------
    if deep || daemon || ensemble {
        return Ok(None);
    }

    // -----------------------------------------------------------------------
    // (e) Default code mode → ModeMacro with bundled axes
    // -----------------------------------------------------------------------
    if mode == crate::review_mode::ReviewMode::Code && !available_skills.is_empty() {
        let mut skills = Vec::new();
        let mut all_found = true;
        for &axis_name in CODE_MODE_MACRO_AXES {
            if let Some(skill) = available_skills
                .iter()
                .find(|s| s.manifest.name.eq_ignore_ascii_case(axis_name))
            {
                skills.push(skill.clone());
            } else {
                tracing::warn!(
                    skill = axis_name,
                    "bundled skill not found; falling back to legacy review"
                );
                all_found = false;
                break;
            }
        }
        if all_found {
            return Ok(Some(ResolvedAxes {
                skills,
                source: crate::skill_audit::AxisSelectionSource::ModeMacro,
            }));
        }
    }

    // -----------------------------------------------------------------------
    // (f) Prose modes (plan, docs) without --axes → legacy fallback
    // -----------------------------------------------------------------------
    Ok(None)
}

// ---------------------------------------------------------------------------
// Tests for resolve_axes
// ---------------------------------------------------------------------------

#[cfg(test)]
mod axes_tests {
    use super::*;

    fn mock_skill(name: &str) -> crate::skill_manifest::LoadedSkill {
        crate::skill_manifest::LoadedSkill {
            manifest: crate::skill_manifest::SkillManifest {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                display_name: name.to_string(),
                description: String::new(),
                axis: crate::skill_manifest::Axis::Correctness,
                max_severity: crate::finding::Severity::Critical,
                target_findings: None,
                capability: crate::skill_manifest::Capability {
                    mode: crate::skill_manifest::CapabilityMode::Pure,
                },
                preferred_model: None,
                fallback_models: None,
                calibration_namespace: None,
                prompts: crate::skill_manifest::Prompts {
                    primary: "test prompt".into(),
                    anthropic: None,
                    openai: None,
                    google: None,
                },
                checklist: vec![],
                ast_rules: vec![],
            },
            source_path: std::path::PathBuf::from(format!("skills/{}.toml", name)),
            manifest_sha256: "abc123".to_string(),
            trust_tier: crate::skill_manifest::TrustTier::Bundled,
        }
    }

    fn bundled_skills() -> Vec<crate::skill_manifest::LoadedSkill> {
        vec![
            mock_skill("architecture"),
            mock_skill("correctness"),
            mock_skill("performance"),
            mock_skill("security"),
            mock_skill("simplicity"),
            mock_skill("testing-antipatterns"),
        ]
    }

    // A1: explicit_axes_resolves
    #[test]
    fn explicit_axes_resolves() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["security".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let resolved = result.unwrap().unwrap();
        assert_eq!(resolved.skills.len(), 1);
        assert_eq!(resolved.skills[0].manifest.name, "security");
        assert_eq!(
            resolved.source,
            crate::skill_audit::AxisSelectionSource::ExplicitAxes,
        );
    }

    // A2: explicit_multiple_axes
    #[test]
    fn explicit_multiple_axes() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["correctness".into(), "security".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let resolved = result.unwrap().unwrap();
        assert_eq!(resolved.skills.len(), 2);
        assert_eq!(resolved.skills[0].manifest.name, "correctness");
        assert_eq!(resolved.skills[1].manifest.name, "security");
        assert_eq!(
            resolved.source,
            crate::skill_audit::AxisSelectionSource::ExplicitAxes,
        );
    }

    // A3: default_code_mode_resolves_to_bundled
    #[test]
    fn default_code_mode_resolves_to_bundled() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let resolved = result.unwrap().unwrap();
        assert_eq!(resolved.skills.len(), 6);
        assert_eq!(
            resolved.source,
            crate::skill_audit::AxisSelectionSource::ModeMacro,
        );
        let names: Vec<&str> = resolved
            .skills
            .iter()
            .map(|s| s.manifest.name.as_str())
            .collect();
        assert_eq!(
            names,
            &[
                "correctness",
                "security",
                "testing-antipatterns",
                "simplicity",
                "performance",
                "architecture",
            ]
        );
    }

    // A4: deep_suppresses_default_axes
    #[test]
    fn deep_suppresses_default_axes() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Code,
            true,
            false,
            false,
            &skills,
        );
        assert!(result.unwrap().is_none());
    }

    // A5: daemon_suppresses_default_axes
    #[test]
    fn daemon_suppresses_default_axes() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Code,
            false,
            true,
            false,
            &skills,
        );
        assert!(result.unwrap().is_none());
    }

    // A6: ensemble_suppresses_default_axes
    #[test]
    fn ensemble_suppresses_default_axes() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            true,
            &skills,
        );
        assert!(result.unwrap().is_none());
    }

    // A7: explicit_axes_with_deep_errors
    #[test]
    fn explicit_axes_with_deep_errors() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["security".into()],
            crate::review_mode::ReviewMode::Code,
            true,
            false,
            false,
            &skills,
        );
        let err = result.unwrap_err();
        assert!(err.contains("--deep"), "expected --deep in error: {}", err);
    }

    // A8: explicit_axes_with_ensemble_errors
    #[test]
    fn explicit_axes_with_ensemble_errors() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["security".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            true,
            &skills,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("--ensemble"),
            "expected --ensemble in error: {}",
            err
        );
    }

    // A9: explicit_axes_with_daemon_errors
    #[test]
    fn explicit_axes_with_daemon_errors() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["security".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            true,
            false,
            &skills,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("--daemon"),
            "expected --daemon in error: {}",
            err
        );
    }

    // A10: reserved_mode_tests_errors
    #[test]
    fn reserved_mode_tests_errors() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Tests,
            false,
            false,
            false,
            &skills,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("test-coverage"),
            "expected test-coverage in error: {}",
            err
        );
        assert!(
            err.contains("mode 'tests'"),
            "expected mode name in error: {}",
            err
        );
    }

    // A11: reserved_mode_release_errors
    #[test]
    fn reserved_mode_release_errors() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Release,
            false,
            false,
            false,
            &skills,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("release-readiness"),
            "expected release-readiness in error: {}",
            err
        );
        assert!(
            err.contains("mode 'release'"),
            "expected mode name in error: {}",
            err
        );
    }

    // A12: unknown_axis_errors
    #[test]
    fn unknown_axis_errors() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["nonexistent".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("unknown skill axis 'nonexistent'"),
            "expected unknown axis in error: {}",
            err
        );
        assert!(
            err.contains("available:"),
            "expected available list in error: {}",
            err
        );
    }

    // A13: prose_plan_uses_legacy
    #[test]
    fn prose_plan_uses_legacy() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Plan,
            false,
            false,
            false,
            &skills,
        );
        assert!(result.unwrap().is_none());
    }

    // A14: prose_docs_uses_legacy
    #[test]
    fn prose_docs_uses_legacy() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Docs,
            false,
            false,
            false,
            &skills,
        );
        assert!(result.unwrap().is_none());
    }

    // A15: case_insensitive_axis_match
    #[test]
    fn case_insensitive_axis_match() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["SECURITY".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let resolved = result.unwrap().unwrap();
        assert_eq!(resolved.skills.len(), 1);
        assert_eq!(resolved.skills[0].manifest.name, "security");
    }

    // A16: empty_axis_strings_filtered
    #[test]
    fn empty_axis_strings_filtered_with_valid() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["".into(), "security".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let resolved = result.unwrap().unwrap();
        assert_eq!(resolved.skills.len(), 1);
        assert_eq!(resolved.skills[0].manifest.name, "security");
    }

    #[test]
    fn empty_axis_strings_filtered_all_empty() {
        let skills = bundled_skills();
        // All empty strings → normalized is empty → falls through to default
        // code mode which resolves bundled axes.
        let result = resolve_axes(
            &["".into(), "  ".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let resolved = result.unwrap().unwrap();
        // Falls through to code-mode macro since no explicit axes remain
        assert_eq!(resolved.skills.len(), 6);
        assert_eq!(
            resolved.source,
            crate::skill_audit::AxisSelectionSource::ModeMacro,
        );
    }

    // A17: duplicate_axes_deduplicated
    #[test]
    fn duplicate_axes_deduplicated() {
        let skills = bundled_skills();
        let result = resolve_axes(
            &["security".into(), "security".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        );
        let resolved = result.unwrap().unwrap();
        assert_eq!(resolved.skills.len(), 1);
        assert_eq!(resolved.skills[0].manifest.name, "security");
    }

    // A18: empty_available_skills_errors
    #[test]
    fn empty_available_skills_falls_back_to_legacy() {
        let result = resolve_axes(
            &[],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &[], // no skills available
        );
        assert!(
            result.unwrap().is_none(),
            "should fall back to legacy when no skills are installed"
        );
    }

    // A19: multi_legacy_flags_reports_first
    #[test]
    fn multi_legacy_flags_reports_first() {
        let skills = bundled_skills();
        // deep + daemon both set; deep should be reported first
        let result = resolve_axes(
            &["security".into()],
            crate::review_mode::ReviewMode::Code,
            true,
            true,
            false,
            &skills,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("--deep"),
            "expected --deep (first) in error: {}",
            err
        );
    }
}

#[cfg(test)]
mod skill_integration_tests {
    use crate::skill_audit;
    use quorum::skill_executor;
    use quorum::skill_integrator;
    use quorum::skill_manifest;

    fn mock_loaded_skill(name: &str) -> skill_manifest::LoadedSkill {
        skill_manifest::LoadedSkill {
            manifest: skill_manifest::SkillManifest {
                name: name.to_string(),
                version: "1.0.0".into(),
                display_name: name.into(),
                description: format!("{name} skill"),
                preferred_model: None,
                fallback_models: None,
                calibration_namespace: None,
                axis: skill_manifest::Axis::Correctness,
                max_severity: quorum::finding::Severity::Critical,
                target_findings: None,
                capability: skill_manifest::Capability {
                    mode: skill_manifest::CapabilityMode::Pure,
                },
                prompts: skill_manifest::Prompts {
                    primary: "Review for {name}".into(),
                    anthropic: None,
                    openai: None,
                    google: None,
                },
                checklist: vec![],
                ast_rules: vec![],
            },
            trust_tier: skill_manifest::TrustTier::Bundled,
            source_path: std::path::PathBuf::from(format!("skills/{name}.toml")),
            manifest_sha256: "test-sha".into(),
        }
    }

    fn make_finding_json(title: &str) -> String {
        let f = quorum::finding::FindingBuilder::new()
            .title(title)
            .severity(quorum::finding::Severity::Medium)
            .category(quorum::category::Category::Correctness)
            .source(quorum::finding::Source::Llm("test-model".into()))
            .lines(1, 5)
            .build();
        serde_json::to_string(&f).unwrap()
    }

    struct MockReviewer;
    impl skill_executor::LlmReviewer for MockReviewer {
        fn review(
            &self,
            _prompt: &str,
            _model: &str,
            _system_prompt: &str,
        ) -> anyhow::Result<skill_executor::LlmResponse> {
            let json = format!("[{}]", make_finding_json("Test bug"));
            Ok(skill_executor::LlmResponse {
                content: json,
                usage: Some(skill_executor::TokenUsage {
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    cached_tokens: 0,
                }),
            })
        }
    }

    #[test]
    fn full_pipeline_resolve_execute_integrate() {
        let skills = vec![
            mock_loaded_skill("correctness"),
            mock_loaded_skill("security"),
        ];

        let resolved = super::resolve_axes(
            &["correctness".into(), "security".into()],
            crate::review_mode::ReviewMode::Code,
            false,
            false,
            false,
            &skills,
        )
        .expect("resolve_axes should succeed")
        .expect("should return Some for explicit axes");

        assert_eq!(resolved.skills.len(), 2);
        assert_eq!(
            resolved.source,
            skill_audit::AxisSelectionSource::ExplicitAxes
        );

        let exec_cfg = skill_executor::SkillExecutorConfig {
            run_id: "test-integration".into(),
            axis_selection_source: resolved.source.clone(),
            global_models: vec!["test-model".into()],
            ensemble_pool: vec![],
            ensemble: false,
            max_tokens_per_review: 100_000,
            max_calls_per_review: 10,
            audit_writer: None,
        };
        let files = vec![("test.rs".into(), "abc123".into(), "fn main() {}".into())];
        let results =
            skill_executor::execute_matrix(&resolved.skills, &files, &MockReviewer, &exec_cfg);

        assert_eq!(results.len(), 2, "one CellResult per skill");
        for r in &results {
            assert!(!r.findings.is_empty(), "each skill should produce findings");
            for f in &r.findings {
                assert!(
                    f.originating_skill.is_some(),
                    "findings must carry originating_skill"
                );
            }
        }

        let tagged: Vec<_> = results
            .iter()
            .flat_map(|cr| {
                cr.findings.iter().map(|f| skill_integrator::TaggedFinding {
                    file_path: "test.rs".into(),
                    finding: f.clone(),
                })
            })
            .collect();
        let int_cfg = skill_integrator::IntegratorConfig::default();
        let output = skill_integrator::integrate(tagged, &int_cfg);
        assert!(
            !output.findings.is_empty(),
            "integrator should emit findings"
        );
        assert!(
            output.findings.len() < 4,
            "integrator should merge overlapping findings from 2 skills"
        );

        let total_prompt: u64 = results.iter().map(|r| r.usage.prompt_tokens).sum();
        assert!(total_prompt > 0, "token usage should be tracked");
    }
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

    // Prose review modes are not yet supported with --daemon or --deep.
    if opts.mode.is_prose() && (opts.daemon || opts.deep) {
        eprintln!(
            "error: --mode {} is not supported with {} yet",
            opts.mode,
            if opts.daemon { "--daemon" } else { "--deep" }
        );
        return 3;
    }

    // Reserved modes are not yet implemented.
    if opts.mode.is_reserved() {
        eprintln!(
            "error: --mode {} is reserved and not yet implemented",
            opts.mode,
        );
        return 3;
    }

    // --axes is not supported with --daemon (daemon has its own review path).
    if opts.daemon && !opts.axes.is_empty() {
        eprintln!("error: --axes is not supported with --daemon");
        return 3;
    }

    // If --daemon flag is set, send requests to running daemon
    if opts.daemon {
        // The daemon path uses reqwest::blocking and synchronous file I/O.
        // Keep it off Tokio's runtime so its blocking client can shut down
        // without panicking in an asynchronous context.
        return match tokio::task::spawn_blocking(move || run_review_via_daemon(&opts)).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("Error: daemon review worker failed: {}", e);
                3
            }
        };
    }

    // Warn on mode/extension mismatch (advisory, never blocks the review).
    {
        let prose_extensions = ["md", "txt", "adoc", "rst"];
        for f in &opts.files {
            if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                let is_prose_ext = prose_extensions.contains(&ext_lower.as_str());
                if opts.mode == crate::review_mode::ReviewMode::Code && is_prose_ext {
                    eprintln!(
                        "warning: '{}' looks like a prose file. \
                         Use --mode plan or --mode docs for non-code review.",
                        f.display()
                    );
                } else if opts.mode.is_prose() && !is_prose_ext {
                    eprintln!(
                        "warning: '{}' does not look like a prose file but --mode {} was specified.",
                        f.display(),
                        opts.mode,
                    );
                }
            }
        }
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
            let bypass_proxy_cache = opts.no_cache
                || std::env::var("QUORUM_BYPASS_PROXY_CACHE")
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

    // -----------------------------------------------------------------------
    // Skill axis resolution: load manifests, resolve --axes, build executor
    // infrastructure.
    // -----------------------------------------------------------------------
    let quorum_home_for_skills =
        quorum_dir().unwrap_or_else(|| std::path::PathBuf::from(".quorum"));
    let bundled_skills_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let user_skills_dir = quorum_home_for_skills.clone();

    let available_skills = match skill_manifest::load_skills(&bundled_skills_dir, &user_skills_dir)
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load skill manifests; skills disabled");
            vec![]
        }
    };

    let resolved_axes = match resolve_axes(
        &opts.axes,
        opts.mode,
        opts.deep,
        opts.daemon,
        opts.ensemble,
        &available_skills,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {}", e);
            return 3;
        }
    };

    // Warn if --axes given but no LLM client.
    if resolved_axes.is_some() && llm_client.is_none() && !opts.axes.is_empty() {
        eprintln!("warning: --axes requires an LLM client; running AST-only review");
    }

    // Log axis resolution.
    if let Some(ref ra) = resolved_axes {
        let names: Vec<&str> = ra.skills.iter().map(|s| s.manifest.name.as_str()).collect();
        tracing::info!(axes = ?names, source = ?ra.source, "skill axes resolved");
    }

    // Build skill executor infrastructure when axes are resolved AND LLM is available.
    let skill_adapter: Option<SkillLlmAdapter> =
        llm_client.as_ref().map(|c| SkillLlmAdapter(c.clone()));
    let skill_audit_writer: Option<
        std::sync::Arc<skill_audit::AuditWriter<skill_audit::SkillInvocationRecord>>,
    > = resolved_axes.as_ref().map(|_| {
        std::sync::Arc::new(skill_audit::AuditWriter::new(
            quorum_home_for_skills.join(skill_audit::SKILL_INVOCATIONS_FILE),
        ))
    });
    let integrator_audit_writer: Option<
        std::sync::Arc<skill_audit::AuditWriter<skill_audit::IntegratorDecisionRecord>>,
    > = resolved_axes.as_ref().map(|_| {
        std::sync::Arc::new(skill_audit::AuditWriter::new(
            quorum_home_for_skills.join(skill_audit::INTEGRATOR_DECISIONS_FILE),
        ))
    });

    // Build pipeline config.
    // Precedence: --model > QUORUM_ENSEMBLE_MODELS (ensemble only) > QUORUM_MODEL.
    let flag_models: Vec<String> = opts
        .model
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let models = if !flag_models.is_empty() {
        flag_models
    } else if opts.ensemble {
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
    let storage_handle = quorum::storage::initialize(&qhome).unwrap_or_else(|e| {
        eprintln!(
            "warning: failed to initialize storage: {}. Using in-memory database.",
            e
        );
        quorum::storage::in_memory_handle()
    });
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

    let injector_project_root = opts.files.first().map(|f| pipeline::find_project_root(f));
    let context_injector = context::bootstrap::build_production_injector_with_project(
        &qhome,
        &feedback_entries,
        injector_project_root.as_deref(),
    );
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

    // Generate run_id early so it can be shared between TraceProvenance
    // and ReviewRecord (design constraint: same ULID for join key).
    let run_id = review_log::ReviewRecord::new_ulid();

    // Compute trace provenance ONCE before per-file fanout (design H3).
    let trace_provenance = {
        let first_file = opts.files.first().map(|p| p.as_path());
        let repo = first_file.and_then(review_log::detect_repo);
        let git_dir = first_file
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let commit_sha = std::process::Command::new("git")
            .current_dir(&git_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let dirty = std::process::Command::new("git")
            .current_dir(&git_dir)
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| !o.stdout.is_empty());

        quorum::calibrator_trace::TraceProvenance {
            quorum_version: Some(env!("CARGO_PKG_VERSION").into()),
            repo,
            commit_sha,
            dirty,
            review_model: models.first().cloned(),
            run_id: Some(run_id.clone()),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
        }
    };

    // Build calibrator config, loading data-driven thresholds if available.
    let mut calibrator_config = calibrator::CalibratorConfig {
        review_model: models.first().cloned(),
        trace_provenance: Some(trace_provenance),
        ..Default::default()
    };
    let thresholds_path = qhome.join("calibrator_thresholds.toml");
    let loaded_tc =
        quorum::threshold_config::ThresholdConfig::load_from(&thresholds_path.to_string_lossy());
    if let Some(tc) = loaded_tc.as_ref() {
        calibrator_config.suppress_threshold = tc.suppress.as_ref().map(|p| p.threshold);
        calibrator_config.boost_threshold = tc.boost.as_ref().map(|p| p.threshold);
        tracing::info!(
            suppress = ?calibrator_config.suppress_threshold,
            boost = ?calibrator_config.boost_threshold,
            composite = tc.composite_model,
            "loaded data-driven calibrator thresholds"
        );
    }
    // Load composite calibrator model if available.
    let model_path = qhome.join("calibrator_model.toml");
    if let Some(model) =
        quorum::calibrator_model::CalibratorModel::load_from(&model_path.to_string_lossy())
    {
        tracing::info!(
            word_lor_entries = model.word_lor.len(),
            family_rates = model.family_fp_rate.len(),
            language_rates = model.language_fp_rate.len(),
            has_logistic = model.logistic_model.is_some(),
            "loaded composite calibrator model"
        );
        calibrator_config.model = Some(model);
    }
    if loaded_tc.as_ref().is_some_and(|tc| tc.composite_model) && calibrator_config.model.is_none()
    {
        tracing::warn!(
            "calibrator_thresholds.toml declares composite_model=true but \
             calibrator_model.toml is missing; clearing thresholds to fall \
             back to defaults"
        );
        calibrator_config.suppress_threshold = None;
        calibrator_config.boost_threshold = None;
    }
    // QUORUM_FORCE_THRESHOLD overrides both suppress and boost.
    if let Ok(v) = std::env::var("QUORUM_FORCE_THRESHOLD") {
        let has_composite = calibrator_config.model.is_some();
        match v.parse::<f64>() {
            Ok(t) if t.is_finite() && (has_composite || (0.0..=1.0).contains(&t)) => {
                calibrator_config.force_threshold = Some(t);
                tracing::warn!(
                    threshold = t,
                    "QUORUM_FORCE_THRESHOLD active -- collapses neutral zone \
                     (suppress when score < {t}, boost when score >= {t})"
                );
            }
            Ok(t) => {
                let expected = if has_composite {
                    "expected finite value"
                } else {
                    "expected finite value in [0.0, 1.0]"
                };
                tracing::warn!(
                    raw = %v,
                    parsed = t,
                    "ignoring QUORUM_FORCE_THRESHOLD: {expected}"
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

    // Phase 2: live registry lookups for popularity-tier token-budget assignment.
    // Gated behind --live-registry CLI flag or QUORUM_CONTEXT7_LIVE_REGISTRY=1.
    let live_registry = opts.live_registry
        || std::env::var("QUORUM_CONTEXT7_LIVE_REGISTRY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    let registry_client: Option<std::sync::Arc<dyn crate::enrichment_policy::RegistryClient>> =
        if live_registry && !opts.skip_context7 && !context7_disabled {
            match crate::enrichment_policy::HttpRegistryClient::new() {
                Ok(http) => {
                    let cached = crate::enrichment_policy::OwnedCachedRegistryClient::new(
                        Box::new(http),
                        128,
                    );
                    Some(std::sync::Arc::new(cached))
                }
                Err(e) => {
                    eprintln!("Warning: failed to initialize registry client: {e}");
                    None
                }
            }
        } else {
            None
        };

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
        mode: opts.mode,
        registry_client,
        judge_enabled: opts.judge
            || std::env::var("QUORUM_JUDGE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        judge_model: opts
            .judge_model
            .clone()
            .or_else(|| {
                std::env::var("QUORUM_JUDGE_MODEL")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "gpt-4.1-mini".into()),
        judge_client: llm_client.clone(),
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
    let mode = output::resolve_output_mode(
        opts.json,
        opts.compact,
        std::io::IsTerminal::is_terminal(&std::io::stdout()),
    );
    let use_json = mode == output::OutputMode::Json;
    let use_compact = mode == output::OutputMode::Compact;
    let mut all_findings = Vec::new();
    let mut file_results: Vec<pipeline::FileReviewResult> = Vec::new();
    let mut had_errors = false;
    // Shared across the sequential and parallel review paths; folded into the
    // exit status so a review whose skill axes all failed cannot exit 0.
    let skill_cells_failed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

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
            if opts.deep
                && let Some(client) = llm_client.as_deref()
            {
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
                        let sup_result =
                            suppress::apply_suppressions(findings, &suppress_rules, &file_display);
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

            // Run pipeline: full (AST + LLM) for supported languages, LLM-only for others.
            // When skills are active, suppress the single-prompt LLM in the pipeline
            // (AST-only), then run the skill matrix executor + integrator after.
            let use_skills = resolved_axes.is_some() && skill_adapter.is_some();
            let llm_for_pipeline: Option<&dyn LlmReviewer> =
                if use_skills { None } else { llm_reviewer };

            let review_result = if let Some(l) = lang {
                pipeline::review_source(
                    file_path,
                    &source,
                    l,
                    llm_for_pipeline,
                    &pipeline_cfg,
                    Some(&parse_cache),
                )
                .await
            } else {
                if !use_skills {
                    eprintln!(
                        "Note: No AST support for {}, using LLM-only review",
                        file_path.display()
                    );
                }
                pipeline::review_file(file_path, &source, None, llm_for_pipeline, &pipeline_cfg)
                    .await
            };
            match review_result {
                Ok(mut result) => {
                    // If skills active, run executor + integrator.
                    if use_skills
                        && let (Some(ra), Some(adapter)) = (&resolved_axes, &skill_adapter)
                    {
                        let file_str = file_path.to_string_lossy().to_string();
                        let file_sha = {
                            use sha2::{Digest, Sha256};
                            let mut h = Sha256::new();
                            h.update(source.as_bytes());
                            hex::encode(h.finalize())
                        };

                        let _exec_span = tracing::info_span!(
                            "phase.skill_executor",
                            skills = ra.skills.len(),
                            file = %file_str,
                        )
                        .entered();

                        let exec_cfg = quorum::skill_executor::SkillExecutorConfig {
                            run_id: run_id.clone(),
                            axis_selection_source: ra.source.clone(),
                            global_models: pipeline_cfg.models.clone(),
                            ensemble_pool: vec![],
                            ensemble: false,
                            max_tokens_per_review: 500_000,
                            max_calls_per_review: 50,
                            audit_writer: skill_audit_writer.clone(),
                        };
                        let files_input = vec![(file_str.clone(), file_sha, source.clone())];
                        let cell_results = quorum::skill_executor::execute_matrix(
                            &ra.skills,
                            &files_input,
                            adapter,
                            &exec_cfg,
                        );

                        drop(_exec_span);

                        // Surface skill-cell failures on stderr. The audit log
                        // already recorded `wrong_schema` 213 times while the
                        // axis reviewer silently emitted zero findings for two
                        // months; nothing ever read it back. A parse failure
                        // must not look like a clean file.
                        skill_cells_failed.fetch_add(
                            report_failed_skill_cells(&cell_results, &file_str),
                            std::sync::atomic::Ordering::Relaxed,
                        );

                        let _int_span = tracing::info_span!(
                                "phase.integrator",
                                input_findings = cell_results.iter().map(|c| c.findings.len()).sum::<usize>(),
                                file = %file_str,
                            ).entered();

                        let tagged: Vec<quorum::skill_integrator::TaggedFinding> = cell_results
                            .iter()
                            .flat_map(|cr| {
                                cr.findings.iter().map(|f| {
                                    quorum::skill_integrator::TaggedFinding {
                                        file_path: file_str.clone(),
                                        finding: f.clone(),
                                    }
                                })
                            })
                            .collect();

                        let int_cfg = quorum::skill_integrator::IntegratorConfig {
                            run_id: run_id.clone(),
                            confidence_floor: 0.30,
                            audit_writer: integrator_audit_writer.clone(),
                        };
                        let int_output = quorum::skill_integrator::integrate(tagged, &int_cfg);

                        tracing::info!(
                            findings = int_output.findings.len(),
                            suppressed = int_output.suppressed.len(),
                            "integrator complete"
                        );

                        // Accumulate token usage from all skill cells.
                        for cr in &cell_results {
                            result.usage.prompt_tokens += cr.usage.prompt_tokens;
                            result.usage.completion_tokens += cr.usage.completion_tokens;
                            result.usage.cached_tokens += cr.usage.cached_tokens;
                        }

                        // #486: stamp the skill findings before they join the result.
                        // review_file already classified everything it merged itself,
                        // but these arrive afterwards -- without this every LLM finding
                        // in a multi-axis review keeps in_diff: None.
                        let mut int_findings = int_output.findings;
                        if let Some(ref diff_ranges) = pipeline_cfg.diff_ranges {
                            pipeline::classify_findings_for_file(
                                &mut int_findings,
                                std::path::Path::new(&file_str),
                                diff_ranges,
                            );
                        }
                        result.findings.extend(int_findings);
                        result.suppressed += int_output.suppressed.len();
                    }

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

        // Arc-wrap skill infrastructure for cross-thread sharing.
        let resolved_axes_arc: Option<std::sync::Arc<ResolvedAxes>> =
            resolved_axes.map(std::sync::Arc::new);
        let skill_audit_writer_arc = skill_audit_writer;
        let integrator_audit_writer_arc = integrator_audit_writer;

        for (idx, file_path) in opts.files.iter().enumerate() {
            let file_path = file_path.clone();
            let pipeline_cfg = pipeline_cfg.clone();
            let suppress_rules = suppress_rules.clone();
            let _show_suppressed = opts.show_suppressed;
            let deep = opts.deep;
            let llm_client = llm_client.clone();
            let resolved_axes = resolved_axes_arc.clone();
            let skill_audit_writer = skill_audit_writer_arc.clone();
            let integrator_audit_writer = integrator_audit_writer_arc.clone();
            let run_id = run_id.clone();
            let skill_cells_failed = skill_cells_failed.clone();

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
                if deep && let Some(ref client) = llm_client {
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
                                judge_metrics: Default::default(),
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

                // Standard review path. When skills are active, suppress single-prompt LLM.
                let use_skills = resolved_axes.is_some() && llm_client.is_some();
                let llm_reviewer: Option<&dyn pipeline::LlmReviewer> = if use_skills {
                    None
                } else {
                    llm_client.as_deref().map(|c| c as _)
                };
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
                        pipeline::review_file(
                            &file_path,
                            &source,
                            None,
                            llm_reviewer,
                            &pipeline_cfg,
                        )
                        .await
                    }
                });

                match review_result {
                    Ok(mut result) => {
                        // If skills active, run executor + integrator.
                        if use_skills
                            && let (Some(ra), Some(client)) = (&resolved_axes, &llm_client)
                        {
                                let file_str = file_path.to_string_lossy().to_string();
                                let file_sha = {
                                    use sha2::{Sha256, Digest};
                                    let mut h = Sha256::new();
                                    h.update(source.as_bytes());
                                    hex::encode(h.finalize())
                                };

                                let _exec_span = tracing::info_span!(
                                    "phase.skill_executor",
                                    skills = ra.skills.len(),
                                    file = %file_str,
                                ).entered();

                                let adapter = SkillLlmAdapter(client.clone());
                                let exec_cfg = quorum::skill_executor::SkillExecutorConfig {
                                    run_id: run_id.clone(),
                                    axis_selection_source: ra.source.clone(),
                                    global_models: pipeline_cfg.models.clone(),
                                    ensemble_pool: vec![],
                                    ensemble: false,
                                    max_tokens_per_review: 500_000,
                                    max_calls_per_review: 50,
                                    audit_writer: skill_audit_writer.clone(),
                                };
                                let files_input = vec![(file_str.clone(), file_sha, source.clone())];
                                let cell_results = quorum::skill_executor::execute_matrix(
                                    &ra.skills, &files_input, &adapter, &exec_cfg,
                                );

                                drop(_exec_span);

                                skill_cells_failed.fetch_add(
                                    report_failed_skill_cells(&cell_results, &file_str),
                                    std::sync::atomic::Ordering::Relaxed,
                                );

                                let _int_span = tracing::info_span!(
                                    "phase.integrator",
                                    input_findings = cell_results.iter().map(|c| c.findings.len()).sum::<usize>(),
                                    file = %file_str,
                                ).entered();

                                let tagged: Vec<quorum::skill_integrator::TaggedFinding> = cell_results
                                    .iter()
                                    .flat_map(|cr| {
                                        cr.findings.iter().map(|f| quorum::skill_integrator::TaggedFinding {
                                            file_path: file_str.clone(),
                                            finding: f.clone(),
                                        })
                                    })
                                    .collect();

                                let int_cfg = quorum::skill_integrator::IntegratorConfig {
                                    run_id: run_id.clone(),
                                    confidence_floor: 0.30,
                                    audit_writer: integrator_audit_writer.clone(),
                                };
                                let int_output = quorum::skill_integrator::integrate(tagged, &int_cfg);

                                tracing::info!(
                                    findings = int_output.findings.len(),
                                    suppressed = int_output.suppressed.len(),
                                    "integrator complete"
                                );

                                // Accumulate token usage from all skill cells.
                                for cr in &cell_results {
                                    result.usage.prompt_tokens += cr.usage.prompt_tokens;
                                    result.usage.completion_tokens += cr.usage.completion_tokens;
                                    result.usage.cached_tokens += cr.usage.cached_tokens;
                                }

                                // #486: same stamping as the sequential path.
                                let mut int_findings = int_output.findings;
                                if let Some(ref diff_ranges) = pipeline_cfg.diff_ranges {
                                    pipeline::classify_findings_for_file(
                                        &mut int_findings,
                                        std::path::Path::new(&file_str),
                                        diff_ranges,
                                    );
                                }
                                result.findings.extend(int_findings);
                                result.suppressed += int_output.suppressed.len();
                        }

                        let sup_result = suppress::apply_suppressions(
                            result.findings,
                            &suppress_rules,
                            &file_display,
                        );
                        result.findings = sup_result.kept;
                        result.suppressed += sup_result.suppressed.len();
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
        for (result, suppressed_findings) in indexed_results.into_iter().flatten() {
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
        // Name the model. A stale `QUORUM_MODEL` export silently downgraded a
        // real review and the only way to notice was grepping a shell config:
        // nothing in the output said which model ran. PR comments have always
        // carried it; the CLI did not.
        let model_label = if pipeline_cfg.models.len() > 1 {
            pipeline_cfg.models.join(",")
        } else {
            pipeline_cfg
                .models
                .first()
                .cloned()
                .unwrap_or_else(|| "none".to_string())
        };
        let withheld: u32 = file_results
            .iter()
            .map(|r| r.judge_metrics.withheld_unjudged)
            .sum();
        eprintln!(
            "Reviewed {} file(s) in {:.1}s using {}: {} finding(s){}{}",
            file_results.len(),
            review_duration.as_secs_f64(),
            model_label,
            total_findings,
            if total_suppressed > 0 {
                format!(", {} suppressed", total_suppressed)
            } else {
                String::new()
            },
            if withheld > 0 {
                format!(", {withheld} speculative withheld (run with --judge to evaluate them)")
            } else {
                String::new()
            }
        );
    }

    // Record telemetry (best-effort, don't fail the review). Reuses the
    // outer-scope `qhome` resolved via quorum_dir() so review/telemetry/
    // reviews_jsonl all live in the same dir.
    {
        let telem_store = telemetry::TelemetryStore::with_storage(storage_handle.clone());
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
            context7_skipped_popular: file_results
                .iter()
                .map(|r| r.enrichment_metrics.context7_skipped_popular)
                .sum(),
            context7_budget_reduced: file_results
                .iter()
                .map(|r| r.enrichment_metrics.context7_budget_reduced)
                .sum(),
            // #123 Layer 1 (Task 10): adoption telemetry for the FpKind
            // taxonomy. Computed over the loaded feedback store (same one
            // pipeline_cfg.feedback was built from). None when no FP
            // entries exist — utilization is undefined, not zero.
            fp_kind_utilization_rate: feedback::compute_fp_kind_utilization_rate(
                &pipeline_cfg.feedback,
            ),
            judge_calls: file_results.iter().map(|r| r.judge_metrics.calls).sum(),
            judge_approved: file_results.iter().map(|r| r.judge_metrics.approved).sum(),
            judge_rejected: file_results.iter().map(|r| r.judge_metrics.rejected).sum(),
            judge_uncertain: file_results.iter().map(|r| r.judge_metrics.uncertain).sum(),
            judge_skipped: file_results.iter().map(|r| r.judge_metrics.skipped).sum(),
            judge_cache_hits: file_results
                .iter()
                .map(|r| r.judge_metrics.cache_hits)
                .sum(),
            judge_latency_ms: file_results
                .iter()
                .map(|r| r.judge_metrics.latency_ms)
                .sum(),
        };
        if let Err(e) = telem_store.record(&telem_entry) {
            tracing::warn!("Failed to record telemetry: {e}");
        }

        // Per-review record for dimensional stats (by-repo, by-caller, rolling).
        let review_log = review_log::ReviewLog::with_storage(storage_handle.clone());
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

        let finding_meta: Vec<review_log::FindingMeta> = file_results
            .iter()
            .flat_map(|fr| {
                fr.findings.iter().map(|f| review_log::FindingMeta {
                    id: f.id.clone(),
                    title: f.title.clone(),
                    file_path: fr.file_path.clone(),
                })
            })
            .collect();

        let record = review_log::ReviewRecord {
            run_id: run_id.clone(),
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
            mode: if opts.mode == crate::review_mode::ReviewMode::Code {
                None
            } else {
                Some(opts.mode.as_str().to_string())
            },
            context: context_telem,
            finding_ids: quorum::finding::collect_finding_ids(&all_findings),
            skills_used: Vec::new(),
            skill_findings: None,
            integrator_findings_out: None,
        };
        if let Err(e) = review_log.record_with_meta(&record, &finding_meta) {
            eprintln!("Warning: failed to write review log: {}", e);
        }

        // Read the severity ledger back. `reviews.jsonl` recorded the 0.28.0
        // collapse (1.17 -> 0.014 crit+high per file) for two months and nothing
        // ever queried it, because querying required a suspicion the tool's own
        // "success" output actively discouraged. Checking here costs one bounded
        // read and makes systemic silence loud on the next review rather than
        // whenever someone thinks to look.
        const REGRESSION_LOOKBACK: usize = 500;
        match review_log.load_recent(REGRESSION_LOOKBACK) {
            Ok(recent) => {
                let by_version = dimensions::group_by_version(&recent);
                if let Some(reg) = dimensions::detect_severity_regression(
                    &by_version,
                    dimensions::DEFAULT_REGRESSION_RATIO,
                    dimensions::DEFAULT_REGRESSION_MIN_FILES,
                ) {
                    eprint!("{}", stats::format_severity_regression(&reg));
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "severity-regression check skipped");
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

    if let Some(pr_number) = opts.github_pr {
        let review_exit = output::compute_exit_code(&all_findings);
        let ctx = match github_report::resolve_github_context(
            opts.github_token.as_deref(),
            opts.github_repo.as_deref(),
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "Error: GitHub post failed: {} (review exit code preserved: {})",
                    e, review_exit
                );
                return review_exit;
            }
        };

        let client = match reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "Error: GitHub post failed: cannot initialize HTTP client: {} (review exit code preserved: {})",
                    e, review_exit
                );
                return review_exit;
            }
        };

        let diff_text = if let Some(ref diff_path) = opts.diff_file {
            match std::fs::read_to_string(diff_path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "Error: GitHub post failed: cannot read diff file: {} (review exit code preserved: {})",
                        e, review_exit
                    );
                    return review_exit;
                }
            }
        } else {
            match github_report::fetch_pr_diff(
                &client, &ctx.owner, &ctx.repo, pr_number, &ctx.token, None,
            )
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "Error: GitHub post failed: cannot fetch PR diff: {} (review exit code preserved: {})",
                        e, review_exit
                    );
                    return review_exit;
                }
            }
        };

        let commit_sha = match github_report::fetch_pr_head_sha(
            &client, &ctx.owner, &ctx.repo, pr_number, &ctx.token, None,
        )
        .await
        {
            Ok(sha) => sha,
            Err(e) => {
                eprintln!(
                    "Error: GitHub post failed: cannot fetch PR head SHA: {} (review exit code preserved: {})",
                    e, review_exit
                );
                return review_exit;
            }
        };

        let run_id = ulid::Ulid::new().to_string();
        let version = env!("CARGO_PKG_VERSION").to_string();

        let req = github_report::PostReviewRequest {
            owner: ctx.owner,
            repo: ctx.repo,
            pr_number,
            token: ctx.token,
            findings: all_findings.clone(),
            diff_text,
            version,
            run_id,
            commit_sha,
            api_base_url: None,
        };

        eprint!(
            "Posting {} findings to PR #{}...",
            req.findings.len(),
            pr_number
        );
        match github_report::post_review(&client, &req).await {
            Ok(result) => {
                if let Some(dismissed) = result.dismissed_previous {
                    eprint!(" dismissed review {}...", dismissed);
                }
                eprintln!(
                    " done ({} inline, {} in summary)",
                    result.inline_count, result.body_count
                );
            }
            Err(e) => {
                eprintln!(
                    "\nError: GitHub post failed: {} (review exit code preserved: {})",
                    e, review_exit
                );
            }
        }

        return review_exit;
    }

    // A review whose skill axes failed is not clean, even with no findings:
    // that combination is exactly what hid the parser regression for two
    // months. Floor the status at 1 (warnings) without escalating transient
    // LLM failures to a tool error.
    let code = output::compute_exit_code(&all_findings);
    if skill_cells_failed.load(std::sync::atomic::Ordering::Relaxed) > 0 {
        code.max(1)
    } else {
        code
    }
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
        Golangcilint => exts.contains("go"),
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
    // The daemon holds one long-lived LLM client, so a per-request cache
    // bypass cannot be honoured. Reject rather than accept-and-ignore: a flag
    // that silently does nothing is the failure mode this release exists to
    // remove.
    if opts.no_cache {
        eprintln!(
            "error: --no-cache is not supported with --daemon.\n\
             The daemon shares one LLM client across requests, so cache bypass \
             is a process-level setting.\n\
             Start the daemon with QUORUM_BYPASS_PROXY_CACHE=1, or drop --daemon \
             for a one-off uncached review."
        );
        return 3;
    }

    // The daemon is always local. Bypass ambient system proxies so a proxy
    // cannot turn a healthy loopback daemon into a misleading 4xx/5xx result.
    let client = match reqwest::blocking::Client::builder().no_proxy().build() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Error: Could not create daemon HTTP client: {}", e);
            return 3;
        }
    };
    let base = format!("http://127.0.0.1:{}", opts.daemon_port);

    // Same precedence as the local path: --model wins, else the daemon's own
    // configured model. Sent per request so `--daemon --model X` is honoured
    // rather than silently ignored.
    let daemon_models: Vec<String> = opts
        .model
        .iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();

    // Check if daemon is running
    match client.get(format!("{}/health", base)).send() {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            eprintln!("Error: Daemon health check returned {}", resp.status());
            eprintln!(
                "Error: Daemon is running on port {} but reported an unhealthy status.",
                opts.daemon_port
            );
            return 3;
        }
        Err(e) => {
            eprintln!("Error: Could not reach daemon health endpoint: {}", e);
            eprintln!(
                "Error: Daemon is not reachable on port {}. Start with: quorum daemon",
                opts.daemon_port
            );
            return 3;
        }
    }

    let use_json = opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout());
    let style = output::Style::detect(opts.no_color);
    let mut all_findings = Vec::new();
    let mut had_errors = false;

    for file_path in &opts.files {
        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: Could not read {}: {}", file_path.display(), e);
                had_errors = true;
                continue;
            }
        };

        let body = serde_json::json!({
            "file_path": file_path.to_string_lossy(),
            "code": source,
            "models": daemon_models,
        });

        match client.post(format!("{}/review", base)).json(&body).send() {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<http_server::ReviewResponse>() {
                    Ok(review) => {
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
                    Err(e) => {
                        had_errors = true;
                        eprintln!(
                            "Error: Invalid response from daemon for {}: {}",
                            file_path.display(),
                            e
                        );
                    }
                }
            }
            Ok(resp) => {
                had_errors = true;
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

    let code = output::compute_exit_code(&all_findings);
    if had_errors {
        // A daemon review is incomplete when any input or response failed.
        // Preserve finding severity for completed files, but never report a
        // clean review after a partial failure.
        if all_findings.is_empty() {
            3
        } else {
            code.max(1)
        }
    } else {
        code
    }
}

#[cfg(test)]
mod daemon_review_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    /// What the mock daemon actually did, returned rather than panicked.
    ///
    /// Issue #518. Every operation on a *client* socket here can fail for a
    /// reason that is not a defect: several of these tests deliberately drive
    /// `run_review_via_daemon` down an early-return path, so the client is
    /// *expected* to hang up partway through. When the scaffolding panicked on
    /// that, `server.join().unwrap()` re-raised it and the test failed without
    /// ever reaching its assertion -- a behavioural regression is what it
    /// looked like, and a timing race is what it was. It failed only on Linux,
    /// where the client-close/server-accept ordering loses more often.
    ///
    /// The class is worth naming: test *scaffolding* held to a lower standard
    /// than the test. The better-known form is an assertion that passes
    /// without testing anything; this is the mirror image, scaffolding that
    /// fails without testing anything, and looking for vacuous assertions does
    /// not find it.
    #[derive(Debug, Default)]
    struct ServerLog {
        /// Loop iterations that accepted a connection and ran to the end.
        ///
        /// Distinct from `responses_served` on purpose. An iteration can
        /// complete without a single byte reaching the client -- that is the
        /// normal outcome when the peer has hung up, and it is what the
        /// hung-up-client regression test below deliberately produces.
        iterations_completed: usize,
        /// Responses whose body actually reached the client.
        ///
        /// Counted from `write_response`'s return value rather than
        /// incremented per iteration. An earlier version incremented
        /// unconditionally after a fully best-effort write, so it measured
        /// iterations while reading as deliveries -- which silently weakened
        /// every assertion built on it, including the two added to catch a
        /// client that never reaches the review request.
        responses_served: usize,
        /// Why the server stopped before serving every queued response.
        stopped_early: Option<String>,
    }

    /// Write one response, returning whether the body actually reached the
    /// client.
    ///
    /// Best-effort throughout: a closed peer means the client already gave up,
    /// which for several cases here is the behaviour under test rather than a
    /// failure. The return value is what `ServerLog::responses_served` counts,
    /// so a silently-dropped write cannot be mistaken for a delivered one.
    fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> bool {
        let reason = match status {
            200 => "OK",
            500 => "Internal Server Error",
            _ => "Response",
        };
        let written = write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .is_ok();
        let flushed = stream.flush().is_ok();
        // A failed shutdown does not un-send a body that already went out, so
        // it is not part of the delivery verdict.
        let _ = stream.shutdown(Shutdown::Write);
        written && flushed
    }

    fn daemon_server(responses: Vec<(u16, String)>) -> (u16, JoinHandle<ServerLog>) {
        // Setup failures stay fatal: if the listener cannot be created, the
        // test genuinely cannot run and silence would be worse.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle = thread::spawn(move || {
            let mut log = ServerLog::default();

            for (status, body) in responses {
                let accepted = (0..500).find_map(|_| match listener.accept() {
                    Ok(connection) => Some(Ok(connection)),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        None
                    }
                    Err(error) => Some(Err(error)),
                });

                let (mut stream, _) = match accepted {
                    Some(Ok(connection)) => connection,
                    Some(Err(error)) => {
                        log.stopped_early = Some(format!("accept failed: {error}"));
                        return log;
                    }
                    None => {
                        log.stopped_early = Some("client made no further requests".to_owned());
                        return log;
                    }
                };

                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                if write_response(&mut stream, status, &body) {
                    log.responses_served += 1;
                }
                log.iterations_completed += 1;
            }

            log
        });

        (port, handle)
    }

    /// Join the mock server, failing only on a genuine panic in its thread.
    ///
    /// Distinct from the old `server.join().unwrap()` in what it *cannot* do:
    /// a client that hung up early no longer reaches this as a panic, so a
    /// failure here means the scaffolding itself broke, not the client.
    fn join_server(server: JoinHandle<ServerLog>) -> ServerLog {
        server.join().expect("mock daemon thread panicked")
    }

    fn review_opts(port: u16, files: &[PathBuf]) -> cli::ReviewOpts {
        let mut args = vec![
            "quorum".to_owned(),
            "--daemon".to_owned(),
            "--json".to_owned(),
            "--daemon-port".to_owned(),
            port.to_string(),
        ];
        args.extend(files.iter().map(|path| path.to_string_lossy().into_owned()));
        cli::ReviewOpts::try_parse_from(args).unwrap()
    }

    fn valid_response(findings: &[quorum::finding::Finding]) -> String {
        serde_json::json!({
            "findings": findings,
            "cache_hit": false,
        })
        .to_string()
    }

    /// The scaffolding regression test for #518.
    ///
    /// The race that caused the original Linux-only failure is not
    /// reproducible on demand, but the *class* is: a client that hangs up
    /// before the server finishes with the socket. Every write the mock makes
    /// then fails, and the old code unwrapped them, so the panic surfaced
    /// through `server.join().unwrap()` as a failure of whichever test
    /// happened to be running -- looking like a behavioural regression rather
    /// than a scaffolding race.
    ///
    /// This pins the invariant directly: a hung-up client is a supported
    /// state, not a test failure.
    ///
    /// The oversized body is what makes it deterministic rather than
    /// timing-dependent. A small response to a dropped peer is simply buffered
    /// by the kernel and the write succeeds, so the first formulation of this
    /// test passed against the buggy code -- it reproduced the situation but
    /// not the failure. Writing more than the socket buffer can hold forces
    /// the write to observe the dead peer. Verified in both directions:
    /// restoring the `.unwrap()` calls in `write_response` makes this fail
    /// with the same panic-in-server-thread shape seen in CI.
    #[test]
    fn a_client_that_hangs_up_does_not_panic_the_mock_server() {
        // Far larger than any socket buffer, so the server's write cannot
        // complete into the kernel and must observe the dead peer.
        let big = "x".repeat(8 * 1024 * 1024);
        let (port, server) = daemon_server(vec![(200, big)]);

        drop(TcpStream::connect(("127.0.0.1", port)).expect("connect to mock daemon"));

        // The assertion is that joining does not panic.
        let log = join_server(server);
        assert_eq!(
            log.iterations_completed, 1,
            "the server should complete its iteration against a dead peer \
             rather than abort; stopped_early={:?}",
            log.stopped_early
        );
        // And it must not claim delivery. The oversized body is chosen so the
        // write cannot complete -- if this is ever 1, either the premise of
        // this test has quietly stopped holding or the counter is lying again.
        assert_eq!(
            log.responses_served, 0,
            "an 8 MB body to a dropped peer cannot have been delivered"
        );
    }

    #[test]
    fn unreadable_file_is_a_tool_error() {
        let (port, server) = daemon_server(vec![(200, "ok".to_owned())]);
        let missing = Path::new("this-file-does-not-exist-for-quorum-493.rs").to_owned();
        let result = run_review_via_daemon(&review_opts(port, &[missing]));
        let _log = join_server(server);
        assert_eq!(result, 3);
    }

    #[test]
    fn daemon_http_error_is_a_tool_error() {
        let (port, server) = daemon_server(vec![
            (200, "ok".to_owned()),
            (500, "daemon failed".to_owned()),
        ]);
        let file = tempfile::NamedTempFile::new().unwrap();
        let result = run_review_via_daemon(&review_opts(port, &[file.path().to_owned()]));
        let log = join_server(server);
        assert_eq!(result, 3);
        // The client must actually reach the review request, not just the
        // health check -- otherwise this passes for the wrong reason. The old
        // scaffolding turned this into a 5s timeout and a confusing panic;
        // now it is an assertion that says what it means.
        assert_eq!(
            log.responses_served, 2,
            "expected health check + review request; stopped_early={:?}",
            log.stopped_early
        );
    }

    #[test]
    fn invalid_success_response_is_a_tool_error() {
        let (port, server) = daemon_server(vec![
            (200, "ok".to_owned()),
            (200, "not valid review json".to_owned()),
        ]);
        let file = tempfile::NamedTempFile::new().unwrap();
        let result = run_review_via_daemon(&review_opts(port, &[file.path().to_owned()]));
        let log = join_server(server);
        assert_eq!(result, 3);
        assert_eq!(
            log.responses_served, 2,
            "expected health check + review request; stopped_early={:?}",
            log.stopped_early
        );
    }

    #[test]
    fn partial_daemon_failure_is_never_reported_clean() {
        let finding = quorum::finding::FindingBuilder::new()
            .severity(quorum::finding::Severity::Info)
            .build();
        let (port, server) = daemon_server(vec![
            (200, "ok".to_owned()),
            (200, valid_response(&[finding])),
            (200, "not valid review json".to_owned()),
        ]);
        let first = tempfile::NamedTempFile::new().unwrap();
        let second = tempfile::NamedTempFile::new().unwrap();
        let result = run_review_via_daemon(&review_opts(
            port,
            &[first.path().to_owned(), second.path().to_owned()],
        ));
        let _log = join_server(server);
        assert_eq!(result, 1);
    }
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
    in_diff: Option<bool>,
    provenance: Option<feedback::Provenance>,
    json: bool,
    feedback_path: &std::path::Path,
    finding_id_override: Option<String>,
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

    // Auto-resolve finding_id from the review log when not explicitly provided.
    let finding_id = finding_id_override.or_else(|| {
        let quorum_home = quorum_dir()?;
        let handle = crate::storage::initialize(&quorum_home).ok()?;
        let log = review_log::ReviewLog::with_storage(handle);
        log.resolve_finding_id(file, finding)
    });

    let entry = feedback::FeedbackEntry {
        file_path: file.to_string(),
        finding_title: finding.to_string(),
        // Mirror record_external/MCP normalization: trim and treat blank as
        // missing so analytics buckets don't fragment by ingestion path.
        // #499: blank, never "manual". A placeholder that parses as a real
        // category is worse than an absent one -- the precedent matcher can
        // skip a blank, but it cannot tell a laundered default from a real
        // Maintainability verdict.
        finding_category: category
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_default()
            .to_string(),
        verdict: verdict.clone(),
        reason: reason.to_string(),
        model: model.map(|s| s.to_string()),
        timestamp: chrono::Utc::now(),
        provenance: provenance.unwrap_or(feedback::Provenance::Human),
        fp_kind,
        finding_id,
        rule_id: None,
        in_diff,
        skill_name: None,
        skill_version: None,
        manifest_sha256: None,
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

    let linked = entry.finding_id.as_deref();
    let linked_suffix = linked
        .map(|fid| format!("|linked:{fid}"))
        .unwrap_or_default();
    let output = if use_json {
        let mut json_obj = serde_json::json!({
            "verdict": verdict_label,
            "file_path": entry.file_path,
            "finding_title": entry.finding_title,
            "total": total,
        });
        if let Some(fid) = linked {
            json_obj["linked"] = serde_json::json!(fid);
        }
        serde_json::to_string(&json_obj).unwrap_or_default()
    } else if use_compact {
        format!(
            "feedback:{}|{}|{}{}",
            verdict_label, entry.file_path, entry.finding_title, linked_suffix
        )
    } else {
        let link_info = linked
            .map(|fid| format!(", linked: {fid}"))
            .unwrap_or_default();
        format!(
            "Recorded: {} for \"{}\" in {} ({} entries{})",
            verdict_label, entry.finding_title, entry.file_path, total, link_info,
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
        // Explicit --finding-id bypasses auto-resolve; otherwise fall back
        // to auto-resolution from the review log.
        let finding_id = opts.finding_id.clone().or_else(|| {
            let quorum_home = quorum_dir()?;
            let handle = crate::storage::initialize(&quorum_home).ok()?;
            let log = review_log::ReviewLog::with_storage(handle);
            log.resolve_finding_id(&opts.file, &opts.finding)
        });
        let input = feedback::ExternalVerdictInput {
            file_path: opts.file.clone(),
            finding_title: opts.finding.clone(),
            finding_category: opts.category.clone(),
            verdict,
            reason: opts.reason.clone(),
            agent: agent.to_string(),
            agent_model: opts.agent_model.clone(),
            confidence: opts.confidence,
            in_diff: opts.in_diff,
            finding_id: finding_id.clone(),
        };
        if let Some(parent) = feedback_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!("Failed to create feedback directory: {e}");
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
                    let mut json_obj = serde_json::json!({
                        "verdict": verdict_label,
                        "file_path": opts.file,
                        "finding_title": opts.finding,
                        "agent": agent,
                        "provenance": "external",
                        "total": total,
                    });
                    if let Some(ref fid) = finding_id {
                        json_obj["linked"] = serde_json::json!(fid);
                    }
                    println!("{}", serde_json::to_string(&json_obj).unwrap_or_default());
                } else if use_compact {
                    let linked_suffix = finding_id
                        .as_ref()
                        .map(|fid| format!("|linked:{fid}"))
                        .unwrap_or_default();
                    println!(
                        "feedback:{}|{}|{}|external:{}{}",
                        verdict_label, opts.file, opts.finding, agent, linked_suffix
                    );
                } else {
                    let link_info = finding_id
                        .as_ref()
                        .map(|fid| format!(", linked: {fid}"))
                        .unwrap_or_default();
                    println!(
                        "Recorded external verdict from agent {} ({} entries{}).",
                        agent, total, link_info
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
        if let Some(parent) = feedback_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!("Failed to create feedback directory: {e}");
        }
        // Derive fp_kind from CLI flags. Errors only when verdict=fp and
        // a kind was specified that requires associated data (e.g.
        // compensating-control needs --fp-reference). Returns None silently
        // when verdict != fp; warn the user so the dropped flag is visible.
        let fp_kind = match opts.to_fp_kind() {
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
            opts.in_diff,
            opts.provenance,
            opts.json,
            &feedback_path,
            opts.finding_id.clone(),
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

// ── backfill-linkage ──────────────────────────────────────────────────

/// Re-run `resolve_finding_id` on every unlinked feedback entry and
/// atomically rewrite `feedback.jsonl`. Returns `(newly_linked, candidates)`
/// where `candidates = total - already_linked`.
fn backfill_linkage_inner(quorum_home: &std::path::Path) -> (usize, usize) {
    use fs2::FileExt;

    let feedback_path = quorum_home.join("feedback.jsonl");

    let conn = match quorum::storage::initialize(quorum_home) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: cannot open review storage: {e}");
            return (0, 0);
        }
    };
    let log = review_log::ReviewLog::with_storage(conn);

    // Hold an exclusive lock for the entire read-resolve-write cycle so
    // concurrent `FeedbackStore::record()` appends cannot be lost (#452).
    // Use try_lock to keep this opportunistic — skip if another process holds it.
    let mut lock_file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&feedback_path)
    {
        Ok(f) => f,
        Err(_) => return (0, 0),
    };
    if lock_file.try_lock_exclusive().is_err() {
        return (0, 0);
    }

    // Read directly under our lock — FeedbackStore::load_all() would try to
    // acquire its own shared lock on the same file, deadlocking on macOS (#452).
    let mut content = String::new();
    if std::io::Read::read_to_string(&mut lock_file, &mut content).is_err() {
        let _ = lock_file.unlock();
        return (0, 0);
    }
    let mut entries: Vec<feedback::FeedbackEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let already_linked = entries.iter().filter(|e| e.finding_id.is_some()).count();
    let candidates = entries.len() - already_linked;
    let mut newly_linked = 0usize;

    for entry in &mut entries {
        if entry.finding_id.is_some() {
            continue;
        }
        if let Some(fid) = log.resolve_finding_id(&entry.file_path, &entry.finding_title) {
            entry.finding_id = Some(fid);
            newly_linked += 1;
        }
    }

    if newly_linked > 0 {
        let tmp_path = feedback_path.with_extension("jsonl.tmp");
        let mut buf = String::new();
        for entry in &entries {
            match serde_json::to_string(entry) {
                Ok(line) => {
                    buf.push_str(&line);
                    buf.push('\n');
                }
                Err(e) => {
                    eprintln!("error: failed to serialize feedback entry: {e}");
                    let _ = lock_file.unlock();
                    return (0, candidates);
                }
            }
        }
        if let Err(e) = std::fs::write(&tmp_path, &buf) {
            eprintln!("error: failed to write {}: {e}", tmp_path.display());
            let _ = lock_file.unlock();
            return (0, candidates);
        }
        if let Err(e) = std::fs::rename(&tmp_path, &feedback_path) {
            eprintln!("error: failed to rename tmp to feedback.jsonl: {e}");
            let _ = lock_file.unlock();
            return (0, candidates);
        }
    }

    let _ = lock_file.unlock();
    (newly_linked, candidates)
}

/// CLI entry point for `quorum backfill-linkage`.
fn run_backfill_linkage(opts: cli::BackfillLinkageOpts) -> i32 {
    let quorum_home = quorum_dir().unwrap_or_else(|| std::path::PathBuf::from(".quorum"));

    // Pre-load counts for the summary.
    let feedback_path = quorum_home.join("feedback.jsonl");
    let store = feedback::FeedbackStore::new(feedback_path);
    let all_entries = match store.load_all() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: cannot read feedback: {e}");
            return 3;
        }
    };
    let total = all_entries.len();
    let already_linked = all_entries
        .iter()
        .filter(|e| e.finding_id.is_some())
        .count();
    drop(all_entries);

    let (newly_linked, candidates) = backfill_linkage_inner(&quorum_home);

    let is_pipe = !std::io::IsTerminal::is_terminal(&std::io::stdout());
    let use_compact = output::should_use_compact(false);
    let use_json = opts.json || (is_pipe && !use_compact);

    if use_json {
        let payload = serde_json::json!({
            "total": total,
            "already_linked": already_linked,
            "candidates": candidates,
            "newly_linked": newly_linked,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else if use_compact {
        println!(
            "backfill total={} already_linked={} candidates={} newly_linked={}",
            total, already_linked, candidates, newly_linked
        );
    } else {
        println!("Backfill linkage");
        println!("  Total feedback entries: {}", total);
        println!("  Already linked:         {}", already_linked);
        println!("  Candidates (unlinked):  {}", candidates);
        println!("  Newly linked:           {}", newly_linked);
    }
    0
}

/// Extract the previously-deployed logistic thresholds `(suppress, boost)` from
/// a loaded calibrator model, if it carries a logistic sub-model.
///
/// Pure and unit-testable. Returns `(None, None)` when the model is absent or
/// has no logistic sub-model. Used by `run_calibrate` to report prior-vs-candidate
/// threshold deltas (Phase 1 observability).
fn prior_thresholds(
    model: Option<&calibrator_model::CalibratorModel>,
) -> (Option<f64>, Option<f64>) {
    match model.and_then(|m| m.logistic_model.as_ref()) {
        Some(l) => (Some(l.suppress_threshold), Some(l.boost_threshold)),
        None => (None, None),
    }
}

/// CLI entry point for `quorum calibrate`.
fn run_calibrate(opts: cli::CalibrateOpts) -> i32 {
    if let Err(e) = opts.validate() {
        eprintln!("error: {e}");
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

    if opts.backfill_paths {
        let mut traces_mut = traces;
        let stats = quorum::calibrate::backfill_file_paths(&mut traces_mut, &feedback);
        eprintln!("\nBackfill results:");
        eprintln!("  already had file_path: {}", stats.already_present);
        eprintln!("  feedback (exact):      {}", stats.feedback_exact);
        eprintln!("  feedback (normalized): {}", stats.feedback_normalized);
        eprintln!("  precedent inferred:    {}", stats.precedent_inferred);
        eprintln!("  ambiguous (skipped):   {}", stats.ambiguous);
        eprintln!("  no match (skipped):    {}", stats.no_match);
        eprintln!("  total backfilled:      {}", stats.total_backfilled);

        if stats.total_backfilled == 0 {
            eprintln!("\nNo traces to backfill.");
            return 0;
        }

        if opts.dry_run {
            eprintln!("\n(dry run -- no files written)");
            return 0;
        }

        // Atomic write: backup original, write new
        let bak_path = traces_path.with_extension("jsonl.bak");
        if let Err(e) = std::fs::copy(&traces_path, &bak_path) {
            eprintln!("error: failed to create backup: {e}");
            return 3;
        }
        eprintln!("Backup: {}", bak_path.display());

        let tmp_path = traces_path.with_file_name(format!(
            "calibrator_traces.{}.{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        let mut out = match std::fs::File::create(&tmp_path) {
            Ok(f) => std::io::BufWriter::new(f),
            Err(e) => {
                eprintln!("error: failed to create temp file: {e}");
                return 3;
            }
        };
        use std::io::Write;
        for t in &traces_mut {
            if let Err(e) = writeln!(out, "{}", serde_json::to_string(t).unwrap()) {
                eprintln!("error: write failed: {e}");
                return 3;
            }
        }
        drop(out);
        if let Err(e) = std::fs::rename(&tmp_path, &traces_path) {
            eprintln!("error: rename failed: {e}");
            return 3;
        }
        eprintln!("Wrote {}", traces_path.display());
        return 0;
    }

    let filter = quorum::calibrate::JoinFilter {
        quorum_version: opts.trace_version.clone(),
        clean_only: opts.clean_only,
        repo: opts.trace_repo.clone(),
        commit_sha: opts.trace_commit.clone(),
        run_id: opts.trace_run_id.clone(),
    };
    let disable_fuzzy = opts.disable_fuzzy
        || std::env::var("QUORUM_DISABLE_FUZZY_MATCHING")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
    let (samples, join_stats) = quorum::calibrate::join_feedback_and_traces_with_options(
        &feedback,
        &traces,
        &filter,
        disable_fuzzy,
    );
    let positives = samples.iter().filter(|(_, l)| *l).count();
    let negatives = samples.len() - positives;

    eprintln!(
        "Joined corpus: {} samples ({} TP/partial, {} FP)",
        samples.len(),
        positives,
        negatives,
    );
    eprintln!("\nJoin strategy breakdown:");
    eprintln!("  exact (raw):        {}", join_stats.exact_raw);
    eprintln!("  exact (normalized): {}", join_stats.exact_normalized);
    eprintln!("  path (normalized):  {}", join_stats.path_normalized);
    eprintln!("  suffix matched:     {}", join_stats.suffix_matched);
    eprintln!("  fuzzy (same-file):  {}", join_stats.fuzzy_same_file);
    eprintln!("  title-only (raw):   {}", join_stats.raw_title_only);
    eprintln!("  title-only (norm):  {}", join_stats.normalized_title_only);
    eprintln!("  ambiguous skipped:  {}", join_stats.ambiguous_skipped);
    eprintln!("  below threshold:    {}", join_stats.below_threshold);
    eprintln!("  unmatched:          {}", join_stats.unmatched);

    // Compute composite model from feedback
    let mut composite_model = quorum::calibrate::compute_calibrator_model(&feedback);

    // Feature importance diagnostics (early exit when --feature-importance)
    if opts.feature_importance {
        if let Some(ref model) = composite_model {
            let joined = quorum::calibrate::extract_joined_samples(
                &feedback,
                &traces,
                model,
                &filter,
                disable_fuzzy,
            );
            if joined.is_empty() {
                eprintln!("No joined samples available for feature importance.");
                return 0;
            }
            let refs: Vec<&quorum::calibrate::JoinedSample> = joined.iter().collect();
            let stats = quorum::calibrate::compute_fold_local_stats(&refs);
            let expanded: Vec<(quorum::calibrate::ExpandedFeatures, bool)> = joined
                .iter()
                .map(|s| quorum::calibrate::extract_single_expanded(s, &stats))
                .collect();
            let n_fp = expanded.iter().filter(|(_, l)| *l).count();
            let n_tp = expanded.len() - n_fp;
            let baseline = n_fp as f64 / expanded.len() as f64;

            eprintln!("Feature importance ({} FP, {} non-FP):", n_fp, n_tp);
            let scores = quorum::calibrate::feature_importance_scores(&expanded);
            let names = quorum::calibrate::ExpandedFeatures::feature_names();
            for (idx, ap) in &scores {
                let lift = ap - baseline;
                let marker = if lift < 0.02 {
                    "  [below threshold]"
                } else {
                    ""
                };
                eprintln!(
                    "  {:35} AP={:.4}  (lift {:+.4}){}",
                    names[*idx], ap, lift, marker
                );
            }
        } else {
            eprintln!(
                "No composite model available (insufficient feedback for feature importance)."
            );
        }
        return 0;
    }

    // Learn weights from data when requested
    if opts.learn_weights
        && let Some(ref mut model) = composite_model
    {
        // Try logistic calibrator first
        let joined = quorum::calibrate::extract_joined_samples(
            &feedback,
            &traces,
            model,
            &filter,
            disable_fuzzy,
        );

        // Load the previously-deployed model so we can report prior thresholds
        // and prior->candidate deltas. NOTE: run_calibrate builds a *fresh*
        // composite_model from feedback (whose logistic_model is None), which is
        // a distinct code path from the review-time load at model_path (~2137).
        // Without this explicit load there is no prior deployed threshold in
        // scope here.
        let prior_model_path = qhome.join("calibrator_model.toml");
        let prior_model = quorum::calibrator_model::CalibratorModel::load_from(
            &prior_model_path.to_string_lossy(),
        );
        let (prior_suppress, prior_boost) = prior_thresholds(prior_model.as_ref());

        match quorum::calibrate::learn_logistic(&joined, 5) {
            Some(result) => {
                eprintln!(
                    "\nLogistic model ({} FP, {} non-FP, 5-fold GroupKFold):",
                    result.n_fp,
                    result.n_samples - result.n_fp
                );
                eprintln!(
                    "  Selected features ({}/{}): {:?}",
                    result.selected_feature_names.len(),
                    quorum::calibrate::ExpandedFeatures::feature_names().len(),
                    result.selected_feature_names
                );
                eprintln!("  AP (OOF):      {:.4}", result.ap_score);
                eprintln!("  AP (baseline): {:.4}", result.baseline_ap);
                eprintln!(
                    "  Lift:          {:+.4}",
                    result.ap_score - result.baseline_ap
                );
                eprintln!(
                    "  FP recall @ 99% TP recall: {:.4}",
                    result.fp_recall_at_99_tp_recall
                );
                // Phase 1 is observability-first: there is NO hold guard yet, so
                // the deployed value is ALWAYS the freshly-computed candidate.
                // The label makes that explicit so nobody assumes a guard is
                // gating deployment. The [prior ...] bracket is omitted on first
                // run (no prior model on disk).
                let suppress_delta = prior_suppress.map(|p| result.suppress_threshold - p);
                let boost_delta = prior_boost.map(|p| result.boost_threshold - p);

                match prior_suppress {
                    Some(p) => eprintln!(
                        "  Suppress threshold: {:.4} deployed (= candidate; no hold guard in Phase 1)  [prior {:.4}, d={:+.4}]",
                        result.suppress_threshold,
                        p,
                        result.suppress_threshold - p
                    ),
                    None => eprintln!(
                        "  Suppress threshold: {:.4} deployed (= candidate; no hold guard in Phase 1)",
                        result.suppress_threshold
                    ),
                }
                match prior_boost {
                    Some(p) => eprintln!(
                        "  Boost threshold:    {:.4} deployed (= candidate; no hold guard in Phase 1)  [prior {:.4}, d={:+.4}]",
                        result.boost_threshold,
                        p,
                        result.boost_threshold - p
                    ),
                    None => eprintln!(
                        "  Boost threshold:    {:.4} deployed (= candidate; no hold guard in Phase 1)",
                        result.boost_threshold
                    ),
                }

                tracing::info!(
                    threshold = "suppress",
                    prior = ?prior_suppress,
                    candidate = result.suppress_threshold,
                    deployed = result.suppress_threshold,
                    delta = ?suppress_delta,
                    "threshold report"
                );
                tracing::info!(
                    threshold = "boost",
                    prior = ?prior_boost,
                    candidate = result.boost_threshold,
                    deployed = result.boost_threshold,
                    delta = ?boost_delta,
                    "threshold report"
                );

                let lm = quorum::calibrator_model::LogisticModel {
                    computed_at: chrono::Utc::now().to_rfc3339(),
                    n_samples: result.n_samples,
                    n_fp: result.n_fp,
                    selected_features: result.selected_feature_names,
                    coefficients: result.coefficients,
                    intercept: result.intercept,
                    feature_means: result.feature_means,
                    feature_stddevs: result.feature_stddevs,
                    suppress_threshold: result.suppress_threshold,
                    boost_threshold: result.boost_threshold,
                    ap_score: result.ap_score,
                    fp_recall_at_99_tp_recall: result.fp_recall_at_99_tp_recall,
                    baseline_ap: result.baseline_ap,
                };
                model.logistic_model = Some(lm);
                eprintln!("  -> Logistic model applied");
            }
            None => {
                eprintln!("\n  Logistic model: insufficient data or no improvement over baseline.");
                eprintln!("  Falling back to grid search...");

                // Fallback: existing grid search
                let features = quorum::calibrate::extract_join_features(
                    &feedback,
                    &traces,
                    model,
                    &filter,
                    disable_fuzzy,
                );
                match quorum::calibrate::learn_weights(&features, 5) {
                    Some(result) => {
                        let mean_cv_auc = if result.fold_aucs.is_empty() {
                            0.0
                        } else {
                            result.fold_aucs.iter().sum::<f64>() / result.fold_aucs.len() as f64
                        };
                        let lift = result.pr_auc - result.baseline_auc;
                        eprintln!("\nWeight learning ({} samples):", features.len());
                        eprintln!(
                            "  score={:.2}  word_lor={:.2}  family_fp_inv={:.2}  language_fp_inv={:.2}",
                            result.weights.score,
                            result.weights.word_lor,
                            result.weights.family_fp_inv,
                            result.weights.language_fp_inv,
                        );
                        eprintln!("  PR-AUC (full):     {:.4}", result.pr_auc);
                        eprintln!("  PR-AUC (baseline): {:.4}", result.baseline_auc);
                        eprintln!("  PR-AUC (5-fold):   {:.4}", mean_cv_auc);
                        eprintln!("  Lift over baseline: {:.4}", lift);
                        eprintln!(
                            "  Fold stability:    {}",
                            if result.stable {
                                "stable (all folds within 20%)"
                            } else {
                                "UNSTABLE (fold weights diverge >20%)"
                            }
                        );
                        let min_lift = 0.005;
                        if lift < min_lift {
                            eprintln!("  -> No improvement over baseline (lift < {min_lift})");
                        } else if result.stable {
                            model.weights = result.weights;
                            model.meta.learned_weights = Some(true);
                            eprintln!("  -> Weights applied to model");
                        } else {
                            eprintln!("  -> Using hardcoded weights (unstable folds)");
                        }
                    }
                    None => {
                        eprintln!(
                            "\nWeight learning: skipped (need >=50 samples, have {})",
                            features.len()
                        );
                    }
                }
            }
        }
    }

    let scoring_samples = if let Some(ref model) = composite_model {
        eprintln!(
            "\nComposite model: {} word_lor entries, {} family rates, {} language rates",
            model.word_lor.len(),
            model.family_fp_rate.len(),
            model.language_fp_rate.len(),
        );
        quorum::calibrate::rescore_samples_with_model(
            &feedback,
            &traces,
            model,
            &filter,
            disable_fuzzy,
        )
    } else {
        eprintln!("\nComposite model: not computed (no eligible feedback)");
        samples.clone()
    };

    let mut config = quorum::calibrate::compute_thresholds(
        &scoring_samples,
        opts.suppress_precision,
        opts.boost_precision,
    );
    if composite_model.is_some() {
        config.composite_model = true;
    }

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

    // Populate rate maps from full corpus stats
    {
        let joined_for_maps = composite_model.as_ref().map(|model| {
            quorum::calibrate::extract_joined_samples(
                &feedback,
                &traces,
                model,
                &filter,
                disable_fuzzy,
            )
        });
        if let (Some(model), Some(joined)) = (&mut composite_model, joined_for_maps)
            && !joined.is_empty()
        {
            let refs: Vec<&quorum::calibrate::JoinedSample> = joined.iter().collect();
            let all_stats = quorum::calibrate::compute_fold_local_stats(&refs);
            quorum::calibrate::store_rate_maps_in_model(model, &all_stats);
            eprintln!(
                "Rate maps: {} categories, {} severities, {} models, {} files",
                model.category_fp_rate_map.as_ref().map_or(0, |m| m.len()),
                model.severity_fp_rate.as_ref().map_or(0, |m| m.len()),
                model.model_fp_rate.as_ref().map_or(0, |m| m.len()),
                model.file_fp_rate.as_ref().map_or(0, |m| m.len()),
            );
        }
    }

    let has_logistic = composite_model
        .as_ref()
        .is_some_and(|m| m.logistic_model.is_some());

    if opts.dry_run {
        eprintln!("\n(dry run -- no file written)");
    } else if config.suppress.is_none() && config.boost.is_none() && !has_logistic {
        eprintln!("\nNo thresholds computed (insufficient data). Existing config preserved.");
    } else {
        if let Err(e) = std::fs::create_dir_all(&qhome) {
            eprintln!("\nerror: failed to create {}: {e}", qhome.display());
            return 3;
        }

        // Write composite model
        if let Some(ref model) = composite_model {
            let model_path = qhome.join("calibrator_model.toml");
            let model_toml = model.to_toml();
            match std::fs::write(&model_path, &model_toml) {
                Ok(()) => {
                    eprintln!("Wrote {}", model_path.display());
                }
                Err(e) => {
                    eprintln!("error: failed to write {}: {}", model_path.display(), e);
                    return 3;
                }
            }
        }

        // Write thresholds
        let toml_path = qhome.join("calibrator_thresholds.toml");
        let toml_str = config.to_toml();
        match std::fs::write(&toml_path, &toml_str) {
            Ok(()) => {
                eprintln!("Wrote {}", toml_path.display());
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

        let calibrator_config = calibrator::CalibratorConfig {
            suppress_threshold: tc.suppress.map(|p| p.threshold),
            boost_threshold: tc.boost.map(|p| p.threshold),
            ..Default::default()
        };

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
            None,
            None,
            false,
            &path,
            None, // finding_id_override
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
            None,
            None,
            false,
            &path,
            None, // finding_id_override
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
            None,
            None,
            false,
            &path,
            None, // finding_id_override
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"provenance\":\"human\""));
    }

    #[test]
    fn feedback_provenance_post_fix() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_inner(
            "src/auth.rs",
            "SQL injection",
            "tp",
            "Fixed in this branch",
            None,
            None,
            None,
            None,
            None,
            Some(feedback::Provenance::PostFix),
            false,
            &path,
            None, // finding_id_override
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"provenance\":\"post_fix\""));
    }

    #[test]
    fn feedback_category_defaults_to_blank_not_manual() {
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
            None,
            None,
            false,
            &path,
            None, // finding_id_override
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        // #499: an omitted --category records blank. "manual" parsed as a
        // real Maintainability category and false-matched every genuine
        // Maintainability precedent; blank correctly matches nothing.
        assert!(contents.contains("\"finding_category\":\"\""));
        assert!(!contents.contains("manual"));
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
            None,
            None,
            false,
            &path,
            None, // finding_id_override
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
            None,
            None,
            false,
            &path,
            None, // finding_id_override
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
            None,
            None,
            false,
            &path,
            None, // finding_id_override
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
            None, // in_diff
            None, // provenance
            false,
            &path,
            None, // finding_id_override
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
            None, // in_diff
            None, // provenance
            false,
            &path,
            None, // finding_id_override
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
            None, // in_diff
            None, // provenance
            false,
            &path,
            None, // finding_id_override
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

#[cfg(test)]
mod join_health_tests {
    use super::*;
    use tempfile::TempDir;

    /// Write `content` (already JSONL-formatted) to `dir/<name>` for the
    /// fixture-driven format_join_health tests. Builders rather than
    /// inline JSON literals so test intent reads top-to-bottom.
    fn write_jsonl(dir: &std::path::Path, name: &str, lines: &[&str]) {
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn join_health_empty_dir_reports_no_feedback() {
        let dir = TempDir::new().unwrap();
        let out = format_join_health(dir.path());
        assert!(out.contains("Linkage health"));
        assert!(out.contains("Reviews: 0"));
        assert!(out.contains("Feedback: 0"));
        assert!(out.contains("(no feedback entries)"));
    }

    #[test]
    fn join_health_below_85_percent_emits_fallback_banner() {
        let dir = TempDir::new().unwrap();
        // 1 review with 1 finding_id; 2 feedback entries (1 linked, 1 legacy).
        // Linkage = 50% → below threshold.
        write_jsonl(
            dir.path(),
            "reviews.jsonl",
            &[
                r#"{"run_id":"R1","timestamp":"2026-01-01T00:00:00Z","quorum_version":"0.1","repo":null,"invoked_from":"tty","model":"gpt","files_reviewed":1,"lines_added":null,"lines_removed":null,"findings_by_severity":{"critical":0,"high":0,"medium":0,"low":0,"info":0},"tokens_in":0,"tokens_out":0,"duration_ms":0,"finding_ids":["FID-LINKED"]}"#,
            ],
        );
        write_jsonl(
            dir.path(),
            "feedback.jsonl",
            &[
                r#"{"file_path":"x.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":"human","finding_id":"FID-LINKED"}"#,
                r#"{"file_path":"y.rs","finding_title":"t","finding_category":"c","verdict":"fp","reason":"r","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":"human"}"#,
            ],
        );

        let out = format_join_health(dir.path());
        assert!(out.contains("Reviews: 1 with 1 findings"), "got:\n{out}");
        assert!(
            out.contains("Feedback: 2 entries (1 linked, 1 unlinked legacy)"),
            "got:\n{out}"
        );
        assert!(out.contains("50%"), "rate must show as 50%: got:\n{out}");
        assert!(
            out.contains("below 85% threshold"),
            "fallback banner missing: got:\n{out}"
        );
    }

    #[test]
    fn join_health_surfaces_review_log_read_error_instead_of_silent_zeros() {
        // Regression: ensure format_join_health surfaces storage errors
        // rather than silently reporting "0 reviews". Write corrupt data
        // to quorum.db and block recovery by placing a directory at the
        // backup path so rename fails and the corrupt file persists.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("quorum.db"), b"not a database").unwrap();
        std::fs::create_dir(dir.path().join("quorum.db.corrupt")).unwrap();

        let out = format_join_health(dir.path());
        assert!(
            out.to_lowercase().contains("error"),
            "must surface read failure; got:\n{out}"
        );
        assert!(
            !out.contains("Reviews: 0 with"),
            "must not falsely report empty dataset; got:\n{out}"
        );
    }

    #[test]
    fn join_health_rate_below_85_that_rounds_to_85_still_shows_fallback() {
        // Regression: rate-then-round comparison gives a false pass at the
        // 85% gate. 169 linked + 31 unlinked = 200 entries → exactly 84.5%
        // linkage. Rust's f64::round is round-half-away-from-zero, so
        // (84.5).round() == 85.0. The threshold check must compare the
        // unrounded rate, not the rendered integer percent.
        let dir = TempDir::new().unwrap();
        let ids: Vec<String> = (0..169).map(|i| format!("\"FID-{i}\"")).collect();
        let id_array = format!("[{}]", ids.join(","));
        write_jsonl(
            dir.path(),
            "reviews.jsonl",
            &[&format!(
                r#"{{"run_id":"R1","timestamp":"2026-01-01T00:00:00Z","quorum_version":"0.1","repo":null,"invoked_from":"tty","model":"gpt","files_reviewed":1,"lines_added":null,"lines_removed":null,"findings_by_severity":{{"critical":0,"high":0,"medium":0,"low":0,"info":0}},"tokens_in":0,"tokens_out":0,"duration_ms":0,"finding_ids":{}}}"#,
                id_array
            )],
        );
        let mut fb_lines: Vec<String> = (0..169).map(|i| {
            format!(r#"{{"file_path":"x.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":"human","finding_id":"FID-{}"}}"#, i)
        }).collect();
        for _ in 0..31 {
            fb_lines.push(r#"{"file_path":"y.rs","finding_title":"t","finding_category":"c","verdict":"fp","reason":"r","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":"human"}"#.to_string());
        }
        let fb_refs: Vec<&str> = fb_lines.iter().map(|s| s.as_str()).collect();
        write_jsonl(dir.path(), "feedback.jsonl", &fb_refs);

        let out = format_join_health(dir.path());
        assert!(
            out.contains("below 85% threshold"),
            "84.5% must trigger fallback banner; got:\n{out}"
        );
    }

    #[test]
    fn join_health_at_or_above_85_percent_omits_fallback_banner() {
        let dir = TempDir::new().unwrap();
        // 9 linked + 1 legacy = 90% linkage, above threshold.
        let mut review_ids = String::from("[");
        for i in 0..9 {
            if i > 0 {
                review_ids.push(',');
            }
            review_ids.push_str(&format!("\"FID-{}\"", i));
        }
        review_ids.push(']');
        write_jsonl(
            dir.path(),
            "reviews.jsonl",
            &[&format!(
                r#"{{"run_id":"R1","timestamp":"2026-01-01T00:00:00Z","quorum_version":"0.1","repo":null,"invoked_from":"tty","model":"gpt","files_reviewed":1,"lines_added":null,"lines_removed":null,"findings_by_severity":{{"critical":0,"high":0,"medium":0,"low":0,"info":0}},"tokens_in":0,"tokens_out":0,"duration_ms":0,"finding_ids":{}}}"#,
                review_ids
            )],
        );

        let mut fb_lines: Vec<String> = (0..9).map(|i| {
            format!(r#"{{"file_path":"x.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":"human","finding_id":"FID-{}"}}"#, i)
        }).collect();
        fb_lines.push(r#"{"file_path":"y.rs","finding_title":"t","finding_category":"c","verdict":"fp","reason":"r","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":"human"}"#.to_string());
        let fb_refs: Vec<&str> = fb_lines.iter().map(|s| s.as_str()).collect();
        write_jsonl(dir.path(), "feedback.jsonl", &fb_refs);

        let out = format_join_health(dir.path());
        assert!(out.contains("90%"), "rate must show as 90%: got:\n{out}");
        assert!(
            !out.contains("below 85% threshold"),
            "must NOT show fallback banner at 90%: got:\n{out}"
        );
        assert!(
            out.contains("per-finding precision math active"),
            "got:\n{out}"
        );
    }
}

#[cfg(test)]
mod backfill_linkage_tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test environment with a SQLite DB containing a review record
    /// with finding metadata, and a feedback.jsonl with an unlinked entry.
    fn setup_backfill_env() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let qhome = dir.path().to_path_buf();

        // Initialize SQLite storage and record a review with finding metadata.
        let conn = quorum::storage::initialize(&qhome).unwrap();
        let log = review_log::ReviewLog::with_storage(conn);
        let record = review_log::ReviewRecord {
            run_id: review_log::ReviewRecord::new_ulid(),
            timestamp: chrono::Utc::now(),
            quorum_version: "0.1".into(),
            repo: None,
            invoked_from: "test".into(),
            model: "test".into(),
            files_reviewed: 1,
            lines_added: None,
            lines_removed: None,
            findings_by_severity: review_log::SeverityCounts::default(),
            suppressed_by_rule: std::collections::HashMap::new(),
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            duration_ms: 0,
            flags: review_log::Flags {
                deep: false,
                parallel_n: 1,
                ensemble: false,
            },
            mode: None,
            context: review_log::ContextTelemetry::default(),
            finding_ids: vec!["FIND1".into()],
            skills_used: vec![],
            skill_findings: None,
            integrator_findings_out: None,
        };
        let meta = vec![review_log::FindingMeta {
            id: "FIND1".into(),
            title: "SQL injection risk".into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();

        // Write a feedback.jsonl with one unlinked entry that should match.
        let fb_path = qhome.join("feedback.jsonl");
        let fb_line = r#"{"file_path":"src/auth.rs","finding_title":"SQL injection risk","finding_category":"security","verdict":"tp","reason":"fixed","model":null,"timestamp":"2026-01-01T00:00:00Z","provenance":"human"}"#;
        std::fs::write(&fb_path, format!("{fb_line}\n")).unwrap();

        (dir, qhome)
    }

    #[test]
    fn backfill_linkage_links_matching_entries() {
        let (_dir, qhome) = setup_backfill_env();

        let (newly_linked, candidates) = backfill_linkage_inner(&qhome);
        assert_eq!(candidates, 1, "one unlinked entry");
        assert_eq!(newly_linked, 1, "should link the matching entry");

        // Verify the rewritten feedback.jsonl has finding_id populated.
        let store = feedback::FeedbackStore::new(qhome.join("feedback.jsonl"));
        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].finding_id.as_deref(),
            Some("FIND1"),
            "finding_id must be populated after backfill"
        );
    }

    #[test]
    fn backfill_linkage_is_idempotent() {
        let (_dir, qhome) = setup_backfill_env();

        // First run: links the entry.
        let (linked1, cand1) = backfill_linkage_inner(&qhome);
        assert_eq!(linked1, 1);
        assert_eq!(cand1, 1);

        // Second run: entry already linked, 0 newly linked.
        let (linked2, cand2) = backfill_linkage_inner(&qhome);
        assert_eq!(linked2, 0, "second run must link 0 — already done");
        assert_eq!(cand2, 0, "no unlinked candidates remain");
    }
}

#[cfg(test)]
mod prior_thresholds_tests {
    use super::*;
    use quorum::calibrator_model::{CalibratorModel, LogisticModel, ModelMeta, ScoreWeights};
    use std::collections::HashMap;

    fn model_with(logistic: Option<LogisticModel>) -> CalibratorModel {
        CalibratorModel {
            meta: ModelMeta {
                computed_at: "2026-07-03T00:00:00Z".to_string(),
                feedback_count: 10,
                global_fp_rate: 0.3,
                learned_weights: None,
            },
            weights: ScoreWeights {
                score: 0.5,
                word_lor: 1.5,
                family_fp_inv: 1.0,
                language_fp_inv: 0.5,
            },
            logistic_model: logistic,
            word_lor: HashMap::new(),
            family_fp_rate: HashMap::new(),
            language_fp_rate: HashMap::new(),
            category_fp_rate_map: None,
            severity_fp_rate: None,
            model_fp_rate: None,
            file_fp_rate: None,
            file_finding_counts: None,
        }
    }

    fn logistic_with(suppress: f64, boost: f64) -> LogisticModel {
        LogisticModel {
            computed_at: "2026-07-03T00:00:00Z".to_string(),
            n_samples: 300,
            n_fp: 60,
            selected_features: vec!["a".to_string(), "b".to_string()],
            coefficients: vec![0.1, 0.2],
            intercept: 0.0,
            feature_means: vec![0.0, 0.0],
            feature_stddevs: vec![1.0, 1.0],
            suppress_threshold: suppress,
            boost_threshold: boost,
            ap_score: 0.5,
            fp_recall_at_99_tp_recall: 0.2,
            baseline_ap: 0.2,
        }
    }

    #[test]
    fn prior_thresholds_returns_both_when_logistic_present() {
        let model = model_with(Some(logistic_with(0.3170, 0.7400)));
        let (suppress, boost) = prior_thresholds(Some(&model));
        assert_eq!(suppress, Some(0.3170));
        assert_eq!(boost, Some(0.7400));
    }

    #[test]
    fn prior_thresholds_returns_none_when_logistic_absent() {
        let model = model_with(None);
        let (suppress, boost) = prior_thresholds(Some(&model));
        assert_eq!(suppress, None);
        assert_eq!(boost, None);
    }

    #[test]
    fn prior_thresholds_returns_none_when_model_absent() {
        let (suppress, boost) = prior_thresholds(None);
        assert_eq!(suppress, None);
        assert_eq!(boost, None);
    }
}
