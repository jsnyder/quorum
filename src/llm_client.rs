/// OpenAI-compatible LLM client for code review.
/// Uses reqwest directly for full control over base_url, API key, and parallel calls.

use crate::pipeline::LlmReviewer;

pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenAiClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    async fn chat_completion(&self, model: &str, prompt: &str) -> anyhow::Result<String> {
        let system_msg = concat!(
            "You are a code reviewer. Respond ONLY with a JSON array of findings. ",
            "Each finding must have: title (string), description (string), ",
            "severity (critical/high/medium/low/info), category (string), ",
            "line_start (number), line_end (number). ",
            "If no issues found, respond with an empty array: []"
        );

        let body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_msg},
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.3,
            "max_tokens": 16384
        });

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
            let truncated = if error_text.len() > 200 {
                format!("{}...", &error_text[..200])
            } else {
                error_text
            };
            anyhow::bail!("API Error ({}): {}", status.as_u16(), truncated);
        }

        let json: serde_json::Value = resp.json().await?;

        // Check for truncation
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
}

/// Uses block_in_place for safe sync-over-async in multi-threaded tokio runtime.
impl LlmReviewer for OpenAiClient {
    fn review(&self, prompt: &str, model: &str) -> anyhow::Result<String> {
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("No tokio runtime available"))?;
        tokio::task::block_in_place(|| rt.block_on(self.chat_completion(model, prompt)))
    }
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

    // Integration tests requiring a real API endpoint are in tests/llm_integration.rs
    // and gated behind the QUORUM_API_KEY env var check.
}
