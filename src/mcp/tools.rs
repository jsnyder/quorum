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
    description = "Record whether a review finding was a true positive (tp), false positive (fp), partial, wontfix, or context_misleading. Improves future reviews."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackTool {
    /// File path the finding was about
    #[serde(rename = "filePath")]
    pub file_path: String,
    /// The finding title/message
    pub finding: String,
    /// Verdict: tp, fp, partial, wontfix, or context_misleading
    pub verdict: String,
    /// Reason for the verdict
    pub reason: String,
    /// Which model produced the finding (optional)
    #[serde(default)]
    pub model: Option<String>,
    /// Chunk IDs blamed for misleading context. Only meaningful with
    /// `verdict = "context_misleading"`. May be empty or omitted.
    #[serde(default, rename = "blamedChunks")]
    pub blamed_chunks: Option<Vec<String>>,
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

#[mcp_tool(
    name = "chat",
    description = "Ask questions about code. Provide a file for context and ask anything about its behavior, design, or potential issues."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatTool {
    /// The question to ask
    pub question: String,
    /// Code context for the question
    #[serde(default)]
    pub code: Option<String>,
    /// File path for context
    #[serde(rename = "filePath", default)]
    pub file_path: Option<String>,
}

#[mcp_tool(
    name = "debug",
    description = "Analyze code with a specific error message for debugging help. Returns potential causes and fixes."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DebugTool {
    /// The error message or stack trace
    pub error: String,
    /// The code where the error occurs
    pub code: String,
    /// File path for context
    #[serde(rename = "filePath")]
    pub file_path: String,
}

#[mcp_tool(
    name = "testgen",
    description = "Generate tests for a function or module. Returns test code in the appropriate framework."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TestgenTool {
    /// The code to generate tests for
    pub code: String,
    /// File path (used to detect language and framework)
    #[serde(rename = "filePath")]
    pub file_path: String,
    /// Test framework preference (e.g., pytest, jest, cargo-test)
    #[serde(default)]
    pub framework: Option<String>,
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

    #[test]
    fn chat_tool_has_correct_name() {
        assert_eq!(ChatTool::tool().name, "chat");
    }

    #[test]
    fn debug_tool_has_correct_name() {
        assert_eq!(DebugTool::tool().name, "debug");
    }

    #[test]
    fn testgen_tool_has_correct_name() {
        assert_eq!(TestgenTool::tool().name, "testgen");
    }

    #[test]
    fn chat_tool_optional_fields() {
        let json = r#"{"question":"What does this do?"}"#;
        let tool: ChatTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.question, "What does this do?");
        assert!(tool.code.is_none());
        assert!(tool.file_path.is_none());
    }

    #[test]
    fn debug_tool_deserializes() {
        let json = r#"{"error":"NullPointerException","code":"x.foo()","filePath":"Main.java"}"#;
        let tool: DebugTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.error, "NullPointerException");
    }

    #[test]
    fn testgen_tool_optional_framework() {
        let json = r#"{"code":"def add(a,b): return a+b","filePath":"math.py"}"#;
        let tool: TestgenTool = serde_json::from_str(json).unwrap();
        assert!(tool.framework.is_none());
    }
}
