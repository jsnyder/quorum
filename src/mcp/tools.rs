/// MCP tool definitions for quorum.

use rust_mcp_sdk::macros::{mcp_tool, JsonSchema};
use serde::{Deserialize, Serialize};

#[mcp_tool(
    name = "review",
    description = "Review code for bugs, security issues, and quality problems. Returns structured findings with severity, category, and line numbers."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

/// Strict wire contract for `FeedbackTool.verdict` (issue #94).
///
/// The MCP JSON-Schema surfaced the field as an unconstrained `string` when
/// this was a plain `String`, so schema-driven clients couldn't discover that
/// only five values were accepted. Enum variants serialize via `snake_case`
/// to exactly the five valid wire strings: `tp`, `fp`, `partial`, `wontfix`,
/// `context_misleading`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackVerdict {
    Tp,
    Fp,
    Partial,
    Wontfix,
    ContextMisleading,
}

#[mcp_tool(
    name = "feedback",
    description = "Record whether a review finding was a true positive (tp), false positive (fp), partial, wontfix, or context_misleading. Improves future reviews."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackTool {
    /// File path the finding was about
    #[serde(rename = "filePath")]
    pub file_path: String,
    /// The finding title/message
    pub finding: String,
    /// Verdict: one of `tp`, `fp`, `partial`, `wontfix`, `context_misleading`.
    pub verdict: FeedbackVerdict,
    /// Reason for the verdict
    pub reason: String,
    /// Which model produced the finding (optional)
    #[serde(default)]
    pub model: Option<String>,
    /// Chunk IDs blamed for misleading context. Only meaningful with
    /// `verdict = "context_misleading"`. May be empty or omitted.
    #[serde(default, rename = "blamedChunks")]
    pub blamed_chunks: Option<Vec<String>>,
    /// Optional: record as an external review agent's verdict (pal, third-opinion,
    /// reviewdog, etc.) instead of a human verdict. When set, the entry is
    /// persisted with External provenance and counts toward the external-agent
    /// calibration weight.
    #[serde(default, rename = "fromAgent")]
    pub from_agent: Option<String>,
    /// Optional: the LLM model the external agent used (only meaningful with
    /// `from_agent`).
    #[serde(default, rename = "agentModel")]
    pub agent_model: Option<String>,
    /// Optional: agent-reported confidence. Accepted as an unconstrained
    /// `Option<f32>` at the MCP boundary; `FeedbackStore::record_external`
    /// drops non-finite values and clamps finite values to [0,1] before
    /// persistence. Ignored by the calibrator in v1; stored for analytics only.
    #[serde(default)]
    pub confidence: Option<f32>,
    /// Optional: finding category (e.g. "security", "correctness"). Only
    /// honored on the External path (when `from_agent` is set). Without it,
    /// External entries get the canonical default "unknown"; Human entries
    /// retain the existing empty-string default.
    #[serde(default)]
    pub category: Option<String>,
    /// Optional: discriminate the FP verdict by reason (#123 Layer 1).
    /// Only meaningful when `verdict = "fp"`; silently dropped on other
    /// verdicts. Variants serialize via the same `snake_case` wire
    /// representation as the underlying `feedback::FpKind` enum, so
    /// schema-driven clients see strings like `"hallucination"`,
    /// `"trust_model_assumption"`, or struct payloads
    /// `{"compensating_control": {"reference": "PR #99"}}`.
    /// Required associated data missing (e.g. `compensating_control`
    /// without `reference`) is rejected by serde at deserialization.
    #[serde(default, rename = "fpKind", skip_serializing_if = "Option::is_none")]
    pub fp_kind: Option<crate::feedback::FpKind>,
}

#[mcp_tool(
    name = "catalog",
    description = "List available models, supported languages, and detectable domains/frameworks."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogTool {
    /// What to list: models, languages, or domains
    pub query: String,
}

#[mcp_tool(
    name = "chat",
    description = "Ask questions about code. Provide a file for context and ask anything about its behavior, design, or potential issues."
)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
        assert_eq!(tool.verdict, FeedbackVerdict::Tp);
    }

    // Issue #94: verdict was a plain String; MCP JSON-Schema advertised
    // "string (any)" even though the runtime handler rejected unknowns.
    // Schema-driven clients couldn't discover the constraint. Now the field
    // is a strict enum with snake_case variants.

    #[test]
    fn feedback_verdict_accepts_all_canonical_strings() {
        for (s, expected) in [
            (r#""tp""#, FeedbackVerdict::Tp),
            (r#""fp""#, FeedbackVerdict::Fp),
            (r#""partial""#, FeedbackVerdict::Partial),
            (r#""wontfix""#, FeedbackVerdict::Wontfix),
            (r#""context_misleading""#, FeedbackVerdict::ContextMisleading),
        ] {
            let v: FeedbackVerdict =
                serde_json::from_str(s).unwrap_or_else(|e| panic!("{s} should parse: {e}"));
            assert_eq!(v, expected);
        }
    }

    #[test]
    fn feedback_verdict_rejects_unknown_at_parse_time() {
        let result: Result<FeedbackVerdict, _> = serde_json::from_str(r#""maybe""#);
        assert!(result.is_err(), "unknown variant must fail parse");
    }

    #[test]
    fn feedback_verdict_is_strict_on_case_and_whitespace() {
        // MCP boundary is strict: uppercase and whitespace variants must
        // fail. The CLI is separately permissive (issue #90), but the MCP
        // wire format is a machine contract.
        for bad in [r#""TP""#, r#""Tp""#, r#"" tp ""#] {
            let result: Result<FeedbackVerdict, _> = serde_json::from_str(bad);
            assert!(
                result.is_err(),
                "MCP boundary must reject non-canonical {bad}"
            );
        }
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

    // Issue #134: MCP boundary must reject unknown fields. Without
    // `deny_unknown_fields`, a typo such as `"filepath"` (lowercase 'p')
    // silently deserializes to a struct missing `file_path`, and the schema
    // surfaces no error. All six tool structs are wire contracts for
    // schema-driven clients; misspellings should fail loudly at parse time.
    //
    // Each test sends an otherwise-valid payload with one extra field that
    // is *not* declared on the struct.

    #[test]
    fn review_tool_rejects_unknown_field() {
        let json = r#"{"code":"fn main(){}","filePath":"src/main.rs","bogus":"x"}"#;
        let result: Result<ReviewTool, _> = serde_json::from_str(json);
        assert!(result.is_err(), "ReviewTool must reject unknown field");
    }

    #[test]
    fn feedback_tool_rejects_unknown_field() {
        // Plausible typo: `findingTitle` (correct field is `finding`).
        let json = r#"{"filePath":"src/auth.rs","finding":"SQLi","verdict":"tp","reason":"r","findingTitle":"oops"}"#;
        let result: Result<FeedbackTool, _> = serde_json::from_str(json);
        assert!(result.is_err(), "FeedbackTool must reject unknown field");
    }

    #[test]
    fn catalog_tool_rejects_unknown_field() {
        let json = r#"{"query":"models","extra":1}"#;
        let result: Result<CatalogTool, _> = serde_json::from_str(json);
        assert!(result.is_err(), "CatalogTool must reject unknown field");
    }

    #[test]
    fn chat_tool_rejects_unknown_field() {
        let json = r#"{"question":"why?","unexpected":true}"#;
        let result: Result<ChatTool, _> = serde_json::from_str(json);
        assert!(result.is_err(), "ChatTool must reject unknown field");
    }

    #[test]
    fn debug_tool_rejects_unknown_field() {
        let json = r#"{"error":"NPE","code":"x()","filePath":"a.rs","stack":"..."}"#;
        let result: Result<DebugTool, _> = serde_json::from_str(json);
        assert!(result.is_err(), "DebugTool must reject unknown field");
    }

    #[test]
    fn testgen_tool_rejects_unknown_field() {
        let json = r#"{"code":"x","filePath":"a.py","language":"python"}"#;
        let result: Result<TestgenTool, _> = serde_json::from_str(json);
        assert!(result.is_err(), "TestgenTool must reject unknown field");
    }

    // Happy-path acceptance: deny_unknown_fields must NOT break payloads
    // that exercise every declared field.

    #[test]
    fn feedback_tool_accepts_all_declared_fields() {
        let json = r#"{
            "filePath":"src/a.rs",
            "finding":"x",
            "verdict":"fp",
            "reason":"r",
            "model":"gpt-5.4",
            "blamedChunks":["c1"],
            "fromAgent":"pal",
            "agentModel":"gemini",
            "confidence":0.9,
            "category":"security",
            "fpKind":"hallucination"
        }"#;
        let tool: FeedbackTool = serde_json::from_str(json).expect("all declared fields must parse");
        assert_eq!(tool.verdict, FeedbackVerdict::Fp);
        assert_eq!(tool.from_agent.as_deref(), Some("pal"));
    }

    #[test]
    fn review_tool_accepts_all_declared_fields() {
        let json = r#"{"code":"fn x(){}","filePath":"a.rs","focus":"security"}"#;
        let tool: ReviewTool = serde_json::from_str(json).expect("declared fields must parse");
        assert_eq!(tool.focus.as_deref(), Some("security"));
    }
}
