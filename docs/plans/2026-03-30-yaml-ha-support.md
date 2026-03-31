# YAML / Home Assistant Filetype Support

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add YAML file support to quorum so Home Assistant configs (automations, scripts, dashboards, ESPHome, packages) get AST-aware review instead of falling through to LLM-only mode.

**Architecture:** Add `tree-sitter-yaml` (v0.7.2) as a grammar alongside existing languages. YAML gets tree-sitter parsing but uses a different analysis strategy than code languages -- instead of function extraction and cyclomatic complexity, it gets HA-specific structural analysis (duplicate keys, hardcoded secrets in YAML values, Jinja2 template extraction). The existing `review_file` pipeline handles AST+LLM review; we just need to wire YAML through it with YAML-appropriate analyzers.

**Tech Stack:** `tree-sitter-yaml` crate, existing tree-sitter infrastructure, `serde_yaml` (already a dep pattern in Cargo.toml via serde)

**ast-grep note:** ast-grep has built-in YAML support and is excellent for pattern-based linting rules (e.g., "find all automations missing `mode:`"). It's a CLI tool, not a library we'd embed. Future work could shell out to `ast-grep scan` as an external linter (like we do with ruff/clippy/eslint) using YAML rule configs. For now, we use tree-sitter-yaml directly for AST parsing, which is what ast-grep uses internally anyway.

---

## Task 1: Add `tree-sitter-yaml` dependency and `Language::Yaml` variant

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/parser.rs`
- Test: `src/parser.rs` (inline tests)

**Step 1: Write the failing tests**

Add to `src/parser.rs` in the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn detect_language_yaml() {
    assert_eq!(Language::from_extension("yaml"), Some(Language::Yaml));
    assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
    assert_eq!(Language::from_extension("YAML"), Some(Language::Yaml));
}

#[test]
fn detect_language_yaml_from_path() {
    assert_eq!(
        Language::from_path(std::path::Path::new("automations.yaml")),
        Some(Language::Yaml)
    );
    assert_eq!(
        Language::from_path(std::path::Path::new("configuration.yml")),
        Some(Language::Yaml)
    );
}

#[test]
fn parse_valid_yaml() {
    let source = "key: value\nlist:\n  - item1\n  - item2\n";
    let tree = parse(source, Language::Yaml).unwrap();
    assert_eq!(tree.root_node().kind(), "stream");
    assert!(!tree.root_node().has_error());
}

#[test]
fn parse_yaml_automation() {
    let source = "automation:\n  - alias: Turn on lights\n    trigger:\n      - platform: state\n        entity_id: binary_sensor.motion\n    action:\n      - service: light.turn_on\n        target:\n          entity_id: light.living_room\n";
    let tree = parse(source, Language::Yaml).unwrap();
    assert!(!tree.root_node().has_error());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib parser::tests::detect_language_yaml -- --no-capture 2>&1 | head -20`
Expected: compilation error — `Language::Yaml` doesn't exist

**Step 3: Add dependency and implement**

In `Cargo.toml`, add under `[dependencies]` after the tree-sitter lines:

```toml
tree-sitter-yaml = "0.7"
```

In `src/parser.rs`, add `Yaml` to the `Language` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    Yaml,
}
```

In `from_extension`, add:
```rust
"yaml" | "yml" => Some(Language::Yaml),
```

In `tree_sitter_language`, add:
```rust
Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
```

In `function_node_kinds`, add:
```rust
// YAML has no functions — return empty slice
Language::Yaml => &[],
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib parser::tests -- --no-capture`
Expected: all parser tests PASS (including new YAML ones)

**Step 5: Commit**

```bash
git add Cargo.toml src/parser.rs
git commit -m "feat: add tree-sitter-yaml and Language::Yaml variant"
```

---

## Task 2: Wire YAML through analysis.rs (no-op for code analysis, add YAML-specific checks)

**Files:**
- Modify: `src/analysis.rs`
- Test: `src/analysis.rs` (inline tests)

**Step 1: Write the failing tests**

Add to `src/analysis.rs` tests:

```rust
#[test]
fn complexity_yaml_returns_empty() {
    // YAML has no functions, so complexity analysis should return nothing
    let source = "key: value\nlist:\n  - item\n";
    let tree = parse(source, Language::Yaml).unwrap();
    let findings = analyze_complexity(&tree, source, Language::Yaml, 5);
    assert!(findings.is_empty());
}

#[test]
fn insecure_yaml_hardcoded_secret() {
    let source = "api_key: sk-proj-abc123def456ghi\npassword: SuperSecret123!\ntoken: ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
    let tree = parse(source, Language::Yaml).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
    assert!(
        findings.iter().any(|f| f.title.contains("secret") || f.title.contains("Secret")),
        "Should flag hardcoded secrets in YAML. Got: {:?}",
        findings.iter().map(|f| &f.title).collect::<Vec<_>>()
    );
}

#[test]
fn insecure_yaml_safe_reference() {
    // env vars and !secret references should NOT be flagged
    let source = "api_key: !secret api_key\npassword: !env_var PASSWORD\ntoken: !include secrets.yaml\n";
    let tree = parse(source, Language::Yaml).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
    assert!(
        !findings.iter().any(|f| f.title.contains("secret") || f.title.contains("Secret")),
        "!secret and !env_var references should NOT be flagged. Got: {:?}",
        findings.iter().map(|f| &f.title).collect::<Vec<_>>()
    );
}

#[test]
fn insecure_yaml_duplicate_keys() {
    let source = "automation:\n  alias: First\nautomation:\n  alias: Second\n";
    let tree = parse(source, Language::Yaml).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
    assert!(
        findings.iter().any(|f| f.title.contains("Duplicate") || f.title.contains("duplicate")),
        "Should flag duplicate top-level keys. Got: {:?}",
        findings.iter().map(|f| &f.title).collect::<Vec<_>>()
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib analysis::tests::complexity_yaml -- --no-capture 2>&1 | head -20`
Expected: compilation error or match arm missing for `Language::Yaml`

**Step 3: Implement YAML analysis**

In `analyze_complexity` — add `Language::Yaml` to the `func_kinds` match:
```rust
Language::Yaml => &[][..],
```

In `scan_insecure_nodes` — add YAML dispatch:
```rust
Language::Yaml => scan_insecure_yaml(node, source, findings),
```

Add `scan_insecure_yaml` function:
```rust
fn scan_insecure_yaml(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    let line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    // Detect hardcoded secrets in YAML key-value pairs
    // tree-sitter-yaml: block_mapping_pair has key + value children
    if node.kind() == "block_mapping_pair" {
        if let (Some(key_node), Some(val_node)) = (
            node.child_by_field_name("key"),
            node.child_by_field_name("value"),
        ) {
            let key_text = source[key_node.byte_range()].to_lowercase();
            let secret_keys = [
                "password", "passwd", "secret", "api_key", "apikey",
                "token", "auth_token", "private_key", "secret_key",
                "access_key", "client_secret",
            ];
            if secret_keys.iter().any(|s| key_text.contains(s)) {
                let val_text = source[val_node.byte_range()].trim();
                // Skip HA !secret, !env_var, !include references and empty values
                if !val_text.is_empty()
                    && !val_text.starts_with("!secret")
                    && !val_text.starts_with("!env_var")
                    && !val_text.starts_with("!include")
                    && val_text.len() > 8
                {
                    // Check it looks like a real secret (mixed chars, not a placeholder)
                    let has_upper = val_text.chars().any(|c| c.is_ascii_uppercase());
                    let has_digit = val_text.chars().any(|c| c.is_ascii_digit());
                    let has_special = val_text.chars().any(|c| matches!(c, '-' | '/' | '+' | '=' | '_'));
                    let looks_like_secret = (has_upper || has_digit || has_special)
                        && val_text.len() > 8;
                    if looks_like_secret {
                        findings.push(Finding {
                            title: format!("Hardcoded secret in YAML key `{}`", &source[key_node.byte_range()]),
                            description: "Secrets should use !secret references or environment variables, not hardcoded values.".into(),
                            severity: Severity::High,
                            category: "security".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![format!("{}: [REDACTED]", &source[key_node.byte_range()])],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }
        }
    }

    // Detect duplicate keys at the same mapping level
    if node.kind() == "block_mapping" {
        let mut seen_keys: Vec<(String, u32)> = Vec::new();
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "block_mapping_pair" {
                    if let Some(key_node) = child.child_by_field_name("key") {
                        let key_text = source[key_node.byte_range()].to_string();
                        let key_line = key_node.start_position().row as u32 + 1;
                        if let Some((_, first_line)) = seen_keys.iter().find(|(k, _)| k == &key_text) {
                            findings.push(Finding {
                                title: format!("Duplicate YAML key `{}`", key_text),
                                description: format!(
                                    "Key `{}` appears at line {} and line {}. The second value silently overwrites the first.",
                                    key_text, first_line, key_line
                                ),
                                severity: Severity::High,
                                category: "bug".into(),
                                source: Source::LocalAst,
                                line_start: key_line,
                                line_end: key_line,
                                evidence: vec![format!("first at line {}", first_line)],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        } else {
                            seen_keys.push((key_text, key_line));
                        }
                    }
                }
            }
        }
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib analysis::tests -- --no-capture`
Expected: all analysis tests PASS

**Step 5: Commit**

```bash
git add src/analysis.rs
git commit -m "feat: add YAML-specific AST analysis (secrets, duplicate keys)"
```

---

## Task 3: Wire YAML through hydration.rs

**Files:**
- Modify: `src/hydration.rs`
- Test: `src/hydration.rs` (inline tests)

**Step 1: Write the failing test**

```rust
#[test]
fn hydrate_yaml_returns_empty_context() {
    // YAML doesn't have functions/types/imports like code languages
    let source = "key: value\nlist:\n  - item\n";
    let tree = parse(source, Language::Yaml).unwrap();
    let ctx = hydrate(&tree, source, Language::Yaml, &[(1, 3)]);
    // Should not crash; context will be empty since YAML has no functions
    assert!(ctx.callee_signatures.is_empty());
    assert!(ctx.type_definitions.is_empty());
    assert!(ctx.callers.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib hydration::tests::hydrate_yaml -- --no-capture 2>&1 | head -20`
Expected: match arm missing for `Language::Yaml`

**Step 3: Implement — add Yaml arms to all match statements**

In `function_def_kinds`:
```rust
Language::Yaml => vec![],
```

In `type_def_kinds`:
```rust
Language::Yaml => vec![],
```

In `call_expr_kinds`:
```rust
Language::Yaml => vec![],
```

In `import_kinds`:
```rust
Language::Yaml => vec![],
```

**Step 4: Run tests**

Run: `cargo test --lib hydration::tests -- --no-capture`
Expected: PASS

**Step 5: Commit**

```bash
git add src/hydration.rs
git commit -m "feat: wire Language::Yaml through hydration (empty context)"
```

---

## Task 4: Wire YAML through pipeline.rs and lang_name

**Files:**
- Modify: `src/pipeline.rs`
- Test: `src/pipeline.rs` (inline tests)

**Step 1: Write the failing test**

```rust
#[test]
fn pipeline_yaml_local_analysis() {
    let source = "api_key: sk-proj-abc123def456ghi\nname: test\n";
    let tree = parser::parse(source, Language::Yaml).unwrap();
    let config = PipelineConfig::default();
    let result = review_file(
        Path::new("config.yaml"), source, Language::Yaml,
        &tree, None, &config,
    ).unwrap();
    assert!(
        result.findings.iter().any(|f| f.category == "security"),
        "YAML with hardcoded secret should produce security finding. Got: {:?}",
        result.findings.iter().map(|f| &f.title).collect::<Vec<_>>()
    );
}

#[test]
fn pipeline_yaml_clean_file() {
    let source = "automation:\n  - alias: Test\n    trigger:\n      - platform: state\n        entity_id: binary_sensor.motion\n    action:\n      - service: light.turn_on\n";
    let tree = parser::parse(source, Language::Yaml).unwrap();
    let config = PipelineConfig::default();
    let result = review_file(
        Path::new("automations.yaml"), source, Language::Yaml,
        &tree, None, &config,
    ).unwrap();
    assert!(result.findings.is_empty(), "Clean YAML should have no findings");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib pipeline::tests::pipeline_yaml -- --no-capture 2>&1 | head -20`
Expected: match arm missing in `lang_name`

**Step 3: Implement**

In `lang_name`:
```rust
Language::Yaml => "yaml",
```

**Step 4: Run tests**

Run: `cargo test --lib pipeline::tests -- --no-capture`
Expected: PASS

**Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat: wire YAML through review pipeline"
```

---

## Task 5: Update CLI dispatch — YAML files now get full pipeline

**Files:**
- Modify: `src/main.rs` (the `run_review` function dispatch)

**Step 1: Verify current behavior**

Currently `Language::from_path("foo.yaml")` returns `None`, so YAML files go to `review_file_llm_only`. After Task 1, it returns `Some(Language::Yaml)`, so they'll automatically route to `review_source` (full pipeline). No code change needed in main.rs dispatch logic.

**Step 2: Verify the dispatch works**

Run: `cargo test -- --no-capture 2>&1 | tail -5`
Expected: all tests pass

**Step 3: Manual smoke test**

Create a test YAML file:
```bash
echo 'api_key: sk-proj-abc123def456ghi
name: test
password: SuperSecret123!' > /tmp/test-quorum.yaml
```

Run: `cargo run -- review /tmp/test-quorum.yaml 2>&1`
Expected: findings about hardcoded secrets (local AST), no "LLM-only" note

**Step 4: Commit (if any changes needed)**

```bash
git add -A
git commit -m "feat: YAML files now route through full AST pipeline"
```

---

## Task 6: Add HA-specific linter detection (yamllint)

**Files:**
- Modify: `src/linter.rs`
- Test: `src/linter.rs` (inline tests)

**Step 1: Write the failing tests**

```rust
#[test]
fn detect_yamllint_from_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".yamllint"), "extends: default\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Yamllint));
}

#[test]
fn detect_yamllint_from_yaml_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".yamllint.yaml"), "extends: default\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Yamllint));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib linter::tests::detect_yamllint -- --no-capture 2>&1 | head -20`
Expected: `LinterKind::Yamllint` doesn't exist

**Step 3: Implement**

Add `Yamllint` to `LinterKind` enum:
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinterKind {
    Ruff,
    Clippy,
    Eslint,
    Yamllint,
}
```

Add name:
```rust
LinterKind::Yamllint => "yamllint",
```

Add detection in `detect_linters`:
```rust
// Yamllint: .yamllint, .yamllint.yaml, .yamllint.yml
let yamllint_configs = [".yamllint", ".yamllint.yaml", ".yamllint.yml"];
for config in &yamllint_configs {
    if project_dir.join(config).exists() {
        linters.push(LinterKind::Yamllint);
        break;
    }
}
```

Add `run_linter` arm:
```rust
LinterKind::Yamllint => runner.run("yamllint", &["-f", "parsable", &file_str], cwd)?,
```

Add normalizer:
```rust
LinterKind::Yamllint => normalize_yamllint_output(&output.stdout),
```

Add `normalize_yamllint_output`:
```rust
pub fn normalize_yamllint_output(output: &str) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    // yamllint parsable format: file:line:col: [level] message (rule)
    for line in output.lines() {
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        if parts.len() < 4 {
            continue;
        }
        let line_num = parts[1].trim().parse::<u32>().unwrap_or(1);
        let rest = parts[3].trim();

        let (severity, message) = if rest.starts_with("[error]") {
            (Severity::High, rest.trim_start_matches("[error]").trim())
        } else if rest.starts_with("[warning]") {
            (Severity::Medium, rest.trim_start_matches("[warning]").trim())
        } else {
            (Severity::Low, rest)
        };

        findings.push(Finding {
            title: format!("yamllint: {}", message),
            description: message.to_string(),
            severity,
            category: "lint".into(),
            source: Source::Linter("yamllint".into()),
            line_start: line_num,
            line_end: line_num,
            evidence: vec!["yamllint".into()],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }
    Ok(findings)
}
```

**Step 4: Run tests**

Run: `cargo test --lib linter::tests -- --no-capture`
Expected: PASS

**Step 5: Commit**

```bash
git add src/linter.rs
git commit -m "feat: add yamllint detection and output normalization"
```

---

## Task 7: Add domain detection for Home Assistant projects

**Files:**
- Modify: `src/domain.rs`
- Test: `src/domain.rs` (inline tests)

**Step 1: Check current domain detection**

Read `src/domain.rs` to see current framework detection logic. Add HA detection if not present.

**Step 2: Write the failing test**

```rust
#[test]
fn detect_ha_domain() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("configuration.yaml"), "homeassistant:\n  name: Home\n").unwrap();
    let domain = detect_domain(dir.path());
    assert!(
        domain.frameworks.iter().any(|f| f.contains("home-assistant") || f.contains("homeassistant")),
        "Should detect Home Assistant. Got: {:?}",
        domain.frameworks
    );
}
```

**Step 3: Implement HA detection**

Add detection logic that looks for `configuration.yaml` containing `homeassistant:` key, or `.HA_VERSION` file, or `esphome:` key patterns.

**Step 4: Run tests**

Run: `cargo test --lib domain::tests -- --no-capture`
Expected: PASS

**Step 5: Commit**

```bash
git add src/domain.rs
git commit -m "feat: detect Home Assistant projects for Context7 enrichment"
```

---

## Task 8: Fix any remaining exhaustive match compilation errors

**Files:**
- Potentially: any file with `match lang { ... }` that doesn't handle `Language::Yaml`

**Step 1: Compile and find errors**

Run: `cargo build 2>&1`

Any remaining `non-exhaustive patterns` errors will tell you exactly which files/lines need a `Language::Yaml` arm.

**Step 2: Fix each one**

For each error, add the appropriate `Language::Yaml => ...` arm. Most will be empty/no-op since YAML doesn't have functions, types, etc.

**Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -10`
Expected: all 358+ tests pass, plus new YAML tests

**Step 4: Commit**

```bash
git add -A
git commit -m "fix: handle Language::Yaml in all exhaustive matches"
```

---

## Task 9: Update MEMORY.md and ARCHITECTURE.md

**Files:**
- Modify: `docs/ARCHITECTURE.md` — add YAML to supported languages table
- Modify: memory file if needed

**Step 1: Update architecture docs**

Add YAML/Home Assistant to the language support section of ARCHITECTURE.md.

**Step 2: Commit**

```bash
git add docs/ARCHITECTURE.md
git commit -m "docs: add YAML/HA support to architecture docs"
```

---

## Future Work (not in this plan)

- **ast-grep as external linter**: Add `LinterKind::AstGrep` that shells out to `sg scan` with YAML rules for HA-specific patterns (missing `mode:` on automations, deprecated entity formats, etc.)
- **Jinja2 template extraction**: Extract `{% %}` / `{{ }}` blocks from YAML values and analyze them separately
- **HA schema validation**: Validate automation/script/dashboard YAML structure against known schemas
- **ESPHome lambda extraction**: Pull C++ lambdas from ESPHome YAML for tree-sitter-cpp analysis
