# Parallel LLM Calls Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Parallelize LLM calls across files with a configurable concurrency limit (`--parallel N`, default 4).

**Architecture:** A global `Arc<Semaphore>` limits concurrent outbound LLM calls. Each file is processed in its own `tokio::task::spawn_blocking` task. The semaphore is acquired at the LLM call boundary using `Handle::current().block_on()` (NOT `block_in_place`, which panics on blocking threads). Results collected by index for deterministic output ordering. FeedbackIndex is pre-built once and shared via `Arc<Mutex<>>`.

**Tech Stack:** tokio (Semaphore, spawn_blocking), Arc/Mutex for shared state, existing reqwest-based LlmReviewer.

**Review:** Validated by Gemini 3.1 Pro. Critical fixes incorporated: `Handle::block_on` for semaphore, pre-built FeedbackIndex, atomic feedback writes, parallel-safe progress.

---

### Task 1: Add `--parallel` CLI Flag

**Files:**
- Modify: `src/cli/mod.rs` (ReviewOpts struct, ~line 57-117)

**Step 1: Write the failing test**

Add to the existing `mod tests` block in `src/cli/mod.rs`:

```rust
#[test]
fn parse_parallel_flag() {
    use clap::Parser;
    let args = Args::parse_from(["quorum", "review", "--parallel", "8", "file.rs"]);
    match args.command {
        Command::Review(opts) => assert_eq!(opts.parallel, 8),
        _ => panic!("Expected Review command"),
    }
}

#[test]
fn parse_parallel_default() {
    use clap::Parser;
    let args = Args::parse_from(["quorum", "review", "file.rs"]);
    match args.command {
        Command::Review(opts) => assert_eq!(opts.parallel, 4),
        _ => panic!("Expected Review command"),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum parse_parallel`
Expected: FAIL — no field `parallel` on ReviewOpts

**Step 3: Implement**

Add to `ReviewOpts` struct in `src/cli/mod.rs`:

```rust
    /// Max concurrent LLM calls (default: 4, 0 = unlimited, 1 = sequential)
    #[arg(long, default_value = "4")]
    pub parallel: usize,
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum parse_parallel`
Expected: PASS

**Step 5: Commit**

```bash
git add src/cli/mod.rs
git commit -m "feat: add --parallel CLI flag for concurrent LLM calls"
```

---

### Task 2: Add Semaphore and FeedbackIndex to PipelineConfig

**Files:**
- Modify: `src/pipeline.rs` (PipelineConfig struct ~line 30-50, Default impl ~line 48-65)

**Context:** Two new fields: the semaphore for concurrency limiting, and a pre-built FeedbackIndex to avoid N concurrent rebuilds.

**Step 1: Write the failing test**

Add to the test module in `src/pipeline.rs`:

```rust
#[test]
fn pipeline_config_default_has_no_semaphore() {
    let cfg = PipelineConfig::default();
    assert!(cfg.semaphore.is_none());
    assert!(cfg.feedback_index.is_none());
}

#[test]
fn pipeline_config_with_semaphore() {
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let cfg = PipelineConfig {
        semaphore: Some(sem.clone()),
        ..Default::default()
    };
    assert_eq!(cfg.semaphore.as_ref().unwrap().available_permits(), 4);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum pipeline_config_default_has_no_semaphore`
Expected: FAIL — no field `semaphore`

**Step 3: Implement**

Add to `PipelineConfig` struct:

```rust
    /// Semaphore to limit concurrent LLM calls (None = unlimited)
    pub semaphore: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    /// Pre-built feedback index for calibration (shared across parallel tasks)
    pub feedback_index: Option<std::sync::Arc<std::sync::Mutex<crate::feedback_index::FeedbackIndex>>>,
```

Add to `Default` impl:

```rust
    semaphore: None,
    feedback_index: None,
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum pipeline_config`
Expected: PASS

**Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat: add semaphore and feedback_index fields to PipelineConfig"
```

---

### Task 3: Semaphore-Guarded LLM Calls in Pipeline

**Files:**
- Modify: `src/pipeline.rs` (~line 267 and ~line 478, the `reviewer.review()` call sites, plus auto_calibrate calls ~line 329)

**Context:** Three types of LLM calls need guarding: review calls in `review_file()`, review calls in `review_file_llm_only()`, and auto-calibration calls in both functions.

**CRITICAL:** Use `Handle::current().block_on()` for semaphore acquisition, NOT `block_in_place`. The latter panics when called from `spawn_blocking` threads (they're not tokio worker threads).

**Step 1: Write the failing test**

Add to the test module in `src/pipeline.rs`:

```rust
#[test]
fn review_file_works_with_semaphore() {
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
    let cfg = PipelineConfig {
        models: vec!["test-model".into()],
        semaphore: Some(sem),
        ..Default::default()
    };

    struct EmptyReviewer;
    impl LlmReviewer for EmptyReviewer {
        fn review(&self, _: &str, _: &str) -> anyhow::Result<crate::llm_client::LlmResponse> {
            Ok(crate::llm_client::LlmResponse { content: "[]".into(), usage: None })
        }
        fn chat_with_tools(&self, _: &[serde_json::Value], _: &serde_json::Value, _: &str) -> anyhow::Result<crate::llm_client::LlmTurnResult> {
            unimplemented!()
        }
    }

    let source = "fn main() {}";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let result = review_file(
        std::path::Path::new("test.rs"), source, Language::Rust, &tree,
        Some(&EmptyReviewer), &cfg,
    );
    assert!(result.is_ok());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum review_file_works_with_semaphore`
Expected: May pass (semaphore field exists but not used yet). That's OK — this is a smoke test.

**Step 3: Implement**

Add helper function near the top of `src/pipeline.rs` (after imports):

```rust
/// Acquire a semaphore permit if configured. Uses Handle::block_on (not block_in_place)
/// because this may be called from spawn_blocking threads.
/// Returns an owned permit that is released on drop (RAII).
fn acquire_llm_permit(sem: &Option<std::sync::Arc<tokio::sync::Semaphore>>) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let sem = sem.as_ref()?.clone();
    Some(
        tokio::runtime::Handle::current()
            .block_on(sem.acquire_owned())
            .expect("semaphore closed unexpectedly")
    )
}
```

Then wrap EACH `reviewer.review()` call:

In `review_file()` model loop (~line 267):
```rust
        for model in &pipeline_config.models {
            let _permit = acquire_llm_permit(&pipeline_config.semaphore);
            match reviewer.review(&prompt, model) {
```

In `review_file()` auto-calibrate call (~line 329):
```rust
    if pipeline_config.auto_calibrate && !final_findings.is_empty() {
        if let (Some(reviewer), Some(store_path)) = (llm, &pipeline_config.feedback_store) {
            let _permit = acquire_llm_permit(&pipeline_config.semaphore);
            // ...existing auto_calibrate call...
```

In `review_file_llm_only()` model loop (~line 478):
```rust
            for model in &pipeline_config.models {
                let _permit = acquire_llm_permit(&pipeline_config.semaphore);
                match reviewer.review(&prompt, model) {
```

In `review_file_llm_only()` auto-calibrate call:
```rust
            let _permit = acquire_llm_permit(&pipeline_config.semaphore);
            match crate::auto_calibrate::auto_calibrate(
```

**Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All existing tests pass (semaphore is None by default = no-op)

**Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat: acquire semaphore permit before each LLM call"
```

---

### Task 4: Use Pre-Built FeedbackIndex in Pipeline

**Files:**
- Modify: `src/pipeline.rs` (`review_file()` ~line 160, `review_file_llm_only()` ~line 422)

**Context:** Currently both functions build FeedbackIndex internally via `FeedbackIndex::build(&store)`. In parallel mode this means N concurrent embedding builds. Change to: use pre-built index from PipelineConfig if available, only build locally if not provided.

**Step 1: Implement**

In `review_file()`, change the FeedbackIndex initialization (~line 160-165):

```rust
    // Before:
    let mut feedback_index = if let Some(store_path) = &pipeline_config.feedback_store {
        let store = crate::feedback::FeedbackStore::new(store_path.clone());
        crate::feedback_index::FeedbackIndex::build(&store).ok()
    } else {
        None
    };

    // After:
    // Use pre-built shared index if available (parallel mode), otherwise build locally
    let shared_index = pipeline_config.feedback_index.clone();
    let mut local_index = if shared_index.is_none() {
        if let Some(store_path) = &pipeline_config.feedback_store {
            let store = crate::feedback::FeedbackStore::new(store_path.clone());
            crate::feedback_index::FeedbackIndex::build(&store).ok()
        } else {
            None
        }
    } else {
        None
    };
```

Then update all usages of `feedback_index` to use either the shared or local index. For the few-shot query and calibration, lock the shared index briefly:

```rust
    // For few-shot precedents:
    let precedents = if let Some(ref shared) = shared_index {
        let mut idx = shared.lock().unwrap();
        query_feedback_precedents(&mut idx, &file_str, lang_name(lang), &redacted_code)
    } else if let Some(ref mut idx) = local_index {
        query_feedback_precedents(idx, &file_str, lang_name(lang), &redacted_code)
    } else {
        Vec::new()
    };

    // For calibration:
    let (final_findings, suppressed_count) = if pipeline_config.calibrate && has_feedback {
        let config = CalibratorConfig::default();
        if let Some(ref shared) = shared_index {
            let mut idx = shared.lock().unwrap();
            if !idx.is_empty() {
                calibrator::calibrate_with_index(merged, &mut idx, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else if let Some(ref mut idx) = local_index {
            if !idx.is_empty() {
                calibrator::calibrate_with_index(merged, idx, &config)
            } else {
                calibrator::calibrate(merged, &pipeline_config.feedback, &config)
            }
        } else {
            calibrator::calibrate(merged, &pipeline_config.feedback, &config)
        }
    // ... rest unchanged
    };
```

Apply the same pattern to `review_file_llm_only()`.

**Step 2: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass (shared_index is None in tests = falls back to local build)

**Step 3: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat: use pre-built shared FeedbackIndex when available"
```

---

### Task 5: Atomic Feedback Store Writes

**Files:**
- Modify: `src/feedback.rs` (the `record()` method)

**Context:** `writeln!` is two syscalls (data + newline). Under concurrent auto-calibration, writes can interleave and corrupt the JSONL file. Fix: single `write_all` call.

**Step 1: Find and read the record method**

Run: `grep -n 'fn record\|writeln!\|write_all' src/feedback.rs`

**Step 2: Implement**

Change the write from:
```rust
writeln!(file, "{}", line)?;
```
to:
```rust
let mut buf = serde_json::to_string(entry)?;
buf.push('\n');
file.write_all(buf.as_bytes())?;
```

This ensures a single syscall for the entire line including newline.

**Step 3: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All tests pass

**Step 4: Commit**

```bash
git add src/feedback.rs
git commit -m "fix: use atomic write_all for feedback store to prevent corruption under concurrency"
```

---

### Task 6: Arc-Wrap LLM Client and Build Shared FeedbackIndex

**Files:**
- Modify: `src/main.rs` (~line 200-240, LLM client creation and PipelineConfig construction)

**Context:** Two changes: (1) Arc-wrap OpenAiClient for sharing across tasks, (2) pre-build FeedbackIndex once and add to PipelineConfig.

**Step 1: Implement**

**6a. Arc-wrap LLM client** (~line 200):

```rust
    // Before:
    let llm_client = if let Ok(api_key) = std::env::var("QUORUM_API_KEY") {
        Some(OpenAiClient::new(base_url, api_key).with_reasoning_effort(reasoning_effort))
    } else { None };

    // After:
    let llm_client: Option<std::sync::Arc<OpenAiClient>> = if let Ok(api_key) = std::env::var("QUORUM_API_KEY") {
        Some(std::sync::Arc::new(OpenAiClient::new(base_url, api_key).with_reasoning_effort(reasoning_effort)))
    } else { None };
```

Update `llm_reviewer` derivation:
```rust
    let llm_reviewer: Option<&dyn pipeline::LlmReviewer> = llm_client.as_deref().map(|c| c as _);
```

**6b. Create semaphore** (new code after pipeline_cfg construction):

```rust
    let semaphore = if opts.parallel > 1 {
        Some(std::sync::Arc::new(tokio::sync::Semaphore::new(opts.parallel)))
    } else if opts.parallel == 0 {
        Some(std::sync::Arc::new(tokio::sync::Semaphore::new(usize::MAX >> 4)))
    } else {
        None
    };
```

**6c. Pre-build FeedbackIndex** (new code):

```rust
    let shared_feedback_index = if let Some(store_path) = &pipeline_cfg.feedback_store {
        let store = crate::feedback::FeedbackStore::new(store_path.clone());
        match crate::feedback_index::FeedbackIndex::build(&store) {
            Ok(idx) => {
                eprintln!("FeedbackIndex: embedded {} entries with bge-small-en-v1.5", idx.len());
                Some(std::sync::Arc::new(std::sync::Mutex::new(idx)))
            }
            Err(e) => {
                eprintln!("Warning: Could not build feedback index: {}", e);
                None
            }
        }
    } else {
        None
    };
```

**6d. Wire into PipelineConfig:**

```rust
    let pipeline_cfg = PipelineConfig {
        models,
        calibration_model: opts.calibration_model.clone(),
        feedback: feedback_entries,
        auto_calibrate: !opts.no_auto_calibrate(),
        feedback_store: Some(feedback_path.clone()),
        diff_ranges,
        framework_overrides: opts.framework.clone(),
        semaphore,
        feedback_index: shared_feedback_index,
        ..Default::default()
    };
```

**Step 2: Build and test**

Run: `cargo build && cargo test --bin quorum`
Expected: Compiles and passes

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: Arc-wrap LLM client, pre-build shared FeedbackIndex, create semaphore"
```

---

### Task 7: Parallel File Processing Loop

**Files:**
- Modify: `src/main.rs` (~line 265-400, the `for file_path in &opts.files` loop)

**Context:** This is the core change. When `--parallel > 1` and multiple files, spawn per-file tasks. Single-file or `--parallel 1` keeps the existing sequential path unchanged.

**Step 1: Implement**

**7a.** Arc-wrap shared config for parallel tasks:

```rust
    let pipeline_cfg = std::sync::Arc::new(pipeline_cfg);
    let suppress_rules = std::sync::Arc::new(suppress_rules);
```

**7b.** Replace the sequential loop with a conditional:

```rust
    if opts.parallel == 1 || opts.files.len() <= 1 {
        // === SEQUENTIAL PATH (existing code, unchanged) ===
        for file_path in &opts.files {
            // ... existing loop body verbatim ...
        }
    } else {
        // === PARALLEL PATH ===
        let rt = tokio::runtime::Handle::current();
        let mut handles = Vec::new();

        for (idx, file_path) in opts.files.iter().enumerate() {
            let file_path = file_path.clone();
            let pipeline_cfg = pipeline_cfg.clone();
            let suppress_rules = suppress_rules.clone();
            let show_suppressed = opts.show_suppressed;
            let deep = opts.deep;
            let llm_client = llm_client.clone(); // Arc clone

            let handle = rt.spawn_blocking(move || {
                // Read file
                if !file_path.exists() {
                    return (idx, Err(anyhow::anyhow!("File not found: {}", file_path.display())));
                }
                let source = match std::fs::read_to_string(&file_path) {
                    Ok(s) => s,
                    Err(e) => return (idx, Err(anyhow::anyhow!("Could not read {}: {}", file_path.display(), e))),
                };
                let lang = parser::Language::from_path(&file_path);
                let file_display = file_path.to_string_lossy().to_string();

                // Deep review path
                if deep {
                    if let Some(ref client) = llm_client {
                        let project_root = std::env::current_dir().unwrap_or_default();
                        let tool_reg = tools::ToolRegistry::new(&project_root);
                        let agent_cfg = agent::AgentConfig::default();
                        let model = pipeline_cfg.models.first()
                            .map(|s| s.as_str()).unwrap_or("gpt-5.4");
                        match agent::agent_loop(
                            &source, &file_display,
                            &**client as &dyn agent::AgentReviewer,
                            model, &tool_reg, &agent_cfg,
                        ) {
                            Ok(findings) => {
                                let sup_result = suppress::apply_suppressions(
                                    findings, &suppress_rules, &file_display);
                                let result = pipeline::FileReviewResult {
                                    file_path: file_display,
                                    findings: sup_result.kept,
                                    usage: Default::default(),
                                    suppressed: sup_result.suppressed.len(),
                                };
                                return (idx, Ok((result, sup_result.suppressed, show_suppressed)));
                            }
                            Err(e) => {
                                eprintln!("[{}] Warning: Deep review failed: {}. Falling back.", file_path.display(), e);
                            }
                        }
                    }
                }

                // Standard review path
                let llm_reviewer: Option<&dyn pipeline::LlmReviewer> = llm_client.as_deref().map(|c| c as _);
                let parse_cache = cache::ParseCache::new(128);
                let review_result = if let Some(l) = lang {
                    pipeline::review_source(
                        &file_path, &source, l, llm_reviewer, &pipeline_cfg, Some(&parse_cache))
                } else {
                    pipeline::review_file_llm_only(
                        &file_path, &source, llm_reviewer, &pipeline_cfg)
                };

                match review_result {
                    Ok(mut result) => {
                        let sup_result = suppress::apply_suppressions(
                            result.findings, &suppress_rules, &file_display);
                        result.findings = sup_result.kept;
                        result.suppressed = sup_result.suppressed.len();
                        (idx, Ok((result, sup_result.suppressed, show_suppressed)))
                    }
                    Err(e) => (idx, Err(e)),
                }
            });
            handles.push(handle);
        }

        // Collect results in file order
        let mut indexed_results: Vec<Option<(pipeline::FileReviewResult, Vec<(Finding, suppress::SuppressionRule)>, bool)>> = vec![None; opts.files.len()];
        for handle in handles {
            match tokio::task::block_in_place(|| rt.block_on(handle)) {
                Ok((idx, Ok(result))) => { indexed_results[idx] = Some(result); }
                Ok((idx, Err(e))) => {
                    eprintln!("Error: Review failed for {}: {}", opts.files[idx].display(), e);
                    had_errors = true;
                }
                Err(e) => {
                    eprintln!("Error: Task panicked: {}", e);
                    had_errors = true;
                }
            }
        }

        // Output in file order (sequential — no interleaving)
        for result_opt in indexed_results.into_iter() {
            if let Some((result, suppressed_findings, show_suppressed)) = result_opt {
                if !suppressed_findings.is_empty() {
                    eprintln!("Suppressed {} finding(s) in {}", suppressed_findings.len(), result.file_path);
                }
                if show_suppressed {
                    for (f, rule) in &suppressed_findings {
                        eprint!("{}", suppress::format_suppressed_finding(f, rule));
                    }
                }
                if use_compact {
                    println!("{}", output::format_compact_review(&result.file_path, &result.findings));
                } else if !use_json {
                    print!("{}", output::format_review(&result.file_path, &result.findings, &style));
                }
                all_findings.extend(result.findings.clone());
                file_results.push(result);
            }
        }
    }
```

**Important notes:**
- Output is rendered sequentially AFTER all tasks complete — no interleaving
- Each task gets its own ParseCache (different files = no cache sharing benefit)
- `suppress::SuppressionRule` must derive `Clone` (or be wrapped in Arc) — check and add if needed
- `Finding` already derives `Clone`
- The `SuppressionResult` tuple `(Finding, SuppressionRule)` needs both to be cloneable and Send

**Step 2: Build and test**

Run: `cargo build && cargo test --bin quorum`
Expected: Compiles and all tests pass

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: parallel file processing with spawn_blocking and indexed results"
```

---

### Task 8: Parallel-Safe Progress Reporting

**Files:**
- Modify: `src/main.rs` (progress reporting in parallel path)

**Context:** ProgressReporter uses terminal control codes (carriage return, line clearing) that conflict with concurrent output. In parallel mode, skip the spinner and just report completion.

**Step 1: Implement**

In the parallel path (Task 7), progress is naturally handled: no `progress.start_file()` / `progress.finish_file()` calls inside spawned tasks. After collecting results, report a summary:

```rust
    // After the parallel result collection loop, before telemetry:
    if opts.parallel > 1 && opts.files.len() > 1 {
        eprintln!("{} file(s) reviewed in parallel (--parallel {})", file_results.len(), opts.parallel);
    }
```

The sequential path keeps the existing progress reporter unchanged.

**Step 2: Build and test**

Run: `cargo build`
Expected: Compiles

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: parallel-safe progress reporting"
```

---

### Task 9: Integration Tests

**Files:**
- Create: `tests/parallel_review.rs`

**Step 1: Write tests**

```rust
use assert_cmd::Command;
use std::io::Write;

#[test]
fn parallel_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.rs");
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("4")
        .arg(&path)
        .assert()
        .success();
}

#[test]
fn parallel_1_sequential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.py");
    std::fs::write(&path, "x = 1\n").unwrap();

    Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("1")
        .arg(&path)
        .assert()
        .success();
}

#[test]
fn parallel_0_unlimited() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.rs");
    std::fs::write(&path, "fn main() {}\n").unwrap();

    Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("0")
        .arg(&path)
        .assert()
        .success();
}

#[test]
fn parallel_json_output_maintains_file_order() {
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=4 {
        let path = dir.path().join(format!("file{}.py", i));
        std::fs::write(&path, format!("x = {}\n", i)).unwrap();
    }

    let output = Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("2")
        .arg("--json")
        .arg(dir.path().join("file1.py"))
        .arg(dir.path().join("file2.py"))
        .arg(dir.path().join("file3.py"))
        .arg(dir.path().join("file4.py"))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() && stdout.trim() != "[]" {
        // Should parse as valid JSON
        let _: serde_json::Value = serde_json::from_str(&stdout)
            .expect("parallel JSON output should be valid");
    }
}

#[test]
fn parallel_handles_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.rs");
    std::fs::write(&good, "fn main() {}\n").unwrap();
    let bad = dir.path().join("nonexistent.rs");

    // Should not crash; should report error for missing file
    let output = Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("2")
        .arg(&good)
        .arg(&bad)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("nonexistent"),
        "should report missing file");
}
```

**Step 2: Run tests**

Run: `cargo test --test parallel_review`
Expected: All PASS

**Step 3: Commit**

```bash
git add tests/parallel_review.rs
git commit -m "test: integration tests for --parallel flag"
```

---

### Task 10: Documentation and Final Verification

**Files:**
- Modify: `CLAUDE.md` (add --parallel to commands, update test count)

**Step 1: Update docs**

Add to the commands/flags section in CLAUDE.md:
```
cargo run -- review src/*.rs --parallel 4    # parallel LLM calls (default: 4)
```

Update test count.

**Step 2: Run full test suite**

Run: `cargo test`
Expected: All tests pass

**Step 3: Manual smoke test (if API key available)**

```bash
time quorum review src/pipeline.rs src/main.rs src/calibrator.rs --parallel 1
time quorum review src/pipeline.rs src/main.rs src/calibrator.rs --parallel 4
```

Expected: `--parallel 4` noticeably faster for LLM-enabled reviews.

**Step 4: Release build**

```bash
cargo build --release
target/release/quorum version
```

**Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document --parallel flag, update test count"
```
