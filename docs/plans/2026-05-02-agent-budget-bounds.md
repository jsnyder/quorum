# Agent Budget Bounds Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix two agent.rs bugs — #180 (code under review inserted into prompt without size bound) and #181 (tool byte budget enforced only after full output allocation) — so that both the prompt and tool outputs respect configurable byte limits proactively, not reactively.

**Architecture:** Two independent fixes in the same PR. #180 adds `max_code_bytes` to `AgentConfig` (default 100KB) and a new `wrap_code_with_budget()` that mirrors the existing `wrap_listing_with_budget()` pattern — truncate with in-prompt marker + stderr warning. #181 adds `max_output_bytes: usize` to `ToolRegistry::execute()`, pushing the byte cap into each tool (`read_file` via `Read::take()`, `grep`/`list_files` via byte-count accumulator) so the full output is never allocated. The post-hoc truncation in `execute_tool_call()` becomes a safety net, not the primary mechanism.

**Tech Stack:** Rust 2024, tree-sitter 0.26 (unchanged), no new dependencies.

---

## Task 1: Add `max_code_bytes` to `AgentConfig` (#180 foundation)

**Files:**
- Modify: `src/agent.rs:44-57` (AgentConfig struct + Default impl)

**Step 1: Write the failing test**

Add to the `#[cfg(test)]` module in `src/agent.rs`:

```rust
#[test]
fn agent_config_has_max_code_bytes_default() {
    let config = AgentConfig::default();
    assert_eq!(config.max_code_bytes, 100_000);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum agent_config_has_max_code_bytes_default`
Expected: FAIL — `no field named max_code_bytes`

**Step 3: Write minimal implementation**

Add field to `AgentConfig`:

```rust
pub struct AgentConfig {
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub max_bytes_read: usize,
    pub max_code_bytes: usize,
}
```

Update `Default`:

```rust
impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            max_tool_calls: 10,
            max_bytes_read: 50_000,
            max_code_bytes: 100_000,
        }
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum agent_config_has_max_code_bytes_default`
Expected: PASS

**Step 5: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): add max_code_bytes field to AgentConfig (#180)"
```

---

## Task 2: Implement `wrap_code_with_budget()` (#180 core fix)

**Files:**
- Modify: `src/agent.rs:173-176` (replace `wrap_code` with budget-aware version)

**Step 1: Write the failing test**

```rust
#[test]
fn wrap_code_with_budget_truncates_oversized_input() {
    let big_code = "x".repeat(1000);
    let budget = 200;
    let result = wrap_code_with_budget(&big_code, budget);
    assert!(
        result.len() <= budget,
        "wrapped code {} exceeds budget {}",
        result.len(),
        budget
    );
    assert!(result.contains(CODE_OPEN_TAG));
    assert!(result.contains(CODE_CLOSE_TAG));
    assert!(result.contains("truncated"));
}

#[test]
fn wrap_code_with_budget_passes_small_input_unchanged() {
    let code = "fn main() {}";
    let budget = 10_000;
    let result = wrap_code_with_budget(code, budget);
    assert!(result.contains("fn main() {}"));
    assert!(result.contains(CODE_OPEN_TAG));
    assert!(result.contains(CODE_CLOSE_TAG));
    assert!(!result.contains("truncated"));
}

#[test]
fn wrap_code_with_budget_handles_budget_smaller_than_tags() {
    let code = "fn main() {}";
    let budget = 5; // smaller than open+close tags
    let result = wrap_code_with_budget(code, budget);
    assert!(result.is_empty(), "should return empty when budget can't fit tags");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum wrap_code_with_budget`
Expected: FAIL — `cannot find function wrap_code_with_budget`

**Step 3: Write minimal implementation**

Replace `wrap_code` with `wrap_code_with_budget`:

```rust
const CODE_TRUNC_NOTE: &str = "\n... (truncated: code size limit reached)";

fn wrap_code_with_budget(code: &str, budget: usize) -> String {
    let wrapper_overhead = CODE_OPEN_TAG.len() + CODE_CLOSE_TAG.len();
    if budget < wrapper_overhead {
        return String::new();
    }
    let body_budget = budget - wrapper_overhead;
    let escaped = escape_for_xml_wrap(code);
    let body = if escaped.len() > body_budget {
        eprintln!(
            "Agent: code under review truncated ({} bytes exceeds {} byte limit)",
            escaped.len(),
            body_budget
        );
        if body_budget < CODE_TRUNC_NOTE.len() {
            let safe_end = escaped.floor_char_boundary(body_budget);
            escaped[..safe_end].to_string()
        } else {
            let trunc_room = body_budget - CODE_TRUNC_NOTE.len();
            let safe_end = escaped.floor_char_boundary(trunc_room);
            format!("{}{}", &escaped[..safe_end], CODE_TRUNC_NOTE)
        }
    } else {
        escaped
    };
    format!("{}{}{}", CODE_OPEN_TAG, body, CODE_CLOSE_TAG)
}
```

**Step 4: Update call sites**

In `render_review_prompt` (line 117), change:
```rust
let code_block = wrap_code(code);
```
to:
```rust
let code_block = wrap_code_with_budget(code, config.max_code_bytes);
```

In `agent_loop` (line 312), change:
```rust
wrap_code(code)
```
to:
```rust
wrap_code_with_budget(code, config.max_code_bytes)
```

Remove the old `wrap_code` function (lines 173-176) once no callers remain.

Update `render_review_prompt_for_test` and `render_review_prompt_with_budget_for_test` helper functions in tests to pass `config` (they already have it).

**Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum wrap_code_with_budget && cargo test --bin quorum agent`
Expected: ALL PASS

**Step 6: Commit**

```bash
git add src/agent.rs
git commit -m "fix(agent): bound code under review to max_code_bytes (#180)"
```

---

## Task 3: Add `max_output_bytes` parameter to `ToolRegistry::execute()` (#181 interface change)

**Files:**
- Modify: `src/tools.rs:68-75` (execute signature)
- Modify: `src/agent.rs:198` (call site)

**Step 1: Write the failing test**

Add to `src/tools.rs` test module:

```rust
#[test]
fn execute_read_file_respects_max_output_bytes() {
    let dir = setup_repo();
    // setup_repo creates main.py; write a bigger file
    std::fs::write(dir.path().join("big.txt"), "x".repeat(10_000)).unwrap();
    let reg = ToolRegistry::new(dir.path());
    let result = reg
        .execute("read_file", &serde_json::json!({"path": "big.txt"}), 200)
        .unwrap();
    assert!(
        result.len() <= 200,
        "read_file output {} exceeded max_output_bytes 200",
        result.len()
    );
}

#[test]
fn execute_grep_respects_max_output_bytes() {
    let dir = setup_repo();
    // Write many matching lines
    let content: String = (0..500).map(|i| format!("match line {}\n", i)).collect();
    std::fs::write(dir.path().join("many.txt"), &content).unwrap();
    let reg = ToolRegistry::new(dir.path());
    let result = reg
        .execute("grep", &serde_json::json!({"pattern": "match"}), 300)
        .unwrap();
    assert!(
        result.len() <= 300,
        "grep output {} exceeded max_output_bytes 300",
        result.len()
    );
}

#[test]
fn execute_with_large_budget_returns_full_output() {
    let dir = setup_repo();
    let reg = ToolRegistry::new(dir.path());
    let result = reg
        .execute("read_file", &serde_json::json!({"path": "main.py"}), usize::MAX)
        .unwrap();
    assert!(result.contains("print"), "full output should include file content");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum execute_read_file_respects_max_output_bytes`
Expected: FAIL — wrong number of arguments to `execute()`

**Step 3: Write minimal implementation**

Update `execute` signature:

```rust
pub fn execute(
    &self,
    tool_name: &str,
    args: &serde_json::Value,
    max_output_bytes: usize,
) -> anyhow::Result<String> {
    let result = match tool_name {
        "read_file" => self.exec_read_file(args)?,
        "grep" => self.exec_grep(args)?,
        "list_files" => self.exec_list_files(args)?,
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    };
    Ok(truncate(&result, max_output_bytes))
}
```

This is a two-phase approach: first add the parameter and use `truncate()` at the `execute()` boundary (prevents oversized returns), then push the cap deeper in Task 4.

Update the call site in `agent.rs` `execute_tool_call`:

```rust
// line 198: pass remaining budget into execute
let remaining = config.max_bytes_read.saturating_sub(self.total_bytes_read);
match tools.execute(&tc.name, &args, remaining) {
```

Update all existing test call sites in `tools.rs` to pass `usize::MAX` (or `MAX_OUTPUT_CHARS`) as the third argument so they compile unchanged.

**Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/tools.rs src/agent.rs
git commit -m "fix(agent): pass byte budget into ToolRegistry::execute (#181)"
```

---

## Task 4: Push byte cap into individual tools (#181 deep fix)

**Files:**
- Modify: `src/tools.rs:93-113` (exec_read_file)
- Modify: `src/tools.rs:115-128` (exec_grep)
- Modify: `src/tools.rs:184-197` (exec_list_files)
- Modify: `src/tools.rs:130-182` (grep_recursive — add byte accumulator)
- Modify: `src/tools.rs:199-242` (list_recursive — add byte accumulator)

**Step 1: Write the failing tests**

```rust
#[test]
fn read_file_does_not_allocate_beyond_budget() {
    let dir = setup_repo();
    // 1MB file — must not fully allocate if budget is 500 bytes
    std::fs::write(dir.path().join("huge.txt"), "y".repeat(1_000_000)).unwrap();
    let reg = ToolRegistry::new(dir.path());
    let result = reg
        .execute("read_file", &serde_json::json!({"path": "huge.txt"}), 500)
        .unwrap();
    assert!(result.len() <= 500);
}

#[test]
fn grep_stops_accumulating_at_byte_budget() {
    let dir = setup_repo();
    let content: String = (0..10_000).map(|i| format!("pattern {}\n", i)).collect();
    std::fs::write(dir.path().join("huge.txt"), &content).unwrap();
    let reg = ToolRegistry::new(dir.path());
    let result = reg
        .execute("grep", &serde_json::json!({"pattern": "pattern"}), 500)
        .unwrap();
    assert!(result.len() <= 500);
}

#[test]
fn list_files_stops_accumulating_at_byte_budget() {
    let dir = setup_repo();
    // Create many files
    for i in 0..200 {
        std::fs::write(dir.path().join(format!("file_{:04}.txt", i)), "x").unwrap();
    }
    let reg = ToolRegistry::new(dir.path());
    let result = reg
        .execute("list_files", &serde_json::json!({}), 300)
        .unwrap();
    assert!(result.len() <= 300);
}
```

**Step 2: Run tests to verify they fail**

The `truncate()` at the `execute()` boundary from Task 3 already caps the output, so these tests should pass. But the point of this task is to push the cap *deeper* so the full string is never allocated. Since we can't easily test memory allocation in unit tests, the tests serve as contract tests — the behavioral guarantee is the same, but the implementation avoids the spike.

If tests already pass from Task 3's boundary truncation, proceed to Step 3 anyway to push the cap deeper.

**Step 3: Push budget into each tool**

Change `execute` to pass `max_output_bytes` through:

```rust
pub fn execute(
    &self,
    tool_name: &str,
    args: &serde_json::Value,
    max_output_bytes: usize,
) -> anyhow::Result<String> {
    match tool_name {
        "read_file" => self.exec_read_file(args, max_output_bytes),
        "grep" => self.exec_grep(args, max_output_bytes),
        "list_files" => self.exec_list_files(args, max_output_bytes),
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    }
}
```

**exec_read_file** — use `Read::take()` to cap bytes read from disk:

```rust
fn exec_read_file(
    &self,
    args: &serde_json::Value,
    max_output_bytes: usize,
) -> anyhow::Result<String> {
    let path_str = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?;
    let resolved = self.resolve_path(path_str)?;

    let file = std::fs::File::open(&resolved)?;
    let mut limited = String::new();
    use std::io::Read;
    file.take(max_output_bytes as u64 + 1)
        .read_to_string(&mut limited)?;
    let was_truncated = limited.len() > max_output_bytes;

    let start = args["start_line"].as_u64().map(|n| n as usize).unwrap_or(1);
    let end = args["end_line"].as_u64().map(|n| n as usize);

    let lines: Vec<&str> = limited.lines().collect();
    let start_idx = start.saturating_sub(1).min(lines.len());
    let end_idx = end.unwrap_or(lines.len()).min(lines.len()).max(start_idx);

    let selected: String = lines[start_idx..end_idx]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4} | {}", start_idx + i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(truncate(&selected, max_output_bytes))
}
```

**exec_grep** — pass budget through, stop accumulating when byte count reached:

```rust
fn exec_grep(
    &self,
    args: &serde_json::Value,
    max_output_bytes: usize,
) -> anyhow::Result<String> {
    let pattern = args["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern required"))?;
    let max = args["max_results"].as_u64().unwrap_or(MAX_GREP_RESULTS as u64) as usize;
    let path_glob = args["path_glob"].as_str();

    let mut results = Vec::new();
    let mut total_bytes = 0usize;
    self.grep_recursive(&self.root, pattern, path_glob, &mut results, max, &mut total_bytes, max_output_bytes)?;

    if results.is_empty() {
        Ok("No matches found.".into())
    } else {
        Ok(truncate(&results.join("\n"), max_output_bytes))
    }
}
```

Update `grep_recursive` to accept `total_bytes: &mut usize, byte_budget: usize` and check `*total_bytes >= byte_budget` alongside the existing `results.len() >= max` guard. Each time a result is pushed, add its `.len()` to `*total_bytes`.

Apply the same pattern to `exec_list_files` and `list_recursive`.

**Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/tools.rs
git commit -m "fix(tools): push byte budget into read_file/grep/list_files (#181)"
```

---

## Task 5: Simplify post-hoc truncation in `execute_tool_call` (cleanup)

**Files:**
- Modify: `src/agent.rs:184-250` (execute_tool_call)

Now that `tools.execute()` returns already-budgeted output, the post-hoc truncation in `execute_tool_call` is a safety net. Simplify it.

**Step 1: Write a test confirming the safety net still works**

```rust
#[test]
fn execute_tool_call_safety_net_truncates_even_if_tool_overshoots() {
    // Simulate a hypothetical tool that ignores max_output_bytes.
    // The execute_tool_call layer must still cap total_bytes_read.
    let config = AgentConfig { max_bytes_read: 100, ..AgentConfig::default() };
    let dir = tempfile::tempdir().unwrap();
    // Write a file small enough to pass the tool-level cap but verify
    // agent-level accounting still works across multiple calls.
    std::fs::write(dir.path().join("a.txt"), "a".repeat(60)).unwrap();
    std::fs::write(dir.path().join("b.txt"), "b".repeat(60)).unwrap();
    let tools = crate::tools::ToolRegistry::new(dir.path());
    let mut state = AgentState { total_bytes_read: 0, total_tool_calls: 0 };
    let tc1 = crate::llm_client::ToolCall {
        id: "1".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"a.txt"}"#.into(),
    };
    let tc2 = crate::llm_client::ToolCall {
        id: "2".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"b.txt"}"#.into(),
    };
    let _ = state.execute_tool_call(&tc1, &tools, &config);
    let _ = state.execute_tool_call(&tc2, &tools, &config);
    assert!(
        state.total_bytes_read <= config.max_bytes_read,
        "total_bytes_read {} exceeds max {}",
        state.total_bytes_read,
        config.max_bytes_read
    );
}
```

**Step 2: Run test — should pass** (existing logic already handles this)

**Step 3: Simplify `execute_tool_call`**

Keep the post-hoc truncation as a safety net but pass the remaining budget into `tools.execute()`:

```rust
fn execute_tool_call(
    &mut self,
    tc: &crate::llm_client::ToolCall,
    tools: &ToolRegistry,
    config: &AgentConfig,
) -> Option<String> {
    self.total_tool_calls += 1;
    if self.total_tool_calls > config.max_tool_calls {
        eprintln!("Agent: tool call limit ({}) reached", config.max_tool_calls);
        return None;
    }

    let remaining = config.max_bytes_read.saturating_sub(self.total_bytes_read);

    let result = match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
        Ok(args) => {
            match tools.execute(&tc.name, &args, remaining) {
                Ok(output) => {
                    // Safety net: tools should already respect the budget,
                    // but truncate here in case of contract violation.
                    let truncated_path = output.len() > remaining;
                    let output = if truncated_path {
                        let truncated = if remaining < TRUNCATION_MARKER.len() {
                            let safe_end = output.floor_char_boundary(remaining);
                            output[..safe_end].to_string()
                        } else {
                            let body_budget = remaining - TRUNCATION_MARKER.len();
                            let safe_end = output.floor_char_boundary(body_budget);
                            let mut t = output[..safe_end].to_string();
                            t.push_str(TRUNCATION_MARKER);
                            t
                        };
                        eprintln!("Agent: byte limit ({}) reached", config.max_bytes_read);
                        truncated
                    } else {
                        output
                    };
                    if truncated_path {
                        self.total_bytes_read = config.max_bytes_read;
                    } else {
                        self.total_bytes_read += output.len();
                    }
                    output
                }
                Err(e) => format!("Error: {}", e),
            }
        }
        Err(e) => format!("Error: malformed arguments: {}", e),
    };
    Some(result)
}
```

**Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/agent.rs
git commit -m "refactor(agent): pass remaining budget to tools.execute, keep safety net (#181)"
```

---

## Task 6: Final verification

**Step 1:** `cargo test --bin quorum` — all tests pass
**Step 2:** `cargo clippy --bin quorum -- -W clippy::all` — no new warnings
**Step 3:** `cargo build --release` — compiles clean

---

## Summary of changes

| File | What changes |
|------|-------------|
| `src/agent.rs` | `AgentConfig.max_code_bytes` (100KB default), `wrap_code_with_budget()`, budget passed to `tools.execute()` |
| `src/tools.rs` | `execute()` gains `max_output_bytes` param, each tool caps output proactively via `Read::take()` / byte accumulators |

**No new files.** ~60 lines added (mostly tests), ~10 lines removed (old `wrap_code`). Net positive line count is from tests only.
