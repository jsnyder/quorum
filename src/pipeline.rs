/// Review pipeline: parse -> hydrate -> parallel(LLM + local + linters) -> merge -> calibrate -> output
/// Orchestrates all review sources and produces merged, calibrated findings.

use std::path::Path;

use crate::analysis;
use crate::ast_grep;
use crate::calibrator::{self, CalibratorConfig};
use crate::feedback::FeedbackEntry;
use crate::finding::Finding;
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

/// Acquire a semaphore permit if configured. Uses Handle::block_on (not block_in_place)
/// because this may be called from spawn_blocking threads.
/// Returns an owned permit that is released on drop (RAII).
///
/// Degrades gracefully (returns `None`) in three failure modes that
/// shouldn't crash the caller — same observable behavior as the
/// no-semaphore path:
/// - No semaphore configured (the common case)
/// - Semaphore closed (shutdown in flight)
/// - No Tokio runtime in the current thread (issue #58 — happens when a
///   pipeline is wired lazily from a sync caller; throttling can't work
///   without a runtime, so degrade rather than panic)
fn acquire_llm_permit(sem: &Option<std::sync::Arc<tokio::sync::Semaphore>>) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let sem = sem.as_ref()?.clone();
    let handle = tokio::runtime::Handle::try_current().ok()?;
    handle.block_on(sem.acquire_owned()).ok()
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
    pub feedback_index: Option<std::sync::Arc<std::sync::Mutex<crate::feedback_index::FeedbackIndex>>>,
    /// Skip Context7 enrichment (default false: fail if frameworks detected but docs unavailable)
    pub skip_context7: bool,
    /// Skip fastembed model — fall back to Jaccard word-overlap for calibration.
    pub fast: bool,
    /// Optional hook into the `quorum context` retrieve→plan→render pipeline.
    /// When `Some` and the injector returns `Some(markdown)`, the block is
    /// spliced into the LLM prompt. When `None` (the default), behavior is
    /// byte-identical to the pre-context pipeline.
    pub context_injector: Option<std::sync::Arc<dyn crate::context::inject::ContextInjectionSource>>,
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
        }
    }
}

/// Truncate source code for LLM review if it exceeds the line limit.
/// Returns (possibly truncated source, optional truncation notice).
fn truncate_for_review(source: &str, max_lines: usize) -> (String, Option<String>) {
    let max_lines = max_lines.max(1);
    let total_lines = source.lines().count();
    if total_lines <= max_lines {
        return (source.to_string(), None);
    }
    let truncated: String = source.lines().take(max_lines).collect::<Vec<_>>().join("\n");
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
    let tps: Vec<_> = candidates.iter().filter(|s| s.entry.verdict == Verdict::Tp).take(2).collect();
    let fps: Vec<_> = candidates.iter().filter(|s| s.entry.verdict == Verdict::Fp).take(2).collect();

    let mut selected: Vec<_> = Vec::new();
    // Take up to 2 TPs
    selected.extend(tps.iter().take(2));
    // Fill remaining slots with FPs (up to 3 total)
    for fp in &fps {
        if selected.len() >= 3 { break; }
        selected.push(fp);
    }
    // If we still have room and more TPs, fill
    let remaining_tps: Vec<_> = candidates.iter()
        .filter(|s| s.entry.verdict == Verdict::Tp)
        .skip(2)
        .collect();
    for tp in &remaining_tps {
        if selected.len() >= 3 { break; }
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
        .map(|s| {
            let verdict_label = match s.entry.verdict {
                Verdict::Tp => "TRUE POSITIVE",
                Verdict::Fp => "FALSE POSITIVE",
                _ => "NOTED",
            };
            let truncated_reason: String = s.entry.reason.chars().take(100).collect();
            format!(
                "[{}] {}: {}",
                verdict_label,
                s.entry.finding_title,
                truncated_reason,
            )
        })
        .collect()
}

/// Run the full review pipeline on a single file.
pub fn review_file(
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
        local_findings.extend(analysis::analyze_complexity(tree, source, lang, pipeline_config.complexity_threshold));
        local_findings.extend(analysis::analyze_insecure_patterns(tree, source, lang));
        tracing::info!(phase = "local_ast", duration_ms = t0.elapsed().as_millis() as u64, findings = local_findings.len(), "phase complete");
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
        tracing::info!(phase = "ast_grep", duration_ms = t0.elapsed().as_millis() as u64, rules = rules.len(), findings = ag_count, "phase complete");
    }

    // Source 3: LLM review (if configured and models specified)
    if let Some(reviewer) = llm {
        if pipeline_config.models.is_empty() {
            // No models configured — skip LLM review
        } else {
        // Hydrate context: use diff ranges if available, else full file
        let changed_lines: Vec<(u32, u32)> = if let Some(ref diff_ranges) = pipeline_config.diff_ranges {
            diff_ranges
                .iter()
                .filter(|(path, _)| file_str.ends_with(path.as_str()) || path.ends_with(&file_str))
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
            tracing::info!(phase = "hydrate", duration_ms = hydrate_t0.elapsed().as_millis() as u64, callees = c.callee_signatures.len(), types = c.type_definitions.len(), imports = c.import_targets.len(), "phase complete");
            c
        };

        // Redact secrets in both code AND hydration context before LLM
        let redacted_code = redact::redact_secrets(source);
        let (review_code, truncation_notice) = truncate_for_review(&redacted_code, pipeline_config.max_review_lines);
        let redacted_ctx = crate::hydration::HydrationContext {
            callee_signatures: ctx.callee_signatures.iter().map(|s| redact::redact_secrets(s)).collect(),
            type_definitions: ctx.type_definitions.iter().map(|s| redact::redact_secrets(s)).collect(),
            callers: ctx.callers.clone(),
            import_targets: ctx.import_targets.iter().map(|s| redact::redact_secrets(s)).collect(),
            qualified_names: ctx.qualified_names.clone(),
        };

        // Fetch Context7 framework docs (curated frameworks + dep-based enrichment).
        let framework_docs = if pipeline_config.skip_context7 {
            // --skip-context7: opt out entirely, no Context7 client constructed,
            // no HTTP calls. Metrics stay at defaults.
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
            // Always run enrichment: dep-based path may produce docs even when no
            // curated framework was detected (e.g. a Rust project with tokio).
            let fetcher = crate::context_enrichment::Context7HttpFetcher::new()?;
            let cached_fetcher = crate::context_enrichment::CachedContextFetcher::new(&fetcher, 32);
            let ctx7_t0 = std::time::Instant::now();
            let _span = tracing::info_span!("phase.context7", file = %file_str).entered();
            let result = crate::context_enrichment::enrich_for_review_in_project(
                &project_root,
                &redacted_ctx.import_targets,
                &domain.frameworks,
                &cached_fetcher,
            );
            let docs = result.docs;
            enrichment_metrics = result.metrics;
            tracing::info!(phase = "context7", duration_ms = ctx7_t0.elapsed().as_millis() as u64, frameworks = domain.frameworks.len(), docs = docs.len(), resolved = enrichment_metrics.context7_resolved, resolve_failed = enrichment_metrics.context7_resolve_failed, query_failed = enrichment_metrics.context7_query_failed, "phase complete");
            if !docs.is_empty() {
                tracing::debug!(docs_injected = docs.len(), "Context7 docs injected");
                Some(docs.iter().map(|d| crate::context_enrichment::format_context_section(&[d.clone()])).collect())
            } else if domain.frameworks.is_empty() {
                // No curated framework was requested — silently skip if dep path
                // also produced nothing. Long-tail dep failures are NOT fatal.
                None
            } else if pipeline_config.skip_context7 {
                tracing::debug!("Context7: no docs fetched (skipped via --skip-context7)");
                None
            } else {
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
            feedback_precedents: if precedents.is_empty() { None } else { Some(precedents) },
            context_block,
            truncation_notice: truncation_notice.clone(),
        };

        let prompt = review::build_review_prompt(&req);

        for model in &pipeline_config.models {
            let _span = tracing::info_span!("phase.llm_call", model = %model, file = %file_str).entered();
            let t0 = std::time::Instant::now();
            let _permit = acquire_llm_permit(&pipeline_config.semaphore);
            match reviewer.review(&prompt, model) {
                Ok(resp) => {
                    let (prompt_tok, completion_tok, cached_tok) = resp.usage.as_ref()
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
        tracing::info!(phase = "merge", duration_ms = merge_t0.elapsed().as_millis() as u64, merged_findings = result.len(), "phase complete");
        result
    };

    // Calibrate using feedback precedent (prefer FeedbackIndex for semantic matching)
    let has_feedback = !pipeline_config.feedback.is_empty() || pipeline_config.feedback_store.is_some();
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
        tracing::info!(phase = "calibrate", duration_ms = cal_t0.elapsed().as_millis() as u64, suppressed = cal_result.suppressed, boosted = cal_result.boosted, final_findings = cal_result.findings.len(), "phase complete");

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
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

/// Walk up from file path to find the project root (directory containing pyproject.toml, package.json, Cargo.toml, etc.)
pub fn find_project_root(file_path: &Path) -> std::path::PathBuf {
    let markers = [
        "pyproject.toml", "package.json", "Cargo.toml", "go.mod", "setup.py",
        ".terraform.lock.hcl", "terraform.tfvars",
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
pub fn review_source(
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
    review_file(file_path, source, lang, &tree, llm, pipeline_config)
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
pub fn review_file_llm_only(
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
            let (review_code, truncation_notice) = truncate_for_review(&redacted_code, pipeline_config.max_review_lines);
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
                let fetcher = crate::context_enrichment::Context7HttpFetcher::new()?;
                let cached_fetcher = crate::context_enrichment::CachedContextFetcher::new(&fetcher, 32);
                let result = crate::context_enrichment::enrich_for_review_in_project(
                    &project_root,
                    &[],
                    &domain.frameworks,
                    &cached_fetcher,
                );
                let docs = result.docs;
                enrichment_metrics = result.metrics;
                if !docs.is_empty() {
                    Some(docs.iter().map(|d| crate::context_enrichment::format_context_section(&[d.clone()])).collect())
                } else if domain.frameworks.is_empty() {
                    None
                } else if pipeline_config.skip_context7 {
                    eprintln!("Context7: no docs fetched (skipped via --skip-context7)");
                    None
                } else {
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
                feedback_precedents: if precedents.is_empty() { None } else { Some(precedents) },
                context_block: None,
                truncation_notice: truncation_notice.clone(),
            };

            let prompt = review::build_review_prompt(&req);
            for model in &pipeline_config.models {
                let _permit = acquire_llm_permit(&pipeline_config.semaphore);
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
                            Err(e) => eprintln!("Warning: Failed to parse {} response: {}", model, e),
                        }
                    }
                    Err(e) => eprintln!("Warning: {} review failed: {}", model, e),
                }
            }
        }
    }

    let merged = merge::merge_findings(all_sources, pipeline_config.similarity_threshold);

    // Calibrate
    let has_feedback = !pipeline_config.feedback.is_empty() || pipeline_config.feedback_store.is_some();
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
        tracing::info!(phase = "calibrate", duration_ms = cal_t0.elapsed().as_millis() as u64, suppressed = cal_result.suppressed, boosted = cal_result.boosted, final_findings = cal_result.findings.len(), "phase complete");

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
    use crate::finding::Source;

    use crate::test_support::fakes::FakeReviewer;

    fn parse_and_review(source: &str, lang: Language, llm: Option<&dyn LlmReviewer>, models: Vec<String>) -> FileReviewResult {
        let tree = parser::parse(source, lang).unwrap();
        let config = PipelineConfig {
            models,
            ..Default::default()
        };
        review_file(Path::new("test.rs"), source, lang, &tree, llm, &config).unwrap()
    }

    #[test]
    fn acquire_llm_permit_does_not_panic_outside_tokio_runtime() {
        // Issue #58: acquire_llm_permit calls Handle::current() which
        // panics if no Tokio runtime exists. Callers that lazily wire a
        // Pipeline from a sync context (e.g., embedders, tests) would
        // crash on the first throttled review. Degrade to "no permit"
        // instead — same observable behavior as the no-semaphore path.
        let sem = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            acquire_llm_permit(&sem)
        }));
        assert!(
            result.is_ok(),
            "acquire_llm_permit panicked outside Tokio runtime: {:?}",
            result.err()
        );
        // None is acceptable here: throttling can't work without a runtime,
        // so we degrade rather than crash.
        let permit = result.unwrap();
        let _ = permit;
    }

    #[test]
    fn acquire_llm_permit_returns_none_when_no_semaphore() {
        let result = acquire_llm_permit(&None);
        assert!(result.is_none());
    }

    // -- Complexity threshold default --

    #[test]
    fn default_complexity_threshold_is_ten() {
        assert_eq!(PipelineConfig::default().complexity_threshold, 10);
    }

    #[test]
    fn pipeline_default_does_not_flag_cc_six_function() {
        let source = "fn moderate(a: bool, b: bool, c: bool) {\n    if a {\n        if b {\n            if c {\n                for i in 0..10 {\n                    if i > 5 { break; }\n                }\n            }\n        }\n    }\n}\n";
        let result = parse_and_review(source, Language::Rust, None, vec![]);
        assert!(
            !result.findings.iter().any(|f| f.category == "complexity"),
            "CC=6 should not flag at default threshold=10"
        );
    }

    // -- Local-only mode --

    #[test]
    fn pipeline_local_only_no_llm() {
        let source = "fn simple() -> i32 { 42 }";
        let result = parse_and_review(source, Language::Rust, None, vec![]);
        // Simple function: no findings expected
        assert!(result.findings.is_empty());
    }

    #[test]
    fn pipeline_local_finds_complexity() {
        // CC=11: 10 decision points + 1 baseline. Must exceed default threshold (10).
        let source = "fn complex(a: bool, b: bool, c: bool, d: bool, e: bool) {\n    if a { return; }\n    if b { return; }\n    if c { return; }\n    if d { return; }\n    if e { return; }\n    for i in 0..10 {\n        if i > 5 { break; }\n        while i < 3 { break; }\n        match i { 0 => {}, 1 => {}, _ => {} }\n    }\n}\n";
        let result = parse_and_review(source, Language::Rust, None, vec![]);
        assert!(!result.findings.is_empty());
        assert!(result.findings.iter().any(|f| f.category == "complexity"));
    }

    #[test]
    fn pipeline_local_finds_insecure() {
        let source = "def run(code):\n    eval(code)\n";
        let result = parse_and_review(source, Language::Python, None, vec![]);
        assert!(result.findings.iter().any(|f| f.category == "security"));
    }

    // -- With LLM --

    #[test]
    fn pipeline_llm_findings_merged_with_local() {
        let source = "def run(code):\n    eval(code)\n";
        let llm_response = r#"[{"title":"Dangerous eval","description":"eval is dangerous","severity":"critical","category":"security","line_start":2,"line_end":2}]"#;
        let llm = FakeReviewer::always(llm_response);
        let result = parse_and_review(
            source, Language::Python,
            Some(&llm),
            vec!["gpt-5.4".into()],
        );
        // Should have findings from both local and LLM, merged
        assert!(!result.findings.is_empty());
        assert!(result.findings.iter().any(|f| matches!(&f.source, Source::LocalAst)));
    }

    #[test]
    fn pipeline_llm_empty_response() {
        let source = "fn safe() -> i32 { 42 }";
        let llm = FakeReviewer::always("[]");
        let result = parse_and_review(
            source, Language::Rust,
            Some(&llm),
            vec!["gpt-5.4".into()],
        );
        assert!(result.findings.is_empty());
    }

    #[test]
    fn pipeline_llm_failure_degrades_gracefully() {
        let source = "fn safe() -> i32 { 42 }";
        let llm = FakeReviewer::failing("network error");
        let result = parse_and_review(
            source, Language::Rust,
            Some(&llm),
            vec!["gpt-5.4".into()],
        );
        // LLM failure should not crash; local results still work
        assert!(result.findings.is_empty());
    }

    #[test]
    fn pipeline_llm_malformed_response_degrades_gracefully() {
        let source = "fn safe() -> i32 { 42 }";
        let llm = FakeReviewer::always("not valid json");
        let result = parse_and_review(
            source, Language::Rust,
            Some(&llm),
            vec!["gpt-5.4".into()],
        );
        assert!(result.findings.is_empty());
    }

    // -- Multi-model ensemble --

    #[test]
    fn pipeline_ensemble_multiple_models() {
        let source = "fn x() -> i32 { 42 }";
        let llm_response = r#"[{"title":"Style issue","description":"Consider naming","severity":"info","category":"style","line_start":1,"line_end":1}]"#;
        let llm = FakeReviewer::always(llm_response);
        let result = parse_and_review(
            source, Language::Rust,
            Some(&llm),
            vec!["gpt-5.4".into(), "claude".into()],
        );
        // Same response from both models should be deduped
        assert!(!result.findings.is_empty());
        // Should be merged (not duplicated)
        let style_findings: Vec<_> = result.findings.iter()
            .filter(|f| f.category == "style")
            .collect();
        assert_eq!(style_findings.len(), 1, "Duplicate findings should be merged");
    }

    // -- Secret redaction --

    #[test]
    fn pipeline_redacts_secrets_before_llm() {
        let source = "API_KEY = \"sk-proj-secret123456\"\nfn main() {}";
        let llm = FakeReviewer::always("[]");
        // We can't directly verify the prompt content through the FakeLlmReviewer,
        // but we verify redaction works on the source
        let redacted = redact::redact_secrets(source);
        assert!(!redacted.contains("sk-proj-secret123456"));

        // Pipeline should still work
        let result = parse_and_review(
            source, Language::Rust,
            Some(&llm),
            vec!["gpt-5.4".into()],
        );
        // Pipeline completes without panic — redaction doesn't affect local analysis
        assert!(result.file_path == "test.rs");
    }

    #[test]
    fn pipeline_file_path_in_result() {
        let source = "fn x() {}";
        let result = parse_and_review(source, Language::Rust, None, vec![]);
        assert_eq!(result.file_path, "test.rs");
    }

    // -- Cache integration --

    #[test]
    fn review_source_without_cache() {
        let source = "fn simple() -> i32 { 42 }";
        let config = PipelineConfig::default();
        let result = review_source(
            Path::new("test.rs"), source, Language::Rust,
            None, &config, None,
        ).unwrap();
        assert!(result.findings.is_empty());
    }

    #[test]
    fn review_source_with_cache_populates_cache() {
        let cache = crate::cache::ParseCache::new(10);
        let source = "fn simple() -> i32 { 42 }";
        let config = PipelineConfig::default();

        let _result = review_source(
            Path::new("test.rs"), source, Language::Rust,
            None, &config, Some(&cache),
        ).unwrap();

        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);

        // Second call with same content should hit cache
        let _result2 = review_source(
            Path::new("test.rs"), source, Language::Rust,
            None, &config, Some(&cache),
        ).unwrap();

        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn review_source_cache_different_files() {
        let cache = crate::cache::ParseCache::new(10);
        let config = PipelineConfig::default();

        review_source(
            Path::new("a.rs"), "fn a() {}", Language::Rust,
            None, &config, Some(&cache),
        ).unwrap();
        review_source(
            Path::new("b.rs"), "fn b() {}", Language::Rust,
            None, &config, Some(&cache),
        ).unwrap();

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
        let source = (0..600).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let (truncated, notice) = truncate_for_review(&source, 500);
        let truncated_lines = truncated.lines().count();
        assert_eq!(truncated_lines, 500);
        let notice = notice.expect("should have truncation notice");
        assert!(notice.contains("500"));
        assert!(notice.contains("600"));
    }

    #[test]
    fn truncate_source_at_exact_limit() {
        let source = (0..500).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
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

    #[test]
    fn review_file_works_with_semaphore() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
        let cfg = PipelineConfig {
            models: vec!["test-model".into()],
            semaphore: Some(sem),
            ..Default::default()
        };

        struct EmptyReviewer;
        impl LlmReviewer for EmptyReviewer {
            fn review(&self, _: &str, _: &str) -> anyhow::Result<crate::llm_client::LlmResponse> {
                Ok(crate::llm_client::LlmResponse { content: "[]".into(), usage: None })
            }
        }

        let source = "fn main() {}";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let result = review_file(
            std::path::Path::new("test.rs"), source, Language::Rust, &tree,
            Some(&EmptyReviewer), &cfg,
        );
        assert!(result.is_ok());
    }
}
