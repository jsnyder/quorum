# quorum context Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build `quorum context` — a local/offline retrieval layer that indexes registered private repos / internal services / custom Terraform modules into a hybrid SQLite store and auto-injects relevant chunks into LLM code reviews.

**Architecture:** Extractors (ast-grep + tree-sitter + markdown splitter) → plain JSONL chunk store (source of truth) → derived SQLite index (FTS5 + sqlite-vec) → hybrid retrieval with additive rerank → adaptive threshold planner → markdown renderer → injected into existing review prompt. Retrieval-first, no graph DB, no SCIP.

**Tech Stack:** Rust, `rusqlite` (FTS5 bundled), `sqlite-vec`, `fastembed` (reused from feedback), `ast-grep-core 0.42.1` (already in-tree), `serde_json`, `git2` or `gix` (whichever quorum already uses).

**Design reference:** `docs/plans/2026-04-20-quorum-context-design.md`
**Test boundaries reference:** `docs/plans/tdd-inputs/2026-04-20-context-testing-boundaries.md`

---

## Test Strategy Summary

From testing-antipatterns-expert review (2026-04-20):

- **Test shape:** honeycomb (~40% pure-unit, ~50% integration, ~10% CLI smoke). Not a pyramid. Real bugs live in extractor → store → index → retriever interactions.
- **Always real in tests:** SQLite (`:memory:` or tempfile), tree-sitter/ast-grep on fixture source, `FixedClock`.
- **Always mocked:** `Embedder` (`HashEmbedder` — never load fastembed in CI), `GitOps` trait (mock for unit; real bare repo in tempdir for integration), LLM (never call; `insta` snapshot rendered markdown *secondary*).
- **Public API surfaces to test:** `SourcesConfig::load`, `extract_source`, `split_markdown`, `ChunkStore::{append,load,validate}`, `IndexBuilder::rebuild_from_jsonl`, `Retriever::query`, `plan_injection`, `render_context_block`, `annotate_staleness`, `FeedbackStore::record_context_misleading`, `Calibrator::injection_threshold_for`, `run_context_cmd`.
- **Don't test:** internal `blend()`/`apply_boosts()` directly, `_sort_candidates()`, private helpers. Test through public API.
- **Assertions:** relative rank order, inequality on float scores, named SQL queries against SQLite (no binary snapshots of `index.db`).

## Fixture Tree (create early, reused across phases)

```
tests/fixtures/context/
├── repos/
│   ├── mini-rust/              # ~5 files, pub fns with doc comments, README, one ADR
│   │   ├── Cargo.toml
│   │   ├── README.md
│   │   ├── docs/adr/001-foo.md
│   │   └── src/{lib.rs,token.rs,util.rs}
│   ├── mini-ts/                # ~4 files, exported fns/types, README
│   │   ├── package.json
│   │   ├── README.md
│   │   └── src/{index.ts,auth.ts}
│   └── mini-terraform/         # 2 modules with vars/outputs
│       ├── README.md
│       └── networking/{main.tf,variables.tf,outputs.tf}
├── sources/
│   └── example-sources.toml    # references paths above
├── chunks/
│   ├── golden-mini-rust.jsonl  # expected extraction output, regenerated via QUORUM_UPDATE_GOLDEN=1
│   └── synthetic-50-chunks.jsonl  # hand-crafted chunks for retrieval tests
└── eval/
    └── gold-relevance.jsonl    # 20-item query→expected-chunk-ids gold set
```

**Golden update flow:** `QUORUM_UPDATE_GOLDEN=1 cargo test context::` rewrites goldens; review diff like code.

## Development Workflow

- **Commit discipline:** one commit per green task. Tests + impl together.
- **Branch:** feature branch `feat/context`, PR per phase.
- **RED-GREEN:** write all RED tests for a task, run to confirm fail, then GREEN implementation, run to confirm pass, commit.
- **Lint:** `cargo clippy --all-targets --all-features -- -D warnings` before every commit.
- **Speed check:** per-task tests run in < 2s each on M2. Flag and split any task whose tests exceed 10s.

---

## Phase 1: Foundations (types, config, chunk store)

Goal: the types and on-disk format exist and roundtrip. No behavior beyond persistence.

### Task 1.1: Fixture skeleton

**Files:**
- Create: `tests/fixtures/context/repos/mini-rust/{Cargo.toml,README.md,src/lib.rs,src/token.rs,src/util.rs,docs/adr/001-foo.md}`
- Create: `tests/fixtures/context/repos/mini-ts/{package.json,README.md,src/index.ts,src/auth.ts}`
- Create: `tests/fixtures/context/repos/mini-terraform/{README.md,networking/main.tf,networking/variables.tf,networking/outputs.tf}`
- Create: `tests/fixtures/context/sources/example-sources.toml`

**Content specs:**
- `mini-rust/src/token.rs` must expose `pub fn verify_token(token: &str, opts: VerifyOpts) -> Result<Claims, AuthError>` with a 2-line doc comment mentioning "JWT" and "signing key"
- `mini-rust/src/util.rs` exposes `pub fn clamp<T: Ord>(v: T, lo: T, hi: T) -> T`
- `mini-rust/README.md` has ≥ 2 headings, one code block
- `mini-rust/docs/adr/001-foo.md` contains "We chose X over Y because..."
- `mini-ts/src/auth.ts` exports `verifyToken(token: string, opts: VerifyOpts): Result<Claims, AuthError>` with JSDoc
- `mini-terraform/networking/variables.tf` has 2 `variable` blocks with descriptions
- `mini-terraform/networking/outputs.tf` has 1 `output` block with description

**Step 1:** Create files with the exact content specs. No tests yet — fixtures are test data.
**Step 2:** `git add tests/fixtures/context/ && git commit -m "test: fixture repos for context feature"`

### Task 1.2: Chunk types

**Files:**
- Create: `src/context/mod.rs`
- Create: `src/context/types.rs`
- Create: `src/context/types_tests.rs` (or inline `#[cfg(test)] mod tests`)

**Step 1: Write RED test**

```rust
// src/context/types_tests.rs
use super::types::*;

#[test]
fn chunk_serializes_to_jsonl_and_roundtrips() {
    let chunk = Chunk {
        id: "mini-rust:src/token.rs:verify_token".into(),
        source: "mini-rust".into(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: Some("token::verify_token".into()),
        signature: Some("pub fn verify_token(token: &str, opts: VerifyOpts) -> Result<Claims, AuthError>".into()),
        content: "Validates a JWT against the signing key.".into(),
        metadata: ChunkMeta::test_default("src/token.rs", (10, 25)),
        provenance: Provenance::test_default("ast-grep-rust"),
    };
    let line = serde_json::to_string(&chunk).unwrap();
    assert!(!line.contains('\n'), "JSONL lines must be single-line");
    let decoded: Chunk = serde_json::from_str(&line).unwrap();
    assert_eq!(decoded, chunk);
}

#[test]
fn chunk_kind_serializes_as_snake_case() {
    assert_eq!(serde_json::to_string(&ChunkKind::Symbol).unwrap(), "\"symbol\"");
    assert_eq!(serde_json::to_string(&ChunkKind::Doc).unwrap(), "\"doc\"");
    assert_eq!(serde_json::to_string(&ChunkKind::Schema).unwrap(), "\"schema\"");
}
```

**Step 2: Run → expect fail (types don't exist).** `cargo test -p quorum context::types` → FAIL.

**Step 3: GREEN — minimal types**

```rust
// src/context/types.rs
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind { Symbol, Doc, Schema }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub source: String,
    pub kind: ChunkKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub content: String,
    pub metadata: ChunkMeta,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub source_path: PathBuf,
    pub line_range: (u32, u32),
    pub commit_sha: String,
    pub indexed_at: DateTime<Utc>,
    pub source_version: Option<String>,
    pub language: Option<String>,
    pub is_exported: bool,
    pub neighboring_symbols: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub extractor: String,
    pub confidence: f32,
    pub source_uri: String,
}

#[cfg(test)]
impl ChunkMeta {
    pub fn test_default(path: &str, range: (u32, u32)) -> Self { /* ... */ }
}
#[cfg(test)]
impl Provenance {
    pub fn test_default(extractor: &str) -> Self { /* ... */ }
}
```

Register module in `src/lib.rs`: `pub mod context;` and `src/context/mod.rs`: `pub mod types; #[cfg(test)] mod types_tests;`

**Step 4: Run → expect pass.** `cargo test -p quorum context::types` → PASS.

**Step 5: Commit.**
```bash
git add src/context/ src/lib.rs && git commit -m "context: chunk types with JSONL roundtrip"
```

### Task 1.3: SourcesConfig loader

**Files:**
- Create: `src/context/config.rs`
- Create: `src/context/config_tests.rs` (inline)

**Step 1: Write RED tests**

```rust
#[test]
fn loads_valid_sources_toml() {
    let toml = r#"
        [[source]]
        name = "internal-auth"
        git = "git@github.com:myorg/auth.git"
        kind = "rust"
        weight = 10

        [[source]]
        name = "tf-net"
        path = "../terraform-modules/networking"
        kind = "terraform"

        [context]
        inject_budget_tokens = 1500
        inject_min_score = 0.65
    "#;
    let config = SourcesConfig::from_str(toml).unwrap();
    assert_eq!(config.sources.len(), 2);
    assert_eq!(config.sources[0].name, "internal-auth");
    assert_eq!(config.sources[0].weight, Some(10));
    assert!(matches!(config.sources[0].location, SourceLocation::Git { .. }));
    assert!(matches!(config.sources[1].location, SourceLocation::Path(_)));
    assert_eq!(config.context.inject_budget_tokens, 1500);
}

#[test]
fn rejects_source_with_both_git_and_path() {
    let toml = r#"[[source]]
        name = "bad"
        git = "x"
        path = "y"
        kind = "rust""#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(err.to_string().contains("exactly one"), "got: {err}");
}

#[test]
fn rejects_duplicate_source_names() {
    let toml = r#"
        [[source]]
        name = "dup"
        path = "a"
        kind = "rust"
        [[source]]
        name = "dup"
        path = "b"
        kind = "rust"
    "#;
    let err = SourcesConfig::from_str(toml).unwrap_err();
    assert!(err.to_string().contains("duplicate"), "got: {err}");
}

#[test]
fn defaults_fill_missing_context_block() {
    let toml = r#"[[source]]
        name = "x"
        path = "."
        kind = "rust""#;
    let config = SourcesConfig::from_str(toml).unwrap();
    assert_eq!(config.context.inject_budget_tokens, 1500);
    assert!((config.context.inject_min_score - 0.65).abs() < f32::EPSILON);
    assert_eq!(config.context.rerank_recency_floor, 0.25);
}

#[test]
fn example_fixture_loads() {
    let path = std::path::Path::new("tests/fixtures/context/sources/example-sources.toml");
    let _config = SourcesConfig::load(path).unwrap();
}
```

**Step 2: Run → FAIL (config doesn't exist).**

**Step 3: GREEN — minimal implementation**

```rust
// src/context/config.rs
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct SourcesConfig {
    #[serde(rename = "source", default)]
    pub sources: Vec<SourceEntry>,
    #[serde(default)]
    pub context: ContextConfig,
}

#[derive(Debug, Deserialize)]
pub struct SourceEntry {
    pub name: String,
    pub kind: String,                    // validated in post-process
    pub git: Option<String>,
    pub path: Option<PathBuf>,
    pub rev: Option<String>,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    pub weight: Option<i32>,
    #[serde(skip_deserializing)]
    pub location: SourceLocation,
}

#[derive(Debug, Default)]
pub enum SourceLocation {
    #[default]
    Unresolved,
    Git { url: String, rev: Option<String> },
    Path(PathBuf),
}

#[derive(Debug, Deserialize)]
pub struct ContextConfig {
    #[serde(default = "default_budget")]
    pub inject_budget_tokens: u32,
    #[serde(default = "default_min_score")]
    pub inject_min_score: f32,
    #[serde(default = "default_max_chunks")]
    pub inject_max_chunks: u32,
    #[serde(default = "default_halflife")]
    pub rerank_recency_halflife_days: u32,
    #[serde(default = "default_recency_floor")]
    pub rerank_recency_floor: f32,
    #[serde(default = "default_max_size")]
    pub max_source_size_mb: u32,
    #[serde(default)]
    pub ignore: Vec<String>,
}

// default fns + Default impl + SourcesConfig::{from_str, load} with validation pass
// that: (a) sets location from git/path, (b) rejects both-or-neither, (c) rejects dup names.
```

**Step 4: Run → PASS.**

**Step 5: Commit.**
```bash
git add src/context/config.rs src/context/mod.rs && git commit -m "context: sources.toml loader with validation"
```

### Task 1.4: ChunkStore (JSONL I/O)

**Files:**
- Create: `src/context/store.rs`
- Create: `src/context/store_tests.rs` (inline)

**Step 1: RED tests**

```rust
#[test]
fn append_and_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.jsonl");
    let mut store = ChunkStore::new(&path);
    let c1 = test_chunk("a");
    let c2 = test_chunk("b");
    store.append(&c1).unwrap();
    store.append(&c2).unwrap();
    let loaded = ChunkStore::load_all(&path).unwrap();
    assert_eq!(loaded, vec![c1, c2]);
}

#[test]
fn load_skips_malformed_lines_and_reports_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.jsonl");
    std::fs::write(&path, r#"{"valid":"but_wrong_schema"}
{"id":"x","source":"s","kind":"symbol","content":"","metadata":{"source_path":"x","line_range":[1,2],"commit_sha":"c","indexed_at":"2026-01-01T00:00:00Z","is_exported":true,"neighboring_symbols":[]},"provenance":{"extractor":"e","confidence":1.0,"source_uri":"u"}}
"#).unwrap();
    let result = ChunkStore::load_all_lenient(&path).unwrap();
    assert_eq!(result.chunks.len(), 1);
    assert_eq!(result.errors.len(), 1);
}

#[test]
fn validate_detects_duplicate_ids() {
    let chunks = vec![test_chunk("same"), test_chunk("same")];
    let report = ChunkStore::validate(&chunks);
    assert!(report.has_errors());
    assert!(report.errors.iter().any(|e| e.contains("duplicate")));
}
```

**Step 2: FAIL.**

**Step 3: GREEN**

```rust
pub struct ChunkStore { path: PathBuf, writer: Option<BufWriter<File>> }
pub struct LoadReport { pub chunks: Vec<Chunk>, pub errors: Vec<LoadError> }
pub struct ValidationReport { pub errors: Vec<String> }

impl ChunkStore {
    pub fn new(path: &Path) -> Self { /* defer file open to first append */ }
    pub fn append(&mut self, chunk: &Chunk) -> io::Result<()> { /* open-append, write line, flush */ }
    pub fn load_all(path: &Path) -> io::Result<Vec<Chunk>> { /* strict parse, any error fails */ }
    pub fn load_all_lenient(path: &Path) -> io::Result<LoadReport> { /* skip malformed */ }
    pub fn validate(chunks: &[Chunk]) -> ValidationReport { /* dup ids, empty fields, etc. */ }
}
```

**Step 4: PASS.**

**Step 5: Commit.**
```bash
git add src/context/store.rs src/context/mod.rs && git commit -m "context: JSONL chunk store with lenient load"
```

### Task 1.5: Phase 1 integration test

**File:** Create: `tests/context_foundations_integration.rs` (quorum crate's integration test dir)

**Step 1: RED test**

```rust
use quorum::context::{SourcesConfig, ChunkStore, types::*};

#[test]
fn config_loads_and_store_roundtrips_chunks() {
    let config_path = std::path::Path::new("tests/fixtures/context/sources/example-sources.toml");
    let config = SourcesConfig::load(config_path).unwrap();
    assert!(!config.sources.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("chunks.jsonl");
    let mut store = ChunkStore::new(&store_path);
    let sample = Chunk { source: config.sources[0].name.clone(), /* ... */ };
    store.append(&sample).unwrap();

    let loaded = ChunkStore::load_all(&store_path).unwrap();
    assert_eq!(loaded, vec![sample]);
}
```

**Step 2: Run → PASS** (if Tasks 1.2-1.4 are GREEN, this should already pass).

**Step 3: Commit.**
```bash
git add tests/context_foundations_integration.rs && git commit -m "test: context foundations integration"
```

---

## Phase 2: Extractors (source → chunks)

Goal: given a local source path, produce a `chunks.jsonl` via ast-grep + markdown splitter.

### Task 2.1: Markdown splitter (public API: `split_markdown`)

**Files:**
- Create: `src/context/extract/mod.rs`
- Create: `src/context/extract/markdown.rs`
- Create: `src/context/extract/markdown_tests.rs`

**Step 1: RED tests**

```rust
#[test]
fn splits_by_h2_preserving_code_blocks() {
    let md = r#"# Project
intro
## Usage
Call `foo()`:
```rust
fn main() { foo(); }
```
## Design
some prose"#;
    let chunks = split_markdown(md, "README.md", "test-src", Subtype::Readme).collect::<Vec<_>>();
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[1].subtype.as_deref(), Some("README"));
    assert!(chunks[1].content.contains("```rust"));
    assert!(chunks[1].id.ends_with("README.md:usage"));
}

#[test]
fn heading_slug_is_stable_for_duplicate_headings() {
    let md = "# A\n## Same\nx\n## Same\ny";
    let chunks: Vec<_> = split_markdown(md, "d.md", "s", Subtype::Doc).collect();
    assert_ne!(chunks[0].id, chunks[1].id);
    assert!(chunks[1].id.contains("same-2") || chunks[1].id.contains("same_2"));
}

#[test]
fn empty_markdown_yields_no_chunks() {
    let chunks: Vec<_> = split_markdown("", "e.md", "s", Subtype::Doc).collect();
    assert!(chunks.is_empty());
}

#[test]
fn mini_rust_readme_splits_into_expected_sections() {
    let md = std::fs::read_to_string("tests/fixtures/context/repos/mini-rust/README.md").unwrap();
    let chunks: Vec<_> = split_markdown(&md, "README.md", "mini-rust", Subtype::Readme).collect();
    assert!(chunks.len() >= 2);
}
```

**Step 2: FAIL.**

**Step 3: GREEN.** Use `pulldown-cmark` (already likely a dep; if not, add). Split by H2 (configurable depth). Slugify heading for ID suffix, disambiguate collisions with `-N`. Preserve code blocks intact.

**Step 4: PASS.**
**Step 5: Commit.**

### Task 2.2: Ast-grep symbol extractor (rust)

**Files:**
- Create: `src/context/extract/astgrep_rust.rs`
- Create: `src/context/extract/astgrep_rust_tests.rs`
- Create: `rules/rust/extraction/public-functions.yml` (ast-grep rule)
- Create: `rules/rust/extraction/public-structs.yml`
- Create: `rules/rust/extraction/public-enums.yml`
- Create: `rules/rust/extraction/public-traits.yml`

**Step 1: RED tests**

```rust
#[test]
fn extracts_pub_fn_with_signature_and_doc_comment() {
    let src = r#"
/// Validates a JWT.
/// Errors if expired.
pub fn verify_token(token: &str, opts: VerifyOpts) -> Result<Claims, AuthError> {
    todo!()
}
"#;
    let chunks = extract_rust(src, "src/token.rs", "mini-rust").unwrap();
    let vt = chunks.iter().find(|c| c.qualified_name.as_deref() == Some("verify_token")).unwrap();
    assert_eq!(vt.kind, ChunkKind::Symbol);
    assert!(vt.signature.as_ref().unwrap().contains("pub fn verify_token"));
    assert!(vt.content.contains("Validates a JWT"));
    assert!(vt.metadata.is_exported);
}

#[test]
fn skips_private_fn() {
    let src = "fn private() {}";
    assert!(extract_rust(src, "x.rs", "s").unwrap().is_empty());
}

#[test]
fn extracts_neighboring_symbols() {
    let src = r#"
pub fn a() {}
pub fn b() {}
pub fn c() {}
"#;
    let chunks = extract_rust(src, "x.rs", "s").unwrap();
    let a = chunks.iter().find(|c| c.qualified_name.as_deref() == Some("a")).unwrap();
    assert_eq!(a.metadata.neighboring_symbols, vec!["b".to_string(), "c".to_string()]);
}

#[test]
fn mini_rust_token_rs_extraction_matches_golden() {
    let src = std::fs::read_to_string("tests/fixtures/context/repos/mini-rust/src/token.rs").unwrap();
    let chunks = extract_rust(&src, "src/token.rs", "mini-rust").unwrap();
    assert_golden("golden-mini-rust-token.jsonl", &chunks);
}
```

**Step 2: FAIL.**

**Step 3: GREEN.** Load ast-grep rules from `rules/rust/extraction/` via the same mechanism used in existing ast-grep integration. Map each match → `Chunk`. Extract doc comment via preceding `///` lines. Collect sibling `pub fn|struct|enum|trait` names as neighbors.

Golden assertion: `assert_golden` is a helper that reads the expected JSONL, compares against actual; `QUORUM_UPDATE_GOLDEN=1` rewrites it. Create `tests/helpers/golden.rs` if not present.

**Step 4: PASS.** Run `QUORUM_UPDATE_GOLDEN=1 cargo test context::extract::astgrep_rust` once, review the diff, commit the golden.

**Step 5: Commit.**

### Task 2.3: Ast-grep extractors for TS, Python, Terraform (per-language)

Follow Task 2.2 pattern. One task per language:

- **2.3a TS:** `src/context/extract/astgrep_ts.rs` + `rules/typescript/extraction/*.yml` (exported fns, types, interfaces). Fixture test against `mini-ts/`.
- **2.3b Python:** `src/context/extract/astgrep_py.rs` + `rules/python/extraction/*.yml` (`def` + `class` where name doesn't start with `_`, or is in `__all__`).
- **2.3c Terraform:** `src/context/extract/astgrep_hcl.rs` + `rules/hcl/extraction/*.yml` (`variable`, `output`, `resource`, `module` blocks). Variables + outputs become Symbol chunks with their `description` as content.

Each is its own commit. **Skip v2 languages (Go, Java, etc.) — not in MVP.**

### Task 2.4: Extractor dispatch (`extract_source`)

**Files:**
- Create: `src/context/extract/dispatch.rs`
- Create: `src/context/extract/dispatch_tests.rs`

**Step 1: RED tests**

```rust
#[test]
fn extracts_mini_rust_source_end_to_end() {
    let source = SourceEntry::local("mini-rust", "tests/fixtures/context/repos/mini-rust", "rust");
    let result = extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    assert!(result.chunks.iter().any(|c| c.qualified_name.as_deref() == Some("verify_token")));
    assert!(result.chunks.iter().any(|c| c.kind == ChunkKind::Doc));
    assert!(result.diagnostics.ignored_count >= 0);
}

#[test]
fn respects_ignore_patterns() {
    let source = /* mini-rust with ignore = ["docs/"] */;
    let result = extract_source(&source, ...).unwrap();
    assert!(!result.chunks.iter().any(|c| c.metadata.source_path.starts_with("docs")));
    assert_eq!(result.diagnostics.skipped_by_tier.per_source, /* > 0 */);
}

#[test]
fn skips_file_larger_than_cap_and_logs() {
    // Write a 10MB garbage.rs into tempdir, verify not extracted + logged
}

#[test]
fn extractor_crash_on_one_file_skips_not_fails() {
    // Point at a file that makes ast-grep choke — assert: other files still extracted, error logged
}
```

**Step 2: FAIL.**

**Step 3: GREEN.** `extract_source` walks source paths, filters via ignore tiers (per-source > global > .gitignore), dispatches per-extension to the right extractor, collects all chunks + a `Diagnostics` struct with per-tier skip counts and top skipped globs. Never panics on a single-file failure.

**Step 4: PASS.**
**Step 5: Commit.**

### Task 2.5: Phase 2 integration test

```rust
#[test]
fn extract_source_writes_jsonl_that_loads_back() {
    let source = fixture_source("mini-rust");
    let dir = tempfile::tempdir().unwrap();
    let chunks_path = dir.path().join("chunks.jsonl");

    let result = extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    let mut store = ChunkStore::new(&chunks_path);
    for c in &result.chunks { store.append(c).unwrap(); }

    let loaded = ChunkStore::load_all(&chunks_path).unwrap();
    assert_eq!(loaded, result.chunks);
    assert!(loaded.iter().any(|c| c.qualified_name.as_deref() == Some("verify_token")));
    assert!(loaded.iter().any(|c| matches!(c.kind, ChunkKind::Doc)));
}
```

Commit.

---

## Phase 3: Indexing (chunks → searchable SQLite)

Goal: given `chunks.jsonl`, produce `index.db` (FTS5 + sqlite-vec) that's queryable via SQL.

### Task 3.1: Clock + Embedder traits

**File:** `src/context/index/traits.rs`

```rust
pub trait Clock: Send + Sync { fn now(&self) -> DateTime<Utc>; }
pub struct FixedClock(pub DateTime<Utc>);
impl Clock for FixedClock { fn now(&self) -> DateTime<Utc> { self.0 } }
impl FixedClock { pub fn epoch() -> Self { Self(DateTime::from_timestamp(0, 0).unwrap()) } }

pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>>;
    fn model_hash(&self) -> String;  // for state.json
}

/// Deterministic test embedder — hash tokens to random-but-stable floats.
pub struct HashEmbedder { pub dim: usize }
impl Embedder for HashEmbedder { /* hash each token, distribute across dims */ }

/// Real embedder wrapping fastembed bge-small-en-v1.5
pub struct FastembedEmbedder { /* ... */ }
```

RED test: `HashEmbedder` produces same vector for same input, different for different. Model hash stable. Dim correct. Commit.

### Task 3.2: SQLite schema + IndexBuilder::new

**File:** `src/context/index/builder.rs`

```rust
pub struct IndexBuilder<'a, C: Clock, E: Embedder> { /* ... */ }

impl<'a, C: Clock, E: Embedder> IndexBuilder<'a, C, E> {
    pub fn new(db_path: &Path, clock: &'a C, embedder: &'a E) -> rusqlite::Result<Self> {
        // Open, run schema migrations, load sqlite-vec extension
    }
    pub fn schema_version(&self) -> u32;
}
```

**Schema:**

```sql
CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT);
-- state: schema_version, embedder_model_hash, last_full_rebuild_at

CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    kind TEXT NOT NULL,
    subtype TEXT,
    qualified_name TEXT,
    signature TEXT,
    content TEXT NOT NULL,
    source_path TEXT NOT NULL,
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    commit_sha TEXT NOT NULL,
    indexed_at TEXT NOT NULL,
    source_version TEXT,
    language TEXT,
    is_exported INTEGER NOT NULL,
    neighboring_symbols_json TEXT,
    extractor TEXT NOT NULL,
    confidence REAL NOT NULL,
    source_uri TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    id UNINDEXED,
    content,
    qualified_name,
    neighboring_symbols,
    tokenize = 'unicode61 tokenchars ''_::$'''
);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(
    id TEXT PRIMARY KEY,
    embedding FLOAT[384]  -- bge-small-en-v1.5 dim
);

CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source);
CREATE INDEX IF NOT EXISTS idx_chunks_kind ON chunks(kind);
CREATE INDEX IF NOT EXISTS idx_chunks_commit ON chunks(commit_sha);
```

**RED tests:**

```rust
#[test]
fn new_creates_schema_in_empty_db() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder { dim: 384 };
    let builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
    assert_eq!(builder.schema_version(), 1);

    // Assert tables exist via SQL
    let conn = rusqlite::Connection::open(&db).unwrap();
    let tables: Vec<String> = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").unwrap()
        .query_map([], |r| r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
    assert!(tables.contains(&"chunks".to_string()));
    assert!(tables.contains(&"state".to_string()));
}

#[test]
fn reopening_existing_db_does_not_reset() {
    // Create, write state row, reopen, assert row still present
}

#[test]
fn model_hash_mismatch_is_detectable() {
    // Initial open with HashEmbedder dim=384
    // Reopen with different embedder hash — assert builder.requires_reembedding() == true
}
```

Commit.

### Task 3.3: `rebuild_from_jsonl`

```rust
impl IndexBuilder {
    /// Full rebuild: truncate chunks/chunks_fts/chunks_vec, re-insert all chunks, re-embed.
    pub fn rebuild_from_jsonl(&mut self, source_name: &str, jsonl_path: &Path) -> Result<RebuildReport>;
}

pub struct RebuildReport {
    pub chunks_loaded: usize,
    pub chunks_embedded: usize,
    pub elapsed_ms: u64,
    pub errors: Vec<String>,
}
```

**RED tests:**

```rust
#[test]
fn rebuild_loads_chunks_into_all_three_tables() {
    let dir = tempfile::tempdir().unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    // write 3 valid chunks to jsonl
    let mut builder = IndexBuilder::new(&dir.path().join("index.db"), &FixedClock::epoch(), &HashEmbedder{dim:384}).unwrap();
    let report = builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();
    assert_eq!(report.chunks_loaded, 3);
    assert_eq!(report.chunks_embedded, 3);

    // Assert: SELECT count(*) FROM chunks == 3
    // Assert: SELECT count(*) FROM chunks_fts == 3
    // Assert: SELECT count(*) FROM chunks_vec == 3
}

#[test]
fn rebuild_replaces_prior_source_chunks_atomically() {
    // Rebuild with 3 chunks, then rebuild with 2 chunks for same source
    // Assert: exactly 2 chunks present for that source; prior 3 fully gone
}

#[test]
fn malformed_chunk_is_skipped_not_fatal() {
    // jsonl with 1 valid + 1 invalid — assert: 1 loaded, errors.len() == 1
}

#[test]
fn rebuild_preserves_other_sources() {
    // Two sources indexed. Rebuild only source A. Assert source B's chunks untouched.
}
```

Use transaction: `BEGIN`, delete from `chunks WHERE source=?`, delete matching fts/vec rows, insert new, `COMMIT`.

Commit.

### Task 3.4: state.json + model hash tracking

**File:** `src/context/index/state.rs`

Small JSON file at `~/.quorum/context/state.json`:

```json
{"schema_version": 1, "embedder_model_hash": "hashembedder-384-v1", "quorum_version": "0.15.0"}
```

RED tests: load/save, detect model hash drift, emit `StateCheck::ReembedRequired` vs `Ok`. Commit.

### Task 3.5: Phase 3 integration test

```rust
#[test]
fn extracted_chunks_become_queryable_via_fts_and_vec() {
    let dir = tempfile::tempdir().unwrap();
    let source = fixture_source("mini-rust");

    // Extract
    let extracted = extract_source(&source, &ExtractConfig::default(), &FixedClock::epoch()).unwrap();
    let jsonl = dir.path().join("chunks.jsonl");
    let mut store = ChunkStore::new(&jsonl);
    for c in &extracted.chunks { store.append(c).unwrap(); }

    // Index
    let emb = HashEmbedder { dim: 384 };
    let mut builder = IndexBuilder::new(&dir.path().join("index.db"), &FixedClock::epoch(), &emb).unwrap();
    builder.rebuild_from_jsonl("mini-rust", &jsonl).unwrap();

    // Query FTS directly
    let conn = rusqlite::Connection::open(dir.path().join("index.db")).unwrap();
    let hits: Vec<String> = conn.prepare(
        "SELECT id FROM chunks_fts WHERE chunks_fts MATCH 'verify_token'"
    ).unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
    assert!(hits.iter().any(|id| id.contains("verify_token")));

    // Query vec directly
    let q_vec: Vec<f32> = emb.embed("jwt verification");
    // ... similar assertion on chunks_vec MATCH knn
}
```

Commit.

---

## Phase 4: Retrieval (query → ranked chunks)

Goal: a single `Retriever::query(RetrievalQuery) -> Vec<ScoredChunk>` that does BM25 + vec + rerank. Public API is the only thing tests touch.

### Task 4.1: BM25 retrieval (internal, tested via Retriever later)

Implement `bm25_search(conn, query, filters, k) -> Vec<(chunk_id, score)>` in `src/context/retrieve/bm25.rs`. No public tests at this layer — exercised through `Retriever::query` in Task 4.6. Commit once Retriever test goes green.

### Task 4.2: Vector retrieval (internal)

`vec_search(conn, q_embedding, filters, k) -> Vec<(chunk_id, score)>` in `src/context/retrieve/vector.rs`. Also internal.

### Task 4.3: Rerank formula

**File:** `src/context/retrieve/rerank.rs`

Per design §Retrieval:
```
blended     = 0.6 * bm25_norm + 0.4 * vec_norm  (min-max per query)
id_boost    = 1.0 if identifier_exact_match else 0.0
path_boost  = 0.5 if source lang matches reviewed file lang else 0.0
recency_mul = clamp(exp(-ln(2) * age_days / halflife), 0.25, 1.0)
score       = (blended + id_boost + path_boost) * recency_mul
```

**Tests are at `Retriever::query` level.** Don't unit-test `rerank()` directly — brittle. Keep the fn `pub(crate)` and exercise via integration.

### Task 4.4: Identifier harvester with fallback

**File:** `src/context/retrieve/identifiers.rs`

```rust
pub fn harvest_identifiers(
    refs: &[Symbol],                    // from hydration
    reviewed_file: &ReviewedFile,
    stoplist: &GenericStoplist,
) -> Vec<String>;
```

**RED tests** (pure function, heavy unit coverage here):

```rust
#[test]
fn returns_refs_when_specific() {
    let refs = symbols(&["verify_token", "sign_jwt"]);
    let file = file_with_path("src/auth.rs", "rust");
    let ids = harvest_identifiers(&refs, &file, &stoplist_rust());
    assert_eq!(ids, vec!["verify_token", "sign_jwt"]);
}

#[test]
fn augments_when_refs_are_generic() {
    let refs = symbols(&["Client", "Handler"]);
    let file = file_with_path("src/services/payment/processor.rs", "rust");
    let file = file.with_neighbors(&["PaymentProcessor", "process_charge"]);
    let ids = harvest_identifiers(&refs, &file, &stoplist_rust());
    // Expect augmented: module path + neighbors
    assert!(ids.contains(&"payment".to_string()));
    assert!(ids.contains(&"processor".to_string()));
    assert!(ids.contains(&"PaymentProcessor".to_string()));
}

#[test]
fn augments_when_refs_are_empty() {
    let ids = harvest_identifiers(&[], &file_with_path("src/foo/bar.rs", "rust"), &stoplist_rust());
    assert!(!ids.is_empty());
}

#[test]
fn stoplist_is_language_scoped() {
    let ids = harvest_identifiers(&symbols(&["Client"]), &file_with_path("x.py", "python"), &stoplist_rust());
    // Client may not be generic in python stoplist (depends on your list)
    // This tests that we load the right stoplist
}
```

**GREEN.** Load stoplist from `rules/<lang>/generic-names.yml`. Augment strategy: `if len(refs) < 2 OR all(r in stoplist)`, add module path segments + neighbors + distinctive imports.

Commit.

### Task 4.5: `Retriever::query` public API

**File:** `src/context/retrieve/mod.rs`

```rust
pub struct Retriever<'a, E: Embedder> { conn: &'a Connection, embedder: &'a E, clock: Box<dyn Clock> }

impl<'a, E: Embedder> Retriever<'a, E> {
    pub fn query(&self, q: RetrievalQuery) -> rusqlite::Result<Vec<ScoredChunk>>;
}

pub struct RetrievalQuery {
    pub text: String,
    pub identifiers: Vec<String>,
    pub filters: Filters,
    pub k: usize,
    pub min_score: f32,
    pub reviewed_file_language: Option<String>,
}

pub struct ScoredChunk {
    pub chunk: Chunk,
    pub score: f32,
    pub components: ScoreBreakdown,  // bm25_norm, vec_norm, id_boost, path_boost, recency_mul
}
```

**RED tests** — these are the real retrieval tests, using `synthetic-50-chunks.jsonl` and inequality assertions:

```rust
#[test]
fn query_returns_empty_when_below_threshold() {
    let retriever = retriever_with_fixture("synthetic-50-chunks.jsonl");
    let hits = retriever.query(RetrievalQuery {
        text: "zzzzzzzzzz unrelated string".into(),
        identifiers: vec![],
        filters: Filters::default(),
        k: 5, min_score: 0.95, reviewed_file_language: None,
    }).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn exact_identifier_match_outranks_semantic_only_match() {
    // Chunk A: qname = "verify_token", prose about unrelated topic
    // Chunk B: qname = "process_request", prose about JWT validation
    // Query: identifiers = ["verify_token"], text = "JWT validation"
    let hits = retriever.query(query_for("verify_token", "JWT validation")).unwrap();
    let a_rank = hits.iter().position(|h| h.chunk.qualified_name.as_deref() == Some("verify_token")).unwrap();
    let b_rank = hits.iter().position(|h| h.chunk.qualified_name.as_deref() == Some("process_request")).unwrap();
    assert!(a_rank < b_rank, "id match should outrank semantic-only");
}

#[test]
fn recency_decay_does_not_annihilate_old_authoritative_chunks() {
    // Inject 2 chunks: one 5yr old with perfect BM25+vec match, one 1wk old with mediocre match
    // Assert: old chunk still appears above threshold (recency_floor=0.25 kicks in)
    // Don't assert exact ordering — just that old_chunk.score > 0 and appears in results
}

#[test]
fn respects_kind_filter() {
    let doc_only = retriever.query(query_with_kind_filter(&[ChunkKind::Doc])).unwrap();
    assert!(doc_only.iter().all(|h| h.chunk.kind == ChunkKind::Doc));
}

#[test]
fn respects_source_filter() {
    // With synthetic chunks from source A and B — query with filter sources=[A], assert all hits from A
}

#[test]
fn bm25_normalization_prevents_short_doc_collapse() {
    // Inject 5 very short doc chunks with identical BM25 raw scores
    // Plus 1 long chunk with mediocre raw score
    // Assert: vector component still influences final ranking (not pure BM25 dominance)
}

#[test]
fn score_components_are_exposed_for_telemetry() {
    let hits = retriever.query(normal_query()).unwrap();
    assert!(hits[0].components.bm25_norm >= 0.0 && hits[0].components.bm25_norm <= 1.0);
    assert!(hits[0].components.recency_mul >= 0.25);  // floor
}
```

**GREEN.** Wire BM25 + vec + rerank + threshold gate. Normalize per-query with min-max.

Commit.

### Task 4.6: Phase 4 integration — real extracted chunks

```rust
#[test]
fn retrieval_over_mini_rust_returns_verify_token_for_jwt_query() {
    // Extract mini-rust, build index, create retriever
    // Query: identifiers=["verify_token"], text="JWT validation"
    // Assert: top hit's qualified_name == Some("verify_token")
}
```

Commit.

---

## Phase 5: Planning + rendering

Goal: given retrieved chunks, produce a markdown block under a token budget.

### Task 5.1: `plan_injection` — adaptive threshold + budget spillover

**File:** `src/context/inject/plan.rs`

```rust
pub struct InjectionPlan {
    pub injected: Vec<ScoredChunk>,
    pub skipped_budget: Vec<ScoredChunk>,
    pub skipped_stale: Vec<ScoredChunk>,
    pub below_threshold_count: usize,
}

pub fn plan_injection(
    symbol_hits: Vec<ScoredChunk>,
    prose_candidates: Vec<ScoredChunk>,
    config: &ContextConfig,
    tokenize: impl Fn(&str) -> usize,
) -> InjectionPlan;
```

Pure function. Heavy table-test coverage.

**RED tests:**

```rust
#[test]
fn empty_hits_below_40pct_floor_injects_nothing() {
    let plan = plan_injection(vec![], vec![], &config(budget=1500), token_count);
    assert!(plan.injected.is_empty());
}

#[test]
fn symbol_starvation_lowers_prose_threshold() {
    // 0 symbols survive τ=0.65
    // Prose candidates: one at 0.55 (normally below τ, but with -0.10 drop it survives)
    let plan = plan_injection(vec![], vec![prose_scored(0.55)], &config_with_tau(0.65), token_count);
    assert_eq!(plan.injected.len(), 1);
}

#[test]
fn unused_symbol_budget_spills_to_prose() {
    // 2 symbols take 400 tokens, budget is 1500, so 1100 left
    // 4 prose candidates total ~1000 tokens — all should fit
    let plan = plan_injection(two_symbols(400), four_prose(1000), &config(1500), /* ... */);
    assert_eq!(plan.injected.len(), 6);
}

#[test]
fn budget_clip_never_splits_a_chunk() {
    // 3 chunks totaling 2000 tokens, budget 1500
    // Assert: accepted chunks' total ≤ 1500, no chunk present partially
    let plan = plan_injection(three_chunks_totaling(2000), vec![], &config(1500), tok);
    assert!(plan.injected.iter().map(|c| tok(&c.chunk.content)).sum::<usize>() <= 1500);
}

#[test]
fn under_40pct_floor_skips_injection_entirely() {
    // Single tiny chunk at 100 tokens vs budget 1500 → 6.6% of budget → skip all
    let plan = plan_injection(one_small_chunk(100), vec![], &config(1500), tok);
    assert!(plan.injected.is_empty());
}
```

Commit.

### Task 5.2: `render_context_block`

**File:** `src/context/inject/render.rs`

```rust
pub fn render_context_block(
    plan: &InjectionPlan,
    staleness: &dyn StalenessAnnotator,
    precedence: &PrecedenceLog,
) -> String;
```

**RED tests:**

```rust
#[test]
fn renders_symbol_card_with_signature_code_fence() {
    let plan = injection_plan_with_single_symbol(/* rust, verify_token */);
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::empty());
    assert!(out.contains("```rust"));
    assert!(out.contains("pub fn verify_token"));
    assert!(out.contains("Source: git://"));
}

#[test]
fn normalizes_chunk_content_headings_to_avoid_nesting_break() {
    // Chunk content starts with "## Usage"
    // Wrapper uses "### card"
    // Expect: the "## Usage" is demoted to "#### Usage"
    let out = render_context_block(&plan_with_h2_content(), &NoStaleness, &PrecedenceLog::empty());
    assert!(!out.contains("\n## "));  // no raw h2 in chunk body
}

#[test]
fn annotates_stale_local_chunks() {
    let plan = plan_with_local_source_chunk();
    struct DirtySource;
    impl StalenessAnnotator for DirtySource {
        fn annotate(&self, c: &Chunk) -> Option<String> {
            Some("source has edits since last index (2h ago)".into())
        }
    }
    let out = render_context_block(&plan, &DirtySource, &PrecedenceLog::empty());
    assert!(out.contains("⚠ source has edits since last index"));
}

#[test]
fn shows_precedence_footer_when_suppression_occurred() {
    let mut prec = PrecedenceLog::new();
    prec.record_winner("verify_token", "internal-auth", "internal-auth-fork", "weight 10 > 5");
    let out = render_context_block(&plan_with_winner(), &NoStaleness, &prec);
    assert!(out.contains("precedence: internal-auth wins over internal-auth-fork"));
}

#[test]
fn footer_reports_token_count_and_freshness() {
    let out = render_context_block(&plan, &NoStaleness, &PrecedenceLog::empty());
    assert!(out.contains("tokens across"));
    assert!(out.contains("chunks from"));
}

#[test]
fn insta_snapshot_of_typical_rendered_block() {
    // Use insta for visual regression — secondary assertion
    let out = render_context_block(&typical_plan(), &NoStaleness, &PrecedenceLog::empty());
    insta::assert_snapshot!(out);
}
```

Commit.

### Task 5.3: `StalenessAnnotator` trait + timestamp-based MVP impl

**File:** `src/context/inject/stale.rs`

```rust
pub trait StalenessAnnotator {
    fn annotate(&self, chunk: &Chunk) -> Option<String>;
}

pub struct NoStaleness;
impl StalenessAnnotator for NoStaleness { fn annotate(&self, _: &Chunk) -> Option<String> { None } }

pub struct TimestampStaleness<'a, G: GitOps> {
    pub current_source: Option<&'a str>,
    pub git: &'a G,
    pub clock: &'a dyn Clock,
}
impl<'a, G: GitOps> StalenessAnnotator for TimestampStaleness<'a, G> {
    fn annotate(&self, c: &Chunk) -> Option<String> {
        if Some(c.source.as_str()) != self.current_source { return None; }
        let status = self.git.status_porcelain().ok()?;
        if status.is_empty() { return None; }
        let age = self.clock.now() - c.metadata.indexed_at;
        Some(format!("source has edits since last index ({} ago)", humanize(age)))
    }
}

pub trait GitOps { fn status_porcelain(&self) -> Result<String>; }  // minimal
```

**RED tests:** annotate returns None for clean, Some for dirty, None for non-current-source chunks. Commit.

### Task 5.4: Precedence + conflict resolution

**File:** `src/context/retrieve/precedence.rs`

Pure function. Given multiple chunks with matching `qualified_name`, pick winner by `[weight → commit_sha recency → alphabetical]`. Record suppression in `PrecedenceLog`.

**RED tests:** deterministic winner selection across all four tiebreakers. Commit.

### Task 5.5: Phase 5 integration — retrieve → plan → render

```rust
#[test]
fn retrieval_plus_rendering_produces_injectable_block_under_budget() {
    // Full pipeline on mini-rust; query about "JWT"
    // Plan injection → render → assert:
    //   - result is valid markdown (starts with ##)
    //   - token count (by whitespace split, rough) < 1500
    //   - contains "verify_token"
    //   - contains source_uri
}
```

Commit.

---

## Phase 6: Review integration + telemetry

### Task 6.1: Wire into review pipeline

Locate existing review hydration path (search `hydrate`, `context7`, `review`). Add optional context injection step after hydration, before LLM call. Gate behind `config.context.auto_inject == true`.

**RED test:**

```rust
#[test]
fn review_prompt_includes_context_block_when_source_registered() {
    // Set up tempdir with .quorum/sources.toml pointing at fixture
    // Trigger review on a file that imports from the fixture source
    // Assert: captured prompt contains "## Referenced context"
}
```

Commit.

### Task 6.2: reviews.jsonl context telemetry block

Extend existing `reviews.jsonl` writer with new `context: {...}` block per design §Relevance evaluation harness.

**RED tests:** assert every telemetry field present after a review with injection; all zeros when `auto_inject=false`. Commit.

### Task 6.3: `stats context` dimensions

Extend existing `quorum stats` with `--by-source`, `--by-reviewed-repo`, `--misleading` for context metrics. RED tests on the aggregation functions. Commit.

### Task 6.4: Phase 6 integration — end-to-end review

```rust
#[test]
fn end_to_end_review_with_context_injection_logs_telemetry() {
    // Fixture review, capture reviews.jsonl, assert context block populated,
    // assert injected_chunk_ids present, assert rendered_prompt_hash present.
}
```

Commit.

---

## Phase 7: CLI surface

### Task 7.1: `ContextDeps` trait for testability

```rust
pub trait ContextDeps {
    type Git: GitOps;
    type Clock: Clock;
    type Embedder: Embedder;
    fn git(&self) -> &Self::Git;
    fn clock(&self) -> &Self::Clock;
    fn embedder(&self) -> &Self::Embedder;
    fn home_dir(&self) -> &Path;
}

pub struct ProdDeps { /* real fastembed, real git2, real clock, ~/.quorum */ }
pub struct TestDeps { /* HashEmbedder, FakeGit, FixedClock, tempdir */ }
```

Then `fn run_context_cmd(cmd: ContextCmd, deps: &dyn ContextDeps) -> Result<CmdOutput>`.

**RED test:** test fake exercises `run_context_cmd(Init)` and asserts files created in TestDeps.home_dir. Commit.

### Task 7.2: `init`, `add`, `list`

Per-subcommand RED tests via `run_context_cmd(cmd, &test_deps)`. Assertions on output + filesystem state. Commit each.

### Task 7.3: `index`, `refresh`, `query`

- `index` = extract_source → ChunkStore::append → rebuild_from_jsonl
- `refresh` = for each source, if `git rev-parse HEAD != stored_HEAD`, run index
- `query` = Retriever::query → pretty-print with `--explain` optional score breakdown

RED tests for each via TestDeps. Commit.

### Task 7.4: `prune`, `doctor`, `doctor --repair`

- `prune` = remove `sources/<name>/` dirs not in config
- `doctor` = run all checks from design §doctor, return pass/fail table
- `doctor --repair` = rebuild db from jsonl, re-embed on model hash mismatch

RED tests for each. Commit.

### Task 7.5: Wire `quorum context` into main CLI argparse

Extend `src/cli.rs` (or equivalent) with `Context(ContextArgs)`. Dispatch to `run_context_cmd`. Compile-check. Smoke test: `cargo run -- context --help` shows subcommands. Commit.

---

## Phase 8: Feedback integration (`context_misleading`)

### Task 8.1: New verdict in feedback store

Extend `Verdict` enum with `ContextMisleading { blamed_chunk_ids: Vec<String> }`. Update JSONL schema version, roundtrip test.

**RED test:**
```rust
#[test]
fn feedback_records_context_misleading_with_chunk_ids() {
    let store = FeedbackStore::new_in_tempdir();
    store.record_context_misleading(
        "file.rs", "finding text", &["chunk-a", "chunk-b"], "deprecated API"
    ).unwrap();
    let entries = store.load_all().unwrap();
    assert_eq!(entries.len(), 1);
    match &entries[0].verdict {
        Verdict::ContextMisleading { blamed_chunk_ids } => {
            assert_eq!(blamed_chunk_ids, &vec!["chunk-a".to_string(), "chunk-b".to_string()]);
        }
        _ => panic!("wrong verdict"),
    }
}
```

Commit.

### Task 8.2: CLI `quorum feedback --verdict context_misleading`

Extend existing `quorum feedback` subcommand. RED test via TestDeps. Commit.

### Task 8.3: Calibrator integration

New method on calibrator: `injection_threshold_for(chunk_id: &str) -> f32`. Starts at `config.inject_min_score`; for each `context_misleading` entry blaming this chunk, raises threshold. After N confirmations (default 3), returns `f32::INFINITY` (full suppress).

```rust
#[test]
fn threshold_rises_then_fully_suppresses_after_n_confirmations() {
    let cal = Calibrator::new(/* default */);
    let id = "chunk-a";
    assert_eq!(cal.injection_threshold_for(id), 0.65);

    cal.record_misleading(id, "fp1");
    assert!(cal.injection_threshold_for(id) > 0.65);
    cal.record_misleading(id, "fp2");
    cal.record_misleading(id, "fp3");
    assert!(cal.injection_threshold_for(id).is_infinite());
}
```

Commit.

### Task 8.4: Wire calibrator into Retriever

At query time, for each candidate, replace `config.inject_min_score` with `max(config.min_score, calibrator.injection_threshold_for(chunk_id))`.

RED test: previously-retrieved chunk disappears from results after 3 `context_misleading` confirmations. Commit.

---

## Post-MVP verification checklist (before release)

Run these manually before merging `feat/context` to main:

1. **Success criteria gate (design §Success criteria):**
   - [ ] Build 60-file calibration set across 3 sources; two reviewers label; Cohen's κ computed; ≥70% relevance
   - [ ] 100-review fake-citation sample < 5%
   - [ ] 50k-LOC source cold index < 120s on M2
   - [ ] Warm p95 retrieval+render < 500ms
   - [ ] All `doctor` checks pass on macOS + Linux
   - [ ] `reviews.jsonl` context block populated for every run

2. **Mutation tests on critical paths:**
   - [ ] `cargo mutants --file src/context/retrieve/rerank.rs` — no surviving mutants (indicates rerank behavior locked in)
   - [ ] `cargo mutants --file src/context/inject/plan.rs` — no surviving mutants

3. **Docs updated:**
   - [ ] `README.md` — add `quorum context` section
   - [ ] `CLAUDE.md` — add context subcommand
   - [ ] `docs/ARCHITECTURE.md` — add context module
   - [ ] `CHANGELOG.md` — entry for v0.16.0

4. **Dogfood on real codebase:**
   - [ ] Register 1 real internal repo
   - [ ] Run 10 reviews, inspect rendered prompts manually
   - [ ] Check for any misleading injections, log `context_misleading` if found

---

## Notes

- **Commit cadence:** target ≥1 commit per Task (i.e., per ~4-step RED-GREEN-commit cycle). Phase boundary = PR.
- **Parallelization:** Phase 2 extractors (2.2, 2.3a, 2.3b, 2.3c) are independent — can dispatch to subagents in parallel.
- **Rollback plan:** every phase is behind `context.auto_inject = false` at the top level. If regression, set false in user config; feature becomes a no-op.
- **Performance checkpoint:** after Task 3.5, benchmark `rebuild_from_jsonl` on mini-rust. If > 5s, investigate before Phase 4 (budget cap is 120s for 50k LOC; mini-rust is ~50 LOC so budget proportional ~120ms).
