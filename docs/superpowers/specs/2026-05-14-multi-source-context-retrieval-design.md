# Multi-Source Context Retrieval

## Problem

The context injection feature currently queries only the first valid source in `sources.toml`. Users register 16+ repos but only one contributes context to reviews. Cross-repo patterns, dependency context, and shared code are invisible to the reviewer.

## Goals

- Fan out retrieval across all indexed sources
- Produce a globally ranked top-k result set with comparable scores
- Preserve existing single-source behavior as a fallback
- Add dependency-manifest awareness as a soft scoring signal
- Emit telemetry to guide future tuning

## Non-Goals

- AST fingerprint enrichment (Phase 2 follow-up)
- Two-stage BM25-first candidate narrowing (Phase 2 if telemetry shows latency pain)
- Precomputed adjacency graph from dependency manifests (Phase 3, may never be needed)

## Architecture

### Change Boundary

The `RetrieverFn` closure in `bootstrap.rs` is the sole change surface. `ContextInjector` and its inject pipeline (NaN filter, floor gate, calibrator gate, precedence, plan, render) remain unchanged. Multi-source logic is entirely within the retriever closure and a new `merge_and_rerank` module.

### Two-Pass Retrieval

**Pass 1 — Per-source retrieval:**

`build_production_injector` collects all valid sources via `filter_map` (replacing the current `find_map`). The retriever closure iterates sources sequentially (Phase 1), computing the query embedding once via the shared `Arc<ProdEmbedder>` before the loop.

Each source:
1. Opens a read-only SQLite connection (fresh per call, as today)
2. Runs the existing `Retriever::query()` with the same `RetrievalQuery`
3. Returns its local top-N candidates (N = `inject_max_chunks * 2`)

Bounded by `max_sources_queried` (default 10, configurable). Sources are sorted by: current-repo first, then by weight descending, then by config order.

**Pass 2 — Merge and re-rank:**

All candidates from all sources are collected into a single pool. Per-source scores are normalized via min-max scaling:

```
normalized = (score - source_min) / (source_max - source_min)
```

For sources returning a single candidate, `normalized = 1.0`.

Multiplicative boosts applied to normalized scores:
- `CURRENT_REPO_BOOST = 1.3` — chunk is from the repo being reviewed
- `DEP_MANIFEST_BOOST = 1.2` — chunk's source is a declared dependency
- `LANG_MATCH_BOOST = 1.1` — chunk's source has the same `kind` as the file under review
- `source.weight` — existing per-source weight from sources.toml (default 1.0)

Boosts compose: a same-language dependency chunk from the current repo gets `1.3 * 1.2 * 1.1 = 1.716x`.

Diversity constraints:
- Reserve `current_repo_reserved` slots (default 2) for current-repo chunks
- Cap non-current repos at `per_source_cap` chunks each (default 3)
- `inject_min_score` applies to source-local scores before normalization (semantics unchanged)
- Reserved slots draw from the min_score survivors only — no backfilling below threshold

Final: sort by boosted normalized score descending, take top `inject_max_chunks`.

### Current-Repo Detection

Path-backed sources only. At bootstrap:
1. `find_project_root()` locates the nearest manifest (Cargo.toml, package.json, etc.)
2. Canonicalize via `std::fs::canonicalize()`
3. For each source with `SourceLocation::Path(p)`, canonicalize `p`
4. Match: `canonical_project_root.starts_with(canonical_source_path)`
5. If canonicalization fails, do not set `is_current_repo`

Git-backed sources never receive the current-repo flag.

### Dependency-Manifest Integration

At bootstrap (once per `quorum review` invocation):
1. `find_project_root()` locates the project manifest
2. `parse_dependencies()` extracts the dep list (existing code in `dep_manifest.rs`)
3. Match dep names against source names (exact match) and `provides` aliases
4. Build `HashSet<String>` of dependency source names for the re-ranker

Matching is name-based. For divergent names, sources declare aliases:

```toml
[[source]]
name = "homeassist"
provides = ["home-assistant-core", "ha-core"]
```

If `parse_dependencies()` fails, proceed without dep boost and log a warning.

## Configuration

### New fields on `[[source]]` (RawSource)

```toml
[[source]]
name = "homeassist"
kind = "rust"
path = "/path/to/repo"
# New optional fields:
include_for = ["quorum-self"]   # only inject into reviews of these source names
exclude_for = ["blockcraft"]    # never inject into reviews of these source names
provides = ["home-assistant-core"]  # dep-name aliases for manifest matching
```

- `include_for` / `exclude_for`: match on source name. Evaluated at bootstrap to build the eligible source list. `exclude_for` wins when both match. Both optional; when omitted, source is eligible for all projects.
- `provides`: optional list of package names this source provides, for dep-manifest matching.

**Compatibility:** `deny_unknown_fields` must be removed from `RawSource` to allow new fields without breaking existing configs. Keep `deny_unknown_fields` on `RawConfig` and `RawContext`.

### New `[context.multi_source]` sub-table (RawContext)

```toml
[context.multi_source]
enabled = true              # default true; false restores single-source behavior
max_sources_queried = 10    # cap on sources queried per review
per_source_cap = 3          # max chunks from any non-current source in final top-k
current_repo_reserved = 2   # guaranteed slots for current-repo chunks
current_repo_boost = 1.3    # optional override for CURRENT_REPO_BOOST
dep_manifest_boost = 1.2    # optional override for DEP_MANIFEST_BOOST
lang_match_boost = 1.1      # optional override for LANG_MATCH_BOOST
```

New sub-table avoids `deny_unknown_fields` issues on `RawContext`. All fields optional with sane defaults.

## Telemetry

Extend `ContextTelemetry` (all `#[serde(default)]` for backward compat):

| Field | Type | Description |
|-------|------|-------------|
| `sources_queried` | `u32` | Source DBs queried in this review |
| `sources_contributing` | `u32` | Sources with chunks in final top-k |
| `per_source_contributions` | `BTreeMap<String, u32>` | source_name -> chunk_count for each contributing source |
| `dep_boost_applied` | `u32` | Chunks that received the dep-manifest boost |
| `current_repo_reserved_available` | `u32` | Reserved slots available |
| `current_repo_reserved_filled` | `u32` | Reserved slots actually filled |

Existing `injected_sources: Vec<String>` already tracks contributing source names.

## Error Handling

- Source DB fails to open or probe: skip with `tracing::warn`, same as current behavior
- All sources fail: return empty injection, `retriever_errored = true`
- `parse_dependencies()` fails: proceed without dep boost, log warning
- `std::fs::canonicalize()` fails: don't set `is_current_repo` for that source
- Single-source fallback: `[context.multi_source] enabled = false` restores `find_map` behavior

## Module Structure

| File | Change |
|------|--------|
| `src/context/bootstrap.rs` | Collect all sources, build multi-source retriever closure |
| `src/context/retrieve/multi_source.rs` | New: `merge_and_rerank()`, score normalization, boost application, diversity constraints |
| `src/context/config.rs` | Remove `deny_unknown_fields` from `RawSource`; add `MultiSourceConfig` sub-table; add `provides`, `include_for`, `exclude_for` to `RawSource` |
| `src/context/retrieve/mod.rs` | Re-export `multi_source` module |
| `src/dep_manifest.rs` | New: `match_sources_to_deps()` helper |
| `src/review_log.rs` | New telemetry fields on `ContextTelemetry` |
| `src/context/inject/injector.rs` | No changes — multi-source is behind RetrieverFn |

## Testing Strategy

- Unit tests for `merge_and_rerank`: score normalization, boost application, diversity caps, reserved slots, edge cases (single source, all sources fail, empty candidates)
- Unit tests for `match_sources_to_deps`: exact match, provides aliases, no manifest
- Integration test: multi-source bootstrap with 2+ fixture sources, verify chunks from both appear in injection
- Config tests: new fields parse correctly, old configs without new fields still parse, `deny_unknown_fields` removal doesn't break validation
- Telemetry tests: new fields populated correctly, backward compat with old records
