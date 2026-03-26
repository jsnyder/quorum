/// OpenAI-compatible LLM client for code review.
/// Supports both Chat Completions API (/v1/chat/completions) and
/// Responses API (/v1/responses) for models like gpt-5.3-codex.

use crate::pipeline::LlmReviewer;

/// Models that require the Responses API instead of Chat Completions.
const RESPONSES_API_MODELS: &[&str] = &[
    "gpt-5.3-codex",
    "gpt-5.1-codex",
    "gpt-5.1-codex-mini",
    "gpt-5-codex",
];

pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    reasoning_effort: Option<String>,
}

impl OpenAiClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            reasoning_effort: None,
        }
    }

    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    fn needs_responses_api(model: &str) -> bool {
        RESPONSES_API_MODELS.iter().any(|m| *m == model)
    }

    async fn call_model(&self, model: &str, prompt: &str) -> anyhow::Result<String> {
        if Self::needs_responses_api(model) {
            self.responses_api(model, prompt).await
        } else {
            self.chat_completion(model, prompt).await
        }
    }

    async fn chat_completion(&self, model: &str, prompt: &str) -> anyhow::Result<String> {
        let system_msg = Self::system_prompt();

        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_msg},
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.3,
            "max_tokens": 16384
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = serde_json::Value::String(effort.clone());
        }

        let url = format!("{}/chat/completions", self.base_url);
        let resp = self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            let truncated: String = error_text.chars().take(200).collect();
            anyhow::bail!("API Error ({}): {}", status.as_u16(), truncated);
        }

        let json: serde_json::Value = resp.json().await?;

        let finish_reason = json["choices"][0]["finish_reason"].as_str().unwrap_or("unknown");
        if finish_reason == "length" {
            anyhow::bail!("Response truncated (finish_reason=length). Model {} may need a higher max_tokens.", model);
        }

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!(
                "Unexpected API response structure: no choices[0].message.content"
            ))?;

        Ok(content.to_string())
    }

    /// OpenAI Responses API (/v1/responses) for codex and other responses-only models.
    async fn responses_api(&self, model: &str, prompt: &str) -> anyhow::Result<String> {
        let mut body = serde_json::json!({
            "model": model,
            "instructions": Self::system_prompt(),
            "input": prompt,
            "max_output_tokens": 16384,
            "store": false
        });
        // Codex models don't support temperature; only add for non-codex responses API models
        if !model.contains("codex") {
            body["temperature"] = serde_json::json!(0.3);
        }
        if let Some(effort) = &self.reasoning_effort {
            // Responses API uses nested reasoning.effort format
            body["reasoning"] = serde_json::json!({ "effort": effort });
        }

        let url = format!("{}/responses", self.base_url);
        let resp = self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            let truncated: String = error_text.chars().take(200).collect();
            anyhow::bail!("Responses API Error ({}): {}", status.as_u16(), truncated);
        }

        let json: serde_json::Value = resp.json().await?;

        if json["status"].as_str() == Some("incomplete") {
            let reason = json["incomplete_details"].to_string();
            anyhow::bail!("Response incomplete: {}", reason);
        }

        // Extract and concatenate all text from output[].content[].text
        let output = json["output"].as_array()
            .ok_or_else(|| anyhow::anyhow!("No output in Responses API response"))?;

        let mut texts = Vec::new();
        for item in output {
            if item["type"].as_str() == Some("message") {
                if let Some(content) = item["content"].as_array() {
                    for block in content {
                        if block["type"].as_str() == Some("output_text") {
                            if let Some(text) = block["text"].as_str() {
                                texts.push(text.to_string());
                            }
                        }
                    }
                }
            }
        }

        if texts.is_empty() {
            anyhow::bail!("No text content in Responses API output");
        }
        Ok(texts.join("\n"))
    }

    /// Send a chat completion request with tool definitions.
    /// Returns either final text content or a list of tool calls the model wants to make.
    pub async fn chat_with_tools(
        &self,
        messages: &[serde_json::Value],
        tools: &serde_json::Value,
        model: &str,
    ) -> anyhow::Result<LlmTurnResult> {
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": 0.3,
            "max_tokens": 16384,
            "tools": tools
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = serde_json::Value::String(effort.clone());
        }

        let url = format!("{}/chat/completions", self.base_url);
        let resp = self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            let truncated: String = error_text.chars().take(200).collect();
            anyhow::bail!("API Error ({}): {}", status.as_u16(), truncated);
        }

        let json: serde_json::Value = resp.json().await?;
        let message = &json["choices"][0]["message"];
        let finish_reason = json["choices"][0]["finish_reason"].as_str().unwrap_or("stop");

        // Check for tool calls
        if let Some(tool_calls) = message["tool_calls"].as_array() {
            if !tool_calls.is_empty() {
                let calls = tool_calls.iter().filter_map(|tc| {
                    let id = tc["id"].as_str()?.to_string();
                    let name = tc["function"]["name"].as_str()?.to_string();
                    let arguments = tc["function"]["arguments"].as_str()?.to_string();
                    Some(ToolCall { id, name, arguments })
                }).collect();
                return Ok(LlmTurnResult::ToolCalls(calls));
            }
        }

        // Otherwise, return final content
        let content = message["content"].as_str().unwrap_or("");
        if finish_reason == "length" {
            anyhow::bail!("Response truncated (finish_reason=length)");
        }
        Ok(LlmTurnResult::FinalContent(content.to_string()))
    }

    fn system_prompt() -> &'static str {
        concat!(
            "You are a code reviewer. Respond ONLY with a JSON array of findings. ",
            "Each finding must have: title (string), description (string), ",
            "severity (critical/high/medium/low/info), category (string), ",
            "line_start (number), line_end (number). ",
            "If no issues found, respond with an empty array: []"
        )
    }
}

/// Bridges sync LlmReviewer trait to async HTTP calls.
/// Uses block_in_place on multi-thread runtime, spawns a new runtime on current-thread.
impl LlmReviewer for OpenAiClient {
    fn review(&self, prompt: &str, model: &str) -> anyhow::Result<String> {
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                // Inside a tokio runtime — use block_in_place (safe on multi-thread)
                tokio::task::block_in_place(|| rt.block_on(self.call_model(model, prompt)))
            }
            Err(_) => {
                // No runtime — create a temporary one
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| anyhow::anyhow!("Failed to create tokio runtime: {}", e))?;
                rt.block_on(self.call_model(model, prompt))
            }
        }
    }
}

impl crate::agent::AgentReviewer for OpenAiClient {
    fn chat_turn(
        &self,
        messages: &[serde_json::Value],
        tools: &serde_json::Value,
        model: &str,
    ) -> anyhow::Result<LlmTurnResult> {
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => tokio::task::block_in_place(|| {
                rt.block_on(self.chat_with_tools(messages, tools, model))
            }),
            Err(_) => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(self.chat_with_tools(messages, tools, model))
            }
        }
    }
}

/// Format tool definitions for OpenAI function calling API.
pub fn format_tools_for_api(tools: &[crate::tools::ToolDefinition]) -> serde_json::Value {
    serde_json::Value::Array(
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": &t.name,
                        "description": &t.description,
                        "parameters": &t.parameters,
                    }
                })
            })
            .collect(),
    )
}

/// Response from a tool-calling LLM turn.
#[derive(Debug)]
pub enum LlmTurnResult {
    /// Model produced final text content.
    FinalContent(String),
    /// Model wants to call tools.
    ToolCalls(Vec<ToolCall>),
}

/// A single tool call requested by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_creation() {
        let client = OpenAiClient::new("https://api.example.com/v1", "sk-test");
        assert_eq!(client.base_url, "https://api.example.com/v1");
        assert_eq!(client.api_key, "sk-test");
    }

    #[test]
    fn tool_definitions_format_for_openai() {
        use crate::tools::ToolDefinition;
        let tools = vec![ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let formatted = format_tools_for_api(&tools);
        let arr = formatted.as_array().unwrap();
        assert_eq!(arr[0]["type"], "function");
        assert_eq!(arr[0]["function"]["name"], "read_file");
        assert_eq!(arr[0]["function"]["description"], "Read a file");
        assert!(arr[0]["function"]["parameters"]["properties"]["path"].is_object());
    }

    // Integration tests requiring a real API endpoint are in tests/llm_integration.rs
    // and gated behind the QUORUM_API_KEY env var check.
}
