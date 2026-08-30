# Go Language Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Go as a first-class language in quorum with full pipeline integration — parser, 18 AST rules, golangci-lint, symbol extraction, fingerprinting, go.mod parsing, and Context7 enrichment.

**Architecture:** The existing multi-language system uses a `Language` enum dispatching to per-language tree-sitter grammars, ast-grep YAML rules (bundled via `include_str!`), and language-specific extractors/fingerprinters. Go follows the same pattern with additions for module-path-aware import matching.

**Tech Stack:** Rust, tree-sitter-go, ast-grep (SupportLang::Go), golangci-lint (external), go.mod text parsing

---

## Parallelism Map

```
Phase 1 (sequential): Task 1 — Foundation (Cargo.toml + parser.rs)
Phase 2 (ALL PARALLEL): Tasks 2-7 — independent once Phase 1 compiles
  Task 2: AST lint rules (18 YAML files + test fixtures)
  Task 3: Symbol extraction rules (4 YAML files)
  Task 4: go.mod parsing (dep_manifest.rs)
  Task 5: Linter integration (linter.rs)
  Task 6: Fingerprinter (fingerprint_go.rs)
  Task 7: AST extractor (astgrep_go.rs) — depends on Task 3
Phase 3 (sequential after Phase 2): Task 8 — Dispatch wiring + Context7 enrichment
Phase 4 (sequential after Phase 3): Task 9 — Integration wiring + final tests
```

---

### Task 1: Foundation — Cargo.toml + Parser Registration

**Files:**
- Modify: `Cargo.toml:73-92`
- Modify: `src/parser.rs:3-74`

- [ ] **Step 1: Add tree-sitter-go dependency to Cargo.toml**

Add after line 78 (`tree-sitter-bash = "0.25"`):

```toml
tree-sitter-go = "0.23"
```

Add `"tree-sitter-go"` to the ast-grep-language features list (line 85-92):

```toml
ast-grep-language = { version = "=0.42.2", default-features = false, features = [
    "tree-sitter-bash",
    "tree-sitter-go",
    "tree-sitter-hcl",
    "tree-sitter-javascript",
    "tree-sitter-python",
    "tree-sitter-rust",
    "tree-sitter-typescript",
    "tree-sitter-yaml",
] }
```

- [ ] **Step 2: Add Language::Go variant and all match arms**

In `src/parser.rs`, add `Go` to the enum (line 4-13):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    Yaml,
    Bash,
    Dockerfile,
    Terraform,
    Go,
}
```

Add extension mapping in `from_extension` (before the `_ => None` arm):

```rust
"go" => Some(Language::Go),
```

Add tree-sitter binding in `tree_sitter_language`:

```rust
Language::Go => tree_sitter_go::LANGUAGE.into(),
```

Add function node kinds:

```rust
Language::Go => &["function_declaration", "method_declaration"],
```

- [ ] **Step 3: Add parser unit tests for Go**

Add to `mod tests` in `src/parser.rs`:

```rust
#[test]
fn detect_language_go() {
    assert_eq!(Language::from_extension("go"), Some(Language::Go));
    assert_eq!(Language::from_extension("GO"), Some(Language::Go));
}

#[test]
fn detect_language_go_from_path() {
    assert_eq!(
        Language::from_path(std::path::Path::new("main.go")),
        Some(Language::Go)
    );
    assert_eq!(
        Language::from_path(std::path::Path::new("pkg/server/handler.go")),
        Some(Language::Go)
    );
}

#[test]
fn parse_valid_go() {
    let source = "package main\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n";
    let tree = parse(source, Language::Go).unwrap();
    assert_eq!(tree.root_node().kind(), "source_file");
    assert!(!tree.root_node().has_error());
}

#[test]
fn extract_functions_go() {
    let source = "package main\n\nfunc foo() {}\nfunc bar() {}\n";
    let tree = parse(source, Language::Go).unwrap();
    let fns = extract_functions(&tree, source, Language::Go);
    let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["foo", "bar"]);
}

#[test]
fn extract_functions_go_methods() {
    let source = "package main\n\ntype Server struct{}\n\nfunc (s *Server) Serve() {}\nfunc (s *Server) Stop() {}\n";
    let tree = parse(source, Language::Go).unwrap();
    let fns = extract_functions(&tree, source, Language::Go);
    let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["Serve", "Stop"]);
}
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check 2>&1 | head -30`
Expected: Successful compilation (or only unrelated warnings). If tree-sitter-go version is wrong, adjust to the latest compatible version.

- [ ] **Step 5: Run parser tests**

Run: `cargo test --bin quorum parser::tests -- --nocapture 2>&1 | tail -20`
Expected: All new Go tests pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/parser.rs
git commit -m "feat(go): add Language::Go with tree-sitter parser and function extraction"
```

---

### Task 2: AST Lint Rules (18 YAML files + test fixtures)

**Files:**
- Create: `rules/go/*.yml` (18 files)
- Create: `rules/go/tests/*.go` (18 fixture files)

**Note:** These are pure YAML/Go files — no Rust compilation needed. Can run in parallel with Tasks 3-7.

- [ ] **Step 1: Create rules/go/ directory structure**

```bash
mkdir -p rules/go/tests
```

- [ ] **Step 2: Write error handling rules (5 files)**

`rules/go/ignored-error-return.yml`:
```yaml
id: ignored-error-return
language: Go
severity: warning
message: "Error return value assigned to blank identifier. Handle or propagate the error."
rule:
  pattern: $_, _ = $FUNC($$$ARGS)
```

`rules/go/bare-error-format.yml`:
```yaml
id: bare-error-format
language: Go
severity: warning
message: "fmt.Errorf without %w verb loses the error chain. Use %w to wrap errors."
rule:
  pattern: fmt.Errorf($FMT, $$$ARGS)
constraints:
  FMT:
    not:
      regex: '%w'
```

`rules/go/error-sprintf.yml`:
```yaml
id: error-sprintf
language: Go
severity: hint
message: "Use fmt.Errorf directly instead of errors.New(fmt.Sprintf(...))"
rule:
  pattern: errors.New(fmt.Sprintf($$$ARGS))
```

`rules/go/empty-error-check.yml`:
```yaml
id: empty-error-check
language: Go
severity: warning
message: "Error is checked but silently swallowed. Return or log the error."
rule:
  pattern: |
    if $ERR != nil {
      return $$$RETVALS
    }
  constraints:
    ERR:
      regex: '^err'
    RETVALS:
      not:
        regex: 'err'
```

`rules/go/errors-new-fmt.yml`:
```yaml
id: errors-new-fmt
language: Go
severity: warning
message: "Use fmt.Errorf(\"prefix: %w\", err) instead of errors.New(\"prefix: \" + err.Error())"
rule:
  pattern: errors.New($PREFIX + $ERR.Error())
```

- [ ] **Step 3: Write concurrency rules (4 files)**

`rules/go/mutex-copy.yml`:
```yaml
id: mutex-copy
language: Go
severity: error
message: "sync.Mutex must not be copied. Pass by pointer instead."
rule:
  kind: parameter_declaration
  has:
    kind: qualified_type
    regex: 'sync\.(Mutex|RWMutex)'
  not:
    has:
      kind: pointer_type
```

`rules/go/waitgroup-add-in-goroutine.yml`:
```yaml
id: waitgroup-add-in-goroutine
language: Go
severity: warning
message: "wg.Add() called inside goroutine. Call wg.Add() before launching the goroutine."
rule:
  pattern: $WG.Add($$$)
  inside:
    kind: go_statement
    stopBy: end
```

`rules/go/range-loop-variable-capture.yml`:
```yaml
id: range-loop-variable-capture
language: Go
severity: warning
message: "Loop variable captured by goroutine closure. In Go <1.22 this causes a data race."
rule:
  pattern: |
    go func($$$PARAMS) {
      $$$BODY
    }($$$ARGS)
  inside:
    kind: for_statement
    stopBy: end
```

`rules/go/sync-pool-non-pointer.yml`:
```yaml
id: sync-pool-non-pointer
language: Go
severity: hint
message: "sync.Pool with non-pointer type defeats pooling. Store pointer types."
rule:
  pattern: |
    sync.Pool{
      New: func() $TYPE {
        $$$BODY
      },
    }
  constraints:
    TYPE:
      not:
        regex: '^\*'
```

- [ ] **Step 4: Write security rules (4 files)**

`rules/go/sql-string-concat.yml`:
```yaml
id: sql-string-concat
language: Go
severity: error
message: "SQL query built with string concatenation. Use parameterized queries."
rule:
  any:
    - pattern: $DB.Query($SQL + $$$)
    - pattern: $DB.Exec($SQL + $$$)
    - pattern: $DB.QueryRow($SQL + $$$)
    - pattern: $DB.Query(fmt.Sprintf($$$))
    - pattern: $DB.Exec(fmt.Sprintf($$$))
    - pattern: $DB.QueryRow(fmt.Sprintf($$$))
```

`rules/go/exec-command-variable.yml`:
```yaml
id: exec-command-variable
language: Go
severity: warning
message: "exec.Command with variable command name may allow command injection."
rule:
  pattern: exec.Command($CMD, $$$ARGS)
  constraints:
    CMD:
      not:
        kind: interpreted_string_literal
```

`rules/go/tls-insecure-skip.yml`:
```yaml
id: tls-insecure-skip
language: Go
severity: warning
message: "InsecureSkipVerify disables TLS certificate verification."
rule:
  pattern: 'InsecureSkipVerify: true'
```

`rules/go/bind-all-interfaces.yml`:
```yaml
id: bind-all-interfaces
language: Go
severity: hint
message: "Listening on all interfaces (0.0.0.0). Consider binding to a specific interface."
rule:
  any:
    - pattern: net.Listen($$$, "0.0.0.0:$$$")
    - pattern: http.ListenAndServe(":$PORT", $$$)
```

- [ ] **Step 5: Write anti-pattern rules (5 files)**

`rules/go/defer-in-loop.yml`:
```yaml
id: defer-in-loop
language: Go
severity: warning
message: "defer inside loop body defers until function exit, not loop iteration. Resources accumulate."
rule:
  kind: defer_statement
  inside:
    kind: for_statement
    stopBy: end
```

`rules/go/nil-map-assign.yml`:
```yaml
id: nil-map-assign
language: Go
severity: error
message: "Assignment to nil map causes runtime panic. Initialize with make() first."
rule:
  pattern: |
    var $M map[$K]$V
    $$$
    $M[$KEY] = $VAL
```

`rules/go/http-body-not-closed.yml`:
```yaml
id: http-body-not-closed
language: Go
severity: warning
message: "HTTP response body should be closed with defer resp.Body.Close()"
rule:
  any:
    - pattern: $RESP, $ERR := http.Get($$$)
    - pattern: $RESP, $ERR := $CLIENT.Do($$$)
  not:
    follows:
      pattern: defer $RESP.Body.Close()
```

`rules/go/string-byte-slice-in-loop.yml`:
```yaml
id: string-byte-slice-in-loop
language: Go
severity: hint
message: "Repeated []byte/string conversion in loop. Convert once outside the loop."
rule:
  any:
    - pattern: '[]byte($S)'
    - pattern: string($B)
  inside:
    kind: for_statement
    stopBy: end
```

`rules/go/init-side-effects.yml`:
```yaml
id: init-side-effects
language: Go
severity: hint
message: "init() with I/O side effects makes testing difficult. Consider explicit initialization."
rule:
  kind: function_declaration
  has:
    field: name
    regex: '^init$'
  has:
    any:
      - pattern: http.$METHOD($$$)
      - pattern: os.Open($$$)
      - pattern: net.$METHOD($$$)
```

- [ ] **Step 6: Write test fixtures (one per rule)**

Create `rules/go/tests/ignored-error-return.go`:
```go
package test

import "os"

// Should trigger:
func bad() {
	_, _ = os.Open("file.txt")
}

// Should NOT trigger:
func good() {
	f, err := os.Open("file.txt")
	if err != nil {
		return
	}
	_ = f.Close()
}
```

Create `rules/go/tests/defer-in-loop.go`:
```go
package test

import "os"

// Should trigger:
func bad() {
	for i := 0; i < 10; i++ {
		f, _ := os.Open("file.txt")
		defer f.Close()
	}
}

// Should NOT trigger:
func good() {
	f, _ := os.Open("file.txt")
	defer f.Close()
}
```

Create `rules/go/tests/mutex-copy.go`:
```go
package test

import "sync"

// Should trigger:
func bad(m sync.Mutex) {}

// Should NOT trigger:
func good(m *sync.Mutex) {}
```

Create `rules/go/tests/tls-insecure-skip.go`:
```go
package test

import "crypto/tls"

// Should trigger:
func bad() *tls.Config {
	return &tls.Config{InsecureSkipVerify: true}
}

// Should NOT trigger:
func good() *tls.Config {
	return &tls.Config{InsecureSkipVerify: false}
}
```

Create `rules/go/tests/sql-string-concat.go`:
```go
package test

import "database/sql"

// Should trigger:
func bad(db *sql.DB, user string) {
	db.Query("SELECT * FROM users WHERE name = '" + user + "'")
}

// Should NOT trigger:
func good(db *sql.DB, user string) {
	db.Query("SELECT * FROM users WHERE name = $1", user)
}
```

(Create remaining 13 fixture files following the same pattern — positive and negative cases in each.)

- [ ] **Step 7: Commit**

```bash
git add rules/go/
git commit -m "feat(go): add 18 ast-grep lint rules with test fixtures"
```

---

### Task 3: Symbol Extraction Rules (4 YAML files)

**Files:**
- Create: `rules/go/extraction/exported-functions.yml`
- Create: `rules/go/extraction/exported-methods.yml`
- Create: `rules/go/extraction/exported-structs.yml`
- Create: `rules/go/extraction/exported-interfaces.yml`

- [ ] **Step 1: Create extraction directory**

```bash
mkdir -p rules/go/extraction
```

- [ ] **Step 2: Write extraction rules**

`rules/go/extraction/exported-functions.yml`:
```yaml
id: go-exported-function
language: Go
rule:
  kind: function_declaration
  has:
    field: name
    regex: '^[A-Z]'
```

`rules/go/extraction/exported-methods.yml`:
```yaml
id: go-exported-method
language: Go
rule:
  kind: method_declaration
  has:
    field: name
    regex: '^[A-Z]'
```

`rules/go/extraction/exported-structs.yml`:
```yaml
id: go-exported-struct
language: Go
rule:
  kind: type_declaration
  has:
    kind: type_spec
    has:
      field: name
      regex: '^[A-Z]'
    has:
      kind: struct_type
```

`rules/go/extraction/exported-interfaces.yml`:
```yaml
id: go-exported-interface
language: Go
rule:
  kind: type_declaration
  has:
    kind: type_spec
    has:
      field: name
      regex: '^[A-Z]'
    has:
      kind: interface_type
```

- [ ] **Step 3: Commit**

```bash
git add rules/go/extraction/
git commit -m "feat(go): add symbol extraction rules for exported functions, methods, structs, interfaces"
```

---

### Task 4: go.mod Parsing

**Files:**
- Modify: `src/dep_manifest.rs:1-405`

- [ ] **Step 1: Write failing tests for go.mod parsing**

Add to `mod tests` in `src/dep_manifest.rs`:

```rust
// -- Go module (go.mod) parsing --

#[test]
fn go_mod_single_require_parsed() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "go.mod", "module github.com/example/app\n\ngo 1.21\n\nrequire github.com/gin-gonic/gin v1.9.1\n");
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "github.com/gin-gonic/gin" && d.language == "go"));
}

#[test]
fn go_mod_block_require_parsed() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "go.mod", "module github.com/example/app\n\ngo 1.21\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgithub.com/stretchr/testify v1.8.4 // indirect\n\tgoogle.golang.org/grpc v1.58.0\n)\n");
    let deps = parse_dependencies(dir.path());
    let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"github.com/gin-gonic/gin"));
    assert!(names.contains(&"github.com/stretchr/testify"));
    assert!(names.contains(&"google.golang.org/grpc"));
}

#[test]
fn go_mod_version_suffix_preserved() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "go.mod", "module github.com/example/app\n\nrequire github.com/acme/lib/v2 v2.3.0\n");
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "github.com/acme/lib/v2"), "v2 suffix must be preserved: {:?}", deps);
}

#[test]
fn go_mod_replace_directive_handled() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "go.mod", "module github.com/example/app\n\nrequire github.com/foo/bar v1.0.0\n\nreplace github.com/foo/bar => github.com/my/fork v1.0.1\n");
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "github.com/foo/bar"));
}

#[test]
fn go_mod_indirect_deps_included() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "go.mod", "module example.com/app\n\nrequire (\n\tgithub.com/direct v1.0.0\n\tgithub.com/indirect v2.0.0 // indirect\n)\n");
    let deps = parse_dependencies(dir.path());
    assert_eq!(deps.len(), 2);
}

#[test]
fn go_mod_malformed_returns_empty() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "go.mod", "this is not a valid go.mod\n");
    let deps = parse_dependencies(dir.path());
    let go_deps: Vec<_> = deps.iter().filter(|d| d.language == "go").collect();
    assert!(go_deps.is_empty());
}

#[test]
fn go_work_use_directives_parsed() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "go.work", "go 1.21\n\nuse (\n\t./services/api\n\t./libs/shared\n)\n");
    write(dir.path(), "go.mod", "module github.com/example/mono\n\nrequire github.com/gin-gonic/gin v1.9.1\n");
    let deps = parse_dependencies(dir.path());
    assert!(deps.iter().any(|d| d.name == "github.com/gin-gonic/gin"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum dep_manifest::tests::go_mod -- --nocapture 2>&1 | tail -10`
Expected: FAIL — `parse_go_mod` not defined yet.

- [ ] **Step 3: Implement parse_go_mod**

Add before `pub fn parse_dependencies` in `src/dep_manifest.rs`:

```rust
fn parse_go_mod(path: &Path) -> Vec<Dependency> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    let mut in_require_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("require (") || trimmed == "require (" {
            in_require_block = true;
            continue;
        }
        if in_require_block && trimmed == ")" {
            in_require_block = false;
            continue;
        }

        if in_require_block {
            if let Some(dep) = parse_go_require_line(trimmed) {
                out.push(dep);
            }
        } else if let Some(rest) = trimmed.strip_prefix("require ") {
            let rest = rest.trim();
            if rest.starts_with('(') {
                in_require_block = true;
            } else if let Some(dep) = parse_go_require_line(rest) {
                out.push(dep);
            }
        }
    }
    out
}

fn parse_go_require_line(line: &str) -> Option<Dependency> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("//") {
        return None;
    }
    // Format: module_path version [// indirect]
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 2 {
        let module_path = parts[0];
        // Skip lines that don't look like module paths
        if !module_path.contains('/') && !module_path.contains('.') {
            return None;
        }
        Some(Dependency {
            name: module_path.to_string(),
            language: "go".into(),
        })
    } else {
        None
    }
}
```

- [ ] **Step 4: Wire go.mod into parse_dependencies**

Add to `parse_dependencies` function, after the pyproject/requirements block:

```rust
let go_mod = project_dir.join("go.mod");
if go_mod.exists() {
    out.extend(parse_go_mod(&go_mod));
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum dep_manifest::tests::go_mod -- --nocapture 2>&1 | tail -15`
Expected: All go_mod tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/dep_manifest.rs
git commit -m "feat(go): add go.mod dependency parsing with block require and version suffix support"
```

---

### Task 5: Linter Integration (golangci-lint)

**Files:**
- Modify: `src/linter.rs:15-260`

- [ ] **Step 1: Write failing tests for golangci-lint**

Add to `mod tests` in `src/linter.rs`:

```rust
// -- golangci-lint detection --

#[test]
fn detect_golangcilint_from_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".golangci.yml"), "linters:\n  enable:\n    - errcheck\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Golangcilint));
}

#[test]
fn detect_golangcilint_from_go_mod() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/app\n\ngo 1.21\n").unwrap();
    let linters = detect_linters(dir.path());
    assert!(linters.contains(&LinterKind::Golangcilint));
}

// -- golangci-lint output normalization --

#[test]
fn normalize_golangcilint_valid_output() {
    let json = r#"{"Issues":[{"FromLinter":"errcheck","Text":"Error return value not checked","Severity":"warning","SourceLines":["os.Remove(f)"],"Pos":{"Filename":"main.go","Offset":0,"Line":42,"Column":10}}]}"#;
    let findings = normalize_golangcilint_output(json).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].line_start, 42);
    assert_eq!(findings[0].severity, Severity::Medium);
    assert_eq!(findings[0].source, Source::Linter("golangci-lint".into()));
    assert!(findings[0].title.contains("errcheck"));
}

#[test]
fn normalize_golangcilint_empty_issues() {
    let json = r#"{"Issues":[]}"#;
    let findings = normalize_golangcilint_output(json).unwrap();
    assert!(findings.is_empty());
}

#[test]
fn normalize_golangcilint_null_issues() {
    let json = r#"{"Issues":null}"#;
    let findings = normalize_golangcilint_output(json).unwrap();
    assert!(findings.is_empty());
}

#[test]
fn normalize_golangcilint_error_severity() {
    let json = r#"{"Issues":[{"FromLinter":"govet","Text":"unreachable code","Severity":"error","Pos":{"Filename":"main.go","Line":5,"Column":1}}]}"#;
    let findings = normalize_golangcilint_output(json).unwrap();
    assert_eq!(findings[0].severity, Severity::High);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum linter::tests::detect_golangcilint -- --nocapture 2>&1 | tail -5`
Expected: FAIL — `LinterKind::Golangcilint` not defined.

- [ ] **Step 3: Add LinterKind::Golangcilint variant**

Add `Golangcilint` to the `LinterKind` enum and its `name()` method:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinterKind {
    Ruff,
    Clippy,
    Eslint,
    Yamllint,
    Shellcheck,
    Hadolint,
    Tflint,
    Golangcilint,
}

impl LinterKind {
    pub fn name(&self) -> &'static str {
        match self {
            // ... existing arms ...
            LinterKind::Golangcilint => "golangci-lint",
        }
    }
}
```

- [ ] **Step 4: Add detection logic**

Add to `detect_linters` function (after tflint block):

```rust
// golangci-lint: .golangci.yml/.yaml/.toml/.json or go.mod
let golangci_configs = [".golangci.yml", ".golangci.yaml", ".golangci.toml", ".golangci.json"];
let has_golangci_config = golangci_configs.iter().any(|c| project_dir.join(c).exists());
if has_golangci_config || project_dir.join("go.mod").exists() {
    linters.push(LinterKind::Golangcilint);
}
```

- [ ] **Step 5: Add invocation and normalization**

Add to `run_linter` match:

```rust
LinterKind::Golangcilint => {
    runner.run("golangci-lint", &["run", "--out-format=json", &file_str], cwd)?
}
```

Add to normalization match:

```rust
LinterKind::Golangcilint => normalize_golangcilint_output(&output.stdout),
```

Add the normalization function:

```rust
pub fn normalize_golangcilint_output(json_output: &str) -> anyhow::Result<Vec<Finding>> {
    let wrapper: serde_json::Value = serde_json::from_str(json_output)?;
    let issues = wrapper.get("Issues").and_then(|i| i.as_array());
    let mut findings = Vec::new();

    if let Some(items) = issues {
        for item in items {
            let from_linter = item["FromLinter"].as_str().unwrap_or("unknown");
            let message = item["Text"].as_str().unwrap_or("");
            let severity_str = item["Severity"].as_str().unwrap_or("warning");
            let line = item["Pos"]["Line"].as_u64().unwrap_or(1) as u32;
            let col = item["Pos"]["Column"].as_u64().unwrap_or(1) as u32;

            let severity = match severity_str {
                "error" => Severity::High,
                "warning" => Severity::Medium,
                _ => Severity::Low,
            };

            findings.push(Finding {
                id: crate::finding::new_finding_ulid(),
                title: format!("{}: {}", from_linter, message),
                description: message.to_string(),
                severity,
                category: "lint".into(),
                source: Source::Linter("golangci-lint".into()),
                line_start: line,
                line_end: line,
                evidence: vec![format!("golangci-lint/{} col:{}", from_linter, col)],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
                suggested_fix: None,
                based_on_excerpt: None,
                reasoning: None,
                llm_confidence: None,
                confidence: None,
                cited_lines: None,
                grounding_status: None,
                grounding_confidence: None,
                model_agreement: None,
                rule_id: None,
                judge_verdict: None,
                judge_confidence: None,
                precision_tier: None,
                in_diff: None,
            });
        }
    }

    Ok(findings)
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --bin quorum linter::tests -- --nocapture 2>&1 | grep -E "(PASS|FAIL|test result)"`
Expected: All linter tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/linter.rs
git commit -m "feat(go): add golangci-lint integration with detection, invocation, and output parsing"
```

---

### Task 6: Fingerprinter

**Files:**
- Create: `src/context/extract/fingerprint_go.rs`
- Modify: `src/context/extract/mod.rs`

- [ ] **Step 1: Create fingerprint_go.rs**

```rust
//! Go AST structural fingerprinter.
//!
//! Extracts structural features from Go functions and methods, producing a
//! StructuralFingerprint for similarity search.

use ast_grep_core::Doc;
use ast_grep_language::{LanguageExt, SupportLang};

use super::fingerprint::{
    ControlFlowSketch, MIN_BODY_NODE_COUNT, SemanticCounts, SignatureShape, StructuralFingerprint,
    TypeCategory,
};

pub struct GoFingerprinter;

impl GoFingerprinter {
    pub fn fingerprint_source(&self, src: &str) -> Option<StructuralFingerprint> {
        let root = SupportLang::Go.ast_grep(src);
        let root_node = root.root();
        let func_node = root_node.dfs().find(|n| {
            let k = n.kind();
            k.as_ref() == "function_declaration" || k.as_ref() == "method_declaration"
        })?;
        self.fingerprint_node(&func_node, src)
    }

    pub fn fingerprint_all_functions(&self, src: &str) -> Vec<(String, StructuralFingerprint)> {
        let root = SupportLang::Go.ast_grep(src);
        let root_node = root.root();
        let func_nodes: Vec<_> = root_node
            .dfs()
            .filter(|n| {
                let k = n.kind();
                k.as_ref() == "function_declaration"
                    || k.as_ref() == "method_declaration"
            })
            .collect();
        let mut results = Vec::new();
        for node in &func_nodes {
            let name = node
                .children()
                .find(|c| c.kind().as_ref() == "identifier" || c.kind().as_ref() == "field_identifier")
                .map(|c| c.text().into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            if let Some(fp) = self.fingerprint_node(node, src) {
                results.push((name, fp));
            }
        }
        results
    }

    pub fn fingerprint_node<'a, D: Doc>(
        &self,
        node: &'a ast_grep_core::Node<'a, D>,
        _source: &str,
    ) -> Option<StructuralFingerprint> {
        let kind = node.kind();
        let kind_str = kind.as_ref();
        if kind_str != "function_declaration" && kind_str != "method_declaration" {
            return None;
        }

        let body = node.children().find(|c| c.kind().as_ref() == "block")?;
        let body_node_count = body.dfs().count();
        if body_node_count < MIN_BODY_NODE_COUNT {
            return None;
        }

        let signature = extract_go_signature(node);
        let control_flow = extract_go_control_flow(&body);
        let semantic_counts = extract_go_semantic_counts(&body);

        Some(StructuralFingerprint {
            signature,
            control_flow,
            semantic_counts,
        })
    }
}

fn extract_go_signature<'a, D: Doc>(node: &'a ast_grep_core::Node<'a, D>) -> SignatureShape {
    let mut shape = SignatureShape::default();
    let kind = node.kind();
    shape.is_method = kind.as_ref() == "method_declaration";

    if let Some(params) = node.children().find(|c| c.kind().as_ref() == "parameter_list") {
        shape.arity = params
            .children()
            .filter(|c| c.kind().as_ref() == "parameter_declaration")
            .count() as u8;
    }

    if let Some(result) = node.children().find(|c| {
        c.kind().as_ref() == "parameter_list" || c.kind().as_ref() == "type_identifier"
    }) {
        if result.kind().as_ref() == "type_identifier" {
            let text = result.text();
            shape.return_category = Some(TypeCategory::classify_go(text.as_ref()));
        }
    }

    shape
}

fn extract_go_control_flow<D: Doc>(body: &ast_grep_core::Node<'_, D>) -> ControlFlowSketch {
    let mut cf = ControlFlowSketch::default();
    for descendant in body.dfs() {
        let dk = descendant.kind();
        let kind = dk.as_ref();
        match kind {
            "if_statement" => cf.branches += 1,
            "for_statement" => cf.loops += 1,
            "return_statement" => cf.early_returns += 1,
            "go_statement" => cf.awaits += 1,
            "defer_statement" => cf.closures += 1,
            "select_statement" => cf.match_arms += 1,
            "expression_case" => cf.match_arms += 1,
            "type_case" => cf.match_arms += 1,
            _ => {}
        }
    }
    cf
}

fn extract_go_semantic_counts<D: Doc>(body: &ast_grep_core::Node<'_, D>) -> SemanticCounts {
    let mut sc = SemanticCounts::default();
    for descendant in body.dfs() {
        let dk = descendant.kind();
        let kind = dk.as_ref();
        match kind {
            "call_expression" => sc.calls += 1,
            "assignment_statement" => sc.assignments += 1,
            "short_var_declaration" => sc.assignments += 1,
            "selector_expression" => sc.member_access += 1,
            "index_expression" => sc.index_ops += 1,
            "binary_expression" => sc.binary_ops += 1,
            "composite_literal" => sc.collection_literals += 1,
            "func_literal" => sc.lambdas += 1,
            _ => {}
        }
    }
    sc
}
```

- [ ] **Step 2: Add TypeCategory::classify_go**

In `src/context/extract/fingerprint.rs`, add a `classify_go` method to `TypeCategory`:

```rust
pub fn classify_go(name: &str) -> Self {
    match name {
        "error" => Self::Res,
        "bool" => Self::Primitive,
        "int" | "int8" | "int16" | "int32" | "int64" => Self::Primitive,
        "uint" | "uint8" | "uint16" | "uint32" | "uint64" => Self::Primitive,
        "float32" | "float64" => Self::Primitive,
        "string" => Self::Str,
        "byte" | "rune" => Self::Primitive,
        _ => Self::Named,
    }
}
```

- [ ] **Step 3: Register module in mod.rs**

Add to `src/context/extract/mod.rs`:

```rust
pub mod fingerprint_go;
```

And add test module:

```rust
#[cfg(test)]
mod fingerprint_go_tests;
```

- [ ] **Step 4: Create basic test file**

Create `src/context/extract/fingerprint_go_tests.rs`:

```rust
use super::fingerprint_go::GoFingerprinter;

#[test]
fn fingerprint_go_simple_function() {
    let src = r#"package main

import "fmt"

func processItems(items []string) error {
    for _, item := range items {
        if item == "" {
            continue
        }
        fmt.Println(item)
    }
    return nil
}
"#;
    let fp = GoFingerprinter.fingerprint_source(src);
    assert!(fp.is_some(), "non-trivial Go function should produce a fingerprint");
    let fp = fp.unwrap();
    assert!(fp.control_flow.loops >= 1);
    assert!(fp.control_flow.branches >= 1);
}

#[test]
fn fingerprint_go_trivial_function_skipped() {
    let src = "package main\n\nfunc trivial() int { return 42 }\n";
    let fp = GoFingerprinter.fingerprint_source(src);
    assert!(fp.is_none(), "trivial function should be skipped");
}

#[test]
fn fingerprint_go_method() {
    let src = r#"package main

type Server struct{ port int }

func (s *Server) Start() error {
    if s.port == 0 {
        s.port = 8080
    }
    listener, err := net.Listen("tcp", fmt.Sprintf(":%d", s.port))
    if err != nil {
        return err
    }
    defer listener.Close()
    for {
        conn, err := listener.Accept()
        if err != nil {
            return err
        }
        go s.handleConn(conn)
    }
}
"#;
    let results = GoFingerprinter.fingerprint_all_functions(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "Start");
    assert!(results[0].1.signature.is_method);
}
```

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo test --bin quorum fingerprint_go -- --nocapture 2>&1 | tail -10`
Expected: All fingerprint_go tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/context/extract/fingerprint_go.rs src/context/extract/fingerprint_go_tests.rs src/context/extract/mod.rs src/context/extract/fingerprint.rs
git commit -m "feat(go): add Go structural fingerprinter for function similarity search"
```

---

### Task 7: AST Extractor (astgrep_go.rs)

**Files:**
- Create: `src/context/extract/astgrep_go.rs`
- Create: `src/context/extract/astgrep_go_tests.rs`
- Modify: `src/context/extract/mod.rs`

**Depends on:** Task 3 (extraction rules must exist for `include_str!`)

- [ ] **Step 1: Create astgrep_go.rs**

```rust
//! Go symbol extractor via ast-grep.
//!
//! Emits one Chunk of kind Symbol per exported func, method, struct, or interface.

use ast_grep_config::{GlobalRules, RuleConfig, from_yaml_string};
use ast_grep_language::{LanguageExt, SupportLang};
use chrono::{DateTime, Utc};

use super::super::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

const RULE_YAMLS: &[&str] = &[
    include_str!("../../../rules/go/extraction/exported-functions.yml"),
    include_str!("../../../rules/go/extraction/exported-methods.yml"),
    include_str!("../../../rules/go/extraction/exported-structs.yml"),
    include_str!("../../../rules/go/extraction/exported-interfaces.yml"),
];

fn load_extraction_rules() -> anyhow::Result<Vec<RuleConfig<SupportLang>>> {
    let globals = GlobalRules::default();
    let mut rules = Vec::with_capacity(RULE_YAMLS.len());
    for yaml in RULE_YAMLS {
        let parsed = from_yaml_string::<SupportLang>(yaml, &globals)
            .map_err(|e| anyhow::anyhow!("failed to parse bundled go extraction rule: {e}"))?;
        rules.extend(parsed);
    }
    Ok(rules)
}

pub fn extract_go(
    src: &str,
    source_path: &str,
    source: &str,
    commit_sha: &str,
    indexed_at: DateTime<Utc>,
) -> anyhow::Result<Vec<Chunk>> {
    if src.is_empty() {
        return Ok(Vec::new());
    }

    let rules = load_extraction_rules()?;
    let root = SupportLang::Go.ast_grep(src);

    let mut raw: Vec<(usize, String, u32, u32, String)> = Vec::new();

    for rule in &rules {
        for m in root.root().find_all(&rule.matcher) {
            let node = m.get_node();
            let name = node
                .children()
                .find(|c| {
                    let k = c.kind();
                    k.as_ref() == "identifier" || k.as_ref() == "field_identifier" || k.as_ref() == "type_spec"
                })
                .map(|c| {
                    // For type_spec, get the name child
                    if c.kind().as_ref() == "type_spec" {
                        c.children()
                            .find(|cc| cc.kind().as_ref() == "type_identifier")
                            .map(|cc| cc.text().into_owned())
                            .unwrap_or_else(|| c.text().into_owned())
                    } else {
                        c.text().into_owned()
                    }
                })
                .unwrap_or_default();

            if name.is_empty() {
                continue;
            }

            let byte_start = node.range().start;
            let start_line = (node.start_pos().line() as u32) + 1;
            let end_line = (node.end_pos().line() as u32) + 1;
            let item_text = &src[node.range()];
            let signature = go_item_signature(item_text);

            raw.push((byte_start, name, start_line, end_line, signature));
        }
    }

    raw.sort_by_key(|s| s.0);
    raw.dedup_by_key(|s| (s.1.clone(), s.0));

    let all_names: Vec<String> = raw.iter().map(|s| s.1.clone()).collect();

    let chunks: Vec<Chunk> = raw
        .into_iter()
        .map(|(byte_start, name, start_line, end_line, signature)| {
            let neighboring_symbols: Vec<String> = all_names
                .iter()
                .filter(|n| **n != name)
                .cloned()
                .collect();

            let id = format!("{source}:{source_path}:{name}");

            Chunk {
                id,
                source: source.to_string(),
                kind: ChunkKind::Symbol,
                subtype: None,
                qualified_name: Some(name.clone()),
                signature: Some(signature.clone()),
                content: signature,
                metadata: ChunkMeta {
                    source_path: source_path.to_string(),
                    line_range: LineRange::new(start_line, end_line)
                        .expect("astgrep-go extractor produced invalid line range"),
                    commit_sha: commit_sha.to_string(),
                    indexed_at,
                    source_version: None,
                    language: Some("go".to_string()),
                    is_exported: true,
                    neighboring_symbols,
                },
                provenance: Provenance::new("ast-grep-go", 0.9, source_path.to_string())
                    .expect("ast-grep-go extractor produced invalid provenance"),
            }
        })
        .collect();

    Ok(chunks)
}

fn go_item_signature(item_text: &str) -> String {
    let end = item_text.find('{').unwrap_or(item_text.len());
    let raw = &item_text[..end];
    let mut out = String::with_capacity(raw.len());
    let mut prev_ws = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}
```

- [ ] **Step 2: Add module to mod.rs**

Add to `src/context/extract/mod.rs`:

```rust
pub mod astgrep_go;
```

And test module:

```rust
#[cfg(test)]
mod astgrep_go_tests;
```

- [ ] **Step 3: Create test file**

Create `src/context/extract/astgrep_go_tests.rs`:

```rust
use super::astgrep_go::extract_go;
use chrono::Utc;

#[test]
fn extract_go_exported_functions() {
    let src = r#"package main

func PublicFunc() {}
func privateFunc() {}
func AnotherPublic(x int) error { return nil }
"#;
    let chunks = extract_go(src, "main.go", "test", "abc123", Utc::now()).unwrap();
    let names: Vec<_> = chunks.iter().filter_map(|c| c.qualified_name.as_deref()).collect();
    assert!(names.contains(&"PublicFunc"));
    assert!(names.contains(&"AnotherPublic"));
    assert!(!names.contains(&"privateFunc"));
}

#[test]
fn extract_go_exported_methods() {
    let src = r#"package main

type Server struct{}

func (s *Server) Start() error { return nil }
func (s *Server) stop() {}
"#;
    let chunks = extract_go(src, "server.go", "test", "abc123", Utc::now()).unwrap();
    let names: Vec<_> = chunks.iter().filter_map(|c| c.qualified_name.as_deref()).collect();
    assert!(names.contains(&"Start"));
    assert!(!names.contains(&"stop"));
}

#[test]
fn extract_go_empty_file() {
    let chunks = extract_go("", "empty.go", "test", "abc123", Utc::now()).unwrap();
    assert!(chunks.is_empty());
}
```

- [ ] **Step 4: Verify compilation and tests**

Run: `cargo test --bin quorum astgrep_go -- --nocapture 2>&1 | tail -10`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/context/extract/astgrep_go.rs src/context/extract/astgrep_go_tests.rs src/context/extract/mod.rs
git commit -m "feat(go): add Go symbol extractor via ast-grep extraction rules"
```

---

### Task 8: Dispatch Wiring + Context7 Enrichment

**Files:**
- Modify: `src/context/extract/dispatch.rs:14-431`
- Modify: `src/context_enrichment.rs:65-433`

**Depends on:** Tasks 6, 7

- [ ] **Step 1: Wire Go into dispatch.rs**

Add import at top of `dispatch.rs`:

```rust
use super::astgrep_go::extract_go;
use super::fingerprint_go::GoFingerprinter;
```

Add `Go` to `FileKind` enum:

```rust
#[derive(Debug, Clone, Copy)]
enum FileKind {
    Rust,
    Typescript,
    Python,
    Hcl,
    Go,
    Markdown,
    Unknown,
}
```

Add `.go` to `classify`:

```rust
Some("go") => FileKind::Go,
```

Add dispatch arm in `extract_source_inner` (in the match on `dispatched`):

```rust
FileKind::Go => {
    extract_go(&src_text, &rel, &source.name, UNVERSIONED_SHA, indexed_at)
}
```

Add to `compute_fingerprints_for_file`:

```rust
FileKind::Go => "go",
```

Add to `compute_source_fingerprints`:

```rust
"go" => GoFingerprinter.fingerprint_all_functions(src),
```

- [ ] **Step 2: Add Go import normalization to context_enrichment.rs**

Add a Go handler in `normalize_import_to_dep_names`. After the TS/Python handlers in the hydration-form block, and update `normalize_clean` to handle Go full-path imports:

In the hydration-form block (after `parse_python_import`), add handling for Go imports:

The existing `normalize_clean` already handles Go-style paths because `"github.com/gin-gonic/gin"` split on `['.','/']` yields `"github"` — but this is wrong for Go. We need a Go-specific path that returns the full module path. 

Add a new function for Go import matching that will be used by the enrichment pipeline:

```rust
/// Match a Go import path to a module from go.mod using longest-prefix matching.
/// Returns the matching module path, or None if no match.
pub fn match_go_import_to_dep<'a>(
    import_path: &str,
    deps: &'a [crate::dep_manifest::Dependency],
) -> Option<&'a crate::dep_manifest::Dependency> {
    deps.iter()
        .filter(|d| d.language == "go")
        .filter(|d| import_path == d.name || import_path.starts_with(&format!("{}/", d.name)))
        .max_by_key(|d| d.name.len())
}
```

- [ ] **Step 3: Add Go curated queries to curated_query_for**

Add Go framework entries to the match in `curated_query_for` (line 416-431):

```rust
"gin" | "github.com/gin-gonic/gin" => "gin HTTP router middleware handlers",
"echo" | "github.com/labstack/echo" => "echo HTTP framework middleware context",
"fiber" | "github.com/gofiber/fiber" => "fiber HTTP framework middleware",
"cobra" | "github.com/spf13/cobra" => "cobra CLI command flags arguments",
"viper" | "github.com/spf13/viper" => "viper configuration binding environment",
"gorm" | "gorm.io/gorm" => "gorm ORM model associations migrations",
"sqlx" | "github.com/jmoiron/sqlx" => "sqlx database query named parameters",
"grpc" | "google.golang.org/grpc" => "gRPC server client interceptors streaming",
"zap" | "go.uber.org/zap" => "zap structured logging fields",
"logrus" | "github.com/sirupsen/logrus" => "logrus structured logging hooks",
"testify" | "github.com/stretchr/testify" => "testify assert require mock suite",
"chi" | "github.com/go-chi/chi" => "chi router middleware context",
"mux" | "github.com/gorilla/mux" => "gorilla mux router variables middleware",
"wire" | "github.com/google/wire" => "wire dependency injection providers",
"protobuf" | "google.golang.org/protobuf" => "protobuf generated code message serialization",
```

- [ ] **Step 4: Add Go to generic_query_for_language**

```rust
"go" => "common pitfalls error handling concurrency goroutine safety",
```

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/context/extract/dispatch.rs src/context_enrichment.rs
git commit -m "feat(go): wire Go dispatch, fingerprinting, and Context7 enrichment with longest-prefix import matching"
```

---

### Task 9: Integration Test + Final Validation

**Files:**
- Create: `tests/fixtures/go_sample.go` (or add to existing integration test pattern)
- Run full test suite and validate

- [ ] **Step 1: Create Go sample file for integration testing**

Create `tests/fixtures/context/repos/mini-go/main.go`:

```go
package main

import (
	"fmt"
	"net/http"
)

// Server handles HTTP requests.
type Server struct {
	port int
}

// NewServer creates a new Server instance.
func NewServer(port int) *Server {
	return &Server{port: port}
}

// Start begins listening for connections.
func (s *Server) Start() error {
	addr := fmt.Sprintf(":%d", s.port)
	return http.ListenAndServe(addr, nil)
}

func helper() {
	fmt.Println("unexported helper")
}
```

- [ ] **Step 2: Run full cargo test**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: All tests pass, zero failures.

- [ ] **Step 3: Run cargo clippy**

Run: `cargo clippy --all-targets 2>&1 | grep -E "(warning|error)" | head -20`
Expected: No new warnings from Go code.

- [ ] **Step 4: Manual smoke test**

Run: `cargo run -- review tests/fixtures/context/repos/mini-go/main.go --skip-context7 2>&1 | head -30`
Expected: Review runs without error, shows findings (or clean result).

- [ ] **Step 5: Commit and verify**

```bash
git add tests/fixtures/context/repos/mini-go/
git commit -m "feat(go): add integration test fixture and verify full pipeline"
```

- [ ] **Step 6: Final commit — version bump (optional)**

If all tests pass and the feature is complete:

```bash
# Bump version in Cargo.toml to 0.26.0
git add Cargo.toml
git commit -m "chore: bump version to 0.26.0 for Go language support"
```

---

## Summary

| Phase | Tasks | Parallelizable | Estimated Steps |
|-------|-------|----------------|-----------------|
| 1 | Task 1 (foundation) | No | 6 |
| 2 | Tasks 2-7 | **Yes — all 6 in parallel** | 37 |
| 3 | Task 8 (wiring) | No | 6 |
| 4 | Task 9 (integration) | No | 6 |

Total: 55 steps across 9 tasks. Phase 2 is the bulk and is fully parallelizable.
