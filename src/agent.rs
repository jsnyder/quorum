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
    config: &AgentConfig,
) -> anyhow::Result<Vec<Finding>> {
    let tool_defs = crate::llm_client::format_tools_for_api(&tools.tool_definitions());

    let mut messages = vec![
        serde_json::json!({"role": "system", "content": agent_system_prompt(file_path)}),
        serde_json::json!({"role": "user", "content": format!(
            "Review this code thoroughly:\n```\n{}\n```", code
        )}),
    ];

    let mut total_bytes_read: usize = 0;
    let mut total_tool_calls: usize = 0;

    for _iteration in 0..config.max_iterations {
        let result = reviewer.chat_turn(&messages, &tool_defs, model)?;

        match result {
            crate::llm_client::LlmTurnResult::FinalContent(text) => {
                return crate::review::parse_llm_response(&text, model);
            }
            crate::llm_client::LlmTurnResult::ToolCalls(calls) => {
                // Build assistant message with tool_calls
                let tc_json: Vec<serde_json::Value> = calls.iter().map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {"name": tc.name, "arguments": tc.arguments}
                    })
                }).collect();
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": tc_json
                }));

                // Execute each tool call
                for tc in &calls {
                    total_tool_calls += 1;
                    if total_tool_calls > config.max_tool_calls {
                        eprintln!("Agent: tool call limit ({}) reached", config.max_tool_calls);
                        break;
                    }

                    let tool_result = match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        Ok(args) => {
                            match tools.execute(&tc.name, &args) {
                                Ok(output) => {
                                    total_bytes_read += output.len();
                                    if total_bytes_read > config.max_bytes_read {
                                        eprintln!("Agent: byte limit ({}) reached", config.max_bytes_read);
                                        format!("Error: byte read limit exceeded ({}/{})", total_bytes_read, config.max_bytes_read)
                                    } else {
                                        output
                                    }
                                }
                                Err(e) => format!("Error: {}", e),
                            }
                        }
                        Err(e) => format!("Error: malformed arguments: {}", e),
                    };

                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "content": tool_result
                    }));
                }

                // Check limits
                if total_tool_calls > config.max_tool_calls || total_bytes_read > config.max_bytes_read {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": "Limit reached. Produce your findings JSON array now based on what you have seen so far."
                    }));
                    if let Ok(crate::llm_client::LlmTurnResult::FinalContent(text)) =
                        reviewer.chat_turn(&messages, &serde_json::json!([]), model)
                    {
                        return crate::review::parse_llm_response(&text, model);
                    }
                    return Ok(vec![]);
                }
            }
        }
    }

    // Max iterations reached
    eprintln!("Agent: iteration limit ({}) reached", config.max_iterations);
    messages.push(serde_json::json!({
        "role": "user",
        "content": "You've reached the investigation limit. Produce your findings JSON array now."
    }));
    if let Ok(crate::llm_client::LlmTurnResult::FinalContent(text)) =
        reviewer.chat_turn(&messages, &serde_json::json!([]), model)
    {
        return crate::review::parse_llm_response(&text, model);
    }
    Ok(vec![])
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
    use crate::llm_client::{LlmTurnResult, ToolCall};
    use std::collections::VecDeque;
    use std::sync::Mutex;

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
    fn agent_loop_single_tool_round() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("auth.py"), "SECRET = 'hunter2'\ndef login(): pass\n").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let config = AgentConfig::default();

        let reviewer = FakeAgentReviewer::new(vec![
            LlmTurnResult::ToolCalls(vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path": "auth.py"}"#.into(),
            }]),
            LlmTurnResult::FinalContent(
                r#"[{"title":"Hardcoded secret","description":"SECRET contains plaintext password","severity":"high","category":"security","line_start":1,"line_end":1}]"#.into()
            ),
        ]);

        let result = agent_loop(
            "SECRET = 'hunter2'\ndef login(): pass\n",
            "auth.py", &reviewer, "gpt-5.4", &tools, &config,
        ).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].title.contains("secret") || result[0].title.contains("Secret"));
    }

    #[test]
    fn agent_loop_message_history_correct() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.py"), "x = 1").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let config = AgentConfig::default();

        struct CapturingReviewer {
            turns: Mutex<VecDeque<LlmTurnResult>>,
            captured_messages: Mutex<Vec<Vec<serde_json::Value>>>,
        }
        impl AgentReviewer for CapturingReviewer {
            fn chat_turn(&self, messages: &[serde_json::Value], _tools: &serde_json::Value, _model: &str)
                -> anyhow::Result<LlmTurnResult>
            {
                self.captured_messages.lock().unwrap().push(messages.to_vec());
                let mut q = self.turns.lock().unwrap();
                Ok(q.pop_front().unwrap_or(LlmTurnResult::FinalContent("[]".into())))
            }
        }

        let reviewer = CapturingReviewer {
            turns: Mutex::new(VecDeque::from([
                LlmTurnResult::ToolCalls(vec![ToolCall {
                    id: "call_1".into(), name: "read_file".into(),
                    arguments: r#"{"path": "test.py"}"#.into(),
                }]),
                LlmTurnResult::FinalContent("[]".into()),
            ])),
            captured_messages: Mutex::new(Vec::new()),
        };

        agent_loop("x = 1", "test.py", &reviewer, "m", &tools, &config).unwrap();

        let captures = reviewer.captured_messages.lock().unwrap();
        assert_eq!(captures.len(), 2, "Should have 2 turns");

        // Second turn should have: system, user, assistant(tool_calls), tool(result)
        let turn2 = &captures[1];
        assert!(turn2.len() >= 4, "Turn 2 should have system + user + assistant + tool messages");

        let tool_msg = turn2.iter().find(|m| m["role"] == "tool").expect("Should have tool role message");
        assert_eq!(tool_msg["tool_call_id"], "call_1");
        assert!(tool_msg["content"].as_str().unwrap().contains("x = 1"));
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
