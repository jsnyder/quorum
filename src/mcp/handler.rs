/// MCP server handler — dispatches tool calls to quorum pipeline.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::mcp_server::ServerHandler;
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
    RpcError,
};

use crate::cache::ParseCache;
use crate::config::{Config, EnvConfigSource};
use crate::feedback::{FeedbackEntry, FeedbackStore, Verdict};
use crate::llm_client::OpenAiClient;
use crate::mcp::tools::{CatalogTool, ChatTool, DebugTool, FeedbackTool, ReviewTool, TestgenTool};
use crate::redact;
use crate::parser::Language;
use crate::pipeline::{self, LlmReviewer, PipelineConfig};

pub struct QuorumHandler {
    config: Config,
    feedback_store: FeedbackStore,
    llm_reviewer: Option<Arc<dyn LlmReviewer>>,
    parse_cache: Arc<ParseCache>,
}

impl QuorumHandler {
    pub fn new() -> anyhow::Result<Self> {
        Self::with_cache(Arc::new(ParseCache::new(256)))
    }

    pub fn with_cache(cache: Arc<ParseCache>) -> anyhow::Result<Self> {
        let config = Config::load(&EnvConfigSource)?;
        let feedback_path = dirs_path()?.join("feedback.jsonl");
        let feedback_store = FeedbackStore::new(feedback_path);

        let llm_reviewer: Option<Arc<dyn LlmReviewer>> = if let Ok(api_key) = config.require_api_key() {
            Some(Arc::new(OpenAiClient::new(&config.base_url, api_key)?))
        } else {
            None
        };

        Ok(Self {
            config,
            feedback_store,
            llm_reviewer,
            parse_cache: cache,
        })
    }

    /// Assemble the `PipelineConfig` for a review request.
    ///
    /// Issue #93: pipeline-level feedback writes (post-fix verdicts,
    /// auto-calibrate recordings) must target the same store the handler
    /// was constructed with — not the global `~/.quorum/feedback.jsonl` —
    /// otherwise tests (or alternate prod constructors) silently split
    /// reads from one DB and writes to another. Threading the path here is
    /// the entire fix; the helper exists so the contract is unit-testable
    /// independently of running a full review.
    ///
    /// `params` is the parsed MCP tool input; per-request config (e.g.
    /// `focus` from issue #104) is plumbed through here so the helper
    /// stays the single place to map handler state + tool params into a
    /// pipeline config.
    pub(crate) fn build_pipeline_config_for_review(
        &self,
        params: &ReviewTool,
    ) -> Result<PipelineConfig, String> {
        let feedback = self
            .feedback_store
            .load_all()
            .map_err(|e| format!("failed to load feedback store: {e}"))?;
        Ok(PipelineConfig {
            models: vec![self.config.model.clone()],
            feedback,
            feedback_store: Some(self.feedback_store.path().to_path_buf()),
            // Issue #104: thread the caller's focus directive into the
            // pipeline. Pre-fix, this field was dropped on the floor.
            focus: params.focus.clone(),
            ..Default::default()
        })
    }

    async fn handle_review(&self, params: ReviewTool) -> Result<CallToolResult, String> {
        let lang = Language::from_path(std::path::Path::new(&params.file_path))
            .ok_or_else(|| format!("Unsupported file type: {}", params.file_path))?;

        // Surface feedback-store read failures (corrupted/unreadable JSONL)
        // instead of silently reviewing without precedent. Otherwise persistent
        // state corruption would mask itself behind degraded review output.
        let pipeline_cfg = self.build_pipeline_config_for_review(&params)?;

        let result = pipeline::review_source(
            std::path::Path::new(&params.file_path),
            &params.code,
            lang,
            self.llm_reviewer.as_deref(),
            &pipeline_cfg,
            Some(&self.parse_cache),
        )
        .await
        .map_err(|e| format!("Review error: {}", e))?;

        let json = serde_json::to_string_pretty(&result.findings)
            .map_err(|e| format!("Serialization error: {}", e))?;

        Ok(CallToolResult::text_content(vec![json.into()]))
    }

    fn handle_feedback(&self, params: FeedbackTool) -> Result<CallToolResult, String> {
        use crate::mcp::tools::FeedbackVerdict;
        // `blamed_chunks` is only meaningful for `context_misleading`. Reject
        // callers that pass it with any other verdict — silent acceptance
        // would discard real data without telling the caller.
        if !matches!(params.verdict, FeedbackVerdict::ContextMisleading)
            && params.blamed_chunks.as_ref().is_some_and(|v| !v.is_empty())
        {
            return Err(format!(
                "blamed_chunks is only valid with verdict='context_misleading' (got '{:?}')",
                params.verdict
            ));
        }
        let verdict = match params.verdict {
            FeedbackVerdict::Tp => Verdict::Tp,
            FeedbackVerdict::Fp => Verdict::Fp,
            FeedbackVerdict::Partial => Verdict::Partial,
            FeedbackVerdict::Wontfix => Verdict::Wontfix,
            FeedbackVerdict::ContextMisleading => Verdict::ContextMisleading {
                blamed_chunk_ids: params.blamed_chunks.clone().unwrap_or_default(),
            },
        };

        // #123 Layer 1: fp_kind is meaningful only for verdict=Fp on the
        // Human path. We drop it on non-Fp verdicts (with a warning so the
        // dropped flag is visible) and we drop it on the External path
        // (matches the CLI External surface, which also doesn't thread
        // fp_kind through ExternalVerdictInput yet — keeps the wire
        // contract uniform across ingestion surfaces).
        let fp_kind = if matches!(verdict, Verdict::Fp) {
            if let Some(crate::feedback::FpKind::OutOfScope { tracked_in: None }) =
                &params.fp_kind
            {
                tracing::warn!(
                    "MCP feedback: out_of_scope fp_kind recorded without tracked_in; \
                     deferral has no tracking link"
                );
            }
            params.fp_kind.clone()
        } else {
            if params.fp_kind.is_some() {
                tracing::warn!(
                    "MCP feedback: fpKind was provided but verdict is not 'fp'; \
                     ignoring the field"
                );
            }
            None
        };

        // External-agent path: when from_agent is provided, route through
        // record_external so Provenance::External is serialized and the
        // calibrator applies the 0.7 weight. Human path is unchanged below.
        if let Some(agent) = params.from_agent {
            if fp_kind.is_some() {
                tracing::warn!(
                    "MCP feedback: fpKind dropped on External path \
                     (ExternalVerdictInput does not carry fp_kind yet)"
                );
            }
            let input = crate::feedback::ExternalVerdictInput {
                file_path: params.file_path,
                finding_title: params.finding,
                finding_category: params.category,
                verdict,
                reason: params.reason,
                agent,
                agent_model: params.agent_model,
                confidence: params.confidence,
            };
            self.feedback_store
                .record_external(input)
                .map_err(|e| format!("Failed to record external feedback: {}", e))?;
            let count = self.feedback_store.count().unwrap_or(0);
            let msg = format!("Recorded external feedback. Total entries: {}", count);
            return Ok(CallToolResult::text_content(vec![msg.into()]));
        }

        let entry = FeedbackEntry {
            file_path: params.file_path,
            finding_title: params.finding,
            // Honor caller-supplied category on the Human path too — keeps
            // analytics aligned across the three ingestion surfaces. Falls
            // back to "manual" to match the CLI Human default.
            finding_category: params
                .category
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "manual".to_string()),
            verdict: verdict.clone(),
            reason: params.reason,
            model: params.model,
            timestamp: chrono::Utc::now(),
            provenance: crate::feedback::Provenance::Human,
            fp_kind,
        };

        self.feedback_store
            .record(&entry)
            .map_err(|e| format!("Failed to record feedback: {}", e))?;

        let count = self.feedback_store.count().unwrap_or(0);
        let msg = format!("Recorded {} feedback. Total entries: {}", verdict_label(&entry), count);
        Ok(CallToolResult::text_content(vec![msg.into()]))
    }

    fn handle_catalog(&self, params: CatalogTool) -> Result<CallToolResult, String> {
        let result = match params.query.to_lowercase().as_str() {
            "models" => {
                format!("Configured model: {}\nSet QUORUM_MODEL to change.", self.config.model)
            }
            "languages" => {
                "Supported languages:\n- Rust (.rs)\n- Python (.py)\n- TypeScript (.ts)\n- TSX (.tsx)\n- Bash (.sh, .bash, .zsh)\n- Dockerfile (Dockerfile*)".to_string()
            }
            "domains" => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let info = crate::domain::detect_domain(&cwd);
                let mut result = String::new();
                if info.languages.is_empty() && info.frameworks.is_empty() {
                    result.push_str("No frameworks or languages detected in current directory.");
                } else {
                    if !info.languages.is_empty() {
                        result.push_str(&format!("Languages: {}\n", info.languages.join(", ")));
                    }
                    if !info.frameworks.is_empty() {
                        result.push_str(&format!("Frameworks: {}", info.frameworks.join(", ")));
                    }
                }
                result
            }
            "stats" => {
                let mut report = String::new();
                // Cache stats
                let cs = self.parse_cache.stats();
                report.push_str(&format!(
                    "Parse cache: {}/{} entries, {} hits, {} misses, {:.0}% hit rate\n\n",
                    cs.size, cs.capacity, cs.hits, cs.misses, cs.hit_rate() * 100.0
                ));
                // Feedback stats
                match self.feedback_store.load_all() {
                    Ok(entries) => {
                        let stats = crate::analytics::compute_stats(&entries);
                        report.push_str(&crate::analytics::format_stats_report(&stats));
                    }
                    Err(e) => report.push_str(&format!("Failed to load feedback: {}", e)),
                }
                report
            }
            other => format!("Unknown catalog query: {}. Use: models, languages, domains, or stats.", other),
        };
        Ok(CallToolResult::text_content(vec![result.into()]))
    }

    async fn handle_chat(&self, params: ChatTool) -> Result<CallToolResult, String> {
        let reviewer = Arc::clone(
            self.llm_reviewer
                .as_ref()
                .ok_or("Chat requires QUORUM_API_KEY to be set.")?,
        );

        let mut prompt = format!("Question: {}\n\n", redact::redact_secrets(&params.question));
        if let Some(code) = &params.code {
            let redacted = redact::redact_secrets(code);
            let lang = params.file_path.as_deref()
                .and_then(|p| Language::from_path(std::path::Path::new(p)))
                .map(|l| match l {
                    Language::Rust => "rust",
                    Language::Python => "python",
                    Language::TypeScript => "typescript",
                    Language::Tsx => "tsx",
                    Language::Yaml => "yaml",
                    Language::Bash => "bash",
                    Language::Dockerfile => "dockerfile",
                    Language::Terraform => "terraform",
                })
                .unwrap_or("text");
            prompt.push_str(&format!("```{}\n{}\n```\n", lang, redacted));
        }

        let model = self.config.model.clone();
        let sys_prompt = crate::llm_client::OpenAiClient::system_prompt().to_string();
        let resp = tokio::task::spawn_blocking(move || reviewer.review(&prompt, &model, &sys_prompt))
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, tool = "chat", "blocking review task failed");
                format!("review task failed: {}", e)
            })?
            .map_err(|e| format!("LLM error: {}", e))?;

        Ok(CallToolResult::text_content(vec![resp.content.into()]))
    }

    async fn handle_debug(&self, params: DebugTool) -> Result<CallToolResult, String> {
        let reviewer = Arc::clone(
            self.llm_reviewer
                .as_ref()
                .ok_or("Debug requires QUORUM_API_KEY to be set.")?,
        );

        let redacted_code = redact::redact_secrets(&params.code);
        let redacted_error = redact::redact_secrets(&params.error);
        let prompt = format!(
            "Debug this error in `{}`.\n\nError:\n```\n{}\n```\n\nCode:\n```\n{}\n```\n\nProvide: 1) Root cause analysis, 2) Suggested fix, 3) Prevention advice.",
            params.file_path, redacted_error, redacted_code
        );

        let model = self.config.model.clone();
        let sys_prompt = crate::llm_client::OpenAiClient::system_prompt().to_string();
        let resp = tokio::task::spawn_blocking(move || reviewer.review(&prompt, &model, &sys_prompt))
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, tool = "debug", "blocking review task failed");
                format!("review task failed: {}", e)
            })?
            .map_err(|e| format!("LLM error: {}", e))?;

        Ok(CallToolResult::text_content(vec![resp.content.into()]))
    }

    async fn handle_testgen(&self, params: TestgenTool) -> Result<CallToolResult, String> {
        let reviewer = Arc::clone(
            self.llm_reviewer
                .as_ref()
                .ok_or("Testgen requires QUORUM_API_KEY to be set.")?,
        );

        let lang = Language::from_path(std::path::Path::new(&params.file_path))
            .ok_or_else(|| format!("Unsupported file type: {}", params.file_path))?;

        let lang_name = match lang {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::Yaml => "yaml",
            Language::Bash => "bash",
            Language::Dockerfile => "dockerfile",
            Language::Terraform => "terraform",
        };

        let framework_hint = params.framework.as_deref().map(|f| format!(" using the {} framework", f)).unwrap_or_default();
        let redacted_code = redact::redact_secrets(&params.code);
        let prompt = format!(
            "Generate comprehensive tests{} for this {} code from `{}`.\n\n```{}\n{}\n```\n\nInclude: happy path, edge cases, error cases. Return ONLY the test code.",
            framework_hint, lang_name, params.file_path, lang_name, redacted_code
        );

        let model = self.config.model.clone();
        let sys_prompt = crate::llm_client::OpenAiClient::system_prompt().to_string();
        let resp = tokio::task::spawn_blocking(move || reviewer.review(&prompt, &model, &sys_prompt))
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, tool = "testgen", "blocking review task failed");
                format!("review task failed: {}", e)
            })?
            .map_err(|e| format!("LLM error: {}", e))?;

        Ok(CallToolResult::text_content(vec![resp.content.into()]))
    }
}

// Free function rather than `impl FeedbackEntry`, because `FeedbackEntry`
// now lives in the `quorum` library crate (bin/lib hybrid split) and Rust's
// orphan rules forbid inherent impl blocks on out-of-crate types.
fn verdict_label(entry: &FeedbackEntry) -> &'static str {
    match &entry.verdict {
        Verdict::Tp => "true positive",
        Verdict::Fp => "false positive",
        Verdict::Partial => "partial",
        Verdict::Wontfix => "wontfix",
        Verdict::ContextMisleading { .. } => "context misleading",
    }
}

fn dirs_path() -> std::io::Result<PathBuf> {
    // Precedence MUST match `src/main.rs::quorum_dir()` byte-for-byte —
    // otherwise an MCP server and `quorum stats`/`feedback`/`review` can
    // diverge to different `feedback.jsonl` files on the same host (e.g.
    // when XDG_DATA_HOME is set without QUORUM_HOME). Order:
    //   1. QUORUM_HOME (treat empty as ".quorum" cwd-relative)
    //   2. $HOME/.quorum
    //   3. ".quorum" cwd-relative as last-resort fallback
    let dir = if let Ok(qh) = std::env::var("QUORUM_HOME") {
        if qh.is_empty() {
            PathBuf::from(".quorum")
        } else {
            PathBuf::from(qh)
        }
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".quorum")
    } else {
        PathBuf::from(".quorum")
    };
    // Surface create_dir_all failures so handler initialization fails fast
    // instead of returning a path that subsequent writes will reject.
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[async_trait]
impl ServerHandler for QuorumHandler {
    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: vec![
                ReviewTool::tool(),
                ChatTool::tool(),
                DebugTool::tool(),
                TestgenTool::tool(),
                FeedbackTool::tool(),
                CatalogTool::tool(),
            ],
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, rust_mcp_sdk::schema::CallToolError> {
        use rust_mcp_sdk::schema::CallToolError;

        let args_value = serde_json::Value::Object(params.arguments.unwrap_or_default());
        let result = match params.name.as_str() {
            "review" => {
                let tool: ReviewTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_review(tool).await
            }
            "feedback" => {
                let tool: FeedbackTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_feedback(tool)
            }
            "catalog" => {
                let tool: CatalogTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_catalog(tool)
            }
            "chat" => {
                let tool: ChatTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_chat(tool).await
            }
            "debug" => {
                let tool: DebugTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_debug(tool).await
            }
            "testgen" => {
                let tool: TestgenTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_testgen(tool).await
            }
            _ => return Err(CallToolError::unknown_tool(params.name)),
        };

        result.map_err(|e| CallToolError::from_message(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_handler_parses_clean_rust() {
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/quorum-test-feedback.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = ReviewTool {
            code: "fn main() { let x = 42; }".into(),
            file_path: "test.rs".into(),
            focus: None,
        };

        let result = handler.handle_review(params).await.unwrap();
        // Clean code should produce empty findings or minor ones
        assert!(!result.content.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_handler_finds_insecure_python() {
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/quorum-test-feedback2.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = ReviewTool {
            code: "def run(code):\n    eval(code)\n".into(),
            file_path: "test.py".into(),
            focus: Some("security".into()),
        };

        let result = handler.handle_review(params).await.unwrap();
        let text = &result.content[0];
        // Should contain eval finding in the JSON output
        let text_str = serde_json::to_string(text).unwrap();
        assert!(text_str.contains("eval"));
    }

    #[test]
    fn feedback_handler_records_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(path.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = FeedbackTool {
            file_path: "src/auth.rs".into(),
            finding: "SQL injection".into(),
            verdict: crate::mcp::tools::FeedbackVerdict::Tp,
            reason: "Fixed".into(),
            model: Some("gpt-5.4".into()),
            blamed_chunks: None,
            from_agent: None,
            agent_model: None,
            confidence: None,
            category: None,
            fp_kind: None,
        };

        let result = handler.handle_feedback(params).unwrap();
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(text.contains("true positive"));

        let store = FeedbackStore::new(path);
        assert_eq!(store.count().unwrap(), 1);
    }

    // Issue #94: removed `feedback_handler_rejects_invalid_verdict` — the
    // FeedbackVerdict enum now makes invalid verdicts statically
    // unconstructable. The wire-level rejection is covered by
    // `feedback_verdict_rejects_unknown_at_parse_time` in tools.rs.

    #[test]
    fn feedback_handler_records_context_misleading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(path.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = FeedbackTool {
            file_path: "src/retriever.rs".into(),
            finding: "Stale API reference".into(),
            verdict: crate::mcp::tools::FeedbackVerdict::ContextMisleading,
            reason: "docs described v1, code uses v2".into(),
            model: None,
            blamed_chunks: Some(vec!["chunk-abc".into(), "chunk-def".into()]),
            from_agent: None,
            agent_model: None,
            confidence: None,
            category: None,
            fp_kind: None,
        };

        let result = handler.handle_feedback(params).unwrap();
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(text.contains("context misleading"));

        let store = FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].verdict {
            Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert_eq!(blamed_chunk_ids,
                    &vec!["chunk-abc".to_string(), "chunk-def".to_string()]);
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
        }
    }

    #[test]
    fn feedback_handler_rejects_blamed_chunks_with_non_context_misleading_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(path.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = FeedbackTool {
            file_path: "src/foo.rs".into(),
            finding: "whatever".into(),
            verdict: crate::mcp::tools::FeedbackVerdict::Tp,
            reason: "r".into(),
            model: None,
            blamed_chunks: Some(vec!["chunk-x".into()]),
            from_agent: None,
            agent_model: None,
            confidence: None,
            category: None,
            fp_kind: None,
        };

        let err = handler.handle_feedback(params).expect_err("must reject");
        assert!(
            err.contains("blamed_chunks is only valid"),
            "error must explain the constraint: {err}"
        );
        // Nothing must have been persisted.
        let store = FeedbackStore::new(path);
        assert_eq!(store.load_all().unwrap().len(), 0);
    }

    #[test]
    fn mcp_from_agent_writes_external_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fb.jsonl");
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(path.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = FeedbackTool {
            file_path: "src/a.rs".into(),
            finding: "SQL injection".into(),
            verdict: crate::mcp::tools::FeedbackVerdict::Tp,
            reason: "confirmed".into(),
            model: None,
            blamed_chunks: None,
            from_agent: Some("pal".into()),
            agent_model: Some("gemini-3-pro-preview".into()),
            confidence: Some(0.9),
            category: None,
            fp_kind: None,
        };
        handler.handle_feedback(params).unwrap();

        let store = FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].provenance {
            crate::feedback::Provenance::External { agent, model, confidence } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gemini-3-pro-preview"));
                assert_eq!(*confidence, Some(0.9));
            }
            other => panic!("expected External provenance, got {:?}", other),
        }
    }

    #[test]
    fn mcp_feedback_without_from_agent_still_writes_human() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fb.jsonl");
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(path.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = FeedbackTool {
            file_path: "src/a.rs".into(),
            finding: "Bug".into(),
            verdict: crate::mcp::tools::FeedbackVerdict::Tp,
            reason: "r".into(),
            model: None,
            blamed_chunks: None,
            from_agent: None,
            agent_model: None,
            confidence: None,
            category: None,
            fp_kind: None,
        };
        handler.handle_feedback(params).unwrap();

        let store = FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(
            matches!(all[0].provenance, crate::feedback::Provenance::Human),
            "default path must be Human, got {:?}",
            all[0].provenance
        );
    }

    #[test]
    fn catalog_handler_lists_languages() {
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = CatalogTool {
            query: "languages".into(),
        };

        let result = handler.handle_catalog(params).unwrap();
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(text.contains("Rust"));
        assert!(text.contains("Python"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_handler_rejects_unsupported_extension() {
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = ReviewTool {
            code: "some code".into(),
            file_path: "file.xyz".into(),
            focus: None,
        };

        assert!(handler.handle_review(params).await.is_err());
    }

    // -- Chat, Debug, Testgen: require LLM --

    fn handler_no_llm() -> QuorumHandler {
        QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused-handler.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    fn handler_with_fake_llm() -> QuorumHandler {
        use crate::pipeline::LlmReviewer;

        struct FakeLlm;
        impl LlmReviewer for FakeLlm {
            fn review(&self, _prompt: &str, _model: &str, _system_prompt: &str) -> anyhow::Result<crate::llm_client::LlmResponse> {
                Ok(crate::llm_client::LlmResponse {
                    content: "This is a helpful response about the code.".into(),
                    usage: None,
                })
            }
        }

        QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: Some("sk-test".into()),
                model: "test-model".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused-handler2.jsonl")),
            llm_reviewer: Some(Arc::new(FakeLlm)),
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    /// Fake LlmReviewer that synchronizes on a std::sync::Barrier inside
    /// review(). Used by the #129 concurrency regression test to prove
    /// that concurrent MCP handler invocations actually run in parallel
    /// on the blocking pool — if the executor is parked on a sync
    /// .review() call, only one caller will ever reach barrier.wait()
    /// and the test will deadlock until the outer tokio::time::timeout.
    ///
    /// std::sync::Barrier is intentional — tokio::sync::Barrier would
    /// yield to the executor and defeat the test.
    struct BarrierLlm {
        barrier: std::sync::Arc<std::sync::Barrier>,
    }

    impl crate::pipeline::LlmReviewer for BarrierLlm {
        fn review(
            &self,
            _prompt: &str,
            _model: &str,
            _system_prompt: &str,
        ) -> anyhow::Result<crate::llm_client::LlmResponse> {
            self.barrier.wait();
            Ok(crate::llm_client::LlmResponse {
                content: "ok".into(),
                usage: None,
            })
        }
    }

    fn handler_with_barrier_llm(barrier: std::sync::Arc<std::sync::Barrier>) -> QuorumHandler {
        QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: Some("sk-test".into()),
                model: "test-model".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused-barrier.jsonl")),
            llm_reviewer: Some(Arc::new(BarrierLlm { barrier })),
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    /// Fake LlmReviewer that returns a fixed sentinel string. Used by
    /// behavioral smoke tests to prove the prompt flowed through the
    /// handler and the response surfaced in the CallToolResult — avoids
    /// the `assert!(result.is_ok())` Liar-test antipattern.
    struct EchoLlm {
        sentinel: &'static str,
    }

    impl crate::pipeline::LlmReviewer for EchoLlm {
        fn review(
            &self,
            _prompt: &str,
            _model: &str,
            _system_prompt: &str,
        ) -> anyhow::Result<crate::llm_client::LlmResponse> {
            Ok(crate::llm_client::LlmResponse {
                content: self.sentinel.into(),
                usage: None,
            })
        }
    }

    fn handler_with_echo_llm(sentinel: &'static str) -> QuorumHandler {
        QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: Some("sk-test".into()),
                model: "test-model".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused-echo.jsonl")),
            llm_reviewer: Some(Arc::new(EchoLlm { sentinel })),
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    /// Fake reviewer that always panics. Used to pin the JoinError
    /// surfacing contract (AC8 / design gap G1) — pre-fix, a panic
    /// inside the sync handler would unwind the tokio worker. Post-fix,
    /// the panic is caught by spawn_blocking and surfaced as a string
    /// error containing "review task failed".
    struct PanicLlm;

    impl crate::pipeline::LlmReviewer for PanicLlm {
        fn review(
            &self,
            _prompt: &str,
            _model: &str,
            _system_prompt: &str,
        ) -> anyhow::Result<crate::llm_client::LlmResponse> {
            panic!("PanicLlm: synthetic panic for JoinError test");
        }
    }

    fn handler_with_panic_llm() -> QuorumHandler {
        QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: Some("sk-test".into()),
                model: "test-model".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused-panic.jsonl")),
            llm_reviewer: Some(Arc::new(PanicLlm)),
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn handle_chat_surfaces_join_error_on_panic() {
        // Pin AC8: a panic inside the blocking review task is caught by
        // spawn_blocking and surfaced as Err("review task failed: ...")
        // — does NOT unwind the worker. tracing::warn! also fires at
        // the failure site for ops visibility (not asserted here).
        let handler = handler_with_panic_llm();
        let result = handler
            .handle_chat(ChatTool {
                question: "y".into(),
                code: Some("x".into()),
                file_path: None,
            })
            .await;
        let err = result.expect_err("panic must surface as Err, not unwind worker");
        assert!(
            err.contains("review task failed"),
            "JoinError must surface with 'review task failed' prefix; got: {}",
            err
        );
    }

    #[test]
    fn handle_chat_runs_concurrent_llm_calls_in_parallel() {
        // INVARIANT: handle_chat must not block the tokio worker.
        //
        // Bug case (handle_chat is sync, called inside an async block):
        //   Task 1 polls, calls handle_chat, calls reviewer.review,
        //   which calls barrier.wait(). The worker is now parked.
        //   Tasks 2..N never get polled. Barrier never releases.
        //
        // Fix case (handle_chat is async + spawn_blocking):
        //   All N invocations land on the blocking pool, each thread
        //   reaches the barrier, barrier releases, all complete in
        //   microseconds.
        //
        // The wall-clock killswitch uses std::sync::mpsc::recv_timeout,
        // NOT tokio::time::timeout. With worker_threads=1, a parked
        // worker also starves tokio's timer driver — so a tokio-side
        // timeout would never fire on the bug branch and the test
        // would hang forever. The body runs inside its own
        // std::thread::spawn so the test thread can wait via OS-level
        // mpsc::recv_timeout regardless of runtime liveness. If the
        // body deadlocks, the body thread leaks (acceptable in a
        // test — the process exits on failure).
        use std::sync::mpsc;

        const N: usize = 4;

        let (tx, rx) = mpsc::channel::<()>();
        let _body = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("build runtime");
            rt.block_on(async move {
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
                let handler = std::sync::Arc::new(handler_with_barrier_llm(
                    std::sync::Arc::clone(&barrier),
                ));

                let mut joins = Vec::new();
                for _ in 0..N {
                    let h = std::sync::Arc::clone(&handler);
                    joins.push(tokio::spawn(async move {
                        h.handle_chat(ChatTool {
                            question: "y".into(),
                            code: Some("x".into()),
                            file_path: None,
                        })
                        .await
                    }));
                }
                for j in joins {
                    j.await
                        .expect("task panicked")
                        .expect("chat handler returned err");
                }
            });
            let _ = tx.send(());
        });

        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("handle_chat serializes LLM calls — barrier deadlocked");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn chat_requires_api_key() {
        let handler = handler_no_llm();
        let params = ChatTool {
            question: "What does this do?".into(),
            code: None,
            file_path: None,
        };
        assert!(handler.handle_chat(params).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn chat_with_llm_returns_response() {
        let handler = handler_with_fake_llm();
        let params = ChatTool {
            question: "What does this function do?".into(),
            code: Some("fn add(a: i32, b: i32) -> i32 { a + b }".into()),
            file_path: Some("math.rs".into()),
        };
        let result = handler.handle_chat(params).await.unwrap();
        assert!(!result.content.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn debug_requires_api_key() {
        let handler = handler_no_llm();
        let params = DebugTool {
            error: "panic!".into(),
            code: "fn main() {}".into(),
            file_path: "main.rs".into(),
        };
        assert!(handler.handle_debug(params).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn debug_with_llm_returns_response() {
        let handler = handler_with_fake_llm();
        let params = DebugTool {
            error: "index out of bounds".into(),
            code: "fn get(v: &[i32]) -> i32 { v[10] }".into(),
            file_path: "main.rs".into(),
        };
        let result = handler.handle_debug(params).await.unwrap();
        assert!(!result.content.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn handle_debug_returns_llm_content() {
        // Behavioral smoke test — proves the prompt flowed through the
        // async + spawn_blocking path AND the response surfaced in the
        // CallToolResult. Sentinel-on-serialized-output avoids the
        // `assert!(result.is_ok())` Liar-test antipattern.
        const SENTINEL: &str = "debug-fake-output-2026-04-29";
        let handler = handler_with_echo_llm(SENTINEL);
        let result = handler
            .handle_debug(DebugTool {
                file_path: "f.rs".into(),
                code: "x".into(),
                error: "e".into(),
            })
            .await
            .expect("handle_debug ok");
        // Tighten scope: assert sentinel is in result.content (the user-facing
        // tool output), not anywhere in the serialized wrapper. Keeps the
        // assertion shape-agnostic about CallToolResult internals while
        // still failing if the prompt/response wiring breaks.
        let body = serde_json::to_string(&result.content).expect("serialize content");
        assert!(
            body.contains(SENTINEL),
            "response.content must contain sentinel from EchoLlm; got: {}",
            body
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn testgen_requires_api_key() {
        let handler = handler_no_llm();
        let params = TestgenTool {
            code: "fn add(a: i32, b: i32) -> i32 { a + b }".into(),
            file_path: "math.rs".into(),
            framework: None,
        };
        assert!(handler.handle_testgen(params).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn testgen_with_llm_returns_response() {
        let handler = handler_with_fake_llm();
        let params = TestgenTool {
            code: "def add(a, b):\n    return a + b\n".into(),
            file_path: "math.py".into(),
            framework: Some("pytest".into()),
        };
        let result = handler.handle_testgen(params).await.unwrap();
        assert!(!result.content.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn testgen_rejects_unsupported_extension() {
        let handler = handler_with_fake_llm();
        let params = TestgenTool {
            code: "code".into(),
            file_path: "file.xyz".into(),
            framework: None,
        };
        assert!(handler.handle_testgen(params).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn handle_testgen_returns_llm_content() {
        // Behavioral smoke test mirror of handle_debug_returns_llm_content.
        const SENTINEL: &str = "testgen-fake-output-2026-04-29";
        let handler = handler_with_echo_llm(SENTINEL);
        let result = handler
            .handle_testgen(TestgenTool {
                file_path: "f.rs".into(),
                code: "fn x() {}".into(),
                framework: None,
            })
            .await
            .expect("handle_testgen ok");
        // Tighten scope: assert sentinel is in result.content (the user-facing
        // tool output), not anywhere in the serialized wrapper. Keeps the
        // assertion shape-agnostic about CallToolResult internals while
        // still failing if the prompt/response wiring breaks.
        let body = serde_json::to_string(&result.content).expect("serialize content");
        assert!(
            body.contains(SENTINEL),
            "response.content must contain sentinel from EchoLlm; got: {}",
            body
        );
    }

    // -- Cache integration --

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn review_populates_parse_cache() {
        let cache = Arc::new(ParseCache::new(10));
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/unused-cache-test.jsonl")),
            llm_reviewer: None,
            parse_cache: cache.clone(),
        };

        let params = ReviewTool {
            code: "fn main() {}".into(),
            file_path: "test.rs".into(),
            focus: None,
        };
        handler.handle_review(params).await.unwrap();
        assert_eq!(cache.stats().misses, 1, "First review should be a cache miss");

        // Second review with same code should hit cache
        let params2 = ReviewTool {
            code: "fn main() {}".into(),
            file_path: "test.rs".into(),
            focus: None,
        };
        handler.handle_review(params2).await.unwrap();
        assert_eq!(cache.stats().hits, 1, "Second review should be a cache hit");
    }

    // --- Issue #93: pipeline writes feedback to handler's configured store ---
    //
    // Pre-fix, `handle_review` constructed `PipelineConfig.feedback_store`
    // from `dirs_path()/feedback.jsonl` regardless of the path the handler
    // was actually constructed with. So a handler instantiated with a custom
    // store path (tests, alternate prod constructors) would READ precedents
    // from the configured store but WRITE pipeline-level recordings (post-fix
    // verdicts, auto-calibrate verdicts) to the global ~/.quorum file.
    //
    // The fix extracts `build_pipeline_config_for_review` so the handler can
    // pass its own store's path. The tests below verify the helper actually
    // does that — covering both a non-default path (the bug's home) and the
    // default-path case (regression guard against the inverse mutation).
    #[test]
    fn build_pipeline_config_uses_handler_store_path_when_non_default() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("issue93-non-default.jsonl");
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(custom.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };
        let cfg = handler
            .build_pipeline_config_for_review(&ReviewTool {
                code: "fn main() {}".into(),
                file_path: "test.rs".into(),
                focus: None,
            })
            .expect("helper must succeed for a fresh tempdir handler");
        assert_eq!(
            cfg.feedback_store,
            Some(custom),
            "PipelineConfig must point at the handler's configured store, not dirs_path()/feedback.jsonl",
        );
    }

    #[test]
    fn build_pipeline_config_threads_handler_store_path_unconditionally() {
        // Regression guard for the inverse mutation: "always use
        // self.feedback_store.path() but ignore the rest of the helper".
        // Two different non-default paths must yield two different cfg paths
        // — distinguishes "helper threads correctly" from "helper hardcodes
        // a single path that happens to match".
        let dir1 = tempfile::tempdir().unwrap();
        let p1 = dir1.path().join("issue93-a.jsonl");
        let h1 = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(p1.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = dir2.path().join("issue93-b.jsonl");
        let h2 = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(p2.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };
        let cfg1 = h1
            .build_pipeline_config_for_review(&ReviewTool {
                code: "fn main() {}".into(),
                file_path: "a.rs".into(),
                focus: None,
            })
            .unwrap();
        let cfg2 = h2
            .build_pipeline_config_for_review(&ReviewTool {
                code: "fn main() {}".into(),
                file_path: "b.rs".into(),
                focus: None,
            })
            .unwrap();
        assert_eq!(cfg1.feedback_store, Some(p1));
        assert_eq!(cfg2.feedback_store, Some(p2));
        assert_ne!(
            cfg1.feedback_store, cfg2.feedback_store,
            "different handlers must produce different cfg paths"
        );
    }

    // --- Issue #104: focus is threaded from ReviewTool into PipelineConfig ---
    //
    // Pre-fix, `handle_review` dropped `params.focus` on the floor. The
    // helper now copies it through; the prompt-side rendering is covered
    // separately in `src/review.rs`. This test kills the "forgot to thread"
    // mutation that any review.rs-only test would miss.

    #[test]
    fn build_pipeline_config_threads_focus_from_review_tool() {
        let dir = tempfile::tempdir().unwrap();
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(dir.path().join("focus.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };
        let cfg = handler
            .build_pipeline_config_for_review(&ReviewTool {
                code: "fn main() {}".into(),
                file_path: "test.rs".into(),
                focus: Some("security".into()),
            })
            .unwrap();
        assert_eq!(
            cfg.focus,
            Some("security".into()),
            "focus must be threaded verbatim from ReviewTool to PipelineConfig"
        );
    }

    #[test]
    fn build_pipeline_config_focus_is_none_when_caller_omits_it() {
        let dir = tempfile::tempdir().unwrap();
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(dir.path().join("nofocus.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };
        let cfg = handler
            .build_pipeline_config_for_review(&ReviewTool {
                code: "fn main() {}".into(),
                file_path: "test.rs".into(),
                focus: None,
            })
            .unwrap();
        assert_eq!(cfg.focus, None);
    }

    // --- Task 8: MCP fpKind round-trip (issue #123) ---

    /// All five FpKind variants must persist through the MCP boundary
    /// when verdict=fp. Each variant goes in via FeedbackTool, comes back
    /// out via FeedbackStore::load_all unchanged. This is the wire-contract
    /// test that proves MCP clients can write any kind we accept on the CLI.
    #[test]
    fn mcp_feedback_persists_fp_kind_each_variant() {
        use crate::feedback::FpKind;

        let cases = vec![
            ("hallucination", FpKind::Hallucination),
            (
                "compensating_control",
                FpKind::CompensatingControl {
                    reference: "PR #99".into(),
                },
            ),
            ("trust_model_assumption", FpKind::TrustModelAssumption),
            (
                "pattern_overgeneralization",
                FpKind::PatternOvergeneralization {
                    discriminator_hint: Some("dyn config — not literal".into()),
                },
            ),
            (
                "out_of_scope",
                FpKind::OutOfScope {
                    tracked_in: Some("issue #200".into()),
                },
            ),
        ];

        for (label, kind) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fb.jsonl");
            let handler = QuorumHandler {
                config: Config {
                    base_url: "https://example.com".into(),
                    api_key: None,
                    model: "test".into(),
                },
                feedback_store: FeedbackStore::new(path.clone()),
                llm_reviewer: None,
                parse_cache: Arc::new(ParseCache::new(10)),
            };

            let params = FeedbackTool {
                file_path: "src/a.rs".into(),
                finding: format!("variant {label}"),
                verdict: crate::mcp::tools::FeedbackVerdict::Fp,
                reason: "round-trip".into(),
                model: None,
                blamed_chunks: None,
                from_agent: None,
                agent_model: None,
                confidence: None,
                category: None,
                fp_kind: Some(kind.clone()),
            };
            handler.handle_feedback(params).unwrap();

            let store = FeedbackStore::new(path);
            let all = store.load_all().unwrap();
            assert_eq!(all.len(), 1, "variant {label} must persist");
            assert_eq!(
                all[0].fp_kind.as_ref(),
                Some(&kind),
                "variant {label} must round-trip unchanged"
            );
        }
    }

    /// `compensating_control` requires `reference`. The MCP boundary leans
    /// on serde to enforce this — the field is non-optional in the FpKind
    /// variant, so a JSON payload missing it must fail at deserialization.
    /// This is a wire-contract test, not a runtime-handler test: it proves
    /// schema-driven clients see the constraint.
    #[test]
    fn mcp_feedback_rejects_compensating_control_missing_reference() {
        let json = r#"{
            "filePath": "src/a.rs",
            "finding": "x",
            "verdict": "fp",
            "reason": "r",
            "fpKind": { "compensating_control": {} }
        }"#;
        let result: Result<FeedbackTool, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "missing `reference` on compensating_control must fail at parse: {:?}",
            result.ok()
        );
    }

    /// fp_kind on a non-fp verdict must be silently dropped at the MCP
    /// boundary (with a warning, but no error). The Human entry persisted
    /// to disk must have fp_kind = None.
    #[test]
    fn mcp_feedback_drops_fp_kind_when_verdict_is_not_fp() {
        use crate::feedback::FpKind;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fb.jsonl");
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(path.clone()),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = FeedbackTool {
            file_path: "src/a.rs".into(),
            finding: "x".into(),
            verdict: crate::mcp::tools::FeedbackVerdict::Tp,
            reason: "r".into(),
            model: None,
            blamed_chunks: None,
            from_agent: None,
            agent_model: None,
            confidence: None,
            category: None,
            fp_kind: Some(FpKind::Hallucination),
        };
        handler.handle_feedback(params).unwrap();

        let store = FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(
            all[0].fp_kind, None,
            "fp_kind must be dropped when verdict is not fp"
        );
    }
}
