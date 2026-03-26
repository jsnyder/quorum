# v0.5 Intelligence Upgrade Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Upgrade quorum's feedback intelligence, add tool-calling deep review, and make hydration diff-aware — all as optional features with keep/discard criteria.

**Architecture:** Each feature is behind a feature flag or runtime option. Develop on `feat/v05-intelligence` branch. After each chunk, benchmark against current v0.4.0 baseline. Features that don't measurably improve review quality get removed before merge.

**Tech Stack:** fastembed (ONNX Runtime, auto-downloads nomic-embed-text), schemars (JSON Schema from Rust types), serde, reqwest, tree-sitter

**Branch:** `feat/v05-intelligence` from `main`

---

## Keep/Discard Criteria

Each feature is evaluated after implementation against these criteria:

| Feature | KEEP if... | DISCARD if... |
|---------|-----------|---------------|
| Feedback provenance | Auto-cal verdicts separable from human | N/A (always keep, low cost) |
| Embedding retrieval | Calibrator precision improves ≥5% on benchmark files | Precision unchanged or worse, or >2s latency per review |
| Tool calling | Finds ≥2 bugs per file that non-tool review misses | Adds >30s latency with <1 unique finding on average |
| Weighted calibrator | FP suppression improves without losing TPs | Over-suppresses TPs or adds complexity without measurable gain |
| Agent loop | Deep review finds real bugs on complex files (pipelines.py) | Token cost >$0.10/file without proportional quality gain |
| Diff-aware hydration | Prompt size reduces ≥30% on multi-file reviews | Misses relevant context that full-file hydration catches |

---

## Chunk 1: Foundation (feedback provenance + remove dspy-rs)

### Task 1: Remove dspy-rs dependency

**Files:**
- Modify: `Cargo.toml`

**Step 1:** Remove `dspy-rs = "0.7"` from `[dependencies]`

**Step 2:** Run `cargo test` — expect PASS (dspy-rs is unused)

**Step 3:** Run `cargo build --release` — verify binary size decreases

**Step 4:** Commit: `chore: remove unused dspy-rs dependency`

---

### Task 2: Add provenance field to FeedbackEntry

**Files:**
- Modify: `src/feedback.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn feedback_entry_has_provenance() {
    let entry = FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: "Bug".into(),
        finding_category: "security".into(),
        verdict: Verdict::Tp,
        reason: "Real bug".into(),
        model: Some("gpt-5.4".into()),
        timestamp: Utc::now(),
        provenance: Provenance::Human,
    };
    assert_eq!(entry.provenance, Provenance::Human);
}

#[test]
fn feedback_entry_auto_calibrate_provenance() {
    let entry = FeedbackEntry {
        // ... same fields ...
        provenance: Provenance::AutoCalibrate("o3".into()),
    };
    match &entry.provenance {
        Provenance::AutoCalibrate(model) => assert_eq!(model, "o3"),
        _ => panic!("Expected AutoCalibrate"),
    }
}

#[test]
fn provenance_serializes_correctly() {
    let human = serde_json::to_value(&Provenance::Human).unwrap();
    assert_eq!(human, "human");
    let auto = serde_json::to_value(&Provenance::AutoCalibrate("o3".into())).unwrap();
    assert!(auto.to_string().contains("o3"));
}

#[test]
fn legacy_entries_without_provenance_load_as_unknown() {
    // Existing JSONL entries don't have provenance field
    let json = r#"{"file_path":"test.rs","finding_title":"Bug","finding_category":"security","verdict":"tp","reason":"test","model":"gpt-5.4","timestamp":"2026-01-01T00:00:00Z"}"#;
    let entry: FeedbackEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.provenance, Provenance::Unknown);
}
```

**Step 2:** Run tests — expect FAIL (Provenance doesn't exist)

**Step 3: Implement**

Add to `feedback.rs`:
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Human,
    AutoCalibrate(String), // model name
    Unknown,
}

impl Default for Provenance {
    fn default() -> Self { Provenance::Unknown }
}
```

Add `#[serde(default)] pub provenance: Provenance` to `FeedbackEntry`.

**Step 4:** Run tests — expect PASS

**Step 5:** Update `auto_calibrate.rs` to set `provenance: Provenance::AutoCalibrate(model.into())` and MCP feedback handler to set `Provenance::Human`.

**Step 6:** Run full `cargo test` — expect PASS

**Step 7:** Commit: `feat: feedback provenance - human vs auto-calibrate separation`

---

### Task 3: Add provenance filtering to calibrator

**Files:**
- Modify: `src/calibrator.rs`
- Modify: `src/pipeline.rs` (PipelineConfig)

**Step 1: Write failing tests**

```rust
#[test]
fn calibrator_weights_human_feedback_higher() {
    // 1 human FP should count more than 1 auto FP
    let findings = vec![FindingBuilder::new().title("SQL injection").category("security").build()];
    let feedback = vec![
        fb_with_provenance("SQL injection", "security", Verdict::Fp, Provenance::Human),
        fb_with_provenance("SQL injection", "security", Verdict::Fp, Provenance::AutoCalibrate("o3".into())),
    ];
    let config = CalibratorConfig { fp_suppress_count: 2, ..Default::default() };
    let result = calibrate(findings, &feedback, &config);
    assert_eq!(result.suppressed, 1, "2 FPs (1 human + 1 auto) should suppress");
}

#[test]
fn calibrator_can_exclude_auto_feedback() {
    let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
    let feedback = vec![
        fb_with_provenance("Bug", "test", Verdict::Fp, Provenance::AutoCalibrate("o3".into())),
        fb_with_provenance("Bug", "test", Verdict::Fp, Provenance::AutoCalibrate("o3".into())),
    ];
    let config = CalibratorConfig {
        fp_suppress_count: 2,
        use_auto_feedback: false, // NEW: exclude auto feedback
        ..Default::default()
    };
    let result = calibrate(findings, &feedback, &config);
    assert_eq!(result.suppressed, 0, "Auto feedback excluded, should not suppress");
}
```

**Step 2:** Run tests — expect FAIL

**Step 3:** Add `use_auto_feedback: bool` to CalibratorConfig (default true). Filter feedback entries by provenance when `use_auto_feedback` is false.

**Step 4:** Run tests — expect PASS

**Step 5:** Commit: `feat: calibrator provenance filtering - human vs auto feedback`

---

## Chunk 2: Embedding-Backed Feedback Retrieval

### Task 4: Add fastembed dependency and embedding module

**Files:**
- Modify: `Cargo.toml` — add `fastembed = { version = "5", optional = true }`
- Create: `src/embeddings.rs`
- Modify: `Cargo.toml` — add `[features] embeddings = ["fastembed"]`

**Step 1:** Add fastembed as optional dep with feature flag `embeddings`

**Step 2: Write failing tests** (gated behind `#[cfg(feature = "embeddings")]`)

```rust
#[cfg(feature = "embeddings")]
mod tests {
    use super::*;

    #[test]
    fn embed_text_returns_vector() {
        let embedder = LocalEmbedder::new().unwrap();
        let vec = embedder.embed("SQL injection in auth module").unwrap();
        assert!(vec.len() > 100); // nomic-embed-text produces 768-dim vectors
    }

    #[test]
    fn similar_texts_have_high_cosine() {
        let embedder = LocalEmbedder::new().unwrap();
        let a = embedder.embed("SQL injection vulnerability").unwrap();
        let b = embedder.embed("SQL injection in query").unwrap();
        let c = embedder.embed("Unused import os").unwrap();
        assert!(cosine_similarity(&a, &b) > 0.8);
        assert!(cosine_similarity(&a, &c) < 0.5);
    }

    #[test]
    fn embed_feedback_entries() {
        let embedder = LocalEmbedder::new().unwrap();
        let entries = vec![
            ("SQL injection", "security"),
            ("Unused import", "style"),
        ];
        let vecs: Vec<Vec<f32>> = entries.iter()
            .map(|(title, cat)| embedder.embed(&format!("{} {}", title, cat)).unwrap())
            .collect();
        assert_eq!(vecs.len(), 2);
    }
}
```

**Step 3:** Implement `src/embeddings.rs`:
- `LocalEmbedder` struct wrapping `fastembed::TextEmbedding`
- Uses `nomic-embed-text` model (auto-downloaded on first use, cached in `~/.quorum/models/`)
- `embed(&self, text: &str) -> Result<Vec<f32>>`
- `embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>`
- `cosine_similarity(a: &[f32], b: &[f32]) -> f32`

**Step 4:** Run `cargo test --features embeddings` — expect PASS (first run downloads model)

**Step 5:** Commit: `feat: local embedding module with fastembed + nomic-embed-text`

---

### Task 5: Embedding-backed feedback index

**Files:**
- Create: `src/feedback_index.rs`

**Step 1: Write failing tests**

```rust
#[cfg(feature = "embeddings")]
#[test]
fn index_and_retrieve_similar_feedback() {
    let store = FeedbackStore::new(test_feedback_path());
    // Add some entries
    store.record(&entry("SQL injection in auth", "security", Verdict::Tp)).unwrap();
    store.record(&entry("Unused import os", "style", Verdict::Fp)).unwrap();
    store.record(&entry("SQL injection via f-string", "security", Verdict::Tp)).unwrap();

    let index = FeedbackIndex::build(&store).unwrap();
    let similar = index.find_similar("SQL injection in query builder", 2);

    assert_eq!(similar.len(), 2);
    assert!(similar[0].entry.finding_title.contains("SQL"));
    assert!(similar[0].similarity > 0.7);
}

#[cfg(feature = "embeddings")]
#[test]
fn index_returns_empty_for_no_matches() {
    let store = FeedbackStore::new(test_feedback_path());
    store.record(&entry("SQL injection", "security", Verdict::Tp)).unwrap();

    let index = FeedbackIndex::build(&store).unwrap();
    let similar = index.find_similar("completely unrelated topic about cooking", 5);

    // May return results but with low similarity
    for s in &similar {
        assert!(s.similarity < 0.5);
    }
}
```

**Step 2:** Implement `FeedbackIndex`:
- On build: embed all feedback entry titles+categories
- Store vectors in-memory (Vec<(FeedbackEntry, Vec<f32>)>)
- `find_similar(query: &str, top_k: usize) -> Vec<SimilarEntry>`
- `SimilarEntry { entry: FeedbackEntry, similarity: f32 }`
- Brute-force cosine search (fine for <10K entries)

**Step 3:** Run tests — expect PASS

**Step 4:** Commit: `feat: embedding-backed feedback index for semantic precedent retrieval`

---

### Task 6: Wire embeddings into calibrator (optional path)

**Files:**
- Modify: `src/calibrator.rs`
- Modify: `src/pipeline.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn calibrator_uses_semantic_similarity_when_available() {
    // Test that semantically similar but lexically different findings match
    // "SQL injection via string concatenation" should match "Unvalidated input in SQL query"
    // This would FAIL with word Jaccard but PASS with embeddings
}
```

**Step 2:** Add optional `FeedbackIndex` to calibrator. When available, use embedding similarity instead of word Jaccard. Fall back to Jaccard when embeddings not available.

**Step 3:** Run tests with and without `--features embeddings`

**Step 4:** Benchmark: run same review on nodriver_spider.py, compare calibrator precision with/without embeddings

**Step 5:** Commit: `feat: calibrator uses semantic similarity when embeddings available`

**Step 6: EVALUATION GATE** — Compare calibrator precision on benchmark files. If no measurable improvement, revert and move on.

---

## Chunk 3: Tool Calling

### Task 7: Tool definitions and registry

**Files:**
- Create: `src/tools.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn tool_registry_has_read_file() {
    let registry = ToolRegistry::new("/tmp/test-repo");
    let tools = registry.tool_definitions();
    assert!(tools.iter().any(|t| t.name == "read_file"));
}

#[test]
fn read_file_tool_executes() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.py"), "def hello(): pass").unwrap();
    let registry = ToolRegistry::new(dir.path());
    let result = registry.execute("read_file", r#"{"path":"test.py","start_line":1,"end_line":1}"#).unwrap();
    assert!(result.contains("def hello"));
}

#[test]
fn read_file_confined_to_repo() {
    let registry = ToolRegistry::new("/tmp/test-repo");
    let result = registry.execute("read_file", r#"{"path":"../../etc/passwd"}"#);
    assert!(result.is_err(), "Path traversal should be blocked");
}

#[test]
fn grep_tool_executes() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.py"), "SECRET_KEY = 'abc'\ndef safe(): pass").unwrap();
    let registry = ToolRegistry::new(dir.path());
    let result = registry.execute("grep", r#"{"pattern":"SECRET","max_results":10}"#).unwrap();
    assert!(result.contains("SECRET_KEY"));
}

#[test]
fn tool_output_is_token_bounded() {
    // Large file should be truncated
    let dir = tempdir().unwrap();
    let big = "x\n".repeat(10000);
    std::fs::write(dir.path().join("big.py"), &big).unwrap();
    let registry = ToolRegistry::new(dir.path());
    let result = registry.execute("read_file", r#"{"path":"big.py"}"#).unwrap();
    assert!(result.len() < 20000, "Output should be bounded");
}
```

**Step 2:** Implement `src/tools.rs`:
- `ToolRegistry` with repo root confinement
- `ToolDefinition { name, description, parameters: serde_json::Value }` (JSON Schema via schemars)
- Tools: `read_file(path, start_line?, end_line?)`, `grep(pattern, path_glob?, max_results?)`, `list_files(path?, pattern?)`
- Each tool: path normalization, traversal prevention, output truncation (max 4000 chars)
- `execute(tool_name, args_json) -> Result<String>`

**Step 3:** Run tests — expect PASS

**Step 4:** Commit: `feat: tool registry with read_file, grep, list_files`

---

### Task 8: Tool calling protocol in LLM client

**Files:**
- Modify: `src/llm_client.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn tool_definitions_serialize_to_openai_format() {
    let tools = vec![ToolDefinition {
        name: "read_file".into(),
        description: "Read file contents".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    }];
    let json = format_tools_for_api(&tools);
    assert!(json.to_string().contains("read_file"));
    assert!(json.to_string().contains("function"));
}
```

**Step 2:** Add `chat_with_tools()` method to OpenAiClient:
- Sends tool definitions in OpenAI function calling format
- Detects `tool_calls` in response
- Returns either final text content or list of tool calls to execute

**Step 3:** Run tests — expect PASS

**Step 4:** Commit: `feat: OpenAI function calling protocol in LLM client`

---

### Task 9: Bounded agent loop

**Files:**
- Create: `src/agent.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn agent_loop_produces_findings_without_tools() {
    // When model doesn't call any tools, should return findings directly
    let reviewer = FakeReviewer::with_response("[]");
    let tools = ToolRegistry::new("/tmp");
    let config = AgentConfig { max_iterations: 3, max_tool_calls: 5, max_bytes: 50000 };
    let result = agent_review("fn main() {}", "test.rs", &reviewer, &tools, &config).unwrap();
    assert!(result.is_empty());
}

#[test]
fn agent_loop_respects_max_iterations() {
    // Model that always calls tools should be stopped
    let reviewer = FakeToolCallingReviewer::always_calls("read_file");
    let tools = ToolRegistry::new("/tmp");
    let config = AgentConfig { max_iterations: 2, max_tool_calls: 5, max_bytes: 50000 };
    let result = agent_review("code", "test.rs", &reviewer, &tools, &config);
    // Should complete without infinite loop
    assert!(result.is_ok());
}

#[test]
fn agent_loop_respects_max_bytes() {
    // Should stop accepting tool results after byte budget exceeded
}
```

**Step 2:** Implement `src/agent.rs`:
- `AgentConfig { max_iterations: usize, max_tool_calls: usize, max_bytes_read: usize }`
- `agent_review(code, file_path, reviewer, tools, config) -> Result<Vec<Finding>>`
- State machine: Initial → ToolCall → Observe → (repeat or) Finalize
- Track: iterations, total tool calls, total bytes read
- Hard stop on any limit exceeded

**Step 3:** Run tests — expect PASS

**Step 4:** Commit: `feat: bounded agent loop for deep review`

---

### Task 10: Wire agent loop into pipeline (--deep flag)

**Files:**
- Modify: `src/pipeline.rs`
- Modify: `src/cli/mod.rs`

**Step 1:** Add `--deep` flag to CLI. When set, use agent loop instead of single-pass review.

**Step 2:** Run on benchmark files, compare findings with and without `--deep`

**Step 3:** Commit: `feat: --deep flag enables tool-calling agent review`

**Step 4: EVALUATION GATE** — Run `--deep` on nodriver_spider.py, pipelines.py, server.py. If it finds ≥2 unique bugs per file that single-pass misses, KEEP. Otherwise, DISCARD.

---

## Chunk 4: Weighted Calibrator + Diff-Aware Hydration

### Task 11: Weighted calibrator scoring

**Files:**
- Modify: `src/calibrator.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn weighted_score_considers_recency() {
    // Recent feedback should count more than old feedback
}

#[test]
fn weighted_score_considers_provenance() {
    // Human feedback weighted higher than auto
}

#[test]
fn weighted_score_considers_language_match() {
    // Feedback from same language should match better
}

#[test]
fn weighted_calibrator_produces_confidence_not_just_severity() {
    // Output should include confidence score, not just boost/suppress
}
```

**Step 2:** Replace binary suppress/boost with weighted scoring:
- Similarity score (from embeddings or Jaccard)
- Provenance weight (human=1.0, auto=0.5, unknown=0.3)
- Recency decay (exponential, half-life ~30 days)
- Language match bonus
- Output: confidence score per finding, not just binary action

**Step 3:** Run tests — expect PASS

**Step 4:** Benchmark calibrator precision vs v0.4.0

**Step 5:** Commit: `feat: weighted calibrator scoring with confidence`

---

### Task 12: Diff-aware hydration

**Files:**
- Modify: `src/hydration.rs`
- Modify: `src/pipeline.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn hydrate_only_changed_functions() {
    // Given a diff that changes lines 10-20, only hydrate context for that region
    // Not the entire file
}

#[test]
fn hydrate_with_diff_produces_smaller_context() {
    let full_ctx = hydrate(&tree, source, lang, &[(1, total_lines)]);
    let diff_ctx = hydrate(&tree, source, lang, &[(10, 20)]);
    // Diff-aware context should be smaller
    assert!(context_size(&diff_ctx) < context_size(&full_ctx));
}
```

**Step 2:** When a diff is available (via `--diff-file` or git), pass only changed line ranges to hydration instead of full file.

**Step 3:** Add `--diff-file <path>` CLI flag

**Step 4:** Benchmark prompt size reduction on multi-file reviews

**Step 5:** Commit: `feat: diff-aware hydration for targeted context`

**Step 6: EVALUATION GATE** — Measure prompt size reduction and whether review quality is maintained.

---

## Chunk 5: Integration + Benchmark

### Task 13: Full integration test

Run the complete upgraded pipeline on benchmark files:
- nodriver_spider.py (705 lines)
- pipelines.py (3800 lines)
- server.py (205 lines)
- AuthMiddleware.ts (223 lines)
- calibrator.rs (367 lines)

Compare v0.4.0 vs v0.5.0:
- Finding count
- TP/FP rate
- Calibrator precision
- Latency
- Token cost

### Task 14: Feature flag evaluation

For each optional feature, run the keep/discard evaluation from the criteria table above. Remove features that don't meet their criteria.

### Task 15: Merge or discard

If net positive: merge `feat/v05-intelligence` → `main`, bump to v0.5.0
If mixed: cherry-pick only the features that passed evaluation
If net negative: close the branch, document learnings

---

## Testing Strategy

Per @test:antipatterns recommendations:
- Unit tests for each module in `#[cfg(test)]`
- Embedding tests gated behind `#[cfg(feature = "embeddings")]`
- Tool tests use `tempfile::TempDir` for isolation
- Agent loop tests use `FakeReviewer` (no real API calls)
- Integration tests in `tests/` for CLI flags
- No mocking of internal code — test through public APIs
- Each test has one reason to fail
