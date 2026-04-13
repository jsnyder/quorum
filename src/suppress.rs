use serde::Deserialize;

#[allow(unused_imports)]
use crate::finding::Finding;

#[derive(Debug, Clone, Deserialize)]
pub struct SuppressionRule {
    pub pattern: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SuppressConfig {
    #[serde(default)]
    suppress: Vec<SuppressionRule>,
}

pub fn parse_suppress_config(toml_str: &str) -> anyhow::Result<Vec<SuppressionRule>> {
    if toml_str.trim().is_empty() {
        return Ok(Vec::new());
    }
    let config: SuppressConfig = toml::from_str(toml_str)?;
    Ok(config.suppress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_suppress_config() {
        let toml = r#"
[[suppress]]
pattern = "TLS certificate"
category = "security"
file = "src/*.py"
reason = "Internal service, TLS not required"
"#;
        let rules = parse_suppress_config(toml).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "TLS certificate");
        assert_eq!(rules[0].category.as_deref(), Some("security"));
        assert_eq!(rules[0].file.as_deref(), Some("src/*.py"));
        assert_eq!(
            rules[0].reason.as_deref(),
            Some("Internal service, TLS not required")
        );
    }

    #[test]
    fn parse_empty_config_returns_empty_vec() {
        let rules = parse_suppress_config("").unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn parse_invalid_toml_returns_error() {
        let result = parse_suppress_config("this is not valid [[[ toml");
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_pattern_returns_error() {
        let toml = r#"
[[suppress]]
category = "security"
"#;
        let result = parse_suppress_config(toml);
        assert!(result.is_err());
    }

    #[test]
    fn parse_comments_only_returns_empty_vec() {
        let toml = r#"
# This is a comment
# Another comment
"#;
        let rules = parse_suppress_config(toml).unwrap();
        assert!(rules.is_empty());
    }
}
