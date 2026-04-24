# Issue #29 Design — Context7 dep-based enrichment + Rust support

**Date:** 2026-04-22
**Issue:** [#29](https://github.com/jsnyder/quorum/issues/29)
**Status:** Approved (brainstorm complete, second-opinion review by Gemini 3.1 Pro folded in)

## Problem

`src/context_enrichment.rs::framework_queries` is a hardcoded 11-entry allow-list (react, nextjs, django, fastapi, flask, express, vue, fastify, home-assistant, esphome, terraform). Two consequences:

1. **Zero Rust enrichment.** `detect_domain` populates `languages: ["rust"]` from Cargo.toml, but no Rust framework is in the list, so every Rust review bypasses Context7.
2. **Brittle long-tail coverage.** Python projects using `httpx` / `pydantic-ai` / `langchain`, JS projects using `drizzle` / `zod` / `trpc` get no enrichment despite docs being routinely useful.

## Solution overview

Parse the project's dependency manifest(s) and query Context7 per dep, with an imports-based relevance filter to avoid spamming the API. Keep the curated allow-list as a per-dep query override (curated wins over generic).

## Decisions

| # | Decision | Rationale |
|---|---|---|
| 1 | **Languages parsed in v1: Rust + JS/TS + Python.** Cargo.toml; package.json; pyproject.toml with requirements.txt fallback. Go deferred. | Covers user's primary stack; validates generalization across very different manifest semantics. |
| 2 | **Relevance filter: imports-only, K=5.** Only enrich deps whose name appears in the file's `import_targets` (already plumbed). First 5 in import-occurrence order. | Issue's explicit recommendation. Small files with no recognized imports get no dep enrichment — correct behavior. |
| 3 | **Generic query for uncurated deps: language-aware baselines.** | Negligible extra complexity, better targeting per language. |
| | Rust: `"common pitfalls async safety error handling"` | |
| | Python: `"common pitfalls security type safety"` | |
| | JS/TS: `"common pitfalls security type safety async"` | |
| 4 | **Curated wins; single query per dep.** Refactor `framework_queries(&[String]) -> Vec<(String,String)>` into `curated_query_for(name: &str) -> Option<String>`. New orchestrator `enrich_for_review(deps, curated_frameworks, imports, fetcher)`. | Honors issue's "additive signal" without duplicate calls. HA/ESPHome (directory-detected) still flow through curated path via `domain.frameworks`. |
| 5 | **Failure mode: best-effort + telemetry + in-memory negative cache.** Three new `ReviewTelemetry` counters. `CachedContextFetcher` LRU<lib_name, Option<library_id>> with 24h TTL. | Visibility into degradation; avoids re-hammering Context7 for known-missing names (private crates, typos). Cross-process persistence deferred. |

## Refinements from Gemini 3.1 Pro review

| # | Refinement | Why it matters |
|---|---|---|
| A | **Crate-name normalization (`-` → `_`)** in `parse_cargo`. | Cargo uses `serde-json`; Rust code uses `serde_json`. Without this, the entire Rust path silently misses on hyphenated crates. |
| B | **JS scoped-package handling.** Match `@scope/pkg` verbatim against imports (no splitting). In `build_code_aware_query`, extract `scope` (the framework hint) instead of `pkg` for keyword extraction. | `@nestjs/core` currently degenerates to keyword `core` (useless). Preserve scope as the meaningful signal. |
| C | **Telemetry via return value, not `&mut`.** `enrich_for_review` returns `EnrichmentResult { docs, metrics }`; caller merges into `ReviewTelemetry`. | Pure function, no borrow-checker friction if pipeline goes async, easier to unit test. |

## Module layout

### New: `src/dep_manifest.rs`
```rust
pub struct Dependency {
    pub name: String,        // normalized: hyphens→underscores for Rust; verbatim for @scope/pkg
    pub language: String,    // "rust" | "javascript" | "typescript" | "python"
}

pub fn parse_dependencies(project_dir: &Path) -> Vec<Dependency>;

// Internals (private):
fn parse_cargo(path: &Path) -> Vec<Dependency>;
fn parse_package_json(path: &Path) -> Vec<Dependency>;  // language="typescript" if tsconfig.json sibling, else "javascript"
fn parse_pyproject(path: &Path) -> Vec<Dependency>;     // PEP 621 first, poetry fallback
fn parse_requirements_txt(path: &Path) -> Vec<Dependency>;  // only when pyproject.toml absent
```

### Refactored: `src/context_enrichment.rs`
```rust
pub fn curated_query_for(name: &str) -> Option<String>;
pub fn generic_query_for_language(lang: &str) -> &'static str;

pub struct EnrichmentMetrics {
    pub context7_resolved: u32,
    pub context7_resolve_failed: u32,
    pub context7_query_failed: u32,
}

pub struct EnrichmentResult {
    pub docs: Vec<ContextDoc>,
    pub metrics: EnrichmentMetrics,
}

pub fn enrich_for_review(
    deps: &[Dependency],
    curated_frameworks: &[String],   // for HA/ESPHome path
    imports: &[String],
    fetcher: &dyn ContextFetcher,
) -> EnrichmentResult;

// build_code_aware_query: updated to preserve @scope as keyword for scoped packages.
// CachedContextFetcher: gains LRU<String, ResolveCacheEntry> for negative results, 24h TTL.
```

### Caller change: `src/pipeline.rs:362` and `:689`
```rust
let deps = dep_manifest::parse_dependencies(&project_root);
let result = context_enrichment::enrich_for_review(
    &deps,
    &domain.frameworks,
    &redacted_ctx.import_targets,
    &cached_fetcher,
);
let docs = result.docs;
telemetry.context7_resolved += result.metrics.context7_resolved;
telemetry.context7_resolve_failed += result.metrics.context7_resolve_failed;
telemetry.context7_query_failed += result.metrics.context7_query_failed;
```

### `src/review.rs` — `ReviewTelemetry` additions
```rust
#[serde(default)] pub context7_resolved: u32,
#[serde(default)] pub context7_resolve_failed: u32,
#[serde(default)] pub context7_query_failed: u32,
```

## Edge cases (must have RED tests)

### Cargo
- `tokio = "1"` (string)
- `tokio = { version = "1", features = [...] }` (table)
- `[dev-dependencies]`, `[build-dependencies]` included
- `tokio = { workspace = true }` — name extracted, version ignored
- Workspace root with no `[dependencies]` — empty `Vec`, no panic
- Hyphenated: `serde-json = "1"` → `Dependency { name: "serde_json", … }`

### package.json
- `dependencies`, `devDependencies`, `peerDependencies`, `optionalDependencies` all included
- Scoped: `"@nestjs/core": "^10"` → `Dependency { name: "@nestjs/core", … }` (verbatim)
- Malformed JSON → empty `Vec`, `tracing::warn` logged
- `tsconfig.json` sibling → `language: "typescript"`; absent → `language: "javascript"`

### pyproject.toml
- PEP 621 `[project.dependencies]` happy path
- Poetry `[tool.poetry.dependencies]` happy path (excluding `python` key)
- Both present → PEP 621 wins (deterministic)
- Version stripping: `"fastapi>=0.100"` → `fastapi`
- Extras stripping: `"pydantic[email]>=2"` → `pydantic`

### requirements.txt
- Comments (`#`), blank lines stripped
- Version specifiers stripped: `requests>=2.0` → `requests`
- `-r other.txt`, `-e .`, git URLs (`git+https://…`) skipped
- Only parsed when `pyproject.toml` absent (gating)

### enrich_for_review
- Imports-filter: dep in manifest but not in imports → skipped
- K=5 cap: 10 import-matched deps → 5 docs
- Curated wins: `react` import-matched → curated query used
- Generic fallback: Rust file imports `tokio` → Rust generic query used (`tokio` not yet curated)
- Dedupe by `library` field: same name from manifest + `domain.frameworks` → one doc
- HA path: `domain.frameworks = ["home-assistant"]` with no manifest match → still produces curated docs
- Empty deps + empty frameworks → empty docs, no Context7 calls

### Telemetry / cache
- `enrich_for_review` returns metrics with correct counts on resolve success / resolve fail / query fail
- Negative-cache hit increments `resolve_failed` (treated same as live failure for the metric)
- Negative-cache TTL respected (stale entry → re-call inner fetcher)

## Accepted v1 limitations

- **`tokio::spawn` without `use tokio;`** — depends on upstream import extractor catching root namespace segments. Documented as known false negative.
- **Go manifests** — deferred per scoping decision.
- **Pipfile / setup.py / pdm.lock** — only pyproject.toml + requirements.txt for Python in v1.
- **Workspace member resolution** — Cargo workspaces with member-only `[dependencies]` (root has none) get nothing in v1. Fix is straightforward but deferred.

## Rollout

- **Rust projects**: behavior change is purely additive (was zero docs; now up to 5). Quality risk is one-way upside.
- **Python/JS projects already using curated frameworks**: strict superset. Risk is noisier prompt → marginally slower LLM call. Mitigated by K=5 cap + 5000-token query budget.
- **No flag for opt-out in v1.** Telemetry catches regressions via `findings_by_severity` shifts in `stats --rolling N`. `--no-context7-deps` is a one-line follow-up if needed.

## Out of scope for v1

- Cross-process cache persistence (`~/.quorum/context7_cache.jsonl`)
- Go / Ruby / Java manifests
- Cargo workspace member traversal
- Per-language generic-query tuning beyond the 3 baselines
