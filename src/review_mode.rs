//! Review mode enum: Code (default), Plan, Docs, Tests, Release.
//!
//! Determines which prompt template and evaluation rubric the LLM pipeline
//! uses. `Code` is the traditional source-code review; `Plan` and `Docs` are
//! prose-oriented modes that swap in prose-specific system prompts and
//! severity scales. `Tests` and `Release` are reserved for future axes and
//! cannot be selected via `--mode` today.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The review mode governs prompt selection, severity rubric, and AST/linter
/// applicability for a given review invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMode {
    /// Traditional source-code review (AST + linters + LLM).
    #[default]
    Code,
    /// Review a plan/design document for feasibility, gaps, and risks.
    Plan,
    /// Review prose documentation for accuracy, clarity, and completeness.
    Docs,
    /// Reserved: test-suite quality review axis (not yet implemented).
    Tests,
    /// Reserved: release-readiness review axis (not yet implemented).
    Release,
}

impl ReviewMode {
    /// Returns `true` for prose-oriented modes (Plan, Docs).
    #[must_use]
    pub fn is_prose(self) -> bool {
        matches!(self, Self::Plan | Self::Docs)
    }

    /// Returns `true` for reserved modes that are not yet implemented.
    #[must_use]
    pub fn is_reserved(self) -> bool {
        matches!(self, Self::Tests | Self::Release)
    }

    /// Stable lowercase string form suitable for CLI flags, serde, and
    /// telemetry fields.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Plan => "plan",
            Self::Docs => "docs",
            Self::Tests => "tests",
            Self::Release => "release",
        }
    }
}

impl fmt::Display for ReviewMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReviewMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "code" => Ok(Self::Code),
            "plan" => Ok(Self::Plan),
            "docs" => Ok(Self::Docs),
            "tests" => Ok(Self::Tests),
            "release" => Ok(Self::Release),
            other => Err(format!(
                "unknown review mode '{other}'; expected one of: code, plan, docs, tests, release"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_code() {
        assert_eq!(ReviewMode::default(), ReviewMode::Code);
    }

    #[test]
    fn is_prose_true_for_plan_and_docs() {
        assert!(ReviewMode::Plan.is_prose());
        assert!(ReviewMode::Docs.is_prose());
    }

    #[test]
    fn is_prose_false_for_code() {
        assert!(!ReviewMode::Code.is_prose());
    }

    #[test]
    fn roundtrip_from_str() {
        for mode in [
            ReviewMode::Code,
            ReviewMode::Plan,
            ReviewMode::Docs,
            ReviewMode::Tests,
            ReviewMode::Release,
        ] {
            let s = mode.as_str();
            let parsed: ReviewMode = s.parse().unwrap();
            assert_eq!(parsed, mode, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn from_str_case_insensitive() {
        assert_eq!("CODE".parse::<ReviewMode>().unwrap(), ReviewMode::Code);
        assert_eq!("Plan".parse::<ReviewMode>().unwrap(), ReviewMode::Plan);
        assert_eq!("DOCS".parse::<ReviewMode>().unwrap(), ReviewMode::Docs);
        assert_eq!("TESTS".parse::<ReviewMode>().unwrap(), ReviewMode::Tests);
        assert_eq!(
            "Release".parse::<ReviewMode>().unwrap(),
            ReviewMode::Release
        );
    }

    #[test]
    fn unknown_mode_errors() {
        let err = "markdown".parse::<ReviewMode>().unwrap_err();
        assert!(
            err.contains("unknown review mode"),
            "error message should mention 'unknown review mode'; got: {err}"
        );
    }

    #[test]
    fn as_str_roundtrip() {
        for mode in [
            ReviewMode::Code,
            ReviewMode::Plan,
            ReviewMode::Docs,
            ReviewMode::Tests,
            ReviewMode::Release,
        ] {
            assert_eq!(mode.to_string(), mode.as_str());
        }
    }

    #[test]
    fn serde_roundtrip() {
        for mode in [
            ReviewMode::Code,
            ReviewMode::Plan,
            ReviewMode::Docs,
            ReviewMode::Tests,
            ReviewMode::Release,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: ReviewMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode, "serde roundtrip failed for {mode}");
            // Verify lowercase serialization.
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
        }
    }

    #[test]
    fn reserved_modes_parse() {
        assert_eq!("tests".parse::<ReviewMode>().unwrap(), ReviewMode::Tests);
        assert_eq!(
            "release".parse::<ReviewMode>().unwrap(),
            ReviewMode::Release
        );
    }

    #[test]
    fn reserved_modes_are_reserved() {
        assert!(ReviewMode::Tests.is_reserved());
        assert!(ReviewMode::Release.is_reserved());
    }

    #[test]
    fn non_reserved_modes_are_not_reserved() {
        assert!(!ReviewMode::Code.is_reserved());
        assert!(!ReviewMode::Plan.is_reserved());
        assert!(!ReviewMode::Docs.is_reserved());
    }
}
