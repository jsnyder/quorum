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

/// Trait for LLM review — allows testing with fake implementations.
pub trait LlmReviewer: Send + Sync {
    fn review(&self, prompt: &str, model: &str) -> anyhow::Result<crate::llm_client::LlmResponse>;
}

/// Result of reviewing a single file.
pub struct FileReviewResult {
    pub file_path: String,
    pub findings: Vec<Finding>,
    pub usage: crate::llm_client::TokenUsage,
    pub suppressed: usize,
}

pub struct PipelineConfig {
    pub complexity_threshold: u32,
    pub similarity_threshold: f64,
    pub models: Vec<String>,
    pub calibration_model: Option<String>,
    pub feedback: Vec<FeedbackEntry>,
    pub calibrate: bool,
    pub auto_calibrate: bool,
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
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            complexity_threshold: 5,
            similarity_threshold: 0.8,
            models: vec![],
            calibration_model: None,
            feedback: vec![],
            calibrate: true,
            auto_calibrate: true,
            feedback_store: None,
            diff_ranges: None,
            max_review_lines: 500,
            framework_overrides: Vec::new(),
            semaphore: None,
            feedback_index: None,
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

    // Build feedback index once — used for both few-shot injection and calibration
    let mut feedback_index = if let Some(store_path) = &pipeline_config.feedback_store {
        let store = crate::feedback::FeedbackStore::new(store_path.clone());
        crate::feedback_index::FeedbackIndex::build(&store).ok()
    } else {
        None
    };

    // Source 1: Local AST analysis
    let mut local_findings = Vec::new();
    local_findings.extend(analysis::analyze_complexity(tree, source, lang, pipeline_config.complexity_threshold));
    local_findings.extend(analysis::analyze_insecure_patterns(tree, source, lang));
    all_sources.push(local_findings);

    // Source 2: ast-grep library rules
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ast_grep::ext_to_language(ext).is_some() {
        let project_root = find_project_root(file_path);
        let home_dir = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let rules = ast_grep::load_rules(&project_root, &home_dir);
        if !rules.is_empty() {
            let ag_findings = ast_grep::scan_file(source, ext, &rules);
            if !ag_findings.is_empty() {
                all_sources.push(ag_findings);
            }
        }
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
        let ctx = hydration::hydrate(tree, source, lang, &hydration_ranges);

        // Redact secrets in both code AND hydration context before LLM
        let redacted_code = redact::redact_secrets(source);
        let (review_code, truncation_notice) = truncate_for_review(&redacted_code, pipeline_config.max_review_lines);
        let redacted_ctx = crate::hydration::HydrationContext {
            callee_signatures: ctx.callee_signatures.iter().map(|s| redact::redact_secrets(s)).collect(),
            type_definitions: ctx.type_definitions.iter().map(|s| redact::redact_secrets(s)).collect(),
            callers: ctx.callers.clone(),
            import_targets: ctx.import_targets.iter().map(|s| redact::redact_secrets(s)).collect(),
        };

        // Fetch Context7 framework docs if frameworks are detected
        let framework_docs = {
            let project_root = find_project_root(file_path);
            let mut domain = crate::domain::detect_domain(&project_root);
            for fw in &pipeline_config.framework_overrides {
                if !domain.frameworks.contains(fw) {
                    domain.frameworks.push(fw.clone());
                }
            }
            if !domain.frameworks.is_empty() {
                eprintln!("Detected frameworks: {:?}", domain.frameworks);
                let fetcher = crate::context_enrichment::Context7HttpFetcher::new();
                let cached_fetcher = crate::context_enrichment::CachedContextFetcher::new(&fetcher, 32);
                let docs = crate::context_enrichment::fetch_framework_docs(&domain.frameworks, &cached_fetcher, &redacted_ctx.import_targets);
                if !docs.is_empty() {
                    eprintln!("Context7: injected {} framework doc(s) into prompt", docs.len());
                    Some(docs.iter().map(|d| crate::context_enrichment::format_context_section(&[d.clone()])).collect())
                } else {
                    eprintln!("Context7: no docs fetched (key missing or API unavailable)");
                    None
                }
            } else {
                None
            }
        };

        // Query feedback index for few-shot precedents
        let precedents = feedback_index.as_mut().map(|idx| {
            query_feedback_precedents(idx, &file_str, lang_name(lang), &redacted_code)
        }).unwrap_or_default();

        let req = ReviewRequest {
            file_path: file_str.clone(),
            language: lang_name(lang).to_string(),
            code: review_code,
            hydration_context: Some(redacted_ctx),
            framework_docs,
            feedback_precedents: if precedents.is_empty() { None } else { Some(precedents) },
            truncation_notice: truncation_notice.clone(),
        };

        let prompt = review::build_review_prompt(&req);

        for model in &pipeline_config.models {
            match reviewer.review(&prompt, model) {
                Ok(resp) => {
                    if let Some(u) = &resp.usage {
                        total_usage.prompt_tokens += u.prompt_tokens;
                        total_usage.completion_tokens += u.completion_tokens;
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
        } // end if models not empty
    }

    // Merge all sources
    let merged = merge::merge_findings(all_sources, pipeline_config.similarity_threshold);

    // Calibrate using feedback precedent (prefer FeedbackIndex for semantic matching)
    let has_feedback = !pipeline_config.feedback.is_empty() || pipeline_config.feedback_store.is_some();
    let (final_findings, suppressed_count) = if pipeline_config.calibrate && has_feedback {
        let config = CalibratorConfig::default();

        // Reuse FeedbackIndex built earlier for few-shot injection
        let cal_result = if let Some(ref mut index) = feedback_index {
            if !index.is_empty() {
                calibrator::calibrate_with_index(merged, index, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else {
            calibrator::calibrate(merged, &pipeline_config.feedback, &config)
        };

        if cal_result.suppressed > 0 || cal_result.boosted > 0 {
            eprintln!(
                "Calibrator: {} suppressed, {} boosted (from {} feedback entries)",
                cal_result.suppressed, cal_result.boosted, pipeline_config.feedback.len()
            );
        }
        (cal_result.findings, cal_result.suppressed)
    } else {
        (merged, 0)
    };

    // Auto-calibration: use a second LLM pass to triage findings and record verdicts
    if pipeline_config.auto_calibrate && !final_findings.is_empty() {
        if let (Some(reviewer), Some(store_path)) = (llm, &pipeline_config.feedback_store) {
            // Use dedicated calibration model if configured, otherwise fall back to review model
            let model = pipeline_config.calibration_model.as_deref()
                .or_else(|| pipeline_config.models.first().map(|s| s.as_str()))
                .unwrap_or("gpt-5.4");
            let store = crate::feedback::FeedbackStore::new(store_path.clone());
            // Redact secrets before sending to auto-calibration LLM
            let redacted_source = redact::redact_secrets(source);
            match crate::auto_calibrate::auto_calibrate(
                &final_findings, &redacted_source, &file_str, reviewer, model, &store,
            ) {
                Ok(result) if result.recorded > 0 => {
                    eprintln!("Auto-calibrate: recorded {} verdicts for {}", result.recorded, file_str);
                }
                Err(e) => eprintln!("Auto-calibrate warning: {}", e),
                _ => {}
            }
        }
    }

    Ok(FileReviewResult {
        file_path: file_str,
        findings: final_findings,
        usage: total_usage,
        suppressed: suppressed_count,
    })
}

/// Walk up from file path to find the project root (directory containing pyproject.toml, package.json, Cargo.toml, etc.)
pub fn find_project_root(file_path: &Path) -> std::path::PathBuf {
    let markers = ["pyproject.toml", "package.json", "Cargo.toml", "go.mod", "setup.py"];
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

    // Build feedback index once — used for both few-shot injection and calibration
    let mut feedback_index = if let Some(store_path) = &pipeline_config.feedback_store {
        let store = crate::feedback::FeedbackStore::new(store_path.clone());
        crate::feedback_index::FeedbackIndex::build(&store).ok()
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

            // Context7 framework docs
            let framework_docs = {
                let project_root = find_project_root(file_path);
                let mut domain = crate::domain::detect_domain(&project_root);
                for fw in &pipeline_config.framework_overrides {
                    if !domain.frameworks.contains(fw) {
                        domain.frameworks.push(fw.clone());
                    }
                }
                if !domain.frameworks.is_empty() {
                    let fetcher = crate::context_enrichment::Context7HttpFetcher::new();
                    let cached_fetcher = crate::context_enrichment::CachedContextFetcher::new(&fetcher, 32);
                    let docs = crate::context_enrichment::fetch_framework_docs(&domain.frameworks, &cached_fetcher, &[]);
                    if !docs.is_empty() {
                        Some(docs.iter().map(|d| crate::context_enrichment::format_context_section(&[d.clone()])).collect())
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            // Query feedback index for few-shot precedents
            let precedents = feedback_index.as_mut().map(|idx| {
                query_feedback_precedents(idx, &file_str, &language, &redacted_code)
            }).unwrap_or_default();

            let req = ReviewRequest {
                file_path: file_str.clone(),
                language,
                code: review_code,
                hydration_context: None,
                framework_docs,
                feedback_precedents: if precedents.is_empty() { None } else { Some(precedents) },
                truncation_notice: truncation_notice.clone(),
            };

            let prompt = review::build_review_prompt(&req);
            for model in &pipeline_config.models {
                match reviewer.review(&prompt, model) {
                    Ok(resp) => {
                        if let Some(u) = &resp.usage {
                            total_usage.prompt_tokens += u.prompt_tokens;
                            total_usage.completion_tokens += u.completion_tokens;
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
        let config = CalibratorConfig::default();
        // Reuse FeedbackIndex built earlier for few-shot injection
        let cal_result = if let Some(ref mut index) = feedback_index {
            if !index.is_empty() {
                calibrator::calibrate_with_index(merged, index, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else {
            calibrator::calibrate(merged, &pipeline_config.feedback, &config)
        };
        if cal_result.suppressed > 0 || cal_result.boosted > 0 {
            eprintln!(
                "Calibrator: {} suppressed, {} boosted (from {} feedback entries)",
                cal_result.suppressed, cal_result.boosted, pipeline_config.feedback.len()
            );
        }
        (cal_result.findings, cal_result.suppressed)
    } else {
        (merged, 0)
    };

    // Auto-calibration
    if pipeline_config.auto_calibrate && !final_findings.is_empty() {
        if let (Some(reviewer), Some(store_path)) = (llm, &pipeline_config.feedback_store) {
            let model = pipeline_config.calibration_model.as_deref()
                .or_else(|| pipeline_config.models.first().map(|s| s.as_str()))
                .unwrap_or("gpt-5.4");
            let store = crate::feedback::FeedbackStore::new(store_path.clone());
            // Redact secrets before sending to auto-calibration LLM
            let redacted_source = redact::redact_secrets(source);
            match crate::auto_calibrate::auto_calibrate(
                &final_findings, &redacted_source, &file_str, reviewer, model, &store,
            ) {
                Ok(result) if result.recorded > 0 => {
                    eprintln!("Auto-calibrate: recorded {} verdicts for {}", result.recorded, file_str);
                }
                Err(e) => eprintln!("Auto-calibrate warning: {}", e),
                _ => {}
            }
        }
    }

    Ok(FileReviewResult {
        file_path: file_str,
        findings: final_findings,
        usage: total_usage,
        suppressed: suppressed_count,
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
        let source = "fn complex(a: bool, b: bool, c: bool) {\n    if a {\n        if b {\n            if c {\n                for i in 0..10 {\n                    if i > 5 { break; }\n                }\n            }\n        }\n    }\n}\n";
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
}
