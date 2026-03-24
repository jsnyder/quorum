/// LLM-powered code review using DSRs (dspy-rs).
/// Defines the review signature and handles structured output parsing.

use crate::finding::{Finding, Severity, Source};
use crate::hydration::HydrationContext;

/// Input for LLM review — code + context, after secret redaction.
#[derive(Debug, Clone)]
pub struct ReviewRequest {
    pub file_path: String,
    pub language: String,
    pub code: String,
    pub hydration_context: Option<HydrationContext>,
}

/// A single finding as returned by the LLM (before normalization).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LlmFinding {
    pub title: String,
    pub description: String,
    pub severity: String,
    pub category: String,
    pub line_start: u32,
    pub line_end: u32,
}

impl LlmFinding {
    pub fn into_finding(self, model_name: &str) -> Finding {
        let severity = match self.severity.to_lowercase().as_str() {
            "critical" => Severity::Critical,
            "high" | "error" => Severity::High,
            "medium" | "warning" | "warn" => Severity::Medium,
            "low" | "note" => Severity::Low,
            "info" | "suggestion" | "hint" => Severity::Info,
            _ => Severity::Info,
        };
        Finding {
            title: self.title,
            description: self.description,
            severity,
            category: self.category,
            source: Source::Llm(model_name.to_string()),
            line_start: self.line_start.max(1),
            line_end: self.line_end.max(self.line_start.max(1)),
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
        }
    }
}

/// Build the review prompt from a ReviewRequest.
pub fn build_review_prompt(req: &ReviewRequest) -> String {
    let mut prompt = format!(
        "Review the following {} code from `{}` for bugs, security issues, and code quality problems.\n\n",
        req.language, req.file_path
    );

    if let Some(ctx) = &req.hydration_context {
        if !ctx.callee_signatures.is_empty() {
            prompt.push_str("## Called function signatures\n");
            for sig in &ctx.callee_signatures {
                prompt.push_str(&format!("- {}\n", sig));
            }
            prompt.push('\n');
        }
        if !ctx.type_definitions.is_empty() {
            prompt.push_str("## Type definitions used\n");
            for td in &ctx.type_definitions {
                prompt.push_str(&format!("```\n{}\n```\n", td));
            }
            prompt.push('\n');
        }
        if !ctx.callers.is_empty() {
            prompt.push_str("## Functions that call into changed code\n");
            for c in &ctx.callers {
                prompt.push_str(&format!("- {}\n", c));
            }
            prompt.push('\n');
        }
    }

    prompt.push_str("## Code\n```");
    prompt.push_str(&req.language);
    prompt.push('\n');
    prompt.push_str(&req.code);
    prompt.push_str("\n```\n");

    prompt
}

/// Parse LLM JSON response into findings.
pub fn parse_llm_response(json_str: &str, model_name: &str) -> anyhow::Result<Vec<Finding>> {
    // Try to extract JSON array from the response (LLM may wrap in markdown fences)
    let cleaned = extract_json_array(json_str);

    // Try parsing directly first
    if let Ok(findings) = serde_json::from_str::<Vec<LlmFinding>>(&cleaned) {
        return Ok(findings.into_iter().map(|f| f.into_finding(model_name)).collect());
    }

    // If that fails, try sanitizing invalid JSON escapes (LLMs emit \d, \s, etc.)
    let sanitized = sanitize_json_escapes(&cleaned);
    if let Ok(findings) = serde_json::from_str::<Vec<LlmFinding>>(&sanitized) {
        return Ok(findings.into_iter().map(|f| f.into_finding(model_name)).collect());
    }

    // Last resort: try the original cleaned string for a better error message
    let llm_findings: Vec<LlmFinding> = serde_json::from_str(&cleaned)?;
    Ok(llm_findings.into_iter().map(|f| f.into_finding(model_name)).collect())
}

/// Fix invalid JSON escape sequences that LLMs produce (e.g., \d, \s, \w from regex patterns).
fn sanitize_json_escapes(json: &str) -> String {
    let mut result = String::with_capacity(json.len());
    let mut chars = json.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                match next {
                    '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u' => {
                        // Valid JSON escape — keep as-is
                        result.push(c);
                        result.push(chars.next().unwrap());
                    }
                    _ => {
                        // Invalid escape (e.g., \d, \s, \w) — escape the backslash
                        result.push('\\');
                        result.push('\\');
                        result.push(chars.next().unwrap());
                    }
                }
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Public alias for use by auto_calibrate module.
pub fn extract_json_array_public(text: &str) -> String {
    extract_json_array(text)
}

fn extract_json_array(text: &str) -> String {
    // Strip markdown code fences if present
    let text = text.trim();
    let text = if text.starts_with("```json") {
        text.trim_start_matches("```json").trim_end_matches("```").trim()
    } else if text.starts_with("```") {
        text.trim_start_matches("```").trim_end_matches("```").trim()
    } else {
        text
    };

    // Find the outermost JSON array using bracket depth tracking
    let bytes = text.as_bytes();
    let mut start = None;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;

    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if b == b'\\' && in_string {
            escape = true;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if b == b'[' {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if b == b']' {
            depth -= 1;
            if depth == 0 {
                if let Some(s) = start {
                    return text[s..=i].to_string();
                }
            }
        }
    }

    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- LlmFinding conversion --

    #[test]
    fn llm_finding_converts_to_finding() {
        let lf = LlmFinding {
            title: "SQL injection".into(),
            description: "User input in query".into(),
            severity: "critical".into(),
            category: "security".into(),
            line_start: 42,
            line_end: 50,
        };
        let f = lf.into_finding("gpt-5.4");
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.source, Source::Llm("gpt-5.4".into()));
        assert_eq!(f.line_start, 42);
    }

    #[test]
    fn llm_finding_unknown_severity_defaults_to_info() {
        let lf = LlmFinding {
            title: "T".into(),
            description: "D".into(),
            severity: "banana".into(),
            category: "c".into(),
            line_start: 1,
            line_end: 1,
        };
        assert_eq!(lf.into_finding("m").severity, Severity::Info);
    }

    #[test]
    fn llm_finding_case_insensitive_severity() {
        let lf = LlmFinding {
            title: "T".into(),
            description: "D".into(),
            severity: "HIGH".into(),
            category: "c".into(),
            line_start: 1,
            line_end: 1,
        };
        assert_eq!(lf.into_finding("m").severity, Severity::High);
    }

    // -- Prompt building --

    #[test]
    fn build_prompt_includes_code_and_path() {
        let req = ReviewRequest {
            file_path: "src/auth.rs".into(),
            language: "rust".into(),
            code: "fn login() {}".into(),
            hydration_context: None,
        };
        let prompt = build_review_prompt(&req);
        assert!(prompt.contains("src/auth.rs"));
        assert!(prompt.contains("fn login() {}"));
        assert!(prompt.contains("rust"));
    }

    #[test]
    fn build_prompt_includes_hydration_context() {
        let ctx = HydrationContext {
            callee_signatures: vec!["fn validate(input: &str) -> bool".into()],
            type_definitions: vec!["struct Request { auth: Option<String> }".into()],
            callers: vec!["handle_request".into()],
            import_targets: vec![],
        };
        let req = ReviewRequest {
            file_path: "test.rs".into(),
            language: "rust".into(),
            code: "fn process() {}".into(),
            hydration_context: Some(ctx),
        };
        let prompt = build_review_prompt(&req);
        assert!(prompt.contains("validate"));
        assert!(prompt.contains("Request"));
        assert!(prompt.contains("handle_request"));
    }

    #[test]
    fn build_prompt_skips_empty_context() {
        let ctx = HydrationContext::default();
        let req = ReviewRequest {
            file_path: "test.rs".into(),
            language: "rust".into(),
            code: "fn x() {}".into(),
            hydration_context: Some(ctx),
        };
        let prompt = build_review_prompt(&req);
        assert!(!prompt.contains("Called function signatures"));
    }

    // -- Response parsing --

    #[test]
    fn parse_clean_json_array() {
        let json = r#"[{"title":"Bug","description":"Desc","severity":"high","category":"logic","line_start":10,"line_end":15}]"#;
        let findings = parse_llm_response(json, "gpt-5.4").unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].title, "Bug");
        assert_eq!(findings[0].source, Source::Llm("gpt-5.4".into()));
    }

    #[test]
    fn parse_markdown_fenced_json() {
        let json = "```json\n[{\"title\":\"Bug\",\"description\":\"D\",\"severity\":\"medium\",\"category\":\"c\",\"line_start\":1,\"line_end\":1}]\n```";
        let findings = parse_llm_response(json, "claude").unwrap();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn parse_empty_array() {
        let findings = parse_llm_response("[]", "m").unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        assert!(parse_llm_response("not json", "m").is_err());
    }

    // -- JSON extraction --

    #[test]
    fn extract_json_from_surrounding_text() {
        let text = "Here are the findings:\n[{\"title\":\"X\",\"description\":\"Y\",\"severity\":\"low\",\"category\":\"c\",\"line_start\":1,\"line_end\":1}]\nDone.";
        let findings = parse_llm_response(text, "m").unwrap();
        assert_eq!(findings.len(), 1);
    }

    // -- Robustness edge cases --

    #[test]
    fn parse_wrapped_in_object() {
        // Some models return {"findings": [...]} instead of bare [...]
        let json = r#"{"findings":[{"title":"Bug","description":"D","severity":"high","category":"c","line_start":1,"line_end":1}]}"#;
        let findings = parse_llm_response(json, "m").unwrap();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn parse_invalid_json_escapes() {
        // LLMs sometimes emit invalid escapes like \x1b or unescaped backslashes in regex
        let json = r#"[{"title":"Bad regex pattern \\d+ in code","description":"The pattern uses \d+ which is invalid","severity":"low","category":"c","line_start":1,"line_end":1}]"#;
        let findings = parse_llm_response(json, "m").unwrap();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn parse_truncated_json_returns_error() {
        // Truncated response from max_tokens limit
        let json = r#"[{"title":"Bug","description":"This is a very long desc"#;
        assert!(parse_llm_response(json, "m").is_err());
    }

    #[test]
    fn parse_json_with_extra_fields_succeeds() {
        // Models may add extra fields we don't expect
        let json = r#"[{"title":"Bug","description":"D","severity":"high","category":"c","line_start":1,"line_end":1,"confidence":"high","suggestion":"fix it"}]"#;
        let findings = parse_llm_response(json, "m").unwrap();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn parse_severity_aliases() {
        let cases = vec![
            ("warning", Severity::Medium),
            ("error", Severity::High),
            ("warn", Severity::Medium),
            ("note", Severity::Low),
            ("hint", Severity::Info),
            ("suggestion", Severity::Info),
        ];
        for (sev_str, expected) in cases {
            let json = format!(
                r#"[{{"title":"T","description":"D","severity":"{}","category":"c","line_start":1,"line_end":1}}]"#,
                sev_str
            );
            let findings = parse_llm_response(&json, "m").unwrap();
            assert_eq!(findings[0].severity, expected, "Failed for severity: {}", sev_str);
        }
    }
}
