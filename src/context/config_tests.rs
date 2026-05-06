use super::config::*;

#[test]
fn loads_valid_sources_toml() {
    let toml = r#"
[[source]]
name = "internal-auth"
git = "git@github.com:myorg/auth.git"
rev = "main"
kind = "rust"
weight = 10

[[source]]
name = "tf-net"
path = "../terraform-modules/networking"
kind = "terraform"

[context]
inject_budget_tokens = 1500
inject_min_score = 0.65
"#;
    let config = SourcesConfig::from_str(toml).unwrap();
    assert_eq!(config.sources.len(), 2);
    assert_eq!(config.sources[0].name, "internal-auth");
    assert_eq!(config.sources[0].kind, SourceKind::Rust);
    assert_eq!(config.sources[0].weight, Some(10));
    assert!(matches!(
        config.sources[0].location,
        SourceLocation::Git { .. }
    ));
    match &config.sources[0].location {
        SourceLocation::Git { url, rev } => {
            assert_eq!(url, "git@github.com:myorg/auth.git");
            assert_eq!(rev.as_deref(), Some("main"));
        }
        _ => panic!("expected git"),
    }
    assert_eq!(config.sources[1].name, "tf-net");
    assert_eq!(config.sources[1].kind, SourceKind::Terraform);
    assert!(matches!(
        config.sources[1].location,
        SourceLocation::Path(_)
    ));
    assert_eq!(config.context.inject_budget_tokens, 1500);
}

#[test]
fn rejects_source_with_both_git_and_path() {
    let toml = r#"
[[source]]
name = "bad"
git = "x"
path = "y"
kind = "rust"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("exactly one")
            || err.to_string().to_lowercase().contains("either"),
        "got: {err}"
    );
}

#[test]
fn rejects_source_with_neither_git_nor_path() {
    let toml = r#"
[[source]]
name = "bad"
kind = "rust"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("exactly one")
            || err.to_string().to_lowercase().contains("either")
            || err.to_string().to_lowercase().contains("missing"),
        "got: {err}"
    );
}

#[test]
fn rejects_duplicate_source_names() {
    let toml = r#"
[[source]]
name = "dup"
path = "a"
kind = "rust"

[[source]]
name = "dup"
path = "b"
kind = "rust"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("duplicate"),
        "got: {err}"
    );
}

#[test]
fn rejects_unknown_kind() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "cobol"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("unknown")
            || err.to_string().to_lowercase().contains("kind")
            || err.to_string().to_lowercase().contains("invalid"),
        "got: {err}"
    );
}

#[test]
fn defaults_fill_missing_context_block() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"
"#;
    let config = SourcesConfig::from_str(toml).unwrap();
    assert_eq!(config.context.inject_budget_tokens, 1500);
    assert!((config.context.inject_min_score - 0.80).abs() < f32::EPSILON);
    assert_eq!(config.context.inject_max_chunks, 4);
    assert_eq!(config.context.rerank_recency_halflife_days, 90);
    assert!((config.context.rerank_recency_floor - 0.25).abs() < f32::EPSILON);
    assert_eq!(config.context.max_source_size_mb, 200);
    assert!(config.context.auto_inject);
}

#[test]
fn min_score_out_of_range_rejected() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[context]
inject_min_score = 1.5
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("min_score")
            || err.to_string().contains("[0.0, 1.0]"),
        "got: {err}"
    );
}

#[test]
fn recency_floor_out_of_range_rejected() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[context]
rerank_recency_floor = 1.5
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("recency_floor")
            || err.to_string().contains("[0.0, 1.0]"),
        "got: {err}"
    );
}

#[test]
fn example_fixture_loads() {
    let path = std::path::Path::new("tests/fixtures/context/sources/example-sources.toml");
    let config = SourcesConfig::load(path).unwrap();
    assert_eq!(config.sources.len(), 3);
    let names: Vec<_> = config.sources.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"mini-rust"));
    assert!(names.contains(&"mini-ts"));
    assert!(names.contains(&"mini-terraform"));
}

#[test]
fn rejects_zero_inject_budget() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[context]
inject_budget_tokens = 0
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(
        err.to_string().contains("inject_budget_tokens"),
        "got: {err}"
    );
}

#[test]
fn rejects_zero_max_chunks() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[context]
inject_max_chunks = 0
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(err.to_string().contains("inject_max_chunks"), "got: {err}");
}

#[test]
fn rejects_zero_halflife() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[context]
rerank_recency_halflife_days = 0
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(err.to_string().contains("halflife"), "got: {err}");
}

#[test]
fn rejects_zero_max_source_size() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[context]
max_source_size_mb = 0
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(err.to_string().contains("max_source_size_mb"), "got: {err}");
}

#[test]
fn rejects_unknown_context_field() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[context]
inject_min_socre = 0.5   # typo
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(
        s.contains("unknown") || s.contains("inject_min_socre"),
        "got: {err}"
    );
}

#[test]
fn rejects_unknown_source_field() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"
weigth = 5   # typo
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(s.contains("unknown") || s.contains("weigth"), "got: {err}");
}

#[test]
fn rejects_unknown_top_level_field() {
    let toml = r#"
[[source]]
name = "x"
path = "."
kind = "rust"

[unknown_block]
foo = "bar"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(
        s.contains("unknown") || s.contains("unknown_block"),
        "got: {err}"
    );
}

#[test]
fn rejects_empty_git_string() {
    let toml = r#"
[[source]]
name = "x"
git = ""
kind = "rust"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(
        s.contains("exactly one") || s.contains("non-empty"),
        "got: {err}"
    );
}

#[test]
fn rejects_whitespace_only_path() {
    let toml = r#"
[[source]]
name = "x"
path = "   "
kind = "rust"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(
        s.contains("exactly one") || s.contains("non-empty"),
        "got: {err}"
    );
}

#[test]
fn rejects_rev_on_path_source() {
    let toml = r#"
[[source]]
name = "x"
path = "./foo"
rev = "main"
kind = "rust"
"#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(s.contains("rev") && s.contains("path"), "got: {err}");
}

#[test]
fn accepts_rev_on_git_source() {
    let toml = r#"
[[source]]
name = "x"
git = "git@host:org/repo.git"
rev = "main"
kind = "rust"
"#;
    let config = SourcesConfig::from_str(toml).unwrap();
    assert_eq!(config.sources.len(), 1);
    match &config.sources[0].location {
        SourceLocation::Git { url, rev } => {
            assert_eq!(url, "git@host:org/repo.git");
            assert_eq!(rev.as_deref(), Some("main"));
        }
        _ => panic!("expected git source"),
    }
}
