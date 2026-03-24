/// MCP tool definitions for quorum.

use rust_mcp_sdk::macros::{mcp_tool, JsonSchema};
use serde::{Deserialize, Serialize};

#[mcp_tool(
    name = "review",
    description = "Review code for bugs, security issues, and quality problems. Returns structured findings with severity, category, and line numbers."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReviewTool {
    /// The code to review
    pub code: String,
    /// File path for context (e.g., "src/auth.rs")
    #[serde(rename = "filePath")]
    pub file_path: String,
    /// Focus areas: security, performance, style, best-practices (comma-separated)
    #[serde(default)]
    pub focus: Option<String>,
}

#[mcp_tool(
    name = "feedback",
    description = "Record whether a review finding was a true positive (tp), false positive (fp), partial, or wontfix. Improves future reviews."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackTool {
    /// File path the finding was about
    #[serde(rename = "filePath")]
    pub file_path: String,
    /// The finding title/message
    pub finding: String,
    /// Verdict: tp, fp, partial, or wontfix
    pub verdict: String,
    /// Reason for the verdict
    pub reason: String,
    /// Which model produced the finding (optional)
    #[serde(default)]
    pub model: Option<String>,
}

#[mcp_tool(
    name = "catalog",
    description = "List available models, supported languages, and detectable domains/frameworks."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CatalogTool {
    /// What to list: models, languages, or domains
    pub query: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_tool_has_correct_name() {
        let tool = ReviewTool::tool();
        assert_eq!(tool.name, "review");
    }

    #[test]
    fn feedback_tool_has_correct_name() {
        let tool = FeedbackTool::tool();
        assert_eq!(tool.name, "feedback");
    }

    #[test]
    fn catalog_tool_has_correct_name() {
        let tool = CatalogTool::tool();
        assert_eq!(tool.name, "catalog");
    }

    #[test]
    fn review_tool_has_description() {
        let tool = ReviewTool::tool();
        assert!(tool.description.as_ref().unwrap().contains("Review code"));
    }

    #[test]
    fn review_tool_deserializes_input() {
        let json = r#"{"code":"fn main() {}","filePath":"src/main.rs","focus":"security"}"#;
        let tool: ReviewTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.file_path, "src/main.rs");
        assert_eq!(tool.focus, Some("security".into()));
    }

    #[test]
    fn feedback_tool_deserializes_input() {
        let json = r#"{"filePath":"src/auth.rs","finding":"SQL injection","verdict":"tp","reason":"Fixed"}"#;
        let tool: FeedbackTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.verdict, "tp");
    }
}
