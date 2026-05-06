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
        //
        // Boundary class is `[^A-Za-z0-9]` (NOT `_` or `-`) so identifier
        // separators count as boundaries: `oauth` (no separator before `auth`)
        // does NOT match, but `MY_SECRET`, `GITHUB_TOKEN`, `DB-PASSWORD` do.
        //
        // The trailing `(?:[_-][A-Za-z0-9]+)*` allows the secret keyword to
        // be followed by additional `_WORD` / `-word` segments — required
        // for composite names like `AWS_SECRET_ACCESS_KEY` (matches on
        // `SECRET_ACCESS_KEY`) and `DB_PASSWORD_PRIMARY`.
        //
        // Captured boundary char ($1) is preserved in the replacement so we
        // don't accidentally rewrite surrounding source.
        // Issue #68: value class is escape-aware: `\\.` matches any
        // backslash-escape sequence (`\"`, `\\`, etc.); `[^\n"]` matches
        // any other non-quote/non-newline char. Greedy `+` still stops at
        // the first UNESCAPED closing quote, so we don't over-match across
        // adjacent quoted values on the same line. The `{6,}` floor is
        // dropped — the secret-keyword anchor is sufficient (cf. #61).
        // Issue #72: bare `auth` keyword was dropped — it produced a wide
        // FP class (`auth_log_path`, `auth_db_url`, `auth_endpoint`,
        // `auth_provider`, ...). True credential-shaped keys like
        // `auth_token` / `auth_password` / `auth_secret` / `auth_key`
        // STILL redact, because the boundary char `_` satisfies
        // `[^A-Za-z0-9]` and the regex then matches forward on the
        // credential-shaped keyword (`token`, `password`, `secret`, `api_key`).
        (Regex::new(r#"(?i)(^|[^A-Za-z0-9])((?:api[_-]?key|password|secret|token|passwd)(?:[_-][A-Za-z0-9]+)*\s*[=:]\s*)"((?:\\.|[^\n"])+)""#).unwrap(),
         "$1$2\"[REDACTED]\""),
        (Regex::new(r#"(?i)(^|[^A-Za-z0-9])((?:api[_-]?key|password|secret|token|passwd)(?:[_-][A-Za-z0-9]+)*\s*[=:]\s*)'((?:\\.|[^\n'])+)'"#).unwrap(),
         "$1$2'[REDACTED]'"),
        // OpenAI-style keys
        (Regex::new(r"sk-[a-zA-Z0-9\-]{6,}").unwrap(), "[REDACTED]"),
        // URLs with passwords: protocol://user:password@host.
        // Floor at 1 char: short passwords are rare in practice, but the
        // surrounding `://USER:` ... `@` context anchors the match well
        // enough that we don't need a length floor as a precision filter.
        (Regex::new(r"(://[^:/@]+:)([^@\s]+)(@)").unwrap(), "${1}[REDACTED]${3}"),
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
        assert_eq!(
            input, output,
            "Bare variable references should NOT be redacted"
        );
    }

    #[test]
    fn redact_preserves_variable_references() {
        let input = "api_key = os.getenv('OPENAI_API_KEY')\nopenai_api_key = config.api_key";
        let output = redact_secrets(input);
        assert_eq!(
            input, output,
            "Variable references and function calls should NOT be redacted"
        );
    }

    #[test]
    fn redact_bearer_token() {
        let input =
            r#"Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc123"#;
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
    fn redact_short_password_in_url() {
        // Issue #61: previously the URL-password regex required {3,}
        // characters, letting 1- and 2-char passwords leak through. Real
        // short passwords are rare but the floor was arbitrary.
        let cases = [
            ("postgres://user:a@host", "a"),
            ("postgres://user:ab@host", "ab"),
        ];
        for (input, password) in cases {
            let output = redact_secrets(input);
            assert!(
                !output.contains(&format!(":{password}@")),
                "short password {password:?} leaked through; output: {output}"
            );
        }
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
        let input =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
        let output = redact_secrets(input);
        assert!(!output.contains("MIIEpAIBAAKCAQEA"));
        assert!(output.contains("[REDACTED"));
    }

    #[test]
    fn redact_aws_secret_access_key_composite_name() {
        // The current alternation requires the secret keyword to sit
        // immediately before `=` or `:`. AWS_SECRET_ACCESS_KEY suffixes
        // `secret` with `_ACCESS_KEY`, so without composite-name handling
        // the value goes through to the LLM verbatim.
        let input = r#"AWS_SECRET_ACCESS_KEY = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY""#;
        let output = redact_secrets(input);
        assert!(
            !output.contains("wJalrXUtnFEMI"),
            "AWS_SECRET_ACCESS_KEY value must be redacted; got: {output}"
        );
    }

    #[test]
    fn redact_db_password_composite_name() {
        let input = r#"DB_PASSWORD_PRIMARY = "hunter2-prod""#;
        let output = redact_secrets(input);
        assert!(
            !output.contains("hunter2-prod"),
            "composite *_PASSWORD_* name must be redacted; got: {output}"
        );
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

    #[test]
    fn redact_quoted_secret_with_escaped_quote_in_value() {
        // Issue #68: PASSWORD="pa\"ssword" — the value class [^\n"]{6,}
        // stops at the first " and the {6,} floor fails on the 3-char
        // prefix `pa\`, so the secret leaks through.
        let cases = [r#"PASSWORD = "pa\"ssword""#, r#"API_KEY = "abc\"def""#];
        for input in cases {
            let output = redact_secrets(input);
            assert!(
                output.contains("[REDACTED]"),
                "expected redaction for {input:?}; got: {output}"
            );
            // Tighten: assert inner secret bytes are gone.
            assert!(
                !output.contains("ssword") && !output.contains("def"),
                "secret bytes leaked through; got: {output}"
            );
        }
    }

    #[test]
    fn redact_quoted_secret_with_escaped_single_quote_in_value() {
        let input = r#"TOKEN = 'it\'s-secret'"#;
        let output = redact_secrets(input);
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("s-secret"));
    }

    #[test]
    fn redact_does_not_consume_trailing_quote_after_escaped_quoted_value() {
        // Greedy escape-aware class must still stop at the FIRST unescaped
        // closing quote — not consume everything between first and last "
        // on the line.
        let input = r#"PASSWORD = "pa\"ssword" PUBLIC = "visible""#;
        let output = redact_secrets(input);
        assert!(
            output.contains("[REDACTED]"),
            "expected redaction; got: {output}"
        );
        // The non-secret keyword `PUBLIC` is not in the secret-keyword
        // anchor list, so its value should remain literally visible.
        assert!(
            output.contains("visible"),
            "regex over-matched and ate trailing keyword's value; got: {output}"
        );
    }

    #[test]
    fn redact_does_not_match_empty_quoted_value() {
        // Without the {6,} floor, the value class (?:\\.|[^\n"])+ requires
        // at least one char — empty quoted value should not redact.
        // (`+` is one-or-more; this pins that we didn't accidentally use `*`.)
        let input = r#"PASSWORD = """#;
        let output = redact_secrets(input);
        assert!(
            !output.contains("[REDACTED]"),
            "empty quoted value should not match; got: {output}"
        );
    }

    // ----- Issue #72: drop bare `auth` keyword -----

    #[test]
    fn redact_does_not_match_auth_log_path() {
        let input = r#"auth_log_path = "/var/log/auth.log""#;
        assert_eq!(
            input,
            redact_secrets(input).as_str(),
            "auth_log_path is a path, not a credential"
        );
    }

    #[test]
    fn redact_does_not_match_auth_db_url() {
        let input = r#"auth_db_url = "postgres://app@db.local:5432/auth""#;
        assert_eq!(
            input,
            redact_secrets(input).as_str(),
            "auth_db_url with no embedded password should not be redacted"
        );
    }

    #[test]
    fn redact_does_not_match_auth_endpoint() {
        let input = r#"auth_endpoint = "https://login.example.com/oauth/token""#;
        assert_eq!(input, redact_secrets(input).as_str());
    }

    #[test]
    fn redact_does_not_match_auth_provider() {
        let input = r#"auth_provider = "okta""#;
        assert_eq!(input, redact_secrets(input).as_str());
    }

    #[test]
    fn redact_does_not_match_authority_or_authorize() {
        // Regression guard: confirm `authority`/`authorize_url` pass through
        // unchanged. Note the current regex also doesn't match these (the
        // char after `auth` is `o`, which is neither `_`/`-` nor `\s`/`=`/`:`),
        // so this is a guard, not a RED test.
        for input in [
            r#"authority = "main""#,
            r#"authorize_url = "/oauth/authorize""#,
        ] {
            assert_eq!(
                input,
                redact_secrets(input).as_str(),
                "authority/authorize_* are not credentials; got: {}",
                redact_secrets(input)
            );
        }
    }

    #[test]
    fn redact_does_not_match_bare_auth_assignment() {
        // Locked behavior change for #72: `auth = "okta"` is no longer
        // redacted (was: bare `auth` keyword caught it). The argument is that
        // bare `auth = "..."` is rare in real configs and the value is more
        // likely a provider name than a secret. If you intend to revert this,
        // you'll need to delete this test first — that's the signal we want.
        let input = r#"auth = "okta""#;
        assert_eq!(input, redact_secrets(input).as_str());
    }

    #[test]
    fn redact_still_matches_auth_token_via_token_keyword() {
        // `auth_token` still redacts: boundary `_` satisfies `[^A-Za-z0-9]`,
        // then the `token` keyword matches forward. See plan #72 for full reasoning.
        let input = r#"auth_token = "abc123secret""#;
        let output = redact_secrets(input);
        assert!(
            output.contains("[REDACTED]"),
            "auth_token IS a credential and must still be redacted via the `token` keyword. got: {output}"
        );
        assert!(
            !output.contains("abc123secret"),
            "secret value leaked. got: {output}"
        );
    }

    #[test]
    fn redact_still_matches_auth_credential_keyword_case_insensitive() {
        // Pin the (?i) flag still works for auth_<credential> after dropping
        // bare `auth`. Each case must redact via its credential-shaped suffix.
        for input in [
            r#"Auth_Token = "tok1""#,   // via `token`
            r#"AUTH_PASSWORD = "pw1""#, // via `password`
            r#"auth-secret = "shh""#,   // via `secret` (hyphen variant)
            r#"AUTH_API_KEY = "ak""#,   // via `api_key` suffix chain
        ] {
            let output = redact_secrets(input);
            assert!(
                output.contains("[REDACTED]"),
                "case-insensitive auth_<credential> must still redact: input={input:?}, got={output}"
            );
        }
    }

    #[test]
    fn redact_still_matches_bare_token_password_secret_key() {
        // Regression guard: the four other keywords must still match bare
        // assignments after the regex change. Loop is fine here — failure
        // message names the keyword.
        for (input, kw) in [
            (r#"PASSWORD = "hunter2""#, "password"),
            (r#"SECRET = "shh""#, "secret"),
            (r#"TOKEN = "tok123""#, "token"),
            (r#"API_KEY = "ak_456""#, "api_key"),
            (r#"PASSWD = "pw1""#, "passwd"),
        ] {
            let output = redact_secrets(input);
            assert!(
                output.contains("[REDACTED]"),
                "{kw} keyword regressed: input={input:?}, got={output}"
            );
        }
    }
}
