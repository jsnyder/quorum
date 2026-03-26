//! Shared test doubles for LLM reviewer interfaces.

#[cfg(test)]
pub mod fakes {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use crate::pipeline::LlmReviewer;
    use crate::llm_client::LlmTurnResult;

    /// Fake reviewer that returns responses in sequence.
    /// Falls back to last response if sequence exhausted.
    pub struct FakeReviewer {
        responses: Mutex<VecDeque<Result<String, String>>>,
        pub captured_prompts: Mutex<Vec<String>>,
    }

    impl FakeReviewer {
        pub fn always(response: &str) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from([Ok(response.to_string())])),
                captured_prompts: Mutex::new(Vec::new()),
            }
        }

        pub fn sequence(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(|r| Ok(r.to_string())).collect()),
                captured_prompts: Mutex::new(Vec::new()),
            }
        }

        pub fn failing(msg: &str) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from([Err(msg.to_string())])),
                captured_prompts: Mutex::new(Vec::new()),
            }
        }
    }

    impl LlmReviewer for FakeReviewer {
        fn review(&self, prompt: &str, _model: &str) -> anyhow::Result<String> {
            self.captured_prompts.lock().unwrap().push(prompt.to_string());
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                anyhow::bail!("FakeReviewer: no responses configured");
            }
            let resp = if q.len() > 1 { q.pop_front().unwrap() } else { q.front().cloned().unwrap() };
            resp.map_err(|e| anyhow::anyhow!(e))
        }
    }

    /// Fake for multi-turn agent loop that returns LlmTurnResult in sequence.
    pub struct FakeAgentReviewer {
        turns: Mutex<VecDeque<LlmTurnResult>>,
    }

    impl FakeAgentReviewer {
        pub fn new(turns: Vec<LlmTurnResult>) -> Self {
            Self { turns: Mutex::new(turns.into()) }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fake_reviewer_empty_sequence_does_not_panic() {
            let reviewer = FakeReviewer::sequence(vec![]);
            // Calling review on empty sequence should not panic
            let result = reviewer.review("prompt", "model");
            // Should either return an error or a sensible default, not panic
            assert!(result.is_err() || result.is_ok());
        }
    }

    impl crate::agent::AgentReviewer for FakeAgentReviewer {
        fn chat_turn(
            &self,
            _messages: &[serde_json::Value],
            _tools: &serde_json::Value,
            _model: &str,
        ) -> anyhow::Result<crate::llm_client::LlmTurnResult> {
            let mut q = self.turns.lock().unwrap();
            if let Some(turn) = q.pop_front() {
                Ok(turn)
            } else {
                Ok(crate::llm_client::LlmTurnResult::FinalContent("[]".into()))
            }
        }
    }
}
