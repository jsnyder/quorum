# Reliability & Correctness Bugfix Rollup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 4 bugs: harden logistic.rs assertions (#374), validate embed_batch output (#353), reject --rev with --path (#289), and omit temperature for reasoning models (#432).

**Architecture:** Each bug is independent — one task per bug, each producing a self-contained commit. All fixes are in existing files with no new modules. Tests use the existing `#[cfg(test)] mod tests` blocks in each file.

**Tech Stack:** Rust, serde_json, anyhow, clap (CLI), assert_cmd (integration tests)

---

### Task 1: Harden debug_assert to assert in logistic.rs (#374)

**Files:**
- Modify: `src/logistic.rs:77-81` (predict_one debug_assert_eq)
- Modify: `src/logistic.rs:140-143` (fit debug_assert)
- Modify: `src/logistic.rs:132` (fit — add lambda assertion)
- Test: `src/logistic.rs` (inline test module, starting at line 237)

- [ ] **Step 1: Write failing tests for predict_one dimension mismatch**

Add to the `mod tests` block at the bottom of `src/logistic.rs`:

```rust
#[test]
#[should_panic(expected = "input dimension must match model")]
fn predict_one_wrong_dimension_panics() {
    let model = LogisticFit {
        coefficients: vec![1.0, 2.0],
        intercept: 0.0,
        feature_means: vec![0.0, 0.0],
        feature_stddevs: vec![1.0, 1.0],
    };
    model.predict_one(&[1.0, 2.0, 3.0]); // 3 features vs 2 coefficients
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum predict_one_wrong_dimension_panics`
Expected: FAIL — `debug_assert_eq!` is stripped in test builds (which use debug mode, so it actually passes in debug — but the point is it would fail in release). The test should pass in debug mode since `debug_assert_eq!` fires. Verify: `rtk cargo test --bin quorum --release predict_one_wrong_dimension_panics` — this will FAIL because `debug_assert_eq!` is stripped.

- [ ] **Step 3: Replace debug_assert_eq with assert_eq in predict_one**

In `src/logistic.rs`, change line 77:

```rust
// Before:
        debug_assert_eq!(
            x.len(),
            self.coefficients.len(),
            "input dimension must match model"
        );
// After:
        assert_eq!(
            x.len(),
            self.coefficients.len(),
            "input dimension must match model"
        );
```

- [ ] **Step 4: Run test in release mode to verify it now passes**

Run: `rtk cargo test --bin quorum --release predict_one_wrong_dimension_panics`
Expected: PASS (panic fires in release mode)

- [ ] **Step 5: Write failing test for ragged matrix in fit**

Add to `mod tests`:

```rust
#[test]
#[should_panic(expected = "feature matrix must be rectangular")]
fn fit_ragged_matrix_panics() {
    let x = vec![vec![1.0, 2.0], vec![3.0]]; // ragged
    let y = vec![true, false];
    fit(&x, &y, 0.1, 100);
}
```

- [ ] **Step 6: Replace debug_assert with assert in fit**

In `src/logistic.rs`, change line 140:

```rust
// Before:
    debug_assert!(
        x.iter().all(|row| row.len() == p),
        "feature matrix must be rectangular"
    );
// After:
    assert!(
        x.iter().all(|row| row.len() == p),
        "feature matrix must be rectangular"
    );
```

- [ ] **Step 7: Run test to verify it passes**

Run: `rtk cargo test --bin quorum fit_ragged_matrix_panics`
Expected: PASS

- [ ] **Step 8: Write failing test for negative lambda**

Add to `mod tests`:

```rust
#[test]
#[should_panic(expected = "lambda must be non-negative")]
fn fit_negative_lambda_panics() {
    let x = vec![vec![1.0], vec![2.0]];
    let y = vec![true, false];
    fit(&x, &y, -0.1, 100);
}
```

- [ ] **Step 9: Add lambda assertion in fit**

In `src/logistic.rs`, add after line 138 (the existing `assert_eq!` for x.len()/y.len()):

```rust
    assert!(lambda >= 0.0, "lambda must be non-negative");
```

The full block after line 132 should read:

```rust
pub fn fit(x: &[Vec<f64>], y: &[bool], lambda: f64, max_iter: usize) -> LogisticFit {
    assert!(!x.is_empty(), "fit requires at least one sample");
    assert_eq!(
        x.len(),
        y.len(),
        "feature matrix rows must match label count"
    );
    assert!(lambda >= 0.0, "lambda must be non-negative");
    let p = x[0].len();
    assert!(
        x.iter().all(|row| row.len() == p),
        "feature matrix must be rectangular"
    );
```

- [ ] **Step 10: Run all logistic tests**

Run: `rtk cargo test --bin quorum logistic`
Expected: ALL PASS (including existing tests — lambda=0.1 is valid)

- [ ] **Step 11: Commit**

```bash
rtk git add src/logistic.rs
git commit -m "fix(logistic): harden debug_assert to assert for release-mode safety (#374)"
```

---

### Task 2: Validate embed_batch output length (#353)

**Files:**
- Modify: `src/embeddings.rs:44-46` (embed_batch function)
- Test: `src/embeddings.rs` (inline test module, starting at line 63)

- [ ] **Step 1: Write failing test for empty input**

Add to the `mod tests` block in `src/embeddings.rs` (this test doesn't need the `embeddings` feature since it tests the contract, not the model):

Since `LocalEmbedder` is behind `#[cfg(feature = "embeddings")]` and requires the actual model, we can't easily unit-test `embed_batch` without it. Instead, add the test gated on the feature:

```rust
#[cfg(feature = "embeddings")]
#[test]
fn embed_batch_empty_input_returns_empty() {
    let mut embedder = match LocalEmbedder::new() {
        Ok(e) => e,
        Err(err) => {
            eprintln!("skipping: embedding model unavailable: {err}");
            return;
        }
    };
    let result = embedder.embed_batch(&[]).unwrap();
    assert!(result.is_empty(), "empty input should produce empty output");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum --features embeddings embed_batch_empty_input`
Expected: Behavior depends on fastembed — may pass or panic. The point is to establish the contract.

- [ ] **Step 3: Implement embed_batch validation**

In `src/embeddings.rs`, replace the `embed_batch` function (lines 44-46):

```rust
// Before:
    pub fn embed_batch(&mut self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.model.embed(texts, None)
    }

// After:
    pub fn embed_batch(&mut self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let results = self.model.embed(texts, None)?;
        if results.len() != texts.len() {
            anyhow::bail!(
                "embedding batch size mismatch: got {} vectors for {} inputs",
                results.len(),
                texts.len()
            );
        }
        Ok(results)
    }
```

- [ ] **Step 4: Run all embeddings tests**

Run: `rtk cargo test --bin quorum embeddings`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
rtk git add src/embeddings.rs
git commit -m "fix(embeddings): validate embed_batch returns one result per input (#353)"
```

---

### Task 3: Reject --rev with --path in context add (#289)

**Files:**
- Modify: `src/main.rs:594-605` (run_context match arm)
- Test: `tests/context_cli.rs` (integration test)

- [ ] **Step 1: Write failing integration test**

Add to `tests/context_cli.rs`:

```rust
#[test]
fn context_add_path_with_rev_is_rejected() {
    let tmp = TempDir::new().unwrap();
    quorum(tmp.path())
        .args([
            "context",
            "add",
            "--name",
            "core",
            "--kind",
            "rust",
            "--path",
            "/some/path",
            "--rev",
            "main",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--rev"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --test context_cli context_add_path_with_rev_is_rejected`
Expected: FAIL — currently `--rev` is silently ignored, so the command succeeds (or fails for a different reason)

- [ ] **Step 3: Add validation in run_context**

In `src/main.rs`, modify the match block at line 594. Insert a new arm before the existing `(Some(p), None)`:

```rust
            let location = match (a.path, a.git) {
                (Some(_), None) if a.rev.is_some() => {
                    eprintln!("error: --rev may only be used with --git, not --path");
                    return 1;
                }
                (Some(p), None) => AddLocation::Path(p),
                (None, Some(url)) => AddLocation::Git { url, rev: a.rev },
                (Some(_), Some(_)) => {
                    eprintln!("error: --path and --git are mutually exclusive");
                    return 1;
                }
                (None, None) => {
                    eprintln!("error: one of --path or --git is required");
                    return 1;
                }
            };
```

- [ ] **Step 4: Run test to verify it passes**

Run: `rtk cargo test --test context_cli context_add_path_with_rev_is_rejected`
Expected: PASS

- [ ] **Step 5: Run all context CLI tests**

Run: `rtk cargo test --test context_cli`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
rtk git add src/main.rs tests/context_cli.rs
git commit -m "fix(cli): reject --rev when used with --path in context add (#289)"
```

---

### Task 4: Omit temperature for reasoning models (#432)

**Files:**
- Modify: `src/llm_client.rs:270` (add supports_temperature function near RESPONSES_API_MODELS)
- Modify: `src/llm_client.rs:785-793` (judge_completion)
- Modify: `src/llm_client.rs:949-956` (chat_completion)
- Modify: `src/llm_client.rs:1026-1029` (responses_api)
- Modify: `src/llm_client.rs:1096-1101` (chat_with_tools)
- Test: `src/llm_client.rs` (inline test module)

- [ ] **Step 1: Write failing tests for supports_temperature**

Add to the `mod tests` block in `src/llm_client.rs`:

```rust
#[test]
fn supports_temperature_gpt4o() {
    assert!(super::supports_temperature("gpt-4o"));
    assert!(super::supports_temperature("gpt-4o-mini"));
    assert!(super::supports_temperature("gpt-4-turbo"));
}

#[test]
fn supports_temperature_rejects_reasoning_models() {
    assert!(!super::supports_temperature("gpt-5.4"));
    assert!(!super::supports_temperature("gpt-5.5"));
    assert!(!super::supports_temperature("gpt-5"));
    assert!(!super::supports_temperature("gpt-5-mini"));
    assert!(!super::supports_temperature("gpt-5-nano"));
    assert!(!super::supports_temperature("gpt-5.1"));
    assert!(!super::supports_temperature("gpt-5.3-codex"));
    assert!(!super::supports_temperature("o1"));
    assert!(!super::supports_temperature("o1-mini"));
    assert!(!super::supports_temperature("o3"));
    assert!(!super::supports_temperature("o3-mini"));
    assert!(!super::supports_temperature("o4-mini"));
}

#[test]
fn supports_temperature_case_insensitive() {
    assert!(!super::supports_temperature("GPT-5.4"));
    assert!(!super::supports_temperature("O3-mini"));
    assert!(super::supports_temperature("GPT-4o"));
}

#[test]
fn supports_temperature_non_openai_models() {
    assert!(super::supports_temperature("claude-sonnet-4-5-20250514"));
    assert!(super::supports_temperature("gemini-2.5-pro"));
    assert!(super::supports_temperature("deepseek-r1"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test --bin quorum supports_temperature`
Expected: FAIL — function doesn't exist yet

- [ ] **Step 3: Implement supports_temperature**

In `src/llm_client.rs`, add after the `RESPONSES_API_MODELS` constant (after line 275):

```rust
/// Returns `false` for reasoning models that reject the `temperature` parameter.
/// OpenAI o-series and gpt-5.x models return HTTP 400 when temperature is included.
fn supports_temperature(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    !(m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.starts_with("gpt-5"))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test --bin quorum supports_temperature`
Expected: ALL PASS

- [ ] **Step 5: Apply supports_temperature to chat_completion**

In `src/llm_client.rs`, modify the `chat_completion` body construction (around line 949):

```rust
// Before:
        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.3
        });

// After:
        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": prompt}
            ]
        });
        if supports_temperature(model) {
            body["temperature"] = serde_json::json!(0.3);
        }
```

- [ ] **Step 6: Apply supports_temperature to judge_completion**

In `src/llm_client.rs`, modify the `judge_completion` body construction (around line 785):

```rust
// Before:
        let body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": safe_system},
                {"role": "user", "content": safe_prompt}
            ],
            "temperature": 0,
            "max_tokens": 2048
        });

// After:
        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": safe_system},
                {"role": "user", "content": safe_prompt}
            ],
            "max_tokens": 2048
        });
        if supports_temperature(model) {
            body["temperature"] = serde_json::json!(0);
        }
```

- [ ] **Step 7: Apply supports_temperature to responses_api**

In `src/llm_client.rs`, modify the temperature block in `responses_api` (around line 1026):

```rust
// Before:
        // Codex models don't support temperature; only add for non-codex responses API models
        if !model.contains("codex") {
            body["temperature"] = serde_json::json!(0.3);
        }

// After:
        if supports_temperature(model) {
            body["temperature"] = serde_json::json!(0.3);
        }
```

- [ ] **Step 8: Apply supports_temperature to chat_with_tools**

In `src/llm_client.rs`, modify the `chat_with_tools` body construction (around line 1096):

```rust
// Before:
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": 0.3,
            "tools": tools
        });

// After:
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "tools": tools
        });
        if supports_temperature(model) {
            body["temperature"] = serde_json::json!(0.3);
        }
```

- [ ] **Step 9: Run all llm_client tests**

Run: `rtk cargo test --bin quorum llm_client`
Expected: ALL PASS

- [ ] **Step 10: Commit**

```bash
rtk git add src/llm_client.rs
git commit -m "fix(llm): omit temperature for reasoning models that reject it (#432)"
```
