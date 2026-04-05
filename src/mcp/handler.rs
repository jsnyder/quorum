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
    llm_reviewer: Option<Box<dyn LlmReviewer>>,
    parse_cache: Arc<ParseCache>,
}

impl QuorumHandler {
    pub fn new() -> anyhow::Result<Self> {
        Self::with_cache(Arc::new(ParseCache::new(256)))
    }

    pub fn with_cache(cache: Arc<ParseCache>) -> anyhow::Result<Self> {
        let config = Config::load(&EnvConfigSource)?;
        let feedback_path = dirs_path().join("feedback.jsonl");
        let feedback_store = FeedbackStore::new(feedback_path);

        let llm_reviewer: Option<Box<dyn LlmReviewer>> = if let Ok(api_key) = config.require_api_key() {
            Some(Box::new(OpenAiClient::new(&config.base_url, api_key)))
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

    fn handle_review(&self, params: ReviewTool) -> Result<CallToolResult, String> {
        let lang = Language::from_path(std::path::Path::new(&params.file_path))
            .ok_or_else(|| format!("Unsupported file type: {}", params.file_path))?;

        let feedback = self.feedback_store.load_all().unwrap_or_default();
        let feedback_path = dirs_path().join("feedback.jsonl");
        let pipeline_cfg = PipelineConfig {
            models: vec![self.config.model.clone()],
            feedback,
            feedback_store: Some(feedback_path),
            ..Default::default()
        };

        let result = pipeline::review_source(
            std::path::Path::new(&params.file_path),
            &params.code,
            lang,
            self.llm_reviewer.as_deref(),
            &pipeline_cfg,
            Some(&self.parse_cache),
        )
        .map_err(|e| format!("Review error: {}", e))?;

        let json = serde_json::to_string_pretty(&result.findings)
            .map_err(|e| format!("Serialization error: {}", e))?;

        Ok(CallToolResult::text_content(vec![json.into()]))
    }

    fn handle_feedback(&self, params: FeedbackTool) -> Result<CallToolResult, String> {
        let verdict = match params.verdict.to_lowercase().as_str() {
            "tp" => Verdict::Tp,
            "fp" => Verdict::Fp,
            "partial" => Verdict::Partial,
            "wontfix" => Verdict::Wontfix,
            other => return Err(format!("Invalid verdict: {}. Use tp, fp, partial, or wontfix.", other)),
        };

        let entry = FeedbackEntry {
            file_path: params.file_path,
            finding_title: params.finding,
            finding_category: String::new(),
            verdict: verdict.clone(),
            reason: params.reason,
            model: params.model,
            timestamp: chrono::Utc::now(),
            provenance: crate::feedback::Provenance::Human,
        };

        self.feedback_store
            .record(&entry)
            .map_err(|e| format!("Failed to record feedback: {}", e))?;

        let count = self.feedback_store.count().unwrap_or(0);
        let msg = format!("Recorded {} feedback. Total entries: {}", entry.verdict_label(), count);
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

    fn handle_chat(&self, params: ChatTool) -> Result<CallToolResult, String> {
        let reviewer = self.llm_reviewer.as_ref()
            .ok_or("Chat requires QUORUM_API_KEY to be set.")?;

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
                })
                .unwrap_or("text");
            prompt.push_str(&format!("```{}\n{}\n```\n", lang, redacted));
        }

        let resp = reviewer.review(&prompt, &self.config.model)
            .map_err(|e| format!("LLM error: {}", e))?;

        Ok(CallToolResult::text_content(vec![resp.content.into()]))
    }

    fn handle_debug(&self, params: DebugTool) -> Result<CallToolResult, String> {
        let reviewer = self.llm_reviewer.as_ref()
            .ok_or("Debug requires QUORUM_API_KEY to be set.")?;

        let redacted_code = redact::redact_secrets(&params.code);
        let redacted_error = redact::redact_secrets(&params.error);
        let prompt = format!(
            "Debug this error in `{}`.\n\nError:\n```\n{}\n```\n\nCode:\n```\n{}\n```\n\nProvide: 1) Root cause analysis, 2) Suggested fix, 3) Prevention advice.",
            params.file_path, redacted_error, redacted_code
        );

        let resp = reviewer.review(&prompt, &self.config.model)
            .map_err(|e| format!("LLM error: {}", e))?;

        Ok(CallToolResult::text_content(vec![resp.content.into()]))
    }

    fn handle_testgen(&self, params: TestgenTool) -> Result<CallToolResult, String> {
        let reviewer = self.llm_reviewer.as_ref()
            .ok_or("Testgen requires QUORUM_API_KEY to be set.")?;

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
        };

        let framework_hint = params.framework.as_deref().map(|f| format!(" using the {} framework", f)).unwrap_or_default();
        let redacted_code = redact::redact_secrets(&params.code);
        let prompt = format!(
            "Generate comprehensive tests{} for this {} code from `{}`.\n\n```{}\n{}\n```\n\nInclude: happy path, edge cases, error cases. Return ONLY the test code.",
            framework_hint, lang_name, params.file_path, lang_name, redacted_code
        );

        let resp = reviewer.review(&prompt, &self.config.model)
            .map_err(|e| format!("LLM error: {}", e))?;

        Ok(CallToolResult::text_content(vec![resp.content.into()]))
    }
}

impl FeedbackEntry {
    fn verdict_label(&self) -> &'static str {
        match self.verdict {
            Verdict::Tp => "true positive",
            Verdict::Fp => "false positive",
            Verdict::Partial => "partial",
            Verdict::Wontfix => "wontfix",
        }
    }
}

fn dirs_path() -> PathBuf {
    // Prefer XDG_DATA_HOME, fall back to HOME/.quorum
    let dir = if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg).join("quorum")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".quorum")
    } else {
        // Last resort: use current directory (not ideal, but don't crash)
        PathBuf::from(".quorum")
    };
    std::fs::create_dir_all(&dir).ok();
    dir
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
                self.handle_review(tool)
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
                self.handle_chat(tool)
            }
            "debug" => {
                let tool: DebugTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_debug(tool)
            }
            "testgen" => {
                let tool: TestgenTool = serde_json::from_value(args_value)
                    .map_err(|e| CallToolError::from_message(format!("Invalid parameters: {}", e)))?;
                self.handle_testgen(tool)
            }
            _ => return Err(CallToolError::unknown_tool(params.name)),
        };

        result.map_err(|e| CallToolError::from_message(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_handler_parses_clean_rust() {
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

        let result = handler.handle_review(params).unwrap();
        // Clean code should produce empty findings or minor ones
        assert!(!result.content.is_empty());
    }

    #[test]
    fn review_handler_finds_insecure_python() {
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

        let result = handler.handle_review(params).unwrap();
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
            verdict: "tp".into(),
            reason: "Fixed".into(),
            model: Some("gpt-5.4".into()),
        };

        let result = handler.handle_feedback(params).unwrap();
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(text.contains("true positive"));

        let store = FeedbackStore::new(path);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn feedback_handler_rejects_invalid_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let handler = QuorumHandler {
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(dir.path().join("fb.jsonl")),
            llm_reviewer: None,
            parse_cache: Arc::new(ParseCache::new(10)),
        };

        let params = FeedbackTool {
            file_path: "test.rs".into(),
            finding: "Bug".into(),
            verdict: "invalid".into(),
            reason: "test".into(),
            model: None,
        };

        assert!(handler.handle_feedback(params).is_err());
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

    #[test]
    fn review_handler_rejects_unsupported_extension() {
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

        assert!(handler.handle_review(params).is_err());
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
            fn review(&self, _prompt: &str, _model: &str) -> anyhow::Result<crate::llm_client::LlmResponse> {
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
            llm_reviewer: Some(Box::new(FakeLlm)),
            parse_cache: Arc::new(ParseCache::new(10)),
        }
    }

    #[test]
    fn chat_requires_api_key() {
        let handler = handler_no_llm();
        let params = ChatTool {
            question: "What does this do?".into(),
            code: None,
            file_path: None,
        };
        assert!(handler.handle_chat(params).is_err());
    }

    #[test]
    fn chat_with_llm_returns_response() {
        let handler = handler_with_fake_llm();
        let params = ChatTool {
            question: "What does this function do?".into(),
            code: Some("fn add(a: i32, b: i32) -> i32 { a + b }".into()),
            file_path: Some("math.rs".into()),
        };
        let result = handler.handle_chat(params).unwrap();
        assert!(!result.content.is_empty());
    }

    #[test]
    fn debug_requires_api_key() {
        let handler = handler_no_llm();
        let params = DebugTool {
            error: "panic!".into(),
            code: "fn main() {}".into(),
            file_path: "main.rs".into(),
        };
        assert!(handler.handle_debug(params).is_err());
    }

    #[test]
    fn debug_with_llm_returns_response() {
        let handler = handler_with_fake_llm();
        let params = DebugTool {
            error: "index out of bounds".into(),
            code: "fn get(v: &[i32]) -> i32 { v[10] }".into(),
            file_path: "main.rs".into(),
        };
        let result = handler.handle_debug(params).unwrap();
        assert!(!result.content.is_empty());
    }

    #[test]
    fn testgen_requires_api_key() {
        let handler = handler_no_llm();
        let params = TestgenTool {
            code: "fn add(a: i32, b: i32) -> i32 { a + b }".into(),
            file_path: "math.rs".into(),
            framework: None,
        };
        assert!(handler.handle_testgen(params).is_err());
    }

    #[test]
    fn testgen_with_llm_returns_response() {
        let handler = handler_with_fake_llm();
        let params = TestgenTool {
            code: "def add(a, b):\n    return a + b\n".into(),
            file_path: "math.py".into(),
            framework: Some("pytest".into()),
        };
        let result = handler.handle_testgen(params).unwrap();
        assert!(!result.content.is_empty());
    }

    #[test]
    fn testgen_rejects_unsupported_extension() {
        let handler = handler_with_fake_llm();
        let params = TestgenTool {
            code: "code".into(),
            file_path: "file.xyz".into(),
            framework: None,
        };
        assert!(handler.handle_testgen(params).is_err());
    }

    // -- Cache integration --

    #[test]
    fn review_populates_parse_cache() {
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
        handler.handle_review(params).unwrap();
        assert_eq!(cache.stats().misses, 1, "First review should be a cache miss");

        // Second review with same code should hit cache
        let params2 = ReviewTool {
            code: "fn main() {}".into(),
            file_path: "test.rs".into(),
            focus: None,
        };
        handler.handle_review(params2).unwrap();
        assert_eq!(cache.stats().hits, 1, "Second review should be a cache hit");
    }
}
