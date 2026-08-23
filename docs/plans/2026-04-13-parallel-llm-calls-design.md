# Parallel LLM Calls Design

## Goal

Parallelize LLM calls across files to reduce wall-clock time for multi-file reviews.
Default to 4 concurrent LLM calls, configurable via `--parallel N`.

## Architecture

### Global Semaphore at LLM Call Boundary

A single `Arc<Semaphore>` controls all outbound LLM calls — standard reviews,
ensemble models, auto-calibration, and deep review agent turns. This prevents
fan-out regardless of nesting (e.g., 5 files x 3 ensemble models = 15 potential
calls, but only N execute concurrently).

```
                  ┌──────────────────────┐
                  │   Arc<Semaphore(N)>   │
                  └──────────┬───────────┘
                             │
         ┌───────────────────┼───────────────────┐
         │                   │                   │
    ┌────▼────┐         ┌────▼────┐         ┌────▼────┐
    │ File 1  │         │ File 2  │         │ File 3  │
    │ (task)  │         │ (task)  │         │ (task)  │
    └────┬────┘         └────┬────┘         └────┬────┘
         │                   │                   │
    acquire(sem)        acquire(sem)        acquire(sem)
    reviewer.review()   reviewer.review()   reviewer.review()
    release(sem)        release(sem)        release(sem)
         │                   │                   │
    acquire(sem)        acquire(sem)             │
    auto_calibrate()    auto_calibrate()         │
    release(sem)        release(sem)             │
```

### Shared State Model

**Arc-wrapped read-only (shared across tasks):**
- `Arc<dyn LlmReviewer>` — HTTP client (reqwest is clone-cheap)
- `Arc<FeedbackStore>` — loaded once, read-only during review
- `Arc<FeedbackIndex>` — embeddings index, read-only
- `Arc<PipelineConfig>` — configuration
- `Arc<Semaphore>` — concurrency limiter

**Per-task owned (no sharing):**
- `Vec<Finding>` — review results per file
- `Vec<Finding>` — suppressed findings per file
- `TelemetryEntry` — per-file token/timing data
- Auto-calibration verdicts per file

**Post-join merge:**
- Results slotted into pre-sized `Vec<Option<FileReviewResult>>` by file index
- Telemetry entries appended in file order
- Exit code = max severity across all results

### Semaphore Placement

The semaphore is acquired at the actual outbound LLM call, not at task spawn:

```rust
// In pipeline.rs review_file(), around each LLM call:
if let Some(ref sem) = pipeline_config.semaphore {
    let _permit = sem.acquire().await.unwrap();
    // reviewer.review() happens here
}
```

Since `reviewer.review()` is sync (wraps async internally via `block_in_place`),
the semaphore acquire needs to happen in an async context before entering the
blocking call. Pattern:

```rust
// Spawn each file as a blocking task
tokio::task::spawn_blocking(move || {
    // Inside spawn_blocking, use block_in_place for semaphore
    tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(semaphore.acquire())
    });
    reviewer.review(&prompt, model)  // blocking call
    // permit dropped here (RAII)
})
```

### File-Level Parallelism

```rust
// main.rs: parallel file processing
let mut handles = Vec::new();
for (idx, file_path) in opts.files.iter().enumerate() {
    let sem = semaphore.clone();
    let reviewer = reviewer.clone();
    let config = config.clone();
    let store = store.clone();
    let index = feedback_index.clone();

    let handle = tokio::task::spawn_blocking(move || {
        // Each file: local AST + ast-grep + LLM (with semaphore) + calibrate
        let result = review_file(&file_path, &config, &reviewer, &store, &index, &sem);
        (idx, result)
    });
    handles.push(handle);
}

// Collect results in original file order
let mut results = vec![None; opts.files.len()];
for handle in handles {
    let (idx, result) = handle.await?;
    results[idx] = Some(result);
}
```

### Deep Review Parallelism

Deep review is parallelized at the file level. Each file's agent loop runs
independently — multiple agent loops can be mid-conversation simultaneously.
The global semaphore ensures only N LLM calls execute at once across all loops.

```
File 1 agent loop:  [LLM] → [read_file] → [LLM] → [grep] → [LLM]
File 2 agent loop:  [LLM] → [grep] → [LLM] → [read_file] → [LLM]
                     ↑                  ↑
                     └── semaphore ──────┘  (at most N concurrent)
```

Tool calls (read_file, grep, list_files) are read-only filesystem ops and
don't need the semaphore — they can execute freely while other tasks wait
for LLM permits.

### CLI Interface

```
--parallel N    Max concurrent LLM calls (default: 4, 0 = unlimited, 1 = sequential)
```

- `--parallel 1` preserves current sequential behavior (good for debugging)
- `--parallel 0` creates no semaphore (unlimited, use with caution)
- Default 4 is conservative for LiteLLM proxy
- Deep review respects the same `--parallel` flag

### Error Handling

- Per-file errors are isolated — one file failing doesn't cancel others
- Failed files produce error results (same as today)
- Semaphore permits released via RAII drop on both success and failure
- No retries at this layer (LiteLLM handles retries upstream)

### What Doesn't Change

- `LlmReviewer` trait stays sync (no async cascade through 15+ call sites)
- `review_file()` internals stay the same, just gains semaphore acquire
- Daemon mode unaffected (has its own async server)
- MCP server unaffected
- Output format unchanged (results ordered by input file)

## Files to Modify

| File | Change |
|------|--------|
| `src/cli/mod.rs` | Add `--parallel N` to ReviewOpts |
| `src/pipeline.rs` | Add `semaphore: Option<Arc<Semaphore>>` to PipelineConfig; acquire before LLM calls |
| `src/main.rs` | Spawn per-file tasks, collect indexed results, merge telemetry |
| `src/agent.rs` | Thread semaphore through agent_review/agent_loop for deep review |
| `src/auto_calibrate.rs` | Acquire semaphore before calibration LLM call |

## Testing

- Unit test: semaphore limits concurrent calls (mock reviewer with delay)
- Unit test: results maintain file order regardless of completion order
- Unit test: `--parallel 1` produces identical results to current behavior
- Unit test: one file failure doesn't prevent other files from completing
- Integration test: `--parallel 4` with 8 files completes faster than sequential
