use quorum::category::Category;
use quorum::finding::{Finding, FindingBuilder, GroundingStatus, Severity, Source};

#[test]
fn finding_with_new_fields_roundtrips() {
    let finding = Finding {
        title: "SQL injection".into(),
        description: "Unsanitized input".into(),
        severity: Severity::High,
        category: Category::Security,
        source: Source::Llm("gpt-5.4".into()),
        line_start: 10,
        line_end: 15,
        evidence: vec!["query = f\"SELECT ...\"".into()],
        calibrator_action: None,
        similar_precedent: vec![],
        canonical_pattern: None,
        suggested_fix: Some("Use parameterized queries".into()),
        based_on_excerpt: None,
        reasoning: Some("Direct string interpolation in SQL".into()),
        confidence: Some(0.92),
        cited_lines: Some((10, 12)),
        grounding_status: None,
    };

    let json = serde_json::to_string(&finding).unwrap();
    let back: Finding = serde_json::from_str(&json).unwrap();
    assert_eq!(finding, back);
}

#[test]
fn old_json_without_new_fields_deserializes() {
    let old_json = r#"{
        "title": "Unused import",
        "description": "os is imported but never used",
        "severity": "low",
        "category": "maintainability",
        "source": {"local-ast": null},
        "line_start": 1,
        "line_end": 1,
        "evidence": [],
        "calibrator_action": null,
        "similar_precedent": []
    }"#;

    let finding: Finding = serde_json::from_str(old_json).unwrap();
    assert_eq!(finding.title, "Unused import");
    assert_eq!(finding.category, Category::Maintainability);
    assert!(finding.reasoning.is_none());
    assert!(finding.confidence.is_none());
    assert!(finding.cited_lines.is_none());
}

#[test]
fn new_fields_omitted_from_json_when_none() {
    let finding = Finding {
        title: "Test".into(),
        description: "Test".into(),
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
        confidence: None,
        cited_lines: None,
        grounding_status: None,
    };

    let json = serde_json::to_string(&finding).unwrap();
    assert!(!json.contains("reasoning"));
    assert!(!json.contains("confidence"));
    assert!(!json.contains("cited_lines"));
}

#[test]
fn category_serializes_as_kebab_case_in_finding() {
    let finding = Finding {
        title: "Test".into(),
        description: "Test".into(),
        severity: Severity::Info,
        category: Category::ErrorHandling,
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
        confidence: None,
        cited_lines: None,
        grounding_status: None,
    };

    let json = serde_json::to_string(&finding).unwrap();
    assert!(
        json.contains("\"error-handling\""),
        "expected kebab-case in JSON: {json}"
    );
}

#[test]
fn confidence_accepts_float_values() {
    let json = r#"{
        "title": "Test",
        "description": "Test",
        "severity": "info",
        "category": "security",
        "source": {"local-ast": null},
        "line_start": 1,
        "line_end": 1,
        "evidence": [],
        "calibrator_action": null,
        "similar_precedent": [],
        "confidence": 0.75
    }"#;

    let finding: Finding = serde_json::from_str(json).unwrap();
    assert_eq!(finding.confidence, Some(0.75));
}

#[test]
fn cited_lines_tuple_roundtrips() {
    let json = r#"{
        "title": "Test",
        "description": "Test",
        "severity": "info",
        "category": "security",
        "source": {"local-ast": null},
        "line_start": 1,
        "line_end": 1,
        "evidence": [],
        "calibrator_action": null,
        "similar_precedent": [],
        "cited_lines": [10, 25]
    }"#;

    let finding: Finding = serde_json::from_str(json).unwrap();
    assert_eq!(finding.cited_lines, Some((10, 25)));
}

#[test]
fn grounding_status_serde_roundtrip() {
    for status in [
        GroundingStatus::Verified,
        GroundingStatus::SymbolNotFound,
        GroundingStatus::LineOutOfRange,
        GroundingStatus::NotChecked,
    ] {
        let f = FindingBuilder::new()
            .grounding_status(status.clone())
            .build();
        let json = serde_json::to_string(&f).unwrap();
        let back: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(f.grounding_status, back.grounding_status);
    }
}

#[test]
fn grounding_status_absent_deserializes_as_none() {
    let json = r#"{"title":"T","description":"D","severity":"info","category":"security","source":"local-ast","line_start":1,"line_end":1,"evidence":[],"calibrator_action":null,"similar_precedent":[]}"#;
    let f: Finding = serde_json::from_str(json).unwrap();
    assert!(f.grounding_status.is_none());
}
