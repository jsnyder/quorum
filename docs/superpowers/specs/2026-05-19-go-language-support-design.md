# Go Language Support — Design Spec

**Date:** 2026-05-19
**Approach:** Full pipeline integration (Approach A)
**Review:** GPT-5.4 + Gemini 2.5 Pro design review completed; all findings incorporated

## Overview

Add Go as a first-class language in quorum with the same depth as Python/TypeScript: parser registration, ~18 ast-grep rules, golangci-lint integration, symbol extraction, fingerprinting, go.mod dependency parsing with full module metadata, Context7 framework enrichment with longest-prefix import matching, and comprehensive tests.

## 1. Parser & Language Registration

### Language Enum (`src/parser.rs`)

Add `Go` variant to `Language` enum.

- **Extension mapping:** `"go" => Some(Language::Go)`
- **Tree-sitter binding:** `Language::Go => tree_sitter_go::LANGUAGE.into()`
- **Function node kinds:** `Language::Go => &["function_declaration", "method_declaration"]`
  - `method_declaration` is required for Go methods with receivers (`func (s *Server) Serve()`)

### Cargo.toml Dependencies

- Add `tree-sitter-go = "0.23"` (or latest compatible)
- Add `"tree-sitter-go"` feature to `ast-grep-language`

### FileKind Dispatch (`src/context/extract/dispatch.rs`)

- Add `Go` variant to `FileKind` enum
- Map `.go` extension to `FileKind::Go`
- Add match arm calling `extract_go()` from new `astgrep_go` module
- Import `GoFingerprinter` from new `fingerprint_go` module

## 2. AST Rules (18 rules)

All rules in `rules/go/` using ast-grep YAML format. Test fixtures in `rules/go/tests/`.

### Error Handling (5)

| Rule ID | Pattern | Severity |
|---------|---------|----------|
| `ignored-error-return` | Function call returning error where error is assigned to `_` or discarded | warning |
| `bare-error-format` | `fmt.Errorf("...")` without `%w` verb (loses error chain) | warning |
| `error-sprintf` | `errors.New(fmt.Sprintf(...))` instead of `fmt.Errorf(...)` | hint |
| `empty-error-check` | `if err != nil { return nil }` or `if err != nil { return }` (swallows error) | warning |
| `errors-new-fmt` | `errors.New("prefix: " + err.Error())` instead of `fmt.Errorf("prefix: %w", err)` | warning |

### Concurrency (4)

| Rule ID | Pattern | Severity |
|---------|---------|----------|
| `mutex-copy` | `sync.Mutex` or `sync.RWMutex` passed by value (copies lock state) | error |
| `waitgroup-add-in-goroutine` | `wg.Add(N)` called inside `go func()` instead of before launch | warning |
| `range-loop-variable-capture` | Loop variable captured by closure in `go func()` or deferred call (Go <1.22) | warning |
| `sync-pool-non-pointer` | `sync.Pool` storing non-pointer types (defeats pooling) | hint |

### Security (4)

| Rule ID | Pattern | Severity |
|---------|---------|----------|
| `sql-string-concat` | String concatenation or `fmt.Sprintf` in SQL query arguments | error |
| `exec-command-variable` | `exec.Command` with non-literal first argument | warning |
| `tls-insecure-skip` | `InsecureSkipVerify: true` in TLS config | warning |
| `bind-all-interfaces` | `net.Listen` or `http.ListenAndServe` on `0.0.0.0` or `:port` | hint |

### Anti-patterns (5)

| Rule ID | Pattern | Severity |
|---------|---------|----------|
| `defer-in-loop` | `defer` statement inside `for` loop body | warning |
| `nil-map-assign` | Assignment to map without prior `make()` (runtime panic) | error |
| `http-body-not-closed` | `http.Get`/`client.Do` without `defer resp.Body.Close()` | warning |
| `string-byte-slice-in-loop` | Repeated `[]byte(s)` or `string(b)` conversions inside loop | hint |
| `init-side-effects` | `init()` function containing `http.`, `os.Open`, `net.` calls | hint |

### Dropped Rules (from original design, per review)

The following were dropped because they require scope/dataflow analysis beyond ast-grep YAML:
- `shadow-err` — requires scope tracking across nested blocks
- `goroutine-leak-no-ctx` — requires analyzing goroutine lifetime and cancellation propagation
- `unbuffered-channel-in-loop` — requires combining channel type analysis with loop context
- `context-todo-production` — reliably distinguishing main vs non-main is not syntax-local

These patterns are better caught by golangci-lint (via `errcheck`, `govet`, `contextcheck` linters) or LLM review.

## 3. Symbol Extraction Rules

In `rules/go/extraction/`:

| Rule File | Pattern | Description |
|-----------|---------|-------------|
| `exported-functions.yml` | `func $NAME(` where Name is capitalized | Public free functions |
| `exported-methods.yml` | `func ($RECV) $NAME(` where Name is capitalized | Public methods |
| `exported-structs.yml` | `type $NAME struct` where Name is capitalized | Public struct types |
| `exported-interfaces.yml` | `type $NAME interface` where Name is capitalized | Public interface types |

## 4. Linter Integration (`src/linter.rs`)

### LinterKind

Add `Golangcilint` variant. `name()` returns `"golangci-lint"`.

### Detection

Check for `.golangci.yml`, `.golangci.yaml`, `.golangci.toml`, `.golangci.json`, or `go.mod` in project root.

### Invocation

```
golangci-lint run --out-format=json <file>
```

### Output Parsing

Parse golangci-lint JSON schema:
```json
{
  "Issues": [{
    "FromLinter": "errcheck",
    "Text": "Error return value not checked",
    "Severity": "warning",
    "Pos": { "Filename": "main.go", "Line": 42, "Column": 10 }
  }]
}
```

Map `FromLinter` to finding source, `Severity` to quorum severity, extract file/line/column.

## 5. Context7 Enrichment

### go.mod Parsing (`src/dep_manifest.rs`)

Parse into a richer structure than flat `Vec<Dependency>`:

```rust
struct GoModuleMeta {
    module_path: Option<String>,         // `module` directive
    go_version: Option<String>,          // `go` directive
    requires: Vec<GoRequire>,            // `require` entries
    replaces: Vec<(String, String)>,     // `replace` old => new
}

struct GoRequire {
    module_path: String,                 // e.g. "github.com/gin-gonic/gin"
    version: String,                     // e.g. "v1.9.1"
    indirect: bool,                      // // indirect comment
}
```

Handle:
- Single-line `require`: `require github.com/foo/bar v1.2.3`
- Block `require`: `require ( ... )`
- `replace` directives (map replaced module to replacement for import matching)
- `/v2`, `/v3` major version suffixes as part of module identity (do not strip)
- `// indirect` comment marker

**go.work (workspace mode):** Parse `go.work` when present to discover `use` directives pointing to local modules. Treat workspace modules as additional dependency sources. This is important for monorepos.

### Import Matching

Go imports are full module paths. Use **longest-prefix matching** against go.mod require paths:

```rust
fn match_go_import_to_module<'a>(
    import_path: &str,
    requires: &'a [GoRequire],
) -> Option<&'a GoRequire> {
    requires.iter()
        .filter(|r| import_path == r.module_path
                  || import_path.starts_with(&format!("{}/", r.module_path)))
        .max_by_key(|r| r.module_path.len())
}
```

Examples:
- `"github.com/gin-gonic/gin/middleware"` matches `"github.com/gin-gonic/gin"`
- `"google.golang.org/grpc/status"` matches `"google.golang.org/grpc"`
- `"github.com/acme/lib/v2/foo"` matches `"github.com/acme/lib/v2"`

### Curated Context7 Queries

Add entries to `curated_query_for()`:

| Framework | Query |
|-----------|-------|
| gin | "gin HTTP router middleware handlers" |
| echo | "echo HTTP framework middleware context" |
| fiber | "fiber HTTP framework middleware" |
| cobra | "cobra CLI command flags arguments" |
| viper | "viper configuration binding environment" |
| gorm | "gorm ORM model associations migrations" |
| sqlx | "sqlx database query named parameters" |
| grpc-go | "gRPC server client interceptors streaming" |
| zap | "zap structured logging fields" |
| logrus | "logrus structured logging hooks" |
| testify | "testify assert require mock suite" |
| chi | "chi router middleware context" |
| gorilla/mux | "gorilla mux router variables middleware" |
| wire | "wire dependency injection providers" |
| protobuf | "protobuf generated code message serialization" |

### Generic Query Fallback

Add Go to `generic_query_for_language()`:
```
"Go package API usage patterns error handling"
```

## 6. Fingerprinting (`src/context/extract/fingerprint_go.rs`)

Create `GoFingerprinter` struct implementing the same pattern as `RustFingerprinter`:

- Parse Go source via `SupportLang::Go.ast_grep(src)`
- Extract `function_declaration`, `method_declaration`, `type_declaration` nodes
- Compute structural fingerprint: signature tokens, control-flow depth, semantic counts
- Apply same threshold-based filtering as existing fingerprinters

Include `type_declaration` nodes to capture type aliases and interface definitions.

## 7. AST Extractor (`src/context/extract/astgrep_go.rs`)

Follow the `astgrep_rust.rs` pattern:

```rust
const RULE_YAMLS: &[&str] = &[
    include_str!("../../../rules/go/extraction/exported-functions.yml"),
    include_str!("../../../rules/go/extraction/exported-methods.yml"),
    include_str!("../../../rules/go/extraction/exported-structs.yml"),
    include_str!("../../../rules/go/extraction/exported-interfaces.yml"),
];

pub fn extract_go(
    src: &str,
    source_path: &str,
    source: &str,
    commit_sha: &str,
    indexed_at: DateTime<Utc>,
) -> anyhow::Result<Vec<Chunk>> { ... }
```

Emit `Chunk` with `ChunkKind::Symbol` for each extracted item.

## 8. Testing

### Unit Tests

- `Language::from_extension("go")` returns `Some(Language::Go)`
- `Language::Go.tree_sitter_language()` returns valid grammar
- `Language::Go.function_node_kinds()` returns `["function_declaration", "method_declaration"]`

### AST Rule Fixtures

Each of the 18 rules gets positive and negative test fixtures in `rules/go/tests/`:
- `ignored-error-return.go` — code that should and should not trigger
- etc.

### go.mod Parsing Tests

- Single-line require
- Block require with multiple entries
- `// indirect` marker handling
- `replace` directives (local path and module replacements)
- `/v2` major version suffix preserved
- `module` directive extraction
- `go.work` workspace file with `use` directives
- Malformed go.mod (graceful failure)

### golangci-lint Output Parsing Tests

- Standard JSON output with multiple issues
- Empty issues array
- Missing optional fields

### Integration Test

- Review a `.go` file end-to-end with AST rules + LLM review
- Verify findings include ast-grep rule IDs
- Verify golangci-lint findings when linter is available

## 9. Files to Create/Modify

### New Files (7)
- `src/context/extract/astgrep_go.rs` — Go symbol extractor
- `src/context/extract/fingerprint_go.rs` — Go structural fingerprinter
- `rules/go/extraction/exported-functions.yml`
- `rules/go/extraction/exported-methods.yml`
- `rules/go/extraction/exported-structs.yml`
- `rules/go/extraction/exported-interfaces.yml`
- 18 AST rule YAML files in `rules/go/`
- Test fixture `.go` files in `rules/go/tests/`

### Modified Files (~8)
- `Cargo.toml` — tree-sitter-go dep, ast-grep-language feature
- `src/parser.rs` — Language::Go variant, extension, tree-sitter, node kinds
- `src/linter.rs` — LinterKind::Golangcilint, detection, invocation, output parsing
- `src/dep_manifest.rs` — parse_go_mod(), GoModuleMeta struct
- `src/context/extract/dispatch.rs` — FileKind::Go, dispatch match arm
- `src/context/extract/mod.rs` — module declarations for astgrep_go, fingerprint_go
- `src/context_enrichment.rs` — Go import normalization, curated queries, generic query
- `src/main.rs` or `src/analysis.rs` — wire Go linter into analysis pipeline (if needed)

## 10. Out of Scope

- `go vet` integration (golangci-lint already runs it)
- `go.sum` integrity verification (not relevant to code review)
- Go workspace cross-module analysis (single-file review scope)
- Go generics-specific rules (low signal-to-noise for v1)
