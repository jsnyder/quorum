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

use crate::config::{Config, EnvConfigSource};
use crate::feedback::{FeedbackEntry, FeedbackStore, Verdict};
use crate::llm_client::OpenAiClient;
use crate::mcp::tools::{CatalogTool, FeedbackTool, ReviewTool};
use crate::parser::Language;
use crate::pipeline::{self, LlmReviewer, PipelineConfig};

pub struct QuorumHandler {
    config: Config,
    feedback_store: FeedbackStore,
    llm_reviewer: Option<Box<dyn LlmReviewer>>,
}

impl QuorumHandler {
    pub fn new() -> anyhow::Result<Self> {
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
        })
    }

    fn handle_review(&self, params: ReviewTool) -> Result<CallToolResult, String> {
        let lang = Language::from_path(std::path::Path::new(&params.file_path))
            .ok_or_else(|| format!("Unsupported file type: {}", params.file_path))?;

        let tree = crate::parser::parse(&params.code, lang)
            .map_err(|e| format!("Parse error: {}", e))?;

        let pipeline_cfg = PipelineConfig {
            models: vec![self.config.model.clone()],
            ..Default::default()
        };

        let result = pipeline::review_file(
            std::path::Path::new(&params.file_path),
            &params.code,
            lang,
            &tree,
            self.llm_reviewer.as_deref(),
            &pipeline_cfg,
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
                "Supported languages:\n- Rust (.rs)\n- Python (.py)\n- TypeScript (.ts)\n- TSX (.tsx)".to_string()
            }
            "domains" => {
                "Domain detection not yet implemented. Coming in Phase 2.".to_string()
            }
            other => format!("Unknown catalog query: {}. Use: models, languages, or domains.", other),
        };
        Ok(CallToolResult::text_content(vec![result.into()]))
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
        };

        let params = ReviewTool {
            code: "some code".into(),
            file_path: "file.xyz".into(),
            focus: None,
        };

        assert!(handler.handle_review(params).is_err());
    }
}
