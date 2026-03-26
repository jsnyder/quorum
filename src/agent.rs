/// Bounded agent loop for deep code review.
/// Gives the LLM tools to investigate the codebase before producing findings.
/// Bounded by max iterations, max tool calls, and max bytes read.

use crate::finding::Finding;
use crate::pipeline::LlmReviewer;
use crate::tools::ToolRegistry;

/// Trait for multi-turn LLM interaction with tool calling.
pub trait AgentReviewer: Send + Sync {
    fn chat_turn(
        &self,
        messages: &[serde_json::Value],
        tools: &serde_json::Value,
        model: &str,
    ) -> anyhow::Result<crate::llm_client::LlmTurnResult>;
}

pub struct AgentConfig {
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub max_bytes_read: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            max_tool_calls: 10,
            max_bytes_read: 50_000,
        }
    }
}

/// Run a tool-calling agent loop for deep code review.
/// The LLM can call read_file/grep/list_files to investigate before producing findings.
///
/// Current implementation: single-pass with tool context in the system prompt.
/// Future: multi-turn loop using chat_with_tools for iterative investigation.
pub fn agent_review(
    code: &str,
    file_path: &str,
    reviewer: &dyn LlmReviewer,
    model: &str,
    tools: &ToolRegistry,
    config: &AgentConfig,
) -> anyhow::Result<Vec<Finding>> {
    // Build tool descriptions for the prompt
    let tool_descriptions: String = tools
        .tool_definitions()
        .iter()
        .map(|t| format!("- {}: {}", t.name, t.description))
        .collect::<Vec<_>>()
        .join("\n");

    // Get project file listing for context (bounded by config)
    let file_listing = tools
        .execute("list_files", &serde_json::json!({}))
        .unwrap_or_else(|_| "Unable to list files.".into());

    let truncated_listing = if file_listing.len() > config.max_bytes_read / 2 {
        format!(
            "{}\n... (truncated)",
            &file_listing[..config.max_bytes_read / 2]
        )
    } else {
        file_listing
    };

    let prompt = format!(
        "You are performing a deep code review of `{file_path}`. \
         You have access to the following tools for investigating the codebase:\n\
         {tool_descriptions}\n\n\
         ## Project files\n```\n{truncated_listing}\n```\n\n\
         Review this code thoroughly. Consider how it interacts with the rest of the codebase. \
         Respond with a JSON array of findings. Each finding must have: \
         title (string), description (string), severity (critical/high/medium/low/info), \
         category (string), line_start (number), line_end (number). \
         If no issues found, respond with an empty array: []\n\n\
         ## Code under review\n```\n{code}\n```"
    );

    let response = reviewer.review(&prompt, model)?;
    crate::review::parse_llm_response(&response, model)
}

/// Run a multi-turn agent loop for deep code review.
pub fn agent_loop(
    code: &str,
    file_path: &str,
    reviewer: &dyn AgentReviewer,
    model: &str,
    tools: &ToolRegistry,
    _config: &AgentConfig,
) -> anyhow::Result<Vec<Finding>> {
    let system_msg = serde_json::json!({"role": "system", "content": agent_system_prompt(file_path)});
    let user_msg = serde_json::json!({"role": "user", "content": format!("Review this code thoroughly:\n```\n{}\n```", code)});
    let messages = vec![system_msg, user_msg];
    let tool_defs = crate::llm_client::format_tools_for_api(&tools.tool_definitions());

    match reviewer.chat_turn(&messages, &tool_defs, model)? {
        crate::llm_client::LlmTurnResult::FinalContent(text) => {
            crate::review::parse_llm_response(&text, model)
        }
        crate::llm_client::LlmTurnResult::ToolCalls(_) => {
            // Will implement iteration in Task 3
            Ok(vec![])
        }
    }
}

fn agent_system_prompt(file_path: &str) -> String {
    format!(
        "You are a code reviewer performing deep analysis of `{}`. \
         You have tools to investigate the codebase. Use them to understand context \
         before producing findings. When done investigating, respond with a JSON array \
         of findings. Each finding: title, description, severity, category, line_start, line_end. \
         If no issues: []",
        file_path
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::test_support::fakes::{FakeReviewer, FakeAgentReviewer};
    use crate::llm_client::LlmTurnResult;

    #[test]
    fn agent_loop_no_tool_calls_returns_findings() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.py"), "eval(input())").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let config = AgentConfig::default();

        let reviewer = FakeAgentReviewer::new(vec![
            LlmTurnResult::FinalContent(
                r#"[{"title":"Dangerous eval","description":"eval on user input","severity":"critical","category":"security","line_start":1,"line_end":1}]"#.into()
            ),
        ]);

        let result = agent_loop("eval(input())", "test.py", &reviewer, "gpt-5.4", &tools, &config).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Dangerous eval");
    }

    #[test]
    fn agent_returns_findings_without_tool_calls() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.py"), "x = 1").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let reviewer = FakeReviewer::always("[]");
        let config = AgentConfig::default();
        let result =
            agent_review("x = 1", "test.py", &reviewer, "gpt-5.4", &tools, &config).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn agent_returns_findings_from_llm() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.py"), "eval(input())").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let response = r#"[{"title":"Dangerous eval","description":"eval on user input","severity":"critical","category":"security","line_start":1,"line_end":1}]"#;
        let reviewer = FakeReviewer::always(response);
        let config = AgentConfig::default();
        let result = agent_review(
            "eval(input())",
            "test.py",
            &reviewer,
            "gpt-5.4",
            &tools,
            &config,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Dangerous eval");
    }

    #[test]
    fn agent_config_defaults_are_bounded() {
        let config = AgentConfig::default();
        assert!(config.max_iterations <= 5);
        assert!(config.max_tool_calls <= 15);
        assert!(config.max_bytes_read <= 100_000);
    }

    #[test]
    fn agent_includes_file_listing_in_prompt() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.py"), "x = 1").unwrap();
        std::fs::write(dir.path().join("utils.py"), "y = 2").unwrap();

        // Capture what the reviewer sees
        struct CapturingReviewer(std::sync::Mutex<String>);
        impl crate::pipeline::LlmReviewer for CapturingReviewer {
            fn review(&self, prompt: &str, _model: &str) -> anyhow::Result<String> {
                *self.0.lock().unwrap() = prompt.to_string();
                Ok("[]".into())
            }
        }

        let tools = ToolRegistry::new(dir.path());
        let reviewer = CapturingReviewer(std::sync::Mutex::new(String::new()));
        let config = AgentConfig::default();
        agent_review("x = 1", "main.py", &reviewer, "m", &tools, &config).unwrap();

        let captured = reviewer.0.lock().unwrap().clone();
        assert!(captured.contains("main.py"), "Prompt should contain file listing");
        assert!(captured.contains("deep code review"), "Prompt should mention deep review");
    }
}
