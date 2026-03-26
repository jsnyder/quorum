# Quorum TODO — Post v0.5.1

## Tuning & Validation (built but untested)

### 1. Wire FeedbackIndex into calibrator
- `feedback_index.rs` can retrieve similar entries but calibrator still uses inline Jaccard
- Replace `finding_feedback_similarity()` in `calibrator.rs` with `FeedbackIndex::find_similar()`
- Effort: Small | Impact: High

### 2. Build and test with `--features embeddings`
- fastembed + bge-small-en-v1.5 is wired but never run in production
- `cargo build --release --features embeddings` then review real files
- Compare calibrator precision with/without embeddings on same files
- Effort: Small | Impact: High

### 3. Test `--diff-file` on real git diffs
- Diff parser and pipeline wiring done, never tested on actual `git diff` output
- Run: `git diff HEAD~1 > /tmp/diff.patch && quorum review src/*.rs --diff-file /tmp/diff.patch`
- Measure prompt size reduction, verify finding quality maintained
- Effort: Small | Impact: Medium

### 4. Tune calibrator weights
- Provenance weights (human=1.0, auto=0.5, unknown=0.3) are initial guesses
- Recency half-life (~42 days) is a guess
- FP suppress threshold (1.5 weighted) is a guess
- Run calibrator on known files, review what gets suppressed/boosted, adjust
- Effort: Medium | Impact: Medium

### 5. Real multi-turn agent loop (v0.6)
- Current `--deep` is single-pass MVP — tools described in prompt but never called
- AgentConfig limits (max_iterations=3, max_tool_calls=10, max_bytes=50K) not enforced
- Implement: send tool defs → parse tool_calls → execute → append results → iterate
- Use `chat_with_tools()` method already in llm_client.rs
- Effort: Large | Impact: High

### 6. Embedding vs Jaccard A/B benchmark
- Take 20 finding titles as queries against 932 feedback entries
- Compare retrieval precision: raw Jaccard vs embedding vs embedding+pattern normalization
- Determines if embeddings are worth the 70MB model download + latency
- Effort: Medium | Impact: Medium

### 7. Backfill legacy provenance
- 900+ legacy entries have `provenance: Unknown`
- New entries correctly tagged as Human or AutoCalibrate
- Option A: backfill based on `model` field (if starts with "auto-calibrate:" → AutoCalibrate)
- Option B: let new data accumulate, Unknown decays via recency weighting
- Effort: Small | Impact: Low

## Feature Gaps

### 8. LLM-only fallback for unsupported languages
- Files with unknown extensions (.go, .java, .rb, .c) are skipped entirely
- Could still send to LLM for review without local AST analysis
- Effort: Small | Impact: Medium

### 9. Update skills to v0.5
- `~/.claude/skills/quorum-cli/skill.md` doesn't mention --deep, --diff-file, --reasoning-effort, --calibration-model
- Model recommendation table not in skill
- Effort: Small | Impact: Low

### 10. Context7 fetcher is synchronous and slow
- `Context7HttpFetcher` uses `block_in_place` + `reqwest::blocking`
- Could be made async or cached per-project
- Effort: Medium | Impact: Low

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
