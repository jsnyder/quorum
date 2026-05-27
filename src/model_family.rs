//! Model-family-aware prompt assembly.
//!
//! Detects which model family a model name belongs to (Anthropic, OpenAI,
//! Google, Other) and selects per-family prompt overrides when available.
//! Assembles the final prompt messages with a deterministic SHA-256 digest
//! for cache keying and telemetry.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

// ---------------------------------------------------------------------------
// ModelFamily enum
// ---------------------------------------------------------------------------

/// Broad model-family classification used to select prompt variants and
/// assembly order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFamily {
    Anthropic,
    #[serde(rename = "openai")]
    OpenAi,
    Google,
    Other,
}

impl ModelFamily {
    /// Stable lowercase string form for display, telemetry, and config keys.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Google => "google",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for ModelFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Detect the model family from a model name string.
///
/// Matching is case-insensitive and uses prefix rules (with a few
/// substring rules for OpenAI legacy model names).
#[must_use]
pub fn detect_family(model_name: &str) -> ModelFamily {
    let lower = model_name.to_ascii_lowercase();

    // Anthropic: "claude-*", "anthropic/*", "anthropic-*"
    if lower.starts_with("claude") || lower.starts_with("anthropic") {
        return ModelFamily::Anthropic;
    }

    // OpenAI: prefix matches
    if lower.starts_with("gpt")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
        || lower.starts_with("chatgpt")
        || lower.starts_with("openai")
    {
        return ModelFamily::OpenAi;
    }

    // OpenAI: substring matches for legacy model names
    if lower.contains("davinci") || lower.contains("turbo") {
        return ModelFamily::OpenAi;
    }

    // Google: "gemini-*", "palm-*", "google/*", "gemma-*"
    if lower.starts_with("gemini")
        || lower.starts_with("palm")
        || lower.starts_with("google")
        || lower.starts_with("gemma")
    {
        return ModelFamily::Google;
    }

    ModelFamily::Other
}

// ---------------------------------------------------------------------------
// Prompt variant selection
// ---------------------------------------------------------------------------

/// A family-specific prompt override.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptOverride {
    /// The full override text that replaces the primary prompt for this family.
    pub override_text: String,
}

/// Prompt variants for a single skill. The primary prompt is always present;
/// per-family overrides are optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillPrompts {
    /// Default prompt used when no family-specific override exists.
    pub primary: String,
    /// Anthropic-specific override.
    pub anthropic: Option<PromptOverride>,
    /// OpenAI-specific override.
    pub openai: Option<PromptOverride>,
    /// Google-specific override.
    pub google: Option<PromptOverride>,
}

/// Select the prompt text for the given model family, falling back to the
/// primary prompt when no override exists.
#[must_use]
pub fn select_prompt<'a>(prompts: &'a SkillPrompts, family: ModelFamily) -> &'a str {
    let maybe_override = match family {
        ModelFamily::Anthropic => prompts.anthropic.as_ref(),
        ModelFamily::OpenAi => prompts.openai.as_ref(),
        ModelFamily::Google => prompts.google.as_ref(),
        ModelFamily::Other => None,
    };
    match maybe_override {
        Some(o) => &o.override_text,
        None => &prompts.primary,
    }
}

// ---------------------------------------------------------------------------
// Prompt assembly
// ---------------------------------------------------------------------------

/// Which message slot a prompt fragment occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPosition {
    System,
    User,
}

/// A fully assembled prompt ready for LLM submission.
#[derive(Debug, Clone)]
pub struct AssembledPrompt {
    /// Content for the system message slot.
    pub system_message: String,
    /// Content for the user message slot.
    pub user_message: String,
    /// SHA-256 hex digest of (system_message || user_message), deterministic
    /// for identical content across runs.
    pub prompt_sha256: String,
}

/// Assemble the final prompt from its constituent parts.
///
/// The assembly order is identical across families today (system =
/// `base_system`, user = skill + code + schema). The key differentiator is
/// prompt *content* selection via [`select_prompt`], which picks per-family
/// overrides. Structure divergence (e.g., OpenAI terminal-position system
/// messages) is deferred to the transport layer.
#[must_use]
pub fn assemble_prompt(
    base_system: &str,
    skill_prompt: &str,
    code_to_review: &str,
    output_schema: &str,
    _family: ModelFamily,
) -> AssembledPrompt {
    let system_message = base_system.to_owned();

    let mut user_message =
        String::with_capacity(skill_prompt.len() + code_to_review.len() + output_schema.len() + 4);
    user_message.push_str(skill_prompt);
    user_message.push('\n');
    user_message.push_str(code_to_review);
    user_message.push('\n');
    user_message.push_str(output_schema);

    let prompt_sha256 = compute_sha256(&system_message, &user_message);

    AssembledPrompt {
        system_message,
        user_message,
        prompt_sha256,
    }
}

/// Compute a deterministic SHA-256 hex digest over the concatenated system and
/// user messages.
fn compute_sha256(system: &str, user: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(system.as_bytes());
    hasher.update(user.as_bytes());
    hex::encode(hasher.finalize())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // detect_family
    // -----------------------------------------------------------------------

    #[test]
    fn detect_anthropic_models() {
        assert_eq!(detect_family("claude-opus-4-7"), ModelFamily::Anthropic);
        assert_eq!(detect_family("claude-sonnet-4-6"), ModelFamily::Anthropic);
        assert_eq!(detect_family("anthropic/claude-3"), ModelFamily::Anthropic);
    }

    #[test]
    fn detect_openai_models() {
        assert_eq!(detect_family("gpt-5.4"), ModelFamily::OpenAi);
        assert_eq!(detect_family("o4-mini"), ModelFamily::OpenAi);
        assert_eq!(detect_family("gpt-4-turbo"), ModelFamily::OpenAi);
    }

    #[test]
    fn detect_google_models() {
        assert_eq!(detect_family("gemini-2.5-pro"), ModelFamily::Google);
        assert_eq!(detect_family("gemma-3"), ModelFamily::Google);
        assert_eq!(detect_family("palm-2"), ModelFamily::Google);
    }

    #[test]
    fn detect_other_models() {
        assert_eq!(detect_family("llama-3"), ModelFamily::Other);
        assert_eq!(detect_family("mistral-large"), ModelFamily::Other);
        assert_eq!(detect_family("deepseek-v3"), ModelFamily::Other);
    }

    #[test]
    fn detect_case_insensitive() {
        assert_eq!(detect_family("Claude-Opus-4"), ModelFamily::Anthropic);
        assert_eq!(detect_family("GPT-5"), ModelFamily::OpenAi);
        assert_eq!(detect_family("GEMINI-2.5-PRO"), ModelFamily::Google);
    }

    #[test]
    fn detect_openai_legacy_substring() {
        assert_eq!(detect_family("text-davinci-003"), ModelFamily::OpenAi);
        assert_eq!(detect_family("ft:gpt-3.5-turbo:acme"), ModelFamily::OpenAi);
    }

    #[test]
    fn detect_openai_prefix_variants() {
        assert_eq!(detect_family("o1-preview"), ModelFamily::OpenAi);
        assert_eq!(detect_family("o3-mini"), ModelFamily::OpenAi);
        assert_eq!(detect_family("chatgpt-4o"), ModelFamily::OpenAi);
        assert_eq!(detect_family("openai/gpt-4"), ModelFamily::OpenAi);
    }

    #[test]
    fn detect_google_prefix_variants() {
        assert_eq!(detect_family("google/gemini-pro"), ModelFamily::Google);
    }

    // -----------------------------------------------------------------------
    // select_prompt
    // -----------------------------------------------------------------------

    fn skill_prompts_with_overrides() -> SkillPrompts {
        SkillPrompts {
            primary: "primary prompt".into(),
            anthropic: Some(PromptOverride {
                override_text: "anthropic override".into(),
            }),
            openai: Some(PromptOverride {
                override_text: "openai override".into(),
            }),
            google: Some(PromptOverride {
                override_text: "google override".into(),
            }),
        }
    }

    fn skill_prompts_primary_only() -> SkillPrompts {
        SkillPrompts {
            primary: "primary prompt".into(),
            anthropic: None,
            openai: None,
            google: None,
        }
    }

    #[test]
    fn select_with_override_present() {
        let prompts = skill_prompts_with_overrides();
        assert_eq!(
            select_prompt(&prompts, ModelFamily::Anthropic),
            "anthropic override"
        );
        assert_eq!(
            select_prompt(&prompts, ModelFamily::OpenAi),
            "openai override"
        );
        assert_eq!(
            select_prompt(&prompts, ModelFamily::Google),
            "google override"
        );
    }

    #[test]
    fn select_with_override_absent_falls_back_to_primary() {
        let prompts = skill_prompts_primary_only();
        assert_eq!(
            select_prompt(&prompts, ModelFamily::Anthropic),
            "primary prompt"
        );
        assert_eq!(
            select_prompt(&prompts, ModelFamily::OpenAi),
            "primary prompt"
        );
        assert_eq!(
            select_prompt(&prompts, ModelFamily::Google),
            "primary prompt"
        );
    }

    #[test]
    fn select_other_always_returns_primary() {
        let prompts = skill_prompts_with_overrides();
        assert_eq!(
            select_prompt(&prompts, ModelFamily::Other),
            "primary prompt"
        );
    }

    #[test]
    fn select_primary_only_works_for_all_families() {
        let prompts = skill_prompts_primary_only();
        for family in [
            ModelFamily::Anthropic,
            ModelFamily::OpenAi,
            ModelFamily::Google,
            ModelFamily::Other,
        ] {
            assert_eq!(
                select_prompt(&prompts, family),
                "primary prompt",
                "select_prompt should return primary for {family}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // assemble_prompt
    // -----------------------------------------------------------------------

    #[test]
    fn assemble_produces_deterministic_sha256() {
        let p1 = assemble_prompt("sys", "skill", "code", "schema", ModelFamily::Anthropic);
        let p2 = assemble_prompt("sys", "skill", "code", "schema", ModelFamily::Anthropic);
        assert_eq!(p1.prompt_sha256, p2.prompt_sha256);
    }

    #[test]
    fn assemble_same_inputs_same_sha256_across_families() {
        // Family does not affect assembly structure today, so the hash should
        // be identical for the same content.
        let p_anthropic = assemble_prompt("sys", "skill", "code", "schema", ModelFamily::Anthropic);
        let p_openai = assemble_prompt("sys", "skill", "code", "schema", ModelFamily::OpenAi);
        let p_google = assemble_prompt("sys", "skill", "code", "schema", ModelFamily::Google);
        let p_other = assemble_prompt("sys", "skill", "code", "schema", ModelFamily::Other);
        assert_eq!(p_anthropic.prompt_sha256, p_openai.prompt_sha256);
        assert_eq!(p_openai.prompt_sha256, p_google.prompt_sha256);
        assert_eq!(p_google.prompt_sha256, p_other.prompt_sha256);
    }

    #[test]
    fn assemble_different_skill_prompt_different_sha256() {
        let p1 = assemble_prompt("sys", "skill-a", "code", "schema", ModelFamily::Anthropic);
        let p2 = assemble_prompt("sys", "skill-b", "code", "schema", ModelFamily::Anthropic);
        assert_ne!(p1.prompt_sha256, p2.prompt_sha256);
    }

    #[test]
    fn assemble_system_message_is_base_system() {
        let p = assemble_prompt(
            "base-system",
            "skill",
            "code",
            "schema",
            ModelFamily::Anthropic,
        );
        assert_eq!(p.system_message, "base-system");
    }

    #[test]
    fn assemble_user_message_contains_all_parts() {
        let p = assemble_prompt("sys", "SKILL", "CODE", "SCHEMA", ModelFamily::Anthropic);
        assert!(p.user_message.contains("SKILL"));
        assert!(p.user_message.contains("CODE"));
        assert!(p.user_message.contains("SCHEMA"));
    }

    #[test]
    fn assemble_sha256_is_valid_hex() {
        let p = assemble_prompt("s", "k", "c", "o", ModelFamily::Other);
        assert_eq!(p.prompt_sha256.len(), 64, "SHA-256 hex should be 64 chars");
        assert!(
            p.prompt_sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA-256 should be valid hex"
        );
    }

    // -----------------------------------------------------------------------
    // ModelFamily Display + serde
    // -----------------------------------------------------------------------

    #[test]
    fn display_matches_as_str() {
        for family in [
            ModelFamily::Anthropic,
            ModelFamily::OpenAi,
            ModelFamily::Google,
            ModelFamily::Other,
        ] {
            assert_eq!(family.to_string(), family.as_str());
        }
    }

    #[test]
    fn serde_roundtrip() {
        for family in [
            ModelFamily::Anthropic,
            ModelFamily::OpenAi,
            ModelFamily::Google,
            ModelFamily::Other,
        ] {
            let json = serde_json::to_string(&family).unwrap();
            let back: ModelFamily = serde_json::from_str(&json).unwrap();
            assert_eq!(back, family, "serde roundtrip failed for {family}");
            assert_eq!(json, format!("\"{}\"", family.as_str()));
        }
    }
}
