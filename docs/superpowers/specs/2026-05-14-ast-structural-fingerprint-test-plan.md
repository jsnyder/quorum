# AST Structural Fingerprint -- Acceptance Criteria & Test Cases

Version: 1.0 | Date: 2026-05-14 | Author: TPIA

## Feature Summary

A 64-dimension structural fingerprint vector per code symbol, stored in sqlite-vec, queried via KNN as a fourth retrieval leg alongside BM25, vector embedding, and structural name match. The fingerprint encodes: type-category vocabulary (prim, str, col, opt, res, ref, fn, self, unk, T), control-flow sketch, and semantic counts. Three language implementations: Rust, Python, TypeScript.

---

## Acceptance Criteria

### AC-1: Fingerprint Generation

1. Given a Rust/Python/TypeScript source file with named symbols (functions, methods, structs/classes), the extractor produces a 64-dim `f32` vector per symbol chunk.
2. The fingerprint is deterministic: identical source always yields identical vectors.
3. Empty functions produce a valid 64-dim vector (all zeros or a defined baseline), never panic.
4. Symbols with no type annotations produce a vector with only control-flow and count dimensions populated.

### AC-2: Type-Category Vocabulary Mapping

1. Each language maps its native types to the 10-category vocabulary correctly (prim, str, col, opt, res, ref, fn, self, unk, T).
2. Generic type parameters map to T regardless of name (`T`, `U`, `E` in Rust; `TypeVar` in Python; `<T>` in TS).
3. Unmapped/unknown types map to `unk`, not panic.

### AC-3: Control-Flow Sketch

1. The fingerprint distinguishes: a function with only sequential flow vs. one with branching (if/match) vs. one with loops vs. one with error handling (try/catch, `?`).
2. Nested control flow (loop inside if) is reflected in the vector differently than flat sequential if + loop.

### AC-4: Semantic Counts

1. Line count, parameter count, return-point count, and call-site count are captured and normalized into their vector dimensions.
2. Normalization caps extreme values (1000-line function) without overflow or NaN.

### AC-5: SQLite Storage

1. The `chunks` table (or a new `chunk_fingerprints` table) stores the 64-dim vector via sqlite-vec.
2. Chunks without fingerprints (doc chunks, markdown, unsupported languages) store NULL, not a zero vector.
3. Re-indexing the same file replaces the fingerprint (no stale duplicates).

### AC-6: KNN Retrieval Leg

1. Given a query fingerprint, KNN returns the K nearest neighbors ordered by cosine similarity.
2. The fingerprint leg integrates into `Retriever::query` as a fourth candidate source alongside BM25, vector, and structural.
3. `RetrievalLeg::Fingerprint` is emitted in `ScoredChunk::source_legs` for chunks surfaced by this leg.
4. Filters (sources, kinds, exclude_source_paths) apply to fingerprint search identically to other legs.

### AC-7: Rerank Integration

1. Fingerprint similarity feeds into `RerankInput` as a new raw signal alongside `bm25_raw` and `vec_raw`.
2. The rerank formula blends fingerprint score with configurable weight (default TBD, likely 0.1-0.2).

### AC-8: Graceful Degradation

1. If sqlite-vec is unavailable or the fingerprint column does not exist, the retriever proceeds with three legs (no panic, warning logged).
2. If a language extractor fails to produce a fingerprint for one symbol, the chunk is still indexed without a fingerprint.
3. If the query symbol has no fingerprint (e.g., from an unsupported language), the fingerprint leg returns empty results and the other three legs proceed normally.

### AC-9: Telemetry

1. Per-review telemetry includes: `fingerprint_candidates_returned`, `fingerprint_unique_contributions` (chunks surfaced only by fingerprint leg).

---

## Test Cases by Component

### C1: Fingerprint Encoder (per language)

| ID | Case | Input | Expected |
|----|-------|-------|----------|
| C1-01 | Trivial function | `fn add(a: i32, b: i32) -> i32 { a + b }` | 64-dim vec; prim dims nonzero; no control-flow dims set |
| C1-02 | Function with Option return | `fn find(k: &str) -> Option<String>` | opt + str + ref dims nonzero |
| C1-03 | Function with Result + ? | `fn read() -> Result<Vec<u8>, io::Error> { f.read()? }` | res + col + prim dims; error-handling sketch bit set |
| C1-04 | Generic function | `fn map<T, U>(v: Vec<T>, f: impl Fn(T)->U) -> Vec<U>` | T dim populated, fn dim populated, col dim populated |
| C1-05 | Empty function body | `fn noop() {}` | Valid 64-dim; all semantic counts at baseline |
| C1-06 | Complex control flow | Function with nested if/match/for/while | Control-flow sketch dims differentiate from C1-01 |
| C1-07 | Python: untyped function | `def process(data): ...` | Only control-flow + count dims populated; type dims zero |
| C1-08 | Python: typed function | `def parse(s: str) -> Optional[dict]` | prim(str) + opt + col(dict) dims nonzero |
| C1-09 | TypeScript: async function | `async function fetch(url: string): Promise<Response>` | fn + str + res(Promise) dims set |
| C1-10 | Self parameter | `fn method(&self, x: i32)` / `def method(self, x)` | self dim set |
| C1-11 | Determinism | Same source extracted twice | Byte-identical vectors |
| C1-12 | NaN/Inf guard | Pathological input (0 params, 0 lines after normalization) | No NaN or Inf in any dimension |

### C2: SQLite Storage

| ID | Case | Expected |
|----|-------|----------|
| C2-01 | Insert fingerprint alongside chunk | Fingerprint retrievable via chunk_id |
| C2-02 | NULL fingerprint for doc chunk | KNN query skips doc chunks (not matched) |
| C2-03 | Re-index overwrites fingerprint | Old vector replaced, KNN reflects new version |
| C2-04 | Schema migration on existing DB | DB without fingerprint column upgrades without data loss |
| C2-05 | sqlite-vec not compiled in | Graceful error at table creation; index/query proceed without fingerprints |

### C3: KNN Retrieval Leg

| ID | Case | Expected |
|----|-------|----------|
| C3-01 | Query with fingerprint, K=5 | Returns up to 5 nearest by cosine similarity |
| C3-02 | Query fingerprint is all zeros | Returns empty (no meaningful match) or lowest-scoring results |
| C3-03 | Identical fingerprint in DB | Cosine similarity = 1.0, returned as top hit |
| C3-04 | Filters restrict results | source/kind/exclude_path filters apply; excluded chunks not returned |
| C3-05 | Empty fingerprint table | Returns empty vec, no error |
| C3-06 | Large DB (10k+ chunks with fingerprints) | KNN returns in under 50ms (performance guard) |

### C4: Retriever Integration

| ID | Case | Expected |
|----|-------|----------|
| C4-01 | All four legs contribute | ScoredChunk.source_legs may contain all four tags |
| C4-02 | Fingerprint-only surfaced chunk | Chunk not found by BM25/vector/structural but found by fingerprint; appears in results with `[Fingerprint]` leg |
| C4-03 | Fingerprint leg disabled (no sqlite-vec) | Three-leg retrieval works identically to current behavior |
| C4-04 | Query has no fingerprint input | Fingerprint leg skipped; three-leg behavior preserved |
| C4-05 | Overlap: fingerprint + BM25 find same chunk | Chunk carries both leg tags; rerank blends both scores |

### C5: Rerank Blending

| ID | Case | Expected |
|----|-------|----------|
| C5-01 | Fingerprint score normalized with min-max | Consistent with BM25/vector normalization |
| C5-02 | High fingerprint + low BM25 | Blended score is intermediate (not dominated by either) |
| C5-03 | Fingerprint weight = 0.0 | Fingerprint leg has no effect on ranking (feature toggle off) |
| C5-04 | All candidates have identical fingerprint score | Normalized to 1.0 (same as existing tie behavior) |

### C6: Cross-Language Consistency

| ID | Case | Expected |
|----|-------|----------|
| C6-01 | Equivalent function in Rust and Python | Fingerprints are within cosine distance < 0.3 (structurally similar) |
| C6-02 | Structurally different functions | Fingerprints have cosine distance > 0.7 |
| C6-03 | Same function, different variable names | Identical fingerprints (names are not encoded) |

### C7: Edge Cases & Regressions

| ID | Case | Expected |
|----|-------|----------|
| C7-01 | 0-byte source file | No fingerprint produced, no error |
| C7-02 | File with only comments | No symbol chunks, no fingerprints |
| C7-03 | Macro-generated code (Rust) | Best-effort fingerprint or skip with warning |
| C7-04 | Deeply nested function (20+ levels) | Control-flow sketch saturates, does not overflow |
| C7-05 | Function with 100+ parameters | Count dim caps at normalized ceiling, no panic |
| C7-06 | Unicode identifiers | Type mapping still works; unk for unmapped types |
| C7-07 | Mixed-language file (e.g., inline JS in HTML) | Unsupported; no fingerprint, no crash |
| C7-08 | Concurrent index writes | sqlite-vec INSERT is WAL-safe; no corruption |

### C8: Telemetry

| ID | Case | Expected |
|----|-------|----------|
| C8-01 | Review with fingerprint leg active | `fingerprint_candidates_returned` > 0 in telemetry |
| C8-02 | Fingerprint surfaces unique chunk | `fingerprint_unique_contributions` incremented |
| C8-03 | Fingerprint leg disabled | Telemetry fields absent or zero (serde default) |

---

## Priority Matrix

| Priority | Test IDs | Rationale |
|----------|----------|-----------|
| P0 (must-ship) | C1-01..06, C1-11, C1-12, C2-01..04, C3-01, C3-04, C3-05, C4-02, C4-03, C7-01 | Core correctness and graceful degradation |
| P1 (should-ship) | C1-07..10, C3-02, C3-03, C4-01, C4-04, C4-05, C5-01..04, C6-01..03, C8-01..03 | Cross-language parity, rerank integration, telemetry |
| P2 (nice-to-have) | C2-05, C3-06, C7-02..08, C6-02 | Performance, exotic edge cases |
