# CLI Feedback Command Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a `quorum feedback` CLI subcommand for recording finding verdicts (tp/fp/partial/wontfix) from the terminal, achieving parity with the MCP feedback tool.

**Architecture:** New `FeedbackOpts` in cli/mod.rs, new `run_feedback()` in main.rs. Reuses existing `FeedbackStore::record()`. Three output modes per DESIGN.md (human/compact/JSON). Verdict validation at the CLI layer.

**Tech Stack:** Rust, clap (existing), serde_json (existing), chrono (existing)

**Testing guidance (from anti-pattern review):**
- Use real FeedbackStore with tempfile, don't mock internals
- Test through public interfaces (run_feedback), not private helpers
- Assert on properties (contains key fields), never snapshot full output
- Every test asserts exit code, JSONL content, or output substring

---

## Task 1: Add FeedbackOpts and Verdict Parsing

**Files:**
- Modify: `src/cli/mod.rs` (add FeedbackOpts, add Feedback to Command enum)

**Step 1: Write failing test — verdict parsing**

In `src/cli/mod.rs`, add to test module (or create one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_valid() {
        assert_eq!(parse_verdict("tp").unwrap(), crate::feedback::Verdict::Tp);
        assert_eq!(parse_verdict("fp").unwrap(), crate::feedback::Verdict::Fp);
        assert_eq!(parse_verdict("partial").unwrap(), crate::feedback::Verdict::Partial);
        assert_eq!(parse_verdict("wontfix").unwrap(), crate::feedback::Verdict::Wontfix);
    }

    #[test]
    fn parse_verdict_case_insensitive() {
        assert_eq!(parse_verdict("TP").unwrap(), crate::feedback::Verdict::Tp);
        assert_eq!(parse_verdict("Fp").unwrap(), crate::feedback::Verdict::Fp);
    }

    #[test]
    fn parse_verdict_invalid() {
        assert!(parse_verdict("maybe").is_err());
        assert!(parse_verdict("").is_err());
    }
}
```

**Step 2: Run test to verify it fails**

```bash
rtk cargo test --bin quorum cli::tests -v
```

Expected: compilation error (parse_verdict doesn't exist)

**Step 3: Implement**

Add to `src/cli/mod.rs`:

```rust
/// Parse a verdict string into a Verdict enum.
pub fn parse_verdict(s: &str) -> anyhow::Result<crate::feedback::Verdict> {
    match s.to_lowercase().as_str() {
        "tp" => Ok(crate::feedback::Verdict::Tp),
        "fp" => Ok(crate::feedback::Verdict::Fp),
        "partial" => Ok(crate::feedback::Verdict::Partial),
        "wontfix" => Ok(crate::feedback::Verdict::Wontfix),
        other => anyhow::bail!("Invalid verdict '{}'. Must be: tp, fp, partial, wontfix", other),
    }
}
```

Add `FeedbackOpts` struct:

```rust
#[derive(Parser)]
pub struct FeedbackOpts {
    /// File path the finding was about
    #[arg(long)]
    pub file: String,

    /// Finding title or substring to match
    #[arg(long)]
    pub finding: String,

    /// Verdict: tp, fp, partial, wontfix
    #[arg(long)]
    pub verdict: String,

    /// Reason for the verdict
    #[arg(long)]
    pub reason: String,

    /// Model that produced the finding (optional)
    #[arg(long)]
    pub model: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

Add `Feedback(FeedbackOpts)` variant to the `Command` enum.

**Step 4: Run tests**

```bash
rtk cargo test --bin quorum cli::tests
```

Expected: all pass

**Step 5: Commit**

```bash
git add src/cli/mod.rs
git commit -m "feat(cli): add FeedbackOpts and verdict parsing for feedback subcommand"
```

---

## Task 2: Implement run_feedback

**Files:**
- Modify: `src/main.rs` (add run_feedback function, wire into command dispatch)

**Step 1: Write failing test — entry construction**

Since `run_feedback` writes to a file and produces output, test it end-to-end with a tempfile. Add a test module at the bottom of `src/main.rs` or in a new test file.

Actually, since run_feedback will be a simple function, test it via its observable effects: JSONL file content and exit code. Add to inline tests in main.rs:

```rust
#[cfg(test)]
mod feedback_tests {
    use super::*;
    use tempfile::TempDir;

    fn run_feedback_with_args(
        file: &str, finding: &str, verdict: &str, reason: &str, feedback_path: &std::path::Path,
    ) -> (i32, String) {
        run_feedback_inner(file, finding, verdict, reason, None, feedback_path)
    }

    #[test]
    fn feedback_records_tp_verdict() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _output) = run_feedback_with_args(
            "src/auth.rs", "SQL injection", "tp", "Fixed with params", &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("SQL injection"));
        assert!(contents.contains("\"verdict\":\"tp\""));
        assert!(contents.contains("src/auth.rs"));
    }

    #[test]
    fn feedback_invalid_verdict_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _output) = run_feedback_with_args(
            "src/auth.rs", "SQL injection", "maybe", "Not sure", &path,
        );
        assert_eq!(exit_code, 3);
    }

    #[test]
    fn feedback_provenance_is_human() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let (exit_code, _) = run_feedback_with_args(
            "src/auth.rs", "SQL injection", "tp", "Real issue", &path,
        );
        assert_eq!(exit_code, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"provenance\":\"human\""));
    }
}
```

**Step 2: Run to verify failure**

```bash
rtk cargo test --bin quorum feedback_tests
```

Expected: compilation error

**Step 3: Implement run_feedback_inner and run_feedback**

Add to `src/main.rs`:

```rust
/// Core feedback logic — testable with custom feedback path.
fn run_feedback_inner(
    file: &str,
    finding: &str,
    verdict_str: &str,
    reason: &str,
    model: Option<&str>,
    feedback_path: &std::path::Path,
) -> (i32, String) {
    let verdict = match cli::parse_verdict(verdict_str) {
        Ok(v) => v,
        Err(e) => {
            return (3, format!("Error: {}", e));
        }
    };

    let entry = feedback::FeedbackEntry {
        file_path: file.to_string(),
        finding_title: finding.to_string(),
        finding_category: "manual".to_string(),
        verdict: verdict.clone(),
        reason: reason.to_string(),
        model: model.map(|s| s.to_string()),
        timestamp: chrono::Utc::now(),
        provenance: feedback::Provenance::Human,
    };

    let store = feedback::FeedbackStore::new(feedback_path.to_path_buf());
    if let Err(e) = store.record(&entry) {
        return (3, format!("Error: Failed to write feedback: {}", e));
    }

    let total = store.count().unwrap_or(0);
    let verdict_str = format!("{:?}", entry.verdict).to_lowercase();

    // Format output based on mode
    let use_compact = output::should_use_compact(false);
    let use_json = !use_compact && !std::io::IsTerminal::is_terminal(&std::io::stdout());

    let output = if use_json {
        // Use serde for proper escaping of special chars in finding titles
        let json_obj = serde_json::json!({
            "verdict": verdict_str,
            "file_path": entry.file_path,
            "finding_title": entry.finding_title,
            "total": total,
        });
        serde_json::to_string(&json_obj).unwrap_or_default()
    } else if use_compact {
        format!("feedback:{}|{}|{}", verdict_str, entry.file_path, entry.finding_title)
    } else {
        format!(
            "Recorded: {} for \"{}\" in {} ({} entries)",
            verdict_str, entry.finding_title, entry.file_path, total,
        )
    };

    (0, output)
}

/// CLI entry point for `quorum feedback`.
fn run_feedback(opts: cli::FeedbackOpts) -> i32 {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let feedback_path = std::path::PathBuf::from(&home).join(".quorum/feedback.jsonl");
    let (exit_code, output) = run_feedback_inner(
        &opts.file, &opts.finding, &opts.verdict, &opts.reason,
        opts.model.as_deref(), &feedback_path,
    );
    if exit_code != 0 {
        eprintln!("{}", output);
    } else {
        println!("{}", output);
    }
    exit_code
}
```

Wire into command dispatch in `main()`:

```rust
Command::Feedback(opts) => std::process::exit(run_feedback(opts)),
```

**Step 4: Run tests**

```bash
rtk cargo test --bin quorum feedback_tests
```

Expected: all 3 pass

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): implement quorum feedback subcommand

Records tp/fp/partial/wontfix verdicts to ~/.quorum/feedback.jsonl.
Three output modes: human (TTY), compact (CLAUDE_CODE), JSON (piped).
Provenance set to 'human' for CLI-recorded feedback."
```

---

## Task 3: Output Mode Tests

**Files:**
- Modify: `src/main.rs` (add output format tests)

**Step 1: Write failing tests**

```rust
#[test]
fn feedback_output_contains_key_fields() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("feedback.jsonl");
    let (_, output) = run_feedback_with_args(
        "src/auth.rs", "SQL injection", "tp", "Fixed", &path,
    );
    // Human mode output (in tests, stdout may not be TTY so it may be JSON)
    // Just verify key fields are present in either format
    assert!(output.contains("tp"));
    assert!(output.contains("src/auth.rs"));
    assert!(output.contains("SQL injection"));
}

#[test]
fn feedback_json_output_parseable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("feedback.jsonl");
    let (exit_code, output) = run_feedback_with_args(
        "src/auth.rs", "SQL injection", "fp", "Not a real issue", &path,
    );
    assert_eq!(exit_code, 0);
    // When not a TTY (test environment), output is JSON
    // Try parsing — if it's JSON, verify fields
    if output.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["verdict"], "fp");
        assert_eq!(v["file_path"], "src/auth.rs");
        assert_eq!(v["finding_title"], "SQL injection");
        assert!(v["total"].is_number());
    }
}
```

**Step 2: Run to verify**

```bash
rtk cargo test --bin quorum feedback_tests
```

Expected: pass (these test against the already-implemented function)

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "test(feedback): add output format validation tests"
```

---

## Task 4: Update DESIGN.md and CLAUDE.md

**Files:**
- Modify: `DESIGN.md` (add feedback command section if not present)
- Modify: `CLAUDE.md` (add feedback command to Commands section)

**Step 1: Add to CLAUDE.md Commands section**

```bash
cargo run -- feedback --file src/main.rs --finding "SQL injection" --verdict tp --reason "Fixed"
```

**Step 2: Commit**

```bash
git add CLAUDE.md DESIGN.md
git commit -m "docs: add quorum feedback CLI command to documentation"
```

---

## Task 5: Verify End-to-End

**Step 1: Run full test suite**

```bash
rtk cargo test --bin quorum
```

**Step 2: Manual smoke test**

```bash
# Record a test verdict
cargo run -- feedback --file src/test.rs --finding "test finding" --verdict tp --reason "smoke test"

# Verify it appears in stats
cargo run -- stats --compact

# Verify JSON output (piped)
cargo run -- feedback --file src/test.rs --finding "another finding" --verdict fp --reason "not real" | jq .

# Verify compact output
CLAUDE_CODE=1 cargo run -- feedback --file src/test.rs --finding "compact test" --verdict tp --reason "test"
```

**Step 3: Run clippy**

```bash
rtk cargo clippy --all-targets
```
