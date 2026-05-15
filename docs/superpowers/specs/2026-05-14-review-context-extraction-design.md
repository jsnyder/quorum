# Review Context Extraction (#339)

## Problem

`review_file` and `review_file_llm_only` in `src/pipeline.rs` share ~60% of their logic (feedback index setup, Context7 enrichment, precedent lookup, prompt construction, model loop, merge, grounding, diff classification) but are maintained as independent ~500-line functions. Changes to shared logic must be applied twice and diverge silently when one function is updated without the other.

Issue #339 identifies a "rediscovery problem" — context is reassembled from scratch on every review run. The fix has two phases: first, make the context assembly explicit and deduplicated (this spec); second, cache it across runs (future work).

## Design Review Findings

Three independent reviewers (Claude oracle, GPT-5.4 via pal, and codebase exploration) converged on the same conclusion:

1. **Per-model recomputation doesn't exist.** Both functions already build one prompt and loop over models. A "ReviewBundle" abstraction targeting per-model sharing would solve a non-problem.
2. **The real waste is code duplication.** The two review functions are the maintenance hazard and the barrier to future caching.
3. **The right seam is a context-assembly helper,** not a new struct that bundles mutable and immutable data together.

## Approach

### 1. Unify `review_file` and `review_file_llm_only`

Merge into a single `review_file` that accepts an optional AST context:

```rust
pub async fn review_file(
    file_path: &Path,
    source: &str,
    ast: Option<AstContext<'_>>,  // Tree + Language + rules, or None
    reviewer: Option<&dyn LlmReviewer>,
    pipeline_config: &PipelineConfig,
    parse_cache: Option<&ParseCache>,
) -> anyhow::Result<ReviewResult>
```

When `ast` is `None`, the function skips: local AST complexity analysis, ast-grep rule scanning, judge phase, and hydration context assembly. All shared logic (feedback index, Context7 enrichment, precedent lookup, prompt building, model loop, merge, grounding, diff classification, calibration) executes once in one code path.

`AstContext` is a thin carrier struct:

```rust
pub struct AstContext<'a> {
    pub tree: &'a Tree,
    pub language: Language,
    pub rule_metadata: HashMap<String, RuleMetadata>,
}
```

### 2. Extract `build_file_context()`

Extract the shared pre-prompt assembly into a helper that returns immutable, reusable context:

```rust
pub struct FileContext {
    pub redacted_code: String,
    pub truncation_notice: Option<String>,
    pub framework_docs: Option<Vec<String>>,
    pub feedback_precedents: Option<Vec<String>>,
    pub hydration_context: Option<HydrationContext>,
    pub context_block: Option<String>,
    pub enrichment_metrics: EnrichmentMetrics,
}

fn build_file_context(
    file_path: &Path,
    source: &str,
    ast: Option<&AstContext<'_>>,
    pipeline_config: &PipelineConfig,
) -> anyhow::Result<FileContext>
```

This function performs: secret redaction, truncation, Context7 enrichment, feedback precedent selection, hydration context assembly (if AST available), and context injection. It returns an immutable `FileContext` that feeds directly into `ReviewRequest` field population.

The model loop stays in `review_file` — it consumes `FileContext` to build `ReviewRequest`, calls each model, and collects results.

### 3. Keep mutable findings separate

AST findings (`Vec<Finding>`) and judge results stay in `review_file`'s local scope, not in `FileContext`. The judge mutates findings in-place via `&mut Vec<Finding>` — bundling them into a shared immutable struct would require cloning or architectural gymnastics that aren't worth the complexity.

Flow:

```
review_file(path, source, ast, reviewer, config)
  │
  ├─ build_file_context(path, source, ast, config)
  │     → FileContext (immutable, reusable)
  │
  ├─ if ast.is_some():
  │     ├─ run local_ast + ast_grep → Vec<Finding>
  │     ├─ judge_findings(&mut findings, ...)
  │     └─ push into all_sources
  │
  ├─ for model in models:
  │     ├─ build ReviewRequest from &FileContext
  │     ├─ build_review_prompt(&req)
  │     ├─ reviewer.review(&prompt, model, sys_prompt)
  │     └─ parse findings → all_sources
  │
  ├─ merge_findings(all_sources)
  ├─ grounding pass
  ├─ diff classification
  └─ calibration
```

### 4. Prompt ordering for prefix caching

When building `ReviewRequest` from `FileContext`, order the prompt sections so stable content comes first:

1. System prompt (identical across runs)
2. File context block (from `FileContext` — stable for same file content)
3. Framework docs (stable for same dependency set)
4. Feedback precedents (stable within a session)
5. Hydration context / AST findings (stable for same file content)
6. Focus directives, mode-specific instructions (varies per perspective)

This ordering maximizes LLM API prefix cache hits when multiple perspectives review the same file — the shared prefix is cached and only the tail (focus/mode) differs. The existing `build_review_prompt` already roughly follows this pattern; the constraint is: don't reorder sections in ways that break this property during the refactor. If natural logical structure conflicts with cache-friendly ordering, prefer structure — correctness over cache hits.

### 5. Future: perspective-based multi-run

The `FileContext` seam enables a future pattern where multiple review perspectives (security focus, logic focus, performance focus) each build their own `ReviewRequest` from the same pre-computed `FileContext` with different system prompts or focus directives. This is distinct from ensemble mode (same prompt, multiple models) and more aligned with how review depth will scale.

This is explicitly deferred — no code for it in this change.

### 5. Future: cross-run caching

`FileContext` is the natural caching unit. A future change could serialize it with a composite cache key (content hash + feedback state hash + rule version + config hash) and TTL-based staleness tracking. The extraction in this spec makes that possible without the premature `content_hash` field that the original design proposed.

This is explicitly deferred — no code for it in this change.

## What changes

| File | Change |
|------|--------|
| `src/pipeline.rs` | Unify `review_file` + `review_file_llm_only` into single function; extract `build_file_context()` and `FileContext` struct |
| `src/main.rs` | Update call sites in `run_review` to use unified signature |
| `src/pipeline.rs` (tests) | Update/consolidate tests for the unified function |

## What doesn't change

- `ReviewRequest` struct (stays as-is, populated from `FileContext`)
- LLM client interface
- Prompt rendering (`build_review_prompt`)
- Judge phase (still mutates findings in local scope)
- `PipelineConfig` (still carries shared state across files)
- External API (`review_source` public function)

## Testing

- Unit tests for `build_file_context`: given known inputs, returns expected `FileContext` fields (redacted code, truncation, precedents)
- Integration test: `review_file` with `ast: None` produces same results as old `review_file_llm_only`
- Integration test: `review_file` with `ast: Some(...)` produces same results as old `review_file`
- Regression: all existing pipeline tests pass without behavior change
- Property: `FileContext` fields are deterministic given same inputs (no ordering variance)

## Scope guard

This change is a refactor. No new features, no new user-facing behavior, no new files on disk. If the unified function grows beyond ~400 lines, extract additional helpers but don't add new abstractions. The goal is fewer lines of code, not more.
