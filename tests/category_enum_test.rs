use quorum::category::Category;

#[test]
fn all_returns_ten_variants() {
    let all = Category::all();
    assert_eq!(all.len(), 10);
}

#[test]
fn all_contains_every_variant() {
    let all = Category::all();
    assert!(all.contains(&Category::Security));
    assert!(all.contains(&Category::Correctness));
    assert!(all.contains(&Category::Logic));
    assert!(all.contains(&Category::Concurrency));
    assert!(all.contains(&Category::Reliability));
    assert!(all.contains(&Category::Robustness));
    assert!(all.contains(&Category::ErrorHandling));
    assert!(all.contains(&Category::Validation));
    assert!(all.contains(&Category::Performance));
    assert!(all.contains(&Category::Maintainability));
}

#[test]
fn serde_roundtrip_all_variants() {
    for cat in Category::all() {
        let json = serde_json::to_string(&cat).unwrap();
        let back: Category = serde_json::from_str(&json).unwrap();
        assert_eq!(cat, back, "roundtrip failed for {cat:?}: json={json}");
    }
}

#[test]
fn serde_uses_kebab_case() {
    assert_eq!(serde_json::to_string(&Category::ErrorHandling).unwrap(), "\"error-handling\"");
    assert_eq!(serde_json::to_string(&Category::Security).unwrap(), "\"security\"");
}

#[test]
fn from_string_maps_legacy_bug_to_correctness() {
    assert_eq!(Category::from("bug"), Category::Correctness);
}

#[test]
fn from_string_maps_code_quality_variants() {
    assert_eq!(Category::from("code_quality"), Category::Maintainability);
    assert_eq!(Category::from("code-quality"), Category::Maintainability);
    assert_eq!(Category::from("quality"), Category::Maintainability);
}

#[test]
fn from_string_maps_complexity_to_performance() {
    assert_eq!(Category::from("complexity"), Category::Performance);
    assert_eq!(Category::from("performance"), Category::Performance);
}

#[test]
fn from_string_maps_error_handling_variants() {
    assert_eq!(Category::from("error handling"), Category::ErrorHandling);
    assert_eq!(Category::from("error-handling"), Category::ErrorHandling);
}

#[test]
fn from_string_maps_security_variants() {
    assert_eq!(Category::from("security"), Category::Security);
    assert_eq!(Category::from("safety"), Category::Security);
}

#[test]
fn from_string_maps_correctness() {
    assert_eq!(Category::from("correctness"), Category::Correctness);
    assert_eq!(Category::from("functional_bug"), Category::Correctness);
}

#[test]
fn from_string_maps_reliability() {
    assert_eq!(Category::from("reliability"), Category::Reliability);
    assert_eq!(Category::from("resource-lifecycle"), Category::Reliability);
    assert_eq!(Category::from("resource-management"), Category::Reliability);
}

#[test]
fn from_string_case_insensitive() {
    assert_eq!(Category::from("Security"), Category::Security);
    assert_eq!(Category::from("PERFORMANCE"), Category::Performance);
    assert_eq!(Category::from("Testing"), Category::Maintainability);
}

#[test]
fn from_string_unknown_falls_to_maintainability() {
    assert_eq!(Category::from("style"), Category::Maintainability);
    assert_eq!(Category::from("docs"), Category::Maintainability);
    assert_eq!(Category::from("ast-pattern"), Category::Maintainability);
    assert_eq!(Category::from(""), Category::Maintainability);
}

#[test]
fn display_matches_as_str() {
    for cat in Category::all() {
        assert_eq!(format!("{cat}"), cat.as_str());
    }
}

#[test]
fn partial_eq_str_matches_as_str() {
    assert!(Category::Security == "security");
    assert!(Category::ErrorHandling == "error-handling");
    assert!(!(Category::Security == "performance"));
}
