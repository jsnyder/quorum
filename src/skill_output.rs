//! Strict JSON review output contract for skill-based reviews.
//!
//! Owns the `ParseErrorClass` taxonomy for telemetry, `ModelCapabilities`
//! for per-family JSON mode detection, and `SkillResponseOutcome` for
//! carrying either parsed findings or classified parse failures.
//!
//! The `classify_response` function reuses the existing multi-strategy
//! extraction logic from `review::parse_llm_response` where possible,
//! wrapping it with `ParseErrorClass` classification. The actual wiring
//! into the LLM client happens in issue #410 — this module provides the
//! building blocks.

use crate::finding::{Finding, LlmFinding};
use crate::model_family::ModelFamily;
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// ParseErrorClass
// ---------------------------------------------------------------------------

/// Classification of why a skill response could not be parsed into findings.
///
/// Used for telemetry bucketing and retry decisions. Each variant maps to a
/// distinct remediation path: `Truncated` allows one internal retry with a
/// continuation prompt, while other classes are terminal drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseErrorClass {
    /// Response body is empty or whitespace-only.
    Empty,
    /// Response contains text but cannot be parsed as JSON at all.
    NotJson,
    /// Valid JSON but does not match the `Finding[]` schema.
    WrongSchema,
    /// The model's `finish_reason` was `"length"` — response was cut off by
    /// the token limit.
    Truncated,
}

impl fmt::Display for ParseErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty"),
            Self::NotJson => f.write_str("not_json"),
            Self::WrongSchema => f.write_str("wrong_schema"),
            Self::Truncated => f.write_str("truncated"),
        }
    }
}

// ---------------------------------------------------------------------------
// ModelCapabilities
// ---------------------------------------------------------------------------

/// Per-model-family capability flags.
///
/// Today this only tracks `response_format` support (`json_object` mode).
/// Future flags (tool use, vision, reasoning tokens, etc.) will land here
/// as the skill framework expands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// Whether the model supports `response_format: { "type": "json_object" }`
    /// via the OpenAI-compatible API surface that quorum uses.
    pub supports_json_mode: bool,
}

/// Return the capability flags for a given model family.
///
/// - **OpenAI (GPT)**: supports `response_format: { "type": "json_object" }`.
/// - **Google (Gemini)**: supports the same via OpenAI-compatible endpoints.
/// - **Anthropic (Claude)**: does NOT support `response_format` in the
///   OpenAI-compatible API format. Prompt-based JSON only.
/// - **Other**: no structured output support; prompt-based only.
#[must_use]
pub fn capabilities_for(family: ModelFamily) -> ModelCapabilities {
    let supports_json_mode = matches!(family, ModelFamily::OpenAi | ModelFamily::Google);
    ModelCapabilities { supports_json_mode }
}

// ---------------------------------------------------------------------------
// build_json_mode_params
// ---------------------------------------------------------------------------

/// Return the `response_format` JSON value to merge into the request body
/// for models that support JSON mode, or `None` if the family does not
/// support structured output.
///
/// Currently returns `None` for all families: `json_object` forces a
/// top-level object, which conflicts with the `Vec<Finding>` array
/// contract. Foundation C (#410) will implement `json_schema` with an
/// explicit schema for providers that support it.
#[must_use]
pub fn build_json_mode_params(_family: ModelFamily) -> Option<serde_json::Value> {
    // Deferred: json_schema with explicit Finding[] schema (#410).
    None
}

// ---------------------------------------------------------------------------
// SkillResponseOutcome
// ---------------------------------------------------------------------------

/// Result of attempting to parse a skill's LLM response into findings.
///
/// Carries either the successfully parsed findings (with optional parse
/// warnings for non-fatal issues), a terminal parse error, or a retryable
/// error with a continuation prompt.
#[derive(Debug, Clone)]
pub enum SkillResponseOutcome {
    /// Parsing succeeded. `parse_warnings` captures non-fatal issues like
    /// sanitized JSON escapes or unknown severity values.
    Ok {
        findings: Vec<Finding>,
        parse_warnings: Vec<String>,
    },
    /// Terminal parse failure. The response could not be converted to
    /// findings. `raw_snippet` carries up to 200 chars of the original
    /// response for telemetry / debugging.
    ParseError {
        class: ParseErrorClass,
        raw_snippet: String,
    },
    /// Retryable parse failure. The caller should issue one internal retry
    /// with `continuation_prompt` appended. Only `Truncated` uses this
    /// path today.
    Retry {
        class: ParseErrorClass,
        continuation_prompt: String,
    },
}

/// Maximum length of the raw snippet stored in `ParseError` for telemetry.
const RAW_SNIPPET_MAX_LEN: usize = 200;

/// Truncate a string to at most `RAW_SNIPPET_MAX_LEN` chars, appending
/// "..." if truncated.
fn snippet(raw: &str) -> String {
    let char_count = raw.chars().count();
    if char_count <= RAW_SNIPPET_MAX_LEN {
        raw.to_owned()
    } else {
        let end = raw
            .char_indices()
            .nth(RAW_SNIPPET_MAX_LEN)
            .map_or(raw.len(), |(i, _)| i);
        format!("{}...", &raw[..end])
    }
}

// ---------------------------------------------------------------------------
// classify_response
// ---------------------------------------------------------------------------

/// Classify an LLM response, attempting to parse it into `Finding[]`.
///
/// Decision tree:
/// 1. If `finish_reason` is `"length"`, return `Retry { Truncated, ... }`.
/// 2. If the raw body is empty/whitespace, return `ParseError { Empty, ... }`.
/// 3. Attempt multi-strategy JSON extraction (reusing logic from
///    `review::parse_llm_response`'s 4-strategy fallback).
/// 4. If extraction yields valid `Finding[]`, return `Ok`.
/// 5. If the text is valid JSON but the wrong shape, return
///    `ParseError { WrongSchema, ... }`.
/// 6. Otherwise, return `ParseError { NotJson, ... }`.
#[must_use]
pub fn classify_response(
    raw: &str,
    finish_reason: Option<&str>,
    model: &str,
) -> SkillResponseOutcome {
    // 1. Truncation check (finish_reason == "length").
    if finish_reason == Some("length") {
        return SkillResponseOutcome::Retry {
            class: ParseErrorClass::Truncated,
            continuation_prompt: "Your previous response was truncated. \
                Please continue the JSON array from where you left off."
                .to_owned(),
        };
    }

    // 2. Empty check.
    if raw.trim().is_empty() {
        return SkillResponseOutcome::ParseError {
            class: ParseErrorClass::Empty,
            raw_snippet: String::new(),
        };
    }

    // 3. Attempt to parse as Finding[] using the same strategies as
    //    review::parse_llm_response. We inline a simplified version here
    //    to avoid a circular dependency on the binary-only review module.
    //    The strategies are:
    //    a) Strip markdown fences + extract JSON array bracket-scan
    //    b) Try as bare Vec<Finding>
    //    c) Try as {"findings": [...]} envelope
    //    d) Sanitize invalid JSON escapes and retry both shapes

    let stripped = strip_control_chars(raw);
    let defenced = strip_markdown_fence(&stripped);

    // Try envelope first — if the response is `{"findings": [...]}`, bare
    // array extraction would grab the first `[...]` it finds (which could
    // be a different field like `"warnings": []`).
    if let Some(findings) = try_parse_envelope(&defenced) {
        return SkillResponseOutcome::Ok {
            findings: into_findings(findings, model),
            parse_warnings: vec![],
        };
    }

    // Try bare array via bracket-depth extraction.
    let extracted = extract_json_block(&stripped);
    if let Some(findings) = try_parse_findings(&extracted) {
        return SkillResponseOutcome::Ok {
            findings: into_findings(findings, model),
            parse_warnings: vec![],
        };
    }

    // Sanitize invalid JSON escapes and retry both shapes.
    let sanitized_defenced = sanitize_json_escapes(&defenced);
    if let Some(findings) = try_parse_envelope(&sanitized_defenced) {
        return SkillResponseOutcome::Ok {
            findings: into_findings(findings, model),
            parse_warnings: vec!["sanitized invalid JSON escapes".to_owned()],
        };
    }

    let sanitized_extracted = sanitize_json_escapes(&extracted);
    if let Some(findings) = try_parse_findings(&sanitized_extracted) {
        return SkillResponseOutcome::Ok {
            findings: into_findings(findings, model),
            parse_warnings: vec!["sanitized invalid JSON escapes".to_owned()],
        };
    }

    // 4-5. Classify failure: valid JSON but wrong shape vs not JSON at all.
    //
    // Check the extracted text first (likely the most JSON-like portion),
    // then fall back to the full payload.
    if is_valid_json(&sanitized_extracted) || is_valid_json(&sanitized_defenced) {
        return SkillResponseOutcome::ParseError {
            class: ParseErrorClass::WrongSchema,
            raw_snippet: snippet(raw),
        };
    }

    SkillResponseOutcome::ParseError {
        class: ParseErrorClass::NotJson,
        raw_snippet: snippet(raw),
    }
}

// ---------------------------------------------------------------------------
// Internal parsing helpers
// ---------------------------------------------------------------------------

/// Stamp parsed `LlmFinding`s with their provenance, yielding real `Finding`s.
/// Shares `LlmFinding::into_finding` with the legacy review path so severity
/// mapping and line clamping stay identical across both.
fn into_findings(raw: Vec<LlmFinding>, model: &str) -> Vec<Finding> {
    raw.into_iter().map(|f| f.into_finding(model)).collect()
}

/// Try to deserialize a string as `Vec<LlmFinding>`. Returns `None` on failure.
///
/// Deserializes into `LlmFinding` -- the narrow shape the skill prompts
/// actually ask for -- NOT `Finding`. `Finding` additionally requires
/// `source`, `evidence`, `calibrator_action` and `similar_precedent`, none of
/// which have a serde default and none of which an LLM emits, so targeting it
/// here made every non-empty skill response fail as `wrong_schema`.
fn try_parse_findings(s: &str) -> Option<Vec<LlmFinding>> {
    serde_json::from_str::<Vec<LlmFinding>>(s).ok()
}

/// Wrapper envelope shape: `{"findings": [...]}`.
#[derive(Deserialize)]
struct FindingsEnvelope {
    findings: Vec<LlmFinding>,
}

/// Try to deserialize a string as a `{"findings": [...]}` envelope.
fn try_parse_envelope(s: &str) -> Option<Vec<LlmFinding>> {
    serde_json::from_str::<FindingsEnvelope>(s.trim())
        .ok()
        .map(|e| e.findings)
}

/// Check whether a string is valid JSON of any shape.
fn is_valid_json(s: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(s.trim()).is_ok()
}

/// Strip raw control characters from LLM output while preserving JSON
/// structure. Mirrors `review::strip_control_chars`.
fn strip_control_chars(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\r' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Strip surrounding ````json` / ```` markdown fences if present.
/// Mirrors `review::strip_markdown_fence`.
fn strip_markdown_fence(text: &str) -> String {
    let t = text.trim();
    let after_prefix = if let Some(rest) = t.strip_prefix("```json") {
        rest
    } else if let Some(rest) = t.strip_prefix("```") {
        rest
    } else {
        return t.to_string();
    };
    let trimmed = after_prefix.trim_end();
    let inner = trimmed.strip_suffix("```").unwrap_or(trimmed);
    inner.trim().to_string()
}

/// Extract the outermost JSON array from text using bracket-depth tracking.
/// If no balanced array is found, returns the fence-stripped text as-is.
/// Mirrors `review::extract_json_array`.
fn extract_json_block(text: &str) -> String {
    let text = strip_markdown_fence(text);
    let text = text.trim();
    let bytes = text.as_bytes();
    let mut start = None;
    let mut depth: i32 = 0;
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
        } else if b == b']' && depth > 0 {
            depth -= 1;
            if depth == 0
                && let Some(s) = start
            {
                return text[s..=i].to_string();
            }
        }
    }

    text.to_string()
}

/// Fix invalid JSON escape sequences that LLMs sometimes emit (e.g. `\d`,
/// `\s`). Converts them to `\\d`, `\\s`. Mirrors `review::sanitize_json_escapes`.
fn sanitize_json_escapes(json: &str) -> String {
    let mut result = String::with_capacity(json.len());
    let mut chars = json.chars().peekable();
    let mut in_string = false;

    while let Some(c) = chars.next() {
        if c == '"' && in_string {
            // Check if this quote is escaped by counting preceding backslashes.
            // We already emitted those backslashes, so just toggle.
            in_string = false;
            result.push(c);
            continue;
        }
        if c == '"' {
            in_string = true;
            result.push(c);
            continue;
        }
        if c == '\\' && in_string {
            if let Some(&next) = chars.peek() {
                // Valid JSON escapes: " \ / b f n r t u
                if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') {
                    result.push(c);
                    result.push(chars.next().unwrap());
                } else {
                    // Invalid escape like \d, \s — double the backslash.
                    result.push('\\');
                    result.push(c);
                    result.push(chars.next().unwrap());
                }
            } else {
                result.push(c);
            }
            continue;
        }
        result.push(c);
    }
    result
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract lock: every bundled skill prompt asks for exactly these fields.
    /// If the parser stops accepting this shape, the axis reviewer silently
    /// emits zero findings -- which is precisely what happened for 440
    /// invocations. `evidence` is included because all six bundled prompts
    /// request it.
    #[test]
    fn every_bundled_skill_output_shape_parses() {
        for (skill, category) in [
            ("correctness", "correctness"),
            ("security", "security"),
            ("testing-antipatterns", "testing"),
            ("simplicity", "simplicity"),
            ("performance", "performance"),
            ("architecture", "architecture"),
        ] {
            let raw = format!(
                r#"[{{"title":"t","description":"d","severity":"medium",
                     "category":"{category}","line_start":1,"line_end":2,
                     "evidence":["e1"]}}]"#
            );
            match classify_response(&raw, None, "gpt-5.4") {
                SkillResponseOutcome::Ok { findings, .. } => {
                    assert_eq!(findings.len(), 1, "{skill}: shape must parse");
                    assert_eq!(
                        findings[0].evidence,
                        vec!["e1".to_owned()],
                        "{skill}: evidence must survive parsing, not be dropped"
                    );
                }
                other => panic!("{skill}: expected Ok, got {other:?}"),
            }
        }
    }

    /// A response carrying internal-only fields must still parse. Unknown keys
    /// are ignored rather than rejected, so prompt changes cannot regress the
    /// parser into `wrong_schema`.
    #[test]
    fn unknown_fields_are_ignored_not_rejected() {
        let raw = r#"[{"title":"t","description":"d","severity":"high",
                      "category":"security","line_start":3,"line_end":3,
                      "evidence":[],"source":"whatever","made_up_field":123}]"#;
        match classify_response(raw, None, "gpt-5.4") {
            SkillResponseOutcome::Ok { findings, .. } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(
                    findings[0].source,
                    crate::finding::Source::Llm("gpt-5.4".to_owned()),
                    "source must come from the caller, never from model output"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    /// Regression: the skill prompts ask for exactly these fields, and NOTHING
    /// else. Parsing must target `LlmFinding`, not `Finding` -- `Finding`
    /// additionally requires `source`, `evidence`, `calibrator_action` and
    /// `similar_precedent`, none of which are defaulted and none of which an
    /// LLM emits. Targeting `Finding` made every non-empty skill response fail
    /// as `wrong_schema`: 440 real invocations emitted 0 findings, ever.
    ///
    /// The pre-existing tests missed this because they fed the parser
    /// internal `Finding` JSON rather than real model output.
    #[test]
    fn parses_the_shape_the_skill_prompt_actually_requests() {
        let raw = r#"[{
            "title": "[cut] duplicate guard",
            "description": "The freshness check runs twice.",
            "severity": "medium",
            "category": "simplicity",
            "line_start": 12,
            "line_end": 18,
            "evidence": ["line 12 duplicates line 18"]
        }]"#;
        match classify_response(raw, None, "gpt-5.4") {
            SkillResponseOutcome::Ok { findings, .. } => {
                assert_eq!(findings.len(), 1, "skill-shaped JSON must parse");
                assert_eq!(findings[0].severity, crate::finding::Severity::Medium);
                assert_eq!(findings[0].line_start, 12);
                assert_eq!(
                    findings[0].source,
                    crate::finding::Source::Llm("gpt-5.4".to_owned()),
                    "provenance must be stamped from the calling model"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    /// The empty case must stay `Ok` with no findings -- this is the arm that
    /// masked the bug, reporting exit_status=ok on 174 invocations.
    #[test]
    fn empty_array_still_parses_as_ok() {
        match classify_response("[]", None, "gpt-5.4") {
            SkillResponseOutcome::Ok { findings, .. } => assert!(findings.is_empty()),
            other => panic!("expected Ok, got {other:?}"),
        }
    }
    use crate::finding::{FindingBuilder, Severity};

    // -----------------------------------------------------------------------
    // ParseErrorClass serde roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn parse_error_class_serde_roundtrip() {
        for class in [
            ParseErrorClass::Empty,
            ParseErrorClass::NotJson,
            ParseErrorClass::WrongSchema,
            ParseErrorClass::Truncated,
        ] {
            let json = serde_json::to_string(&class).unwrap();
            let back: ParseErrorClass = serde_json::from_str(&json).unwrap();
            assert_eq!(back, class, "serde roundtrip failed for {class}");
        }
    }

    #[test]
    fn parse_error_class_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(ParseErrorClass::Empty).unwrap(),
            "empty"
        );
        assert_eq!(
            serde_json::to_value(ParseErrorClass::NotJson).unwrap(),
            "not_json"
        );
        assert_eq!(
            serde_json::to_value(ParseErrorClass::WrongSchema).unwrap(),
            "wrong_schema"
        );
        assert_eq!(
            serde_json::to_value(ParseErrorClass::Truncated).unwrap(),
            "truncated"
        );
    }

    #[test]
    fn parse_error_class_display_matches_serde() {
        for class in [
            ParseErrorClass::Empty,
            ParseErrorClass::NotJson,
            ParseErrorClass::WrongSchema,
            ParseErrorClass::Truncated,
        ] {
            let display = class.to_string();
            let serde_str = serde_json::to_value(class)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(
                display, serde_str,
                "Display and serde disagree for {class:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // ModelCapabilities
    // -----------------------------------------------------------------------

    #[test]
    fn capabilities_openai_supports_json_mode() {
        let caps = capabilities_for(ModelFamily::OpenAi);
        assert!(caps.supports_json_mode);
    }

    #[test]
    fn capabilities_google_supports_json_mode() {
        let caps = capabilities_for(ModelFamily::Google);
        assert!(caps.supports_json_mode);
    }

    #[test]
    fn capabilities_anthropic_no_json_mode() {
        let caps = capabilities_for(ModelFamily::Anthropic);
        assert!(!caps.supports_json_mode);
    }

    #[test]
    fn capabilities_other_no_json_mode() {
        let caps = capabilities_for(ModelFamily::Other);
        assert!(!caps.supports_json_mode);
    }

    // -----------------------------------------------------------------------
    // build_json_mode_params
    // -----------------------------------------------------------------------

    #[test]
    fn json_mode_params_returns_none_for_all_families() {
        for family in [
            ModelFamily::Anthropic,
            ModelFamily::OpenAi,
            ModelFamily::Google,
            ModelFamily::Other,
        ] {
            assert!(
                build_json_mode_params(family).is_none(),
                "build_json_mode_params should return None for {family} \
                 (json_schema deferred to #410)"
            );
        }
    }

    // -----------------------------------------------------------------------
    // SkillResponseOutcome construction
    // -----------------------------------------------------------------------

    #[test]
    fn outcome_ok_variant() {
        let findings = vec![
            FindingBuilder::new()
                .title("test")
                .severity(Severity::High)
                .build(),
        ];
        let outcome = SkillResponseOutcome::Ok {
            findings: findings.clone(),
            parse_warnings: vec!["warning1".into()],
        };
        match outcome {
            SkillResponseOutcome::Ok {
                findings: f,
                parse_warnings: w,
            } => {
                assert_eq!(f.len(), 1);
                assert_eq!(f[0].title, "test");
                assert_eq!(w, vec!["warning1"]);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn outcome_parse_error_variant() {
        let outcome = SkillResponseOutcome::ParseError {
            class: ParseErrorClass::NotJson,
            raw_snippet: "garbled text".into(),
        };
        match outcome {
            SkillResponseOutcome::ParseError { class, raw_snippet } => {
                assert_eq!(class, ParseErrorClass::NotJson);
                assert_eq!(raw_snippet, "garbled text");
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn outcome_retry_variant() {
        let outcome = SkillResponseOutcome::Retry {
            class: ParseErrorClass::Truncated,
            continuation_prompt: "continue please".into(),
        };
        match outcome {
            SkillResponseOutcome::Retry {
                class,
                continuation_prompt,
            } => {
                assert_eq!(class, ParseErrorClass::Truncated);
                assert_eq!(continuation_prompt, "continue please");
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // classify_response: empty input
    // -----------------------------------------------------------------------

    #[test]
    fn classify_empty_string() {
        let outcome = classify_response("", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, .. } => {
                assert_eq!(class, ParseErrorClass::Empty);
            }
            other => panic!("expected Empty ParseError, got {other:?}"),
        }
    }

    #[test]
    fn classify_whitespace_only() {
        let outcome = classify_response("   \n\t  ", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, .. } => {
                assert_eq!(class, ParseErrorClass::Empty);
            }
            other => panic!("expected Empty ParseError, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // classify_response: truncated (finish_reason == "length")
    // -----------------------------------------------------------------------

    #[test]
    fn classify_truncated_finish_reason() {
        let outcome = classify_response("[{\"partial\": true", Some("length"), "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Retry {
                class,
                continuation_prompt,
            } => {
                assert_eq!(class, ParseErrorClass::Truncated);
                assert!(!continuation_prompt.is_empty());
            }
            other => panic!("expected Retry/Truncated, got {other:?}"),
        }
    }

    #[test]
    fn classify_truncated_even_if_empty_body() {
        // finish_reason takes priority over body analysis.
        let outcome = classify_response("", Some("length"), "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Retry { class, .. } => {
                assert_eq!(class, ParseErrorClass::Truncated);
            }
            other => panic!("expected Retry/Truncated, got {other:?}"),
        }
    }

    #[test]
    fn classify_stop_finish_reason_does_not_trigger_truncation() {
        let outcome = classify_response("not json at all", Some("stop"), "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, .. } => {
                assert_eq!(class, ParseErrorClass::NotJson);
            }
            other => panic!("expected NotJson, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // classify_response: not JSON
    // -----------------------------------------------------------------------

    #[test]
    fn classify_garbage_text() {
        let outcome = classify_response("Here are my thoughts on the code...", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, raw_snippet } => {
                assert_eq!(class, ParseErrorClass::NotJson);
                assert!(raw_snippet.contains("Here are my thoughts"));
            }
            other => panic!("expected NotJson, got {other:?}"),
        }
    }

    #[test]
    fn classify_partial_json() {
        let outcome = classify_response("[{\"title\": \"incomplete", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, .. } => {
                assert_eq!(class, ParseErrorClass::NotJson);
            }
            other => panic!("expected NotJson, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // classify_response: wrong schema
    // -----------------------------------------------------------------------

    #[test]
    fn classify_valid_json_wrong_shape_object() {
        let outcome = classify_response("{\"message\": \"no findings here\"}", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, .. } => {
                assert_eq!(class, ParseErrorClass::WrongSchema);
            }
            other => panic!("expected WrongSchema, got {other:?}"),
        }
    }

    #[test]
    fn classify_valid_json_wrong_shape_array_of_strings() {
        let outcome = classify_response("[\"foo\", \"bar\"]", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, .. } => {
                assert_eq!(class, ParseErrorClass::WrongSchema);
            }
            other => panic!("expected WrongSchema, got {other:?}"),
        }
    }

    #[test]
    fn classify_valid_json_wrong_shape_array_of_wrong_objects() {
        let outcome = classify_response("[{\"name\": \"foo\", \"age\": 42}]", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::ParseError { class, .. } => {
                assert_eq!(class, ParseErrorClass::WrongSchema);
            }
            other => panic!("expected WrongSchema, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // classify_response: valid Finding array
    // -----------------------------------------------------------------------

    /// Emit only the fields the skill prompts declare. Building a `Finding`
    /// and serializing it back would round-trip `Finding -> JSON -> Finding`,
    /// which is exactly the blind spot that let the parser reject all real
    /// model output while these tests stayed green.
    fn make_finding_json(title: &str) -> String {
        serde_json::json!({
            "title": title,
            "description": "mock finding body",
            "severity": "high",
            "category": "security",
            "line_start": 10,
            "line_end": 20,
            "evidence": ["mock evidence line"],
        })
        .to_string()
    }

    #[test]
    fn classify_valid_bare_array() {
        let json = format!("[{}]", make_finding_json("SQL injection"));
        let outcome = classify_response(&json, None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Ok {
                findings,
                parse_warnings,
            } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].title, "SQL injection");
                assert!(parse_warnings.is_empty());
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn classify_valid_envelope() {
        let json = format!(
            "{{\"findings\": [{}]}}",
            make_finding_json("Buffer overflow")
        );
        let outcome = classify_response(&json, None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Ok { findings, .. } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].title, "Buffer overflow");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn classify_valid_array_in_markdown_fence() {
        let json = format!("```json\n[{}]\n```", make_finding_json("XSS"));
        let outcome = classify_response(&json, None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Ok { findings, .. } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].title, "XSS");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn classify_empty_findings_array() {
        let outcome = classify_response("[]", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Ok { findings, .. } => {
                assert!(findings.is_empty());
            }
            other => panic!("expected Ok with empty findings, got {other:?}"),
        }
    }

    #[test]
    fn classify_empty_findings_envelope() {
        let outcome = classify_response("{\"findings\": []}", None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Ok { findings, .. } => {
                assert!(findings.is_empty());
            }
            other => panic!("expected Ok with empty findings, got {other:?}"),
        }
    }

    #[test]
    fn classify_multiple_findings() {
        let json = format!(
            "[{}, {}]",
            make_finding_json("Finding A"),
            make_finding_json("Finding B")
        );
        let outcome = classify_response(&json, Some("stop"), "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Ok { findings, .. } => {
                assert_eq!(findings.len(), 2);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // classify_response: sanitized JSON escapes
    // -----------------------------------------------------------------------

    #[test]
    fn classify_with_invalid_json_escapes() {
        // Build a valid finding JSON, then inject an invalid escape in a
        // string field to force the sanitize path.
        let base = make_finding_json("Regex issue");
        // Replace "Regex issue" with "Regex \d+ issue" (invalid \d escape).
        let broken = base.replace("Regex issue", "Regex \\d+ issue");
        let json = format!("[{}]", broken);
        let outcome = classify_response(&json, None, "gpt-5.4");
        match outcome {
            SkillResponseOutcome::Ok { parse_warnings, .. } => {
                assert!(
                    parse_warnings.iter().any(|w| w.contains("sanitized")),
                    "expected sanitization warning, got {parse_warnings:?}"
                );
            }
            other => panic!("expected Ok with sanitization warning, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // snippet truncation
    // -----------------------------------------------------------------------

    #[test]
    fn snippet_short_text_not_truncated() {
        let s = snippet("hello");
        assert_eq!(s, "hello");
    }

    #[test]
    fn snippet_long_text_truncated() {
        let long = "x".repeat(300);
        let s = snippet(&long);
        assert!(s.len() < 300);
        assert!(s.ends_with("..."));
    }

    // -----------------------------------------------------------------------
    // Internal helper coverage
    // -----------------------------------------------------------------------

    #[test]
    fn extract_json_block_from_prose() {
        let text = "Here is the result:\n[1, 2, 3]\nDone.";
        let extracted = extract_json_block(text);
        assert_eq!(extracted, "[1, 2, 3]");
    }

    #[test]
    fn extract_json_block_no_array_returns_full() {
        let text = "no array here";
        let extracted = extract_json_block(text);
        assert_eq!(extracted, "no array here");
    }

    #[test]
    fn strip_markdown_fence_json() {
        let input = "```json\n{\"a\": 1}\n```";
        let result = strip_markdown_fence(input);
        assert_eq!(result, "{\"a\": 1}");
    }

    #[test]
    fn strip_markdown_fence_no_fence() {
        let input = "{\"a\": 1}";
        let result = strip_markdown_fence(input);
        assert_eq!(result, "{\"a\": 1}");
    }

    #[test]
    fn sanitize_json_escapes_valid_passthrough() {
        let input = r#"{"title": "foo\nbar"}"#;
        let result = sanitize_json_escapes(input);
        assert_eq!(result, input);
    }

    #[test]
    fn sanitize_json_escapes_fixes_invalid() {
        let input = r#"{"title": "regex \d+"}"#;
        let result = sanitize_json_escapes(input);
        assert!(result.contains(r#"\\d"#));
    }

    #[test]
    fn is_valid_json_true_for_object() {
        assert!(is_valid_json("{\"a\": 1}"));
    }

    #[test]
    fn is_valid_json_true_for_array() {
        assert!(is_valid_json("[1, 2]"));
    }

    #[test]
    fn is_valid_json_false_for_garbage() {
        assert!(!is_valid_json("not json at all"));
    }
}
