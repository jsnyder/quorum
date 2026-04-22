/// Secret redaction: strip API keys, passwords, tokens from code before sending to LLM.
/// Always-on — no opt-out.

use regex::Regex;
use std::sync::LazyLock;

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        // Private key blocks
        (Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(),
         "[REDACTED PRIVATE KEY]"),
        // AWS access key IDs (covers AKIA, ASIA, ABIA, ACCA, A3T*)
        (Regex::new(r"(?:A3T[A-Z0-9]|ABIA|ACCA|AKIA|ASIA)[0-9A-Z]{16}").unwrap(), "[REDACTED]"),
        // GitHub tokens (official format with underscore in charset)
        (Regex::new(r"(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9_]{36,}").unwrap(), "[REDACTED]"),
        // Slack tokens
        (Regex::new(r"xox[abposr]-(?:\d+-)+[a-z0-9]+").unwrap(), "[REDACTED]"),
        // Stripe keys
        (Regex::new(r"[rs]k_live_[0-9a-zA-Z]{24,}").unwrap(), "[REDACTED]"),
        // Twilio keys
        (Regex::new(r"(?:AC|SK)[a-z0-9]{32}").unwrap(), "[REDACTED]"),
        // Bearer tokens (JWT-like)
        (Regex::new(r"Bearer\s+[A-Za-z0-9\-._~+/]+=*").unwrap(), "Bearer [REDACTED]"),
        // Generic secret assignments: KEY="value", PASSWORD='value'
        // Only matches quoted string literals — two patterns for double and single quotes.
        // The leading `(^|[^A-Za-z])` group anchors the alternation to either
        // start-of-input or a non-letter character, preventing partial matches
        // inside longer identifiers like `oauth = "..."` (which used to match
        // the `auth` substring) or `mytoken = "..."` (matched on `token`).
        // Underscores ARE allowed as boundaries so legitimate names like
        // `GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY` still redact correctly.
        // Captured boundary char ($1) is preserved in the replacement so we
        // don't accidentally rewrite surrounding source.
        (Regex::new(r#"(?i)(^|[^A-Za-z])((?:api[_-]?key|password|secret|token|passwd|auth)\s*[=:]\s*)"([^\n"]{6,})""#).unwrap(),
         "$1$2\"[REDACTED]\""),
        (Regex::new(r#"(?i)(^|[^A-Za-z])((?:api[_-]?key|password|secret|token|passwd|auth)\s*[=:]\s*)'([^\n']{6,})'"#).unwrap(),
         "$1$2'[REDACTED]'"),
        // OpenAI-style keys
        (Regex::new(r"sk-[a-zA-Z0-9\-]{6,}").unwrap(), "[REDACTED]"),
        // URLs with passwords: protocol://user:password@host
        (Regex::new(r"(://[^:/@]+:)([^@\s]{3,})(@)").unwrap(), "${1}[REDACTED]${3}"),
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
    fn redact_preserves_getenv_calls() {
        let input = "api_key = os.getenv('API_KEY')";
        let output = redact_secrets(input);
        assert_eq!(input, output, "getenv calls should NOT be redacted");
    }

    #[test]
    fn redact_preserves_bare_variable_assignment() {
        let input = "api_key=openai_api_key";
        let output = redact_secrets(input);
        assert_eq!(input, output, "Bare variable references should NOT be redacted");
    }

    #[test]
    fn redact_preserves_variable_references() {
        let input = "api_key = os.getenv('OPENAI_API_KEY')\nopenai_api_key = config.api_key";
        let output = redact_secrets(input);
        assert_eq!(input, output, "Variable references and function calls should NOT be redacted");
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
    fn redact_does_not_match_inside_larger_identifier() {
        // Regression: previously the unanchored alternation matched the `auth`
        // substring in `oauth` and the `token` substring in `mytoken`, redacting
        // benign value strings that happened to be assigned to non-secret vars.
        let input = "let oauth = \"client_id_abc123\";\nlet mytoken = \"opaque_value\";";
        let output = redact_secrets(input);
        assert_eq!(
            input, output,
            "non-secret identifiers ending in auth/token must not be redacted; got: {output}"
        );
    }

    #[test]
    fn redact_multiple_secrets_in_one_text() {
        let input = "KEY=\"sk-test-123\"\nPASSWORD=\"hunter2-pass\"\nfn safe() {}";
        let output = redact_secrets(input);
        assert!(!output.contains("sk-test-123"));
        assert!(!output.contains("hunter2-pass"));
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

    #[test]
    fn redact_aws_temporary_credentials() {
        let input = "AWS_KEY=ASIAXXX1234567890123";
        let output = redact_secrets(input);
        assert!(!output.contains("ASIAXXX123456789"));
    }

    #[test]
    fn redact_slack_token() {
        let input = "SLACK_TOKEN=xoxb-123456-789012-abcdef123456";
        let output = redact_secrets(input);
        assert!(!output.contains("xoxb-"));
    }

    #[test]
    fn redact_stripe_key() {
        let input = "STRIPE_KEY=sk_live_abc123def456ghi789jkl012";
        let output = redact_secrets(input);
        assert!(!output.contains("sk_live_"));
    }
}
