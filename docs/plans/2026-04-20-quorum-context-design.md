# quorum context — local/offline alternative to context7

**Status:** design (rev 2, final)
**Date:** 2026-04-20
**Author:** jsnyder, with brainstorm + consensus review by Claude Opus 4.7, gpt-5.4, gemini-3-pro-preview, gpt-5.1-codex. Final pass simplified the working-tree reconciler (per gpt-5.4) and added `context_misleading` feedback tracking (per gpt-5.4).

## Problem

Quorum reviews files by running local AST + ast-grep (53 bundled rules) + LLM passes. Framework docs are pulled from context7 during review. But context7 only covers public, registered libraries.

**Gaps context7 can't fill:**
- Internal services called over HTTP (their OpenAPI spec, route handlers, response shapes)
- Private GitHub repos (proprietary libraries, monorepo siblings)
- Custom Terraform modules (inputs/outputs, required providers)
- Niche OSS that never got indexed externally
- Service-specific conventions — "this function makes sense alone but is misused for this service's flow"

The LLM either hallucinates APIs from these sources or reviews the file in isolation without knowing how it integrates.

## Proposal

`quorum context` — a local indexing + retrieval layer that:

1. Takes registered sources (local checkouts or git remotes) defined in `.quorum/sources.toml`
2. Extracts symbols + docs via ast-grep + tree-sitter + markdown splitting
3. Stores chunks as plain JSONL (our schema, no external format)
4. Derives a SQLite (FTS5 + `sqlite-vec`) index for hybrid retrieval
5. Auto-injects relevant chunks into default reviews (threshold-gated, adaptive budget)
6. Exposes MCP-style tools in `--deep` mode (v2)

**Design ethos:** aligned with quorum's multi-source corroboration model. Multiple extractors produce evidence about the same symbols; hybrid retrieval weighs them together; chunks carry provenance so LLM claims cite source URIs. Retrieval-first, not graph-first.

## Architecture

```
Sources (.quorum/sources.toml)
  ↓
Extractors:
  - ast-grep + tree-sitter     — symbols, signatures, doc comments
  - Markdown splitter          — README, /docs, ADRs, CHANGELOG
  - Schema parsers (v2)        — OpenAPI/GraphQL/proto for "service" kind
  ↓
Chunk store (plain JSONL per source)
  - Our own schema — no SCIP, no protobuf
  - chunks.jsonl is the source of truth on disk
  ↓
Derived search index (single SQLite file)
  - FTS5 virtual table for BM25 (exact identifiers)
  - sqlite-vec virtual table for vector search (fastembed bge-small-en-v1.5)
  - Rebuildable from chunks.jsonl at any time
  ↓
Hybrid retrieval
  1. BM25 over FTS5 (identifier lookup)
  2. Vector search (conceptual)
  3. Merge + dedupe + metadata filter
  4. Rerank (additive + clamped multiplicative boosts — see §Retrieval)
  5. Adaptive threshold with budget spillover
  ↓
Consumers:
  a) Auto-inject (MVP) — markdown-rendered chunks spliced into review prompt
  b) Tool surface (v2) — search_docs, get_symbol, expand_callers, etc.
```

### Why no SCIP

Earlier revisions proposed SCIP as the on-disk format. Dropped in this revision because:

- SCIP's canonical moniker format (`<scheme> <package_manager> <package_name> <version> <descriptor>`) assumes type-resolved package metadata. Ast-grep extraction cannot honestly produce that — we'd be emitting `git local <source> <commit>` placeholders and calling them "canonical," which they aren't.
- The canonical-symbol-name-for-interop argument was the main benefit; without it, SCIP is just protobuf wrapping around our data.
- `scip snapshot` golden testing is nice but not worth committing to a format whose core semantics we can't honestly satisfy.
- If a user later wants to ingest real SCIP output (e.g., a team already runs `scip-typescript` in CI), we can add a SCIP *reader* then without needing to be a SCIP *writer* now.

Net: simpler code, one fewer dep surface, no pretense of canonicity we can't back up.

## Source registration

`.quorum/sources.toml` (project-local) or `~/.quorum/sources.toml` (global):

```toml
[[source]]
name = "internal-auth"
git = "git@github.com:myorg/auth-service.git"
rev = "main"                    # or pinned SHA
kind = "service"                # rust | typescript | python | go | terraform | service | docs
paths = ["src/", "docs/", "openapi.yaml"]
weight = 10                     # precedence tiebreaker (higher wins)

[[source]]
name = "tf-networking"
path = "../terraform-modules/networking"
kind = "terraform"

[context]
auto_inject = true
inject_budget_tokens = 1500
inject_min_score = 0.65
inject_max_chunks = 4
rerank_recency_halflife_days = 90
rerank_recency_floor = 0.25     # clamp recency_decay to [0.25, 1.0] — prevents old-but-authoritative chunks from being annihilated
max_source_size_mb = 200
ignore = ["target/", "node_modules/", "vendor/", "*.lock", "generated/"]
```

**Private repos:** honors local git credentials (SSH keys, HTTPS tokens via `~/.git-credentials`). No new auth surface.

**MVP: explicit registration only.** Auto-discovery from manifests (Cargo.toml / package.json / go.mod / terraform blocks / pyproject.toml) deferred to v2.

## Extractors

### ast-grep + tree-sitter (primary, all languages)

Already integrated in quorum via `ast-grep-core/config/language 0.42.1`. Reused to:
- Enumerate top-level declarations per file (fn, struct, class, enum, trait, interface, type alias)
- Extract signatures (syntactic; no type resolution)
- Pull adjacent doc comments (`///`, `"""docstring"""`, `/** JSDoc */`)
- Detect exported vs private via language-specific rules (pub, export, `__all__`, etc.)
- Extract neighboring symbols (same module/file/class) for locality signals

Per-language symbol-extractor rules live in `rules/<lang>/extraction/*.yml` — same mechanism as existing bundled ast-grep rules. Each emits `(symbol_name, signature, doc_comment, source_range, neighbors[])`.

### Markdown splitter (all sources)

Scans `README*`, `/docs/**/*.md`, `CHANGELOG.md`, `ARCHITECTURE.md`, `.quorum/notes/*.md` per registered source. Splits by heading hierarchy; preserves code blocks. Each top-level section becomes one chunk with metadata: `heading_path`, `source_file`, `neighboring_headings`.

**Heading normalization:** at render time, headings inside chunk content are demoted so they don't collide with the injection wrapper's own heading levels (see §Rendering).

### Schema parsers (v2)

For `kind = "service"` sources with OpenAPI / GraphQL / proto specs: parse routes/types/operations into structured Schema chunks. Deferred; MVP treats these as plain markdown.

## Chunk schema

```rust
enum ChunkKind {
    Symbol,   // function, type, class, trait, variable, tf-var, tf-output
    Doc,      // prose section from markdown (subtype: README | ADR | CHANGELOG | notes)
    Schema,   // endpoint, API operation, message type (subtype: REST | GraphQL | proto | tf-provider)
}

struct Chunk {
    id: String,                    // format: "<source>:<path>:<symbol-or-heading-slug>"
    source: String,                // "internal-auth"
    kind: ChunkKind,
    subtype: Option<String>,       // "ADR", "Endpoint", "Config", "Test", ...
    qualified_name: Option<String>,// "auth::verify_token" for symbols
    signature: Option<String>,     // syntactic from ast-grep
    content: String,               // hover card body — what gets rendered to LLM
    metadata: ChunkMeta,
    provenance: Provenance,
}

struct ChunkMeta {
    source_path: PathBuf,
    line_range: (u32, u32),
    commit_sha: String,
    indexed_at: DateTime<Utc>,
    source_version: Option<String>,
    language: Option<String>,
    is_exported: bool,
    neighboring_symbols: Vec<String>,
}

struct Provenance {
    extractor: String,             // "ast-grep-rust", "markdown-splitter", ...
    confidence: f32,
    source_uri: String,            // git://internal-auth@abc123/src/token.rs#L45-62
}
```

Chunk IDs are unique within quorum's world but make no claim of global canonicity. Rationale: honest, debuggable, doesn't pretend to be something it isn't.

## Storage layout

```
~/.quorum/context/
├── sources/
│   ├── internal-auth/
│   │   ├── HEAD                  # last-indexed commit SHA (plain text)
│   │   ├── chunks.jsonl          # canonical chunk store (source of truth)
│   │   └── manifest.json         # kind, paths, last_indexed_at, error log, ignore-diagnostics
│   └── tf-networking/
│       └── ...
├── index.db                      # SQLite: FTS5 + sqlite-vec (derived, rebuildable)
└── state.json                    # schema version, embedder model hash, quorum version
```

- `chunks.jsonl` per source is the source of truth. Human-readable, diffable, easy to audit.
- `index.db` is a single SQLite file (FTS5 + `sqlite-vec` both in the same DB) — one transaction model, no dual-index consistency bugs.
- Rebuilding the SQLite index from JSONL is straightforward and checks `state.json.embedder_model_hash` before reusing cached vectors (re-embeds on model change).

## Retrieval

```rust
struct RetrievalQuery {
    text: String,
    identifiers: Vec<String>,
    filters: Filters {
        sources: Option<Vec<String>>,
        kinds: Option<Vec<ChunkKind>>,
        languages: Option<Vec<String>>,
        min_commit_date: Option<DateTime<Utc>>,
    },
    k: usize,
    min_score: f32,
}
```

### Rerank formula (revised)

Earlier draft used pure multiplicative stacking:
```
score = blended * (1 + id_boost) * (1 + path_boost) * recency_decay
```
Problem (flagged by gpt-5.1-codex): if any factor goes to zero, the whole score collapses — an old but authoritative ADR gets annihilated regardless of relevance. Revised:

```
blended     = 0.6 * bm25_norm + 0.4 * vec_norm                    // min-max normalized per query
id_boost    = 1.0 if identifier_exact_match else 0.0              // additive
path_boost  = 0.5 if source_lang matches reviewed_file_lang else 0.0  // additive
recency_mul = clamp(exp(-ln(2) * age_days / halflife), 0.25, 1.0) // floor prevents annihilation

score = (blended + id_boost + path_boost) * recency_mul
```

- Normalization: BM25 and vector scores are min-max normalized **per query** (so identical BM25 scores on short docs don't collapse the blend to pure vector).
- Boosts are additive, so missing a boost lowers score but doesn't zero it.
- Recency is multiplicative but floored at 0.25, so a 5-year-old canonical spec still ranks if other signals are strong.
- All component scores are logged to `reviews.jsonl` for telemetry and tuning.

### Identifier harvesting with fallback

Primary query identifiers come from tree-sitter hydration (`non_stdlib_symbols()`). But in DSL-heavy or dynamic-dispatch files, that set can collapse to generic names like `Client` / `Handler` that starve BM25. Fallback strategy:

```
if len(identifiers) < 2 OR all(id in GENERIC_STOPLIST for id in identifiers):
    augment with:
      - reviewed_file's module path segments
      - neighboring_symbols of reviewed declarations
      - distinctive imports (non-stdlib, non-wildcard)
```

`GENERIC_STOPLIST` is a per-language list of overly-common names (`Client`, `Handler`, `Config`, `Error`, etc.) loaded from `rules/<lang>/generic-names.yml`.

### Adaptive threshold with budget spillover

gpt-5.1-codex's load-bearing suggestion: the original design ran separate symbol + prose queries with hard τ, so if symbols starved, prose quota wasted even though prose was what the review needed. Revised:

```rust
// Phase 1: primary query (symbols)
let symbols = query(kind = Symbol|Schema, min_score = τ);
let symbol_budget_used = render_tokens(symbols);

// Phase 2: secondary query (prose) with adaptive threshold
let prose_budget = inject_budget_tokens - symbol_budget_used;
let τ_prose = if symbols.len() < inject_max_chunks / 2 {
    τ - 0.10  // lower threshold if symbols starved
} else {
    τ
};
let prose = query(kind = Doc, min_score = τ_prose, budget = prose_budget);

// Phase 3: if total is still under 40% of inject_budget_tokens, skip injection entirely
if render_tokens(symbols + prose) < 0.4 * inject_budget_tokens {
    inject nothing  // no weak filler
}
```

Still threshold-gated overall. Still drops to zero injection if nothing clears the bar. But doesn't waste unused symbol quota on nothing when prose could fill in.

## Consumer 1: Auto-inject (MVP)

```rust
let refs = hydrated.non_stdlib_symbols();
let identifiers = augment_with_fallback(refs, &file);

let symbol_chunks = context::query(RetrievalQuery {
    identifiers,
    filters: Filters { kinds: Some(vec![Symbol, Schema]), .. },
    k: config.inject_max_chunks,
    min_score: config.inject_min_score,
    ..
})?;

let prose_chunks = context::query_adaptive(RetrievalQuery {
    text: file.module_path().to_string(),
    filters: Filters { kinds: Some(vec![Doc]), .. },
    budget: remaining_budget,
    min_score: adaptive_threshold(config.inject_min_score, symbol_chunks.len()),
    ..
})?;

let context_block = render(
    symbol_chunks,
    prose_chunks,
    budget = config.inject_budget_tokens,
    reconcile_against = &workspace,
);
prompt.push(context_block);
```

### Rendering

Markdown renderer for MVP. Each chunk becomes a self-contained card:

```markdown
## Referenced context

### `auth::verify_token` — internal-auth @ abc123
```rust
pub fn verify_token(token: &str, opts: VerifyOpts) -> Result<Claims, AuthError>
```
> Validates a JWT against the service's signing key. Errors with `AuthError::Expired`
> if token.exp is in the past. Side effect: writes verification attempt to audit log.
>
> Source: git://internal-auth@abc123/src/token.rs#L45-62

### ADR-014: Token verification — internal-auth
> We chose HS256 over RS256 because... [180 tokens]
>
> Source: git://internal-auth@abc123/docs/adr/014-tokens.md

_context: 1340 tokens across 3 chunks from 1 source, all fresh (<7d)_
```

**Heading normalization:** chunk content is scanned for markdown headings; any heading at a shallower level than the injection wrapper's level+2 is demoted (e.g., `##` in chunk content becomes `####` when wrapped under `### card`). Prevents chunk content from breaking out of its card visually or structurally.

**Conflict annotation:** when precedence rules selected a source over a near-duplicate, the card footer shows:
```
_precedence: internal-auth wins over internal-auth-fork (weight 10 > 5)_
```
So if the LLM cites the wrong source, there's an obvious signal in the prompt that clarifies.

**Staleness annotation:** chunks suppressed or annotated by the working-tree reconciler get marked:
```
### `auth::verify_token` ⚠ local edits diverge from indexed version
```

The LLM is prompted to cite chunk `source_uri` (or `chunk_id`) when making claims referencing this context. Post-hoc, quorum verifies citations resolve and logs `fake_citation_count`.

## Consumer 2: Tool surface (v2)

Exposed via quorum's existing MCP server infrastructure:

```
search_docs(query, filters?, k?)  → Chunk[]
get_symbol(qname)                  → Chunk
get_chunk(chunk_id)                → Chunk
expand_callers(symbol, depth?)     → Chunk[]   // intra-source
list_endpoints(source)             → Schema[]
diff_since(source, since)          → {added, modified, removed: Chunk[]}
list_sources()                     → Source[]
```

Deferred from MVP. Auto-inject proves retrieval quality first.

## Working-tree reconciler

Chunks from the local project can become stale when the reviewer has uncommitted changes. Earlier drafts proposed full staged/worktree blob-SHA tracking; gpt-5.4's final review flagged this as the most code-heavy new subsystem with gnarly partial-staging edge cases, and recommended deferring to v2.

**MVP approach: timestamp-based staleness annotation.**

For each injected chunk whose source corresponds to the current project (same repo as the reviewed file), the renderer checks `chunk.metadata.indexed_at` against the most recent local change time. If the source has modifications since indexing, the chunk card gets a header annotation:

```markdown
### `auth::verify_token` ⚠ source has edits since last index (2h ago)
```

Detection is crude but honest: `git status --porcelain` for the source's paths. Any non-clean status → annotate all chunks from that source as potentially stale. No per-chunk blob-SHA tracking, no attempt to distinguish staged vs. worktree edits. The LLM gets the signal; we don't silently suppress useful context.

Users are prompted by `quorum context list` to run `quorum context refresh` when indexed HEAD doesn't match working HEAD. The common path — "refresh before review" — keeps most chunks Fresh without needing reconciliation at all.

**v2 upgrade path:** the full staged/worktree blob-SHA reconciler (earlier drafts) moves to v2 behind `context.stale_tracking = timestamp | blob_sha` config flag. Ship once we have telemetry data showing how often timestamp-only misfires.

## Relevance evaluation harness

`reviews.jsonl` extends with context fields:

```jsonl
{
  "run_id": "01HW...",
  ...,
  "context": {
    "chunks_queried": 8,
    "chunks_retrieved_above_threshold": 5,
    "chunks_injected": 3,
    "chunks_suppressed_budget": 2,
    "chunks_suppressed_stale": 0,
    "sources_hit": ["internal-auth"],
    "injected_chunk_ids": ["internal-auth:src/token.rs:verify_token", ...],
    "rendered_prompt_hash": "sha256:...",      // to validate fake-citation detection against actual rendered context
    "cited_chunk_ids": ["internal-auth:src/token.rs:verify_token"],
    "fake_citations": [],
    "stale_annotations": 0,
    "rerank_score_breakdown": {                // for tuning
      "bm25_norm": [0.91, 0.78, 0.62],
      "vec_norm":  [0.81, 0.84, 0.71],
      "id_boost":  [1.0, 1.0, 0.0],
      "path_boost":[0.5, 0.5, 0.5],
      "recency_mul":[1.0, 0.92, 0.71]
    }
  }
}
```

New stats dimensions:
- `quorum stats context --by-source` — coverage, citation, fake-citation rates per source
- `quorum stats context --by-reviewed-repo` — context-hit rate when reviewing repo X

Metrics derivable from this:
- **Context coverage** = % of reviewed files with ≥1 chunk above threshold
- **Citation rate** = % of injected chunks cited in LLM output
- **Fake citation rate** = % of cited chunk ids that don't resolve (validates against `rendered_prompt_hash`, so we distinguish "LLM hallucinated an id" from "budget clip removed the chunk")
- **Helpful comment rate** = joined with feedback store via `run_id` — TP rate for findings on files with injected context vs. without
- **Context-misleading rate** = joined with feedback store — % of findings flagged as caused by misleading retrieved context (see below)

### Negative prompt impact tracking

Relevance metrics measure whether retrieved context is on-topic; they don't measure whether it actively *misleads* the LLM. A chunk can score high yet mislead if its content is deprecated, partially out-of-date, or reflects an earlier API version. To catch this, extend the feedback store with a new verdict:

```
quorum feedback --file X --finding "..." --verdict context_misleading \
    --reason "injected chunk referenced deprecated fn still documented in README"
```

`context_misleading` verdict semantics:
- **Not a TP/FP axis** — orthogonal. A finding can be a true positive that was made worse by misleading context, or a false positive caused by misleading context.
- When recorded, the verdict captures which injected chunk ids the reviewer blames.
- In aggregate (`quorum stats context --misleading`), surfaces chunks that recur in misleading-flagged reviews — candidates for suppression, reindex, or source-level deprecation marking.

Calibrator treatment: `context_misleading` feedback increases the injection threshold for the specific chunk id (soft suppression at first, full suppression after N confirmations), without affecting TP/FP calibration for unrelated findings.

This is the "fail-safe" for retrieval quality: relevance scores catch when we retrieve the wrong thing; `context_misleading` catches when we retrieve the right thing but at the wrong moment in its lifecycle.

---

Telemetry is how we know when the ranking formula is wrong. Tuning without it is guessing.

## Source precedence + conflict resolution

Multi-source name collision (same qualified_name from sources A and B):

1. `[[source]] weight` value — higher wins
2. Direct vs transitive (v2 once discovery exists — direct wins)
3. More recent `commit_sha` of the winning entry
4. Alphabetical source name (deterministic fallback)

`quorum context query --explain` shows which source won and the reason. `render` includes a precedence footer annotation when a near-duplicate was suppressed (see §Rendering).

## Ignore rules

Per-source + global + `.gitignore`. Precedence spec (undefined in rev 1, now explicit):

1. Per-source `ignore` patterns apply first (most specific)
2. Global `[context].ignore` patterns apply second
3. `.gitignore` rules apply last
4. Within each tier, later patterns override earlier ones (standard gitignore semantics)
5. `!pattern` negations work as in gitignore: a negation can re-include a file ignored by an earlier rule **within the same tier**, but not across tiers (a global `ignore` wins over a per-source `!ignore`)

Each indexing run writes per-source diagnostics to `manifest.json`:
```json
{
  "ignore_diagnostics": {
    "total_files_scanned": 1523,
    "skipped_by_tier": { "per_source": 112, "global": 45, "gitignore": 289 },
    "top_skipped_globs": [["target/", 289], ["node_modules/", 156], ...]
  }
}
```

Debuggable: `quorum context list --ignore-stats` shows why files vanished.

## CLI surface

```
quorum context init
quorum context add <name> --git <url> --kind <kind>
quorum context list [--ignore-stats]
quorum context index [<name>|--all]
quorum context refresh
quorum context query "<text>" [--source X] [--kind Y] [--explain]
quorum context prune
quorum context doctor [--repair]
```

### `doctor [--repair]` checks

Defined (undefined in rev 1):

| Check | Pass criteria |
|---|---|
| Disk space | ≥ 500 MB free in `~/.quorum/context/` |
| Git auth | Each `git:` source's `git ls-remote` succeeds |
| JSONL integrity | Each `chunks.jsonl` parses cleanly |
| SQLite integrity | `PRAGMA integrity_check` passes on `index.db` |
| Embedder version | `state.json.embedder_model_hash` matches current `fastembed` model |
| HEAD freshness | For each source, `stored_HEAD` vs `git rev-parse` on source's default branch — warn if > 30 days |
| Ignore rules sanity | Warn if ignore rules exclude >95% of files in a source (probably misconfigured) |

`--repair` flow:
1. Integrity-check each JSONL; any corrupt file triggers source re-index
2. If SQLite integrity fails, rebuild `index.db` from JSONLs
3. If `embedder_model_hash` mismatches, re-embed all chunks into `index.db`
4. Prune orphaned source directories no longer in config
5. Log before/after sizes and elapsed time

## Failure modes

| Scenario | Behavior |
|---|---|
| Source clone fails | Keep previous index, mark stale in `list`, warn at review |
| Private repo unauthenticated | Skip source, one-time remediation warning |
| Source > max_source_size_mb | Skip, warn. Bypass via `--force` |
| ast-grep extractor crashes on file | Skip file, log to `manifest.json`, continue |
| SQLite corrupt | `quorum context doctor --repair` rebuilds from JSONL |
| Embedder model change | `state.json` tracks hash; `doctor --repair` reembeds |
| Retrieval returns nothing above threshold | Review proceeds with no injection (no-op) |
| Reviewed file path diverged from index | Working-tree reconciler annotates per freshness state |
| Same qname in multiple sources | Precedence rules; losing source annotated in render |

## Privacy posture

- All indexes stored in `~/.quorum/context/` — user-local, no daemon, no network egress
- `chunks.jsonl` is plain text, trivially auditable
- `quorum context list --paths` shows exactly what's indexed where
- `quorum context prune` removes sources cleanly
- Private repo content leaves the machine only to whatever LLM the user configured (same trust boundary as reviews themselves)

## MVP scope (locked)

**In scope:**
- Explicit `.quorum/sources.toml` registration with `weight` field
- ast-grep + tree-sitter extraction
- Markdown splitter for docs
- Plain JSONL chunk store (no SCIP, no protobuf)
- SQLite (FTS5 + `sqlite-vec`) derived index
- Hybrid BM25 + vector retrieval with:
  - Min-max score normalization per query
  - Additive boosts (no zero-collapse)
  - Clamped recency decay (floor 0.25)
  - Identifier fallback for thin identifier sets
  - Adaptive threshold with budget spillover
  - Minimum-injection floor (skip injection if < 40% of budget clears)
- 3-kind chunk taxonomy (`Symbol | Doc | Schema`) with subtype metadata
- Auto-inject in default review (1500-token budget, max 4 chunks)
- Markdown renderer with heading normalization + precedence + staleness annotations
- Timestamp-based staleness annotation for local-source chunks (blob-SHA tracking deferred to v2)
- Relevance telemetry in `reviews.jsonl` + `stats context` dimensions
- `context_misleading` feedback verdict with soft/full suppression via calibrator
- Source precedence rules (weight → recency → alphabetical)
- Ignore-rule precedence spec with per-source diagnostics
- `quorum context {init, add, list, index, refresh, query, prune, doctor}`

**Explicitly deferred:**
- Auto-discovery from manifests
- Tool surface for `--deep` (search_docs, get_symbol, expand_callers, etc.)
- OpenAPI / GraphQL / proto schema parsers
- Lazy background refresh
- Daemon watch-mode
- Transitive dep indexing
- Real SCIP ingestion (as reader, not writer)
- Find implementations / trait resolution
- Blob-SHA-based working-tree reconciler (staged vs. worktree) with four-state annotation — enable via `context.stale_tracking = blob_sha`
- TOON renderer as alternative to markdown — candidate for A/B via eval harness
- PageRank / graph-based importance (cf. codemem)
- CO_CHANGED edges from git history as retrieval signal

## v2 experiments (measurable via eval harness)

These are features we've considered but need data before committing to:

| Experiment | What we measure |
|---|---|
| **TOON renderer vs markdown** | Token savings vs. citation accuracy vs. finding TP rate. Uses `toon-format` crate behind config flag. |
| **Auto-discovery from manifests** | Does including direct deps actually help review quality, or just bloat indexes? Compare helpful-comment-rate on registered-only vs. discovered. |
| **Score weight tuning** | BM25/vec blend weights (0.6/0.4 default) — sweep and measure. |
| **Adaptive τ parameters** | Is 0.10 the right threshold-drop for prose fallback? Sweep 0.05–0.20. |
| **Extended chunk kinds** | Does adding `Example` and `Test` kinds help, or is subtype metadata enough? |
| **Re-embed rate** | Does re-embedding on every reindex vs. content-hash caching matter for cold-start times? |

## Key dependencies added

- `rusqlite` with FTS5 feature (bundled)
- `sqlite-vec` — in-SQLite vector search
- `fastembed` — reused from quorum's feedback embeddings
- `serde_json` — reused
- `gix` or `git2` — whichever quorum already uses (for blob-SHA operations in working-tree reconciler)

No new external binaries. No new services. No new auth paths. No protobuf.

## Non-goals

- Not a Sourcegraph replacement. Not a general code intelligence platform.
- Not trying to match type-resolved precision of a real language server.
- Not indexing the internet — sources must be explicitly registered.
- Not building a knowledge graph as primary storage.
- Not claiming canonical cross-repo symbol resolution — our chunk IDs are unique within quorum, no broader claim.

## Risks

Addressed from frontier-model review (2026-04-20):

| Risk | Mitigation |
|---|---|
| False confidence from wrong-but-cited context | Hard threshold, precedence transparency in render, `--explain`, fake-citation telemetry |
| Multiplicative rerank collapse | Additive boosts + clamped recency floor |
| Thin identifier starvation | Fallback augmentation from module path + neighbors |
| Working-tree reconciler oscillation under partial staging | MVP uses timestamp-based annotation only; blob-SHA tracking moves to v2 once usage data shows it's needed |
| Retrieved context misleads LLM into worse suggestions | `context_misleading` feedback verdict + calibrator suppression for repeat-offender chunks |
| Ignore-rule precedence ambiguity | Explicit spec + per-source diagnostics |
| Stale vectors after embedder change | `state.json.embedder_model_hash` + `doctor --repair` re-embed |
| BM25 score collapse on short docs | Min-max normalization per query |
| Chunk heading collisions in render | Heading-level normalization at render time |
| Scope creep toward code intelligence platform | Hard constraint: tool surface deferred, no graph DB, everything under `quorum context` subcommand |

## Success criteria (revised per gpt-5.1-codex critique)

MVP ships when:

1. **Relevance:** on a hand-labeled calibration set of **≥60 files across ≥3 sources**, ≥70% of injected chunks are rated relevant by two independent reviewers (Cohen's κ reported).
2. **Fake citation rate:** < 5% on a 100-review sample, measured against `rendered_prompt_hash` so we distinguish "LLM hallucinated an id" from "budget clip removed the chunk."
3. **Cold indexing time:** < 120s for a 50k-LOC source on baseline hardware (M2 MacBook Air, 16GB), measured from `git clone` through fully populated SQLite. Warm reindex (HEAD change with 10 files modified) < 15s.
4. **Injection overhead:** < 500ms p95 for retrieval + rerank + render on a warm SQLite cache. Cold-cache first-query latency reported separately (expected 1–3s).
5. **Doctor pass:** all checks in §doctor pass on macOS + Linux with vanilla install. Documented checklist.
6. **Eval harness coverage:** all metrics from §eval harness emitted in `reviews.jsonl` for every review run in MVP. No feature ships without measurement.

## Progressive milestones

| Phase | Capability |
|---|---|
| **MVP** | Single-file occurrences + symbols, hover documentation, auto-inject with eval harness |
| **v2** | Tool surface, schema parsers, auto-discovery, TOON renderer experiment |
| **v3** | Real SCIP ingestion (as reader), implementation relationships, transitives |
| **v4** | Find implementations, call graphs beyond intra-source, CO_CHANGED retrieval signal |

## Open questions

1. **Embedding budget per source.** ~2000 chunks for a 50k-LOC crate, ~20s at 100 chunks/s on fastembed. Acceptable for MVP. Alternative (lazy-embed top-N) tracked as v2 experiment.
2. **Max chunk size.** 8k-token cap with 200-token overlap for longer docs. Revisit after first real-world test.
3. **Prebuilt-index distribution.** For v2+ CI: should teams share prebuilt chunk stores? JSONL is portable; decide when we have a use case.
4. **Conflict with context7.** When both can answer, prefer local. Per-source-kind override configurable. Track which source actually got cited in telemetry.

## References

- Brainstorm + consensus review: this conversation, 2026-04-20
- Sourcegraph indexer-writing guide: https://sourcegraph.com/docs/code-navigation/writing-an-indexer
- codemem pipeline (full-fat version of this idea): https://github.com/cogniplex/codemem/blob/main/docs/pipeline.md
- TOON format (v2 render experiment candidate): https://github.com/toon-format/toon
- `toon-format` Rust crate: https://crates.io/crates/toon-format
- Existing telemetry: `docs/plans/2026-04-19-reviews-jsonl-and-stats-dimensions.md`
- ast-grep library integration: `docs/plans/2026-04-13-ast-grep-library-integration.md`
