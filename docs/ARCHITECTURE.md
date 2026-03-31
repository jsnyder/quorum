# Architecture: quorum

**Date**: 2026-03-23
**Origin**: Design exploration for Rust-native code review tool (successor to third-opinion)

## TL;DR

Porting third-opinion to Rust is **justified but only as a v2 rewrite, not a 1:1 translation**. The key reframing: shift from "API wrapper that orchestrates LLM calls" to "local analysis engine that uses LLMs for judgment." This makes the loss of the Ax SDK less painful because the architecture is fundamentally different.

**Pain level: 4-5/10** for core port (with DSRs), **7/10** if trying to replicate every TS feature.

## Motivations (confirmed)

- Memory bloat on long sessions (Node 100-200MB idle vs Rust <30MB)
- npm ecosystem fragility (Node version breakage, native module hell)
- Single binary distribution (no runtime dependencies)
- Personal: knows Rust better than Node
- Performance headroom unlocks new capabilities not practical in TS

## Multi-Model Consensus (Gemini 3 Pro, GPT-5.4, Gemini 2.5 Pro)

All three models converged on the same ranking, which suggests strong consensus:

### Capability Rankings

| Rank | Capability | Impact | Rust Advantage | Complexity |
|------|-----------|--------|---------------|------------|
| 1 | Hybrid local/remote + AST pre-analysis | Highest | Strong | Medium-High |
| 2 | Persistent daemon with warm caches | High (UX) | Strong | Medium |
| 3 | Local embeddings for dynamic few-shot | Medium-High | Strong | Medium |
| 4 | LSP integration | High theory, High risk | Neutral | Very High |
| 5 | Local models (embeddings/triage only) | Low near-term | Strong | Medium |

### The Hybrid Architecture (Consensus #1)

The biggest gain isn't "same thing but faster" -- it's transforming what the tool does:

**Current TS architecture**: Send code as text to LLM, ask "find bugs."

**Proposed Rust architecture**:
1. Local tree-sitter AST analysis produces structured "review facts" (complexity metrics, dead code, import graphs, insecure call patterns, data flow summaries)
2. Facts injected into LLM prompt: "evaluate this pre-analyzed control flow for security issues" instead of "review this code"
3. LLM focuses on judgment, not parsing -- better signal-to-noise ratio

**What's realistic for local AST analysis:**
- Very realistic: complexity metrics, nesting depth, import graphs, duplicate code fingerprints, insecure call patterns, simple dataflow, unreachable branches
- Realistic but bounded: interprocedural flow within a file, dead helper detection
- Not worth building early: universal type inference, precise alias analysis, full CFG/SSA

### Daemon Mode (Consensus #2)

Cache between reviews:
- Parsed tree-sitter ASTs (keyed by file hash)
- Import graph / symbol summaries
- Framework/language detection results
- Few-shot embeddings and ANN index
- Prior review artifacts and calibrator outcomes
- Docs lookup results

Idle footprint: <30MB Rust vs 100-200MB Node.

### Dynamic Few-Shot via Local Embeddings (Consensus #3)

Replace static `data/few-shot-examples.json` (15 examples) with:
- Large curated corpus of review examples
- Local embedding model (candle/ort compiled into binary)
- Semantic retrieval: "find examples most similar to this React hook review"
- Cached in daemon, indexed by language/framework then semantic similarity

## Critical Gaps the Models Glossed Over

### 1. Ax SDK Replacement -- DSRs (github.com/krypticmouse/DSRs)

The `@ax-llm/ax` SDK handles structured output, streaming, agentic tool use, and prompt optimization. The original assessment assumed no Rust equivalent exists.

**DSRs changes this.** It's a "performance-centered DSPy rewrite to Rust" (257 stars, 266 commits, active development as of 2026-02-08).

| Ax Feature | DSRs Equivalent | Status |
|------------|----------------|--------|
| `AxGen` structured output | `Predict<S>` with `#[derive(Signature)]` | Working |
| Chain of Thought | `ChainOfThought<T>` | Working |
| Multi-step pipelines | `Module` trait + chaining | Working |
| BootstrapFewShot | **COPRO + MIPROv2** (more capable) | Working |
| Evaluation | `TypedMetric` + `evaluate_trainset` | Working (better than Ax) |
| Tool calling / agents | `tools: Vec<Arc<dyn ToolDyn>>` on Predict | In API, needs verification |
| Streaming | Deferred (non-goal in current spec) | Not implemented |
| Output recovery | **BAML jsonish parser** (handles malformed JSON, markdown fences) | Working (superior to Ax) |
| Tracing / DAG | `trace::trace()` execution DAG capture | Working (Ax has nothing) |
| LLM providers | OpenAI-compatible + local (vLLM, Ollama) | Working |

**Key advantages of DSRs over Ax:**
- MIPROv2 is a more advanced optimizer than BootstrapFewShot
- BAML jsonish parser recovers from messy LLM output (Ax relies on clean structured output)
- Built-in eval framework replaces our manual `optimize-review.ts` scripting
- Execution tracing as DAG -- useful for debugging pipeline behavior

**Risks:**
- Mid-refactor (Phase 1-2: removing legacy bridge, moving to facet-native APIs). APIs may change.
- Tool calling is in the API but needs hands-on verification
- Streaming explicitly deferred (acceptable -- our pipeline waits for complete responses)
- Young project (1 release) -- less battle-tested than Ax

**Note on Ax effectiveness:** Our optimization experiments showed that playbook instruction optimization had **zero effect** with GPT-5.4 -- the bare model scores the same as optimized. Smaller models may benefit more from structured output frameworks. DSRs' MIPROv2 is worth testing regardless, as it's a fundamentally different optimization approach.

### 2. Prompt Engineering Infrastructure Migration

- 562 feedback entries in `~/.third-opinion/feedback.jsonl` -- format is JSON, portable
- Few-shot examples in `data/few-shot-examples.json` -- portable
- Review artifacts in `~/.third-opinion/artifacts/` -- portable
- Optimization scripts (`scripts/optimize-review.ts`) depend on Ax's BootstrapFewShot -- DSRs' MIPROv2 is a direct (and likely superior) replacement
- DSRs' `TypedMetric` + `evaluate_trainset` could replace custom eval logic

### 3. MCP SDK Maturity

The Rust MCP SDK exists but is less mature than the TS one. The stdio transport is the critical path. Worth prototyping early to validate.

### 4. Opportunity Cost

Honest assessment: 3-6 months of part-time work for core feature parity. During that time, the TS version doesn't improve. Mitigated by the phased approach below.

## Recommended Phased Approach

### Phase 0: Rust Core Library (standalone crate)
- tree-sitter multi-language parsing (Rust, Python, TypeScript/TSX, YAML)
- AST context hydration: callee sigs, type defs, caller blast radius
- AST analysis as reviewer: complexity, dead code, insecure patterns
- Linter orchestration: detect + run available linters, normalize output
- Canonical finding format (source-tagged JSON)
- **Can be called from the TS version via CLI subprocess** -- immediate value

### Phase 1: CLI + DSRs Integration
- DSRs `Predict<S>` for structured review output with `#[derive(Signature)]`
- DSRs tool calling for deep review mode (read_file, search, etc.)
- Multi-model cold-read ensemble (configurable model families)
- Review pipeline: hydrate -> parallel reviewers (LLM + local + linters) -> merge -> calibrate
- MIPROv2 optimizer replaces BootstrapFewShot for prompt optimization
- Single binary, feature parity with `review` tool only
- **Keep TS version alive** -- Rust version is opt-in

### Phase 2: MCP Server + Feedback RAG
- Rust MCP SDK integration (stdio transport)
- All 6 tools: review, chat, debug, testgen, catalog, feedback
- Secret redaction, domain detection
- Embed feedback entries, retrieve similar TP/FP precedent at calibration time
- Calibrator can ADD findings when backed by deterministic local evidence

### Phase 3: Daemon + Embeddings + Analytics
- Persistent daemon with warm AST/embedding/feedback caches
- Local embedding model for dynamic few-shot retrieval
- File watcher for cache invalidation
- Per-source (LLM/local/linter) TP/FP tracking and analytics
- Continuous improvement: feedback loop grows the precedent store

## Key Rust Crates

| Purpose | Crate | Maturity |
|---------|-------|----------|
| Async runtime | `tokio` | Production |
| HTTP client | `reqwest` | Production |
| JSON | `serde` / `serde_json` | Production |
| Tree-sitter core | `tree-sitter` | Production (it IS Rust) |
| Tree-sitter YAML | `tree-sitter-yaml` | Production (v0.7) |
| CLI args | `clap` | Production |
| YAML parsing | `serde_yaml` | Production |
| TOML parsing | `toml` | Production |
| Graph analysis | `petgraph` | Production |
| Embeddings | `candle-core` / `ort` | Maturing |
| Vector search | `lance` or in-memory HNSW | Maturing |
| MCP protocol | `mcp-rs` or similar | Early |
| LRU cache | `lru` | Production |
| Parallel iteration | `rayon` | Production |
| OpenAI client | `async-openai` | Production |
| Structured LLM / DSPy | `dspy-rs` (DSRs) | Active dev (mid-refactor) |
| Prompt optimization | DSRs COPRO / MIPROv2 | Working |
| Output parsing | DSRs + BAML jsonish | Working |

## Reimagined Architecture (v2)

The Rust port is an opportunity to redesign the review pipeline, not just translate it. These changes are grounded in empirical A/B test data from the current system.

### Evidence Base

- Cold reads beat investigation-primed reviews (investigation causes tunnel vision)
- Different models find almost entirely different bugs (3/35 overlap between TO and PAL)
- Prompt optimization has zero effect with GPT-5.4 (model default = optimized)
- 562 labeled feedback entries (276 TP, 111 FP) exist but are unused at review time
- Calibrator deduplicates 30-40% overlap in parallel pipeline

### Pipeline: Reviewer Orchestra

```
                     +------------------+
                     | Code + Diff In   |
                     +--------+---------+
                              |
                    +---------v----------+
                    | AST Context        |
                    | Hydration          |
                    | (tree-sitter)      |
                    |                    |
                    | - Callee sigs      |
                    | - Type definitions |
                    | - Caller blast     |
                    |   radius           |
                    | - Import targets   |
                    +---------+----------+
                              |
               Hydrated payload (code + context)
                              |
          +-------------------+-------------------+
          |                   |                   |
    +-----v------+    +------v------+    +-------v-------+
    | LLM Cold   |    | LLM Cold   |    | Local         |
    | Read #1    |    | Read #2    |    | Reviewers     |
    | (GPT-5.4)  |    | (Claude)   |    |               |
    |            |    |            |    | - AST analyzer|
    | No priming |    | No priming |    | - ruff (link) |
    | No invest. |    | No invest. |    | - clippy      |
    | context    |    | context    |    | - eslint etc  |
    +-----+------+    +------+------+    +-------+-------+
          |                   |                   |
          +-------------------+-------------------+
                              |
                    +---------v----------+
                    | Canonical Finding  |
                    | Format             |
                    | (source-tagged)    |
                    +---------+----------+
                              |
                    +---------v----------+
                    | Merge + Dedup      |
                    | (composite sim)    |
                    +---------+----------+
                              |
                    +---------v----------+
                    | Feedback RAG       |
                    | Retrieve similar   |
                    | TP/FP precedent    |
                    | from 562 entries   |
                    +---------+----------+
                              |
                    +---------v----------+
                    | Calibrator         |
                    | (with precedent    |
                    |  context)          |
                    |                    |
                    | Can ADD findings   |
                    | only if backed by  |
                    | deterministic      |
                    | local evidence     |
                    +---------+----------+
                              |
                    +---------v----------+
                    | Final Findings     |
                    | (with provenance)  |
                    +--------------------+
```

### 1. Heterogeneous Cold-Read Ensemble

**Evidence**: 3/35 overlap between different review systems = ~91% unique findings per model.

Replace prompt diversity with model diversity:
- Current: GPT-5.4 standard + GPT-5.4 investigation -> merge
- v2: GPT-5.4 cold + Claude cold + (optional: Gemini cold) -> merge

All cold reads -- no investigation priming, no prompt variation. Different pre-training mixtures find different classes of bugs. Reuse existing parallel pipeline + dedup infrastructure.

Cost: 2-3x token spend. Configurable: `--ensemble` for thorough, single-model default for fast. The ensemble flag maps to a config choosing which model families to include.

### 2. AST Context Hydration

**Evidence**: Cold reads beat investigation-primed. But cold reads with COMPLETE information beat cold reads with partial information.

Key distinction:
- "Look for X" = investigation priming (narrows attention, BAD)
- "Here's the full definition of the function being called" = context hydration (completes picture, GOOD)

The Rust tree-sitter layer produces a hydrated payload:
- Function call in diff -> attach callee signature, docstring, return type
- Custom type used -> attach type definition
- Function signature changed -> attach callers (blast radius analysis)
- Import added -> attach what's imported and how it's used
- Config value referenced -> attach where it's defined

This gives the cold reader what a human reviewer gets from their IDE. Not priming on what to find -- completing what they can see.

### 3. Linter Orchestration

External linters as first-class reviewers alongside LLMs and local AST analysis:

**Linked (compiled in)**:
- `ruff` (Rust-native) -- can link as library crate for Python reviews
- Custom tree-sitter rules for cross-language patterns

**Opportunistic (subprocess, if available)**:
- `clippy` for Rust projects
- `eslint` / `biome` for JS/TS
- `mypy` / `pyright` for Python type checking
- `rubocop` for Ruby

Detection: scan project for config files (`.eslintrc`, `pyproject.toml [tool.ruff]`, `clippy.toml`). Run available linters, normalize output to canonical finding format.

The LLM calibrator adds value on top: assessing whether a linter finding is contextually relevant, and elevating low-severity linter warnings when broader code context makes them critical.

### 4. Feedback RAG for Calibration

**Data**: 562 entries (276 TP, 111 FP, 109 partial, 66 wontfix) -- unused at review time.

Architecture:
1. Embed all feedback entries (code snippet + finding text + TP/FP label)
2. Store in local vector index (daemon-cached, HNSW or lance)
3. At calibration time, for each candidate finding, retrieve 2-3 most similar past findings
4. Inject as precedent: "Similar findings on similar code were marked: [TP: '...', FP: '...']"

FP examples are especially valuable -- they teach the calibrator domain-specific patterns to NOT flag. TP examples reinforce what's worth catching.

This grows more powerful over time as more feedback accumulates. The feedback loop is: review -> user marks TP/FP -> embedding indexed -> improves next review.

### 5. Local Analysis as First-Class Reviewer

Local AST analyzer produces its own findings (not just context for LLMs):
- Complexity hotspots (cyclomatic > threshold)
- Dead code (unreachable branches, unused exports)
- Insecure call patterns (known dangerous APIs)
- Import graph anomalies
- Simple dataflow (unvalidated input -> sensitive sink)

These findings are tagged `source: "local-ast"` and flow through the same merge -> dedup -> calibrate pipeline alongside `source: "gpt-5.4"` and `source: "claude"` findings.

The calibrator can ADD findings only when backed by deterministic local evidence. This prevents hallucinated additions while allowing the system to catch things all LLM reviewers missed.

Analytics track TP/FP rate per source -- over time you learn which reviewer (LLM or local) is strongest for which finding categories.

### Provenance and Analytics

Every finding carries:
- `source`: which reviewer produced it ("gpt-5.4", "claude", "local-ast", "ruff")
- `evidence`: what data supported the finding (AST facts, similar precedent)
- `calibrator_action`: confirm/dispute/adjust/added
- `similar_precedent`: retrieved feedback entries that influenced calibration

This enables:
- Per-source TP/FP tracking
- Cost/benefit analysis of ensemble vs single-model
- Continuous improvement of local rules based on what LLMs consistently catch

## What NOT to Do

- Don't build a universal multi-language static analyzer -- scope AST analysis narrowly, lean on existing linters
- Don't make LSP foundational -- keep it as optional future enrichment
- Don't try to replace remote LLMs with local models for primary review
- Don't do a 1:1 translation -- rethink the architecture for Rust's strengths
- Don't kill the TS version until Rust has full feature parity

## Decision Framework

**Port if**: you want third-opinion to become a "reviewer orchestra" -- local analysis + linter orchestration + multi-model ensemble + feedback-augmented calibration. The architecture is fundamentally more capable than what TS can practically deliver. Phase 0 (Rust core callable from TS) lets you get value immediately while building toward the full vision.

**Don't port if**: the goal is just "same features, different language." But that's no longer what this is -- the reimagined architecture couldn't reasonably be built in TypeScript (native tree-sitter, linked ruff, daemon-cached embeddings, <30MB idle footprint).

**Hybrid approach**: Phase 0 first. Build the Rust analysis core (AST hydration + local reviewer + linter orchestration), call it from TS via subprocess. Test whether hydrated context improves LLM review quality before committing to the full rewrite. This is both the lowest-risk path and a meaningful quality experiment.
