# Bash/Shell & Dockerfile Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add Bash/shell script and Dockerfile AST-aware review to quorum, with language-specific pattern detections for common bugs and security issues.

**Architecture:** Same pattern as YAML support -- add tree-sitter grammars, wire Language variants through parser/analysis/hydration/pipeline, implement language-specific `scan_insecure_*` functions. Shell gets function extraction (bash has `function_definition`); Dockerfile does not (no functions). Both get LLM-augmented review with domain detection. Linter integration: `shellcheck` for bash, `hadolint` for Dockerfile.

**Tech Stack:** `tree-sitter-bash` 0.25 (mature, official tree-sitter org), `tree-sitter-dockerfile` 0.2 (camdencheek, ast-grep built-in)

---

## Pattern Research

### Bash/Shell Patterns (14 detections)

**tree-sitter-bash key nodes:**
- `function_definition` -- has `name` and `body` fields
- `command` -- a command invocation, has `name` and `argument` children
- `variable_assignment` -- has `name` and `value` fields
- `command_substitution` -- `$(...)` or backticks
- `string` / `raw_string` -- double/single quoted
- `simple_expansion` / `expansion` -- `$VAR` / `${VAR}`
- `if_statement`, `for_statement`, `while_statement`, `case_statement`
- `pipeline` -- piped commands
- `redirected_statement` -- commands with `>`, `>>`, `<`

| # | Pattern | Severity | AST Detection |
|---|---------|----------|---------------|
| B1 | **Unquoted variable expansion** | High/bug | `command` with `simple_expansion` child not inside `string` node. E.g., `rm $file` vs `rm "$file"` |
| B2 | **`eval` usage** | High/security | `command` where name = `eval` |
| B3 | **`curl \| bash` piping** | Critical/security | `pipeline` where left command name matches `curl`/`wget` and right command name = `bash`/`sh` |
| B4 | **Missing `set -e` / `set -euo pipefail`** | Medium/reliability | Check if root `program` node's first statements include a `command` with `set` and args containing `-e` |
| B5 | **Hardcoded secrets in assignments** | High/security | `variable_assignment` where name matches secret patterns and value is a `string`/`raw_string` (not `command_substitution` or env var) |
| B6 | **`rm -rf /` or `rm -rf $VAR/`** | Critical/bug | `command` name=`rm` with `-rf` arg and `/` or variable expansion followed by `/` |
| B7 | **Backtick command substitution** | Low/quality | `command_substitution` using backticks (check source text starts with `` ` `` instead of `$(`) |
| B8 | **`cd` without `|| exit`** | Medium/reliability | `command` name=`cd` not followed by `||` in parent |
| B9 | **`chmod 777`** | Medium/security | `command` name=`chmod` with `777` argument |
| B10 | **Password/secret in command args** | High/security | `command` with argument containing `--password=` or `-p` followed by a `string` (not env var) |
| B11 | **Missing shebang** | Low/quality | Root `program` first child is not a `comment` starting with `#!` |
| B12 | **`sudo` in scripts** | Info/security | `command` name=`sudo` -- flag for awareness |
| B13 | **Unquoted glob in test** | Medium/bug | `test_command` or `[`/`[[` with unquoted expansion |
| B14 | **Hardcoded paths** | Info/quality | `string` values containing `/home/`, `/Users/`, `/root/` |

### Dockerfile Patterns (12 detections)

**tree-sitter-dockerfile key nodes:**
- `from_instruction` -- FROM with image_spec (image, tag, digest, as_name)
- `run_instruction` -- RUN with shell_fragment or json_string_array
- `copy_instruction` -- COPY with `--from`, `--chown` params
- `expose_instruction` -- EXPOSE with port
- `user_instruction` -- USER instruction
- `env_instruction` -- ENV key=value
- `arg_instruction` -- ARG with default value
- `healthcheck_instruction` -- HEALTHCHECK
- `add_instruction` -- ADD (vs COPY)
- `label_instruction` -- LABEL metadata

| # | Pattern | Severity | AST Detection |
|---|---------|----------|---------------|
| D1 | **FROM with `latest` or no tag** | Medium/reliability | `from_instruction` where image_spec has no tag child or tag text = `latest` |
| D2 | **RUN with `apt-get install` without `--no-install-recommends`** | Low/quality | `run_instruction` shell_fragment containing `apt-get install` but not `--no-install-recommends` |
| D3 | **RUN with `apt-get install` without version pinning** | Medium/reliability | `apt-get install` without `=` version pins on packages |
| D4 | **Missing `apt-get clean` or `rm -rf /var/lib/apt/lists`** | Low/quality | `run_instruction` with `apt-get install` but no cleanup in same or adjacent RUN |
| D5 | **ADD instead of COPY** | Medium/quality | `add_instruction` where source is not a URL -- should use COPY |
| D6 | **No USER instruction (running as root)** | Medium/security | No `user_instruction` found in the entire source_file |
| D7 | **EXPOSE with common debug ports** | Info/security | `expose_instruction` with ports 22 (SSH), 5432 (postgres), 3306 (mysql), 6379 (redis) |
| D8 | **No HEALTHCHECK** | Low/reliability | No `healthcheck_instruction` in source_file |
| D9 | **Secrets in ENV/ARG** | High/security | `env_instruction` or `arg_instruction` with key matching secret patterns and hardcoded value |
| D10 | **COPY --chown missing** | Info/quality | `copy_instruction` without `--chown` param |
| D11 | **Multiple CMD/ENTRYPOINT** | Medium/bug | More than one `cmd_instruction` or `entrypoint_instruction` -- only last takes effect |
| D12 | **RUN with `curl \| bash`** | Critical/security | `run_instruction` shell_fragment containing `curl.*\|.*bash` or `wget.*\|.*sh` |

---

## Task 1: Add tree-sitter-bash and Language::Bash

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/parser.rs`

**Step 1: Write the failing tests**

Add to `src/parser.rs` tests:

```rust
#[test]
fn detect_language_bash() {
    assert_eq!(Language::from_extension("sh"), Some(Language::Bash));
    assert_eq!(Language::from_extension("bash"), Some(Language::Bash));
    assert_eq!(Language::from_extension("zsh"), Some(Language::Bash));
}

#[test]
fn detect_language_bash_from_path() {
    assert_eq!(
        Language::from_path(std::path::Path::new("deploy.sh")),
        Some(Language::Bash)
    );
    assert_eq!(
        Language::from_path(std::path::Path::new("install.bash")),
        Some(Language::Bash)
    );
}

#[test]
fn parse_valid_bash() {
    let source = "#!/bin/bash\nset -euo pipefail\necho \"hello\"\n";
    let tree = parse(source, Language::Bash).unwrap();
    assert_eq!(tree.root_node().kind(), "program");
    assert!(!tree.root_node().has_error());
}

#[test]
fn parse_bash_function() {
    let source = "#!/bin/bash\nmy_func() {\n  echo \"hello\"\n  return 0\n}\n";
    let tree = parse(source, Language::Bash).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn extract_functions_bash() {
    let source = "#!/bin/bash\nmy_func() {\n  echo \"inside\"\n}\n\nanother() {\n  return 1\n}\n";
    let tree = parse(source, Language::Bash).unwrap();
    let fns = extract_functions(&tree, source, Language::Bash);
    let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["my_func", "another"]);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum parser::tests::detect_language_bash`
Expected: compile error -- `Language::Bash` doesn't exist

**Step 3: Implement**

In `Cargo.toml` add:
```toml
tree-sitter-bash = "0.23"
```
(Note: use 0.23 which is compatible with tree-sitter 0.25 -- check crates.io for exact compatible version)

In `src/parser.rs`:
- Add `Bash` to `Language` enum
- `from_extension`: `"sh" | "bash" | "zsh" | "bats" => Some(Language::Bash),`
- `tree_sitter_language`: `Language::Bash => tree_sitter_bash::LANGUAGE.into(),`
- `function_node_kinds`: `Language::Bash => &["function_definition"],`

**Step 4: Run tests, fix exhaustive match errors in all files**

Run: `cargo test --bin quorum`
Expected: compile errors for missing `Language::Bash` arms. Add stub arms:
- `analysis.rs`: `Language::Bash => {}` in complexity func_kinds, `Language::Bash => scan_insecure_bash(node, source, findings)` in scan_insecure_nodes (start with empty fn)
- `hydration.rs`: empty vecs for all 4 functions
- `pipeline.rs`: `Language::Bash => "bash"` in lang_name
- `mcp/handler.rs`: `"bash"` in lang name matches

**Step 5: Verify all tests pass**

Run: `cargo test --bin quorum`
Expected: all pass (404 + new bash tests)

**Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/parser.rs src/analysis.rs src/hydration.rs src/pipeline.rs src/mcp/handler.rs
git commit -m "feat: add tree-sitter-bash and Language::Bash variant"
```

---

## Task 2: Add tree-sitter-dockerfile and Language::Dockerfile

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/parser.rs`
- Modify: same files as Task 1 for exhaustive matches

**Step 1: Write the failing tests**

```rust
#[test]
fn detect_language_dockerfile() {
    assert_eq!(Language::from_extension("dockerfile"), Some(Language::Dockerfile));
}

#[test]
fn detect_language_dockerfile_from_path() {
    assert_eq!(
        Language::from_path(std::path::Path::new("Dockerfile")),
        Some(Language::Dockerfile)
    );
    assert_eq!(
        Language::from_path(std::path::Path::new("Dockerfile.prod")),
        Some(Language::Dockerfile)
    );
}

#[test]
fn parse_valid_dockerfile() {
    let source = "FROM node:18-alpine\nRUN npm install\nCOPY . /app\nCMD [\"node\", \"server.js\"]\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    assert_eq!(tree.root_node().kind(), "source_file");
    assert!(!tree.root_node().has_error());
}
```

**Step 2: Run, fail, implement**

In `Cargo.toml`: `tree-sitter-dockerfile = "0.2"`

In `parser.rs`:
- Add `Dockerfile` to enum
- `from_extension`: `"dockerfile" => Some(Language::Dockerfile),`
- `from_path`: **Special case** -- Dockerfiles often have no extension. Add check: if filename starts with "Dockerfile" (case-insensitive), return `Some(Language::Dockerfile)`. Modify `from_path` to check filename before extension:

```rust
pub fn from_path(path: &Path) -> Option<Self> {
    // Check filename first for extensionless files (Dockerfile, Makefile, etc.)
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.to_lowercase().starts_with("dockerfile") {
            return Some(Language::Dockerfile);
        }
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(Self::from_extension)
}
```

- `tree_sitter_language`: `Language::Dockerfile => tree_sitter_dockerfile::LANGUAGE.into(),`
- `function_node_kinds`: `Language::Dockerfile => &[],` (no functions)

**Step 3: Wire through all exhaustive matches (same pattern as Bash)**

**Step 4: Verify all tests pass**

**Step 5: Commit**

```bash
git commit -m "feat: add tree-sitter-dockerfile and Language::Dockerfile variant"
```

---

## Task 3: Implement Bash analysis patterns

**Files:**
- Modify: `src/analysis.rs`

**Step 1: Write the failing tests**

```rust
// -- Bash patterns --

#[test]
fn bash_eval_usage() {
    let source = "#!/bin/bash\neval \"$user_input\"\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(findings.iter().any(|f| f.title.contains("eval")),
        "Should flag eval usage. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn bash_curl_pipe_bash() {
    let source = "#!/bin/bash\ncurl -sL https://example.com/install.sh | bash\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(findings.iter().any(|f| f.severity == Severity::Critical),
        "Should flag curl|bash as critical. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn bash_missing_set_e() {
    let source = "#!/bin/bash\necho hello\nrm -rf /tmp/stuff\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(findings.iter().any(|f| f.title.contains("set -e") || f.title.contains("error handling")),
        "Should flag missing set -e. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn bash_set_euo_pipefail_ok() {
    let source = "#!/bin/bash\nset -euo pipefail\necho hello\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(!findings.iter().any(|f| f.title.contains("set -e")),
        "Script with set -e should NOT be flagged");
}

#[test]
fn bash_hardcoded_secret() {
    let source = "#!/bin/bash\nAPI_KEY=\"sk-proj-abc123def456\"\nPASSWORD='SuperSecret123!'\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(findings.iter().any(|f| f.category == "security" && f.title.contains("secret")),
        "Should flag hardcoded secrets. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn bash_secret_from_env_ok() {
    let source = "#!/bin/bash\nAPI_KEY=$(vault get api-key)\nPASSWORD=\"$DB_PASSWORD\"\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(!findings.iter().any(|f| f.title.contains("secret")),
        "Secrets from env/command should NOT be flagged");
}

#[test]
fn bash_chmod_777() {
    let source = "#!/bin/bash\nchmod 777 /var/www/app\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(findings.iter().any(|f| f.title.contains("chmod") && f.title.contains("777")),
        "Should flag chmod 777. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn bash_missing_shebang() {
    let source = "echo hello\nrm -rf /tmp/stuff\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(findings.iter().any(|f| f.title.contains("shebang")),
        "Should flag missing shebang. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn bash_shebang_present_ok() {
    let source = "#!/usr/bin/env bash\necho hello\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    assert!(!findings.iter().any(|f| f.title.contains("shebang")),
        "Script with shebang should NOT be flagged");
}

#[test]
fn bash_clean_script_no_findings() {
    let source = "#!/usr/bin/env bash\nset -euo pipefail\n\nmain() {\n  echo \"deploying\"\n}\n\nmain \"$@\"\n";
    let tree = parse(source, Language::Bash).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
    // Only info-level findings at most (no bugs, no security issues)
    let serious = findings.iter().filter(|f| f.severity >= Severity::Medium).count();
    assert_eq!(serious, 0, "Clean script should have no serious findings. Got: {:?}",
        findings.iter().map(|f| (&f.severity, &f.title)).collect::<Vec<_>>());
}
```

**Step 2: Implement `scan_insecure_bash`**

Key patterns to implement:
- B2: `eval` usage (command name check)
- B3: `curl|bash` pipeline detection
- B4: Missing `set -e` (check first few statements of program)
- B5: Hardcoded secrets (variable_assignment with secret name + literal value)
- B9: `chmod 777`
- B11: Missing shebang
- B12: `sudo` usage (info level)

For the `curl|bash` pattern, walk `pipeline` nodes and check if any command in the pipeline has name matching `curl`/`wget` and any subsequent command has name `bash`/`sh`/`zsh`.

For `set -e`, walk the first ~5 statements of the `program` node looking for a `command` whose text matches `set.*-e` or `set.*-o.*errexit`.

**Step 3: Run tests, all pass**

**Step 4: Commit**

```bash
git commit -m "feat: bash analysis patterns (eval, curl|bash, set -e, secrets, chmod)"
```

---

## Task 4: Implement Dockerfile analysis patterns

**Files:**
- Modify: `src/analysis.rs`

**Step 1: Write the failing tests**

```rust
// -- Dockerfile patterns --

#[test]
fn dockerfile_from_latest() {
    let source = "FROM node:latest\nRUN npm install\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.title.contains("latest")),
        "Should flag FROM with :latest. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_from_no_tag() {
    let source = "FROM node\nRUN npm install\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.title.contains("tag") || f.title.contains("latest")),
        "Should flag FROM without tag. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_from_pinned_ok() {
    let source = "FROM node:18-alpine\nRUN npm install\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(!findings.iter().any(|f| f.title.contains("latest") || f.title.contains("untagged")),
        "Pinned image should NOT be flagged");
}

#[test]
fn dockerfile_no_user() {
    let source = "FROM node:18\nRUN npm install\nCOPY . /app\nCMD [\"node\", \"app.js\"]\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.title.contains("USER") || f.title.contains("root")),
        "Should flag missing USER. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_has_user_ok() {
    let source = "FROM node:18\nRUN npm install\nUSER node\nCMD [\"node\", \"app.js\"]\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(!findings.iter().any(|f| f.title.contains("USER") && f.title.contains("missing")),
        "Dockerfile with USER should NOT be flagged for missing USER");
}

#[test]
fn dockerfile_add_instead_of_copy() {
    let source = "FROM node:18\nADD . /app\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.title.contains("ADD") || f.title.contains("COPY")),
        "Should flag ADD when COPY would suffice. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_add_url_ok() {
    let source = "FROM node:18\nADD https://example.com/file.tar.gz /tmp/\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(!findings.iter().any(|f| f.title.contains("ADD") && f.title.contains("COPY")),
        "ADD with URL should NOT suggest COPY");
}

#[test]
fn dockerfile_no_healthcheck() {
    let source = "FROM node:18\nRUN npm install\nCMD [\"node\", \"app.js\"]\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.title.contains("HEALTHCHECK")),
        "Should flag missing HEALTHCHECK. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_secrets_in_env() {
    let source = "FROM node:18\nENV API_KEY=sk-proj-abc123def456\nENV PASSWORD=SuperSecret123\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.category == "security" && f.title.contains("secret")),
        "Should flag secrets in ENV. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_curl_pipe_bash() {
    let source = "FROM ubuntu:22.04\nRUN curl -sL https://deb.nodesource.com/setup_18.x | bash -\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.severity == Severity::Critical),
        "Should flag curl|bash in RUN. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_multiple_cmd() {
    let source = "FROM node:18\nCMD [\"echo\", \"first\"]\nCMD [\"echo\", \"second\"]\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    assert!(findings.iter().any(|f| f.title.contains("Multiple CMD") || f.title.contains("CMD")),
        "Should flag multiple CMD. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
}

#[test]
fn dockerfile_clean_no_serious_findings() {
    let source = "FROM node:18-alpine AS build\nWORKDIR /app\nCOPY package*.json ./\nRUN npm ci --only=production\nCOPY . .\n\nFROM node:18-alpine\nWORKDIR /app\nCOPY --from=build /app .\nUSER node\nHEALTHCHECK CMD curl -f http://localhost:3000/ || exit 1\nCMD [\"node\", \"server.js\"]\n";
    let tree = parse(source, Language::Dockerfile).unwrap();
    let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
    let serious = findings.iter().filter(|f| f.severity >= Severity::Medium).count();
    assert_eq!(serious, 0, "Clean Dockerfile should have no serious findings. Got: {:?}",
        findings.iter().filter(|f| f.severity >= Severity::Medium).map(|f| (&f.severity, &f.title)).collect::<Vec<_>>());
}
```

**Step 2: Implement `scan_insecure_dockerfile`**

For whole-file checks (missing USER, missing HEALTHCHECK, multiple CMD), use a separate function that walks the root node once:

```rust
fn analyze_dockerfile_structure(tree: &tree_sitter::Tree, source: &str) -> Vec<Finding> {
    // Walk root children, track: has_user, has_healthcheck, cmd_count, from_instructions
    // Emit findings for missing USER, missing HEALTHCHECK, multiple CMD, FROM latest/untagged
}
```

Call this from `analyze_insecure_patterns` when lang is Dockerfile, in addition to the per-node scan.

For per-node patterns in `scan_insecure_dockerfile`:
- `env_instruction` / `arg_instruction`: check for secret key names with hardcoded values
- `add_instruction`: check if source is not a URL (suggest COPY)
- `run_instruction`: check shell_fragment text for `curl.*|.*bash` pattern
- `expose_instruction`: check for debug ports

**Step 3: Run tests, all pass**

**Step 4: Commit**

```bash
git commit -m "feat: dockerfile analysis patterns (FROM latest, USER, HEALTHCHECK, secrets, ADD)"
```

---

## Task 5: Add shellcheck and hadolint linter detection

**Files:**
- Modify: `src/linter.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn detect_shellcheck() {
    // shellcheck doesn't need a config file -- detect if .sh files exist
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("deploy.sh"), "#!/bin/bash\necho hi\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Shellcheck));
}

#[test]
fn detect_hadolint_from_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".hadolint.yaml"), "ignored: [DL3008]\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Hadolint));
}

#[test]
fn detect_hadolint_from_dockerfile() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Dockerfile"), "FROM node:18\nRUN npm install\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Hadolint));
}

#[test]
fn normalize_shellcheck_output() {
    let json = r#"[{"file":"test.sh","line":3,"endLine":3,"column":1,"endColumn":1,"level":"warning","code":2086,"message":"Double quote to prevent globbing and word splitting."}]"#;
    let findings = normalize_shellcheck_output(json).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].line_start, 3);
    assert!(findings[0].title.contains("SC2086"));
    assert_eq!(findings[0].source, Source::Linter("shellcheck".into()));
}

#[test]
fn normalize_hadolint_output() {
    let output = "Dockerfile:3 DL3008 warning: Pin versions in apt-get install\nDockerfile:1 DL3006 warning: Always tag the version of an image explicitly\n";
    let findings = normalize_hadolint_output(output).unwrap();
    assert_eq!(findings.len(), 2);
    assert!(findings[0].title.contains("DL3008"));
}
```

**Step 2: Implement**

Add `Shellcheck` and `Hadolint` to `LinterKind`.

Detection:
- Shellcheck: detect if any `.sh` file exists in project root
- Hadolint: detect `.hadolint.yaml`/`.hadolint.yml` OR any `Dockerfile` exists

Runner args:
- `shellcheck --format=json1 <file>`
- `hadolint --format tty <file>` (parsable: `file:line rule level: message`)

Add normalizers for each output format.

**Step 3: Run tests, commit**

```bash
git commit -m "feat: add shellcheck and hadolint linter detection"
```

---

## Task 6: Update docs, version bump, compile

**Files:**
- Modify: `CLAUDE.md` -- add Bash/Dockerfile to language table
- Modify: `.claude/skills/quorum-cli.md` -- add Bash/Dockerfile patterns
- Modify: `docs/ARCHITECTURE.md` -- add language crates
- Modify: `Cargo.toml` -- version bump to 0.8.0
- Update: memory files

**Step 1: Update all docs**

**Step 2: Version bump**

**Step 3: Full test suite**

Run: `cargo test --bin quorum`
Expected: all tests pass

**Step 4: Release build + install**

```bash
cargo build --release
cp target/release/quorum ~/.local/bin/quorum
quorum version  # should show 0.8.0
```

**Step 5: Commit**

```bash
git commit -m "chore: bump version to 0.8.0 -- Bash + Dockerfile support"
```

---

## Task 7: Test on real files

**Step 1: Review shell scripts**

```bash
quorum review ~/Sources/github.com/jsnyder/house_memory/scripts/*.sh --json --no-auto-calibrate
```

**Step 2: Review Dockerfiles**

```bash
find ~/Sources/github.com/jsnyder -maxdepth 4 -name "Dockerfile" | head -5 | xargs quorum review --json --no-auto-calibrate
```

**Step 3: Record feedback from results**

Triage findings, record TP/FP to feedback store.
