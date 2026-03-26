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

struct AgentState {
    total_bytes_read: usize,
    total_tool_calls: usize,
}

impl AgentState {
    fn execute_tool_call(
        &mut self,
        tc: &crate::llm_client::ToolCall,
        tools: &ToolRegistry,
        config: &AgentConfig,
    ) -> Option<String> {
        self.total_tool_calls += 1;
        if self.total_tool_calls > config.max_tool_calls {
            eprintln!("Agent: tool call limit ({}) reached", config.max_tool_calls);
            return None;
        }

        let result = match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
            Ok(args) => {
                match tools.execute(&tc.name, &args) {
                    Ok(output) => {
                        // Truncate output to remaining byte budget before accumulating
                        let remaining = config.max_bytes_read.saturating_sub(self.total_bytes_read);
                        let output = if output.len() > remaining {
                            // Floor to char boundary to avoid splitting multi-byte chars
                            let safe_end = output.floor_char_boundary(remaining);
                            let mut truncated = output[..safe_end].to_string();
                            truncated.push_str("\n... (truncated: byte limit reached)");
                            eprintln!("Agent: byte limit ({}) reached", config.max_bytes_read);
                            truncated
                        } else {
                            output
                        };
                        self.total_bytes_read += output.len();
                        output
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            Err(e) => format!("Error: malformed arguments: {}", e),
        };
        Some(result)
    }

    fn limit_reached(&self, config: &AgentConfig) -> bool {
        self.total_tool_calls > config.max_tool_calls
            || self.total_bytes_read >= config.max_bytes_read
    }
}

fn force_final_turn(
    reviewer: &dyn AgentReviewer,
    messages: &mut Vec<serde_json::Value>,
    model: &str,
    prompt: &str,
) -> anyhow::Result<Vec<Finding>> {
    messages.push(serde_json::json!({"role": "user", "content": prompt}));
    if let Ok(crate::llm_client::LlmTurnResult::FinalContent(text)) =
        reviewer.chat_turn(messages, &serde_json::json!([]), model)
    {
        return crate::review::parse_llm_response(&text, model);
    }
    Ok(vec![])
}

fn append_assistant_tool_calls(messages: &mut Vec<serde_json::Value>, calls: &[crate::llm_client::ToolCall]) {
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

    let mut state = AgentState { total_bytes_read: 0, total_tool_calls: 0 };

    for _iteration in 0..config.max_iterations {
        let result = reviewer.chat_turn(&messages, &tool_defs, model)?;

        match result {
            crate::llm_client::LlmTurnResult::FinalContent(text) => {
                return crate::review::parse_llm_response(&text, model);
            }
            crate::llm_client::LlmTurnResult::ToolCalls(calls) => {
                append_assistant_tool_calls(&mut messages, &calls);

                for tc in &calls {
                    let tool_result = match state.execute_tool_call(tc, tools, config) {
                        Some(r) => r,
                        None => break,
                    };
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "content": tool_result
                    }));
                }

                if state.limit_reached(config) {
                    return force_final_turn(
                        reviewer, &mut messages, model,
                        "Limit reached. Produce your findings JSON array now based on what you have seen so far.",
                    );
                }
            }
        }
    }

    eprintln!("Agent: iteration limit ({}) reached", config.max_iterations);
    force_final_turn(
        reviewer, &mut messages, model,
        "You've reached the investigation limit. Produce your findings JSON array now.",
    )
}

fn agent_system_prompt(file_path: &str) -> String {
    format!(
        "You are a code reviewer performing deep analysis of `{path}`. \
         You MUST use the provided tools to investigate before producing findings. \
         \n\nWorkflow:\
         \n1. First, call list_files to see the project structure.\
         \n2. Use read_file to examine files that `{path}` imports, calls, or depends on.\
         \n3. Use grep to search for callers, related patterns, or configuration.\
         \n4. Only after investigating context, produce your final response.\
         \n\nYour final response must be a JSON array of findings. \
         Each finding: title, description, severity (critical/high/medium/low/info), \
         category, line_start, line_end. If no issues: []\
         \n\nDo NOT produce findings without first using at least one tool to gather context.",
        path = file_path
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

    #[test]
    fn agent_loop_max_iterations_stops() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.py"), "x = 1").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let config = AgentConfig { max_iterations: 1, max_tool_calls: 10, max_bytes_read: 50_000 };

        let reviewer = FakeAgentReviewer::new(vec![
            LlmTurnResult::ToolCalls(vec![ToolCall {
                id: "c1".into(), name: "read_file".into(),
                arguments: r#"{"path":"test.py"}"#.into(),
            }]),
            // This turn is the forced "produce findings" turn after limit
            LlmTurnResult::FinalContent("[]".into()),
        ]);

        let result = agent_loop("x = 1", "test.py", &reviewer, "m", &tools, &config).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn agent_loop_max_tool_calls_stops() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.py"), "x").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let config = AgentConfig { max_iterations: 5, max_tool_calls: 2, max_bytes_read: 50_000 };

        let reviewer = FakeAgentReviewer::new(vec![
            // 3 tool calls in one turn — exceeds limit of 2
            LlmTurnResult::ToolCalls(vec![
                ToolCall { id: "c1".into(), name: "read_file".into(), arguments: r#"{"path":"a.py"}"#.into() },
                ToolCall { id: "c2".into(), name: "read_file".into(), arguments: r#"{"path":"a.py"}"#.into() },
                ToolCall { id: "c3".into(), name: "read_file".into(), arguments: r#"{"path":"a.py"}"#.into() },
            ]),
            LlmTurnResult::FinalContent("[]".into()),
        ]);

        let result = agent_loop("x", "a.py", &reviewer, "m", &tools, &config).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn agent_loop_malformed_tool_arguments() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.py"), "x = 1").unwrap();
        let tools = ToolRegistry::new(dir.path());
        let config = AgentConfig::default();

        let reviewer = FakeAgentReviewer::new(vec![
            LlmTurnResult::ToolCalls(vec![ToolCall {
                id: "c1".into(), name: "read_file".into(),
                arguments: "not valid json{{{".into(),
            }]),
            LlmTurnResult::FinalContent("[]".into()),
        ]);

        let result = agent_loop("x = 1", "test.py", &reviewer, "m", &tools, &config).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn agent_loop_tool_execution_error_continues() {
        let dir = TempDir::new().unwrap();
        let tools = ToolRegistry::new(dir.path());
        let config = AgentConfig::default();

        let reviewer = FakeAgentReviewer::new(vec![
            LlmTurnResult::ToolCalls(vec![ToolCall {
                id: "c1".into(), name: "read_file".into(),
                arguments: r#"{"path":"nonexistent.py"}"#.into(),
            }]),
            LlmTurnResult::FinalContent("[]".into()),
        ]);

        let result = agent_loop("x", "test.py", &reviewer, "m", &tools, &config).unwrap();
        assert!(result.is_empty());
    }
}
