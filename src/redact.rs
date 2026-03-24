/// Secret redaction: strip API keys, passwords, tokens from code before sending to LLM.
/// Always-on — no opt-out.

use regex::Regex;
use std::sync::LazyLock;

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        // Private key blocks
        (Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(),
         "[REDACTED PRIVATE KEY]"),
        // AWS access key IDs
        (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "[REDACTED]"),
        // GitHub tokens
        (Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}").unwrap(), "[REDACTED]"),
        // Bearer tokens (JWT-like)
        (Regex::new(r"Bearer\s+[A-Za-z0-9\-._~+/]+=*").unwrap(), "Bearer [REDACTED]"),
        // Generic secret assignments: KEY=value, PASSWORD=value, TOKEN=value, SECRET=value
        (Regex::new(r#"(?i)((?:api[_-]?key|password|secret|token|passwd|auth)\s*[=:]\s*)"?([^\s"'\n]{6,})"?"#).unwrap(),
         "$1[REDACTED]"),
        // OpenAI-style keys
        (Regex::new(r"sk-[a-zA-Z0-9\-]{6,}").unwrap(), "[REDACTED]"),
        // URLs with passwords: protocol://user:password@host
        (Regex::new(r"(://[^:]+:)([^@\s]{3,})(@)").unwrap(), "$1[REDACTED]$3"),
    ]
});

pub fn redact_secrets(text: &str) -> String {
    let mut result = text.to_string();
    for (pattern, replacement) in PATTERNS.iter() {
        result = pattern.replace_all(&result, *replacement).to_string();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_aws_key() {
        let input = r#"AWS_ACCESS_KEY_ID = "AKIAIOSFODNN7EXAMPLE""#;
        let output = redact_secrets(input);
        assert!(!output.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redact_generic_api_key_env() {
        let input = r#"API_KEY = "sk-proj-abc123def456""#;
        let output = redact_secrets(input);
        assert!(!output.contains("sk-proj-abc123def456"));
    }

    #[test]
    fn redact_bearer_token() {
        let input = r#"Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc123"#;
        let output = redact_secrets(input);
        assert!(!output.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn redact_password_in_url() {
        let input = "postgres://user:s3cretP4ss@localhost:5432/db";
        let output = redact_secrets(input);
        assert!(!output.contains("s3cretP4ss"));
    }

    #[test]
    fn redact_preserves_normal_code() {
        let input = "fn main() {\n    let x = 42;\n    println!(\"{}\", x);\n}";
        let output = redact_secrets(input);
        assert_eq!(input, output);
    }

    #[test]
    fn redact_multiple_secrets_in_one_text() {
        let input = "KEY=sk-test-123\nPASSWORD=hunter2\nfn safe() {}";
        let output = redact_secrets(input);
        assert!(!output.contains("sk-test-123"));
        assert!(!output.contains("hunter2"));
        assert!(output.contains("fn safe() {}"));
    }

    #[test]
    fn redact_private_key_block() {
        let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
        let output = redact_secrets(input);
        assert!(!output.contains("MIIEpAIBAAKCAQEA"));
        assert!(output.contains("[REDACTED"));
    }

    #[test]
    fn redact_github_token() {
        let input = r#"GITHUB_TOKEN = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefgh""#;
        let output = redact_secrets(input);
        assert!(!output.contains("ghp_ABCDEF"));
    }
}
