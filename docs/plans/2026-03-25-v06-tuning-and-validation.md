# Quorum TODO — Post v0.5.1

## Tuning & Validation

### ~~1. Wire FeedbackIndex into calibrator~~ DONE
- Pipeline builds FeedbackIndex from feedback store, uses `calibrate_with_index()`
- Semantic similarity (embeddings) with Jaccard fallback

### ~~2. Build and test with `--features embeddings`~~ DONE
- Embeddings now default feature — bge-small-en-v1.5 on by default
- A/B showed 0.94 sim for exact precedents vs 0.70 Jaccard plateau, 8 correct boosts

### ~~3. Test `--diff-file` on real git diffs~~ DONE
- Works correctly — "Diff-aware: scoping hydration to N changed file(s)"
- Same finding count, LLM focuses on changed lines

### ~~4. Tune calibrator weights~~ DONE
- TP must dominate FP by 1.5x to confirm, 2x to boost/suppress
- Wontfix counts on FP side (not-worth-fixing ~ FP for calibration)
- Added PostFix provenance (1.5x weight) for post-fix feedback
- Weights: PostFix=1.5, Human=1.0, Auto=0.5, Unknown=0.3

### ~~5. Real multi-turn agent loop (v0.6)~~ DONE
- Implemented AgentReviewer trait + agent_loop with full tool execution
- State machine: send → parse tool_calls → execute via ToolRegistry → accumulate → iterate
- Bounded by max_iterations, max_tool_calls, max_bytes_read (all enforced)
- Unicode-safe truncation, ANSI injection prevention in progress output

### ~~6. Embedding vs Jaccard A/B benchmark~~ DONE
- Embeddings clearly superior: richer precedent matching, precise sim scores
- Worth the 70MB download — made default feature

### ~~7. Backfill legacy provenance~~ DONE
- Backfilled 637 entries: 479 human, 158 auto_calibrate, 291 remain unknown
- Calibrator now correctly suppresses 4 previously over-confirmed findings

## Feature Gaps

### ~~8. LLM-only fallback for unsupported languages~~ DONE
- Unknown extensions (.go, .java, .rb, .c) now get LLM-only review
- No AST, but still gets LLM + Context7 + calibration + auto-calibration

### ~~9. Update skills to v0.5/v0.6~~ DONE
- Added --deep, --diff-file, --reasoning-effort, --calibration-model docs
- Model recommendation table, feedback provenance weights

### ~~10. Context7 fetcher async conversion~~ DONE
- Replaced reqwest::blocking::Client with reqwest::Client
- Uses shared block_on_async helper, eliminates separate blocking thread pool

## Model & Benchmark Notes

### Optimal configurations (from benchmarks)
- **Default**: gpt-5.4 (reasoning_effort=low) + gpt-5.3-codex calibration (~2s)
- **No gpt-5.4**: gpt-5.2 (reasoning_effort=low) + gpt-5.3-codex calibration (~26s)
- **Fast CI**: gpt-5.3-codex, no calibration (~1s)
- **Deep audit**: gpt-5.2 (high) + o3 calibration (~100s)

### Reasoning effort findings
- `none` and `low` are nearly identical quality for code review (1s each)
- `medium` adds ~1 finding for 20x latency
- `high` adds marginal value for 200x latency
- gpt-5.3-codex ignores reasoning_effort entirely (always 1s)

### Calibrator model comparison
- o3: 61% TP, 25% partial — nuanced, contextual, but slow (34s)
- gpt-5.3-codex: 78% TP, 11% partial — decisive, fast (9s)
- Self-calibration (5.4→5.4): 67% TP — sometimes disagrees with itself
