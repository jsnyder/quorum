use std::io::Read;
use std::path::Path;

use ast_grep_config::{from_yaml_string, GlobalRules, RuleConfig, Severity as AstSeverity};
use ast_grep_language::{LanguageExt, SupportLang};

use crate::finding::{Finding, Severity, Source};

/// Maximum size for a single ast-grep YAML rule file. Files exceeding this
/// are skipped with a warning instead of being read into memory. Intended
/// to prevent DoS from oversized files in the user-rules tree
/// (~/.quorum/rules/<lang>/), where the trust boundary is weaker than the
/// bundled rules tree. Largest bundled rule today is ~1.6 KiB; 1 MiB gives
/// 600x headroom for legitimate growth. See issue #120.
const MAX_RULE_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

/// Read a rule file safely: O_NOFOLLOW open (rejects symlinks at the syscall
/// boundary, eliminating TOCTOU between stat-and-read), validate via the
/// opened handle, then bounded read.
///
/// Codex review of #120 flagged the original stat-then-read design as having
/// a TOCTOU window: an attacker with write access to the rule path could
/// swap a validated regular file for a symlink (or oversized file) between
/// `symlink_metadata` and `read_to_string`. By opening with O_NOFOLLOW first
/// and validating from the resulting handle, we bind the metadata check to
/// the same inode we read.
fn read_rule_file(path: &Path) -> std::io::Result<String> {
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        // libc::O_NOFOLLOW: open() returns ELOOP if the final path component
        // is a symlink. Available on all Unix platforms we support.
        //
        // libc::O_NONBLOCK: open() returns immediately on FIFOs and char
        // devices instead of blocking. Without this, a malicious FIFO at
        // ~/.quorum/rules/<lang>/foo.yml would hang load_rules forever
        // waiting for a writer (quorum self-review caught this in-branch).
        // The is_file() check on the opened handle then rejects the FIFO
        // before any read.
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }

    let file = opts.open(path)?;

    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        // FIFO, socket, char/block device, etc. (Symlinks already rejected
        // at open time on Unix; this is the residual non-regular-file case.)
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "rule path is not a regular file",
        ));
    }
    if meta.len() > MAX_RULE_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "rule file size {} exceeds cap {}",
                meta.len(),
                MAX_RULE_FILE_BYTES
            ),
        ));
    }

    // Defense in depth: bound the read to MAX_RULE_FILE_BYTES + 1 even
    // though we just stat-validated. Inode size can lie on some filesystems
    // (proc, sysfs, network FS); this guarantees we never allocate more
    // than the cap regardless.
    let mut yaml = String::new();
    file.take(MAX_RULE_FILE_BYTES + 1).read_to_string(&mut yaml)?;
    Ok(yaml)
}

/// Map file extension to ast-grep SupportLang.
/// JS/JSX/MJS/CJS map to TypeScript (ast-grep uses TS grammar for JS).
/// TSX maps to Tsx (separate grammar in ast-grep).
pub fn ext_to_language(ext: &str) -> Option<SupportLang> {
    match ext {
        "ts" | "js" | "jsx" | "mjs" | "cjs" => Some(SupportLang::TypeScript),
        "tsx" => Some(SupportLang::Tsx),
        "py" => Some(SupportLang::Python),
        "rs" => Some(SupportLang::Rust),
        "yaml" | "yml" => Some(SupportLang::Yaml),
        "sh" | "bash" | "zsh" => Some(SupportLang::Bash),
        "tf" => Some(SupportLang::Hcl),
        _ => None,
    }
}

/// Load ast-grep rules from bundled `rules/<lang>/` and user `~/.quorum/rules/<lang>/` directories.
/// Skips malformed rules with a warning. Returns sorted rule list for deterministic ordering.
pub fn load_rules(
    project_dir: &Path,
    home_dir: &Path,
) -> Vec<RuleConfig<SupportLang>> {
    let mut rules = Vec::new();
    let globals = GlobalRules::default();

    let bundled_dir = project_dir.join("rules");
    let user_dir = home_dir.join(".quorum").join("rules");

    for rules_dir in [&bundled_dir, &user_dir] {
        // #120: top-level rules-root check. symlink_metadata does NOT follow
        // symlinks, unlike is_dir(). Without this, a symlink at the rules
        // root itself (e.g. ~/.quorum/rules -> /etc/) bypasses every other
        // guard. Codex review of the #120 plan flagged this gap.
        let rules_meta = match std::fs::symlink_metadata(rules_dir) {
            Ok(m) => m,
            Err(_) => continue, // not present is fine
        };
        if rules_meta.file_type().is_symlink() {
            tracing::warn!(
                path = %rules_dir.display(),
                "ast-grep: skipping symlinked rules root"
            );
            continue;
        }
        if !rules_meta.file_type().is_dir() {
            continue;
        }
        let Ok(lang_entries) = std::fs::read_dir(rules_dir) else {
            continue;
        };
        for lang_entry in lang_entries.flatten() {
            let lang_dir = lang_entry.path();
            // #120: per-lang-dir symlink check. Same threat model: a symlink
            // at ~/.quorum/rules/python -> /etc/ssh/ would let read_to_string
            // exfiltrate target content if we naively descended.
            let lang_meta = match std::fs::symlink_metadata(&lang_dir) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        path = %lang_dir.display(),
                        error = %e,
                        "ast-grep: failed to stat lang dir; skipping"
                    );
                    continue;
                }
            };
            if lang_meta.file_type().is_symlink() {
                tracing::warn!(
                    path = %lang_dir.display(),
                    "ast-grep: skipping symlinked lang directory"
                );
                continue;
            }
            if !lang_meta.file_type().is_dir() {
                continue;
            }
            let Ok(rule_entries) = std::fs::read_dir(&lang_dir) else {
                continue;
            };
            let mut rule_files: Vec<_> = rule_entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e == "yml" || e == "yaml")
                        .unwrap_or(false)
                })
                .collect();
            rule_files.sort();

            for rule_path in rule_files {
                let yaml = match read_rule_file(&rule_path) {
                    Ok(y) => y,
                    Err(e) => {
                        tracing::warn!(
                            path = %rule_path.display(),
                            error = %e,
                            "ast-grep: skipping rule file"
                        );
                        continue;
                    }
                };
                match from_yaml_string::<SupportLang>(&yaml, &globals) {
                    Ok(parsed) => rules.extend(parsed),
                    Err(e) => {
                        eprintln!("ast-grep: skipping malformed rule {}: {}", rule_path.display(), e);
                    }
                }
            }
        }
    }

    rules.sort_by(|a, b| a.id.cmp(&b.id));
    rules
}

/// Returns the set of ast-grep languages compatible with a file extension.
/// JS/JSX/MJS/CJS are compatible with both JavaScript and TypeScript rules.
fn compatible_languages(ext: &str) -> Vec<SupportLang> {
    match ext {
        "ts" => vec![SupportLang::TypeScript],
        "tsx" => vec![SupportLang::Tsx],
        "js" | "jsx" | "mjs" | "cjs" => vec![SupportLang::JavaScript, SupportLang::TypeScript],
        "py" => vec![SupportLang::Python],
        "rs" => vec![SupportLang::Rust],
        "yaml" | "yml" => vec![SupportLang::Yaml],
        "sh" | "bash" | "zsh" => vec![SupportLang::Bash],
        "tf" => vec![SupportLang::Hcl],
        _ => vec![],
    }
}

/// Scan source code with the given rules. Per-rule isolation: one bad rule doesn't block others.
/// Returns findings with normalized line numbers (1-indexed) and Source::Linter("ast-grep").
pub fn scan_file(
    source: &str,
    ext: &str,
    rules: &[RuleConfig<SupportLang>],
) -> Vec<Finding> {
    if source.is_empty() {
        return Vec::new();
    }

    let langs = compatible_languages(ext);
    if langs.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();

    // Group rules by language, parse once per language
    for lang in &langs {
        let lang_rules: Vec<_> = rules
            .iter()
            .filter(|r| &r.language == lang && !matches!(r.severity, AstSeverity::Off))
            .collect();
        if lang_rules.is_empty() {
            continue;
        }
        let root = lang.ast_grep(source);
        for rule in lang_rules {
            let matches: Vec<_> = root.root().find_all(&rule.matcher).collect();
            for m in matches {
                let start_line = m.start_pos().line() as u32 + 1;
                let end_line = m.end_pos().line() as u32 + 1;
                let message = rule.get_message(&m);
                let severity = match rule.severity {
                    AstSeverity::Error => Severity::High,
                    AstSeverity::Warning => Severity::Medium,
                    AstSeverity::Info | AstSeverity::Hint => Severity::Low,
                    AstSeverity::Off => Severity::Low,
                };

                findings.push(Finding {
                    title: format!("{}: {}", rule.id, message),
                    description: message,
                    severity,
                    category: "ast-pattern".into(),
                    source: Source::Linter("ast-grep".into()),
                    line_start: start_line,
                    line_end: end_line,
                    evidence: vec![m.text().to_string()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                    suggested_fix: None,
                    based_on_excerpt: None,
                });
            }
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Smoke tests: verify crate API works ──

    #[test]
    fn smoke_load_bundled_rule_from_yaml() {
        let yaml = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/as-any-cast.yml"),
        )
        .unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "as-any-cast");
    }

    #[test]
    fn smoke_js_parses_as_typescript() {
        let lang: SupportLang = "typescript".parse().unwrap();
        let root = lang.ast_grep("const x = 1 as any;");
        let node = root.root();
        assert!(node.text().contains("as any"));
    }

    #[test]
    fn smoke_rule_matches_source() {
        let yaml = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/as-any-cast.yml"),
        )
        .unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();
        let lang: SupportLang = "typescript".parse().unwrap();
        let root = lang.ast_grep("const x = 1 as any;");
        let matches: Vec<_> = root.root().find_all(&rules[0].matcher).collect();
        assert!(!matches.is_empty(), "as-any-cast rule should match `1 as any`");
    }

    #[test]
    fn smoke_rule_no_match() {
        let yaml = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/as-any-cast.yml"),
        )
        .unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();
        let lang: SupportLang = "typescript".parse().unwrap();
        let root = lang.ast_grep("const x: number = 1;");
        let matches: Vec<_> = root.root().find_all(&rules[0].matcher).collect();
        assert!(matches.is_empty(), "as-any-cast should NOT match clean code");
    }

    // ── ext_to_language tests (ported from linter.rs) ──

    #[test]
    fn ext_to_language_typescript_variants() {
        assert_eq!(ext_to_language("ts"), Some(SupportLang::TypeScript));
        assert_eq!(ext_to_language("tsx"), Some(SupportLang::Tsx));
        assert_eq!(ext_to_language("js"), Some(SupportLang::TypeScript));
        assert_eq!(ext_to_language("jsx"), Some(SupportLang::TypeScript));
        assert_eq!(ext_to_language("mjs"), Some(SupportLang::TypeScript));
        assert_eq!(ext_to_language("cjs"), Some(SupportLang::TypeScript));
    }

    #[test]
    fn ext_to_language_other_languages() {
        assert_eq!(ext_to_language("py"), Some(SupportLang::Python));
        assert_eq!(ext_to_language("rs"), Some(SupportLang::Rust));
        assert_eq!(ext_to_language("yaml"), Some(SupportLang::Yaml));
        assert_eq!(ext_to_language("yml"), Some(SupportLang::Yaml));
        assert_eq!(ext_to_language("sh"), Some(SupportLang::Bash));
        assert_eq!(ext_to_language("bash"), Some(SupportLang::Bash));
        assert_eq!(ext_to_language("zsh"), Some(SupportLang::Bash));
    }

    #[test]
    fn ext_to_language_hcl() {
        assert_eq!(ext_to_language("tf"), Some(SupportLang::Hcl));
    }

    #[test]
    fn ext_to_language_unsupported() {
        assert_eq!(ext_to_language("go"), None);
        assert_eq!(ext_to_language("c"), None);
        assert_eq!(ext_to_language(""), None);
    }

    // ── load_rules tests ──

    #[test]
    fn load_rules_from_bundled_dir() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(&project_dir, fake_home.path());
        assert!(!rules.is_empty(), "should load bundled rules from rules/");
    }

    #[test]
    fn load_rules_from_user_dir() {
        let empty_project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let user_rules_dir = home.path().join(".quorum").join("rules").join("typescript");
        std::fs::create_dir_all(&user_rules_dir).unwrap();
        let rule_yaml = r#"id: user-test-rule
language: TypeScript
severity: warning
message: test rule
rule:
  pattern: console.log($$$ARGS)
"#;
        std::fs::write(user_rules_dir.join("user-test.yml"), rule_yaml).unwrap();
        let rules = load_rules(empty_project.path(), home.path());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "user-test-rule");
    }

    #[test]
    fn load_rules_additive_bundled_and_user() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let home = tempfile::tempdir().unwrap();
        let user_rules_dir = home.path().join(".quorum").join("rules").join("typescript");
        std::fs::create_dir_all(&user_rules_dir).unwrap();
        let rule_yaml = r#"id: user-extra-rule
language: TypeScript
severity: warning
message: extra rule
rule:
  pattern: console.log($$$ARGS)
"#;
        std::fs::write(user_rules_dir.join("extra.yml"), rule_yaml).unwrap();
        let rules = load_rules(&project_dir, home.path());
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"as-any-cast"), "should include bundled rule");
        assert!(ids.contains(&"user-extra-rule"), "should include user rule");
    }

    #[test]
    fn load_rules_malformed_yaml_skipped() {
        let project = tempfile::tempdir().unwrap();
        let rules_dir = project.path().join("rules").join("typescript");
        std::fs::create_dir_all(&rules_dir).unwrap();
        // Malformed rule (missing required fields)
        std::fs::write(rules_dir.join("bad.yml"), "not: valid: yaml: rule:").unwrap();
        // Valid rule
        let good_yaml = r#"id: good-rule
language: TypeScript
severity: warning
message: good rule
rule:
  pattern: console.log($$$ARGS)
"#;
        std::fs::write(rules_dir.join("good.yml"), good_yaml).unwrap();
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(project.path(), fake_home.path());
        assert_eq!(rules.len(), 1, "should skip bad rule, keep good one");
        assert_eq!(rules[0].id, "good-rule");
    }

    #[test]
    fn load_rules_missing_rules_dir_returns_empty() {
        let empty = tempfile::tempdir().unwrap();
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(empty.path(), fake_home.path());
        assert!(rules.is_empty());
    }

    #[test]
    fn load_rules_deterministic_order() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_home = tempfile::tempdir().unwrap();
        let rules1 = load_rules(&project_dir, fake_home.path());
        let rules2 = load_rules(&project_dir, fake_home.path());
        let ids1: Vec<&str> = rules1.iter().map(|r| r.id.as_str()).collect();
        let ids2: Vec<&str> = rules2.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids1, ids2, "rule ordering should be deterministic");
    }

    // ── scan_file tests ──

    #[test]
    fn scan_file_finds_match() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(&project_dir, fake_home.path());
        let findings = scan_file("const x = 1 as any;", "ts", &rules);
        assert!(!findings.is_empty(), "should find as-any-cast");
        let f = &findings[0];
        assert!(f.title.contains("as-any-cast"));
        assert_eq!(f.source, Source::Linter("ast-grep".into()));
        assert_eq!(f.category, "ast-pattern");
    }

    #[test]
    fn scan_file_severity_mapping() {
        let yaml = r#"id: hint-rule
language: TypeScript
severity: hint
message: just a hint
rule:
  pattern: console.log($$$ARGS)
"#;
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(yaml, &GlobalRules::default()).unwrap();
        let findings = scan_file("console.log('hello');", "ts", &rules);
        assert_eq!(findings[0].severity, Severity::Low);
    }

    #[test]
    fn scan_file_error_severity() {
        let yaml = r#"id: error-rule
language: TypeScript
severity: error
message: critical issue
rule:
  pattern: eval($$$ARGS)
"#;
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(yaml, &GlobalRules::default()).unwrap();
        let findings = scan_file("eval('code');", "ts", &rules);
        assert_eq!(findings[0].severity, Severity::High);
    }

    #[test]
    fn scan_file_line_numbers_one_indexed() {
        let yaml = r#"id: line-test
language: TypeScript
severity: warning
message: found it
rule:
  pattern: eval($$$ARGS)
"#;
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(yaml, &GlobalRules::default()).unwrap();
        let source = "const a = 1;\nconst b = 2;\neval('code');\n";
        let findings = scan_file(source, "ts", &rules);
        assert_eq!(findings[0].line_start, 3, "line numbers should be 1-indexed");
    }

    #[test]
    fn scan_file_unsupported_extension_returns_empty() {
        let rules = vec![];
        let findings = scan_file("some code", "go", &rules);
        assert!(findings.is_empty());
    }

    #[test]
    fn scan_file_empty_source_returns_empty() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(&project_dir, fake_home.path());
        let findings = scan_file("", "ts", &rules);
        assert!(findings.is_empty());
    }

    #[test]
    fn scan_file_finding_has_evidence() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(&project_dir, fake_home.path());
        let findings = scan_file("const x = 1 as any;", "ts", &rules);
        assert!(!findings.is_empty());
        assert!(!findings[0].evidence.is_empty(), "findings should include evidence text");
    }

    #[test]
    fn scan_file_source_is_ast_grep_linter() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(&project_dir, fake_home.path());
        let findings = scan_file("const x = 1 as any;", "ts", &rules);
        for f in &findings {
            assert_eq!(f.source, Source::Linter("ast-grep".into()));
        }
    }

    // ── sync-in-async rule regression test ──

    #[test]
    fn sync_in_async_rule_loads_and_matches() {
        let yaml = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/sync-in-async.yml"),
        )
        .unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default())
                .expect("sync-in-async rule should parse without errors");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "sync-in-async");

        // Should match: readFileSync inside async function
        let source = "async function foo() { fs.readFileSync('x'); }";
        let lang: SupportLang = "typescript".parse().unwrap();
        let root = lang.ast_grep(source);
        let matches: Vec<_> = root.root().find_all(&rules[0].matcher).collect();
        assert!(
            !matches.is_empty(),
            "sync-in-async should match readFileSync inside async function"
        );

        // Should match: writeFileSync inside async arrow function
        let source2 = "const f = async () => { fs.writeFileSync('x', 'y'); };";
        let root2 = lang.ast_grep(source2);
        let matches2: Vec<_> = root2.root().find_all(&rules[0].matcher).collect();
        assert!(
            !matches2.is_empty(),
            "sync-in-async should match writeFileSync inside async arrow function"
        );

        // Should NOT match: readFileSync inside sync function
        let source3 = "function foo() { fs.readFileSync('x'); }";
        let root3 = lang.ast_grep(source3);
        let matches3: Vec<_> = root3.root().find_all(&rules[0].matcher).collect();
        assert!(
            matches3.is_empty(),
            "sync-in-async should NOT match readFileSync in non-async function"
        );
    }

    // ── block-on-in-async (Rust) ──

    #[test]
    fn block_on_in_async_rule_parses_cleanly() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/rust/block-on-in-async.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let parsed: Result<Vec<RuleConfig<SupportLang>>, _> =
            from_yaml_string(&yaml, &GlobalRules::default());
        parsed.expect("block-on-in-async.yml must parse without errors");
    }

    #[test]
    fn block_on_in_async_matches_block_on_inside_async_fn() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/rust/block-on-in-async.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();
        let findings = scan_file(
            "async fn run() { runtime.block_on(async { 1 }); }",
            "rs",
            &rules,
        );
        assert!(!findings.is_empty(), "should flag block_on inside async fn");
    }

    #[test]
    fn block_on_in_async_does_not_match_in_sync_fn() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/rust/block-on-in-async.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();
        let findings = scan_file(
            "fn run() { runtime.block_on(async { 1 }); }",
            "rs",
            &rules,
        );
        assert!(findings.is_empty(), "must NOT flag in sync fn");
    }

    // ── ha-template-none-fallback rule ──

    fn load_yaml_rule(name: &str) -> Vec<RuleConfig<SupportLang>> {
        let path = format!("{}/rules/yaml/{}.yml", env!("CARGO_MANIFEST_DIR"), name);
        let yaml = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("rule file missing: {}: {}", path, e));
        from_yaml_string(&yaml, &GlobalRules::default())
            .unwrap_or_else(|e| panic!("rule {} failed to parse: {}", name, e))
    }

    fn yaml_matches(source: &str, rules: &[RuleConfig<SupportLang>]) -> usize {
        let lang: SupportLang = "yaml".parse().unwrap();
        let root = lang.ast_grep(source);
        root.root().find_all(&rules[0].matcher).count()
    }

    #[test]
    fn ha_template_none_fallback_matches_percent_unit() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.foo_battery') }}%\"\n";
        assert_eq!(yaml_matches(src, &rules), 1, "percent suffix w/o default should match");
    }

    #[test]
    fn ha_template_none_fallback_matches_kelvin_unit() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.foo_temperature') }} K\"\n";
        assert_eq!(yaml_matches(src, &rules), 1, "K suffix w/o default should match");
    }

    #[test]
    fn ha_template_none_fallback_matches_single_quoted() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: '{{ states(\"sensor.foo\") }}%'\n";
        assert_eq!(yaml_matches(src, &rules), 1);
    }

    #[test]
    fn ha_template_none_fallback_negative_has_default() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.foo') | float(0) | default(0) }}%\"\n";
        assert_eq!(yaml_matches(src, &rules), 0, "default() filter must suppress");
    }

    #[test]
    fn ha_template_none_fallback_negative_default_after_other_filter() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.foo') | round(1) | default(0) }}%\"\n";
        assert_eq!(yaml_matches(src, &rules), 0, "default after round should still suppress");
    }

    #[test]
    fn ha_template_none_fallback_negative_no_unit_suffix() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.foo') }}\"\n";
        assert_eq!(yaml_matches(src, &rules), 0, "no unit suffix is not the risk pattern");
    }

    #[test]
    fn ha_template_none_fallback_negative_plain_string() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"Battery at 80%\"\n";
        assert_eq!(yaml_matches(src, &rules), 0);
    }

    #[test]
    fn ha_template_none_fallback_matches_despite_unrelated_default_call() {
        // A filter like `my_default(0)` should NOT suppress the finding --
        // only the real `| default(...)` filter should. Anchor to pipe.
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.foo') | my_default(0) }}%\"\n";
        assert_eq!(yaml_matches(src, &rules), 1, "my_default() is not the Jinja default filter");
    }

    #[test]
    fn ha_template_none_fallback_negative_default_with_spaces() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.foo') |  default (0) }}%\"\n";
        assert_eq!(yaml_matches(src, &rules), 0, "spaced `| default (` is still the default filter");
    }

    #[test]
    fn ha_template_none_fallback_matches_celsius_suffix() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.temp') }} C\"\n";
        assert_eq!(yaml_matches(src, &rules), 1);
    }

    #[test]
    fn ha_template_none_fallback_matches_degree_c() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.temp') }}°C\"\n";
        assert_eq!(yaml_matches(src, &rules), 1, "°C suffix is a common HA unit");
    }

    #[test]
    fn ha_template_none_fallback_matches_watt_unit() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.power') }} W\"\n";
        assert_eq!(yaml_matches(src, &rules), 1);
    }

    #[test]
    fn ha_template_none_fallback_matches_kilowatt_unit() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        let src = "state: \"{{ states('sensor.load') }} kW\"\n";
        assert_eq!(yaml_matches(src, &rules), 1);
    }

    #[test]
    fn ha_template_none_fallback_matches_block_scalar() {
        let rules = load_yaml_rule("ha-template-none-fallback");
        // Multi-line block scalar: common in HA for templates.
        let src = "state: |\n  {{ states('sensor.foo') }}%\n";
        assert_eq!(yaml_matches(src, &rules), 1, "block scalar template must match");
    }

    // ── Parity: all bundled rules match their test fixtures ──

    #[test]
    fn all_bundled_rules_match_fixtures() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let rules_dir = project_dir.join("rules");
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(&project_dir, fake_home.path());

        let mut tested = 0;
        for lang_entry in std::fs::read_dir(&rules_dir).unwrap() {
            let lang_dir = lang_entry.unwrap().path();
            if !lang_dir.is_dir() {
                continue;
            }
            let test_dir = lang_dir.join("tests");
            if !test_dir.is_dir() {
                continue;
            }
            let lang_name = lang_dir.file_name().unwrap().to_str().unwrap();
            let ext = match lang_name {
                "typescript" => "ts",
                "javascript" => "js",
                "python" => "py",
                "rust" => "rs",
                "yaml" => "yaml",
                "bash" => "sh",
                "hcl" => "tf",
                _ => continue,
            };

            for fixture in std::fs::read_dir(&test_dir).unwrap() {
                let fixture_path = fixture.unwrap().path();
                if fixture_path.extension().and_then(|e| e.to_str()) == Some("txt") {
                    continue;
                }
                let source = std::fs::read_to_string(&fixture_path).unwrap();
                let findings = scan_file(&source, ext, &rules);
                assert!(
                    !findings.is_empty(),
                    "bundled rule should match fixture: {}",
                    fixture_path.display()
                );
                tested += 1;
            }
        }
        assert!(tested > 0, "should have tested at least one fixture");
    }

    // ── cors-wildcard-origin (TypeScript/JavaScript) ──
    //
    // Setting Access-Control-Allow-Origin to '*' is almost always wrong on an
    // authenticated API — per CORS spec it's incompatible with credentials, and
    // even without credentials it leaks same-origin trust. Frequent finding on
    // home-assistant-mcp; previously miscategorized as "style" by calibrator.

    #[test]
    fn cors_wildcard_origin_rule_parses() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/cors-wildcard-origin.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let parsed: Result<Vec<RuleConfig<SupportLang>>, _> =
            from_yaml_string(&yaml, &GlobalRules::default());
        parsed.expect("cors-wildcard-origin.yml must parse without errors");
    }

    #[test]
    fn cors_wildcard_origin_flags_wildcard_acao() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/cors-wildcard-origin.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();

        let setheader = "res.setHeader('Access-Control-Allow-Origin', '*');";
        let findings = scan_file(setheader, "ts", &rules);
        assert!(!findings.is_empty(),
            "should flag setHeader('Access-Control-Allow-Origin', '*')");

        let header_fn = "res.header('Access-Control-Allow-Origin', '*');";
        let findings2 = scan_file(header_fn, "ts", &rules);
        assert!(!findings2.is_empty(),
            "should flag res.header('Access-Control-Allow-Origin', '*')");
    }

    #[test]
    fn cors_wildcard_origin_ignores_unrelated() {
        // Dynamic/echoed origin is a different vulnerability (still reviewable by LLM),
        // but this rule targets only the unambiguous literal '*' case.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/cors-wildcard-origin.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();

        let unrelated = "res.setHeader('X-Custom-Flag', '*');";
        let findings = scan_file(unrelated, "ts", &rules);
        assert!(findings.is_empty(),
            "should NOT flag unrelated header with '*' value");
    }

    #[test]
    fn cors_wildcard_origin_matches_case_insensitively() {
        // HTTP header names are case-insensitive. setHeader('access-control-allow-origin', '*')
        // and setHeader('Access-Control-Allow-Origin', '*') are equivalent on the wire.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/cors-wildcard-origin.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();

        for lowered in &[
            "res.setHeader('access-control-allow-origin', '*');",
            "res.setHeader(\"ACCESS-CONTROL-ALLOW-ORIGIN\", \"*\");",
            "res.setHeader('Access-control-allow-Origin', '*');",
        ] {
            let findings = scan_file(lowered, "ts", &rules);
            assert!(!findings.is_empty(),
                "should flag case variant: {}", lowered);
        }
    }

    #[test]
    fn cors_wildcard_origin_covers_writehead_and_next_style() {
        // Node's response.writeHead(code, {headers}) and Next.js-style
        // headers.set(...) are just as common as setHeader/header.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rules/typescript/cors-wildcard-origin.yml");
        let yaml = std::fs::read_to_string(path).unwrap();
        let rules: Vec<RuleConfig<SupportLang>> =
            from_yaml_string(&yaml, &GlobalRules::default()).unwrap();

        let next_style = "response.headers.set('Access-Control-Allow-Origin', '*');";
        let findings = scan_file(next_style, "ts", &rules);
        assert!(!findings.is_empty(),
            "should flag Next.js/Fetch-style headers.set(..., '*')");
    }

    /// Test harness for new rules added in 2026-04 mining push.
    /// Each entry: (fixture path, file ext, rule id, expected match count).
    /// Fixture files embed both positive and negative cases; this asserts the
    /// positive cases fire and the negatives don't.
    #[test]
    fn mining_2026_04_rules_scan_correctly() {
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_home = tempfile::tempdir().unwrap();
        let rules = load_rules(&project_dir, fake_home.path());

        let cases: &[(&str, &str, &str, usize)] = &[
            ("rules/python/tests/bind-all-interfaces.py",            "py",   "bind-all-interfaces", 2),
            ("rules/python/tests/eval-exec-non-literal.py",          "py",   "eval-exec-non-literal", 3),
            ("rules/python/tests/flask-debug-true.py",               "py",   "flask-debug-true", 2),
            ("rules/python/tests/blocking-call-in-async.py",         "py",   "blocking-call-in-async", 3),
            ("rules/python/tests/fastapi-unbounded-pagination.py",   "py",   "fastapi-unbounded-pagination", 2),
            ("rules/typescript/tests/sql-template-injection.ts",     "ts",   "sql-template-injection", 2),
            ("rules/typescript/tests/tls-reject-unauthorized-false.ts","ts", "tls-reject-unauthorized-false", 2),
            ("rules/typescript/tests/eval-non-literal.ts",           "ts",   "eval-non-literal", 3),
            ("rules/typescript/tests/json-parse-as-type.ts",         "ts",   "json-parse-as-type", 1),
            ("rules/typescript/tests/unsafe-url-concat.ts",          "ts",   "unsafe-url-concat", 2),
            ("rules/javascript/tests/bind-all-interfaces.js",        "js",   "bind-all-interfaces", 2),
            ("rules/yaml/tests/ha-jinja-loop-scoped-reassignment.yaml","yaml","ha-jinja-loop-scoped-reassignment", 1),
            ("rules/bash/tests/unsafe-grep-variable.sh",             "sh",   "unsafe-grep-variable", 2),
            ("rules/bash/tests/toctou-lock-touch.sh",                "sh",   "toctou-lock-touch", 1),
            ("rules/rust/tests/ignored-io-result.rs",                "rs",   "ignored-io-result", 2),
            ("rules/rust/tests/silent-error-conversion.rs",          "rs",   "silent-error-conversion", 2),
            ("rules/hcl/tests/iam-wildcard-action.tf",               "tf",   "iam-wildcard-action", 1),
            ("rules/hcl/tests/iam-wildcard-resource.tf",             "tf",   "iam-wildcard-resource", 1),
            // merged rule: bind-in-event-listener replaces bind-in-add-event-listener
            ("rules/javascript/tests/bind-in-event-listener.js",     "js",   "bind-in-event-listener", 2),
        ];

        let mut failures = Vec::new();
        for (path, ext, rule_id, expected) in cases {
            let src_path = project_dir.join(path);
            let source = std::fs::read_to_string(&src_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", src_path.display()));
            let findings = scan_file(&source, ext, &rules);
            let matches = findings.iter().filter(|f| f.title.contains(rule_id)).count();
            if matches != *expected {
                failures.push(format!(
                    "{rule_id} [{path}]: expected {expected} matches, got {matches}"
                ));
            }
        }
        assert!(failures.is_empty(),
            "{} rule(s) failed expected-match assertion:\n  {}",
            failures.len(),
            failures.join("\n  "));
    }

    // ── Issue #120 hardening: user rules trust boundary ──

    #[test]
    fn load_rules_still_loads_bundled_rules_after_120_hardening() {
        // Regression guard: the symlink + size guards added for #120 must
        // NOT break the bundled-rules path. Invoke load_rules against the
        // actual repo's rules/ directory and assert at least one bundled
        // rule loads.
        let project_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let empty_home = tempfile::tempdir().expect("empty home for test");
        let rules = load_rules(project_dir, empty_home.path());
        assert!(
            !rules.is_empty(),
            "bundled rules must still load after #120 hardening"
        );
        let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
        let has_known = ids.iter().any(|id| {
            id.starts_with("md5") || id.starts_with("eval-") || id.starts_with("subprocess")
        });
        assert!(has_known, "expected a known bundled rule id; got {ids:?}");
    }

    #[test]
    #[cfg(unix)]
    fn load_rules_skips_symlinked_lang_directory() {
        // Adversarial: ~/.quorum/rules is a regular directory, but
        // ~/.quorum/rules/<lang> is a symlink to an arbitrary tree.
        // Per-lang-dir symlink_metadata gate must reject.
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let project = tempdir().expect("project tempdir");
        let home = tempdir().expect("home tempdir");

        // Bundled-side control.
        let bundled_lang = project.path().join("rules").join("python");
        std::fs::create_dir_all(&bundled_lang).unwrap();
        std::fs::write(
            bundled_lang.join("safe.yml"),
            "id: safe-rule\nmessage: safe\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
        ).unwrap();

        // Adversarial: lang dir is a symlink.
        let user_rules = home.path().join(".quorum").join("rules");
        std::fs::create_dir_all(&user_rules).unwrap();
        let evil_target = home.path().join("evil_target");
        std::fs::create_dir_all(&evil_target).unwrap();
        std::fs::write(
            evil_target.join("evil.yml"),
            "id: evil-langlink\nmessage: evil\nseverity: warning\nlanguage: python\nrule:\n  pattern: open($X)\n",
        ).unwrap();
        symlink(&evil_target, user_rules.join("python")).expect("symlink");

        let rules = load_rules(project.path(), home.path());
        let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&"safe-rule".to_string()), "bundled rule should still load");
        assert!(
            !ids.contains(&"evil-langlink".to_string()),
            "rule loaded from symlinked lang directory must be rejected; ids={ids:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn load_rules_skips_symlinked_rule_file() {
        // Adversarial: rules tree + lang dir are real, but a single rule
        // file inside is a symlink to content outside the rules tree.
        // O_NOFOLLOW open must reject (raw_os_error == ELOOP).
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let project = tempdir().expect("project tempdir");
        let home = tempdir().expect("home tempdir");

        let user_python = home.path().join(".quorum").join("rules").join("python");
        std::fs::create_dir_all(&user_python).unwrap();

        // Real rule directly in user dir — must load.
        std::fs::write(
            user_python.join("real.yml"),
            "id: real-rule\nmessage: real\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
        ).unwrap();

        // Symlinked rule file pointing at content outside the rules tree.
        let outside = home.path().join("outside.yml");
        std::fs::write(
            &outside,
            "id: smuggled-rule\nmessage: smuggled\nseverity: warning\nlanguage: python\nrule:\n  pattern: eval($X)\n",
        ).unwrap();
        symlink(&outside, user_python.join("smuggled.yml")).expect("symlink");

        let rules = load_rules(project.path(), home.path());
        let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&"real-rule".to_string()), "real rule should load; ids={ids:?}");
        assert!(
            !ids.contains(&"smuggled-rule".to_string()),
            "symlinked rule file must be rejected (O_NOFOLLOW); ids={ids:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn load_rules_skips_non_regular_rule_file() {
        // Adversarial: a Unix socket file at the rule path. open() with
        // O_NOFOLLOW + O_NONBLOCK succeeds (sockets are not symlinks and
        // O_NONBLOCK prevents the open from hanging on FIFOs/devices) —
        // but file.metadata().file_type().is_file() returns false for
        // sockets, so the handle-validate step rejects.
        //
        // This test exercises the same defense surface that protects against
        // FIFO hang-on-open. Unix sockets are convenient to fixture (no
        // mkfifo dep) and trigger the same is_file() == false rejection.
        use std::os::unix::net::UnixListener;
        use tempfile::tempdir;

        let project = tempdir().expect("project tempdir");
        let home = tempdir().expect("home tempdir");

        let user_python = home.path().join(".quorum").join("rules").join("python");
        std::fs::create_dir_all(&user_python).unwrap();

        // Real rule that must load.
        std::fs::write(
            user_python.join("real.yml"),
            "id: real-rule\nmessage: real\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
        ).unwrap();

        // Bind a Unix socket at a .yml path. The listener stays in scope
        // for the test duration so the socket file exists when load_rules
        // walks the directory.
        let socket_path = user_python.join("evil.yml");
        let _listener = UnixListener::bind(&socket_path).expect("bind unix socket");

        // load_rules must complete (no hang on open) AND not load any
        // rule from the socket file.
        let rules = load_rules(project.path(), home.path());
        let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&"real-rule".to_string()), "real rule should still load");
        // No assertion on rule count — the socket has no rule id to check
        // against by name. The PRIMARY assertion is that load_rules
        // RETURNS within the test timeout, demonstrating no FIFO-class
        // hang. cargo test will kill the test on hang.
    }

    #[test]
    #[cfg(unix)]
    fn load_rules_skips_oversized_rule_file() {
        // Adversarial: a 2 MiB YAML file that PARSES (block scalar with
        // x...x padding). If the size cap is removed, this loads as a real
        // rule. With the cap, read_rule_file rejects it before parse and
        // the rule never enters the corpus.
        use tempfile::tempdir;

        let project = tempdir().expect("project tempdir");
        let home = tempdir().expect("home tempdir");

        let user_python = home.path().join(".quorum").join("rules").join("python");
        std::fs::create_dir_all(&user_python).unwrap();

        // Small, well-formed rule that must load.
        std::fs::write(
            user_python.join("small.yml"),
            "id: small-rule\nmessage: small\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
        ).unwrap();

        // 2 MiB padded YAML (block scalar in description so it still parses
        // if the size gate were removed — distinguishes size-skip from
        // parse-skip).
        let prefix = "id: oversized-rule\nmessage: huge\nseverity: warning\nlanguage: python\nrule:\n  pattern: open($X)\nnote: |\n";
        let padding = "x".repeat(2 * 1024 * 1024);
        let oversized = format!("{prefix}  {padding}\n");
        std::fs::write(user_python.join("oversized.yml"), oversized).unwrap();

        let rules = load_rules(project.path(), home.path());
        let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&"small-rule".to_string()), "small rule should load; ids={ids:?}");
        assert!(
            !ids.contains(&"oversized-rule".to_string()),
            "rule file >1 MiB must be skipped; ids={ids:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn load_rules_skips_symlinked_top_level_rules_dir() {
        // Adversarial: ~/.quorum/rules itself is a symlink to /etc/. The
        // top-level rules-root check must reject the whole tree before
        // descending — without it, every subsequent guard is moot.
        // (Codex review of #120 plan flagged this gap.)
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let project = tempdir().expect("project tempdir");
        let home = tempdir().expect("home tempdir");

        // Bundled-side control: a real rule the loader must still find.
        let bundled_lang = project.path().join("rules").join("python");
        std::fs::create_dir_all(&bundled_lang).unwrap();
        std::fs::write(
            bundled_lang.join("safe.yml"),
            "id: safe-rule\nmessage: safe\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
        ).unwrap();

        // Adversarial: ~/.quorum/rules is itself a symlink pointing at a
        // fully populated rules tree elsewhere. The loader must NOT descend.
        let evil_root = home.path().join("evil_rules_root");
        let evil_lang = evil_root.join("python");
        std::fs::create_dir_all(&evil_lang).unwrap();
        std::fs::write(
            evil_lang.join("evil.yml"),
            "id: evil-toplevel\nmessage: evil\nseverity: warning\nlanguage: python\nrule:\n  pattern: open($X)\n",
        ).unwrap();
        let user_quorum = home.path().join(".quorum");
        std::fs::create_dir_all(&user_quorum).unwrap();
        symlink(&evil_root, user_quorum.join("rules")).expect("symlink");

        let rules = load_rules(project.path(), home.path());
        let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&"safe-rule".to_string()),
            "bundled rule should still load; ids={ids:?}");
        assert!(
            !ids.contains(&"evil-toplevel".to_string()),
            "rule loaded via symlinked top-level ~/.quorum/rules must be rejected; ids={ids:?}"
        );
    }
}
