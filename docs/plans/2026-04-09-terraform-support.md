# Terraform/HCL Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add Terraform (.tf, .tfvars) as the 8th supported language with tree-sitter-hcl parsing, tflint integration, and AST-based security pattern detection.

**Architecture:** Add `Language::Terraform` variant, integrate `tree-sitter-hcl` crate for parsing, add `LinterKind::Tflint` with JSON output normalization, implement `scan_insecure_terraform()` for hardcoded secrets / wildcard IAM / open security groups / unencrypted resources, and `analyze_terraform_structure()` for missing version constraints and backend config. The LLM reviewer already works for any parsed language -- no changes needed there.

**Tech Stack:** tree-sitter-hcl 1.1.0, tflint (JSON output), existing Finding/Source types

---

### Task 1: Add tree-sitter-hcl dependency

**Files:**
- Modify: `Cargo.toml:66` (after tree-sitter-bash line)

**Step 1: Add the dependency**

Add after the `tree-sitter-bash` line in Cargo.toml:

```toml
tree-sitter-hcl = "1.1"
```

**Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles successfully (tree-sitter-hcl 1.1.0 is compatible with tree-sitter 0.25)

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add tree-sitter-hcl 1.1 for Terraform support"
```

---

### Task 2: Register Terraform language in parser.rs

**Files:**
- Modify: `src/parser.rs:4-12` (Language enum)
- Modify: `src/parser.rs:15-26` (from_extension)
- Modify: `src/parser.rs:28-38` (from_path)
- Modify: `src/parser.rs:40-58` (tree_sitter_language)
- Modify: `src/parser.rs:60-72` (function_node_kinds)
- Test: `src/parser.rs` (inline tests module)

**Step 1: Write failing tests**

Add to the `mod tests` block in `src/parser.rs`:

```rust
// -- Terraform support --

#[test]
fn detect_language_terraform() {
    assert_eq!(Language::from_extension("tf"), Some(Language::Terraform));
    assert_eq!(Language::from_extension("tfvars"), Some(Language::Terraform));
}

#[test]
fn detect_language_terraform_from_path() {
    assert_eq!(Language::from_path(std::path::Path::new("main.tf")), Some(Language::Terraform));
    assert_eq!(Language::from_path(std::path::Path::new("terraform.tfvars")), Some(Language::Terraform));
}

#[test]
fn parse_valid_terraform() {
    let source = "resource \"aws_instance\" \"web\" {\n  ami           = \"ami-123456\"\n  instance_type = \"t3.micro\"\n}\n";
    let tree = parse(source, Language::Terraform).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn terraform_no_functions() {
    let source = "variable \"name\" {\n  type = string\n}\n";
    let tree = parse(source, Language::Terraform).unwrap();
    let fns = extract_functions(&tree, source, Language::Terraform);
    assert!(fns.is_empty());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum parser::tests::detect_language_terraform 2>&1 | tail -5`
Expected: FAIL -- `Language::Terraform` does not exist

**Step 3: Implement Language::Terraform**

In the `Language` enum (line 4), add variant:
```rust
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    Yaml,
    Bash,
    Dockerfile,
    Terraform,
}
```

In `from_extension` (line 16), add match arm:
```rust
"tf" | "tfvars" => Some(Language::Terraform),
```

In `tree_sitter_language` (line 40), add match arm:
```rust
Language::Terraform => tree_sitter_hcl::LANGUAGE.into(),
```

In `function_node_kinds` (line 60), add match arm:
```rust
Language::Terraform => &[],
```

**Step 4: Handle exhaustive match compilation errors**

The compiler will flag every `match lang` that doesn't handle `Terraform`. Fix each one -- these are all in analysis.rs, hydration.rs, and pipeline.rs. For each, add the `Terraform` arm following the same pattern as `Dockerfile` (empty arrays for function/type/call/import kinds, include in the lang_name match).

Key locations:
- `src/analysis.rs:13-20` (analyze_complexity func_kinds): add `Language::Terraform => &[][..],`
- `src/analysis.rs:162-175` (scan_insecure_nodes): add `Language::Terraform => scan_insecure_terraform(node, source, findings),` (stub it as empty for now)
- `src/hydration.rs:73-81` (function_def_kinds): add `Language::Terraform => vec![],`
- `src/hydration.rs:84-92` (type_def_kinds): add `Language::Terraform => vec![],`
- `src/hydration.rs:95-103` (call_expr_kinds): add `Language::Terraform => vec![],`
- `src/hydration.rs:106-114` (import_kinds): add `Language::Terraform => vec![],`
- `src/pipeline.rs:332-341` (lang_name): add `Language::Terraform => "terraform",`

Add a stub scanner in `src/analysis.rs` (before the test module):
```rust
fn scan_insecure_terraform(
    _node: &tree_sitter::Node,
    _source: &str,
    _findings: &mut Vec<Finding>,
) {
    // Patterns implemented in Task 5
}
```

**Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum parser::tests 2>&1 | tail -10`
Expected: All parser tests pass including the 4 new Terraform tests

**Step 6: Commit**

```bash
git add src/parser.rs src/analysis.rs src/hydration.rs src/pipeline.rs
git commit -m "feat: register Terraform language with tree-sitter-hcl parsing"
```

---

### Task 3: Add tflint linter integration

**Files:**
- Modify: `src/linter.rs:16-24` (LinterKind enum)
- Modify: `src/linter.rs:26-38` (name())
- Modify: `src/linter.rs:40-110` (detect_linters)
- Modify: `src/linter.rs:149-192` (run_linter + normalize dispatch)
- Modify: `src/linter.rs:461-473` (ext_to_ast_grep_lang)
- Test: `src/linter.rs` (inline tests module)

**Step 1: Write failing tests**

Add to the `mod tests` block in `src/linter.rs`:

```rust
// -- Tflint detection --

#[test]
fn detect_tflint_from_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".tflint.hcl"), "plugin \"terraform\" {\n  enabled = true\n}\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Tflint));
}

#[test]
fn detect_tflint_from_tf_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.tf"), "resource \"aws_instance\" \"web\" {}\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Tflint));
}

// -- Tflint output normalization --

#[test]
fn normalize_tflint_valid_output() {
    let json = r#"{"issues":[{"rule":{"name":"aws_instance_invalid_type","severity":"error","link":"https://github.com/terraform-linters/tflint-ruleset-aws/blob/v0.29.0/docs/rules/aws_instance_invalid_type.md"},"message":"\"t2.nano\" is an invalid instance type.","range":{"filename":"main.tf","start":{"line":3,"column":17},"end":{"line":3,"column":29}},"callers":[]}],"errors":[]}"#;
    let findings = normalize_tflint_output(json).unwrap();
    assert_eq!(findings.len(), 1);
    assert!(findings[0].title.contains("aws_instance_invalid_type"));
    assert_eq!(findings[0].line_start, 3);
    assert_eq!(findings[0].line_end, 3);
    assert_eq!(findings[0].severity, Severity::High);
    assert_eq!(findings[0].source, Source::Linter("tflint".into()));
}

#[test]
fn normalize_tflint_warning_severity() {
    let json = r#"{"issues":[{"rule":{"name":"terraform_deprecated_interpolation","severity":"warning","link":""},"message":"Interpolation-only expressions are deprecated.","range":{"filename":"main.tf","start":{"line":5,"column":10},"end":{"line":5,"column":30}},"callers":[]}],"errors":[]}"#;
    let findings = normalize_tflint_output(json).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Medium);
}

#[test]
fn normalize_tflint_notice_severity() {
    let json = r#"{"issues":[{"rule":{"name":"terraform_naming_convention","severity":"notice","link":""},"message":"resource name should be snake_case","range":{"filename":"main.tf","start":{"line":1,"column":1},"end":{"line":1,"column":20}},"callers":[]}],"errors":[]}"#;
    let findings = normalize_tflint_output(json).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Low);
}

#[test]
fn normalize_tflint_empty_issues() {
    let json = r#"{"issues":[],"errors":[]}"#;
    let findings = normalize_tflint_output(json).unwrap();
    assert!(findings.is_empty());
}

#[test]
fn normalize_tflint_malformed_json() {
    assert!(normalize_tflint_output("not json").is_err());
}

#[test]
fn run_tflint_via_runner() {
    let json = r#"{"issues":[{"rule":{"name":"test_rule","severity":"warning","link":""},"message":"test message","range":{"filename":"main.tf","start":{"line":1,"column":1},"end":{"line":1,"column":10}},"callers":[]}],"errors":[]}"#;
    let runner = FakeCommandRunner::with_exit_code(json, 2);  // tflint exits 2 when issues found
    let file = PathBuf::from("main.tf");
    let cwd = PathBuf::from(".");
    let findings = run_linter(&LinterKind::Tflint, &file, &cwd, &runner).unwrap();
    assert_eq!(findings.len(), 1);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum linter::tests::detect_tflint 2>&1 | tail -5`
Expected: FAIL -- `LinterKind::Tflint` does not exist

**Step 3: Implement tflint integration**

Add `Tflint` to `LinterKind` enum:
```rust
pub enum LinterKind {
    Ruff,
    Clippy,
    Eslint,
    Yamllint,
    Shellcheck,
    Hadolint,
    AstGrep,
    Tflint,
}
```

Add to `name()`:
```rust
LinterKind::Tflint => "tflint",
```

Add detection in `detect_linters()` (after the hadolint block, before ast-grep):
```rust
// Tflint: .tflint.hcl config or .tf files in project root
let has_tflint_config = project_dir.join(".tflint.hcl").exists();
let has_tf_files = std::fs::read_dir(project_dir)
    .ok()
    .map(|entries| {
        entries.flatten().any(|e| {
            e.path().extension().and_then(|ext| ext.to_str()) == Some("tf")
        })
    })
    .unwrap_or(false);
if has_tflint_config || has_tf_files {
    linters.push(LinterKind::Tflint);
}
```

Add to `run_linter()` match (the runner invocation):
```rust
LinterKind::Tflint => runner.run("tflint", &["--format=json", "--force", &file_str], cwd)?,
```

Note: tflint exits 0 (no issues), 2 (issues found), or 3 (error). We need to adjust the exit code check. The current logic bails on exit >= 2 with empty stdout, which works -- tflint with issues outputs JSON to stdout. But we need to also accept exit code 2 with non-empty stdout as normal. The existing logic already handles this: `if output.exit_code >= 2 && output.stdout.trim().is_empty()` -- so exit 2 with JSON stdout will pass through.

Add to the normalize dispatch match:
```rust
LinterKind::Tflint => normalize_tflint_output(&output.stdout),
```

Implement the normalizer:
```rust
pub fn normalize_tflint_output(json_output: &str) -> anyhow::Result<Vec<Finding>> {
    let wrapper: serde_json::Value = serde_json::from_str(json_output)?;
    let issues = wrapper.get("issues").and_then(|i| i.as_array());
    let mut findings = Vec::new();

    if let Some(items) = issues {
        for item in items {
            let rule_name = item["rule"]["name"].as_str().unwrap_or("unknown");
            let severity_str = item["rule"]["severity"].as_str().unwrap_or("warning");
            let message = item["message"].as_str().unwrap_or("");
            let line_start = item["range"]["start"]["line"].as_u64().unwrap_or(1) as u32;
            let line_end = item["range"]["end"]["line"].as_u64().unwrap_or(line_start as u64) as u32;

            let severity = match severity_str {
                "error" => Severity::High,
                "warning" => Severity::Medium,
                "notice" => Severity::Low,
                _ => Severity::Low,
            };

            findings.push(Finding {
                title: format!("{}: {}", rule_name, message),
                description: message.to_string(),
                severity,
                category: "lint".into(),
                source: Source::Linter("tflint".into()),
                line_start,
                line_end,
                evidence: vec![format!("tflint {}", rule_name)],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
            });
        }
    }

    Ok(findings)
}
```

Add `"tf"` to `ext_to_ast_grep_lang`:
```rust
"tf" => Some("hcl"),
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum linter::tests 2>&1 | tail -10`
Expected: All linter tests pass including the 7 new tflint tests

**Step 5: Commit**

```bash
git add src/linter.rs
git commit -m "feat: add tflint linter integration with JSON output normalization"
```

---

### Task 4: Update CLAUDE.md and docs

**Files:**
- Modify: `CLAUDE.md` (supported languages table)

**Step 1: Update the supported languages table**

Add Terraform row to the table in CLAUDE.md:

```
| Terraform | .tf, .tfvars | secrets, wildcard IAM, open SGs, unencrypted resources, missing version pins | tflint |
```

Update the language count references (change "7 languages" to "8 languages" where applicable).

**Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: add Terraform to supported languages table"
```

---

### Task 5: Implement AST security patterns

**Files:**
- Modify: `src/analysis.rs` (scan_insecure_terraform + analyze_terraform_structure)
- Test: `src/analysis.rs` (inline tests module)

**Step 1: Write failing tests for scan_insecure_terraform**

Add to the `mod tests` block in `src/analysis.rs`. Use the existing test patterns as a guide (e.g., `scan_insecure_dockerfile` tests):

```rust
// -- Terraform security patterns --

#[test]
fn terraform_hardcoded_secret_in_variable_default() {
    let source = r#"variable "db_password" {
  type    = string
  default = "supersecret123"
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(findings.iter().any(|f| f.category == "security" && f.title.contains("secret")),
        "Should detect hardcoded secret in variable default: {:?}", findings);
}

#[test]
fn terraform_secret_variable_no_default_is_ok() {
    let source = r#"variable "db_password" {
  type      = string
  sensitive = true
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(!findings.iter().any(|f| f.title.contains("secret")),
        "Variable without default should not flag: {:?}", findings);
}

#[test]
fn terraform_wildcard_iam_action() {
    let source = r#"resource "aws_iam_policy" "admin" {
  name   = "admin-policy"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = "*"
      Resource = "*"
    }]
  })
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(findings.iter().any(|f| f.category == "security" && f.title.contains("wildcard")),
        "Should detect wildcard IAM action: {:?}", findings);
}

#[test]
fn terraform_open_security_group_ingress() {
    let source = r#"resource "aws_security_group" "open" {
  name = "open-sg"

  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(findings.iter().any(|f| f.category == "security" && f.title.contains("0.0.0.0/0")),
        "Should detect open security group: {:?}", findings);
}

#[test]
fn terraform_open_sg_on_443_is_ok() {
    let source = r#"resource "aws_security_group" "web" {
  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(!findings.iter().any(|f| f.title.contains("0.0.0.0/0")),
        "Port 443 open to public is normal for web servers: {:?}", findings);
}

#[test]
fn terraform_hardcoded_secret_in_resource() {
    let source = r#"resource "aws_db_instance" "db" {
  engine   = "postgres"
  password = "hunter2"
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(findings.iter().any(|f| f.category == "security" && f.title.contains("secret")),
        "Should detect hardcoded password in resource: {:?}", findings);
}

#[test]
fn terraform_password_from_variable_is_ok() {
    let source = r#"resource "aws_db_instance" "db" {
  engine   = "postgres"
  password = var.db_password
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(!findings.iter().any(|f| f.title.contains("secret")),
        "Password from variable ref should not flag: {:?}", findings);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum analysis::tests::terraform 2>&1 | tail -10`
Expected: FAIL -- the stub scanner produces no findings

**Step 3: Implement scan_insecure_terraform**

Replace the stub with:

```rust
fn scan_insecure_terraform(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    let kind = node.kind();
    let text = &source[node.byte_range()];
    let line_start = node.start_position().row as u32 + 1;
    let line_end = node.end_position().row as u32 + 1;

    // T1: Hardcoded secrets in attribute values
    // Matches: password = "literal", api_key = "literal", etc.
    // The HCL AST has "attribute" nodes with "identifier" and expression children.
    if kind == "attribute" {
        if let Some(name_node) = node.child(0) {
            let attr_name = &source[name_node.byte_range()];
            let upper = attr_name.to_uppercase();
            let secret_keys = [
                "PASSWORD", "API_KEY", "SECRET", "TOKEN", "PRIVATE_KEY",
                "ACCESS_KEY", "CREDENTIAL", "AUTH_KEY", "SECRET_KEY",
            ];
            if secret_keys.iter().any(|k| upper.contains(k)) {
                // Check if the value is a string literal (not a variable reference)
                if let Some(val_node) = node.child(2) {
                    let val_kind = val_node.kind();
                    // template_literal or string_lit in tree-sitter-hcl
                    if val_kind == "template_literal"
                        || val_kind == "string_lit"
                        || val_kind == "quoted_template"
                    {
                        let val_text = &source[val_node.byte_range()];
                        // Skip empty strings and variable interpolations
                        let inner = val_text.trim_matches('"');
                        if !inner.is_empty()
                            && !inner.starts_with("${var.")
                            && !inner.starts_with("${data.")
                        {
                            findings.push(Finding {
                                title: format!(
                                    "Hardcoded secret in `{}` -- use a variable or secrets manager",
                                    attr_name
                                ),
                                description: format!(
                                    "The attribute `{}` appears to contain a hardcoded secret. \
                                     Use `sensitive = true` variables or a secrets manager instead.",
                                    attr_name
                                ),
                                severity: Severity::High,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start,
                                line_end,
                                evidence: vec![],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // T2: Detect "default" attribute in variable blocks with secret-like names
    // This catches: variable "db_password" { default = "literal" }
    if kind == "block" {
        let block_text = text;
        // Check if this is a variable block with a secret-like name
        if block_text.starts_with("variable") {
            let upper = block_text.to_uppercase();
            let secret_names = [
                "PASSWORD", "API_KEY", "SECRET", "TOKEN", "PRIVATE_KEY",
                "ACCESS_KEY", "CREDENTIAL",
            ];
            if secret_names.iter().any(|k| upper.contains(k)) {
                // Look for a default attribute with a string literal value
                if let Some(default_match) = find_default_with_literal(node, source) {
                    findings.push(Finding {
                        title: "Hardcoded secret in variable default -- use sensitive input".into(),
                        description: "Variable with a secret-like name has a hardcoded default value. \
                                      Remove the default and pass the value at runtime, or use a secrets manager.".into(),
                        severity: Severity::High,
                        category: "security".into(),
                        source: Source::LocalAst,
                        line_start: default_match,
                        line_end: default_match,
                        evidence: vec![],
                        calibrator_action: None,
                        similar_precedent: vec![],
                        canonical_pattern: None,
                    });
                }
            }
        }
    }

    // T3: Wildcard IAM actions -- detect Action = "*" in policy text
    // Policy documents are typically written inline as jsonencode({...})
    // We scan for the pattern in string content
    if kind == "template_literal" || kind == "string_lit" || kind == "quoted_template" {
        let val = text.trim_matches('"');
        // Look for IAM wildcard patterns inside heredocs or jsonencode strings
        if (val.contains("\"Action\"") || val.contains("\"action\""))
            && (val.contains("\"*\"") || val.contains(": \"*\""))
        {
            findings.push(Finding {
                title: "Wildcard IAM action (Action: \"*\") -- violates least-privilege".into(),
                description: "IAM policy grants all actions. Scope down to only the actions required.".into(),
                severity: Severity::High,
                category: "security".into(),
                source: Source::LocalAst,
                line_start,
                line_end,
                evidence: vec!["Action = \"*\"".into()],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
            });
        }
    }

    // T4: Open security groups -- cidr_blocks containing 0.0.0.0/0 on sensitive ports
    // We detect this at the block level: look for ingress blocks with 0.0.0.0/0
    // and check if the port is sensitive (22, 3306, 5432, 3389, etc.)
    if kind == "block" && text.starts_with("ingress") {
        if text.contains("0.0.0.0/0") || text.contains("::/0") {
            // Check if port is sensitive (not 80 or 443)
            let safe_ports = ["80", "443"];
            let has_sensitive_port = !safe_ports.iter().any(|port| {
                // Match from_port = 80 or from_port = 443
                text.contains(&format!("from_port")) && {
                    // Extract the port value
                    if let Some(idx) = text.find("from_port") {
                        let after = &text[idx..];
                        if let Some(eq_idx) = after.find('=') {
                            let val = after[eq_idx + 1..].trim().split_whitespace().next().unwrap_or("");
                            val.trim() == *port
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
            });
            if has_sensitive_port {
                findings.push(Finding {
                    title: "Security group ingress open to 0.0.0.0/0 on sensitive port".into(),
                    description: "Ingress rule allows traffic from any IP. Restrict the CIDR block to known ranges.".into(),
                    severity: Severity::High,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start,
                    line_end,
                    evidence: vec![text.lines().next().unwrap_or("").trim().to_string()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }
}

/// Find a `default` attribute with a string literal value inside a block node.
/// Returns the line number of the default attribute if found.
fn find_default_with_literal(block: &tree_sitter::Node, source: &str) -> Option<u32> {
    // Walk children looking for the body, then attributes named "default"
    for i in 0..block.child_count() {
        let Some(child) = block.child(i) else { continue };
        if child.kind() == "body" || child.kind() == "block" {
            for j in 0..child.child_count() {
                let Some(attr) = child.child(j) else { continue };
                if attr.kind() == "attribute" {
                    if let Some(name_node) = attr.child(0) {
                        let name = &source[name_node.byte_range()];
                        if name == "default" {
                            if let Some(val_node) = attr.child(2) {
                                let vk = val_node.kind();
                                if vk == "template_literal"
                                    || vk == "string_lit"
                                    || vk == "quoted_template"
                                {
                                    let val = &source[val_node.byte_range()];
                                    let inner = val.trim_matches('"');
                                    if !inner.is_empty() && !inner.starts_with("${") {
                                        return Some(attr.start_position().row as u32 + 1);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Recurse into nested structures
        if let Some(line) = find_default_with_literal(&child, source) {
            return Some(line);
        }
    }
    None
}
```

**Important note for implementer:** The exact node kinds in tree-sitter-hcl may differ from what's written here. Before implementing, parse a sample `.tf` file and dump the AST to see the actual node kinds:

```rust
// Debug: print AST structure
fn debug_print_tree(node: &tree_sitter::Node, source: &str, indent: usize) {
    let prefix = " ".repeat(indent);
    let text_preview: String = source[node.byte_range()].chars().take(60).collect();
    eprintln!("{}{} [{}-{}] {:?}", prefix, node.kind(), node.start_position().row, node.end_position().row, text_preview);
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            debug_print_tree(&child, source, indent + 2);
        }
    }
}
```

Use this to verify node kinds like `attribute`, `block`, `body`, `template_literal` etc. and adjust the pattern matchers accordingly. The node kinds listed above are best-guesses from tree-sitter-hcl grammar -- the implementer MUST verify them.

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum analysis::tests::terraform 2>&1 | tail -15`
Expected: All 7 terraform security pattern tests pass

**Step 5: Commit**

```bash
git add src/analysis.rs
git commit -m "feat: add Terraform AST security patterns (secrets, IAM wildcards, open SGs)"
```

---

### Task 6: Add analyze_terraform_structure

**Files:**
- Modify: `src/analysis.rs` (analyze_terraform_structure + hook into analyze_insecure_patterns)
- Test: `src/analysis.rs` (inline tests)

**Step 1: Write failing tests**

```rust
#[test]
fn terraform_missing_required_version() {
    let source = r#"provider "aws" {
  region = "us-east-1"
}

resource "aws_instance" "web" {
  ami = "ami-123"
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(findings.iter().any(|f| f.title.contains("required_version")),
        "Should warn about missing terraform required_version: {:?}", findings);
}

#[test]
fn terraform_has_required_version_is_ok() {
    let source = r#"terraform {
  required_version = ">= 1.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(!findings.iter().any(|f| f.title.contains("required_version")),
        "Should not warn when required_version is present: {:?}", findings);
}

#[test]
fn terraform_missing_variable_description() {
    let source = r#"variable "instance_type" {
  type    = string
  default = "t3.micro"
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(findings.iter().any(|f| f.title.contains("description")),
        "Should warn about missing variable description: {:?}", findings);
}

#[test]
fn terraform_variable_with_description_is_ok() {
    let source = r#"variable "instance_type" {
  type        = string
  description = "The EC2 instance type to use"
  default     = "t3.micro"
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(!findings.iter().any(|f| f.title.contains("description")),
        "Should not warn when description is present: {:?}", findings);
}

#[test]
fn terraform_provider_without_version() {
    let source = r#"terraform {
  required_version = ">= 1.0"
  required_providers {
    aws = {
      source = "hashicorp/aws"
    }
  }
}
"#;
    let tree = crate::parser::parse(source, Language::Terraform).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Terraform);
    assert!(findings.iter().any(|f| f.title.contains("version constraint")),
        "Should warn about missing provider version: {:?}", findings);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum analysis::tests::terraform_missing_required 2>&1 | tail -5`
Expected: FAIL -- no structural analysis yet

**Step 3: Implement analyze_terraform_structure**

```rust
fn analyze_terraform_structure(
    tree: &tree_sitter::Tree,
    source: &str,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let root = tree.root_node();

    let mut has_required_version = false;
    let mut variable_blocks: Vec<(u32, u32, bool)> = Vec::new(); // (start, end, has_description)
    let mut provider_blocks_without_version: Vec<(String, u32)> = Vec::new();

    // Walk top-level blocks
    for i in 0..root.child_count() {
        let Some(child) = root.child(i) else { continue };
        if child.kind() != "block" {
            continue;
        }
        let block_text = &source[child.byte_range()];

        // Check for terraform { required_version = ... }
        if block_text.starts_with("terraform") {
            if block_text.contains("required_version") {
                has_required_version = true;
            }
            // Check for required_providers without version
            if block_text.contains("required_providers") {
                // Naive: look for provider blocks inside that don't contain "version"
                // This is a text heuristic -- good enough for structure-level analysis
                // More precise detection would need deeper AST walking
                let in_providers = &block_text[block_text.find("required_providers").unwrap_or(0)..];
                // Split by provider name patterns -- each "name = {" section
                // For now, flag if source is present but version is not in the same block
                if in_providers.contains("source") && !in_providers.contains("version") {
                    findings.push(Finding {
                        title: "Provider missing version constraint in required_providers".into(),
                        description: "Pin provider versions to avoid unexpected breaking changes on terraform init.".into(),
                        severity: Severity::Medium,
                        category: "reliability".into(),
                        source: Source::LocalAst,
                        line_start: child.start_position().row as u32 + 1,
                        line_end: child.end_position().row as u32 + 1,
                        evidence: vec![],
                        calibrator_action: None,
                        similar_precedent: vec![],
                        canonical_pattern: None,
                    });
                }
            }
        }

        // Check for variable blocks missing description
        if block_text.starts_with("variable") {
            let has_desc = block_text.contains("description");
            if !has_desc {
                findings.push(Finding {
                    title: "Variable missing `description` -- add documentation for consumers".into(),
                    description: "Variables should include a description to help module consumers understand their purpose.".into(),
                    severity: Severity::Low,
                    category: "quality".into(),
                    source: Source::LocalAst,
                    line_start: child.start_position().row as u32 + 1,
                    line_end: child.end_position().row as u32 + 1,
                    evidence: vec![],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }

    // Warn if no terraform { required_version } found and file has resources
    if !has_required_version {
        let has_resources = source.contains("resource \"") || source.contains("data \"");
        if has_resources {
            findings.push(Finding {
                title: "Missing `terraform { required_version }` -- pin Terraform version".into(),
                description: "Without required_version, this configuration may break on incompatible Terraform versions.".into(),
                severity: Severity::Medium,
                category: "reliability".into(),
                source: Source::LocalAst,
                line_start: 1,
                line_end: 1,
                evidence: vec![],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
            });
        }
    }

    findings
}
```

Hook it into `analyze_insecure_patterns` (modify the existing function around line 156):

```rust
pub fn analyze_insecure_patterns(
    tree: &tree_sitter::Tree,
    source: &str,
    lang: Language,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    scan_insecure_nodes(&tree.root_node(), source, lang, &mut findings);
    if matches!(lang, Language::Dockerfile) {
        findings.extend(analyze_dockerfile_structure(tree, source));
    }
    if matches!(lang, Language::Terraform) {
        findings.extend(analyze_terraform_structure(tree, source));
    }
    findings
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum analysis::tests::terraform 2>&1 | tail -15`
Expected: All terraform tests pass

**Step 5: Commit**

```bash
git add src/analysis.rs
git commit -m "feat: add Terraform structural analysis (required_version, variable descriptions, provider versions)"
```

---

### Task 7: Full test suite verification

**Step 1: Run all tests**

Run: `cargo test --bin quorum 2>&1 | tail -20`
Expected: All ~510+ tests pass (492 existing + ~18 new)

**Step 2: Run full test suite including integration tests**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass

**Step 3: Build release binary**

Run: `cargo build --release 2>&1 | tail -5`
Expected: Clean build, check binary size is still reasonable (~31-32MB)

**Step 4: Smoke test with a real .tf file**

Create a test file and review it:

```bash
cat > /tmp/test.tf << 'EOF'
variable "db_password" {
  type    = string
  default = "supersecret"
}

resource "aws_instance" "web" {
  ami           = "ami-123456"
  instance_type = "t3.micro"
}

resource "aws_security_group" "open" {
  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
EOF

cargo run -- review /tmp/test.tf
```

Expected: Should see findings for hardcoded secret, missing required_version, open SG, and missing variable description.

**Step 5: Final commit if any fixups needed**

```bash
git add -A
git commit -m "fix: address any issues found in smoke testing"
```

---

### Task 8: Update memory and version

**Step 1: Bump version to 0.9.4 in Cargo.toml**

Change: `version = "0.9.3"` to `version = "0.9.4"`

**Step 2: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.9.4 for Terraform support"
```
