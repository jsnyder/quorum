# AST Structural Fingerprinting for Cross-Repo Context Retrieval

## Problem

The context injection retrieval system uses three legs: BM25 (text), vector embedding (semantic), and structural name match (exact qualified name). All three rely on textual or naming overlap. When two functions in different repos have the same structural shape (similar signature, similar control flow) but different names and terminology, none of the existing legs can find them. This is common across repos: utility parsers, validation functions, adapter patterns, and CRUD handlers often share structure but not vocabulary.

## Goals

- Add a fourth retrieval leg that finds structurally similar code across indexed repos
- Compute a compact, deterministic fingerprint per code symbol at index time
- Support Rust, Python, and TypeScript in the MVP
- Make the structural signal fully ablatable (toggle on/off with zero impact on existing quality)
- Emit telemetry to measure the leg's contribution before tuning weights

## Non-Goals

- Review precedent calibration using structural fingerprints (Phase 2 follow-up)
- Learned embeddings or autoencoder-based fingerprints (too much upfront work for uncertain payoff)
- Cross-language matching (fingerprints are same-language only in MVP; cross-language is a future concern)
- User-facing configuration (internal constants only until telemetry proves the knobs matter)

## Architecture

### Change Boundary

The fingerprint is computed at index time in the extraction pipeline and stored in a new sqlite-vec table. At query time, it adds a fourth retrieval leg that feeds candidates into the existing reranker. The reranker applies the structural signal as an additive boost, preserving the existing BM25/vector blend unchanged. `ContextInjector` and the inject pipeline are not modified.

### Fingerprint Computation

#### Type Category Vocabulary

Shared across languages, used to generalize concrete types into structural categories:

| Category | Token | Examples (Rust / Python / TS) |
|----------|-------|-------------------------------|
| Primitive | `prim` | u32, bool, f64 / int, float, bool / number, boolean |
| String | `str` | String, &str / str / string |
| Collection | `col` | Vec, HashMap, &[] / list, dict, set / Array, Map, Set |
| Option | `opt` | Option / Optional, None union / T \| undefined, T \| null |
| Result | `res` | Result / (raises pattern) / Promise (error union) |
| Reference | `ref` | &T, &mut T / - / - |
| Callback | `fn` | Fn, FnMut / Callable / (...) => T |
| Self | `self` | &self, &mut self / self / this |
| Unknown | `unk` | Python duck-typed params, TS complex unions where type cannot be classified |
| Generic | `T` | any intentionally generic user-defined type |

`unk` vs `T`: `T` means the code intentionally uses a generic/type parameter. `unk` means the fingerprinter could not determine the type (common in Python without annotations, TS complex unions). This distinction prevents false matches between "intentionally generic" and "couldn't determine."

#### Canonical Signature Format

`(self, ref col<T>, ref T, ref T, prim) -> col<T>`

One level of generic nesting preserved: `Result<Vec<T>>` becomes `res<col<T>>`. Deeper nesting flattens to `T`.

#### 64-Dimensional Structured Vector Encoding

Reserved dimension ranges with specific signals:

| Dims | Signal | Encoding |
|------|--------|----------|
| 0-7 | Signature shape | arity (normalized), param category histogram (prim/str/col/opt/res/ref/fn/unk counts, normalized to sum=1) |
| 8-15 | Return type | category one-hot (8 categories) + nesting depth + result/option wrapping flags |
| 16-23 | Self/receiver | has_self, is_mut_self, is_static, is_method, is_constructor, padding (zeros) |
| 24-39 | Parameter pattern | first 4 params encoded positionally (4 params x 4 dims each: category one-hot over {prim, str, col, ref, T/unk}) |
| 40-47 | Global shape | log1p(body_node_count), max_depth, mean_depth, leaf_ratio — all scaled to [0,1] within this family |
| 48-55 | Control-flow sketch | log1p of: branches, loops, early_returns, error_prop, unsafe, match_arms, closures, awaits — scaled to [0,1] within family |
| 56-63 | Semantic counts | log1p of: calls, assignments, member_access, index_ops, binary_ops, collection_literals, type_annotations, lambdas — scaled to [0,1] within family |

Normalization: dims 40-63 use `log1p(count)` then min-max scale within each 8-dim family. This prevents high-count features (e.g., many calls) from dominating signature-shape features in cosine similarity.

### Schema Changes

New sqlite-vec table per source DB (alongside existing `chunks_vec`):

```sql
CREATE VIRTUAL TABLE chunks_struct_vec USING vec0(
    id TEXT PRIMARY KEY,
    embedding FLOAT[64]
);
```

Only `ChunkKind::Symbol` chunks with a valid fingerprint (body_node_count >= 10) are inserted. Non-code chunks (`Doc`, `Schema`) and trivial symbols (getters, single-line delegates) are omitted entirely — no zero vectors.

State table: new `fingerprint_version` key (e.g., `"structural-v1"`). On mismatch during `context index` or `context refresh`, all fingerprints are recomputed. Same pattern as `embedder_model_hash`.

Schema version: bump from 1 to 2. Existing DBs without `chunks_struct_vec` get the table added via migration on first access. Chunks without fingerprints are invisible to the structural leg (KNN returns empty), which is correct degradation.

### Extraction Pipeline

Fingerprinting hooks into the existing extraction flow:

```
source file -> tree-sitter parse -> extract chunks (existing)
                                         |
                                    compute fingerprint (new)
                                         |
                               insert into chunks + chunks_fts + chunks_vec + chunks_struct_vec
```

#### Fingerprinter Trait

```rust
pub trait Fingerprinter {
    fn fingerprint(&self, node: &tree_sitter::Node, source: &[u8]) -> Option<StructuralFingerprint>;
}
```

Returns `None` for nodes that don't qualify (non-function, body too small). Per-language implementations: `RustFingerprinter`, `PythonFingerprinter`, `TypeScriptFingerprinter`.

#### Output Types

```rust
pub struct StructuralFingerprint {
    pub signature: SignatureShape,
    pub control_flow: ControlFlowSketch,
    pub semantic_counts: SemanticCounts,
}

impl StructuralFingerprint {
    pub fn to_vector(&self) -> [f32; 64] { /* encode per dimension layout above */ }
}
```

`SignatureShape` captures arity, parameter categories (positional and histogram), return type category, self/receiver flags.

`ControlFlowSketch` captures counts of branches, loops, early returns, error propagation (`?` in Rust, try/except in Python, try/catch in TS), unsafe blocks, match arms, closures, awaits.

`SemanticCounts` captures counts of calls, assignments, member access, index ops, binary ops, collection literals, type annotations, lambdas.

#### Module Structure

| File | Purpose |
|------|---------|
| `src/context/extract/fingerprint.rs` | `Fingerprinter` trait, `StructuralFingerprint`, `to_vector()`, type category vocabulary, normalization |
| `src/context/extract/fingerprint_rust.rs` | Rust-specific AST walking and type classification |
| `src/context/extract/fingerprint_python.rs` | Python-specific AST walking and type classification |
| `src/context/extract/fingerprint_typescript.rs` | TypeScript-specific AST walking and type classification |

### Retrieval Leg

New variant: `RetrievalLeg::StructuralFingerprint`.

#### Query Flow

1. Parse the file-under-review with tree-sitter (already happens for AST analysis)
2. Fingerprint top-level named functions/methods, capped at 8 per file, selected by body size descending (largest bodies first = most structurally interesting)
3. For each source DB, run KNN on `chunks_struct_vec` using cosine similarity with `k * 2` overfetch
4. Deduplicate by chunk id against candidates from other legs
5. Feed into the existing reranker

Query symbols are capped at 8 to prevent N x M explosion. A file with 50 symbols across 10 sources would produce 80 KNN queries, not 500.

Minimum body complexity threshold: `body_node_count >= 10`. Trivial getters, single-expression functions, and stub methods are excluded from both indexing and querying.

#### RetrievalQuery Extension

```rust
pub struct RetrievalQuery {
    // ... existing fields ...
    pub structural_fingerprints: Vec<([f32; 64], String)>,  // (vector, qualified_name)
}
```

When `structural_fingerprints` is empty (no tree-sitter grammar for the language, or no qualifying symbols), the structural leg is skipped.

#### Reranker Integration

The existing blend formula is **unchanged**: `0.6 * bm25_norm + 0.4 * vec_norm`.

Structural similarity is applied as an **additive boost**, same pattern as the existing `id_exact_match` (+1.0) and `language_match` (+0.5) boosts:

```
struct_boost = max_cosine_similarity_across_query_fingerprints * STRUCT_BOOST_WEIGHT
```

Where `STRUCT_BOOST_WEIGHT` is an internal constant (initially 0.3). The `max` aggregation across query fingerprints means: for each candidate chunk, we compute cosine similarity against each query symbol's fingerprint and take the best match. This captures "this candidate is structurally similar to at least one symbol in the file under review."

`ScoreBreakdown` gains a new field:

```rust
pub struct ScoreBreakdown {
    pub bm25_norm: f32,
    pub vec_norm: f32,
    pub struct_sim: f32,    // cosine similarity to best-matching query fingerprint, 0.0 if no match
    pub id_boost: f32,
    pub path_boost: f32,
    pub recency_mul: f32,
    pub score: f32,
}
```

Final score: `(blended + id_boost + path_boost + struct_boost) * recency_mul`.

#### Ablation

Setting `STRUCT_BOOST_WEIGHT = 0.0` completely disables the structural signal's influence on ranking while still populating telemetry. This allows A/B comparison: run with boost=0.0 and boost=0.3, compare retrieval quality via telemetry.

The structural retrieval leg can also be fully disabled (skip KNN queries entirely) via a compile-time or runtime flag for performance testing.

### Configuration

No user-facing configuration in the MVP. Internal constants:

```rust
const STRUCT_BOOST_WEIGHT: f32 = 0.3;
const BM25_BLEND_WEIGHT: f32 = 0.6;   // unchanged
const VEC_BLEND_WEIGHT: f32 = 0.4;     // unchanged
const FINGERPRINT_VERSION: &str = "structural-v1";
const FINGERPRINT_DIMS: usize = 64;
const MAX_QUERY_SYMBOLS: usize = 8;
const MIN_BODY_NODE_COUNT: usize = 10;
```

Future: if telemetry shows tuning is needed, expose `struct_boost_weight` and `max_query_symbols` under `[context.retrieval]` in sources.toml.

### Telemetry

Extend `ContextTelemetry` (all `#[serde(default)]` for backward compat):

| Field | Type | Description |
|-------|------|-------------|
| `structural_fingerprint_hits` | `u32` | Chunks found via the fingerprint KNN leg |
| `structural_fingerprint_contributed` | `u32` | Fingerprint-leg chunks that survived reranking into final top-k |
| `fingerprint_query_ms` | `u32` | Wall-clock time for fingerprint KNN queries across all sources |

`RetrievalLeg::StructuralFingerprint` appears in `ScoredChunk.source_legs` when a chunk was found (or also found) via the fingerprint leg.

### Error Handling and Graceful Degradation

- No tree-sitter grammar available (e.g., YAML file): `structural_fingerprints` is empty, leg produces zero candidates. No error.
- Fingerprints absent from source DB (old index): KNN returns empty, same degradation. No error.
- tree-sitter parse fails on a chunk during indexing: skip fingerprint for that chunk (omit from `chunks_struct_vec`), log `tracing::warn`. Chunk is still retrievable via other legs.
- `chunks_struct_vec` table missing (DB from before schema v2): detect during retriever setup, skip fingerprint leg for that source. Log once per source.
- Fingerprint version mismatch: stale fingerprints still work for KNN (valid vectors, older scheme). Quality may be lower. `context refresh` fixes it.

No panics, no hard failures. The fingerprint leg is purely additive. The system is strictly no-worse-than-today when fingerprints are absent, stale, or disabled (boost=0.0).

## Alignment with Other Work

The review context extraction refactor (#339, `docs/superpowers/specs/2026-05-14-review-context-extraction-design.md`) introduces `AstContext { tree, language, rule_metadata }`. Our `Fingerprinter` trait needs the same inputs (tree-sitter `Node` + source bytes). Once both land, the query-side fingerprinting can reuse the already-parsed tree from `AstContext` instead of re-parsing. This is an optimization, not a dependency — either can merge first.

## Testing Strategy

- Unit tests for `StructuralFingerprint::to_vector()`: known fingerprints produce expected dimension values
- Unit tests per language fingerprinter: parse known code snippets, verify signature categories, control-flow counts, semantic counts
- Unit tests for type category classification: Rust types, Python annotations, TS type references
- Unit tests for `unk` vs `T` distinction: untyped Python params produce `unk`, generic type params produce `T`
- Unit tests for minimum body complexity filter: trivial functions return `None`, complex functions produce fingerprints
- Integration test: index 2+ sources, query with structural fingerprints, verify fingerprint-leg chunks appear in results
- Ablation test: same query with `STRUCT_BOOST_WEIGHT = 0.0` produces identical ranking to baseline (no structural leg)
- Reranker test: structural boost is additive, does not change BM25/vector blend
- Schema migration test: v1 DB gets `chunks_struct_vec` added, fingerprint leg returns empty gracefully
- Golden test per language: canonical code snippet produces a stable fingerprint vector (regression guard)
