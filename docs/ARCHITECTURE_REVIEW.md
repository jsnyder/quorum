# Architecture Review — v0.4.0

**Date**: 2026-03-25
**Reviewer**: GPT-5.4 (via PAL)
**Scope**: Current implementation vs original ARCHITECTURE.md vision

## Phase Completion

| Phase | Target | Realized | Key Gaps |
|-------|--------|:--------:|----------|
| Phase 0: Core Library | Parsing, hydration, analysis, linters, findings | **~80%** | Hydration uses full-file range, not diff-aware |
| Phase 1: CLI + DSRs | DSRs typed output, tool calling, ensemble, pipeline | **~60%** | DSRs skipped (raw JSON instead), no MIPROv2 |
| Phase 2: MCP + Feedback RAG | MCP server, redaction, domain, embeddings | **~65%** | Feedback retrieval is heuristic, not semantic |
| Phase 3: Daemon + Analytics | Daemon, cache, watcher, analytics, embeddings | **~70%** | No local embeddings, strong on operations |

## What's Built Well

1. **Core pipeline shape is correct** — parse → hydrate → parallel reviewers → merge → calibrate → output
2. **Product exceeded the plan** — daemon HTTP API, Context7, Responses API, reasoning_effort, model benchmarking
3. **Calibration as operating principle** — default-on, auto-calibration, 800+ feedback entries
4. **Graceful degradation** — LLM failures don't crash, local analysis always works
5. **Scope discipline** — 4.3k LOC for 22 modules is efficient

## Main Gaps (prioritized)

### 1. Feedback retrieval is heuristic, not semantic
Current: word Jaccard + category exact match in calibrator.
Needed: embedding-backed retrieval for finding similarity.
Impact: Largest strategic gap — the product's differentiator is calibrated review.

### 2. Human vs auto-calibration data separation
Risk: Model-generated verdicts (auto-calibrate) feed back into future calibration.
Needed: Provenance flag, confidence weighting, ability to exclude model verdicts.

### 3. Calibrator is too blunt
Current: Binary suppress/boost based on TP/FP count thresholds.
Needed: Weighted scoring (similarity, language, source, recency, verdict type).

### 4. Hydration is full-file, not diff-aware
Current: `&[(1, total_lines)]` — entire file as "changed range."
Needed: Target changed hunks, bound context size.

### 5. DSRs decision needed
Either recommit (for typed output, tool calling, MIPROv2) or formally de-scope.
Recommendation: De-scope unless deep review tool-calling becomes priority.

## Recommended Next Roadmap

### Near term
1. Embedding-backed feedback retrieval (biggest leverage)
2. Human vs auto-calibration data separation
3. Weighted calibrator scoring
4. Diff-aware hydration

### Medium term
5. Provenance-preserving merge + analytics hardening
6. Calibrator confidence/downranking (not just severity boost)
7. Optional local embeddings for offline/daemon mode

### Later / conditional
8. DSRs deep-review tool-calling (if justified)
9. MIPROv2 prompt optimization (if demonstrably valuable)
10. Calibrator-added findings (after grounding is solid)
