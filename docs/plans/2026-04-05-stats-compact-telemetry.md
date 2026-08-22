# Stats, Compact Output & Telemetry Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add token tracking, telemetry capture, compact output mode, numeric formatting, cost estimation, and a `quorum stats` subcommand -- closing the gap between DESIGN.md and implementation.

**Architecture:** Eight features built bottom-up. Pure utility functions first (formatting, token parsing), then data capture (telemetry), then presentation (compact output, stats command). Each feature is independently testable. No feature depends on an unimplemented feature above it in the list.

**Tech Stack:** Rust, serde_json, chrono, clap, tempfile (tests)

**Anti-pattern guide:** See `docs/TDD_ANTIPATTERN_GUIDE.md` for per-feature testing guidance.

---

## Task 1: Numeric Formatting Helper

**Why first:** Pure function, zero dependencies, needed by tasks 6-8. Textbook TDD candidate.

**Files:**
- Create: `src/formatting.rs`
- Modify: `src/main.rs` (add `mod formatting;`)

**Step 1: Write the failing test**

In `src/formatting.rs`:

```rust
/// Numeric formatting: human-readable k/M suffixes per DESIGN.md section 11.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_count_cases() {
        let cases = [
            (0, "0"),
            (1, "1"),
            (999, "999"),
            (1_000, "1.0k"),
            (1_050, "1.1k"),
            (1_500, "1.5k"),
            (10_000, "10.0k"),
            (63_100, "63.1k"),
            (999_999, "1000.0k"),
            (1_000_000, "1.0M"),
            (1_500_000, "1.5M"),
            (42_000_000, "42.0M"),
        ];
        for (input, expected) in cases {
            assert_eq!(format_count(input), expected, "format_count({input})");
        }
    }

    #[test]
    fn format_duration_cases() {
        use std::time::Duration;
        let cases = [
            (Duration::from_millis(0), "0ms"),
            (Duration::from_millis(50), "50ms"),
            (Duration::from_millis(1318), "1318ms"),
            (Duration::from_secs(4), "4.0s"),
            (Duration::from_millis(4200), "4.2s"),
            (Duration::from_secs(62), "62.0s"),
        ];
        for (input, expected) in cases {
            assert_eq!(format_duration(input), expected, "format_duration({input:?})");
        }
    }

    #[test]
    fn format_cost_cases() {
        // Use approx comparison for floats
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(0.005), "$0.01");
        assert_eq!(format_cost(2.14), "$2.14");
        assert_eq!(format_cost(15.7), "$15.70");
    }

    #[test]
    fn format_pct_cases() {
        assert_eq!(format_pct(0.0), "0%");
        assert_eq!(format_pct(0.5), "50%");
        assert_eq!(format_pct(0.888), "89%");
        assert_eq!(format_pct(1.0), "100%");
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum format_count_cases`
Expected: FAIL -- `format_count` not found

**Step 3: Write minimal implementation**

Above the test module in `src/formatting.rs`:

```rust
use std::time::Duration;

pub fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms >= 4_000 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}ms", ms)
    }
}

pub fn format_cost(dollars: f64) -> String {
    format!("${:.2}", dollars)
}

pub fn format_pct(ratio: f64) -> String {
    format!("{}%", (ratio * 100.0).round() as u32)
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum formatting`
Expected: ALL PASS

**Step 5: Register the module**

Add `mod formatting;` to `src/main.rs` near the other `mod` declarations.

**Step 6: Commit**

```bash
git add src/formatting.rs src/main.rs
git commit -m "feat: add numeric formatting helpers (k/M, duration, cost, pct)"
```

---

## Task 2: Token Usage Extraction from LLM Responses

**Why:** Required for telemetry and spend tracking. Pure parsing function extracted from the HTTP client.

**Files:**
- Modify: `src/llm_client.rs`

**Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/llm_client.rs`:

```rust
    // -- parse_usage --

    #[test]
    fn parse_usage_valid_chat_completion() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 1500,
                "completion_tokens": 800,
                "total_tokens": 2300
            }
        });
        let usage = parse_usage(&json).unwrap();
        assert_eq!(usage.prompt_tokens, 1500);
        assert_eq!(usage.completion_tokens, 800);
    }

    #[test]
    fn parse_usage_missing_usage_key() {
        let json = serde_json::json!({"choices": []});
        assert!(parse_usage(&json).is_none());
    }

    #[test]
    fn parse_usage_null_tokens() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": null,
                "completion_tokens": null
            }
        });
        let usage = parse_usage(&json);
        assert!(usage.is_none());
    }

    #[test]
    fn parse_usage_zero_tokens() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0
            }
        });
        let usage = parse_usage(&json).unwrap();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum parse_usage`
Expected: FAIL -- `parse_usage` and `TokenUsage` not found

**Step 3: Write minimal implementation**

Add near the top of `src/llm_client.rs` (after the imports):

```rust
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

pub fn parse_usage(json: &serde_json::Value) -> Option<TokenUsage> {
    let usage = json.get("usage")?;
    let prompt = usage.get("prompt_tokens")?.as_u64()?;
    let completion = usage.get("completion_tokens")?.as_u64()?;
    Some(TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
    })
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum parse_usage`
Expected: ALL PASS

**Step 5: Wire into chat_completion and responses_api**

Change `chat_completion` return type from `Result<String>` to `Result<(String, Option<TokenUsage>)>`:

In `chat_completion()`, after `let json: serde_json::Value = resp.json().await?;` (line ~85):

```rust
let usage = parse_usage(&json);
```

And change the return to:
```rust
Ok((content.to_string(), usage))
```

Do the same for `responses_api()` -- extract usage before returning:
```rust
let usage = parse_usage(&json);
// ... existing text extraction ...
Ok((texts.join("\n"), usage))
```

Update the `LlmReviewer` trait and all callers to handle the tuple. The callers currently only use the `String` -- they can `let (content, _usage) = ...` for now. The `_usage` will be captured in Task 5 (telemetry).

**Step 6: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS (callers destructure the tuple, ignoring usage for now)

**Step 7: Commit**

```bash
git add src/llm_client.rs
git commit -m "feat: extract token usage from LLM API responses"
```

**Note:** If updating the trait signature cascades to many files (test_support.rs FakeReviewer, agent.rs, pipeline.rs), consider wrapping the return in a struct instead:

```rust
pub struct LlmResponse {
    pub content: String,
    pub usage: Option<TokenUsage>,
}
```

This is cleaner than a tuple and easier to extend later. Decide during implementation based on how many callers need updating.

---

## Task 3: Review Duration Tracking

**Why:** Needed for telemetry. Minimal code change.

**Files:**
- Modify: `src/main.rs` (in `run_review()`)

**Step 1: No TDD needed for this** (per anti-pattern guide -- trivial `Instant::now()` wrapper)

**Step 2: Add timing to run_review**

At the start of the review loop in `run_review()` (line ~207, before `for file_path in &opts.files`):

```rust
let review_start = std::time::Instant::now();
```

After the loop completes (before the exit code / output section):

```rust
let review_duration = review_start.elapsed();
```

These values will be consumed by Task 5 (telemetry). For now, just capture them as local variables.

**Step 3: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS (no behavior change)

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: capture review duration for telemetry"
```

---

## Task 4: Compact Output Formatter

**Why:** The `OutputMode::Compact` enum variant exists but has no formatter. This closes the gap.

**Files:**
- Modify: `src/output/mod.rs`
- Modify: `src/cli/mod.rs` (add `--compact` flag)
- Modify: `src/main.rs` (wire compact flag into output mode selection)

**Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/output/mod.rs`:

```rust
    // -- format_compact_finding --

    #[test]
    fn compact_finding_single_line() {
        let f = FindingBuilder::new()
            .title("SQL injection risk")
            .severity(Severity::Critical)
            .category("security")
            .lines(42, 42)
            .build();
        let out = format_compact_finding(&f);
        assert_eq!(out, "!|security|L42|SQL injection risk");
    }

    #[test]
    fn compact_finding_line_range() {
        let f = FindingBuilder::new()
            .title("Complex function")
            .severity(Severity::Medium)
            .category("complexity")
            .lines(10, 25)
            .build();
        let out = format_compact_finding(&f);
        assert_eq!(out, "~|complexity|L10-25|Complex function");
    }

    #[test]
    fn compact_finding_truncates_long_title() {
        let long_title = "A".repeat(100);
        let f = FindingBuilder::new()
            .title(&long_title)
            .severity(Severity::Info)
            .lines(1, 1)
            .build();
        let out = format_compact_finding(&f);
        // 80 chars + "..."
        assert!(out.len() < 120);
        assert!(out.ends_with("..."));
    }

    // -- format_compact_review --

    #[test]
    fn compact_review_with_findings() {
        let findings = vec![
            FindingBuilder::new()
                .title("Bug A")
                .severity(Severity::Critical)
                .category("security")
                .lines(42, 42)
                .build(),
            FindingBuilder::new()
                .title("Bug B")
                .severity(Severity::Medium)
                .category("style")
                .lines(10, 10)
                .build(),
        ];
        let out = format_compact_review("src/main.rs", &findings);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "!|security|L42|Bug A");
        assert_eq!(lines[1], "~|style|L10|Bug B");
        assert!(lines[2].contains("2 findings"));
    }

    #[test]
    fn compact_review_clean() {
        let out = format_compact_review("src/main.rs", &[]);
        assert_eq!(out.trim(), "clean");
    }

    #[test]
    fn compact_no_ansi_codes() {
        let f = FindingBuilder::new()
            .title("Test")
            .severity(Severity::Critical)
            .build();
        let out = format_compact_finding(&f);
        assert!(!out.contains("\x1b["));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum compact_finding`
Expected: FAIL -- `format_compact_finding` not found

**Step 3: Write minimal implementation**

In `src/output/mod.rs`, add:

```rust
pub fn format_compact_finding(f: &Finding) -> String {
    let icon = severity_icon(&f.severity);
    let line_label = if f.line_start == f.line_end {
        format!("L{}", f.line_start)
    } else {
        format!("L{}-{}", f.line_start, f.line_end)
    };
    let title = if f.title.len() > 80 {
        format!("{}...", &f.title[..80])
    } else {
        f.title.clone()
    };
    format!("{icon}|{cat}|{line}|{title}",
        icon = icon,
        cat = f.category,
        line = line_label,
        title = title,
    )
}

pub fn format_compact_review(file_path: &str, findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "clean".to_string();
    }

    let mut lines: Vec<String> = findings.iter()
        .map(|f| format_compact_finding(f))
        .collect();

    let critical = findings.iter()
        .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        .count();
    let warning = findings.iter()
        .filter(|f| f.severity == Severity::Medium)
        .count();
    let info = findings.len() - critical - warning;

    lines.push(format!("{} findings ({}C {}W {}I)",
        findings.len(), critical, warning, info));

    lines.join("\n")
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum compact`
Expected: ALL PASS

**Step 5: Add `--compact` CLI flag**

In `src/cli/mod.rs`, add to `ReviewOpts`:

```rust
    /// Token-efficient output for LLM consumption
    #[arg(long)]
    pub compact: bool,
```

**Step 6: Wire compact mode into main.rs**

In `run_review()`, change the output mode selection (around line ~204):

```rust
let use_compact = opts.compact || std::env::var("CLAUDE_CODE").is_ok();
let use_json = opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout());
```

Then in the output path, add compact handling before the human/json branch:

```rust
if use_compact {
    println!("{}", output::format_compact_review(&file_display, &findings));
} else if use_json {
    // existing json path
} else {
    // existing human path
}
```

**Step 7: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS

**Step 8: Commit**

```bash
git add src/output/mod.rs src/cli/mod.rs src/main.rs
git commit -m "feat: add compact output mode for LLM consumption (--compact / CLAUDE_CODE env)"
```

---

## Task 5: Telemetry Module

**Why:** Append-only JSONL capturing review metadata. Feeds the stats command.

**Files:**
- Create: `src/telemetry.rs`
- Modify: `src/main.rs` (add `mod telemetry;` and write entries after review)

**Step 1: Write the failing tests**

In `src/telemetry.rs`:

```rust
/// Review telemetry: append-only JSONL recording review metadata.
/// No file contents, no finding text, no code snippets. Just counts and metadata.

use std::path::PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryEntry {
    pub ts: DateTime<Utc>,
    pub files: Vec<String>,
    pub findings: HashMap<String, usize>,  // severity -> count
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub duration_ms: u64,
    pub suppressed: usize,
}

pub struct TelemetryStore {
    path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    fn sample_entry() -> TelemetryEntry {
        let mut findings = HashMap::new();
        findings.insert("critical".into(), 1);
        findings.insert("warning".into(), 2);
        TelemetryEntry {
            ts: Utc::now(),
            files: vec!["src/main.rs".into()],
            findings,
            model: "gpt-5.4".into(),
            tokens_in: 4200,
            tokens_out: 1800,
            duration_ms: 3400,
            suppressed: 2,
        }
    }

    #[test]
    fn round_trip_serialization() {
        let entry = sample_entry();
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: TelemetryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, "gpt-5.4");
        assert_eq!(parsed.tokens_in, 4200);
        assert_eq!(parsed.tokens_out, 1800);
        assert_eq!(parsed.files, vec!["src/main.rs"]);
    }

    #[test]
    fn record_and_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let store = TelemetryStore::new(path.clone());

        store.record(&sample_entry()).unwrap();
        store.record(&sample_entry()).unwrap();

        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn load_nonexistent_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let store = TelemetryStore::new(path);
        let entries = store.load_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn load_skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let store = TelemetryStore::new(path.clone());

        store.record(&sample_entry()).unwrap();
        // Append garbage
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{garbage}}").unwrap();
        writeln!(f, "not json at all").unwrap();
        store.record(&sample_entry()).unwrap();

        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 2); // skipped 2 bad lines
    }

    #[test]
    fn load_since_filters_by_date() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let store = TelemetryStore::new(path);

        let mut old = sample_entry();
        old.ts = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap().with_timezone(&Utc);
        store.record(&old).unwrap();

        let recent = sample_entry(); // ts = now
        store.record(&recent).unwrap();

        let since = chrono::DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .unwrap().with_timezone(&Utc);
        let entries = store.load_since(since).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum telemetry`
Expected: FAIL -- functions not implemented

**Step 3: Write minimal implementation**

Above the test module in `src/telemetry.rs`:

```rust
impl TelemetryStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn record(&self, entry: &TelemetryEntry) -> anyhow::Result<()> {
        use anyhow::Context;
        use std::io::Write;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open telemetry file: {}", self.path.display()))?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<TelemetryEntry>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(&self.path)?;
        let mut entries = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(entry) => entries.push(entry),
                Err(_) => continue,
            }
        }
        Ok(entries)
    }

    pub fn load_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<TelemetryEntry>> {
        Ok(self.load_all()?.into_iter().filter(|e| e.ts >= since).collect())
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum telemetry`
Expected: ALL PASS

**Step 5: Register module and wire into run_review**

Add `mod telemetry;` to `src/main.rs`.

After the review loop completes (where `review_duration` is captured from Task 3), add:

```rust
// Record telemetry (best-effort, don't fail the review)
let telem_path = home.join(".quorum/telemetry.jsonl");
let telem_store = telemetry::TelemetryStore::new(telem_path);
let mut finding_counts = std::collections::HashMap::new();
for f in &all_findings {
    let sev = format!("{:?}", f.severity).to_lowercase();
    *finding_counts.entry(sev).or_insert(0usize) += 1;
}
let telem_entry = telemetry::TelemetryEntry {
    ts: chrono::Utc::now(),
    files: opts.files.iter().map(|p| p.to_string_lossy().to_string()).collect(),
    findings: finding_counts,
    model: models.first().cloned().unwrap_or_default(),
    tokens_in: 0,   // TODO: accumulate from LLM responses once wired
    tokens_out: 0,  // TODO: same
    duration_ms: review_duration.as_millis() as u64,
    suppressed: 0,  // TODO: wire from calibrator
};
let _ = telem_store.record(&telem_entry); // best-effort
```

**Step 6: Run full test suite**

Run: `cargo test --bin quorum`
Expected: ALL PASS

**Step 7: Commit**

```bash
git add src/telemetry.rs src/main.rs
git commit -m "feat: add telemetry module -- append-only JSONL review metadata"
```

---

## Task 6: Cost Estimation

**Why:** Pure function mapping model names to per-token pricing. Needed by stats.

**Files:**
- Modify: `src/formatting.rs` (add pricing logic alongside numeric formatting)

**Step 1: Write the failing tests**

Add to `src/formatting.rs` tests:

```rust
    #[test]
    fn estimate_cost_known_model() {
        let cost = estimate_cost("gpt-5.4", 1_000_000, 500_000);
        // gpt-5.4: $2/M input, $8/M output -> $2.00 + $4.00 = $6.00
        assert!((cost - 6.0).abs() < 0.01, "cost was {cost}");
    }

    #[test]
    fn estimate_cost_unknown_model_fallback() {
        let cost = estimate_cost("unknown-model-xyz", 1_000_000, 500_000);
        // fallback: $3/M input, $15/M output -> $3.00 + $7.50 = $10.50
        assert!((cost - 10.5).abs() < 0.01, "cost was {cost}");
    }

    #[test]
    fn estimate_cost_zero_tokens() {
        assert!((estimate_cost("gpt-5.4", 0, 0)).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_cost_gemini() {
        let cost = estimate_cost("gemini-2.5-pro", 1_000_000, 500_000);
        // gemini-2.5-pro: $1.25/M input, $10/M output -> $1.25 + $5.00 = $6.25
        assert!((cost - 6.25).abs() < 0.01, "cost was {cost}");
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum estimate_cost`
Expected: FAIL -- `estimate_cost` not found

**Step 3: Write minimal implementation**

In `src/formatting.rs`:

```rust
/// Estimate cost in USD given model name and token counts.
/// Prices are per 1M tokens. Fallback for unknown models uses conservative estimates.
pub fn estimate_cost(model: &str, tokens_in: u64, tokens_out: u64) -> f64 {
    let (input_per_m, output_per_m) = model_pricing(model);
    (tokens_in as f64 * input_per_m + tokens_out as f64 * output_per_m) / 1_000_000.0
}

fn model_pricing(model: &str) -> (f64, f64) {
    // (input $/M, output $/M)
    // Prices as of 2026-04. Update as needed.
    match model {
        m if m.starts_with("gpt-5.4") => (2.0, 8.0),
        m if m.starts_with("gpt-5.3") => (1.0, 4.0),
        m if m.starts_with("gpt-4o") => (2.5, 10.0),
        m if m.starts_with("gpt-4.1") => (2.0, 8.0),
        m if m.starts_with("o3") => (2.0, 8.0),
        m if m.starts_with("o4-mini") => (1.1, 4.4),
        m if m.contains("claude-sonnet") => (3.0, 15.0),
        m if m.contains("claude-opus") => (15.0, 75.0),
        m if m.contains("claude-haiku") => (0.8, 4.0),
        m if m.starts_with("gemini-2.5-pro") => (1.25, 10.0),
        m if m.starts_with("gemini-2.5-flash") => (0.15, 0.60),
        _ => (3.0, 15.0), // conservative fallback
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum estimate_cost`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/formatting.rs
git commit -m "feat: add cost estimation with per-model pricing table"
```

---

## Task 7: Stats Subcommand

**Why:** Closes the biggest DESIGN.md gap. Reads feedback + telemetry, computes metrics, displays in human/compact/JSON modes.

**Files:**
- Modify: `src/cli/mod.rs` (add `Stats` command variant + `StatsOpts`)
- Create: `src/stats.rs` (stats computation and display)
- Modify: `src/main.rs` (add `mod stats;` and handle Stats command)
- Modify: `src/analytics.rs` (add precision trending)

**Step 1: Add precision trending to analytics**

Write failing tests first in `src/analytics.rs`:

```rust
    #[test]
    fn precision_trend_by_week() {
        use chrono::Duration;
        let now = Utc::now();
        let mut entries = Vec::new();

        // Week 1: 2 TP, 2 FP = 50% precision
        for _ in 0..2 {
            let mut e = entry("model", Verdict::Tp);
            e.timestamp = now - Duration::days(20);
            entries.push(e);
        }
        for _ in 0..2 {
            let mut e = entry("model", Verdict::Fp);
            e.timestamp = now - Duration::days(20);
            entries.push(e);
        }

        // Week 2: 3 TP, 1 FP = 75% precision
        for _ in 0..3 {
            let mut e = entry("model", Verdict::Tp);
            e.timestamp = now - Duration::days(5);
            entries.push(e);
        }
        {
            let mut e = entry("model", Verdict::Fp);
            e.timestamp = now - Duration::days(5);
            entries.push(e);
        }

        let trend = precision_trend(&entries, 7);
        assert!(trend.len() >= 2);
        // Earlier window should be ~0.5, later ~0.75
        let first = trend.first().unwrap();
        let last = trend.last().unwrap();
        assert!((first.precision - 0.5).abs() < 0.1);
        assert!((last.precision - 0.75).abs() < 0.1);
    }

    #[test]
    fn precision_trend_skips_sparse_windows() {
        // Windows with < 10 entries should be excluded
        let entries = vec![entry("model", Verdict::Tp)]; // only 1 entry
        let trend = precision_trend(&entries, 7);
        assert!(trend.is_empty()); // not enough data
    }
```

**Step 2: Implement precision_trend**

In `src/analytics.rs`:

```rust
use chrono::{DateTime, Utc, Duration, Datelike, Weekday, NaiveDate};

#[derive(Debug, Clone)]
pub struct PrecisionWindow {
    pub week_start: DateTime<Utc>,
    pub precision: f64,
    pub count: usize,
}

/// Compute rolling precision over calendar weeks.
/// Requires minimum `min_entries` (default 10) per window to report.
pub fn precision_trend(entries: &[FeedbackEntry], window_days: i64) -> Vec<PrecisionWindow> {
    if entries.is_empty() {
        return vec![];
    }

    let min_entries = 10;
    let mut sorted: Vec<&FeedbackEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.timestamp);

    let first_ts = sorted.first().unwrap().timestamp;
    let last_ts = sorted.last().unwrap().timestamp;
    let mut windows = Vec::new();

    let mut window_start = first_ts;
    while window_start <= last_ts {
        let window_end = window_start + Duration::days(window_days);
        let window_entries: Vec<_> = sorted.iter()
            .filter(|e| e.timestamp >= window_start && e.timestamp < window_end)
            .collect();

        if window_entries.len() >= min_entries {
            let stats = compute_stats(
                &window_entries.iter().map(|e| (*e).clone()).collect::<Vec<_>>()
            );
            let total_tp: usize = stats.values().map(|s| s.tp + s.partial).sum();
            let total_fp: usize = stats.values().map(|s| s.fp).sum();
            let total = total_tp + total_fp;
            let precision = if total > 0 { total_tp as f64 / total as f64 } else { 0.0 };
            windows.push(PrecisionWindow {
                week_start: window_start,
                precision,
                count: window_entries.len(),
            });
        }

        window_start = window_end;
    }

    windows
}
```

**Step 3: Create stats module**

In `src/stats.rs`, implement the stats computation and display logic. This module:
- Loads `feedback.jsonl` and `telemetry.jsonl`
- Computes per-model precision via existing `analytics::compute_stats`
- Computes precision trend via `analytics::precision_trend`
- Aggregates telemetry: total reviews, findings/review, tokens, cost
- Formats in human/compact/JSON per DESIGN.md section 12

```rust
/// Stats dashboard -- reads local data files and computes metrics.

use crate::analytics;
use crate::feedback::FeedbackStore;
use crate::formatting;
use crate::telemetry::TelemetryStore;
use crate::output::Style;

pub struct StatsReport {
    pub feedback_count: usize,
    pub precision: f64,
    pub tp: usize,
    pub fp: usize,
    pub partial: usize,
    pub wontfix: usize,
    pub precision_trend: Vec<analytics::PrecisionWindow>,
    pub reviews_7d: usize,
    pub findings_per_review: f64,
    pub suppression_rate: f64,
    pub tokens_in_7d: u64,
    pub tokens_out_7d: u64,
    pub cost_7d: f64,
    pub tokens_per_finding: f64,
}

pub fn compute_report(
    feedback_store: &FeedbackStore,
    telemetry_store: &TelemetryStore,
) -> anyhow::Result<StatsReport> {
    // Implementation: load both stores, compute all metrics
    // ... (standard aggregation code)
    todo!()
}

pub fn format_human(report: &StatsReport, style: &Style) -> String {
    // Per DESIGN.md section 12 human format
    todo!()
}

pub fn format_compact(report: &StatsReport) -> String {
    // Per DESIGN.md section 12 compact format
    // feedback:2230 precision:0.74 tp:1412 fp:498 trend:0.71>0.74>0.77 ...
    todo!()
}
```

The test approach here follows the anti-pattern guide: unit test the `compute_report` aggregation with synthetic data, and add one CLI integration test for `quorum stats`.

**Step 4: Add CLI wiring**

In `src/cli/mod.rs`:

```rust
#[derive(Subcommand)]
pub enum Command {
    Review(ReviewOpts),
    /// Show feedback and review statistics
    Stats(StatsOpts),
    Serve,
    Daemon(DaemonOpts),
    Version,
}

#[derive(Parser)]
pub struct StatsOpts {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Token-efficient output for LLM consumption
    #[arg(long)]
    pub compact: bool,

    /// Show stats since this date (YYYY-MM-DD, default: all time)
    #[arg(long)]
    pub since: Option<String>,
}
```

In `src/main.rs`, handle the new command:

```rust
cli::Command::Stats(opts) => {
    let home = dirs::home_dir().unwrap_or_default();
    let feedback_store = feedback::FeedbackStore::new(home.join(".quorum/feedback.jsonl"));
    let telemetry_store = telemetry::TelemetryStore::new(home.join(".quorum/telemetry.jsonl"));

    let report = stats::compute_report(&feedback_store, &telemetry_store)?;

    if opts.json {
        // serde_json the report
    } else if opts.compact || std::env::var("CLAUDE_CODE").is_ok() {
        print!("{}", stats::format_compact(&report));
    } else {
        let style = output::Style::detect(false);
        print!("{}", stats::format_human(&report, &style));
    }
    std::process::exit(0);
}
```

**Step 5: Add integration test**

In `tests/cli.rs`:

```rust
#[test]
fn stats_with_no_data() {
    Command::cargo_bin("quorum")
        .unwrap()
        .arg("stats")
        .env("HOME", "/tmp/quorum-test-stats")
        .assert()
        .success();
}
```

**Step 6: Run full test suite**

Run: `cargo test`
Expected: ALL PASS

**Step 7: Commit**

```bash
git add src/stats.rs src/analytics.rs src/cli/mod.rs src/main.rs tests/cli.rs
git commit -m "feat: add quorum stats subcommand with feedback health, activity, and spend metrics"
```

---

## Task 8: Wire Token Usage Through Pipeline

**Why:** Task 2 extracted `TokenUsage` from responses. Now accumulate it through the review pipeline so telemetry captures real token counts instead of zeros.

**Files:**
- Modify: `src/pipeline.rs` (accumulate usage across LLM calls)
- Modify: `src/main.rs` (pass accumulated usage to telemetry entry)

**Step 1: No new TDD** -- this is plumbing that connects Task 2's `TokenUsage` to Task 5's `TelemetryEntry`. The correctness is verified by the existing tests on both ends.

**Step 2: Add usage accumulator**

In the review pipeline, after each LLM call that returns `(content, usage)`:

```rust
let mut total_usage = llm_client::TokenUsage::default();

// After each LLM call:
if let Some(usage) = usage {
    total_usage.prompt_tokens += usage.prompt_tokens;
    total_usage.completion_tokens += usage.completion_tokens;
}
```

**Step 3: Pass to telemetry**

In `main.rs`, update the telemetry entry:

```rust
tokens_in: total_usage.prompt_tokens,
tokens_out: total_usage.completion_tokens,
```

**Step 4: Run full test suite**

Run: `cargo test`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/pipeline.rs src/main.rs
git commit -m "feat: wire token usage accumulation through review pipeline to telemetry"
```

---

## Summary

| Task | Feature | TDD? | Key Files | Depends On |
|------|---------|------|-----------|------------|
| 1 | Numeric formatting | Yes | `src/formatting.rs` | - |
| 2 | Token extraction | Yes | `src/llm_client.rs` | - |
| 3 | Duration tracking | No | `src/main.rs` | - |
| 4 | Compact output | Yes | `src/output/mod.rs`, `src/cli/mod.rs` | - |
| 5 | Telemetry module | Partial | `src/telemetry.rs` | 2, 3 |
| 6 | Cost estimation | Yes | `src/formatting.rs` | - |
| 7 | Stats subcommand | Partial | `src/stats.rs`, `src/analytics.rs`, `src/cli/mod.rs` | 1, 5, 6 |
| 8 | Wire token pipeline | No | `src/pipeline.rs`, `src/main.rs` | 2, 5 |

Tasks 1-4 and 6 are independent and can be parallelized. Tasks 5, 7, 8 have dependencies and must run after their prerequisites.
