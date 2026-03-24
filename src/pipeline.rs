/// Review pipeline: parse -> hydrate -> parallel(LLM + local + linters) -> merge -> calibrate -> output
/// Orchestrates all review sources and produces merged, calibrated findings.

use std::path::Path;

use crate::analysis;
use crate::calibrator::{self, CalibratorConfig};
use crate::config::Config;
use crate::feedback::FeedbackEntry;
use crate::finding::Finding;
use crate::hydration;
use crate::merge;
use crate::parser::{self, Language};
use crate::redact;
use crate::review::{self, ReviewRequest};

/// Trait for LLM review — allows testing with fake implementations.
pub trait LlmReviewer: Send + Sync {
    fn review(&self, prompt: &str, model: &str) -> anyhow::Result<String>;
}

/// Result of reviewing a single file.
pub struct FileReviewResult {
    pub file_path: String,
    pub findings: Vec<Finding>,
}

pub struct PipelineConfig {
    pub complexity_threshold: u32,
    pub similarity_threshold: f64,
    pub models: Vec<String>,
    pub feedback: Vec<FeedbackEntry>,
    pub calibrate: bool,
    pub auto_calibrate: bool,
    pub feedback_store: Option<std::path::PathBuf>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            complexity_threshold: 5,
            similarity_threshold: 0.8,
            models: vec![],
            feedback: vec![],
            calibrate: true,
            auto_calibrate: true,
            feedback_store: None,
        }
    }
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

    // Source 1: Local AST analysis
    let mut local_findings = Vec::new();
    local_findings.extend(analysis::analyze_complexity(tree, source, lang, pipeline_config.complexity_threshold));
    local_findings.extend(analysis::analyze_insecure_patterns(tree, source, lang));
    all_sources.push(local_findings);

    // Source 2: LLM review (if configured and models specified)
    if let Some(reviewer) = llm {
        if pipeline_config.models.is_empty() {
            // No models configured — skip LLM review
        } else {
        // Hydrate context (using full file as changed range for now)
        let total_lines = source.lines().count() as u32;
        let ctx = hydration::hydrate(tree, source, lang, &[(1, total_lines.max(1))]);

        // Redact secrets in both code AND hydration context before LLM
        let redacted_code = redact::redact_secrets(source);
        let redacted_ctx = crate::hydration::HydrationContext {
            callee_signatures: ctx.callee_signatures.iter().map(|s| redact::redact_secrets(s)).collect(),
            type_definitions: ctx.type_definitions.iter().map(|s| redact::redact_secrets(s)).collect(),
            callers: ctx.callers.clone(),
            import_targets: ctx.import_targets.iter().map(|s| redact::redact_secrets(s)).collect(),
        };

        let req = ReviewRequest {
            file_path: file_str.clone(),
            language: lang_name(lang).to_string(),
            code: redacted_code,
            hydration_context: Some(redacted_ctx),
        };

        let prompt = review::build_review_prompt(&req);

        for model in &pipeline_config.models {
            match reviewer.review(&prompt, model) {
                Ok(response) => {
                    match review::parse_llm_response(&response, model) {
                        Ok(findings) => all_sources.push(findings),
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

    // Calibrate using feedback precedent
    let final_findings = if pipeline_config.calibrate && !pipeline_config.feedback.is_empty() {
        let cal_result = calibrator::calibrate(
            merged,
            &pipeline_config.feedback,
            &CalibratorConfig::default(),
        );
        if cal_result.suppressed > 0 || cal_result.boosted > 0 {
            eprintln!(
                "Calibrator: {} suppressed, {} boosted (from {} feedback entries)",
                cal_result.suppressed, cal_result.boosted, pipeline_config.feedback.len()
            );
        }
        cal_result.findings
    } else {
        merged
    };

    // Auto-calibration: use a second LLM pass to triage findings and record verdicts
    if pipeline_config.auto_calibrate && !final_findings.is_empty() {
        if let (Some(reviewer), Some(store_path)) = (llm, &pipeline_config.feedback_store) {
            let model = pipeline_config.models.first().map(|s| s.as_str()).unwrap_or("gpt-5.4");
            let store = crate::feedback::FeedbackStore::new(store_path.clone());
            match crate::auto_calibrate::auto_calibrate(
                &final_findings, source, &file_str, reviewer, model, &store,
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
    })
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Severity, Source};
    use std::path::PathBuf;

    struct FakeLlmReviewer {
        response: String,
    }

    impl FakeLlmReviewer {
        fn with_findings(findings_json: &str) -> Self {
            Self { response: findings_json.into() }
        }

        fn empty() -> Self {
            Self { response: "[]".into() }
        }

        fn failing() -> Self {
            Self { response: "not valid json".into() }
        }
    }

    impl LlmReviewer for FakeLlmReviewer {
        fn review(&self, _prompt: &str, _model: &str) -> anyhow::Result<String> {
            Ok(self.response.clone())
        }
    }

    struct FailingLlmReviewer;
    impl LlmReviewer for FailingLlmReviewer {
        fn review(&self, _prompt: &str, _model: &str) -> anyhow::Result<String> {
            anyhow::bail!("network error")
        }
    }

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
        let llm = FakeLlmReviewer::with_findings(llm_response);
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
        let llm = FakeLlmReviewer::empty();
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
        let llm = FailingLlmReviewer;
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
        let llm = FakeLlmReviewer::failing();
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
        let llm = FakeLlmReviewer::with_findings(llm_response);
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
        let llm = FakeLlmReviewer::empty();
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
}
