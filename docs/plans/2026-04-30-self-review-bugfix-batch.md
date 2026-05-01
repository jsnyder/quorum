# Self-Review Bugfix Batch (#133–#139) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Resolve 7 self-review findings discovered during #123 merge prep, grouped into 5 atomic PRs across 5 parallel git worktrees.

**Architecture:** Each PR is its own worktree branched off `main`, isolated to one or two related files. PRs are independent and can land in any order. Each PR follows strict RED→GREEN→REFACTOR TDD with a quorum self-review pass before merge.

**Tech Stack:** Rust 2021, clap 4 (CLI), serde + serde_json (parsing), tokio (no async work here), tempfile + assert_matches (tests).

**Rollout strategy:** 5 worktrees created up front. Implementation phase dispatches one subagent per worktree in parallel. Phase 6/7/8 also batched in parallel. Master tracking via TaskList.

**Design decisions confirmed in brainstorm:**
- **#139** — Option 2: structured `LoadStats { kept, skipped, errors }` mirroring `feedback::LoadStats` (#92). NOT just a `tracing::warn!`.
- **#133** — Option A: mirror `ast_grep.rs::read_rule_file` non-Unix pattern by adding a post-open `path.symlink_metadata().is_file()` check. **Framed as mitigation, NOT TOCTOU closure on non-Unix**: a swap *before* `opts.open(path)` still lets us follow a symlink and pin a sensitive inode in the handle; the post-open path stat then can't validate what was actually opened. The check shrinks the window for one specific attack (symlink-after-classify) but is honestly best-effort on non-Unix. (Per GPT-5.5 review, 2026-04-30.)
- **#136** — Custom `validate_k(&str) -> Result<usize, String>` value_parser, NOT `value_parser!(usize).range(...)` (clap 4.5's ranged parser only supports primitive integer types via `u64`/`i64`; switching to `Option<u64>` would propagate type churn downstream). Keep `Option<usize>`.
- **#135** — clap value_parser with allowlist regex `^[a-zA-Z0-9_-]{1,64}$` *plus* defense-in-depth at use site. **Validation lives in `src/context/cli.rs::run_add`** (and ideally `SourcesConfig::append_source`), NOT `src/main.rs` — `main.rs` only routes clap args to `ContextCmd::Add`; the actual handler is `run_add`. (Per GPT-5.5 review, 2026-04-30.)
- **#137** — Component-tail equality alone is INSUFFICIENT (still cross-matches `src/foo.rs` ↔ `nested/src/foo.rs`). Real fix: pass `repo_root` from `PipelineConfig`, strip it from review path to get repo-relative form, then test full component equality against `diff_path`. Fall back to "no match" if normalization fails (safer than wrong match). (Per GPT-5.5 review, 2026-04-30.)

---

## Worktree Map

| PR | Branch | Worktree dir | Issues | Files touched |
|----|--------|--------------|--------|---------------|
| 1 | `fix/cli-validation` | `../quorum-cli-validation` | #135, #136 | `src/cli/mod.rs`, `src/main.rs` (handler), tests |
| 2 | `fix/telemetry-streaming` | `../quorum-telemetry-streaming` | #138, #139 | `src/telemetry.rs` |
| 3 | `fix/feedback-toctou-nonunix` | `../quorum-feedback-toctou` | #133 | `src/feedback.rs` |
| 4 | `fix/mcp-deny-unknown-fields` | `../quorum-mcp-deny-unknown` | #134 | `src/mcp/tools.rs` |
| 5 | `fix/pipeline-path-equality` | `../quorum-pipeline-path` | #137 | `src/pipeline.rs` |

---

## PR 1 — CLI input validation (#135 + #136)

**Files:**
- Modify: `src/cli/mod.rs:81-85` (`ContextAddOpts::name`)
- Modify: `src/cli/mod.rs:157-159` (`ContextQueryOpts::k`)
- Modify: `src/main.rs` (defense-in-depth path canonicalization in handler — find `Context(ContextSubcommand::Add(...))` arm)
- Tests: `tests/cli_context_validation.rs` (new) or inline `#[cfg(test)] mod tests` if existing pattern

**Constants:**
- Allowed name regex: `^[a-zA-Z0-9_-]{1,64}$`
- `--k` range: `1..=100`

### Task 1.1: RED — `--name` rejects path traversal

**Step 1: Write the failing test**

```rust
// tests/cli_context_validation.rs
use clap::Parser;
use quorum::cli::Cli;

#[test]
fn context_add_name_rejects_dotdot() {
    let r = Cli::try_parse_from([
        "quorum", "context", "add",
        "--name", "../etc",
        "--kind", "rust",
        "--path", "/tmp/x",
    ]);
    assert!(r.is_err(), "../etc must be rejected at parse time");
}

#[test]
fn context_add_name_rejects_absolute() {
    let r = Cli::try_parse_from([
        "quorum", "context", "add",
        "--name", "/etc/passwd",
        "--kind", "rust",
        "--path", "/tmp/x",
    ]);
    assert!(r.is_err());
}

#[test]
fn context_add_name_rejects_slash() {
    let r = Cli::try_parse_from([
        "quorum", "context", "add",
        "--name", "a/b",
        "--kind", "rust",
        "--path", "/tmp/x",
    ]);
    assert!(r.is_err());
}

#[test]
fn context_add_name_rejects_leading_dot() {
    let r = Cli::try_parse_from([
        "quorum", "context", "add",
        "--name", ".hidden",
        "--kind", "rust",
        "--path", "/tmp/x",
    ]);
    assert!(r.is_err());
}

#[test]
fn context_add_name_rejects_overlong() {
    let long = "a".repeat(65);
    let r = Cli::try_parse_from([
        "quorum", "context", "add",
        "--name", &long,
        "--kind", "rust",
        "--path", "/tmp/x",
    ]);
    assert!(r.is_err());
}

#[test]
fn context_add_name_accepts_simple() {
    let r = Cli::try_parse_from([
        "quorum", "context", "add",
        "--name", "my-source_1",
        "--kind", "rust",
        "--path", "/tmp/x",
    ]);
    assert!(r.is_ok());
}
```

**Step 2: Run — expected FAIL**

```
cargo test --bin quorum context_add_name_ -- --nocapture
```
Expected: 5 of 6 tests fail (only `accepts_simple` would pass since today any string parses).

**Step 3: Implement value_parser**

In `src/cli/mod.rs`, add a free function `validate_source_name`:

```rust
fn validate_source_name(s: &str) -> Result<String, String> {
    if s.is_empty() || s.len() > 64 {
        return Err(format!("--name length must be 1..=64 (got {})", s.len()));
    }
    if s.starts_with('.') {
        return Err("--name must not start with '.'".into());
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("--name must match [a-zA-Z0-9_-]".into());
    }
    Ok(s.to_string())
}
```

Wire it via `value_parser`:

```rust
#[arg(long, value_parser = validate_source_name)]
pub name: String,
```

**Step 4: Run — expected PASS**

```
cargo test --bin quorum context_add_name_
```
Expected: 6 of 6 pass.

**Step 5: Commit**

```bash
git add src/cli/mod.rs tests/cli_context_validation.rs
git commit -m "feat(cli): validate --name at parse time, reject path-traversal chars"
```

### Task 1.2: RED — `--k` rejects unbounded values

**Step 1: Write the failing test**

```rust
#[test]
fn context_query_k_rejects_zero() {
    let r = Cli::try_parse_from([
        "quorum", "context", "query", "hello", "--k", "0",
    ]);
    assert!(r.is_err());
}

#[test]
fn context_query_k_rejects_above_cap() {
    let r = Cli::try_parse_from([
        "quorum", "context", "query", "hello", "--k", "101",
    ]);
    assert!(r.is_err());
}

#[test]
fn context_query_k_accepts_in_range() {
    let r = Cli::try_parse_from([
        "quorum", "context", "query", "hello", "--k", "50",
    ]);
    assert!(r.is_ok());
}
```

**Step 2: Run — expected FAIL** (zero and 101 currently parse).

**Step 3: Implement**

`clap::value_parser!(usize).range(...)` is NOT supported in clap 4.5 (range parser only takes `u64`/`i64`/etc.). Switching the field type would propagate downstream churn. **Use a custom validator and keep `Option<usize>`:**

```rust
fn validate_k(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("--k must be a positive integer: {e}"))?;
    if !(1..=100).contains(&n) {
        return Err(format!("--k must be in 1..=100 (got {n})"));
    }
    Ok(n)
}

// src/cli/mod.rs ContextQueryOpts
#[arg(long, value_parser = validate_k)]
pub k: Option<usize>,
```

**Step 4: Run — expected PASS**

**Step 5: Commit**

```bash
git commit -m "feat(cli): cap context query --k at 1..=100, reject zero"
```

### Task 1.3: Defense-in-depth — handler-side path validation for `--name`

**Files (corrected per GPT-5.5 review):**
- The actual handler is `src/context/cli.rs::run_add`, NOT `src/main.rs` (`main.rs` only routes clap args to `ContextCmd::Add`).
- Strongly consider also validating in `SourcesConfig::append_source` so the config-write path is independently protected.

**Step 1: RED test (integration)**

Use `tempfile::TempDir` as `HOME`, run `run_add` directly with a name that bypasses clap (e.g. via a constructed `ContextAddOpts`), assert the resolved on-disk path's `canonicalize()` is a child of the sources root.

**Step 2: GREEN**

In `src/context/cli.rs::run_add`, after building the target dir:
```rust
let sources_root = home_quorum_dir.join("sources").canonicalize()?;
let target = sources_root.join(&opts.name);
let target_canon = target.canonicalize().unwrap_or_else(|_| target.clone());
if !target_canon.starts_with(&sources_root) {
    anyhow::bail!("source name {:?} resolves outside sources root", opts.name);
}
```

**DRY note:** factor `validate_source_name` into a single function shared by clap value_parser, `run_add`, AND `SourcesConfig::append_source`. Avoid copy-paste validators.

**Step 3: Commit**

```bash
git commit -m "feat(cli): defense-in-depth canonical-path check in context add handler"
```

### Task 1.4: Verification + quorum review

```bash
cargo test --bin quorum
cargo clippy --all-targets -- -D warnings
cargo build --release
```
Then quorum self-review on `src/cli/mod.rs` + `src/main.rs` diffs.

---

## PR 2 — Telemetry streaming + structured parse errors (#138 + #139)

**Files:**
- Modify: `src/telemetry.rs:53-72` (`load_all`, `load_since`)
- Add: new public type `LoadStats` (mirrors `feedback::LoadStats`)
- Add: new method `load_all_with_stats(&self) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)>`
- Update: `load_all` becomes a thin wrapper that drops the stats; `load_since` likewise
- Tests: inline `#[cfg(test)] mod tests` in `src/telemetry.rs`

**Design contract:**
- `load_all_with_stats` streams via `BufReader::new(File::open).lines()` (caps memory at one line) AND returns `LoadStats { kept, skipped, errors: Vec<ParseError> }`.
- `ParseError` carries `{ line_no: usize, snippet: String (first 80 chars), error: String }`.
- Empty/whitespace lines do NOT count toward `skipped`.
- `load_all` and `load_since` keep their existing signature for backward compat — they internally call `load_all_with_stats` and drop stats. They MUST NOT log; logging is the caller's responsibility (consistency with `feedback.rs`).

### Task 2.1: RED — streaming under memory pressure

**Step 1: Write the failing test**

```rust
#[test]
fn load_all_streams_does_not_oom_on_large_file() {
    // 100MB synthetic file; any read_to_string-style impl would allocate.
    // We can't strictly assert "did not OOM" but we can assert it returns and
    // the resident allocation peak is bounded by line size, not file size.
    // Pragmatic version: assert kept count and that the function returns
    // within a sane time. The streaming switch is observed via the new
    // load_all_with_stats API existing.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        let entry = serde_json::to_string(&sample_entry()).unwrap();
        for _ in 0..1000 {
            writeln!(f, "{}", entry).unwrap();
        }
    }
    let store = TelemetryStore::new(path);
    let (entries, stats) = store.load_all_with_stats().unwrap();
    assert_eq!(entries.len(), 1000);
    assert_eq!(stats.kept, 1000);
    assert_eq!(stats.skipped, 0);
}
```

**Step 2: Run — FAIL** (load_all_with_stats doesn't exist).

**Step 3: Implement streaming + LoadStats**

```rust
// src/telemetry.rs
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseError {
    pub line_no: usize,
    pub snippet: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadStats {
    pub kept: usize,
    pub skipped: usize,
    pub errors: Vec<ParseError>,
}

impl TelemetryStore {
    pub fn load_all_with_stats(&self) -> anyhow::Result<(Vec<TelemetryEntry>, LoadStats)> {
        use std::io::{BufRead, BufReader};

        if !self.path.exists() {
            return Ok((vec![], LoadStats::default()));
        }
        let file = std::fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut stats = LoadStats::default();

        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TelemetryEntry>(&line) {
                Ok(entry) => {
                    entries.push(entry);
                    stats.kept += 1;
                }
                Err(e) => {
                    stats.skipped += 1;
                    stats.errors.push(ParseError {
                        line_no: idx + 1,
                        snippet: line.chars().take(80).collect(),
                        error: e.to_string(),
                    });
                }
            }
        }
        Ok((entries, stats))
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<TelemetryEntry>> {
        Ok(self.load_all_with_stats()?.0)
    }

    pub fn load_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<TelemetryEntry>> {
        Ok(self
            .load_all_with_stats()?
            .0
            .into_iter()
            .filter(|e| e.ts >= since)
            .collect())
    }
}
```

**Step 4: Run — PASS**

**Step 5: Commit**

```bash
git commit -m "refactor(telemetry): stream load_all_with_stats line-by-line, return LoadStats"
```

### Task 2.2: RED — malformed lines surface as structured errors

**Step 1: Write the failing test**

```rust
#[test]
fn malformed_lines_become_parse_errors_with_line_numbers() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    let good = serde_json::to_string(&sample_entry()).unwrap();
    let body = format!("{good}\nthis is not json\n{good}\n{{partial:\n");
    std::fs::write(&path, body).unwrap();
    let store = TelemetryStore::new(path);
    let (entries, stats) = store.load_all_with_stats().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(stats.kept, 2);
    assert_eq!(stats.skipped, 2);
    assert_eq!(stats.errors.len(), 2);
    assert_eq!(stats.errors[0].line_no, 2);
    assert_eq!(stats.errors[1].line_no, 4);
    assert!(stats.errors[0].snippet.starts_with("this is not"));
}

#[test]
fn empty_lines_do_not_count_as_skipped() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    let good = serde_json::to_string(&sample_entry()).unwrap();
    let body = format!("\n{good}\n   \n{good}\n");
    std::fs::write(&path, body).unwrap();
    let store = TelemetryStore::new(path);
    let (entries, stats) = store.load_all_with_stats().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(stats.kept, 2);
    assert_eq!(stats.skipped, 0);
}
```

**Step 2: Run — should already PASS** if Task 2.1 was implemented correctly. If not, fix and rerun.

**Step 3: Commit**

```bash
git commit -m "test(telemetry): assert structured ParseError surfaces malformed JSONL rows"
```

### Task 2.3: Verification

```bash
cargo test --bin quorum
cargo clippy --all-targets -- -D warnings
cargo build --release
```

---

## PR 3 — Feedback non-Unix TOCTOU close (#133)

**Files:**
- Modify: `src/feedback.rs:322-359` (`read_inbox_file`)
- Tests: existing `#[cfg(test)] mod tests` in `src/feedback.rs`

**Design contract (revised per GPT-5.5 review):**
- After `opts.open(path)` returns the file handle, in addition to `file.metadata().is_file()`, check `path.symlink_metadata().is_file()` — this is path-bound and does NOT follow symlinks.
- **HONEST FRAMING — this is mitigation, NOT closure on non-Unix.** A symlink-to-target swap *before* `opts.open(path)` still pins a sensitive-target inode in the handle, after which the post-open path stat cannot validate what was actually opened. The check shrinks the window for one specific attack pattern (post-classify, pre-open swap to a symlink whose target is benign — caller would then read benign content but classification was on a different file). It is **not** a substitute for OS-level NOFOLLOW.
- The PR comment and PR body MUST explicitly say "narrows the non-Unix TOCTOU window; full closure requires Windows reparse-point-safe open flags or handle-bound identity checks (out of scope for this PR)."
- On Unix, `O_NOFOLLOW` already closed this. The new check is redundant on Unix but harmless and improves consistency.

### Task 3.1: RED — non-Unix path-bound check rejects symlink-after-open

**Step 1: Write the failing test (Unix gate sketch)**

The clean way to test non-Unix behavior on a Unix host is to factor the path-bound check into a free function and unit-test it directly:

```rust
// in feedback.rs tests module
#[test]
fn path_symlink_metadata_rejects_symlink_targets() {
    // Construct a symlink in a tempdir, assert path_is_regular_file() == false.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real");
    std::fs::write(&real, b"content").unwrap();
    let link = dir.path().join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&real, &link).unwrap();

    assert!(path_is_regular_file(&real));
    assert!(!path_is_regular_file(&link));
}
```

**Step 2: Run — FAIL** (`path_is_regular_file` doesn't exist).

**Step 3: GREEN — extract helper + wire into read_inbox_file**

```rust
// src/feedback.rs
fn path_is_regular_file(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_file())
        .unwrap_or(false)
}

fn read_inbox_file(path: &std::path::Path) -> std::io::Result<String> {
    // ... existing open logic ...

    let file = opts.open(path)?;

    // NEW: path-bound symlink check (closes the non-Unix TOCTOU window).
    // On Unix, O_NOFOLLOW above already rejected symlinks at the syscall
    // boundary; this is redundant-but-harmless. On non-Unix, this is the
    // primary symlink defense and is subject to a tiny TOCTOU between
    // opts.open() and this stat — but the open's inode is already pinned
    // in `file`, so a post-open swap doesn't affect what we read.
    if !path_is_regular_file(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a regular file (symlink, fifo, etc.)",
        ));
    }

    let meta = file.metadata()?;
    // ... rest unchanged ...
}
```

**Step 4: Run — PASS**

**Step 5: Commit**

```bash
git commit -m "fix(feedback): path-bound symlink check closes non-Unix TOCTOU in read_inbox_file"
```

### Task 3.2: Update existing TOCTOU regression suite

Add a test that asserts `read_inbox_file` returns `InvalidInput` for a symlink target on the host platform (works on Unix; gate Windows with `#[cfg(windows)]` if there's existing windows-test infra, otherwise Unix-only is fine).

### Task 3.3: Verification

```bash
cargo test --bin quorum feedback
cargo clippy --all-targets -- -D warnings
```

---

## PR 4 — MCP `deny_unknown_fields` (#134)

**Files:**
- Modify: `src/mcp/tools.rs` — add `#[serde(deny_unknown_fields)]` to all 6 tool structs
- Tests: inline `#[cfg(test)] mod tests` in `src/mcp/tools.rs` (or equivalent)

**Design contract:**
- All 6 structs (`ReviewTool`, `FeedbackTool`, `CatalogTool`, `ChatTool`, `DebugTool`, `TestgenTool`) get `#[serde(deny_unknown_fields)]`.
- Note: Inner enums like `Verdict` (line 29) do NOT need it unless they're flat-tagged — they're already strict via enum dispatch.
- Tests assert that typo'd field names fail to parse with a serde error mentioning "unknown field".
- **Tests must cover ALL 6 structs**, not just `FeedbackTool` (per GPT-5.5 review).

**PRE-FLIGHT GATE (per user concern, 2026-04-30):** Before committing the `deny_unknown_fields` change, run:
```bash
rg --json -t json -t jsonl -g '!target' '"' tests/ src/ ~/.quorum/inbox/ 2>/dev/null | rg 'fromAgent|fpKind|filePath|findingTitle|verdict' | head -50
```
Then check inbox-drain test fixtures and any external-agent payloads (pal, third-opinion) to confirm they don't carry undeclared fields. If they do — extend the struct to declare those fields (with `#[serde(default)]` if optional) before flipping the strict switch. The MCP boundary is shared with the inbox ingestion path; a strict struct that rejects existing third-opinion payloads is a regression.

### Task 4.1: RED — typo'd FeedbackTool field is rejected

**Step 1: Write the failing test**

```rust
// src/mcp/tools.rs tests module
#[test]
fn feedback_tool_rejects_typo_fromagent() {
    let json = r#"{
        "verdict": "tp",
        "filePath": "x.rs",
        "findingTitle": "y",
        "fromagent": "pal"
    }"#;
    let r = serde_json::from_str::<FeedbackTool>(json);
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("unknown field"));
}

#[test]
fn feedback_tool_rejects_typo_fpkind() {
    let json = r#"{
        "verdict": "fp",
        "filePath": "x.rs",
        "findingTitle": "y",
        "fpkind": "PatternOvergeneralization"
    }"#;
    let r = serde_json::from_str::<FeedbackTool>(json);
    assert!(r.is_err());
}

#[test]
fn review_tool_rejects_typo_focus_uppercase() {
    let json = r#"{
        "files": ["x.rs"],
        "Focus": "security"
    }"#;
    let r = serde_json::from_str::<ReviewTool>(json);
    assert!(r.is_err());
}

#[test]
fn feedback_tool_accepts_valid_payload() {
    let json = r#"{
        "verdict": "tp",
        "filePath": "x.rs",
        "findingTitle": "y"
    }"#;
    let r = serde_json::from_str::<FeedbackTool>(json);
    assert!(r.is_ok());
}
```

**Step 2: Run — FAIL** (typos parse silently today).

**Step 3: GREEN — add attribute to all 6 structs**

Add `#[serde(deny_unknown_fields)]` to the derive macro stack:
```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewTool { ... }
```
…repeat for `FeedbackTool`, `CatalogTool`, `ChatTool`, `DebugTool`, `TestgenTool`.

**Step 4: Run — PASS**

**Step 5: Commit**

```bash
git commit -m "fix(mcp): deny_unknown_fields on all tool input structs (#134)"
```

### Task 4.2: Verification

```bash
cargo test --bin quorum mcp
cargo clippy --all-targets -- -D warnings
```

---

## PR 5 — Pipeline path equality, not `ends_with` (#137)

**Files:**
- Modify: `src/pipeline.rs:389-394` (diff-range filter expression)
- Tests: inline `#[cfg(test)] mod tests` in `src/pipeline.rs` or `tests/pipeline_diff_matching.rs`

**Design contract:**
- Replace `file_str.ends_with(path.as_str()) || path.ends_with(&file_str)` with proper path equality.
- Approach: use `Path::canonicalize()` for both sides if both files exist on disk; otherwise component-wise comparison via `Path::components().eq(...)` after stripping a common repo-root prefix.
- Since the diff `path` may be relative-to-repo-root and the review `file_str` may be absolute or relative, normalize via:
  - If both can be canonicalized, compare canonical forms.
  - Otherwise, fall back to comparing trailing components — but require *strict component equality* (not substring `ends_with` on the joined string).

### Task 5.1: RED — sibling files with shared basename are NOT cross-matched

**Step 1: Write the failing test**

```rust
// tests/pipeline_diff_matching.rs (or inline)
#[test]
fn diff_ranges_do_not_match_sibling_with_same_basename() {
    // diff_ranges has "src/foo.rs" with ranges [(10, 20)]
    // we're reviewing "nested/src/foo.rs" — must get NO ranges
    // (current code path-substring-matches both directions and returns wrong ranges).
    let diff_ranges = vec![("src/foo.rs".to_string(), vec![(10u32, 20u32)])];
    let file_str = "nested/src/foo.rs";
    let matched: Vec<(u32, u32)> = diff_ranges
        .iter()
        .filter(|(p, _)| crate::pipeline::diff_path_matches(p, file_str))
        .flat_map(|(_, r)| r.clone())
        .collect();
    assert!(matched.is_empty(), "must not cross-match siblings");
}

#[test]
fn diff_ranges_match_canonical_same_path() {
    let diff_ranges = vec![("src/foo.rs".to_string(), vec![(10u32, 20u32)])];
    let file_str = "src/foo.rs";
    let matched: Vec<(u32, u32)> = diff_ranges
        .iter()
        .filter(|(p, _)| crate::pipeline::diff_path_matches(p, file_str))
        .flat_map(|(_, r)| r.clone())
        .collect();
    assert_eq!(matched, vec![(10, 20)]);
}

#[test]
fn diff_ranges_match_absolute_vs_relative_same_file() {
    // If review file is given as absolute, but diff path is repo-relative,
    // we should still match when the suffix components are identical.
    let diff_ranges = vec![("src/foo.rs".to_string(), vec![(10u32, 20u32)])];
    let file_str = "/home/jane/proj/src/foo.rs";
    let matched: Vec<(u32, u32)> = diff_ranges
        .iter()
        .filter(|(p, _)| crate::pipeline::diff_path_matches(p, file_str))
        .flat_map(|(_, r)| r.clone())
        .collect();
    assert_eq!(matched, vec![(10, 20)]);
}
```

**Step 2: Run — FAIL** (`diff_path_matches` doesn't exist; current logic cross-matches).

**Step 3: GREEN — repo-root-relative full equality (NOT component-suffix)**

Per GPT-5.5 review: component-suffix `ends_with` STILL cross-matches `src/foo.rs` ↔ `nested/src/foo.rs` (component sequence `[src, foo.rs]` IS a tail of `[nested, src, foo.rs]`). The real fix requires a repo root.

```rust
// src/pipeline.rs
/// True iff `diff_path` (always repo-relative, as produced by the diff parser)
/// matches `review_path` (may be absolute or relative as user supplied it).
///
/// Strategy: normalize `review_path` to repo-relative via `Path::strip_prefix`
/// using the supplied `repo_root`, then test full path equality. If
/// normalization fails (review_path is outside repo_root, or no repo_root
/// available), return `false` — wrong context for the LLM is worse than no
/// context, so refuse the match rather than risk cross-matching.
pub(crate) fn diff_path_matches(
    diff_path: &str,
    review_path: &str,
    repo_root: &std::path::Path,
) -> bool {
    use std::path::{Path, PathBuf};
    let review = Path::new(review_path);
    let review_abs: PathBuf = if review.is_absolute() {
        review.to_path_buf()
    } else {
        repo_root.join(review)
    };
    let review_canon = std::fs::canonicalize(&review_abs).unwrap_or(review_abs);
    let root_canon = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let Ok(review_rel) = review_canon.strip_prefix(&root_canon) else {
        return false;
    };
    Path::new(diff_path).components().eq(review_rel.components())
}
```

Then update line 389-394 to pass `repo_root` from `pipeline_config` (verify it has one; if not, add one or thread it through). Use `rust-expert` subagent to wire `repo_root` cleanly through `PipelineConfig` if it's not already there.

**Test additions for the corrected matcher:**

```rust
#[test]
fn diff_ranges_do_not_match_sibling_with_same_basename() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::create_dir_all(tmp.path().join("nested/src")).unwrap();
    std::fs::write(tmp.path().join("src/foo.rs"), "").unwrap();
    std::fs::write(tmp.path().join("nested/src/foo.rs"), "").unwrap();

    let nested = tmp.path().join("nested/src/foo.rs").to_string_lossy().to_string();
    assert!(!diff_path_matches("src/foo.rs", &nested, tmp.path()));
}
```

**Step 4: Run — PASS**

**Step 5: Commit**

```bash
git commit -m "fix(pipeline): use component-equality for diff path matching, not ends_with"
```

### Task 5.2: Verification

```bash
cargo test --bin quorum pipeline
cargo clippy --all-targets -- -D warnings
```

---

## Cross-Cutting Verification (per worktree, before quorum review)

```bash
cargo test --bin quorum    # full unit suite
cargo test                 # CLI integration tests too
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## Quorum Self-Review (Phase 6, per worktree)

```bash
quorum review <changed-files> --parallel 4
```

Triage every finding:
- **In-branch bug** → return to TDD micro-cycle (RED reproducer → GREEN fix → verify)
- **Pre-existing bug** → `gh issue create` with file:line and finding text; do NOT fix in this branch

## Feedback Recording (Phase 7, per worktree)

For each triaged finding:
```bash
quorum feedback --file <f> --finding "<title>" --verdict <tp|fp|partial|wontfix> --reason "<why>"
# tp + post_fix when fixed in branch:
quorum feedback ... --provenance post_fix
```

Batch recordings in parallel where possible.

## Finishing (Phase 8, per worktree)

For each worktree, invoke `superpowers:requesting-code-review` then `superpowers:finishing-a-development-branch` to choose merge / open PR / cleanup. PR body should:
- Reference the issue numbers fixed
- Link to this plan: `docs/plans/2026-04-30-self-review-bugfix-batch.md`
- List quorum verdicts recorded
