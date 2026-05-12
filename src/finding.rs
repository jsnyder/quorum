use crate::category::Category;
use serde::{Deserialize, Serialize};

/// Generate a fresh ULID for a Finding's `id`.
///
/// Used both as the FindingBuilder default and as the serde-default for
/// pre-rollout JSON dumps that lack the `id` field. ULID is monotonic-ish,
/// 26 chars in Crockford base32 — short enough to display in CLI output,
/// stable enough to dedup feedback against.
pub fn new_finding_ulid() -> String {
    ulid::Ulid::new().to_string()
}

/// Collect the stable IDs of a slice of findings, in order.
///
/// Used at review-write time to populate `ReviewRecord.finding_ids` so
/// `feedback.jsonl` entries (`FeedbackEntry.finding_id`) can be joined
/// back to their originating review for per-finding precision math.
pub fn collect_finding_ids(findings: &[Finding]) -> Vec<String> {
    findings.iter().map(|f| f.id.clone()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalibratorAction {
    Confirmed,
    Disputed,
    Adjusted,
    Added,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrecisionTier {
    #[default]
    High,
    Medium,
    Speculative,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeRequirement {
    Required,
    Optional,
    #[default]
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeVerdict {
    Approved,
    Rejected,
    Uncertain,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    LocalAst,
    Linter(String),
    Llm(String),
}

impl Source {
    pub fn provider_name(&self) -> &str {
        match self {
            Source::LocalAst => "local-ast",
            Source::Linter(name) => name,
            Source::Llm(name) => name,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Source::LocalAst => "local-ast",
            Source::Linter(_) => "linter",
            Source::Llm(_) => "llm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroundingStatus {
    Verified,
    VerifiedElsewhere,
    SymbolNotFound,
    LineOutOfRange,
    NotChecked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable per-finding identifier (ULID, 26 chars). Generated at build
    /// time so it's available for feedback recording, dedup, and reviews.jsonl
    /// linkage. `#[serde(default)]` mints a fresh ULID for legacy JSON
    /// dumps that pre-date this field.
    #[serde(default = "new_finding_ulid")]
    pub id: String,
    pub title: String,
    pub description: String,
    pub severity: Severity,
    pub category: Category,
    pub source: Source,
    pub line_start: u32,
    pub line_end: u32,
    pub evidence: Vec<String>,
    pub calibrator_action: Option<CalibratorAction>,
    pub similar_precedent: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_lines: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_status: Option<GroundingStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_agreement: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_verdict: Option<JudgeVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision_tier: Option<PrecisionTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
}

impl Finding {
    pub fn is_valid(&self) -> bool {
        self.line_start >= 1 && self.line_start <= self.line_end
    }

    pub fn compute_confidence(&mut self) {
        let base = match (&self.source, &self.precision_tier) {
            (Source::LocalAst, Some(PrecisionTier::High)) | (Source::LocalAst, None) => 0.95,
            (Source::LocalAst, Some(PrecisionTier::Medium)) => 0.80,
            (Source::LocalAst, Some(PrecisionTier::Speculative)) => 0.50,
            (Source::Linter(_), Some(PrecisionTier::High)) | (Source::Linter(_), None) => 0.90,
            (Source::Linter(_), Some(PrecisionTier::Medium)) => 0.75,
            (Source::Linter(_), Some(PrecisionTier::Speculative)) => 0.45,
            (Source::Llm(_), _) => self
                .grounding_confidence
                .filter(|c| c.is_finite())
                .map(|c| c.clamp(0.0, 1.0))
                .unwrap_or(0.5),
        };
        let base = self
            .judge_confidence
            .filter(|c| c.is_finite())
            .map(|c| c.clamp(0.0, 1.0))
            .unwrap_or(base);
        let cal_factor = match self.calibrator_action {
            Some(CalibratorAction::Confirmed) => 1.1,
            Some(CalibratorAction::Disputed) => 0.6,
            Some(CalibratorAction::Adjusted) => 0.85,
            Some(CalibratorAction::Added) => 0.7,
            None => 1.0,
        };
        let agreement_factor = match (&self.source, self.model_agreement) {
            (Source::Llm(_), Some(a)) if a.is_finite() => {
                let a = a.clamp(0.0, 1.0);
                0.85 + 0.30 * a
            }
            _ => 1.0,
        };
        self.confidence = Some((base * cal_factor * agreement_factor).clamp(0.0, 1.0));
    }

    pub fn severity_label(&self) -> &'static str {
        match self.severity {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Info => "info",
        }
    }
}

// Builder for `Finding` values.
//
// Previously gated behind `#[cfg(test)]`, but the bin/lib hybrid split made
// that unworkable: `#[cfg(test)]` is per-crate, so when `cargo test --bin
// quorum` builds the bin in test mode, the lib (where `Finding` now lives)
// is built in non-test mode and the gated builder is invisible to the bin's
// `#[cfg(test)]` modules. The builder is small, allocation-only, and adds
// negligible production binary size; keeping it always-on is the simplest
// fix.
pub struct FindingBuilder {
    inner: Finding,
}

impl Default for FindingBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FindingBuilder {
    pub fn new() -> Self {
        Self {
            inner: Finding {
                id: new_finding_ulid(),
                title: "Test finding".into(),
                description: "Test description".into(),
                severity: Severity::Info,
                category: Category::Maintainability,
                source: Source::LocalAst,
                line_start: 1,
                line_end: 1,
                evidence: vec![],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
                suggested_fix: None,
                based_on_excerpt: None,
                reasoning: None,
                llm_confidence: None,
                confidence: None,
                cited_lines: None,
                grounding_status: None,
                grounding_confidence: None,
                model_agreement: None,
                rule_id: None,
                judge_verdict: None,
                judge_confidence: None,
                precision_tier: None,
                in_diff: None,
            },
        }
    }

    /// Override the auto-generated ULID. Use only when reconstructing a
    /// known finding (e.g. JSON re-parse, MCP round-trip); production code
    /// should let `new()` mint a fresh ULID. Empty strings are rejected
    /// because `id` is the join key for `collect_finding_ids` and
    /// `analytics::linkage_stats` — an empty id would collide with every
    /// other empty id and silently corrupt linkage.
    pub fn id(mut self, id: &str) -> Self {
        assert!(!id.is_empty(), "FindingBuilder::id: id must be non-empty");
        self.inner.id = id.into();
        self
    }

    pub fn title(mut self, t: &str) -> Self {
        self.inner.title = t.into();
        self
    }

    pub fn description(mut self, d: &str) -> Self {
        self.inner.description = d.into();
        self
    }

    pub fn severity(mut self, s: Severity) -> Self {
        self.inner.severity = s;
        self
    }

    pub fn category(mut self, c: Category) -> Self {
        self.inner.category = c;
        self
    }

    pub fn reasoning(mut self, r: &str) -> Self {
        self.inner.reasoning = Some(r.into());
        self
    }

    pub fn llm_confidence(mut self, c: f32) -> Self {
        self.inner.llm_confidence = Some(c);
        self
    }

    pub fn confidence(mut self, c: f32) -> Self {
        self.inner.confidence = Some(c);
        self
    }

    pub fn cited_lines(mut self, start: u32, end: u32) -> Self {
        self.inner.cited_lines = Some((start, end));
        self
    }

    pub fn source(mut self, s: Source) -> Self {
        self.inner.source = s;
        self
    }

    pub fn lines(mut self, start: u32, end: u32) -> Self {
        self.inner.line_start = start;
        self.inner.line_end = end;
        self
    }

    pub fn line_start(mut self, start: u32) -> Self {
        self.inner.line_start = start;
        self
    }

    pub fn line_end(mut self, end: u32) -> Self {
        self.inner.line_end = end;
        self
    }

    pub fn evidence(mut self, e: &str) -> Self {
        self.inner.evidence.push(e.into());
        self
    }

    pub fn calibrator_action(mut self, a: CalibratorAction) -> Self {
        self.inner.calibrator_action = Some(a);
        self
    }

    pub fn canonical_pattern(mut self, p: &str) -> Self {
        self.inner.canonical_pattern = Some(p.into());
        self
    }

    pub fn suggested_fix(mut self, s: &str) -> Self {
        self.inner.suggested_fix = Some(s.into());
        self
    }

    pub fn based_on_excerpt(mut self, s: &str) -> Self {
        self.inner.based_on_excerpt = Some(s.to_string());
        self
    }

    pub fn grounding_status(mut self, s: GroundingStatus) -> Self {
        self.inner.grounding_status = Some(s);
        self
    }

    pub fn grounding_confidence(mut self, c: f32) -> Self {
        self.inner.grounding_confidence = Some(c);
        self
    }

    pub fn model_agreement(mut self, a: f32) -> Self {
        self.inner.model_agreement = Some(a);
        self
    }

    pub fn precedent(mut self, p: &str) -> Self {
        self.inner.similar_precedent.push(p.into());
        self
    }

    pub fn rule_id(mut self, r: &str) -> Self {
        self.inner.rule_id = Some(r.into());
        self
    }

    pub fn build(self) -> Finding {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Severity ordering --

    #[test]
    fn severity_ordering_critical_is_highest() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::High > Severity::Medium);
        assert!(Severity::Medium > Severity::Low);
        assert!(Severity::Low > Severity::Info);
    }

    #[test]
    fn severity_equality() {
        assert_eq!(Severity::Critical, Severity::Critical);
        assert_ne!(Severity::Critical, Severity::Info);
    }

    // -- Source accessors --

    #[test]
    fn source_provider_name() {
        assert_eq!(Source::LocalAst.provider_name(), "local-ast");
        assert_eq!(Source::Linter("ruff".into()).provider_name(), "ruff");
        assert_eq!(Source::Llm("gpt-5.4".into()).provider_name(), "gpt-5.4");
    }

    #[test]
    fn source_kind() {
        assert_eq!(Source::LocalAst.kind(), "local-ast");
        assert_eq!(Source::Linter("ruff".into()).kind(), "linter");
        assert_eq!(Source::Llm("gpt-5.4".into()).kind(), "llm");
    }

    // -- Source serialization shape --

    #[test]
    fn source_serialization_shape() {
        assert_eq!(
            serde_json::to_value(Source::LocalAst).unwrap(),
            serde_json::json!("local-ast")
        );
        assert_eq!(
            serde_json::to_value(Source::Linter("ruff".into())).unwrap(),
            serde_json::json!({"linter": "ruff"})
        );
        assert_eq!(
            serde_json::to_value(Source::Llm("gpt-5.4".into())).unwrap(),
            serde_json::json!({"llm": "gpt-5.4"})
        );
    }

    // -- Finding JSON roundtrip --

    #[test]
    fn finding_serializes_to_json_with_all_fields() {
        let f = Finding {
            id: new_finding_ulid(),
            title: "Unvalidated input".into(),
            description: "User input flows to db.execute()".into(),
            severity: Severity::Critical,
            category: "security".into(),
            source: Source::LocalAst,
            line_start: 42,
            line_end: 58,
            evidence: vec!["dataflow: req.query -> db.execute()".into()],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            llm_confidence: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
            grounding_confidence: None,
            model_agreement: None,
            rule_id: None,
            judge_verdict: None,
            judge_confidence: None,
            precision_tier: None,
            in_diff: None,
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["title"], "Unvalidated input");
        assert_eq!(json["severity"], "critical");
        assert_eq!(json["source"], "local-ast");
        assert_eq!(json["line_start"], 42);
        assert_eq!(json["line_end"], 58);
        assert_eq!(json["category"], "security");
        assert!(json["evidence"].is_array());
        assert!(json["calibrator_action"].is_null());
        assert!(json["similar_precedent"].is_array());
    }

    #[test]
    fn finding_json_roundtrip() {
        let original = Finding {
            id: new_finding_ulid(),
            title: "Test finding".into(),
            description: "A test".into(),
            severity: Severity::High,
            category: "test".into(),
            source: Source::Llm("claude".into()),
            line_start: 1,
            line_end: 10,
            evidence: vec!["evidence1".into(), "evidence2".into()],
            calibrator_action: Some(CalibratorAction::Confirmed),
            similar_precedent: vec!["similar TP in auth.py".into()],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            llm_confidence: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
            grounding_confidence: None,
            model_agreement: None,
            rule_id: None,
            judge_verdict: None,
            judge_confidence: None,
            precision_tier: None,
            in_diff: None,
        };
        let json_str = serde_json::to_string(&original).unwrap();
        let deserialized: Finding = serde_json::from_str(&json_str).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn finding_with_empty_optional_fields_serializes_cleanly() {
        let f = Finding {
            id: new_finding_ulid(),
            title: "Minimal".into(),
            description: "Desc".into(),
            severity: Severity::Info,
            category: "style".into(),
            source: Source::LocalAst,
            line_start: 1,
            line_end: 1,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            llm_confidence: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
            grounding_confidence: None,
            model_agreement: None,
            rule_id: None,
            judge_verdict: None,
            judge_confidence: None,
            precision_tier: None,
            in_diff: None,
        };
        let json = serde_json::to_value(&f).unwrap();
        assert!(json["calibrator_action"].is_null());
        assert_eq!(json["evidence"].as_array().unwrap().len(), 0);
    }

    // -- Line range validation --

    #[test]
    fn finding_line_range_valid() {
        let f = Finding {
            id: new_finding_ulid(),
            title: "T".into(),
            description: "D".into(),
            severity: Severity::Info,
            category: "c".into(),
            source: Source::LocalAst,
            line_start: 10,
            line_end: 20,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            llm_confidence: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
            grounding_confidence: None,
            model_agreement: None,
            rule_id: None,
            judge_verdict: None,
            judge_confidence: None,
            precision_tier: None,
            in_diff: None,
        };
        assert!(f.is_valid());
    }

    #[test]
    fn finding_line_range_single_line_valid() {
        let f = Finding {
            id: new_finding_ulid(),
            title: "T".into(),
            description: "D".into(),
            severity: Severity::Info,
            category: "c".into(),
            source: Source::LocalAst,
            line_start: 5,
            line_end: 5,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            llm_confidence: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
            grounding_confidence: None,
            model_agreement: None,
            rule_id: None,
            judge_verdict: None,
            judge_confidence: None,
            precision_tier: None,
            in_diff: None,
        };
        assert!(f.is_valid());
    }

    #[test]
    fn finding_line_range_inverted_invalid() {
        let f = Finding {
            id: new_finding_ulid(),
            title: "T".into(),
            description: "D".into(),
            severity: Severity::Info,
            category: "c".into(),
            source: Source::LocalAst,
            line_start: 20,
            line_end: 10,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            llm_confidence: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
            grounding_confidence: None,
            model_agreement: None,
            rule_id: None,
            judge_verdict: None,
            judge_confidence: None,
            precision_tier: None,
            in_diff: None,
        };
        assert!(!f.is_valid());
    }

    #[test]
    fn finding_line_range_zero_start_invalid() {
        let f = Finding {
            id: new_finding_ulid(),
            title: "T".into(),
            description: "D".into(),
            severity: Severity::Info,
            category: "c".into(),
            source: Source::LocalAst,
            line_start: 0,
            line_end: 5,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
            suggested_fix: None,
            based_on_excerpt: None,
            reasoning: None,
            llm_confidence: None,
            confidence: None,
            cited_lines: None,
            grounding_status: None,
            grounding_confidence: None,
            model_agreement: None,
            rule_id: None,
            judge_verdict: None,
            judge_confidence: None,
            precision_tier: None,
            in_diff: None,
        };
        assert!(!f.is_valid());
    }

    // -- CalibratorAction serialization --

    #[test]
    fn calibrator_action_serializes_as_lowercase_string() {
        let actions = vec![
            (CalibratorAction::Confirmed, "confirmed"),
            (CalibratorAction::Disputed, "disputed"),
            (CalibratorAction::Adjusted, "adjusted"),
            (CalibratorAction::Added, "added"),
        ];
        for (action, expected) in actions {
            let json = serde_json::to_value(&action).unwrap();
            assert_eq!(json.as_str().unwrap(), expected);
        }
    }

    // -- Severity serialization --

    #[test]
    fn severity_serializes_as_lowercase() {
        assert_eq!(
            serde_json::to_value(Severity::Critical).unwrap(),
            "critical"
        );
        assert_eq!(serde_json::to_value(Severity::High).unwrap(), "high");
        assert_eq!(serde_json::to_value(Severity::Medium).unwrap(), "medium");
        assert_eq!(serde_json::to_value(Severity::Low).unwrap(), "low");
        assert_eq!(serde_json::to_value(Severity::Info).unwrap(), "info");
    }

    // -- FindingBuilder (test support) --

    #[test]
    fn builder_produces_valid_finding_with_defaults() {
        let f = FindingBuilder::new().build();
        assert!(f.is_valid());
        assert_eq!(f.severity, Severity::Info);
        assert_eq!(f.source, Source::LocalAst);
    }

    #[test]
    fn builder_overrides_all_fields() {
        let f = FindingBuilder::new()
            .title("Custom title")
            .description("Custom desc")
            .severity(Severity::Critical)
            .category(Category::Security)
            .source(Source::Llm("gpt-5.4".into()))
            .lines(10, 20)
            .evidence("some evidence")
            .calibrator_action(CalibratorAction::Confirmed)
            .precedent("similar finding")
            .build();

        assert_eq!(f.title, "Custom title");
        assert_eq!(f.description, "Custom desc");
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.category, Category::Security);
        assert_eq!(f.source, Source::Llm("gpt-5.4".into()));
        assert_eq!(f.line_start, 10);
        assert_eq!(f.line_end, 20);
        assert_eq!(f.evidence, vec!["some evidence".to_string()]);
        assert_eq!(f.calibrator_action, Some(CalibratorAction::Confirmed));
        assert_eq!(f.similar_precedent, vec!["similar finding".to_string()]);
    }

    #[test]
    fn finding_suggested_fix_serializes() {
        let f = FindingBuilder::new()
            .suggested_fix("Use parameterized queries instead")
            .build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("suggested_fix"));
        assert!(json.contains("Use parameterized queries instead"));
    }

    #[test]
    fn finding_no_suggested_fix_omitted_from_json() {
        let f = FindingBuilder::new().build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("suggested_fix"));
    }

    #[test]
    fn finding_based_on_excerpt_serializes() {
        let f = FindingBuilder::new()
            .based_on_excerpt("lines 1-150 of 500")
            .build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("based_on_excerpt"));
        assert!(json.contains("lines 1-150 of 500"));
    }

    #[test]
    fn finding_no_excerpt_omitted_from_json() {
        let f = FindingBuilder::new().build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("based_on_excerpt"));
    }

    // ─── Stats redesign Phase 0: Finding.id (per-finding identity) ───

    #[test]
    fn finding_builder_assigns_unique_ulid_id() {
        let a = FindingBuilder::new().build();
        let b = FindingBuilder::new().build();
        assert_eq!(
            a.id.len(),
            26,
            "ULID is 26 chars in canonical Crockford encoding"
        );
        assert_ne!(a.id, b.id, "each FindingBuilder produces a fresh ULID");
        // ULID monotonicity isn't guaranteed across rapid calls; just check
        // both are valid Crockford-base32 — alphanumeric, no I/L/O/U.
        for c in a.id.chars() {
            assert!(
                c.is_ascii_alphanumeric(),
                "ULID char must be alphanumeric: {c}"
            );
        }
    }

    #[test]
    fn finding_id_explicit_overrides_builder_default() {
        let f = FindingBuilder::new().id("my-explicit-id").build();
        assert_eq!(f.id, "my-explicit-id");
    }

    #[test]
    #[should_panic(expected = "non-empty")]
    fn finding_builder_id_rejects_empty_string() {
        // Empty IDs would silently corrupt collect_finding_ids /
        // linkage_stats: every finding with id="" would join to every
        // feedback row with the same empty id. The default
        // (`new_finding_ulid`) is non-empty by construction; the explicit
        // setter must enforce the same invariant for round-trip /
        // test-fixture callers.
        let _ = FindingBuilder::new().id("").build();
    }

    #[test]
    fn finding_serializes_id_into_json() {
        let f = FindingBuilder::new().id("01HXYZ").build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains("\"id\":\"01HXYZ\""),
            "id must appear in JSON: {json}"
        );
    }

    #[test]
    fn finding_legacy_json_without_id_deserializes_with_default() {
        // Pre-rollout JSON dumps (e.g. piped output saved to disk before the
        // schema bump) must still load. They get a freshly-minted ULID.
        let legacy = r#"{"title":"t","description":"d","severity":"info","category":"maintainability","source":"local-ast","line_start":1,"line_end":1,"evidence":[],"calibrator_action":null,"similar_precedent":[]}"#;
        let f: Finding = serde_json::from_str(legacy).expect("legacy load");
        assert_eq!(f.id.len(), 26, "missing id deserializes to a fresh ULID");
    }

    #[test]
    fn collect_finding_ids_preserves_order() {
        let a = FindingBuilder::new().id("ID-A").build();
        let b = FindingBuilder::new().id("ID-B").build();
        let c = FindingBuilder::new().id("ID-C").build();
        let ids = collect_finding_ids(&[a, b, c]);
        assert_eq!(ids, vec!["ID-A", "ID-B", "ID-C"]);
    }

    #[test]
    fn collect_finding_ids_empty_input_yields_empty_vec() {
        let ids = collect_finding_ids(&[]);
        assert!(ids.is_empty());
    }

    // -- compute_confidence --

    #[test]
    fn confidence_local_ast_is_high() {
        let mut f = FindingBuilder::new().source(Source::LocalAst).build();
        f.compute_confidence();
        assert_eq!(f.confidence, Some(0.95));
    }

    #[test]
    fn confidence_linter_is_high() {
        let mut f = FindingBuilder::new()
            .source(Source::Linter("clippy".into()))
            .build();
        f.compute_confidence();
        assert_eq!(f.confidence, Some(0.90));
    }

    #[test]
    fn confidence_llm_verified_grounding() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        f.grounding_confidence = Some(1.0);
        f.compute_confidence();
        assert_eq!(f.confidence, Some(1.0));
    }

    #[test]
    fn confidence_llm_symbol_not_found() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        f.grounding_confidence = Some(0.3);
        f.compute_confidence();
        assert_eq!(f.confidence, Some(0.3));
    }

    #[test]
    fn confidence_llm_no_grounding_uses_default() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        f.compute_confidence();
        assert_eq!(f.confidence, Some(0.5));
    }

    #[test]
    fn confidence_calibrator_confirmed_boosts() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .calibrator_action(CalibratorAction::Confirmed)
            .build();
        f.grounding_confidence = Some(0.5);
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.55).abs() < 0.01);
    }

    #[test]
    fn confidence_calibrator_disputed_penalizes() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .calibrator_action(CalibratorAction::Disputed)
            .build();
        f.grounding_confidence = Some(1.0);
        f.compute_confidence();
        assert_eq!(f.confidence, Some(0.6));
    }

    #[test]
    fn confidence_clamped_to_one() {
        let mut f = FindingBuilder::new()
            .source(Source::LocalAst)
            .calibrator_action(CalibratorAction::Confirmed)
            .build();
        f.compute_confidence();
        assert_eq!(f.confidence, Some(1.0));
    }

    #[test]
    fn confidence_full_agreement_boosts() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .model_agreement(1.0)
            .build();
        f.grounding_confidence = Some(0.5);
        f.compute_confidence();
        // 0.5 * 1.0 * (0.85 + 0.30) = 0.575
        assert!((f.confidence.unwrap() - 0.575).abs() < 0.01);
    }

    #[test]
    fn confidence_half_agreement_neutral() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .model_agreement(0.5)
            .build();
        f.grounding_confidence = Some(0.5);
        f.compute_confidence();
        // 0.5 * 1.0 * (0.85 + 0.15) = 0.5
        assert!((f.confidence.unwrap() - 0.5).abs() < 0.01);
    }

    #[test]
    fn confidence_no_agreement_info_unchanged() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        f.grounding_confidence = Some(0.5);
        f.compute_confidence();
        assert_eq!(f.confidence, Some(0.5));
    }

    #[test]
    fn confidence_low_agreement_penalizes() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .model_agreement(0.33)
            .build();
        f.grounding_confidence = Some(1.0);
        f.compute_confidence();
        // 1.0 * 1.0 * (0.85 + 0.099) = 0.949
        assert!(f.confidence.unwrap() < 1.0);
        assert!(f.confidence.unwrap() > 0.9);
    }

    #[test]
    fn confidence_nan_grounding_uses_default() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        f.grounding_confidence = Some(f32::NAN);
        f.compute_confidence();
        let c = f.confidence.unwrap();
        assert!(c.is_finite(), "NaN grounding must not propagate");
        assert!((c - 0.5).abs() < 0.01, "should fall back to 0.5 default");
    }

    #[test]
    fn confidence_inf_grounding_uses_default() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        f.grounding_confidence = Some(f32::INFINITY);
        f.compute_confidence();
        let c = f.confidence.unwrap();
        assert!(c.is_finite(), "Inf grounding must not propagate");
        assert!((c - 0.5).abs() < 0.01);
    }

    #[test]
    fn confidence_out_of_range_grounding_clamped() {
        let mut f = FindingBuilder::new()
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        f.grounding_confidence = Some(1.5);
        f.compute_confidence();
        assert!(
            f.confidence.unwrap() <= 1.0,
            "grounding > 1.0 must be clamped"
        );
    }

    // -- rule_id field --

    #[test]
    fn finding_rule_id_defaults_to_none() {
        let f = FindingBuilder::new().build();
        assert_eq!(f.rule_id, None);
    }

    #[test]
    fn finding_rule_id_set_via_builder() {
        let f = FindingBuilder::new()
            .rule_id("ast-grep:python/bare-except-pass")
            .build();
        assert_eq!(
            f.rule_id.as_deref(),
            Some("ast-grep:python/bare-except-pass")
        );
    }

    #[test]
    fn finding_rule_id_survives_json_roundtrip() {
        let f = FindingBuilder::new()
            .rule_id("local-ast:complexity")
            .build();
        let json = serde_json::to_string(&f).unwrap();
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.rule_id.as_deref(), Some("local-ast:complexity"));
    }

    #[test]
    fn finding_without_rule_id_omits_from_json() {
        let f = FindingBuilder::new().build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("rule_id"));
    }

    #[test]
    fn legacy_json_without_rule_id_deserializes_as_none() {
        let json = r#"{"title":"t","description":"d","severity":"info","category":"maintainability","source":"local-ast","line_start":1,"line_end":1,"evidence":[],"similar_precedent":[]}"#;
        let f: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(f.rule_id, None);
    }

    // -- PrecisionTier / JudgeRequirement / JudgeVerdict --

    #[test]
    fn precision_tier_default_is_high() {
        let tier: PrecisionTier = Default::default();
        assert_eq!(tier, PrecisionTier::High);
    }

    #[test]
    fn precision_tier_serde_roundtrip() {
        let tier = PrecisionTier::Speculative;
        let json = serde_json::to_string(&tier).unwrap();
        assert_eq!(json, "\"speculative\"");
        let parsed: PrecisionTier = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, PrecisionTier::Speculative);
    }

    #[test]
    fn judge_requirement_default_is_skip() {
        let req: JudgeRequirement = Default::default();
        assert_eq!(req, JudgeRequirement::Skip);
    }

    #[test]
    fn judge_verdict_serde_roundtrip() {
        let v = JudgeVerdict::Approved;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"approved\"");
        let parsed: JudgeVerdict = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, JudgeVerdict::Approved);
    }

    #[test]
    fn finding_judge_fields_default_to_none() {
        let f = FindingBuilder::new().build();
        assert_eq!(f.judge_verdict, None);
        assert_eq!(f.judge_confidence, None);
        assert_eq!(f.precision_tier, None);
    }

    #[test]
    fn finding_with_judge_fields_survives_roundtrip() {
        let mut f = FindingBuilder::new().build();
        f.judge_verdict = Some(JudgeVerdict::Approved);
        f.judge_confidence = Some(0.85);
        f.precision_tier = Some(PrecisionTier::Speculative);
        let json = serde_json::to_string(&f).unwrap();
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.judge_verdict, Some(JudgeVerdict::Approved));
        assert_eq!(parsed.judge_confidence, Some(0.85));
        assert_eq!(parsed.precision_tier, Some(PrecisionTier::Speculative));
    }

    #[test]
    fn finding_without_judge_fields_omits_from_json() {
        let f = FindingBuilder::new().build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("judge_verdict"));
        assert!(!json.contains("judge_confidence"));
        assert!(!json.contains("precision_tier"));
    }

    // -- Tier-aware confidence --

    #[test]
    fn confidence_speculative_local_ast_is_050() {
        let mut f = FindingBuilder::new().source(Source::LocalAst).build();
        f.precision_tier = Some(PrecisionTier::Speculative);
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.50).abs() < 0.01);
    }

    #[test]
    fn confidence_medium_local_ast_is_080() {
        let mut f = FindingBuilder::new().source(Source::LocalAst).build();
        f.precision_tier = Some(PrecisionTier::Medium);
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.80).abs() < 0.01);
    }

    #[test]
    fn confidence_medium_linter_is_075() {
        let mut f = FindingBuilder::new()
            .source(Source::Linter("ast-grep".into()))
            .build();
        f.precision_tier = Some(PrecisionTier::Medium);
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.75).abs() < 0.01);
    }

    #[test]
    fn confidence_speculative_linter_is_045() {
        let mut f = FindingBuilder::new()
            .source(Source::Linter("ast-grep".into()))
            .build();
        f.precision_tier = Some(PrecisionTier::Speculative);
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.45).abs() < 0.01);
    }

    #[test]
    fn confidence_judge_overrides_base() {
        let mut f = FindingBuilder::new().source(Source::LocalAst).build();
        f.precision_tier = Some(PrecisionTier::Speculative);
        f.judge_confidence = Some(0.85);
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.85).abs() < 0.01);
    }

    #[test]
    fn confidence_no_tier_matches_legacy_behavior() {
        let mut f = FindingBuilder::new().source(Source::LocalAst).build();
        f.precision_tier = None;
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.95).abs() < 0.01);
    }

    #[test]
    fn confidence_judge_nan_falls_back_to_base() {
        let mut f = FindingBuilder::new().source(Source::LocalAst).build();
        f.judge_confidence = Some(f32::NAN);
        f.compute_confidence();
        assert!((f.confidence.unwrap() - 0.95).abs() < 0.01);
    }

    // -- in_diff field --

    #[test]
    fn in_diff_serde_round_trip() {
        let mut f = FindingBuilder::new().build();
        assert_eq!(f.in_diff, None);

        f.in_diff = Some(true);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"in_diff\":true"));
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.in_diff, Some(true));

        f.in_diff = Some(false);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"in_diff\":false"));
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.in_diff, Some(false));
    }

    #[test]
    fn in_diff_omitted_deserializes_as_none() {
        let json = r#"{"title":"t","description":"d","severity":"info","category":"maintainability","source":"local-ast","line_start":1,"line_end":1,"evidence":[],"similar_precedent":[]}"#;
        let f: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(f.in_diff, None);
    }

    #[test]
    fn in_diff_none_not_serialized() {
        let f = FindingBuilder::new().build();
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("in_diff"));
    }
}
