/// Review pipeline: parse -> hydrate -> parallel(LLM + local + linters) -> merge -> calibrate -> output
/// Orchestrates all review sources and produces merged, calibrated findings.
use std::path::Path;

use crate::analysis;
use crate::ast_grep;
use crate::calibrator::{self, CalibratorConfig};
use crate::feedback::FeedbackEntry;
use crate::finding::Finding;
use crate::grounding;
use crate::hydration;
use crate::merge;
use crate::parser::{self, Language};
use crate::redact;
use crate::review::{self, ReviewRequest};
use std::path::PathBuf;

/// Log calibrator traces and write them to JSONL for tuning analysis.
fn write_calibrator_traces(
    traces: &[crate::calibrator_trace::CalibratorTraceEntry],
    feedback_store: Option<&PathBuf>,
) {
    for trace in traces {
        tracing::info!(
            finding = %trace.finding_title,
            category = %trace.finding_category,
            tp_weight = trace.tp_weight,
            fp_weight = trace.fp_weight,
            wontfix_weight = trace.wontfix_weight,
            action = ?trace.action,
            precedent_count = trace.matched_precedents.len(),
            "Calibrator decision"
        );
    }
    if let Some(store_path) = feedback_store {
        let trace_path = store_path.with_file_name("calibrator_traces.jsonl");
        // Serialize all in-process trace writes through a single mutex so
        // parallel review tasks can't interleave bytes within a JSONL line.
        // (Cross-process safety still relies on append-write atomicity below
        // PIPE_BUF; trace records are well under that threshold in practice.)
        static TRACE_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = TRACE_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
        {
            Ok(mut file) => {
                use std::io::Write;
                for trace in traces {
                    match serde_json::to_string(trace) {
                        Ok(json) => {
                            if let Err(e) = writeln!(file, "{}", json) {
                                tracing::warn!(
                                    path = %trace_path.display(),
                                    error = %e,
                                    "calibrator trace write failed"
                                );
                            }
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            "calibrator trace serialize failed"
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                path = %trace_path.display(),
                error = %e,
                "calibrator trace file open failed"
            ),
        }
    }
}

/// Acquire a semaphore permit if configured, awaiting cooperatively.
///
/// Returns an owned permit that is released on drop (RAII). When the
/// semaphore is `None`, returns `None` immediately (no throttling).
/// A closed semaphore (`acquire_owned` returns `Err`) also degrades
/// to `None`, mirroring the prior contract for "throttling can't
/// work, don't crash the caller".
///
/// Issue #81: pre-fix this was a sync helper that branched on the
/// current Tokio runtime flavor — `block_in_place` on multi-thread,
/// `std::thread::scope` + a fresh current-thread runtime + `join()`
/// on current-thread. The current-thread branch deadlocked when the
/// permit holder was another task on the *same* runtime: `join()`
/// blocked the only worker, the holder never ran to release, and
/// the spawned helper runtime awaited forever. The async shape
/// eliminates that class of bug by construction — we just `.await`
/// and let the runtime that owns the holder make progress.
async fn acquire_llm_permit(
    sem: &Option<std::sync::Arc<tokio::sync::Semaphore>>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    sem.as_ref()?.clone().acquire_owned().await.ok()
}

/// Trait for LLM review — allows testing with fake implementations.
pub trait LlmReviewer: Send + Sync {
    fn review(&self, prompt: &str, model: &str) -> anyhow::Result<crate::llm_client::LlmResponse>;
}

/// Result of reviewing a single file.
#[derive(Clone)]
pub struct FileReviewResult {
    pub file_path: String,
    pub findings: Vec<Finding>,
    pub usage: crate::llm_client::TokenUsage,
    pub suppressed: usize,
    /// Context-injection telemetry for this file, if an injector was
    /// wired. `None` when the pipeline ran without `context_injector`
    /// (reviewers that don't support context, or the LLM-only paths).
    pub context_telemetry: Option<crate::review_log::ContextTelemetry>,
    /// Per-file Context7 enrichment counters (resolved/resolve_failed/query_failed).
    /// Aggregated into TelemetryEntry by main.rs.
    pub enrichment_metrics: crate::context_enrichment::EnrichmentMetrics,
}

pub struct PipelineConfig {
    pub complexity_threshold: u32,
    pub similarity_threshold: f64,
    pub models: Vec<String>,
    pub feedback: Vec<FeedbackEntry>,
    pub calibrate: bool,
    pub feedback_store: Option<std::path::PathBuf>,
    /// Per-file changed line ranges from a unified diff (overrides full-file hydration)
    pub diff_ranges: Option<Vec<(String, Vec<(u32, u32)>)>>,
    /// Maximum number of lines to send to the LLM for review
    pub max_review_lines: usize,
    /// Framework overrides from CLI --framework flags
    pub framework_overrides: Vec<String>,
    /// Semaphore to limit concurrent LLM calls (None = unlimited)
    pub semaphore: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    /// Pre-built feedback index for calibration (shared across parallel tasks)
    pub feedback_index:
        Option<std::sync::Arc<std::sync::Mutex<crate::feedback_index::FeedbackIndex>>>,
    /// Skip Context7 enrichment (default false: fail if frameworks detected but docs unavailable)
    pub skip_context7: bool,
    /// Skip fastembed model — fall back to Jaccard word-overlap for calibration.
    pub fast: bool,
    /// Optional hook into the `quorum context` retrieve→plan→render pipeline.
    /// When `Some` and the injector returns `Some(markdown)`, the block is
    /// spliced into the LLM prompt. When `None` (the default), behavior is
    /// byte-identical to the pre-context pipeline.
    pub context_injector:
        Option<std::sync::Arc<dyn crate::context::inject::ContextInjectionSource>>,
    /// Shared Context7 fetcher (typically a `CachedContextFetcher` wrapping
    /// a single `Context7HttpFetcher`). Built once at the review-level scope
    /// in main.rs so cache hits / 24h negative-resolve TTL apply across all
    /// files in a multi-file review. When `None`, each file builds its own
    /// ad-hoc fetcher (suitable for tests / single-file CLI invocations).
    pub context7_fetcher: Option<std::sync::Arc<dyn crate::context_enrichment::ContextFetcher>>,
    /// Set by main.rs when the shared `Context7HttpFetcher::new()` returns
    /// Err at review start. Pipeline checks this BEFORE the per-file
    /// fallback so a single bootstrap failure cleanly disables enrichment
    /// instead of re-failing per file (CR8).
    pub context7_disabled: bool,
    /// Per-request focus directive (e.g. "security", "performance"). When
    /// `Some` and non-empty after trim, threaded into `ReviewRequest.focus`
    /// and rendered as a `<focus_areas>` section in the LLM prompt. When
    /// `None` or whitespace-only, behavior is byte-identical to the
    /// pre-#104 prompt layout. Set by `mcp::handler::QuorumHandler` from the
    /// MCP `ReviewTool.focus` field; the CLI does not currently expose it.
    pub focus: Option<String>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            complexity_threshold: 10,
            similarity_threshold: 0.8,
            models: vec![],
            feedback: vec![],
            calibrate: true,
            feedback_store: None,
            diff_ranges: None,
            max_review_lines: 500,
            framework_overrides: Vec::new(),
            semaphore: None,
            feedback_index: None,
            skip_context7: false,
            fast: false,
            context_injector: None,
            context7_fetcher: None,
            context7_disabled: false,
            focus: None,
        }
    }
}

/// Reasons Context7 enrichment may be skipped at the pipeline level.
/// Returns `Some(reason)` when the per-file enrichment block must NOT run.
/// `--skip-context7` wins over `context7_disabled` for log-message clarity.
fn context7_skip_reason(cfg: &PipelineConfig) -> Option<&'static str> {
    if cfg.skip_context7 {
        return Some("--skip-context7");
    }
    if cfg.context7_disabled {
        return Some("init failed");
    }
    None
}

/// Truncate source code for LLM review if it exceeds the line limit.
/// Returns (possibly truncated source, optional truncation notice).
fn truncate_for_review(source: &str, max_lines: usize) -> (String, Option<String>) {
    let max_lines = max_lines.max(1);
    let total_lines = source.lines().count();
    if total_lines <= max_lines {
        return (source.to_string(), None);
    }
    let truncated: String = source
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    let notice = format!("lines 1-{} of {}", max_lines, total_lines);
    (truncated, Some(notice))
}

/// Query feedback index for high-confidence human-verified precedents to inject as few-shot examples.
/// Enforces a TP/FP mix to avoid anchoring the LLM toward only one verdict type.
fn query_feedback_precedents(
    index: &mut crate::feedback_index::FeedbackIndex,
    file_path: &str,
    language: &str,
    code: &str,
) -> Vec<String> {
    use crate::feedback::{Provenance, Verdict};

    // Ablation knob: bypass few-shot precedent injection entirely. Used by
    // the calibrator-eval harness to isolate prompt-side effects from
    // post-hoc calibrator effects. Returns no precedents to inject.
    if std::env::var("QUORUM_DISABLE_FEW_SHOT").is_ok() {
        return Vec::new();
    }

    // Query with language + filename + first 200 chars of code for better semantic matching
    let code_snippet: String = code.chars().take(200).collect();
    let query = format!(
        "{} {} {}",
        language,
        file_path.rsplit('/').next().unwrap_or(file_path),
        code_snippet
    );
    let similar = index.find_similar(&query, "", 15);

    let candidates: Vec<_> = similar
        .iter()
        .filter(|s| s.similarity >= 0.6)
        .filter(|s| matches!(s.entry.provenance, Provenance::Human | Provenance::PostFix))
        .filter(|s| matches!(s.entry.verdict, Verdict::Tp | Verdict::Fp))
        .collect();

    // Enforce TP/FP mix: pick up to 2 TPs and up to 1 FP (or vice versa if available)
    let tps: Vec<_> = candidates
        .iter()
        .filter(|s| s.entry.verdict == Verdict::Tp)
        .take(2)
        .collect();
    let fps: Vec<_> = candidates
        .iter()
        .filter(|s| s.entry.verdict == Verdict::Fp)
        .take(2)
        .collect();

    let mut selected: Vec<_> = Vec::new();
    // Take up to 2 TPs
    selected.extend(tps.iter().take(2));
    // Fill remaining slots with FPs (up to 3 total)
    for fp in &fps {
        if selected.len() >= 3 {
            break;
        }
        selected.push(fp);
    }
    // If we still have room and more TPs, fill
    let remaining_tps: Vec<_> = candidates
        .iter()
        .filter(|s| s.entry.verdict == Verdict::Tp)
        .skip(2)
        .collect();
    for tp in &remaining_tps {
        if selected.len() >= 3 {
            break;
        }
        selected.push(tp);
    }

    tracing::debug!(
        query_prefix = &query[..query.len().min(100)],
        candidates_found = candidates.len(),
        selected_count = selected.len(),
        "Few-shot precedent retrieval"
    );

    selected
        .iter()
        .map(|s| render_precedent_for_few_shot(&s.entry))
        .collect()
}

/// Render a single feedback precedent for inclusion in the LLM few-shot
/// prompt. Pinned marker phrases — change only by updating both code and
/// the contract tests in `mod tests` (#123 Layer 1, Task 6).
///
/// `PatternOvergeneralization { discriminator_hint: Some(_) }` precedents
/// surface their hint with a "When the pattern IS a real bug:" marker so
/// the LLM learns the boundary instead of blanket-suppressing the pattern.
/// Hint is capped at 200 chars (Unicode-codepoint counted) with `…` ellipsis.
pub(crate) fn render_precedent_for_few_shot(entry: &crate::feedback::FeedbackEntry) -> String {
    use crate::feedback::Verdict;

    let verdict_label = match entry.verdict {
        Verdict::Tp => "TRUE POSITIVE",
        Verdict::Fp => "FALSE POSITIVE",
        _ => "NOTED",
    };
    let truncated_reason: String = entry.reason.chars().take(100).collect();

    let mut rendered = format!(
        "[{}] {}: {}",
        verdict_label, entry.finding_title, truncated_reason,
    );

    // PatternOvergeneralization hint — append marker phrase + truncated hint
    // so the LLM can re-flag the pattern when context differs from this FP.
    if let Some(crate::feedback::FpKind::PatternOvergeneralization {
        discriminator_hint: Some(hint),
    }) = &entry.fp_kind
    {
        const HINT_CAP: usize = 200;
        let hint_chars = hint.chars().count();
        if hint_chars <= HINT_CAP {
            rendered.push_str("\nWhen the pattern IS a real bug: ");
            rendered.push_str(hint);
        } else {
            let truncated: String = hint.chars().take(HINT_CAP).collect();
            rendered.push_str("\nWhen the pattern IS a real bug: ");
            rendered.push_str(&truncated);
            rendered.push('…');
        }
    }

    rendered
}

/// Run the full review pipeline on a single file.
pub async fn review_file(
    file_path: &Path,
    source: &str,
    lang: Language,
    tree: &tree_sitter::Tree,
    llm: Option<&dyn LlmReviewer>,
    pipeline_config: &PipelineConfig,
) -> anyhow::Result<FileReviewResult> {
    let file_str = file_path.to_string_lossy().to_string();
    let mut all_sources: Vec<Vec<Finding>> = Vec::new();
    let mut total_usage = crate::llm_client::TokenUsage::default();
    let mut enrichment_metrics = crate::context_enrichment::EnrichmentMetrics::default();
    // Populated inside the LLM branch when a context_injector is wired.
    // Declared here so the final FileReviewResult construction can see it.
    let mut context_telemetry: Option<crate::review_log::ContextTelemetry> = None;

    // Use pre-built shared index if available (parallel mode), otherwise build locally
    let shared_index = pipeline_config.feedback_index.clone();
    let mut local_index = if shared_index.is_none() {
        if let Some(store_path) = &pipeline_config.feedback_store {
            let store = crate::feedback::FeedbackStore::new(store_path.clone());
            // Surface I/O / corrupted-store errors instead of silently
            // proceeding without precedent injection. The embedder-unavailable
            // path is already a soft fall-back inside FeedbackIndex::build.
            Some(if pipeline_config.fast {
                crate::feedback_index::FeedbackIndex::build_bm25(&store)?
            } else {
                crate::feedback_index::FeedbackIndex::build(&store)?
            })
        } else {
            None
        }
    } else {
        None
    };

    // Source 1: Local AST analysis
    let mut local_findings = Vec::new();
    {
        let _span = tracing::info_span!("phase.local_ast", file = %file_str).entered();
        let t0 = std::time::Instant::now();
        local_findings.extend(analysis::analyze_complexity(
            tree,
            source,
            lang,
            pipeline_config.complexity_threshold,
        ));
        local_findings.extend(analysis::analyze_insecure_patterns(tree, source, lang));
        tracing::info!(
            phase = "local_ast",
            duration_ms = t0.elapsed().as_millis() as u64,
            findings = local_findings.len(),
            "phase complete"
        );
    }
    all_sources.push(local_findings);

    // Source 2: ast-grep library rules
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ast_grep::ext_to_language(ext).is_some() {
        let _span = tracing::info_span!("phase.ast_grep", file = %file_str).entered();
        let t0 = std::time::Instant::now();
        let project_root = find_project_root(file_path);
        let home_dir = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let rules = ast_grep::load_rules(&project_root, &home_dir);
        let mut ag_count = 0;
        if !rules.is_empty() {
            let ag_findings = ast_grep::scan_file(source, ext, &rules);
            ag_count = ag_findings.len();
            if !ag_findings.is_empty() {
                all_sources.push(ag_findings);
            }
        }
        tracing::info!(
            phase = "ast_grep",
            duration_ms = t0.elapsed().as_millis() as u64,
            rules = rules.len(),
            findings = ag_count,
            "phase complete"
        );
    }

    // Source 3: LLM review (if configured and models specified)
    if let Some(reviewer) = llm {
        if pipeline_config.models.is_empty() {
            // No models configured — skip LLM review
        } else {
            // Hydrate context: use diff ranges if available, else full file.
            //
            // #137: match on full repo-relative path equality (NOT ends_with),
            // resolved through the project root so `src/foo.rs` does not
            // cross-match `nested/src/foo.rs`.
            let changed_lines: Vec<(u32, u32)> =
                if let Some(ref diff_ranges) = pipeline_config.diff_ranges {
                    // Hoist canonicalization out of the filter loop: review_path and
                    // repo_root are loop-invariant, only diff_path varies. Without
                    // this, large diffs paid 2 canonicalize syscalls per range entry.
                    let repo_root = find_project_root(file_path);
                    let resolver = ReviewPathResolver::new(&file_str, &repo_root);
                    diff_ranges
                        .iter()
                        .filter(|(path, _)| resolver.matches(path))
                        .flat_map(|(_, ranges)| ranges.clone())
                        .collect()
                } else {
                    Vec::new()
                };
            let hydration_ranges = if changed_lines.is_empty() {
                let total_lines = source.lines().count() as u32;
                vec![(1, total_lines.max(1))]
            } else {
                changed_lines
            };
            let hydrate_t0 = std::time::Instant::now();
            let ctx = {
                let _span = tracing::info_span!("phase.hydrate", file = %file_str).entered();
                let c = hydration::hydrate(tree, source, lang, &hydration_ranges);
                tracing::info!(
                    phase = "hydrate",
                    duration_ms = hydrate_t0.elapsed().as_millis() as u64,
                    callees = c.callee_signatures.len(),
                    types = c.type_definitions.len(),
                    imports = c.import_targets.len(),
                    "phase complete"
                );
                c
            };

            // Redact secrets in both code AND hydration context before LLM
            let redacted_code = redact::redact_secrets(source);
            let (review_code, truncation_notice) =
                truncate_for_review(&redacted_code, pipeline_config.max_review_lines);
            let redacted_ctx = crate::hydration::HydrationContext {
                callee_signatures: ctx
                    .callee_signatures
                    .iter()
                    .map(|s| redact::redact_secrets(s))
                    .collect(),
                type_definitions: ctx
                    .type_definitions
                    .iter()
                    .map(|s| redact::redact_secrets(s))
                    .collect(),
                callers: ctx.callers.clone(),
                import_targets: ctx
                    .import_targets
                    .iter()
                    .map(|s| redact::redact_secrets(s))
                    .collect(),
                qualified_names: ctx.qualified_names.clone(),
            };

            // Fetch Context7 framework docs (curated frameworks + dep-based enrichment).
            let framework_docs = if let Some(reason) = context7_skip_reason(pipeline_config) {
                // --skip-context7 OR upstream init failed: no Context7 client
                // constructed, no HTTP calls. Metrics stay at defaults.
                tracing::debug!(reason, "Context7: enrichment skipped");
                None
            } else {
                let project_root = find_project_root(file_path);
                let mut domain = crate::domain::detect_domain(&project_root);
                for fw in &pipeline_config.framework_overrides {
                    if !domain.frameworks.contains(fw) {
                        domain.frameworks.push(fw.clone());
                    }
                }
                // Always run enrichment: dep-based path may produce docs even when no
                // curated framework was detected (e.g. a Rust project with tokio).
                // Prefer a shared review-level cache from PipelineConfig so cache
                // state (positive AND negative resolves with 24h TTL) is shared
                // across all files in this review. Fall back to a per-file ad-hoc
                // cache when no shared one is wired (tests, single-file CLI).
                let ctx7_t0 = std::time::Instant::now();
                let _span = tracing::info_span!("phase.context7", file = %file_str).entered();
                let result = if let Some(shared) = pipeline_config.context7_fetcher.as_ref() {
                    crate::context_enrichment::enrich_for_review_in_project(
                        &project_root,
                        &redacted_ctx.import_targets,
                        &domain.frameworks,
                        shared.as_ref(),
                    )
                } else {
                    let inner = crate::context_enrichment::Context7HttpFetcher::new()?;
                    let cached = crate::context_enrichment::CachedContextFetcher::new(&inner, 32);
                    crate::context_enrichment::enrich_for_review_in_project(
                        &project_root,
                        &redacted_ctx.import_targets,
                        &domain.frameworks,
                        &cached,
                    )
                };
                let docs = result.docs;
                enrichment_metrics = result.metrics;
                tracing::info!(
                    phase = "context7",
                    duration_ms = ctx7_t0.elapsed().as_millis() as u64,
                    frameworks = domain.frameworks.len(),
                    docs = docs.len(),
                    resolved = enrichment_metrics.context7_resolved,
                    resolve_failed = enrichment_metrics.context7_resolve_failed,
                    query_failed = enrichment_metrics.context7_query_failed,
                    "phase complete"
                );
                if !docs.is_empty() {
                    tracing::debug!(docs_injected = docs.len(), "Context7 docs injected");
                    Some(
                        docs.iter()
                            .map(|d| {
                                crate::context_enrichment::format_context_section(&[d.clone()])
                            })
                            .collect(),
                    )
                } else if domain.frameworks.is_empty() {
                    // No curated framework was requested — silently skip if dep path
                    // also produced nothing. Long-tail dep failures are NOT fatal.
                    None
                } else {
                    // skip_context7 is impossible here: the outer if-let returned None
                    // already if it was set. Only the explicit-framework + Context7-down
                    // case reaches this arm.
                    anyhow::bail!(
                        "Context7: failed to fetch docs for frameworks {:?}. \
                     This degrades review quality. Fix the Context7 connection or use --skip-context7 to proceed without framework docs.",
                        domain.frameworks
                    );
                }
            };

            // Query feedback index for few-shot precedents
            let precedents = if let Some(ref shared) = shared_index {
                let mut idx = shared.lock().unwrap();
                query_feedback_precedents(&mut idx, &file_str, lang_name(lang), &redacted_code)
            } else if let Some(ref mut idx) = local_index {
                query_feedback_precedents(idx, &file_str, lang_name(lang), &redacted_code)
            } else {
                Vec::new()
            };

            // Optional `quorum context` injection (retrieve → plan → render).
            // When no injector is wired, this is a no-op and `context_block`
            // stays `None`, preserving byte-identical prompts.
            let context_block = match pipeline_config.context_injector.as_ref() {
                Some(inj) => {
                    let mut identifiers: Vec<String> = redacted_ctx
                        .callee_signatures
                        .iter()
                        .filter_map(|sig| extract_ident_from_signature(sig))
                        .collect();
                    identifiers.extend(redacted_ctx.import_targets.iter().cloned());
                    identifiers.sort();
                    identifiers.dedup();
                    let text_sample: String = redacted_code.chars().take(400).collect();
                    // Structural retrieval keys: bare qualified names of
                    // callees + imports, drawn from AST hydration. Redact for
                    // parity with the rest of the request surface.
                    let structural_names: Vec<String> = {
                        let mut v: Vec<String> = redacted_ctx
                            .qualified_names
                            .iter()
                            .map(|s| redact::redact_secrets(s))
                            .collect();
                        v.sort();
                        v.dedup();
                        v
                    };
                    let req = crate::context::inject::InjectionRequest {
                        file_path: file_str.clone(),
                        language: Some(lang_name(lang).to_string()),
                        identifiers,
                        structural_names,
                        text: text_sample,
                    };
                    let outcome = inj.inject(&req);
                    context_telemetry = Some(outcome.telemetry);
                    outcome.rendered
                }
                None => None,
            };

            let req = ReviewRequest {
                file_path: file_str.clone(),
                language: lang_name(lang).to_string(),
                code: review_code,
                hydration_context: Some(redacted_ctx),
                framework_docs,
                feedback_precedents: if precedents.is_empty() {
                    None
                } else {
                    Some(precedents)
                },
                context_block,
                truncation_notice: truncation_notice.clone(),
                focus: pipeline_config.focus.clone(),
            };

            let prompt = review::build_review_prompt(&req);

            for model in &pipeline_config.models {
                let t0 = std::time::Instant::now();
                // EnteredSpan is !Send, so it must not cross an `.await`.
                // The span only scopes the tracing events inside the match
                // arms below — `acquire_llm_permit` emits no events itself.
                let _permit = acquire_llm_permit(&pipeline_config.semaphore).await;
                let _span = tracing::info_span!("phase.llm_call", model = %model, file = %file_str)
                    .entered();
                match reviewer.review(&prompt, model) {
                    Ok(resp) => {
                        let (prompt_tok, completion_tok, cached_tok) = resp
                            .usage
                            .as_ref()
                            .map(|u| (u.prompt_tokens, u.completion_tokens, u.cached_tokens))
                            .unwrap_or((0, 0, 0));
                        if let Some(u) = &resp.usage {
                            total_usage.prompt_tokens += u.prompt_tokens;
                            total_usage.completion_tokens += u.completion_tokens;
                            total_usage.cached_tokens += u.cached_tokens;
                        }
                        match review::parse_llm_response(&resp.content, model) {
                            Ok(mut findings) => {
                                let n_findings = findings.len();
                                if let Some(ref notice) = truncation_notice {
                                    for f in &mut findings {
                                        if matches!(f.source, crate::finding::Source::Llm(_)) {
                                            f.based_on_excerpt = Some(notice.clone());
                                        }
                                    }
                                }
                                all_sources.push(findings);
                                tracing::info!(phase = "llm_call", model = %model, duration_ms = t0.elapsed().as_millis() as u64, findings = n_findings, prompt_tokens = prompt_tok, completion_tokens = completion_tok, cached_tokens = cached_tok, "phase complete");
                            }
                            Err(e) => {
                                tracing::warn!(phase = "llm_call", model = %model, error = %e, "failed to parse response");
                                eprintln!("Warning: Failed to parse {} response: {}", model, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(phase = "llm_call", model = %model, duration_ms = t0.elapsed().as_millis() as u64, error = %e, "review call failed");
                        eprintln!("Warning: {} review failed: {}", model, e);
                    }
                }
            }
        } // end if models not empty
    }

    // Merge all sources
    let merge_t0 = std::time::Instant::now();
    let merged = {
        let _span = tracing::info_span!("phase.merge", file = %file_str).entered();
        let result = merge::merge_findings(all_sources, pipeline_config.similarity_threshold);
        tracing::info!(
            phase = "merge",
            duration_ms = merge_t0.elapsed().as_millis() as u64,
            merged_findings = result.len(),
            "phase complete"
        );
        result
    };

    // Grounding: verify LLM-cited symbols exist in source
    let grounding_disabled = std::env::var("QUORUM_DISABLE_AST_GROUNDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let merged = grounding::apply_grounding(merged, source, grounding_disabled);
    {
        let gc = grounding::count_grounding_outcomes(&merged);
        tracing::info!(
            phase = "grounding",
            verified = gc.verified,
            symbol_not_found = gc.symbol_not_found,
            line_out_of_range = gc.line_out_of_range,
            not_checked = gc.not_checked,
            "grounding pass complete"
        );
    }

    // Calibrate using feedback precedent (prefer FeedbackIndex for semantic matching)
    let has_feedback =
        !pipeline_config.feedback.is_empty() || pipeline_config.feedback_store.is_some();
    let (final_findings, suppressed_count) = if pipeline_config.calibrate && has_feedback {
        let _span = tracing::info_span!("phase.calibrate", file = %file_str).entered();
        let cal_t0 = std::time::Instant::now();
        let config = CalibratorConfig::default();

        // Use shared FeedbackIndex (parallel mode) or local index for calibration
        let cal_result = if let Some(ref shared) = shared_index {
            let mut idx = shared.lock().unwrap();
            if !idx.is_empty() {
                calibrator::calibrate_with_index(merged, &mut idx, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else if let Some(ref mut index) = local_index {
            if !index.is_empty() {
                calibrator::calibrate_with_index(merged, index, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else {
            calibrator::calibrate(merged, &pipeline_config.feedback, &config)
        };

        if cal_result.suppressed > 0 || cal_result.boosted > 0 {
            tracing::debug!(
                suppressed = cal_result.suppressed,
                boosted = cal_result.boosted,
                feedback_entries = pipeline_config.feedback.len(),
                "calibrator decision per-file",
            );
        }

        write_calibrator_traces(&cal_result.traces, pipeline_config.feedback_store.as_ref());
        tracing::info!(
            phase = "calibrate",
            duration_ms = cal_t0.elapsed().as_millis() as u64,
            suppressed = cal_result.suppressed,
            boosted = cal_result.boosted,
            final_findings = cal_result.findings.len(),
            "phase complete"
        );

        (cal_result.findings, cal_result.suppressed)
    } else {
        (merged, 0)
    };

    Ok(FileReviewResult {
        file_path: file_str,
        findings: final_findings,
        usage: total_usage,
        suppressed: suppressed_count,
        context_telemetry,
        enrichment_metrics,
    })
}

/// Best-effort extraction of an identifier from a hydrated callee signature
/// string like `fn verify_token(s: &str) -> bool` or
/// `def verify_token(s: str) -> bool`. Returns `None` for shapes we don't
/// recognize; the retriever treats identifiers as optional hints.
fn extract_ident_from_signature(sig: &str) -> Option<String> {
    // Strip leading `pub `/`async `/`fn `/`def ` tokens.
    let cleaned = sig
        .trim_start_matches("pub ")
        .trim_start_matches("async ")
        .trim();
    let cleaned = cleaned
        .strip_prefix("fn ")
        .or_else(|| cleaned.strip_prefix("def "))
        .or_else(|| cleaned.strip_prefix("function "))
        .unwrap_or(cleaned);
    let ident: String = cleaned
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// Walk up from file path to find the project root (directory containing pyproject.toml, package.json, Cargo.toml, etc.)
/// Pre-computed repo-relative form of the review path, so the diff_ranges
/// filter can do O(1) component-equality per iteration instead of two
/// `canonicalize` syscalls per iteration. (#137 perf — both `repo_root` and
/// `review_path` are loop-invariants.)
///
/// `None` means "no legitimate repo-relative form exists" — e.g. the review
/// path resolves outside `repo_root`. In that case every diff_path comparison
/// must return `false`; wrong context is worse than no context.
pub(crate) struct ReviewPathResolver {
    /// Repo-relative path (canonicalized on both sides), or None if unresolvable.
    review_rel: Option<std::path::PathBuf>,
}
impl ReviewPathResolver {
    pub(crate) fn new(review_path: &str, repo_root: &Path) -> Self {
        let review = Path::new(review_path);
        let review_abs: std::path::PathBuf = if review.is_absolute() {
            review.to_path_buf()
        } else {
            repo_root.join(review)
        };
        let review_canon = std::fs::canonicalize(&review_abs).unwrap_or(review_abs);
        let root_canon =
            std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
        let review_rel = review_canon
            .strip_prefix(&root_canon)
            .ok()
            .map(|p| p.to_path_buf());
        Self { review_rel }
    }

    /// True iff `diff_path` (always repo-relative, as produced by the diff
    /// parser) refers to the same file as the resolver's review path.
    pub(crate) fn matches(&self, diff_path: &str) -> bool {
        let Some(ref review_rel) = self.review_rel else {
            return false;
        };
        Path::new(diff_path)
            .components()
            .eq(review_rel.components())
    }
}

/// True iff `diff_path` (always repo-relative, as produced by the diff parser)
/// refers to the same file as `review_path` (may be absolute or relative as the
/// user supplied it).
///
/// Strategy: normalize `review_path` to repo-relative via `Path::strip_prefix`
/// using the supplied `repo_root`, then test full path equality. If
/// normalization fails — e.g. `review_path` resolves outside `repo_root`, or
/// canonicalization fails on a path-traversal layout — return `false`. Wrong
/// context for the LLM is worse than no context, so refuse the match rather
/// than risk cross-matching siblings (#137).
///
/// NOTE: a naive `ends_with` over component suffixes still cross-matches
/// `src/foo.rs` ↔ `nested/src/foo.rs` because the component sequence
/// `[src, foo.rs]` IS a tail of `[nested, src, foo.rs]`. Full repo-relative
/// equality is required.
///
/// For loops, prefer `ReviewPathResolver::new(...)` once outside the loop and
/// `resolver.matches(diff_path)` inside — this thin wrapper rebuilds the
/// resolver on every call.
pub(crate) fn diff_path_matches(diff_path: &str, review_path: &str, repo_root: &Path) -> bool {
    ReviewPathResolver::new(review_path, repo_root).matches(diff_path)
}

pub fn find_project_root(file_path: &Path) -> std::path::PathBuf {
    let markers = [
        "pyproject.toml",
        "package.json",
        "Cargo.toml",
        "go.mod",
        "setup.py",
        ".terraform.lock.hcl",
        "terraform.tfvars",
    ];
    let mut dir = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    for _ in 0..10 {
        for marker in &markers {
            if dir.join(marker).exists() {
                return dir;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    // Fallback to cwd
    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
}

/// Higher-level entry point: parses source (with optional cache) then runs review_file.
pub async fn review_source(
    file_path: &Path,
    source: &str,
    lang: Language,
    llm: Option<&dyn LlmReviewer>,
    pipeline_config: &PipelineConfig,
    cache: Option<&crate::cache::ParseCache>,
) -> anyhow::Result<FileReviewResult> {
    let tree = if let Some(c) = cache {
        c.get_or_parse(source, lang)?
    } else {
        parser::parse(source, lang)?
    };
    review_file(file_path, source, lang, &tree, llm, pipeline_config).await
}

fn lang_name(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "rust",
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::Tsx => "tsx",
        Language::Yaml => "yaml",
        Language::Bash => "bash",
        Language::Dockerfile => "dockerfile",
        Language::Terraform => "terraform",
    }
}

/// Infer a language name from file extension for LLM-only review.
fn lang_name_from_path(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("unknown")
        .to_lowercase()
}

/// LLM-only review for files without tree-sitter support.
/// Skips local AST analysis but still does LLM review, calibration, and auto-calibration.
pub async fn review_file_llm_only(
    file_path: &Path,
    source: &str,
    llm: Option<&dyn LlmReviewer>,
    pipeline_config: &PipelineConfig,
) -> anyhow::Result<FileReviewResult> {
    let file_str = file_path.to_string_lossy().to_string();
    let mut all_sources: Vec<Vec<Finding>> = Vec::new();
    let mut total_usage = crate::llm_client::TokenUsage::default();
    let mut enrichment_metrics = crate::context_enrichment::EnrichmentMetrics::default();

    // Use pre-built shared index if available (parallel mode), otherwise build locally
    let shared_index = pipeline_config.feedback_index.clone();
    let mut local_index = if shared_index.is_none() {
        if let Some(store_path) = &pipeline_config.feedback_store {
            let store = crate::feedback::FeedbackStore::new(store_path.clone());
            // Surface I/O / corrupted-store errors instead of silently
            // proceeding without precedent injection. The embedder-unavailable
            // path is already a soft fall-back inside FeedbackIndex::build.
            Some(if pipeline_config.fast {
                crate::feedback_index::FeedbackIndex::build_bm25(&store)?
            } else {
                crate::feedback_index::FeedbackIndex::build(&store)?
            })
        } else {
            None
        }
    } else {
        None
    };

    // No local AST analysis — unsupported language

    // LLM review (if configured)
    if let Some(reviewer) = llm {
        if !pipeline_config.models.is_empty() {
            let redacted_code = redact::redact_secrets(source);
            let (review_code, truncation_notice) =
                truncate_for_review(&redacted_code, pipeline_config.max_review_lines);
            let language = lang_name_from_path(file_path);

            // Context7 framework docs (metrics aggregate into the function-scoped var)
            let framework_docs = if pipeline_config.skip_context7 {
                tracing::debug!("Context7: enrichment skipped via --skip-context7");
                None
            } else {
                let project_root = find_project_root(file_path);
                let mut domain = crate::domain::detect_domain(&project_root);
                for fw in &pipeline_config.framework_overrides {
                    if !domain.frameworks.contains(fw) {
                        domain.frameworks.push(fw.clone());
                    }
                }
                let result = if let Some(shared) = pipeline_config.context7_fetcher.as_ref() {
                    crate::context_enrichment::enrich_for_review_in_project(
                        &project_root,
                        &[],
                        &domain.frameworks,
                        shared.as_ref(),
                    )
                } else {
                    let inner = crate::context_enrichment::Context7HttpFetcher::new()?;
                    let cached = crate::context_enrichment::CachedContextFetcher::new(&inner, 32);
                    crate::context_enrichment::enrich_for_review_in_project(
                        &project_root,
                        &[],
                        &domain.frameworks,
                        &cached,
                    )
                };
                let docs = result.docs;
                enrichment_metrics = result.metrics;
                if !docs.is_empty() {
                    Some(
                        docs.iter()
                            .map(|d| {
                                crate::context_enrichment::format_context_section(&[d.clone()])
                            })
                            .collect(),
                    )
                } else if domain.frameworks.is_empty() {
                    None
                } else {
                    // skip_context7 unreachable: outer if-let returned None already.
                    anyhow::bail!(
                        "Context7: failed to fetch docs for frameworks {:?}. \
                         This degrades review quality. Fix the Context7 connection or use --skip-context7 to proceed without framework docs.",
                        domain.frameworks
                    );
                }
            };

            // Query feedback index for few-shot precedents
            let precedents = if let Some(ref shared) = shared_index {
                let mut idx = shared.lock().unwrap();
                query_feedback_precedents(&mut idx, &file_str, &language, &redacted_code)
            } else if let Some(ref mut idx) = local_index {
                query_feedback_precedents(idx, &file_str, &language, &redacted_code)
            } else {
                Vec::new()
            };

            let req = ReviewRequest {
                file_path: file_str.clone(),
                language,
                code: review_code,
                hydration_context: None,
                framework_docs,
                feedback_precedents: if precedents.is_empty() {
                    None
                } else {
                    Some(precedents)
                },
                context_block: None,
                truncation_notice: truncation_notice.clone(),
                focus: pipeline_config.focus.clone(),
            };

            let prompt = review::build_review_prompt(&req);
            for model in &pipeline_config.models {
                let _permit = acquire_llm_permit(&pipeline_config.semaphore).await;
                match reviewer.review(&prompt, model) {
                    Ok(resp) => {
                        if let Some(u) = &resp.usage {
                            total_usage.prompt_tokens += u.prompt_tokens;
                            total_usage.completion_tokens += u.completion_tokens;
                            total_usage.cached_tokens += u.cached_tokens;
                        }
                        match review::parse_llm_response(&resp.content, model) {
                            Ok(mut findings) => {
                                if let Some(ref notice) = truncation_notice {
                                    for f in &mut findings {
                                        if matches!(f.source, crate::finding::Source::Llm(_)) {
                                            f.based_on_excerpt = Some(notice.clone());
                                        }
                                    }
                                }
                                all_sources.push(findings);
                            }
                            Err(e) => {
                                eprintln!("Warning: Failed to parse {} response: {}", model, e)
                            }
                        }
                    }
                    Err(e) => eprintln!("Warning: {} review failed: {}", model, e),
                }
            }
        }
    }

    let merged = merge::merge_findings(all_sources, pipeline_config.similarity_threshold);

    // Grounding: verify LLM-cited symbols exist in source
    let grounding_disabled = std::env::var("QUORUM_DISABLE_AST_GROUNDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let merged = grounding::apply_grounding(merged, source, grounding_disabled);
    {
        let gc = grounding::count_grounding_outcomes(&merged);
        tracing::info!(
            phase = "grounding",
            verified = gc.verified,
            symbol_not_found = gc.symbol_not_found,
            line_out_of_range = gc.line_out_of_range,
            not_checked = gc.not_checked,
            "grounding pass complete"
        );
    }

    // Calibrate
    let has_feedback =
        !pipeline_config.feedback.is_empty() || pipeline_config.feedback_store.is_some();
    let (final_findings, suppressed_count) = if pipeline_config.calibrate && has_feedback {
        let _span = tracing::info_span!("phase.calibrate", file = %file_str).entered();
        let cal_t0 = std::time::Instant::now();
        let config = CalibratorConfig::default();
        // Use shared FeedbackIndex (parallel mode) or local index for calibration
        let cal_result = if let Some(ref shared) = shared_index {
            let mut idx = shared.lock().unwrap();
            if !idx.is_empty() {
                calibrator::calibrate_with_index(merged, &mut idx, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else if let Some(ref mut index) = local_index {
            if !index.is_empty() {
                calibrator::calibrate_with_index(merged, index, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else {
            calibrator::calibrate(merged, &pipeline_config.feedback, &config)
        };
        if cal_result.suppressed > 0 || cal_result.boosted > 0 {
            tracing::debug!(
                suppressed = cal_result.suppressed,
                boosted = cal_result.boosted,
                feedback_entries = pipeline_config.feedback.len(),
                "calibrator decision per-file",
            );
        }

        write_calibrator_traces(&cal_result.traces, pipeline_config.feedback_store.as_ref());
        tracing::info!(
            phase = "calibrate",
            duration_ms = cal_t0.elapsed().as_millis() as u64,
            suppressed = cal_result.suppressed,
            boosted = cal_result.boosted,
            final_findings = cal_result.findings.len(),
            "phase complete"
        );

        (cal_result.findings, cal_result.suppressed)
    } else {
        (merged, 0)
    };

    Ok(FileReviewResult {
        file_path: file_str,
        findings: final_findings,
        usage: total_usage,
        suppressed: suppressed_count,
        context_telemetry: None,
        enrichment_metrics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FeedbackEntry, FpKind, Provenance, Verdict};
    use crate::finding::Source;

    use crate::test_support::fakes::FakeReviewer;

    // ---------------------------------------------------------------------
    // #123 Layer 1 (Task 6) — PatternOvergeneralization discriminator hint
    // surfaces in the few-shot prompt rendering.
    // ---------------------------------------------------------------------

    fn entry_for_few_shot(fp_kind: Option<FpKind>, reason: &str) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "src/foo.rs".into(),
            finding_title: "Unused variable".into(),
            finding_category: "style".into(),
            verdict: Verdict::Fp,
            reason: reason.into(),
            model: None,
            timestamp: chrono::Utc::now(),
            provenance: Provenance::Human,
            fp_kind,
        }
    }

    #[test]
    fn pattern_overgeneralization_renders_discriminator_when_hint_present() {
        let entry = entry_for_few_shot(
            Some(FpKind::PatternOvergeneralization {
                discriminator_hint: Some(
                    "Real bug when var has no _ prefix AND not in #[derive]".into(),
                ),
            }),
            "_var prefix is convention",
        );
        let rendered = render_precedent_for_few_shot(&entry);
        assert!(
            rendered.contains("_var prefix is convention"),
            "reason must appear; got: {}",
            rendered,
        );
        assert!(
            rendered.contains("Real bug when var has no _ prefix"),
            "discriminator hint must appear; got: {}",
            rendered,
        );
        // Marker phrase pinned so a refactor doesn't silently break the
        // LLM-facing contract. Update both code + test together if changed.
        assert!(
            rendered.contains("When the pattern IS a real bug"),
            "marker phrase must wrap the hint; got: {}",
            rendered,
        );
    }

    #[test]
    fn pattern_overgeneralization_omits_marker_when_hint_none() {
        let entry = entry_for_few_shot(
            Some(FpKind::PatternOvergeneralization {
                discriminator_hint: None,
            }),
            "_var prefix is convention",
        );
        let rendered = render_precedent_for_few_shot(&entry);
        assert!(
            rendered.contains("_var prefix is convention"),
            "reason still present"
        );
        assert!(
            !rendered.contains("When the pattern IS a real bug"),
            "marker phrase must be ABSENT when hint=None; got: {}",
            rendered,
        );
    }

    #[test]
    fn pattern_overgeneralization_truncates_long_hint_at_200_chars() {
        let long_hint = "X".repeat(300);
        let entry = entry_for_few_shot(
            Some(FpKind::PatternOvergeneralization {
                discriminator_hint: Some(long_hint),
            }),
            "r",
        );
        let rendered = render_precedent_for_few_shot(&entry);
        assert!(
            rendered.contains(&"X".repeat(200)),
            "200 chars must remain in hint"
        );
        assert!(
            !rendered.contains(&"X".repeat(201)),
            "must truncate above 200 chars; got: {}",
            rendered,
        );
        assert!(
            rendered.contains("…"),
            "ellipsis (…) must mark truncation; got: {}",
            rendered,
        );
    }

    async fn parse_and_review(
        source: &str,
        lang: Language,
        llm: Option<&dyn LlmReviewer>,
        models: Vec<String>,
    ) -> FileReviewResult {
        let tree = parser::parse(source, lang).unwrap();
        let config = PipelineConfig {
            models,
            ..Default::default()
        };
        review_file(Path::new("test.rs"), source, lang, &tree, llm, &config)
            .await
            .unwrap()
    }

    #[test]
    fn context7_skip_reason_default_is_none() {
        // Default config: enrichment runs. Pin so a future struct-init
        // change can't silently flip the default to "skipped".
        let cfg = PipelineConfig::default();
        assert!(context7_skip_reason(&cfg).is_none());
    }

    #[test]
    fn context7_skip_reason_honors_skip_context7_flag() {
        // CLI --skip-context7 wins regardless of bootstrap state.
        let cfg = PipelineConfig {
            skip_context7: true,
            ..Default::default()
        };
        assert_eq!(context7_skip_reason(&cfg), Some("--skip-context7"));
    }

    #[test]
    fn context7_skip_reason_honors_context7_disabled_flag() {
        // CR8: when main.rs fails to construct the shared HTTP fetcher
        // it sets context7_disabled = true. Pipeline must skip cleanly
        // instead of falling through to per-file Context7HttpFetcher::new()
        // which would re-fail and abort each file's review.
        let cfg = PipelineConfig {
            context7_disabled: true,
            ..Default::default()
        };
        assert_eq!(context7_skip_reason(&cfg), Some("init failed"));
    }

    /// Issue #58 followup: with the async-permit shape, the helper
    /// no longer cares about runtime presence — building a fresh
    /// current-thread runtime to drive the future is the caller's
    /// responsibility. The function is still safe to *call* from
    /// non-runtime code (returns a future, no panic on construction).
    #[test]
    fn acquire_llm_permit_returns_future_outside_tokio_runtime() {
        let sem = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
        // Constructing the future must not panic. We don't poll it —
        // that's the caller's job once they have a runtime.
        let _fut = acquire_llm_permit(&sem);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_returns_none_when_no_semaphore() {
        let result = acquire_llm_permit(&None).await;
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_returns_some_when_permit_available_on_current_thread() {
        // Issue #71/#81: confirm the happy path works on a
        // current-thread runtime — the formerly-deadlocking flavor.
        let sem = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
        let permit = acquire_llm_permit(&sem).await;
        assert!(permit.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_llm_permit_returns_some_when_permit_available_on_multi_thread() {
        // Multi-thread coverage so the fix isn't current_thread-specific.
        let sem = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
        let permit = acquire_llm_permit(&sem).await;
        assert!(permit.is_some());
    }

    /// Closed-semaphore degrades to None (mirrors no-throttle
    /// contract). Cheap mutation-killer: any change that turns the
    /// `.ok()` into an unwrap or removes the `?` would fail this.
    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_returns_none_when_semaphore_is_closed() {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        sem.close();
        let opt = Some(sem);
        assert!(acquire_llm_permit(&opt).await.is_none());
    }

    /// When a waiter on `acquire_llm_permit` is dropped (cancelled)
    /// before the permit becomes available, no permit is leaked and
    /// later acquisitions still work. Standard async cancellation
    /// guarantee — verifies we haven't accidentally broken it.
    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_cancellation_does_not_leak() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Semaphore;

        let sem = Arc::new(Semaphore::new(1));
        let opt = Some(sem.clone());

        // Hold the only permit until we explicitly drop below.
        let holder = sem.clone().acquire_owned().await.unwrap();

        // Spawn a waiter and immediately abort it.
        let opt_clone = opt.clone();
        let waiter = tokio::spawn(async move { acquire_llm_permit(&opt_clone).await });
        // Give the waiter a chance to start awaiting.
        tokio::task::yield_now().await;
        waiter.abort();
        let _ = waiter.await; // join the cancelled task

        // Available permits unchanged — still 0 because the holder
        // is alive. Drop the holder to release.
        assert_eq!(sem.available_permits(), 0);
        drop(holder);

        // Next acquirer must succeed; 2s bound is generous for
        // slow CI but tight enough to flag a regression.
        let permit = tokio::time::timeout(Duration::from_secs(2), acquire_llm_permit(&opt))
            .await
            .expect("should not time out after holder release");
        assert!(permit.is_some());
    }

    /// Same contention pattern as the current-thread regression but
    /// on a multi-thread runtime. Documents that the fix preserves
    /// production behavior (the path that already worked).
    /// Uses Notify for deterministic happens-before, mirroring the
    /// current-thread test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_llm_permit_does_not_deadlock_under_contention_on_multi_thread() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::{Notify, Semaphore};

        let sem = Arc::new(Semaphore::new(1));
        let opt = Some(sem.clone());
        let waiter_started = Arc::new(Notify::new());

        let holder_sem = sem.clone();
        let holder_signal = waiter_started.clone();
        let holder = async move {
            let _h = holder_sem.acquire_owned().await.unwrap();
            holder_signal.notified().await;
        };
        let waiter_signal = waiter_started.clone();
        let waiter = async move {
            tokio::task::yield_now().await;
            waiter_signal.notify_one();
            acquire_llm_permit(&opt).await
        };

        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let (_, w) = tokio::join!(holder, waiter);
            w
        })
        .await
        .expect("multi-thread contention timed out");
        assert!(result.is_some(), "waiter should receive a permit");
    }

    /// Issue #81 regression: on a current-thread runtime, if the permit
    /// holder is another task on the same runtime, the OLD synchronous
    /// `acquire_llm_permit` deadlocks (it blocks the runtime worker at
    /// `std::thread::scope.join()`, so the holder can never run and
    /// release). Post-fix, async acquisition cooperatively yields and
    /// the holder runs to completion.
    ///
    /// Uses `tokio::sync::Notify` (not `sleep`) for a deterministic
    /// happens-before between holder.acquired and waiter.start —
    /// avoids timing flakiness on slow CI.
    #[tokio::test(flavor = "current_thread")]
    async fn acquire_llm_permit_does_not_deadlock_under_contention_on_current_thread() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::{Notify, Semaphore};

        let sem = Arc::new(Semaphore::new(1));
        let opt = Some(sem.clone());
        let waiter_started = Arc::new(Notify::new());

        // Holder: takes the only permit, waits for the waiter to be
        // observably parked on acquire (Notify), then drops the permit.
        let holder_sem = sem.clone();
        let holder_signal = waiter_started.clone();
        let holder = async move {
            let _h = holder_sem.acquire_owned().await.unwrap();
            holder_signal.notified().await;
            // permit dropped at scope exit — waiter unparks
        };

        // Waiter: yields once so the holder grabs the permit, signals
        // "I'm about to acquire", then awaits permit. Pre-fix, this
        // deadlocks: the SYNC acquire_llm_permit blocks the only worker
        // at join() and the holder can never run to release.
        let waiter_signal = waiter_started.clone();
        let waiter = async move {
            tokio::task::yield_now().await;
            waiter_signal.notify_one();
            acquire_llm_permit(&opt).await
        };

        // Wrap in a 5s timeout so a regression manifests as a fast
        // test failure, not a hung CI job. Capture the waiter's
        // result so the failure message is unambiguous.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(holder, waiter)
        })
        .await;

        let (_, waiter_permit) = result.expect(
            "deadlock regression: tokio::join! on current-thread \
             runtime did not complete within 5s — issue #81",
        );
        assert!(
            waiter_permit.is_some(),
            "waiter must receive a permit once the holder releases"
        );
    }

    // -- Complexity threshold default --

    #[test]
    fn default_complexity_threshold_is_ten() {
        assert_eq!(PipelineConfig::default().complexity_threshold, 10);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_default_does_not_flag_cc_six_function() {
        let source = "fn moderate(a: bool, b: bool, c: bool) {\n    if a {\n        if b {\n            if c {\n                for i in 0..10 {\n                    if i > 5 { break; }\n                }\n            }\n        }\n    }\n}\n";
        let result = parse_and_review(source, Language::Rust, None, vec![]).await;
        assert!(
            !result.findings.iter().any(|f| f.category == "complexity"),
            "CC=6 should not flag at default threshold=10"
        );
    }

    // -- Local-only mode --

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_local_only_no_llm() {
        let source = "fn simple() -> i32 { 42 }";
        let result = parse_and_review(source, Language::Rust, None, vec![]).await;
        // Simple function: no findings expected
        assert!(result.findings.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_local_finds_complexity() {
        // CC=11: 10 decision points + 1 baseline. Must exceed default threshold (10).
        let source = "fn complex(a: bool, b: bool, c: bool, d: bool, e: bool) {\n    if a { return; }\n    if b { return; }\n    if c { return; }\n    if d { return; }\n    if e { return; }\n    for i in 0..10 {\n        if i > 5 { break; }\n        while i < 3 { break; }\n        match i { 0 => {}, 1 => {}, _ => {} }\n    }\n}\n";
        let result = parse_and_review(source, Language::Rust, None, vec![]).await;
        assert!(!result.findings.is_empty());
        assert!(result.findings.iter().any(|f| f.category == "performance"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_local_finds_insecure() {
        let source = "def run(code):\n    eval(code)\n";
        let result = parse_and_review(source, Language::Python, None, vec![]).await;
        assert!(result.findings.iter().any(|f| f.category == "security"));
    }

    // -- With LLM --

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_llm_findings_merged_with_local() {
        let source = "def run(code):\n    eval(code)\n";
        let llm_response = r#"[{"title":"Dangerous eval","description":"eval is dangerous","severity":"critical","category":"security","line_start":2,"line_end":2}]"#;
        let llm = FakeReviewer::always(llm_response);
        let result =
            parse_and_review(source, Language::Python, Some(&llm), vec!["gpt-5.4".into()]).await;
        // Should have findings from both local and LLM, merged
        assert!(!result.findings.is_empty());
        assert!(
            result
                .findings
                .iter()
                .any(|f| matches!(&f.source, Source::LocalAst))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_llm_empty_response() {
        let source = "fn safe() -> i32 { 42 }";
        let llm = FakeReviewer::always("[]");
        let result =
            parse_and_review(source, Language::Rust, Some(&llm), vec!["gpt-5.4".into()]).await;
        assert!(result.findings.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_llm_failure_degrades_gracefully() {
        let source = "fn safe() -> i32 { 42 }";
        let llm = FakeReviewer::failing("network error");
        let result =
            parse_and_review(source, Language::Rust, Some(&llm), vec!["gpt-5.4".into()]).await;
        // LLM failure should not crash; local results still work
        assert!(result.findings.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_llm_malformed_response_degrades_gracefully() {
        let source = "fn safe() -> i32 { 42 }";
        let llm = FakeReviewer::always("not valid json");
        let result =
            parse_and_review(source, Language::Rust, Some(&llm), vec!["gpt-5.4".into()]).await;
        assert!(result.findings.is_empty());
    }

    // -- Multi-model ensemble --

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_ensemble_multiple_models() {
        let source = "fn x() -> i32 { 42 }";
        let llm_response = r#"[{"title":"Style issue","description":"Consider naming","severity":"info","category":"style","line_start":1,"line_end":1}]"#;
        let llm = FakeReviewer::always(llm_response);
        let result = parse_and_review(
            source,
            Language::Rust,
            Some(&llm),
            vec!["gpt-5.4".into(), "claude".into()],
        )
        .await;
        // Same response from both models should be deduped
        assert!(!result.findings.is_empty());
        // Should be merged (not duplicated)
        let style_findings: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.category == "maintainability")
            .collect();
        assert_eq!(
            style_findings.len(),
            1,
            "Duplicate findings should be merged"
        );
    }

    // -- Secret redaction --

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_redacts_secrets_before_llm() {
        let source = "API_KEY = \"sk-proj-secret123456\"\nfn main() {}";
        let llm = FakeReviewer::always("[]");
        // We can't directly verify the prompt content through the FakeLlmReviewer,
        // but we verify redaction works on the source
        let redacted = redact::redact_secrets(source);
        assert!(!redacted.contains("sk-proj-secret123456"));

        // Pipeline should still work
        let result =
            parse_and_review(source, Language::Rust, Some(&llm), vec!["gpt-5.4".into()]).await;
        // Pipeline completes without panic — redaction doesn't affect local analysis
        assert!(result.file_path == "test.rs");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn pipeline_file_path_in_result() {
        let source = "fn x() {}";
        let result = parse_and_review(source, Language::Rust, None, vec![]).await;
        assert_eq!(result.file_path, "test.rs");
    }

    // -- Cache integration --

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_source_without_cache() {
        let source = "fn simple() -> i32 { 42 }";
        let config = PipelineConfig::default();
        let result = review_source(
            Path::new("test.rs"),
            source,
            Language::Rust,
            None,
            &config,
            None,
        )
        .await
        .unwrap();
        assert!(result.findings.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_source_with_cache_populates_cache() {
        let cache = crate::cache::ParseCache::new(10);
        let source = "fn simple() -> i32 { 42 }";
        let config = PipelineConfig::default();

        let _result = review_source(
            Path::new("test.rs"),
            source,
            Language::Rust,
            None,
            &config,
            Some(&cache),
        )
        .await
        .unwrap();

        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);

        // Second call with same content should hit cache
        let _result2 = review_source(
            Path::new("test.rs"),
            source,
            Language::Rust,
            None,
            &config,
            Some(&cache),
        )
        .await
        .unwrap();

        assert_eq!(cache.stats().hits, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_source_cache_different_files() {
        let cache = crate::cache::ParseCache::new(10);
        let config = PipelineConfig::default();

        review_source(
            Path::new("a.rs"),
            "fn a() {}",
            Language::Rust,
            None,
            &config,
            Some(&cache),
        )
        .await
        .unwrap();
        review_source(
            Path::new("b.rs"),
            "fn b() {}",
            Language::Rust,
            None,
            &config,
            Some(&cache),
        )
        .await
        .unwrap();

        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().size, 2);
    }

    #[test]
    fn truncate_source_within_limit() {
        let source = "line1\nline2\nline3\n";
        let (truncated, notice) = truncate_for_review(source, 100);
        assert_eq!(truncated, source);
        assert!(notice.is_none());
    }

    #[test]
    fn truncate_source_over_limit() {
        let source = (0..600)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (truncated, notice) = truncate_for_review(&source, 500);
        let truncated_lines = truncated.lines().count();
        assert_eq!(truncated_lines, 500);
        let notice = notice.expect("should have truncation notice");
        assert!(notice.contains("500"));
        assert!(notice.contains("600"));
    }

    #[test]
    fn truncate_source_at_exact_limit() {
        let source = (0..500)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (truncated, notice) = truncate_for_review(&source, 500);
        assert_eq!(truncated, source);
        assert!(notice.is_none());
    }

    #[test]
    fn truncate_source_zero_limit_clamps_to_one() {
        let source = "line1\nline2\nline3\n";
        let (truncated, notice) = truncate_for_review(source, 0);
        // Should clamp to 1 line, not produce empty output
        assert_eq!(truncated.lines().count(), 1);
        assert!(notice.is_some());
    }

    #[test]
    fn pipeline_config_default_has_no_semaphore() {
        let cfg = PipelineConfig::default();
        assert!(cfg.semaphore.is_none());
        assert!(cfg.feedback_index.is_none());
    }

    #[test]
    fn pipeline_config_with_semaphore() {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let cfg = PipelineConfig {
            semaphore: Some(sem.clone()),
            ..Default::default()
        };
        assert_eq!(cfg.semaphore.as_ref().unwrap().available_permits(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_file_works_with_semaphore() {
        // Pre-#81: this test built its own Runtime + entered it because
        // review_file was sync and acquire_llm_permit needed a runtime
        // context. Post-#81: review_file is async, the #[tokio::test]
        // runtime drives it directly.
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
        let cfg = PipelineConfig {
            models: vec!["test-model".into()],
            semaphore: Some(sem),
            ..Default::default()
        };

        struct EmptyReviewer;
        impl LlmReviewer for EmptyReviewer {
            fn review(&self, _: &str, _: &str) -> anyhow::Result<crate::llm_client::LlmResponse> {
                Ok(crate::llm_client::LlmResponse {
                    content: "[]".into(),
                    usage: None,
                })
            }
        }

        let source = "fn main() {}";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let result = review_file(
            std::path::Path::new("test.rs"),
            source,
            Language::Rust,
            &tree,
            Some(&EmptyReviewer),
            &cfg,
        )
        .await;
        assert!(result.is_ok());
    }

    // ---------------------------------------------------------------------
    // #137 — diff_path_matches: full repo-relative equality, NOT ends_with.
    //
    // Component-suffix matching is INSUFFICIENT: `[src, foo.rs]` IS a tail of
    // `[nested, src, foo.rs]`, so a naive ends_with would still cross-match
    // sibling files. The matcher resolves both sides through repo_root and
    // compares the full repo-relative component sequence.
    // ---------------------------------------------------------------------

    #[test]
    fn diff_path_matches_rejects_sibling_with_same_basename() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("nested/src")).unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "").unwrap();
        std::fs::write(tmp.path().join("nested/src/foo.rs"), "").unwrap();

        let nested = tmp
            .path()
            .join("nested/src/foo.rs")
            .to_string_lossy()
            .to_string();
        assert!(
            !diff_path_matches("src/foo.rs", &nested, tmp.path()),
            "must not cross-match sibling file with the same basename"
        );
    }

    #[test]
    fn diff_path_matches_canonical_same_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "").unwrap();

        let rel = tmp.path().join("src/foo.rs").to_string_lossy().to_string();
        assert!(diff_path_matches("src/foo.rs", &rel, tmp.path()));
    }

    #[test]
    fn diff_path_matches_absolute_review_path_under_repo_root() {
        // diff_path is repo-relative; review path is absolute but resolves
        // under repo_root. Must match.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "").unwrap();

        let abs = tmp
            .path()
            .join("src/foo.rs")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(diff_path_matches("src/foo.rs", &abs, tmp.path()));
    }

    #[test]
    fn diff_path_matches_returns_false_when_review_outside_repo() {
        // Review path resolves outside repo_root → refuse to match. Wrong
        // context for the LLM is worse than no context.
        let repo = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(other.path().join("src")).unwrap();
        std::fs::write(other.path().join("src/foo.rs"), "").unwrap();

        let outside = other
            .path()
            .join("src/foo.rs")
            .to_string_lossy()
            .to_string();
        assert!(!diff_path_matches("src/foo.rs", &outside, repo.path()));
    }

    // ---------------------------------------------------------------------
    // #137 perf — ReviewPathResolver: canonicalization is a loop-invariant.
    // The resolver is constructed once before the diff_ranges filter so the
    // hot path is just `Path::components().eq()`. These tests assert the
    // observable behavior matches the back-compat wrapper across many
    // diff_path inputs from a single resolver instance.
    // ---------------------------------------------------------------------

    #[test]
    fn review_path_resolver_matches_repeatedly_on_single_instance() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("nested/src")).unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "").unwrap();
        std::fs::write(tmp.path().join("nested/src/foo.rs"), "").unwrap();

        let review = tmp.path().join("src/foo.rs").to_string_lossy().to_string();
        let resolver = ReviewPathResolver::new(&review, tmp.path());

        // Many diff_paths against the same resolver — exercises the hot loop.
        assert!(resolver.matches("src/foo.rs"));
        assert!(!resolver.matches("nested/src/foo.rs"));
        assert!(!resolver.matches("src/bar.rs"));
        assert!(!resolver.matches("other/src/foo.rs"));
        // Idempotent — second call should not re-canonicalize.
        assert!(resolver.matches("src/foo.rs"));
    }

    #[test]
    fn review_path_resolver_outside_repo_never_matches() {
        let repo = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(other.path().join("src")).unwrap();
        std::fs::write(other.path().join("src/foo.rs"), "").unwrap();

        let outside = other
            .path()
            .join("src/foo.rs")
            .to_string_lossy()
            .to_string();
        let resolver = ReviewPathResolver::new(&outside, repo.path());
        assert!(!resolver.matches("src/foo.rs"));
        assert!(!resolver.matches("anything"));
    }
}
