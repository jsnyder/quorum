//! Skill matrix execution orchestrator (Foundation C, issue #410).
//!
//! Takes a list of skills, files, and models, expands a
//! (skill x model x file) matrix, executes each cell through the LLM,
//! and collects results with audit logging.
//!
//! Defines its own `TokenUsage`, `LlmResponse`, and `LlmReviewer` trait
//! so the lib crate is self-contained. The binary's `llm_client` and
//! `pipeline` modules use compatible types; the integration layer in
//! `main.rs` bridges them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use chrono::Utc;

use crate::finding::{Finding, Severity};
use crate::model_family::{self, ModelFamily, PromptOverride, SkillPrompts, select_prompt};
use crate::skill_audit::{
    AuditWriter, AxisSelectionSource, ExitStatus, FailureReason, SkillInvocationRecord,
};
use crate::skill_manifest::{CapabilityMode, LoadedSkill, Prompts, TrustTier};
use crate::skill_output::{ParseErrorClass, SkillResponseOutcome, classify_response};
use crate::skill_prompt_defense::{
    BASE_SYSTEM_PROMPT, sanitize_output, wrap_code_to_review, wrap_skill_instructions,
};

// ---------------------------------------------------------------------------
// LLM types (lib-local, mirrors llm_client.rs / pipeline.rs from the binary)
// ---------------------------------------------------------------------------

/// Token usage reported by the LLM API.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// Combined response from an LLM API call.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub usage: Option<TokenUsage>,
}

/// Trait for LLM review -- allows testing with fake implementations.
pub trait LlmReviewer: Send + Sync {
    fn review(&self, prompt: &str, model: &str, system_prompt: &str)
    -> anyhow::Result<LlmResponse>;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const OUTPUT_SCHEMA: &str = "Respond with a JSON array of findings. Each finding \
    must have: title (string), description (string), severity \
    (critical/high/medium/low/info), category (string), line_start (u32), \
    line_end (u32), evidence (string[]).";

// ---------------------------------------------------------------------------
// BudgetExhausted
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct BudgetExhausted;

// ---------------------------------------------------------------------------
// BudgetTracker
// ---------------------------------------------------------------------------

pub(crate) struct BudgetTracker {
    calls_used: AtomicU64,
    tokens_used: AtomicU64,
    max_calls: u64,
    max_tokens: u64,
}

impl BudgetTracker {
    pub(crate) fn new(max_calls: u64, max_tokens: u64) -> Self {
        Self {
            calls_used: AtomicU64::new(0),
            tokens_used: AtomicU64::new(0),
            max_calls,
            max_tokens,
        }
    }

    pub(crate) fn try_reserve_call(&self) -> Result<(), BudgetExhausted> {
        if self.max_calls == 0 {
            self.calls_used.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        let prev = self.calls_used.fetch_add(1, Ordering::Relaxed);
        if prev >= self.max_calls {
            self.calls_used.fetch_sub(1, Ordering::Relaxed);
            return Err(BudgetExhausted);
        }
        Ok(())
    }

    pub(crate) fn record_tokens(&self, n: u64) {
        self.tokens_used.fetch_add(n, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> (u64, u64) {
        (
            self.calls_used.load(Ordering::Relaxed),
            self.tokens_used.load(Ordering::Relaxed),
        )
    }

    fn tokens_exceeded(&self) -> bool {
        self.max_tokens > 0 && self.tokens_used.load(Ordering::Relaxed) >= self.max_tokens
    }
}

// ---------------------------------------------------------------------------
// SkillExecutorConfig
// ---------------------------------------------------------------------------

pub struct SkillExecutorConfig {
    pub run_id: String,
    pub axis_selection_source: AxisSelectionSource,
    pub global_models: Vec<String>,
    pub ensemble_pool: Vec<String>,
    pub ensemble: bool,
    pub max_tokens_per_review: u64,
    pub max_calls_per_review: u64,
    pub audit_writer: Option<Arc<AuditWriter<SkillInvocationRecord>>>,
}

// ---------------------------------------------------------------------------
// CellSpec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CellSpec {
    pub skill: LoadedSkill,
    pub model: String,
    pub file_path: String,
    pub file_sha256: String,
    pub code: String,
}

// ---------------------------------------------------------------------------
// CellResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CellResult {
    pub skill_run_id: String,
    pub findings: Vec<Finding>,
    pub usage: TokenUsage,
    pub duration_ms: u64,
    pub model_was_fallback: bool,
    pub actual_model: String,
    pub exit_status: ExitStatus,
    pub failure_reason: Option<FailureReason>,
    pub parse_error_class: Option<ParseErrorClass>,
    pub findings_clamped: u32,
    pub findings_dropped_invalid_json: u32,
    pub prompt_sha256: String,
    pub prompt_family: String,
}

// ---------------------------------------------------------------------------
// expand_matrix (pub, pure)
// ---------------------------------------------------------------------------

pub fn expand_matrix(
    skills: &[LoadedSkill],
    files: &[(String, String, String)],
    config: &SkillExecutorConfig,
) -> Vec<CellSpec> {
    let mut cells = Vec::new();

    for skill in skills {
        let models: Vec<String> = if let Some(ref preferred) = skill.manifest.preferred_model {
            vec![preferred.clone()]
        } else if config.ensemble && !config.ensemble_pool.is_empty() {
            config.ensemble_pool.clone()
        } else {
            config.global_models.clone()
        };

        for model in &models {
            for (path, sha, code) in files {
                cells.push(CellSpec {
                    skill: skill.clone(),
                    model: model.clone(),
                    file_path: path.clone(),
                    file_sha256: sha.clone(),
                    code: code.clone(),
                });
            }
        }
    }

    cells
}

// ---------------------------------------------------------------------------
// execute_cell (pub(crate), sync)
// ---------------------------------------------------------------------------

pub(crate) fn execute_cell(
    cell: &CellSpec,
    reviewer: &dyn LlmReviewer,
    budget: &BudgetTracker,
) -> CellResult {
    let skill_run_id = ulid::Ulid::new().to_string();
    let zero_usage = TokenUsage::default();

    // Step 1: budget check
    if let Err(BudgetExhausted) = budget.try_reserve_call() {
        return CellResult {
            skill_run_id,
            findings: vec![],
            usage: zero_usage,
            duration_ms: 0,
            model_was_fallback: false,
            actual_model: cell.model.clone(),
            exit_status: ExitStatus::Error,
            failure_reason: Some(FailureReason::BudgetCapHit),
            parse_error_class: None,
            findings_clamped: 0,
            findings_dropped_invalid_json: 0,
            prompt_sha256: String::new(),
            prompt_family: String::new(),
        };
    }

    // Step 2-7: prompt assembly
    let skill_prompts = manifest_prompts_to_skill_prompts(&cell.skill.manifest.prompts);
    let family = model_family::detect_family(&cell.model);
    let selected = select_prompt(&skill_prompts, family);
    let wrapped_skill = wrap_skill_instructions(selected);
    let line_count = cell.code.lines().count().max(1) as u32;
    let wrapped_code = wrap_code_to_review(
        &cell.code,
        &cell.file_path,
        &cell.file_sha256,
        1,
        line_count,
    );
    let assembled = model_family::assemble_prompt(
        BASE_SYSTEM_PROMPT,
        &wrapped_skill,
        &wrapped_code,
        OUTPUT_SCHEMA,
        family,
    );

    let prompt_family = family.as_str().to_owned();
    let prompt_sha256 = assembled.prompt_sha256.clone();

    // Step 8: call the LLM
    let start = Instant::now();
    let review_result = reviewer.review(
        &assembled.user_message,
        &cell.model,
        &assembled.system_message,
    );
    let duration_ms = start.elapsed().as_millis() as u64;

    let (raw_content, mut usage) = match review_result {
        Ok(LlmResponse { content, usage }) => (content, usage.unwrap_or_default()),
        Err(_) => {
            return CellResult {
                skill_run_id,
                findings: vec![],
                usage: zero_usage,
                duration_ms,
                model_was_fallback: false,
                actual_model: cell.model.clone(),
                exit_status: ExitStatus::Error,
                failure_reason: Some(FailureReason::NetworkError),
                parse_error_class: None,
                findings_clamped: 0,
                findings_dropped_invalid_json: 0,
                prompt_sha256,
                prompt_family,
            };
        }
    };

    // Step 14: record tokens
    budget.record_tokens(usage.total());

    // Step 9: classify response
    let outcome = classify_response(&raw_content, None, &cell.model);

    // Handle Retry: one retry with continuation prompt.
    // The tuple tracks (findings, parse_error_class, findings_dropped, exit_status, failure_reason).
    let (
        mut findings,
        parse_error_class,
        findings_dropped_invalid_json,
        exit_status,
        failure_reason,
    ) = match outcome {
        SkillResponseOutcome::Ok {
            findings,
            parse_warnings: _,
        } => (findings, None, 0_u32, ExitStatus::Ok, None),
        SkillResponseOutcome::ParseError { class, .. } => {
            (vec![], Some(class), 0, ExitStatus::Error, None)
        }
        SkillResponseOutcome::Retry {
            class: _,
            continuation_prompt,
        } => {
            let retry_prompt = format!("{}\n{}", assembled.user_message, continuation_prompt);
            let retry_result =
                reviewer.review(&retry_prompt, &cell.model, &assembled.system_message);
            match retry_result {
                Ok(LlmResponse {
                    content,
                    usage: retry_usage,
                }) => {
                    let ru = retry_usage.unwrap_or_default();
                    budget.record_tokens(ru.total());
                    usage.prompt_tokens += ru.prompt_tokens;
                    usage.completion_tokens += ru.completion_tokens;
                    usage.cached_tokens += ru.cached_tokens;
                    match classify_response(&content, None, &cell.model) {
                        SkillResponseOutcome::Ok {
                            findings,
                            parse_warnings: _,
                        } => (findings, None, 0, ExitStatus::Ok, None),
                        SkillResponseOutcome::ParseError { class, .. } => {
                            (vec![], Some(class), 0, ExitStatus::Error, None)
                        }
                        SkillResponseOutcome::Retry { class, .. } => {
                            (vec![], Some(class), 0, ExitStatus::Error, None)
                        }
                    }
                }
                Err(_) => (
                    vec![],
                    None,
                    0,
                    ExitStatus::Error,
                    Some(FailureReason::NetworkError),
                ),
            }
        }
    };

    // Step 11: sanitize finding fields
    sanitize_finding_fields(&mut findings);

    // Step 12: clamp severity
    let findings_clamped = clamp_findings(&mut findings, &cell.skill.manifest.max_severity);

    // Step 13: tag findings
    tag_findings(&mut findings, &cell.skill, family, &skill_run_id);

    CellResult {
        skill_run_id,
        findings,
        usage,
        duration_ms,
        model_was_fallback: false,
        actual_model: cell.model.clone(),
        exit_status,
        failure_reason,
        parse_error_class,
        findings_clamped,
        findings_dropped_invalid_json,
        prompt_sha256,
        prompt_family,
    }
}

// ---------------------------------------------------------------------------
// execute_cell_with_fallback (pub(crate))
// ---------------------------------------------------------------------------

pub(crate) fn execute_cell_with_fallback(
    cell: &CellSpec,
    reviewer: &dyn LlmReviewer,
    budget: &BudgetTracker,
) -> CellResult {
    let mut result = execute_cell(cell, reviewer, budget);

    if result.exit_status == ExitStatus::Error
        && result.failure_reason != Some(FailureReason::BudgetCapHit)
        && let Some(ref fallbacks) = cell.skill.manifest.fallback_models
    {
        for fallback_model in fallbacks {
            let mut fallback_cell = cell.clone();
            fallback_cell.model = fallback_model.clone();
            let fallback_result = execute_cell(&fallback_cell, reviewer, budget);
            if fallback_result.exit_status == ExitStatus::Ok {
                result = fallback_result;
                result.model_was_fallback = true;
                result.actual_model = fallback_model.clone();
                return result;
            }
            if fallback_result.failure_reason == Some(FailureReason::BudgetCapHit) {
                return fallback_result;
            }
            result = fallback_result;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// execute_matrix (pub, sync)
// ---------------------------------------------------------------------------

pub fn execute_matrix(
    skills: &[LoadedSkill],
    files: &[(String, String, String)],
    reviewer: &dyn LlmReviewer,
    config: &SkillExecutorConfig,
) -> Vec<CellResult> {
    let cells = expand_matrix(skills, files, config);
    let budget = BudgetTracker::new(config.max_calls_per_review, config.max_tokens_per_review);
    let mut results = Vec::with_capacity(cells.len());

    for cell in &cells {
        if budget.tokens_exceeded() {
            let skill_run_id = ulid::Ulid::new().to_string();
            let budget_result = CellResult {
                skill_run_id,
                findings: vec![],
                usage: TokenUsage::default(),
                duration_ms: 0,
                model_was_fallback: false,
                actual_model: cell.model.clone(),
                exit_status: ExitStatus::Error,
                failure_reason: Some(FailureReason::BudgetCapHit),
                parse_error_class: None,
                findings_clamped: 0,
                findings_dropped_invalid_json: 0,
                prompt_sha256: String::new(),
                prompt_family: String::new(),
            };
            if let Some(ref writer) = config.audit_writer {
                let record = build_invocation_record(cell, &budget_result, config);
                if let Err(e) = writer.write(&record) {
                    tracing::warn!(
                        target: "quorum::skill_executor",
                        error = %e,
                        "failed to write budget-capped audit record"
                    );
                }
            }
            results.push(budget_result);
            continue;
        }

        let result = execute_cell_with_fallback(cell, reviewer, &budget);

        if let Some(ref writer) = config.audit_writer {
            let record = build_invocation_record(cell, &result, config);
            if let Err(e) = writer.write(&record) {
                tracing::warn!(
                    target: "quorum::skill_executor",
                    error = %e,
                    "failed to write skill invocation audit record"
                );
            }
        }

        results.push(result);
    }

    results
}

// ---------------------------------------------------------------------------
// Helper: manifest_prompts_to_skill_prompts
// ---------------------------------------------------------------------------

fn manifest_prompts_to_skill_prompts(prompts: &Prompts) -> SkillPrompts {
    SkillPrompts {
        primary: prompts.primary.clone(),
        anthropic: prompts.anthropic.as_ref().and_then(|p| {
            p.override_prompt.as_ref().map(|text| PromptOverride {
                override_text: text.clone(),
            })
        }),
        openai: prompts.openai.as_ref().and_then(|p| {
            p.override_prompt.as_ref().map(|text| PromptOverride {
                override_text: text.clone(),
            })
        }),
        google: prompts.google.as_ref().and_then(|p| {
            p.override_prompt.as_ref().map(|text| PromptOverride {
                override_text: text.clone(),
            })
        }),
    }
}

// ---------------------------------------------------------------------------
// Helper: clamp_findings
// ---------------------------------------------------------------------------

fn clamp_findings(findings: &mut [Finding], max_severity: &Severity) -> u32 {
    let mut clamped = 0_u32;
    for f in findings.iter_mut() {
        if f.severity > *max_severity {
            f.clamped_from_severity = Some(f.severity.clone());
            f.severity = max_severity.clone();
            clamped += 1;
        }
    }
    clamped
}

// ---------------------------------------------------------------------------
// Helper: tag_findings
// ---------------------------------------------------------------------------

fn tag_findings(
    findings: &mut [Finding],
    skill: &LoadedSkill,
    family: ModelFamily,
    skill_run_id: &str,
) {
    for f in findings.iter_mut() {
        f.originating_skill = Some(skill.manifest.name.clone());
        f.skill_version = Some(skill.manifest.version.clone());
        f.manifest_sha256 = Some(skill.manifest_sha256.clone());
        f.prompt_family = Some(family.as_str().to_owned());
        f.skill_run_id = Some(skill_run_id.to_owned());
    }
}

// ---------------------------------------------------------------------------
// Helper: sanitize_finding_fields
// ---------------------------------------------------------------------------

fn sanitize_finding_fields(findings: &mut [Finding]) {
    for f in findings.iter_mut() {
        f.title = sanitize_output(&f.title);
        f.description = sanitize_output(&f.description);
        f.evidence = f.evidence.iter().map(|e| sanitize_output(e)).collect();
        if let Some(ref s) = f.suggested_fix {
            f.suggested_fix = Some(sanitize_output(s));
        }
        if let Some(ref s) = f.reasoning {
            f.reasoning = Some(sanitize_output(s));
        }
        if let Some(ref s) = f.canonical_pattern {
            f.canonical_pattern = Some(sanitize_output(s));
        }
        if let Some(ref s) = f.based_on_excerpt {
            f.based_on_excerpt = Some(sanitize_output(s));
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: build_invocation_record
// ---------------------------------------------------------------------------

fn build_invocation_record(
    cell: &CellSpec,
    result: &CellResult,
    config: &SkillExecutorConfig,
) -> SkillInvocationRecord {
    let trust_tier_str = match cell.skill.trust_tier {
        TrustTier::Bundled => "bundled",
        TrustTier::User => "user",
        TrustTier::Untrusted => "untrusted",
    };
    let capability_mode_str = match cell.skill.manifest.capability.mode {
        CapabilityMode::Pure => "pure",
        CapabilityMode::Indexed => "indexed",
        CapabilityMode::Toolful => "toolful",
        CapabilityMode::BinaryAnalyzer => "binary-analyzer",
        CapabilityMode::BinaryToolServer => "binary-tool-server",
    };

    SkillInvocationRecord {
        skill_run_id: result.skill_run_id.clone(),
        run_id: config.run_id.clone(),
        ts: Utc::now(),
        skill_name: cell.skill.manifest.name.clone(),
        skill_version: cell.skill.manifest.version.clone(),
        manifest_sha256: cell.skill.manifest_sha256.clone(),
        prompt_family: result.prompt_family.clone(),
        prompt_sha256: result.prompt_sha256.clone(),
        model: result.actual_model.clone(),
        model_was_fallback: result.model_was_fallback,
        axis_selection_source: config.axis_selection_source.clone(),
        capability_mode: capability_mode_str.to_owned(),
        trust_tier: trust_tier_str.to_owned(),
        file_path: cell.file_path.clone(),
        file_sha256: cell.file_sha256.clone(),
        tokens_in: result.usage.prompt_tokens,
        tokens_out: result.usage.completion_tokens,
        tokens_cache_read: result.usage.cached_tokens,
        llm_cache_hit: result.usage.cached_tokens > 0,
        duration_ms: result.duration_ms,
        findings_emitted: result.findings.len() as u32,
        findings_clamped: result.findings_clamped,
        findings_dropped_invalid_json: result.findings_dropped_invalid_json,
        parse_error_class: result.parse_error_class,
        exit_status: result.exit_status.clone(),
        failure_reason: result.failure_reason.clone(),
        calibrator_suppressions: 0,
        calibrator_precedents_matched: 0,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{FindingBuilder, Severity};
    use crate::skill_audit::{AuditReader, AxisSelectionSource, ExitStatus, FailureReason};
    use crate::skill_manifest::{
        Axis, Capability, CapabilityMode, Prompts, ProviderPrompt, SkillManifest, TrustTier,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // -----------------------------------------------------------------------
    // MockReviewer
    // -----------------------------------------------------------------------

    struct MockResponse {
        content: String,
        usage: TokenUsage,
        error: bool,
    }

    struct MockReviewer {
        responses: Mutex<HashMap<String, MockResponse>>,
        default_response: Mutex<Option<MockResponse>>,
    }

    impl MockReviewer {
        fn new() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
                default_response: Mutex::new(None),
            }
        }

        fn with_default(content: &str, usage: TokenUsage) -> Self {
            let reviewer = Self::new();
            *reviewer.default_response.lock().unwrap() = Some(MockResponse {
                content: content.to_owned(),
                usage,
                error: false,
            });
            reviewer
        }

        fn with_default_error() -> Self {
            let reviewer = Self::new();
            *reviewer.default_response.lock().unwrap() = Some(MockResponse {
                content: String::new(),
                usage: TokenUsage::default(),
                error: true,
            });
            reviewer
        }

        fn add_response(&self, model: &str, content: &str, usage: TokenUsage) {
            self.responses.lock().unwrap().insert(
                model.to_owned(),
                MockResponse {
                    content: content.to_owned(),
                    usage,
                    error: false,
                },
            );
        }

        fn add_error_response(&self, model: &str) {
            self.responses.lock().unwrap().insert(
                model.to_owned(),
                MockResponse {
                    content: String::new(),
                    usage: TokenUsage::default(),
                    error: true,
                },
            );
        }
    }

    impl LlmReviewer for MockReviewer {
        fn review(
            &self,
            _prompt: &str,
            model: &str,
            _system_prompt: &str,
        ) -> anyhow::Result<LlmResponse> {
            let responses = self.responses.lock().unwrap();
            if let Some(resp) = responses.get(model) {
                if resp.error {
                    return Err(anyhow::anyhow!("mock error for model {}", model));
                }
                return Ok(LlmResponse {
                    content: resp.content.clone(),
                    usage: Some(resp.usage.clone()),
                });
            }
            drop(responses);

            let default = self.default_response.lock().unwrap();
            if let Some(ref resp) = *default {
                if resp.error {
                    return Err(anyhow::anyhow!("mock default error"));
                }
                return Ok(LlmResponse {
                    content: resp.content.clone(),
                    usage: Some(resp.usage.clone()),
                });
            }

            Ok(LlmResponse {
                content: "[]".to_owned(),
                usage: Some(TokenUsage::default()),
            })
        }
    }

    // -----------------------------------------------------------------------
    // sample_skill helper
    // -----------------------------------------------------------------------

    fn sample_skill(
        name: &str,
        preferred_model: Option<&str>,
        max_severity: Severity,
    ) -> LoadedSkill {
        LoadedSkill {
            manifest: SkillManifest {
                name: name.to_owned(),
                version: "1.0.0".to_owned(),
                display_name: format!("Test {name}"),
                description: "A test skill.".to_owned(),
                preferred_model: preferred_model.map(String::from),
                fallback_models: None,
                calibration_namespace: None,
                axis: Axis::Security,
                max_severity,
                target_findings: None,
                capability: Capability {
                    mode: CapabilityMode::Pure,
                },
                prompts: Prompts {
                    primary: "Review for issues.".to_owned(),
                    anthropic: None,
                    openai: None,
                    google: None,
                },
                checklist: vec![],
                ast_rules: vec![],
            },
            trust_tier: TrustTier::Bundled,
            source_path: PathBuf::from(format!("/skills/{name}.toml")),
            manifest_sha256: "a".repeat(64),
        }
    }

    fn sample_skill_with_fallbacks(
        name: &str,
        preferred_model: Option<&str>,
        fallback_models: Vec<String>,
        max_severity: Severity,
    ) -> LoadedSkill {
        let mut skill = sample_skill(name, preferred_model, max_severity);
        skill.manifest.fallback_models = Some(fallback_models);
        skill
    }

    /// Build a mock LLM response in the shape the skill PROMPTS actually
    /// request -- title/description/severity/category/line_start/line_end/
    /// evidence, and nothing else.
    ///
    /// This previously serialized an internal `Finding` via `FindingBuilder`
    /// and fed it back to the parser, so every executor test round-tripped
    /// `Finding -> JSON -> Finding` and passed trivially. That blind spot is
    /// why nothing caught the parser targeting `Finding` (whose `source`,
    /// `evidence`, `calibrator_action` and `similar_precedent` have no serde
    /// default) instead of `LlmFinding`: real model output failed as
    /// `wrong_schema` while the suite stayed green. Keep this emitting only
    /// prompt-declared fields -- if it drifts back toward `Finding`, these
    /// tests stop protecting anything.
    fn make_finding_json(title: &str) -> String {
        serde_json::json!({
            "title": title,
            "description": "mock finding body",
            "severity": "high",
            "category": "security",
            "line_start": 10,
            "line_end": 20,
            "evidence": ["mock evidence line"],
        })
        .to_string()
    }

    fn make_findings_json(titles: &[&str]) -> String {
        let items: Vec<String> = titles.iter().map(|t| make_finding_json(t)).collect();
        format!("[{}]", items.join(","))
    }

    fn default_config() -> SkillExecutorConfig {
        SkillExecutorConfig {
            run_id: "test-run-001".to_owned(),
            axis_selection_source: AxisSelectionSource::Default,
            global_models: vec!["gpt-5.4".to_owned()],
            ensemble_pool: vec!["gpt-5.4".to_owned(), "claude-opus-4-7".to_owned()],
            ensemble: false,
            max_tokens_per_review: 0,
            max_calls_per_review: 0,
            audit_writer: None,
        }
    }

    fn sample_files() -> Vec<(String, String, String)> {
        vec![(
            "src/main.rs".to_owned(),
            "abc123".to_owned(),
            "fn main() {}".to_owned(),
        )]
    }

    fn sample_usage() -> TokenUsage {
        TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            cached_tokens: 10,
        }
    }

    // =======================================================================
    // Matrix expansion tests
    // =======================================================================

    #[test]
    fn single_skill_single_model_single_file() {
        let skills = vec![sample_skill("security", None, Severity::Critical)];
        let files = sample_files();
        let config = default_config();
        let cells = expand_matrix(&skills, &files, &config);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].model, "gpt-5.4");
        assert_eq!(cells[0].file_path, "src/main.rs");
    }

    #[test]
    fn preferred_model_overrides_global() {
        let skills = vec![sample_skill(
            "security",
            Some("claude-opus-4-7"),
            Severity::Critical,
        )];
        let files = sample_files();
        let config = default_config();
        let cells = expand_matrix(&skills, &files, &config);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].model, "claude-opus-4-7");
    }

    #[test]
    fn ensemble_expands_unpinned_skills() {
        let skills = vec![sample_skill("security", None, Severity::Critical)];
        let files = sample_files();
        let mut config = default_config();
        config.ensemble = true;
        let cells = expand_matrix(&skills, &files, &config);
        assert_eq!(cells.len(), 2);
        let models: Vec<&str> = cells.iter().map(|c| c.model.as_str()).collect();
        assert!(models.contains(&"gpt-5.4"));
        assert!(models.contains(&"claude-opus-4-7"));
    }

    #[test]
    fn ensemble_pinned_skill_not_expanded() {
        let skills = vec![sample_skill(
            "security",
            Some("gemini-2.5-pro"),
            Severity::Critical,
        )];
        let files = sample_files();
        let mut config = default_config();
        config.ensemble = true;
        let cells = expand_matrix(&skills, &files, &config);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].model, "gemini-2.5-pro");
    }

    #[test]
    fn multiple_files_multiplies() {
        let skills = vec![
            sample_skill("security", None, Severity::Critical),
            sample_skill("performance", None, Severity::High),
        ];
        let files = vec![
            ("a.rs".to_owned(), "sha1".to_owned(), "code1".to_owned()),
            ("b.rs".to_owned(), "sha2".to_owned(), "code2".to_owned()),
        ];
        let config = default_config();
        let cells = expand_matrix(&skills, &files, &config);
        assert_eq!(cells.len(), 4);
    }

    #[test]
    fn empty_skills_empty_matrix() {
        let skills: Vec<LoadedSkill> = vec![];
        let files = sample_files();
        let config = default_config();
        let cells = expand_matrix(&skills, &files, &config);
        assert!(cells.is_empty());
    }

    // =======================================================================
    // Cell execution tests
    // =======================================================================

    #[test]
    fn valid_json_array_produces_findings() {
        let json = make_findings_json(&["SQL injection"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Ok);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].title, "SQL injection");
    }

    #[test]
    fn empty_response_returns_parse_error() {
        let reviewer = MockReviewer::with_default("", sample_usage());
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Error);
        assert_eq!(result.parse_error_class, Some(ParseErrorClass::Empty));
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let reviewer = MockReviewer::with_default("not json at all", sample_usage());
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Error);
        assert_eq!(result.parse_error_class, Some(ParseErrorClass::NotJson));
    }

    #[test]
    fn severity_clamped_above_max() {
        let json = make_findings_json(&["Critical finding"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skill = sample_skill("security", None, Severity::Medium);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Ok);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::Medium);
        assert_eq!(
            result.findings[0].clamped_from_severity,
            Some(Severity::High)
        );
        assert_eq!(result.findings_clamped, 1);
    }

    #[test]
    fn findings_tagged_with_identity() {
        let json = make_findings_json(&["Test finding"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(
            result.findings[0].originating_skill.as_deref(),
            Some("security")
        );
        assert_eq!(result.findings[0].skill_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            result.findings[0].manifest_sha256.as_deref(),
            Some(&"a".repeat(64)[..])
        );
        assert!(result.findings[0].prompt_family.is_some());
        assert!(result.findings[0].skill_run_id.is_some());
    }

    #[test]
    fn output_sanitized() {
        // Prompt-shaped response carrying ANSI escapes in the title. Built as
        // raw JSON, not by serializing a `Finding` -- see make_finding_json.
        let json = serde_json::json!([{
            "title": "\u{1b}[31mRed Alert\u{1b}[0m",
            "description": "mock finding body",
            "severity": "high",
            "category": "security",
            "line_start": 1,
            "line_end": 1,
            "evidence": [],
        }])
        .to_string();
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Ok);
        assert_eq!(result.findings.len(), 1);
        assert!(
            !result.findings[0].title.contains("\x1b["),
            "ANSI escapes must be stripped from title"
        );
        assert_eq!(result.findings[0].title, "Red Alert");
    }

    #[test]
    fn prompt_uses_family_override() {
        let mut skill = sample_skill("security", None, Severity::Critical);
        skill.manifest.prompts.anthropic = Some(ProviderPrompt {
            override_prompt: Some("Anthropic-specific prompt.".to_owned()),
        });

        let prompts = manifest_prompts_to_skill_prompts(&skill.manifest.prompts);
        let family = model_family::detect_family("claude-opus-4-7");
        let selected = select_prompt(&prompts, family);
        assert_eq!(selected, "Anthropic-specific prompt.");
    }

    #[test]
    fn budget_exhausted_returns_cap_hit() {
        let json = make_findings_json(&["Finding"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(1, 0);
        // Use the one allowed call
        let _ = execute_cell(&cell, &reviewer, &budget);
        // Second call should be budget-capped
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Error);
        assert_eq!(result.failure_reason, Some(FailureReason::BudgetCapHit));
    }

    // =======================================================================
    // Fallback tests
    // =======================================================================

    #[test]
    fn primary_succeeds_no_fallback() {
        let json = make_findings_json(&["Finding"]);
        let reviewer = MockReviewer::new();
        reviewer.add_response("gpt-5.4", &json, sample_usage());
        let skill = sample_skill_with_fallbacks(
            "security",
            None,
            vec!["claude-opus-4-7".to_owned()],
            Severity::Critical,
        );
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell_with_fallback(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Ok);
        assert!(!result.model_was_fallback);
        assert_eq!(result.actual_model, "gpt-5.4");
    }

    #[test]
    fn primary_fails_fallback_succeeds() {
        let json = make_findings_json(&["Finding"]);
        let reviewer = MockReviewer::new();
        reviewer.add_error_response("gpt-5.4");
        reviewer.add_response("claude-opus-4-7", &json, sample_usage());
        let skill = sample_skill_with_fallbacks(
            "security",
            None,
            vec!["claude-opus-4-7".to_owned()],
            Severity::Critical,
        );
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell_with_fallback(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Ok);
        assert!(result.model_was_fallback);
        assert_eq!(result.actual_model, "claude-opus-4-7");
    }

    #[test]
    fn all_fail_returns_last_error() {
        let reviewer = MockReviewer::new();
        reviewer.add_error_response("gpt-5.4");
        reviewer.add_error_response("claude-opus-4-7");
        reviewer.add_error_response("gemini-2.5-pro");
        let skill = sample_skill_with_fallbacks(
            "security",
            None,
            vec!["claude-opus-4-7".to_owned(), "gemini-2.5-pro".to_owned()],
            Severity::Critical,
        );
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell_with_fallback(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Error);
    }

    #[test]
    fn no_fallback_models_no_retry() {
        let reviewer = MockReviewer::with_default_error();
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(0, 0);
        let result = execute_cell_with_fallback(&cell, &reviewer, &budget);
        assert_eq!(result.exit_status, ExitStatus::Error);
    }

    // =======================================================================
    // Budget tests
    // =======================================================================

    #[test]
    fn calls_cap_enforced() {
        let budget = BudgetTracker::new(2, 0);
        assert!(budget.try_reserve_call().is_ok());
        assert!(budget.try_reserve_call().is_ok());
        assert!(budget.try_reserve_call().is_err());
        let (calls, _) = budget.snapshot();
        assert_eq!(calls, 2);
    }

    #[test]
    fn tokens_cap_zero_unlimited() {
        let budget = BudgetTracker::new(0, 0);
        for _ in 0..100 {
            assert!(budget.try_reserve_call().is_ok());
        }
        budget.record_tokens(1_000_000);
        assert!(!budget.tokens_exceeded());
    }

    #[test]
    fn tracker_atomic_increment() {
        let budget = BudgetTracker::new(0, 0);
        budget.record_tokens(100);
        budget.record_tokens(200);
        let (_, tokens) = budget.snapshot();
        assert_eq!(tokens, 300);
    }

    #[test]
    fn exhausted_sets_failure_reason() {
        let json = make_findings_json(&["Finding"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let budget = BudgetTracker::new(1, 0);
        let _ = execute_cell(&cell, &reviewer, &budget);
        let result = execute_cell(&cell, &reviewer, &budget);
        assert_eq!(result.failure_reason, Some(FailureReason::BudgetCapHit));
        assert_eq!(result.exit_status, ExitStatus::Error);
    }

    // =======================================================================
    // Audit record tests
    // =======================================================================

    #[test]
    fn record_populated_from_result() {
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let result = CellResult {
            skill_run_id: "run-123".to_owned(),
            findings: vec![FindingBuilder::new().build()],
            usage: TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                cached_tokens: 10,
            },
            duration_ms: 2500,
            model_was_fallback: false,
            actual_model: "gpt-5.4".to_owned(),
            exit_status: ExitStatus::Ok,
            failure_reason: None,
            parse_error_class: None,
            findings_clamped: 0,
            findings_dropped_invalid_json: 0,
            prompt_sha256: "sha-prompt".to_owned(),
            prompt_family: "openai".to_owned(),
        };
        let config = default_config();
        let record = build_invocation_record(&cell, &result, &config);
        assert_eq!(record.skill_run_id, "run-123");
        assert_eq!(record.run_id, "test-run-001");
        assert_eq!(record.skill_name, "security");
        assert_eq!(record.model, "gpt-5.4");
        assert_eq!(record.tokens_in, 100);
        assert_eq!(record.tokens_out, 50);
        assert_eq!(record.tokens_cache_read, 10);
        assert_eq!(record.duration_ms, 2500);
        assert_eq!(record.findings_emitted, 1);
        assert_eq!(record.exit_status, ExitStatus::Ok);
        assert_eq!(record.trust_tier, "bundled");
        assert_eq!(record.capability_mode, "pure");
    }

    #[test]
    fn record_failure_fields() {
        let skill = sample_skill("security", None, Severity::Critical);
        let cell = CellSpec {
            skill,
            model: "gpt-5.4".to_owned(),
            file_path: "src/main.rs".to_owned(),
            file_sha256: "abc123".to_owned(),
            code: "fn main() {}".to_owned(),
        };
        let result = CellResult {
            skill_run_id: "run-456".to_owned(),
            findings: vec![],
            usage: TokenUsage::default(),
            duration_ms: 100,
            model_was_fallback: false,
            actual_model: "gpt-5.4".to_owned(),
            exit_status: ExitStatus::Error,
            failure_reason: Some(FailureReason::ModelTimeout),
            parse_error_class: Some(ParseErrorClass::Truncated),
            findings_clamped: 0,
            findings_dropped_invalid_json: 0,
            prompt_sha256: String::new(),
            prompt_family: String::new(),
        };
        let config = default_config();
        let record = build_invocation_record(&cell, &result, &config);
        assert_eq!(record.exit_status, ExitStatus::Error);
        assert_eq!(record.failure_reason, Some(FailureReason::ModelTimeout));
        assert_eq!(record.parse_error_class, Some(ParseErrorClass::Truncated));
    }

    #[test]
    fn execute_matrix_writes_records() {
        let dir = tempfile::tempdir().unwrap();
        let audit_path = dir.path().join("invocations.jsonl");
        let writer = Arc::new(AuditWriter::<SkillInvocationRecord>::new(
            audit_path.clone(),
        ));

        let json = make_findings_json(&["Finding A"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skills = vec![sample_skill("security", None, Severity::Critical)];
        let files = sample_files();
        let mut config = default_config();
        config.audit_writer = Some(writer);

        let results = execute_matrix(&skills, &files, &reviewer, &config);
        assert_eq!(results.len(), 1);

        let reader = AuditReader::<SkillInvocationRecord>::new(audit_path);
        let (records, stats) = reader.load_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(stats.parsed_ok, 1);
        assert_eq!(records[0].skill_name, "security");
    }

    // =======================================================================
    // Full matrix tests
    // =======================================================================

    #[test]
    fn execute_matrix_collects_all_results() {
        let json = make_findings_json(&["Finding"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skills = vec![
            sample_skill("security", None, Severity::Critical),
            sample_skill("performance", None, Severity::High),
        ];
        let files = sample_files();
        let config = default_config();
        let results = execute_matrix(&skills, &files, &reviewer, &config);
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.exit_status, ExitStatus::Ok);
            assert_eq!(r.findings.len(), 1);
        }
    }

    #[test]
    fn execute_matrix_budget_stops_early() {
        let json = make_findings_json(&["Finding"]);
        let reviewer = MockReviewer::with_default(&json, sample_usage());
        let skills = vec![
            sample_skill("security", None, Severity::Critical),
            sample_skill("performance", None, Severity::High),
            sample_skill("correctness", None, Severity::Medium),
        ];
        let files = sample_files();
        let mut config = default_config();
        config.max_calls_per_review = 1;
        let results = execute_matrix(&skills, &files, &reviewer, &config);
        assert_eq!(results.len(), 3);
        // First should succeed, rest should be budget-capped
        assert_eq!(results[0].exit_status, ExitStatus::Ok);
        assert_eq!(results[1].exit_status, ExitStatus::Error);
        assert_eq!(results[1].failure_reason, Some(FailureReason::BudgetCapHit));
        assert_eq!(results[2].exit_status, ExitStatus::Error);
        assert_eq!(results[2].failure_reason, Some(FailureReason::BudgetCapHit));
    }

    #[test]
    fn execute_matrix_empty_input() {
        let reviewer = MockReviewer::new();
        let config = default_config();
        let results = execute_matrix(&[], &[], &reviewer, &config);
        assert!(results.is_empty());
    }

    // =======================================================================
    // Helper function unit tests
    // =======================================================================

    #[test]
    fn manifest_prompts_conversion_primary_only() {
        let prompts = Prompts {
            primary: "base prompt".to_owned(),
            anthropic: None,
            openai: None,
            google: None,
        };
        let sp = manifest_prompts_to_skill_prompts(&prompts);
        assert_eq!(sp.primary, "base prompt");
        assert!(sp.anthropic.is_none());
        assert!(sp.openai.is_none());
        assert!(sp.google.is_none());
    }

    #[test]
    fn manifest_prompts_conversion_with_overrides() {
        let prompts = Prompts {
            primary: "base prompt".to_owned(),
            anthropic: Some(ProviderPrompt {
                override_prompt: Some("anthropic text".to_owned()),
            }),
            openai: Some(ProviderPrompt {
                override_prompt: Some("openai text".to_owned()),
            }),
            google: None,
        };
        let sp = manifest_prompts_to_skill_prompts(&prompts);
        assert_eq!(
            sp.anthropic.as_ref().unwrap().override_text,
            "anthropic text"
        );
        assert_eq!(sp.openai.as_ref().unwrap().override_text, "openai text");
        assert!(sp.google.is_none());
    }

    #[test]
    fn clamp_findings_reduces_severity() {
        let mut findings = vec![
            FindingBuilder::new().severity(Severity::Critical).build(),
            FindingBuilder::new().severity(Severity::High).build(),
            FindingBuilder::new().severity(Severity::Low).build(),
        ];
        let clamped = clamp_findings(&mut findings, &Severity::Medium);
        assert_eq!(clamped, 2);
        assert_eq!(findings[0].severity, Severity::Medium);
        assert_eq!(findings[0].clamped_from_severity, Some(Severity::Critical));
        assert_eq!(findings[1].severity, Severity::Medium);
        assert_eq!(findings[1].clamped_from_severity, Some(Severity::High));
        assert_eq!(findings[2].severity, Severity::Low);
        assert!(findings[2].clamped_from_severity.is_none());
    }

    #[test]
    fn tag_findings_sets_all_fields() {
        let skill = sample_skill("my-skill", None, Severity::Critical);
        let mut findings = vec![FindingBuilder::new().build()];
        tag_findings(&mut findings, &skill, ModelFamily::OpenAi, "run-42");
        assert_eq!(findings[0].originating_skill.as_deref(), Some("my-skill"));
        assert_eq!(findings[0].skill_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            findings[0].manifest_sha256.as_deref(),
            Some(&"a".repeat(64)[..])
        );
        assert_eq!(findings[0].prompt_family.as_deref(), Some("openai"));
        assert_eq!(findings[0].skill_run_id.as_deref(), Some("run-42"));
    }

    #[test]
    fn sanitize_finding_fields_strips_ansi() {
        let mut findings = vec![
            FindingBuilder::new()
                .title("\x1b[31mBad\x1b[0m")
                .description("\x1b[1mBold\x1b[0m")
                .suggested_fix("\x1b[32mFix\x1b[0m")
                .reasoning("\x1b[33mWhy\x1b[0m")
                .canonical_pattern("\x1b[34mPat\x1b[0m")
                .based_on_excerpt("\x1b[35mExc\x1b[0m")
                .evidence("\x1b[36mEv\x1b[0m")
                .build(),
        ];
        sanitize_finding_fields(&mut findings);
        assert_eq!(findings[0].title, "Bad");
        assert_eq!(findings[0].description, "Bold");
        assert_eq!(findings[0].suggested_fix.as_deref(), Some("Fix"));
        assert_eq!(findings[0].reasoning.as_deref(), Some("Why"));
        assert_eq!(findings[0].canonical_pattern.as_deref(), Some("Pat"));
        assert_eq!(findings[0].based_on_excerpt.as_deref(), Some("Exc"));
        assert_eq!(findings[0].evidence[0], "Ev");
    }
}
