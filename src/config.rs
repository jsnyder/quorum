use std::collections::HashMap;

pub trait ConfigSource {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct EnvConfigSource;

impl ConfigSource for EnvConfigSource {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
pub struct MapConfigSource(pub HashMap<String, String>);

#[cfg(test)]
impl ConfigSource for MapConfigSource {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

pub struct Config {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

impl Config {
    pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
    pub const DEFAULT_MODEL: &str = "gpt-5.4";

    pub fn load(source: &dyn ConfigSource) -> anyhow::Result<Self> {
        let base_url = Self::get_trimmed(source, "QUORUM_BASE_URL")
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| Self::DEFAULT_BASE_URL.into());

        Self::validate_url(&base_url)?;

        let api_key = Self::get_trimmed(source, "QUORUM_API_KEY");

        let model = Self::get_trimmed(source, "QUORUM_MODEL")
            .unwrap_or_else(|| Self::DEFAULT_MODEL.into());

        Ok(Config {
            base_url,
            api_key,
            model,
        })
    }

    fn validate_url(base_url: &str) -> anyhow::Result<()> {
        let parsed = url::Url::parse(base_url)
            .map_err(|e| anyhow::anyhow!("Invalid QUORUM_BASE_URL: {e}"))?;
        let is_local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
        if parsed.scheme() != "https" && !is_local {
            anyhow::bail!("QUORUM_BASE_URL must use https (http allowed only for localhost)");
        }
        Ok(())
    }

    pub fn require_api_key(&self) -> anyhow::Result<&str> {
        self.api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("QUORUM_API_KEY is required. Set it or pass --api-key."))
    }

    fn get_trimmed(source: &dyn ConfigSource, key: &str) -> Option<String> {
        source
            .get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn source_from(pairs: &[(&str, &str)]) -> MapConfigSource {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        MapConfigSource(map)
    }

    fn empty_source() -> MapConfigSource {
        MapConfigSource(HashMap::new())
    }

    // -- Loading from env vars --

    #[test]
    fn config_loads_all_env_vars() {
        let src = source_from(&[
            ("QUORUM_BASE_URL", "https://llm.example.com"),
            ("QUORUM_API_KEY", "sk-test-key"),
            ("QUORUM_MODEL", "claude-opus"),
        ]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.base_url, "https://llm.example.com");
        assert_eq!(config.api_key, Some("sk-test-key".into()));
        assert_eq!(config.model, "claude-opus");
    }

    // -- Defaults --

    #[test]
    fn config_defaults_when_env_unset() {
        let config = Config::load(&empty_source()).unwrap();
        assert_eq!(config.base_url, Config::DEFAULT_BASE_URL);
        assert_eq!(config.model, Config::DEFAULT_MODEL);
        assert_eq!(config.api_key, None);
    }

    #[test]
    fn config_default_base_url_is_openai_compatible() {
        assert!(Config::DEFAULT_BASE_URL.starts_with("https://"));
    }

    // -- API key handling --

    #[test]
    fn config_api_key_none_when_unset() {
        let config = Config::load(&empty_source()).unwrap();
        assert_eq!(config.api_key, None);
    }

    #[test]
    fn config_require_api_key_returns_error_when_missing() {
        let config = Config::load(&empty_source()).unwrap();
        let err = config.require_api_key();
        assert!(err.is_err());
    }

    #[test]
    fn config_require_api_key_returns_key_when_present() {
        let src = source_from(&[("QUORUM_API_KEY", "sk-test")]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.require_api_key().unwrap(), "sk-test");
    }

    // -- Whitespace trimming --

    #[test]
    fn config_trims_whitespace_from_values() {
        let src = source_from(&[
            ("QUORUM_API_KEY", "  sk-test  \n"),
            ("QUORUM_MODEL", "  gpt-5.4\t"),
        ]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.api_key, Some("sk-test".into()));
        assert_eq!(config.model, "gpt-5.4");
    }

    // -- Trailing slash normalization --

    #[test]
    fn config_normalizes_trailing_slash_on_base_url() {
        let src = source_from(&[("QUORUM_BASE_URL", "https://example.com/")]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.base_url, "https://example.com");
    }

    #[test]
    fn config_no_trailing_slash_unchanged() {
        let src = source_from(&[("QUORUM_BASE_URL", "https://example.com")]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.base_url, "https://example.com");
    }

    // -- Empty string treated as unset --

    #[test]
    fn config_empty_base_url_uses_default() {
        let src = source_from(&[("QUORUM_BASE_URL", "")]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.base_url, Config::DEFAULT_BASE_URL);
    }

    #[test]
    fn config_empty_api_key_treated_as_none() {
        let src = source_from(&[("QUORUM_API_KEY", "")]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.api_key, None);
    }

    #[test]
    fn config_whitespace_only_api_key_treated_as_none() {
        let src = source_from(&[("QUORUM_API_KEY", "   ")]);
        let config = Config::load(&src).unwrap();
        assert_eq!(config.api_key, None);
    }

    // -- URL validation --

    #[test]
    fn config_rejects_invalid_url() {
        let src = source_from(&[("QUORUM_BASE_URL", "not a url")]);
        assert!(Config::load(&src).is_err());
    }

    #[test]
    fn config_rejects_http_url() {
        let src = source_from(&[("QUORUM_BASE_URL", "http://insecure.example.com")]);
        assert!(Config::load(&src).is_err());
    }

    #[test]
    fn config_accepts_https_url() {
        let src = source_from(&[("QUORUM_BASE_URL", "https://secure.example.com")]);
        assert!(Config::load(&src).is_ok());
    }

    #[test]
    fn config_accepts_localhost_http_for_development() {
        let src = source_from(&[("QUORUM_BASE_URL", "http://localhost:8080")]);
        assert!(Config::load(&src).is_ok());
    }

    #[test]
    fn config_accepts_127_http_for_development() {
        let src = source_from(&[("QUORUM_BASE_URL", "http://127.0.0.1:4000")]);
        assert!(Config::load(&src).is_ok());
    }
}
