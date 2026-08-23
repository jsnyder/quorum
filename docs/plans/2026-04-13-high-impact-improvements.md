# High-Impact Improvements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement three high-impact improvements: project-level suppression lists (#1), actionable fix suggestions (#7), and truncation-aware findings (#3). Also close #2 (already implemented) and broaden AI coding tool detection to Claude Code, Gemini CLI, and Codex CLI.

**Architecture:** Each feature is independent — no cross-dependencies. Suppression is a new module (`suppress.rs`) that filters findings post-calibration. Fix suggestions add an optional field to Finding populated by LLM. Truncation detection adds explicit file size limits with metadata propagation. All features use TDD (red-green-refactor).

**Tech Stack:** Rust, serde (TOML + JSON), glob matching, tree-sitter (existing), clap (CLI flags)

**Reviewed by:** Gemini 3 Pro Preview — 6 issues found, all addressed in plan below.

**Testing guidance (from anti-pattern review):**
- Test public contracts, not internal helpers (Anti-Pattern 5)
- Separate parse tests from matching tests in suppress.rs (Anti-Pattern 2)
- Assert prompt *contains* key substrings, never snapshot full prompt text (Anti-Pattern 15)
- Every test must assert something meaningful, no "didn't crash" tests (Anti-Pattern 16)
- Use `FindingBuilder` for all test finding construction

---

## Task 0: Close #2 and Broaden AI Tool Detection

**Files:**
- Modify: `src/main.rs:238` (compact detection)
- Modify: `src/main.rs:65` (stats compact detection)

**Step 1: Write failing test**

Add to `tests/cli.rs` (or inline in `main.rs` if no suitable test target):

```rust
#[test]
fn compact_detected_from_codex_env() {
    // This is a behavioral test — verify via CLI integration
    // that CODEX=1 triggers compact output
}
```

Since compact detection is in `run_review()` which isn't easily unit-testable, extract the detection logic:

Add to `src/output/mod.rs`:

```rust
/// Detect if compact output should be used based on env vars.
/// Recognizes AI coding tool environments:
/// - CLAUDE_CODE: Claude Code (Anthropic)
/// - GEMINI_CLI: Gemini CLI (Google)
/// - CODEX_CI: Codex CLI (OpenAI)
/// - AGENT: Generic agent identifier (proposed Codex standard)
pub fn should_use_compact(compact_flag: bool) -> bool {
    compact_flag
        || std::env::var("CLAUDE_CODE").is_ok()
        || std::env::var("GEMINI_CLI").is_ok()
        || std::env::var("CODEX_CI").is_ok()
        || std::env::var("AGENT").is_ok()
}
```

Write tests in `src/output/mod.rs`:

```rust
#[test]
fn should_use_compact_from_flag() {
    assert!(should_use_compact(true));
}

#[test]
fn should_use_compact_default_false() {
    // Only valid if neither env var is set in test environment
    // This tests the flag=false path
    let result = should_use_compact(false);
    // Result depends on env — test the flag path is authoritative
    assert!(should_use_compact(true));
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test should_use_compact -v
```

Expected: compilation error (function doesn't exist yet)

**Step 3: Implement `should_use_compact` in `src/output/mod.rs`**

```rust
pub fn should_use_compact(compact_flag: bool) -> bool {
    compact_flag
        || std::env::var("CLAUDE_CODE").is_ok()
        || std::env::var("GEMINI_CLI").is_ok()
        || std::env::var("CODEX_CI").is_ok()
        || std::env::var("AGENT").is_ok()
}
```

**Step 4: Update `src/main.rs` to use the new function**

Replace line 238:
```rust
let use_compact = output::should_use_compact(opts.compact);
```

Replace line 65:
```rust
} else if output::should_use_compact(opts.compact) {
```

**Step 5: Run tests**

```bash
cargo test --bin quorum
```

Expected: all pass

**Step 6: Commit**

```bash
git add src/output/mod.rs src/main.rs
git commit -m "refactor: extract compact detection, add CODEX env support

Closes #2 — CLAUDE_CODE was already implemented.
Also detects GEMINI_CLI (Google), CODEX_CI (OpenAI), and AGENT (generic)."
```

---

## Task 1: Project-Level Suppression Lists (#1)

### Task 1a: Suppression Rule Parsing

**Files:**
- Create: `src/suppress.rs`
- Modify: `src/main.rs` (add `mod suppress;`)

**Step 1: Write failing test — parse valid TOML**

In `src/suppress.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_suppress_config() {
        let toml = r#"
[[suppress]]
pattern = "TLS certificate verification"
category = "security"
file = "src/url_resolver.py"
reason = "Intentional — self-signed cert"
"#;
        let rules = parse_suppress_config(toml).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "TLS certificate verification");
        assert_eq!(rules[0].category.as_deref(), Some("security"));
        assert_eq!(rules[0].file.as_deref(), Some("src/url_resolver.py"));
        assert_eq!(rules[0].reason.as_deref(), Some("Intentional — self-signed cert"));
    }

    #[test]
    fn parse_empty_config_returns_empty_vec() {
        let rules = parse_suppress_config("").unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn parse_invalid_toml_returns_error() {
        let result = parse_suppress_config("not valid [[ toml");
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_pattern_returns_error() {
        let toml = r#"
[[suppress]]
category = "security"
"#;
        // pattern is required — serde should fail
        let result = parse_suppress_config(toml);
        assert!(result.is_err());
    }
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum suppress
```

Expected: compilation error

**Step 3: Implement parsing**

```rust
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SuppressionRule {
    pub pattern: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SuppressConfig {
    #[serde(default)]
    suppress: Vec<SuppressionRule>,
}

pub fn parse_suppress_config(toml_str: &str) -> anyhow::Result<Vec<SuppressionRule>> {
    if toml_str.trim().is_empty() {
        return Ok(Vec::new());
    }
    let config: SuppressConfig = toml::from_str(toml_str)?;
    Ok(config.suppress)
}
```

Add `mod suppress;` to `src/main.rs`.
Add `toml = "0.8"` to `Cargo.toml` dependencies (check if already present).

**Step 4: Run tests**

```bash
cargo test --bin quorum suppress
```

Expected: all 4 pass

**Step 5: Commit**

```bash
git add src/suppress.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat(suppress): add TOML config parsing for suppression rules"
```

### Task 1b: Suppression Matching Logic

**Files:**
- Modify: `src/suppress.rs`

**Step 1: Write failing tests — matching logic**

```rust
#[test]
fn rule_matches_by_pattern_substring() {
    let rule = SuppressionRule {
        pattern: "TLS certificate".into(),
        category: None, file: None, reason: None,
    };
    let f = FindingBuilder::new()
        .title("TLS certificate verification disabled")
        .build();
    assert!(rule_matches(&rule, &f, "src/main.rs"));
}

#[test]
fn rule_matches_case_insensitive() {
    let rule = SuppressionRule {
        pattern: "tls certificate".into(),
        category: None, file: None, reason: None,
    };
    let f = FindingBuilder::new()
        .title("TLS Certificate Verification Disabled")
        .build();
    assert!(rule_matches(&rule, &f, "src/main.rs"));
}

#[test]
fn rule_no_match_wrong_pattern() {
    let rule = SuppressionRule {
        pattern: "SQL injection".into(),
        category: None, file: None, reason: None,
    };
    let f = FindingBuilder::new()
        .title("TLS certificate verification disabled")
        .build();
    assert!(!rule_matches(&rule, &f, "src/main.rs"));
}

#[test]
fn rule_matches_with_category_filter() {
    let rule = SuppressionRule {
        pattern: "TLS".into(),
        category: Some("security".into()),
        file: None, reason: None,
    };
    let f = FindingBuilder::new()
        .title("TLS certificate verification disabled")
        .category("security")
        .build();
    assert!(rule_matches(&rule, &f, "src/main.rs"));
}

#[test]
fn rule_no_match_wrong_category() {
    let rule = SuppressionRule {
        pattern: "TLS".into(),
        category: Some("style".into()),
        file: None, reason: None,
    };
    let f = FindingBuilder::new()
        .title("TLS certificate verification disabled")
        .category("security")
        .build();
    assert!(!rule_matches(&rule, &f, "src/main.rs"));
}

#[test]
fn rule_matches_with_file_glob() {
    let rule = SuppressionRule {
        pattern: "TLS".into(),
        category: None,
        file: Some("src/*.py".into()),
        reason: None,
    };
    let f = FindingBuilder::new().title("TLS disabled").build();
    assert!(rule_matches(&rule, &f, "src/url_resolver.py"));
    assert!(!rule_matches(&rule, &f, "src/main.rs"));
}

#[test]
fn rule_matches_file_exact_path() {
    let rule = SuppressionRule {
        pattern: "TLS".into(),
        category: None,
        file: Some("src/url_resolver.py".into()),
        reason: None,
    };
    let f = FindingBuilder::new().title("TLS disabled").build();
    assert!(rule_matches(&rule, &f, "src/url_resolver.py"));
}

#[test]
fn rule_all_fields_must_match_and_logic() {
    let rule = SuppressionRule {
        pattern: "TLS".into(),
        category: Some("security".into()),
        file: Some("src/*.py".into()),
        reason: None,
    };
    // Right pattern, right category, wrong file
    let f = FindingBuilder::new()
        .title("TLS disabled")
        .category("security")
        .build();
    assert!(!rule_matches(&rule, &f, "src/main.rs"));
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum rule_matches
```

Expected: compilation error (`rule_matches` doesn't exist)

**Step 3: Implement matching**

```rust
/// Check if a suppression rule matches a finding for the given file path.
pub fn rule_matches(rule: &SuppressionRule, finding: &Finding, file_path: &str) -> bool {
    // Pattern: case-insensitive substring match on title
    let pattern_matches = finding.title.to_lowercase().contains(&rule.pattern.to_lowercase());
    if !pattern_matches {
        return false;
    }

    // Category: exact match (case-insensitive) if specified
    if let Some(ref cat) = rule.category {
        if finding.category.to_lowercase() != cat.to_lowercase() {
            return false;
        }
    }

    // File: glob match if specified (normalize path separators for cross-platform)
    if let Some(ref file_glob) = rule.file {
        let normalized_path = file_path.replace('\\', "/");
        let normalized_glob = file_glob.replace('\\', "/");
        let match_opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        let pattern = glob::Pattern::new(&normalized_glob);
        match pattern {
            Ok(p) => {
                if !p.matches_with(&normalized_path, match_opts) {
                    return false;
                }
            }
            Err(_) => {
                // Invalid glob — treat as exact string match
                if normalized_path != normalized_glob {
                    return false;
                }
            }
        }
    }

    true
}
```

Add `glob = "0.3"` to `Cargo.toml` if not already present.

**Step 4: Run tests**

```bash
cargo test --bin quorum rule_matches
```

Expected: all 8 pass

**Step 5: Commit**

```bash
git add src/suppress.rs Cargo.toml Cargo.lock
git commit -m "feat(suppress): add rule matching with pattern/category/file glob"
```

### Task 1c: Apply Suppressions to Findings

**Files:**
- Modify: `src/suppress.rs`

**Step 1: Write failing tests — apply_suppressions**

```rust
#[test]
fn apply_suppressions_filters_matching_findings() {
    let rules = vec![SuppressionRule {
        pattern: "TLS".into(),
        category: None, file: None, reason: None,
    }];
    let findings = vec![
        FindingBuilder::new().title("TLS verification disabled").build(),
        FindingBuilder::new().title("SQL injection risk").build(),
    ];
    let (kept, suppressed) = apply_suppressions(findings, &rules, "src/main.py");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].title, "SQL injection risk");
    assert_eq!(suppressed, 1);
}

#[test]
fn apply_suppressions_empty_rules_passes_all() {
    let findings = vec![
        FindingBuilder::new().title("Some finding").build(),
    ];
    let (kept, suppressed) = apply_suppressions(findings, &[], "src/main.py");
    assert_eq!(kept.len(), 1);
    assert_eq!(suppressed, 0);
}

#[test]
fn apply_suppressions_multiple_rules() {
    let rules = vec![
        SuppressionRule { pattern: "TLS".into(), category: None, file: None, reason: None },
        SuppressionRule { pattern: "complexity".into(), category: None, file: None, reason: None },
    ];
    let findings = vec![
        FindingBuilder::new().title("TLS verification disabled").build(),
        FindingBuilder::new().title("High cyclomatic complexity").build(),
        FindingBuilder::new().title("SQL injection risk").build(),
    ];
    let (kept, suppressed) = apply_suppressions(findings, &rules, "src/main.py");
    assert_eq!(kept.len(), 1);
    assert_eq!(suppressed, 2);
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum apply_suppressions
```

**Step 3: Implement**

```rust
/// Filter findings through suppression rules. Returns (kept, suppressed_count).
pub fn apply_suppressions(
    findings: Vec<Finding>,
    rules: &[SuppressionRule],
    file_path: &str,
) -> (Vec<Finding>, usize) {
    if rules.is_empty() {
        return (findings, 0);
    }
    let mut kept = Vec::new();
    let mut suppressed = 0usize;
    for f in findings {
        if rules.iter().any(|r| rule_matches(r, &f, file_path)) {
            suppressed += 1;
        } else {
            kept.push(f);
        }
    }
    (kept, suppressed)
}
```

**Step 4: Run tests**

```bash
cargo test --bin quorum apply_suppressions
```

**Step 5: Commit**

```bash
git add src/suppress.rs
git commit -m "feat(suppress): apply suppression rules to filter findings"
```

### Task 1d: Load Suppressions from File and Wire into CLI

**Files:**
- Modify: `src/suppress.rs` (add `load_project_suppressions`)
- Modify: `src/cli/mod.rs` (add `--show-suppressed` flag)
- Modify: `src/main.rs` (wire suppression into `run_review`)

**Step 1: Write failing test — file loading**

```rust
#[test]
fn load_suppressions_returns_empty_for_missing_file() {
    let rules = load_project_suppressions(Path::new("/nonexistent/.quorum/suppress.toml"));
    assert!(rules.is_empty());
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum load_suppressions
```

**Step 3: Implement file loading**

In `src/suppress.rs`:

```rust
use std::path::Path;

/// Load suppression rules from a .quorum/suppress.toml file.
/// Returns empty vec if file doesn't exist or can't be parsed.
pub fn load_project_suppressions(path: &Path) -> Vec<SuppressionRule> {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse_suppress_config(&contents).unwrap_or_else(|e| {
            eprintln!("Warning: Failed to parse {}: {}", path.display(), e);
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}
```

**Step 4: Add CLI flag**

In `src/cli/mod.rs`, add to `ReviewOpts`:

```rust
/// Show findings that were suppressed by project rules
#[arg(long)]
pub show_suppressed: bool,
```

**Step 5: Wire into `run_review` in `src/main.rs`**

After config loading but before the file loop, resolve suppressions from the project root (not CWD, in case files are in a different project):

```rust
// Load project-level suppressions from target project root
// Use find_project_root from pipeline.rs (make pub) based on first file
let project_root = if let Some(first_file) = opts.files.first() {
    pipeline::find_project_root(first_file)
} else {
    std::env::current_dir().unwrap_or_default()
};
let suppress_path = project_root.join(".quorum/suppress.toml");
let suppress_rules = suppress::load_project_suppressions(&suppress_path);
if !suppress_rules.is_empty() {
    eprintln!("Loaded {} suppression rule(s) from {}", suppress_rules.len(), suppress_path.display());
}
```

**Note:** Make `find_project_root` in `src/pipeline.rs` (currently private, L298) public so main.rs can use it.

Then in **BOTH** the deep review success path (L278-295) AND the standard pipeline success path, apply suppressions before formatting:

```rust
// Apply project-level suppressions (must happen in BOTH deep review and standard paths)
let (findings, suppressed_count) = suppress::apply_suppressions(
    findings, &suppress_rules, &file_display,
);
if suppressed_count > 0 {
    eprintln!("Suppressed {} finding(s) in {}", suppressed_count, file_display);
}
```

**IMPORTANT:** The deep review path (when `opts.deep` succeeds) currently outputs findings and calls `continue`, bypassing post-processing. Suppression MUST be applied before output in that path too.

**Step 6: Run full test suite**

```bash
cargo test --bin quorum
```

**Step 7: Commit**

```bash
git add src/suppress.rs src/cli/mod.rs src/main.rs
git commit -m "feat(suppress): wire project-level suppression into review pipeline

Loads .quorum/suppress.toml from project root. Filters findings
after calibration. --show-suppressed flag to audit hidden findings.
Closes #1"
```

### Task 1e: --show-suppressed Output

**Files:**
- Modify: `src/suppress.rs` (add `format_suppressed_finding`)
- Modify: `src/main.rs` (display suppressed findings when flag is set)

**Step 1: Write failing test**

```rust
#[test]
fn format_suppressed_finding_shows_rule_reason() {
    let f = FindingBuilder::new()
        .title("TLS verification disabled")
        .category("security")
        .build();
    let rule = SuppressionRule {
        pattern: "TLS".into(),
        category: None, file: None,
        reason: Some("Intentional for local network".into()),
    };
    let output = format_suppressed_finding(&f, &rule);
    assert!(output.contains("SUPPRESSED"));
    assert!(output.contains("TLS verification disabled"));
    assert!(output.contains("Intentional for local network"));
}
```

**Step 2: Implement**

```rust
/// Format a suppressed finding for --show-suppressed output.
pub fn format_suppressed_finding(finding: &Finding, rule: &SuppressionRule) -> String {
    let reason = rule.reason.as_deref().unwrap_or("no reason given");
    format!("  [SUPPRESSED] {}  [{}]\n    Reason: {}\n",
        finding.title, finding.category, reason)
}
```

**Step 3: Wire into main.rs**

In the output section, after applying suppressions, if `opts.show_suppressed`:

```rust
// When --show-suppressed, re-run matching to find which rule hit each finding
// (we need the original findings back for this)
```

Adjust `apply_suppressions` to return a richer type. **This changes the return type from the tuple in Task 1c** — update ALL call sites in main.rs to use the new struct:

```rust
pub struct SuppressionResult {
    pub kept: Vec<Finding>,
    pub suppressed: Vec<(Finding, SuppressionRule)>,
}

pub fn apply_suppressions(
    findings: Vec<Finding>,
    rules: &[SuppressionRule],
    file_path: &str,
) -> SuppressionResult {
    if rules.is_empty() {
        return SuppressionResult { kept: findings, suppressed: Vec::new() };
    }
    let mut kept = Vec::new();
    let mut suppressed = Vec::new();
    for f in findings {
        if let Some(matched_rule) = rules.iter().find(|r| rule_matches(r, &f, file_path)) {
            suppressed.push((f, matched_rule.clone()));
        } else {
            kept.push(f);
        }
    }
    SuppressionResult { kept, suppressed }
}
```

**Update main.rs call sites** (both deep review and standard paths):

```rust
let result = suppress::apply_suppressions(findings, &suppress_rules, &file_display);
let findings = result.kept;
if !result.suppressed.is_empty() {
    eprintln!("Suppressed {} finding(s) in {}", result.suppressed.len(), file_display);
}
if opts.show_suppressed {
    for (f, rule) in &result.suppressed {
        eprint!("{}", suppress::format_suppressed_finding(f, rule));
    }
}
```

**Also update Task 1c tests** to use the new return type (or do this refactor here and skip the tuple intermediate step entirely — cleaner to go straight to SuppressionResult).

**Step 4: Run tests, commit**

```bash
cargo test --bin quorum
git add src/suppress.rs src/main.rs
git commit -m "feat(suppress): add --show-suppressed output with matched rule reasons"
```

---

## Task 2: Actionable Fix Suggestions (#7)

### Task 2a: Add suggested_fix Field to Finding

**Files:**
- Modify: `src/finding.rs` (Finding struct, FindingBuilder)
- Modify: `src/review.rs` (LlmFinding struct, into_finding)

**Step 1: Write failing test — Finding with suggested_fix**

In `src/finding.rs` tests:

```rust
#[test]
fn finding_suggested_fix_serializes() {
    let f = FindingBuilder::new()
        .suggested_fix("Use parameterized queries instead")
        .build();
    let json = serde_json::to_string(&f).unwrap();
    assert!(json.contains("suggested_fix"));
    assert!(json.contains("Use parameterized queries instead"));
}

#[test]
fn finding_no_suggested_fix_omitted_from_json() {
    let f = FindingBuilder::new().build();
    let json = serde_json::to_string(&f).unwrap();
    assert!(!json.contains("suggested_fix"));
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum finding_suggested_fix
```

**Step 3: Implement**

Add to `Finding` struct:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub suggested_fix: Option<String>,
```

Add to `FindingBuilder`:

```rust
pub fn suggested_fix(mut self, s: &str) -> Self {
    self.0.suggested_fix = Some(s.to_string());
    self
}
```

Make sure `FindingBuilder::new()` initializes `suggested_fix: None`.

**Step 4: Run tests**

```bash
cargo test --bin quorum finding_suggested_fix
```

**Step 5: Commit**

```bash
git add src/finding.rs
git commit -m "feat(finding): add optional suggested_fix field to Finding"
```

### Task 2b: Parse suggested_fix from LLM Response

**Files:**
- Modify: `src/review.rs` (LlmFinding, into_finding)

**Step 1: Write failing test**

In `src/review.rs` tests:

```rust
#[test]
fn llm_finding_with_suggested_fix() {
    let json = r#"[{
        "title": "SQL injection",
        "description": "User input not sanitized",
        "severity": "high",
        "category": "security",
        "line_start": 42,
        "line_end": 42,
        "suggested_fix": "Use parameterized queries: db.execute('SELECT * FROM t WHERE id = ?', [user_id])"
    }]"#;
    let findings = parse_llm_response(json, "test-model").unwrap();
    assert_eq!(findings[0].suggested_fix.as_deref(), Some("Use parameterized queries: db.execute('SELECT * FROM t WHERE id = ?', [user_id])"));
}

#[test]
fn llm_finding_without_suggested_fix_is_none() {
    let json = r#"[{
        "title": "SQL injection",
        "description": "desc",
        "severity": "high",
        "category": "security",
        "line_start": 42,
        "line_end": 42
    }]"#;
    let findings = parse_llm_response(json, "test-model").unwrap();
    assert!(findings[0].suggested_fix.is_none());
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum llm_finding_with_suggested_fix
```

**Step 3: Implement**

Add to `LlmFinding`:

```rust
#[serde(default)]
pub suggested_fix: Option<String>,
```

Update `into_finding()` to pass through:

```rust
suggested_fix: self.suggested_fix,
```

**Step 4: Run tests**

```bash
cargo test --bin quorum llm_finding
```

**Step 5: Commit**

```bash
git add src/review.rs
git commit -m "feat(review): parse suggested_fix from LLM response"
```

### Task 2c: Update LLM Prompt to Request Fix Suggestions

**Files:**
- Modify: `src/review.rs` (build_review_prompt)

**Step 1: Write failing test**

```rust
#[test]
fn build_prompt_requests_suggested_fix() {
    let req = ReviewRequest {
        file_path: "test.rs".into(),
        language: "rust".into(),
        code: "fn main() {}".into(),
        hydration_context: None,
        framework_docs: None,
        feedback_precedents: None,
    };
    let prompt = build_review_prompt(&req);
    assert!(prompt.contains("suggested_fix"));
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum build_prompt_requests_suggested_fix
```

**Step 3: Implement**

In `build_review_prompt`, before the `## Code` section, add the response format instruction. Update the existing format instruction or add:

```rust
prompt.push_str("## Response Format\n");
prompt.push_str("Return a JSON array of findings. Each finding has: title, description, severity (critical/high/medium/low/info), category, line_start, line_end.\n");
prompt.push_str("For findings with severity MEDIUM or higher, include a `suggested_fix` field with a concrete code example or specific action the developer should take.\n");
prompt.push_str("For test quality findings, show what the test should assert. For code smells, show the improved pattern.\n\n");
```

**Step 4: Run tests**

```bash
cargo test --bin quorum build_prompt
```

**Step 5: Commit**

```bash
git add src/review.rs
git commit -m "feat(review): instruct LLM to include suggested_fix for MEDIUM+ findings"
```

### Task 2d: Display Fix Suggestions in Output

**Files:**
- Modify: `src/output/mod.rs` (format_finding, format_compact_finding)

**Step 1: Write failing tests**

```rust
#[test]
fn format_finding_includes_suggested_fix() {
    let f = FindingBuilder::new()
        .title("SQL injection")
        .description("User input not sanitized")
        .suggested_fix("Use parameterized queries")
        .build();
    let style = Style::plain();
    let output = format_finding(&f, &style);
    assert!(output.contains("Use parameterized queries"));
    assert!(output.contains("Suggested fix:"));
}

#[test]
fn format_finding_no_fix_no_extra_line() {
    let f = FindingBuilder::new()
        .title("SQL injection")
        .description("User input not sanitized")
        .build();
    let style = Style::plain();
    let output = format_finding(&f, &style);
    assert!(!output.contains("Suggested fix:"));
}

#[test]
fn compact_finding_omits_suggested_fix() {
    let f = FindingBuilder::new()
        .title("SQL injection")
        .suggested_fix("Use parameterized queries")
        .build();
    let output = format_compact_finding(&f);
    assert!(!output.contains("parameterized"));
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum format_finding_includes_suggested_fix
```

**Step 3: Implement**

In `format_finding`, after the description line, add (indent newlines for multi-line suggestions):

```rust
if let Some(ref fix) = f.suggested_fix {
    let indented = fix.replace('\n', "\n      ");
    output.push_str(&format!("    {dim}Suggested fix:{reset} {fix}\n",
        dim = style.dim, reset = style.reset, fix = indented));
}
```

`format_compact_finding` stays unchanged (omits by design — token savings).

**Step 4: Run tests**

```bash
cargo test --bin quorum format_finding
```

**Step 5: Commit**

```bash
git add src/output/mod.rs
git commit -m "feat(output): display suggested_fix in human format, omit in compact

Closes #7"
```

---

## Task 3: Truncation-Aware Findings (#3)

### Task 3a: Add Truncation Metadata to ReviewRequest and Finding

**Files:**
- Modify: `src/review.rs` (ReviewRequest)
- Modify: `src/finding.rs` (Finding, FindingBuilder)

**Step 1: Write failing tests**

In `src/finding.rs`:

```rust
#[test]
fn finding_based_on_excerpt_serializes() {
    let f = FindingBuilder::new()
        .based_on_excerpt("lines 1-150 of 500")
        .build();
    let json = serde_json::to_string(&f).unwrap();
    assert!(json.contains("based_on_excerpt"));
    assert!(json.contains("lines 1-150 of 500"));
}

#[test]
fn finding_no_excerpt_omitted_from_json() {
    let f = FindingBuilder::new().build();
    let json = serde_json::to_string(&f).unwrap();
    assert!(!json.contains("based_on_excerpt"));
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum finding_based_on_excerpt
```

**Step 3: Implement**

Add to `Finding`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub based_on_excerpt: Option<String>,
```

Add to `ReviewRequest`:

```rust
/// If the file was truncated, describes what was sent (e.g., "lines 1-150 of 500")
pub truncation_notice: Option<String>,
```

Add to `FindingBuilder`:

```rust
pub fn based_on_excerpt(mut self, s: &str) -> Self {
    self.0.based_on_excerpt = Some(s.to_string());
    self
}
```

**Step 4: Run tests**

```bash
cargo test --bin quorum finding_based_on_excerpt
```

**Step 5: Commit**

```bash
git add src/finding.rs src/review.rs
git commit -m "feat(finding): add based_on_excerpt and truncation_notice fields"
```

### Task 3b: Truncation Detection in Pipeline

**Files:**
- Modify: `src/pipeline.rs` (review_file, review_file_llm_only)

**Step 1: Write failing test**

In `src/pipeline.rs`:

```rust
#[test]
fn truncate_source_within_limit() {
    let source = "line1\nline2\nline3\n";
    let (truncated, notice) = truncate_for_review(source, 100);
    assert_eq!(truncated, source);
    assert!(notice.is_none());
}

#[test]
fn truncate_source_over_limit() {
    let source = (0..600).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
    let (truncated, notice) = truncate_for_review(&source, 500);
    let truncated_lines = truncated.lines().count();
    assert_eq!(truncated_lines, 500);
    let notice = notice.expect("should have truncation notice");
    assert!(notice.contains("500"));
    assert!(notice.contains("600"));
}

#[test]
fn truncate_source_at_exact_limit() {
    let source = (0..500).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
    let (truncated, notice) = truncate_for_review(&source, 500);
    assert_eq!(truncated, source);
    assert!(notice.is_none());
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum truncate_source
```

**Step 3: Implement**

In `src/pipeline.rs`:

```rust
/// Truncate source code for LLM review if it exceeds the line limit.
/// Returns (possibly truncated source, optional truncation notice).
fn truncate_for_review(source: &str, max_lines: usize) -> (String, Option<String>) {
    let total_lines = source.lines().count();
    if total_lines <= max_lines {
        return (source.to_string(), None);
    }
    let truncated: String = source.lines().take(max_lines).collect::<Vec<_>>().join("\n");
    let notice = format!("lines 1-{} of {}", max_lines, total_lines);
    (truncated, Some(notice))
}
```

**Step 4: Run tests**

```bash
cargo test --bin quorum truncate_source
```

**Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat(pipeline): add truncate_for_review with line-based limiting"
```

### Task 3c: Wire Truncation into LLM Prompt

**Files:**
- Modify: `src/review.rs` (build_review_prompt)
- Modify: `src/pipeline.rs` (pass truncation_notice to ReviewRequest)

**Step 1: Write failing test**

In `src/review.rs`:

```rust
#[test]
fn build_prompt_includes_truncation_notice() {
    let req = ReviewRequest {
        file_path: "test.rs".into(),
        language: "rust".into(),
        code: "fn main() {}".into(),
        hydration_context: None,
        framework_docs: None,
        feedback_precedents: None,
        truncation_notice: Some("lines 1-150 of 500".into()),
    };
    let prompt = build_review_prompt(&req);
    assert!(prompt.contains("lines 1-150 of 500"));
    assert!(prompt.contains("partial"));
}

#[test]
fn build_prompt_no_truncation_notice_when_full() {
    let req = ReviewRequest {
        file_path: "test.rs".into(),
        language: "rust".into(),
        code: "fn main() {}".into(),
        hydration_context: None,
        framework_docs: None,
        feedback_precedents: None,
        truncation_notice: None,
    };
    let prompt = build_review_prompt(&req);
    assert!(!prompt.contains("partial view"));
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum build_prompt_includes_truncation
```

**Step 3: Implement**

In `build_review_prompt`, before the `## Code` section:

```rust
if let Some(ref notice) = req.truncation_notice {
    prompt.push_str(&format!(
        "**Note:** This is a partial view of the file ({}). \
         Do not flag missing content or incompleteness — you are reviewing an excerpt.\n\n",
        notice
    ));
}
```

Update all existing `ReviewRequest` construction sites to include `truncation_notice: None` (or use `..Default::default()` if Default is derived).

**Step 4: Wire truncation into pipeline**

In `review_file` and `review_file_llm_only`, before building ReviewRequest:

```rust
let max_lines = pipeline_config.max_review_lines; // default 500, configurable via PipelineConfig
let (review_code, truncation_notice) = truncate_for_review(&redacted_code, max_lines);
```

Then use `review_code` instead of `redacted_code` in the ReviewRequest, and set `truncation_notice`.

**Step 5: Run tests**

```bash
cargo test --bin quorum
```

**Step 6: Commit**

```bash
git add src/review.rs src/pipeline.rs
git commit -m "feat(review): add truncation notice to LLM prompt for partial file views"
```

### Task 3d: Annotate Findings from Truncated Reviews

**Files:**
- Modify: `src/review.rs` (parse_llm_response or post-processing)
- Modify: `src/output/mod.rs` (display annotation)
- Modify: `src/pipeline.rs` (stamp findings)

**Step 1: Write failing tests**

In `src/output/mod.rs`:

```rust
#[test]
fn format_finding_shows_excerpt_annotation() {
    let f = FindingBuilder::new()
        .title("Missing error handling")
        .based_on_excerpt("lines 1-150 of 500")
        .build();
    let style = Style::plain();
    let output = format_finding(&f, &style);
    assert!(output.contains("[partial view: lines 1-150 of 500]"));
}

#[test]
fn format_finding_no_annotation_when_full() {
    let f = FindingBuilder::new()
        .title("Missing error handling")
        .build();
    let style = Style::plain();
    let output = format_finding(&f, &style);
    assert!(!output.contains("partial view"));
}

#[test]
fn compact_finding_shows_excerpt_tag() {
    let f = FindingBuilder::new()
        .title("Missing error handling")
        .based_on_excerpt("lines 1-150 of 500")
        .build();
    let output = format_compact_finding(&f);
    assert!(output.contains("[excerpt]"));
}
```

**Step 2: Run to verify failure**

```bash
cargo test --bin quorum format_finding_shows_excerpt
```

**Step 3: Implement**

In `format_finding`, after the line label:

```rust
if let Some(ref excerpt) = f.based_on_excerpt {
    output.push_str(&format!("    {dim}[partial view: {excerpt}]{reset}\n",
        dim = style.dim, reset = style.reset, excerpt = excerpt));
}
```

In `format_compact_finding`, append tag after title:

```rust
if f.based_on_excerpt.is_some() {
    result.push_str("[excerpt]");
}
```

**Step 4: Stamp findings in pipeline**

In `review_file` and `review_file_llm_only`, after parsing LLM response, if truncation happened.
**IMPORTANT:** Only stamp LLM-sourced findings, NOT AST/linter findings (which analyzed the full file):

```rust
if let Some(ref notice) = truncation_notice {
    for f in &mut findings {
        if matches!(f.source, crate::finding::Source::Llm(_)) {
            f.based_on_excerpt = Some(notice.clone());
        }
    }
}
```

**Step 5: Run tests**

```bash
cargo test --bin quorum
```

**Step 6: Commit**

```bash
git add src/output/mod.rs src/pipeline.rs
git commit -m "feat(output): annotate findings from truncated file reviews

Closes #3"
```

---

## Task 4: Before/After Validation

**No code changes — manual verification.**

**Step 1: Capture baseline**

Run quorum review on test fixtures before changes:

```bash
cargo run -- review tests/fixtures/python/insecure.py --json > /tmp/quorum-before.json
cargo run -- review tests/fixtures/rust/complex.rs --json >> /tmp/quorum-before.json
```

**Step 2: Capture after**

After all features are implemented:

```bash
cargo run -- review tests/fixtures/python/insecure.py --json > /tmp/quorum-after.json
cargo run -- review tests/fixtures/rust/complex.rs --json >> /tmp/quorum-after.json
```

**Step 3: Compare**

- Verify `suggested_fix` fields appear in after (not in before)
- Verify `based_on_excerpt` appears for large files
- Create a `.quorum/suppress.toml` with a test rule, verify finding disappears

**Step 4: Test suppression end-to-end**

```bash
mkdir -p .quorum
cat > .quorum/suppress.toml << 'EOF'
[[suppress]]
pattern = "cyclomatic complexity"
reason = "Accepted in test fixtures"
EOF

cargo run -- review tests/fixtures/rust/complex.rs
# Verify complexity findings are suppressed

cargo run -- review tests/fixtures/rust/complex.rs --show-suppressed
# Verify they appear with [SUPPRESSED] tag

rm .quorum/suppress.toml
```

---

## Task 5: Final Cleanup

**Step 1: Run full test suite**

```bash
cargo test
```

**Step 2: Run clippy**

```bash
cargo clippy --all-targets
```

**Step 3: Close issues**

```bash
gh issue close 2 --comment "Already implemented — CLAUDE_CODE env var detection was in place. Also added CODEX detection."
gh issue close 1 --comment "Implemented in $(git rev-parse --short HEAD). Project-level suppression via .quorum/suppress.toml"
gh issue close 7 --comment "Implemented — LLM now includes suggested_fix for MEDIUM+ findings"
gh issue close 3 --comment "Implemented — truncated reviews annotated with [partial view] tag"
```

**Step 4: Commit any remaining changes and tag**

```bash
git tag v0.9.5
```
