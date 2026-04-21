# quorum context — testing boundaries & anti-pattern guardrails

Input for TDD plan. Assumes design doc at `docs/plans/2026-04-20-quorum-context-design.md`.

## 1. Public API boundary per component

Test *these* surfaces. Everything below is a private helper — do not assert against it.

| Component | Public surface to exercise | What NOT to test |
|---|---|---|
| Source registration | `SourcesConfig::load(&Path) -> Result<SourcesConfig>`; round-trip through `SourcesConfig::merge_global(global, project)`. Bad TOML, missing required fields, glob expansion, weight defaults. | TOML deserializer internals; field-by-field setters. |
| Extractor | `extract_source(&SourceSpec, &Path) -> Vec<Chunk>` returning a stable-ordered `Vec<Chunk>`. Assert on `(id, kind, qualified_name, signature, is_exported, neighboring_symbols)` for a **small fixture tree**. | Per-rule ast-grep matchers; tree-sitter cursor walks; doc-comment regexes. |
| Markdown splitter | `split_markdown(text, source_path) -> Vec<Chunk>` with heading_path + subtype inference (ADR/README/CHANGELOG). | Internal heading-level stack, char offsets. |
| Chunk store | `ChunkStore::append(&mut self, &[Chunk])`, `ChunkStore::load(path) -> Vec<Chunk>`, `ChunkStore::validate(path) -> Result<()>`. Golden JSONL round-trip (write → read → equal). | Serde codepaths, line-buffer sizes. |
| Index builder | `IndexBuilder::rebuild_from_jsonl(&[Path], &mut Connection)`, `IndexBuilder::incremental(&[Chunk], &mut Connection) -> IndexStats`. Assert via **SQL queries** against the built DB. | FTS5 tokenizer output, vec blob layout. |
| Hybrid retriever | `Retriever::query(&RetrievalQuery) -> Vec<ScoredChunk>` where ScoredChunk exposes `{chunk, total_score, breakdown: ScoreBreakdown}`. | `blend()`, `apply_boosts()`, `recency_decay()` as separate functions — test through `query()`. |
| Identifier harvester | `harvest_identifiers(&HydratedFile, &GenericStoplist) -> Vec<String>` — pure function; easy to test on fixture hydration structs. | Tree-sitter hydration itself (already tested in quorum). |
| Adaptive threshold/budget | `plan_injection(symbol_results, prose_budget_remaining, cfg) -> InjectionPlan` returning `{symbols_kept, prose_threshold, skip_all: bool}`. Pure function — table-test it. | `render_tokens()` approximation internals. |
| Renderer | `render_context_block(&[ScoredChunk], &RenderOpts) -> String`. Assert on markdown **structure** (presence of card headers, source_uri footer, precedence annotation, staleness marker, total token footer), not exact whitespace. | Heading-demotion regex; individual escape rules. |
| Staleness annotator | `annotate_staleness(&[Chunk], &WorkingTreeStatus) -> Vec<AnnotatedChunk>` where `WorkingTreeStatus` is a trait the test can fake. | `git status` subprocess — faked via trait. |
| Telemetry | `ReviewRecord::context: Option<ContextTelemetry>` serialization; assert JSON shape + presence of `rerank_score_breakdown` arrays aligned in length to `injected_chunk_ids`. | Per-field getters. |
| `context_misleading` feedback + calibrator | `FeedbackStore::record_context_misleading(run_id, chunk_ids, reason)`; `Calibrator::injection_threshold_for(chunk_id) -> f32` after N confirmations. Integration-style test across the pair. | Internal suppression counters. |
| CLI | `run_context_cmd(argv, deps) -> Result<i32>` with a `ContextDeps` trait object for git/fs/clock. Test exit codes + stdout/stderr regexes, not internal dispatch. | clap parser structure. |

## 2. Boundaries to mock (be specific)

- **git operations** — wrap behind a `GitOps` trait: `clone`, `fetch`, `rev_parse`, `status_porcelain`. **Mock the trait**, do not mock `git2`/`gix` internals. For one or two E2E tests, use a real local bare repo created in `tempdir` with `git init`/`git commit` via `git2` — no network.
- **fastembed** — wrap behind `Embedder` trait (`embed(&[&str]) -> Vec<Vec<f32>>`). Default test impl: deterministic hash-to-vec (e.g., seed a small RNG from token hash) producing normalized 384-dim vectors. Keeps retrieval tests fully deterministic without loading a 90MB model. One **ignored-by-default** test wires the real fastembed for a smoke run.
- **SQLite** — always **real** SQLite (`rusqlite` in-memory `:memory:` or `tempfile::NamedTempFile`). FTS5 + sqlite-vec are the feature under test; mocking them defeats the purpose. Never stub the DB.
- **tree-sitter / ast-grep** — use **real parsers on small fixture source files** checked into `tests/fixtures/context/`. Faking `Chunk` structs works for splitter/retriever/renderer tests but the extractor itself must run real grammars. This is anti-pattern #1 insurance (integration must be real where the value is).
- **LLM** — never call a real model. Do snapshot-test the rendered markdown block via `insta` (redact timestamps + SHAs). Snapshots are **secondary**; also assert structural invariants (card count, `source_uri:` lines present, token footer numeric). Avoids anti-pattern #15.
- **clock** — `Clock` trait with `FixedClock` for staleness/recency tests. Essential for deterministic recency_mul assertions.

## 3. Top antipatern risks for this feature

1. **Testing the ranking formula directly (AP #5 Testing Internals).** The blend/boost/decay math will be tuned. Do **not** unit test `blended = 0.6*bm25 + 0.4*vec`. Test `retrieve(query) -> Vec<ScoredChunk>` against a fixture corpus with assertions on **relative ordering** and **score_breakdown presence**, not numeric equality. Exception: `plan_injection` and `recency_mul` are *pure spec functions* and can be table-tested — they encode contract (floor 0.25, adaptive drop 0.10), not implementation.
2. **Snapshot-abusing the SQLite DB (AP #15 Snapshot Abuse).** Do not `toMatchSnapshot` the binary `index.db` or its raw FTS5 contents. Instead, after `IndexBuilder::rebuild`, run **named SQL queries** (`SELECT COUNT(*) FROM chunks WHERE source=?`, `SELECT chunk_id FROM fts WHERE fts MATCH ?`) and assert on query results. Queryable state, not file state.
3. **Over-mocking retrieval (AP #19 The Mockery).** It is tempting to mock `Embedder`, mock the FTS search, mock the reranker, and assert on the final plumbing. That tests nothing. The retriever's value is the *interaction* of FTS5 + vec + rerank — use the deterministic `HashEmbedder` + real SQLite and test end-to-end through `Retriever::query`.
4. **Coverage theater on CLI glue (AP #6).** `quorum context add/list/init` are thin wrappers. Don't chase 100% on them — test one happy path + one error path per subcommand, then let integration tests cover wiring. Add mutation testing (Stryker-equivalent: there is no great Rust mutation tool; `cargo-mutants` is acceptable) to one file: the **rerank scorer** and the **adaptive planner**, since those are where silent bugs cost the most.
5. **Overfitting tests to a hand-tuned query set (retrieval-specific, AP #4 Testing Wrong Functionality).** See §6.

## 4. Integration tests per MVP milestone

One per milestone. Each uses a **real local git fixture** under `tests/fixtures/context/repos/` or a temp repo initialized in the test.

| Milestone | Integration test | Setup | Public API exercised |
|---|---|---|---|
| Source registration + extractor | `extract_fixture_rust_source_produces_symbol_and_doc_chunks` | `tests/fixtures/context/repos/mini-rust/` with 1 crate, 3 .rs files, README.md, one ADR | `register_source()` → `index_source()` → `ChunkStore::load()` |
| Chunk store + index builder | `rebuild_index_from_jsonl_is_queryable_via_fts_and_vec` | 20 synthetic chunks written to JSONL | `IndexBuilder::rebuild_from_jsonl()` then raw SQL `SELECT` + `vec_search()` |
| Hybrid retriever | `retrieve_surfaces_exact_identifier_match_above_semantic_neighbor` | Indexed corpus of 50 chunks with one exact-id target and near-duplicates | `Retriever::query(RetrievalQuery{ identifiers: vec!["verify_token"], ..})` → assert chunk_id of rank-0 |
| Adaptive threshold + planner | `prose_threshold_drops_when_symbols_starve` | Corpus where symbol query returns 1 result | `plan_injection` + `Retriever::query_adaptive` |
| Renderer | `render_includes_precedence_and_staleness_when_conditions_hold` | Two sources with overlapping qnames + fake `WorkingTreeStatus::Dirty` | `render_context_block` |
| Staleness annotation | `dirty_worktree_annotates_chunks_from_same_repo` | Temp git repo with uncommitted change | Renderer + `GitOps` fake returning `Dirty` |
| Telemetry | `review_with_context_emits_context_block_in_reviews_jsonl` | End-to-end: fixture repo indexed → `quorum review --no-llm` (or trait-faked LLM) → parse `reviews.jsonl` tail | Full review path |
| Feedback + calibrator | `context_misleading_threshold_rises_after_n_confirmations` | In-process calibrator with N=3 confirmations | `FeedbackStore::record_context_misleading` + `Calibrator::injection_threshold_for` |
| CLI | `quorum context add then index then query roundtrip` | `assert_cmd` against real binary with `HOME=tempdir`, local bare-repo URL | `quorum context *` |
| Doctor | `doctor_detects_and_repairs_corrupt_jsonl` | Write valid chunks, truncate the file mid-record | `quorum context doctor --repair` |

## 5. Fixtures & data

Create under `tests/fixtures/context/`:

```
tests/fixtures/context/
├── repos/
│   ├── mini-rust/           # 3 .rs files, Cargo.toml, README.md, docs/adr/001.md — pre-committed git repo (init in build.rs or test setup)
│   ├── mini-ts/             # 2 .ts files with exported + internal functions, one README.md
│   └── mini-terraform/      # 1 main.tf with a module, 1 variables.tf, README.md
├── sources/
│   └── example-sources.toml # valid + one malformed variant
├── chunks/
│   ├── golden-mini-rust.jsonl       # expected extraction output — regenerate-on-demand golden
│   └── synthetic-50-chunks.jsonl    # hand-crafted chunks with known rank properties
├── embeddings/
│   └── hash_embedder.rs     # deterministic test embedder (not a fixture, but document here)
└── eval/
    └── gold-relevance.jsonl # 20 (query, chunk_id, relevance:{0,1,2}) triples for rank-quality tests
```

Notes:
- **Do not check in real fastembed vectors.** Use the deterministic `HashEmbedder`.
- Golden JSONLs regenerate via `QUORUM_UPDATE_GOLDEN=1 cargo test`. Review diffs like code — never blind-update (AP #15 guardrail).
- The `gold-relevance.jsonl` set is the **only** place where subjective relevance judgments live. Keep it small and signed off by two humans.

## 6. Retrieval-testing traps (be paranoid here)

- **Overfitting to a tuned query set.** If the same person writes the ranker and the relevance tests, the ranker will pass its own tests and fail in production. Split: one person writes `gold-relevance.jsonl` **before** seeing rerank code; another implements retrieval. Rotate.
- **Asserting exact rank positions.** Retrieval outputs are sets-with-preference, not lists. Assert: "chunk X appears in top-3", "chunk A ranks above chunk B", "chunks with `id_boost` always outrank matching-content chunks without it **when other signals are tied**." Never `assert_eq!(results[0].chunk.id, "...")` unless that chunk is an **exact identifier match** — that case is contract, not quality.
- **Asserting floating-point scores.** Prohibit `assert_eq!(score, 0.73)`. Use `assert!((score - expected).abs() < 1e-4)` only for pure math functions (`recency_mul` at known inputs); for composite scores, assert inequalities (`a.score > b.score`) and breakdown presence.
- **NDCG/MRR on the tiny gold set.** Compute them for trend tracking (store in CI artifact), but **do not gate** builds on a threshold. A 20-item gold set has too much variance. Gate builds on the contract tests above; use metrics as a dashboard.
- **Mutation testing pays off here.** Run `cargo-mutants` against `rerank.rs` and `planner.rs` only; those are the files where a swapped `>` / `>=` silently wrecks quality and no coverage metric notices.
- **Do not test against "the retriever returns sensible results."** Vague assertions rot. Every test names a specific contract (exact-id outranks semantic-near, identifier fallback triggers when len<2, adaptive τ drops by exactly 0.10, recency floor applies at age > ~9×halflife, etc.).

## 7. Testing shape recommendation

This feature is **honeycomb-shaped**, not pyramid. The value is in the interaction of extractor → store → index → retriever. Heavy unit testing of each ranking factor in isolation will produce brittle, refactor-hostile tests and miss the actual bugs (score collapse, stale vectors, FTS tokenization surprises). Target distribution:

- ~40% unit: pure functions only (`plan_injection`, `recency_mul`, `harvest_identifiers`, `SourcesConfig` parsing, markdown splitter, staleness annotator, chunk schema round-trip).
- ~50% integration: real SQLite + real grammars + fixture repos, faked embedder and git where appropriate.
- ~10% end-to-end CLI: `assert_cmd`-driven smoke tests per subcommand, one review-with-context full run.

No E2E browser/UI layer here; the "top" of the cone is just CLI smoke.
