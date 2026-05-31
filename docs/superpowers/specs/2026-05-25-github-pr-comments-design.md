# Design: Native GitHub PR Comment Support

**Issue:** #313
**Date:** 2026-05-25
**Version target:** 0.27.0

## Summary

Add native support for posting review findings as GitHub PR review comments,
enabling CI integration without wrapper scripts. Two entry points: a standalone
`quorum report` subcommand for two-stage CI (Stage 2), and a `--github-pr`
convenience flag on `quorum review` for single-stage privileged contexts.

## Architecture

A single new module `src/github_report.rs` owns all GitHub API interaction.
Both entry points call into it with the same `PostReviewRequest` struct.

```
review pipeline -> Vec<Finding> -> stdout (Human/Compact/JSON, unchanged)
                                \-> github_report module -> GitHub API (new)
```

The existing output flow is untouched. GitHub posting is a side-effect that
happens after stdout output is written.

### Module: `src/github_report.rs`

Public API:

```rust
pub struct PostReviewRequest {
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
    pub token: String,
    pub findings: Vec<Finding>,
    pub diff_text: String,       // PR diff for commentability validation
    pub version: String,         // quorum version for marker
    pub run_id: String,          // ULID for marker traceability
    pub commit_sha: String,      // PR HEAD for commit_id field
}

pub struct PostReviewResult {
    pub review_id: u64,
    pub inline_count: usize,
    pub body_count: usize,
    pub dismissed_previous: Option<u64>,
}

pub async fn post_review(
    client: &reqwest::Client,
    req: &PostReviewRequest,
) -> Result<PostReviewResult, GitHubReportError>;
```

### Internal pipeline within `post_review`:

1. **Classify posting targets** — parse `diff_text` into hunk ranges per file,
   map each finding to `(path, anchor_line, side)`, verify the line falls
   within a commentable hunk range. Findings that fail validation are
   downgraded to the review body.

2. **Dismiss previous review** — list existing reviews via
   `GET /repos/{owner}/{repo}/pulls/{pr}/reviews`, scan for the
   `quorum-review-marker` in the body, dismiss via
   `PUT .../reviews/{id}/dismissals`. Best-effort: 403/404 on dismiss is
   logged to stderr, not fatal. The new review is posted regardless.

3. **Sanitize all text** — run `sanitize_for_github()` on every finding
   title, description, suggested_fix, and evidence entry.

4. **Build review payload** — construct the GitHub review API request:
   - `event: "COMMENT"`
   - `commit_id`: PR HEAD SHA
   - `body`: summary + out-of-diff findings + marker
   - `comments[]`: inline comments for diff-visible findings

5. **Post review** — `POST /repos/{owner}/{repo}/pulls/{pr}/reviews`

## CLI Surface

### `quorum report` subcommand

```
quorum report <findings-file | -> --pr <number> [options]
```

| Flag | Default | Description |
|------|---------|-------------|
| `<file>` or `-` | required | JSON findings file path, or `-` for stdin |
| `--pr <N>` | required | PR number to post to |
| `--github-token` | `GITHUB_TOKEN` env | Override authentication token |
| `--github-repo` | auto-detect | Override `owner/repo` (format: `owner/repo`) |
| `--diff-file` | fetch from GitHub | Local diff file; if omitted, fetched from PR API |

**Repo auto-detection order:**
1. `--github-repo` flag
2. `GITHUB_REPOSITORY` environment variable
3. Parse `git remote get-url origin` (handles both HTTPS and SSH URLs)

**Exit codes:** 0 = posted successfully, 3 = posting failure (network, auth, API error).

### `quorum review --github-pr`

```
quorum review src/*.rs --github-pr <number> [--github-token <token>]
```

Runs the normal review pipeline, writes findings to stdout in the active
output mode, then calls `post_review()`. If posting fails, the review exit
code (0/1/2) is preserved — the error goes to stderr. The findings are
already on stdout, so the user gets them regardless.

When `--github-pr` is set and `--diff-file` is not provided, the PR diff
is fetched from GitHub and used both for diff-scoped review and for
commentability validation.

### CLI structs (src/cli/mod.rs)

```rust
// Add to Command enum:
/// Post review findings as GitHub PR comments
Report(ReportOpts),

// New struct:
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

// Add to ReviewOpts:
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

## Output Format

### CI log output (stdout)

Add `GITHUB_ACTIONS` to the compact mode auto-detection in DESIGN.md Section 2:

```
if --json flag OR !stdout.is_terminal() -> JSON
else if --compact or CLAUDE_CODE or GITHUB_ACTIONS env -> Compact
else -> Human
```

Note: the pipe-detection (`!stdout.is_terminal()`) takes priority. The
`GITHUB_ACTIONS` detection matters when Actions provides a pseudo-TTY.

### stderr during posting

Compact, CI-friendly progress on stderr (only when stderr is a TTY or
`GITHUB_ACTIONS` is set):

```
Posting 3 findings to PR #42... dismissed previous review... done
```

On failure:
```
Error: GitHub post failed: 401 Unauthorized (review exit code preserved: 1)
```

### PR comment format (Markdown)

**Inline review comment** (one per finding, anchored on the diff line):

```markdown
**!** Unvalidated input passed to SQL query — `security`

User input from request.query flows to db.execute()
without sanitization. Use parameterized queries.

*quorum 0.27.0 | gpt-5.4, ast*
```

Format rules (consistent with DESIGN.md):
- Severity icon (`!`, `~`, `-`) bold, matching DESIGN.md Section 4
- Title bold, category in inline code
- Description as plain text
- Source attribution in italics, includes version and model(s)
- No emoji, no decoration, no box-drawing

**Top-level review body:**

```markdown
<!-- quorum-review-marker:v1 run_id=01JTEST sha=abc1234 version=0.27.0 -->

## Quorum Review

3 findings (1 critical, 1 warning, 1 info) | 2 inline, 1 in summary

### Findings outside changed lines

**~** Session token not rotated after privilege change — `security` L89

After role elevation, the existing session token persists.
Rotate tokens on privilege change to prevent session fixation.

*quorum 0.27.0 | gpt-5.4*
```

**Clean review (no findings):**

```markdown
<!-- quorum-review-marker:v1 run_id=01JTEST sha=abc1234 version=0.27.0 -->

## Quorum Review

No findings.
```

**Review body overflow:** If out-of-diff findings exceed 55,000 chars, stop
appending and add: `... N additional findings omitted from review body.
See CI artifact for full results.`

## Dismiss-and-Replace Protocol

Each quorum review includes a hidden HTML comment in the review body:

```
<!-- quorum-review-marker:v1 run_id=01JTEST sha=abc1234 version=0.27.0 -->
```

On re-run:
1. `GET /repos/{owner}/{repo}/pulls/{pr}/reviews` — list all reviews
2. Scan each review body for `quorum-review-marker`
3. Dismiss matching reviews: `PUT .../reviews/{id}/dismissals` with
   message "Superseded by updated quorum review"
4. Post new review

**Dismissal is best-effort.** If the token lacks dismiss permissions (e.g.,
`GITHUB_TOKEN` on a non-protected branch), the 403/404 is logged to stderr
and the new review is posted anyway. The marker metadata (run_id, sha)
makes the latest review identifiable regardless.

## Commentability Validation

GitHub's Review API only accepts inline comments on lines visible in the
PR diff (changed lines + context lines around hunks). The `in_diff` field
on `Finding` is not sufficient — it indicates the finding is conceptually
within changed code, but doesn't guarantee the specific `anchor_line()`
is commentable.

The `github_report` module performs explicit validation:

1. Parse the PR diff into a map: `file_path -> Vec<(start_line, end_line)>`
   representing commentable line ranges per file
2. For each finding with `in_diff == Some(true)`:
   - Look up the file path in the diff map
   - Check if `anchor_line()` falls within any commentable range
   - If yes: inline comment
   - If no: downgrade to review body (listed under "Findings outside changed lines")
3. Findings with `in_diff != Some(true)`: always review body

This reuses the existing `parse_unified_diff()` infrastructure.

## Output Sanitization

Always-on for all text interpolated into PR comment bodies. A
`sanitize_for_github()` function in `src/github_report.rs`:

1. **Control characters** — strip ASCII control chars except `\n` and `\t`
   (reuse pattern from `src/output/mod.rs::strip_control_chars`)
2. **Backtick escape** — replace sequences of 3+ backticks with the same
   count of backticks inside an inline code span, preventing Markdown
   code fence breakout
3. **@mention neutralization** — replace `@username` with `` `@username` ``
   (renders as inline code, no notification triggered)
4. **#ref neutralization** — replace `#123` with `` `#123` `` (no cross-link)
5. **Image tag stripping** — remove `![alt](url)` Markdown images and
   `<img ...>` HTML tags (exfiltration vector via URL parameters)
6. **HTML anchor stripping** — remove `<a ...>...</a>` tags (prevent
   arbitrary link injection)
7. **Truncation** — individual comment bodies capped at 60,000 chars
   (headroom under GitHub's 65,536 char limit)

## GitHub API Details

### Create review with inline comments

```
POST /repos/{owner}/{repo}/pulls/{pull_number}/reviews
Authorization: Bearer <token>

{
  "commit_id": "<PR HEAD SHA>",
  "event": "COMMENT",
  "body": "<review summary + marker>",
  "comments": [
    {
      "path": "src/auth.rs",
      "body": "<sanitized finding text>",
      "line": 42,
      "side": "RIGHT"
    }
  ]
}
```

For multi-line findings (`line_start != line_end` and both in commentable
range), use `start_line` and `start_side` fields for a multi-line comment.

### List reviews

```
GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews
```

### Dismiss review

```
PUT /repos/{owner}/{repo}/pulls/{pull_number}/reviews/{review_id}/dismissals

{
  "message": "Superseded by updated quorum review",
  "event": "DISMISS"
}
```

### Fetch PR diff (when --diff-file not provided)

```
GET /repos/{owner}/{repo}/pulls/{pull_number}
Accept: application/vnd.github.diff
```

### Resolve PR from head SHA (Stage 2)

```
GET /repos/{owner}/{repo}/commits/{sha}/pulls
```

Returns PRs associated with a commit. Used in Stage 2 of the workflow_run
pattern to resolve the PR number from trusted GitHub API data rather than
from artifact files (artifact poisoning defense).

## Exit Codes

| Command | Success | Posting failure | Other error |
|---------|---------|----------------|-------------|
| `quorum report` | 0 | 3 | 3 |
| `quorum review --github-pr` | review code (0/1/2) | review code + stderr error | 3 |

For `review --github-pr`, the review's own exit code takes precedence.
Posting failure is a non-fatal side-effect — the findings are already on
stdout. The error is made clearly visible via stderr and trace events.

## Dogfooding: `.github/workflows/quorum-review.yml`

Two-stage `workflow_run` pattern per GitHub Security Lab recommendations:

### Stage 1: `pull_request` (unprivileged)

```yaml
name: Quorum Review (analyze)
on: [pull_request]

jobs:
  analyze:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install quorum
        run: cargo install --path . --locked
      - name: Generate PR diff
        run: gh pr diff ${{ github.event.pull_request.number }} > pr.diff
        env:
          GH_TOKEN: ${{ github.token }}
      - name: Run review
        run: quorum review --json --diff-file pr.diff $(git diff --name-only origin/main...HEAD) > findings.json
        env:
          QUORUM_BASE_URL: ${{ secrets.QUORUM_BASE_URL }}
          QUORUM_API_KEY: ${{ secrets.QUORUM_API_KEY }}
      - uses: actions/upload-artifact@v4
        with:
          name: quorum-findings
          path: findings.json
```

### Stage 2: `workflow_run` (privileged)

```yaml
name: Quorum Review (report)
on:
  workflow_run:
    workflows: ["Quorum Review (analyze)"]
    types: [completed]

jobs:
  report:
    if: github.event.workflow_run.conclusion == 'success'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install quorum
        run: cargo install --path . --locked
      - uses: actions/download-artifact@v4
        with:
          name: quorum-findings
          run-id: ${{ github.event.workflow_run.id }}
          github-token: ${{ secrets.GITHUB_TOKEN }}
      - name: Post review
        run: |
          PR_NUMBER=$(gh api repos/${{ github.repository }}/commits/${{ github.event.workflow_run.head_sha }}/pulls --jq '.[0].number')
          quorum report findings.json --pr "$PR_NUMBER"
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

Security notes:
- Stage 1 has no write permissions and no access to `GITHUB_TOKEN` on forks
- Stage 2 resolves PR number from trusted GitHub API (head SHA), not from
  artifact contents
- `QUORUM_API_KEY` is only needed in Stage 1 (LLM calls); Stage 2 only
  needs `GITHUB_TOKEN`
- Never interpolate `github.*` context directly in `run:` blocks — use
  `env:` mapping (script injection prevention)

## DESIGN.md Updates

### Section 2: Output Modes

Add `GITHUB_ACTIONS` to the compact detection table:

| Mode | When | Format | Audience |
|------|------|--------|----------|
| Human | stdout is a TTY, no flags | Styled findings | Terminal user |
| Compact | `--compact` or `CLAUDE_CODE`/`GITHUB_ACTIONS` env | Token-optimized | LLM / CI log |
| JSON | `--json` flag or stdout is piped | Machine-parseable | Scripts, pipes |

### New Section 14: GitHub PR Comments

Document the Markdown format, sanitization rules, severity icon mapping,
and marker protocol as established in this spec.

## Testing Strategy

- **Unit tests** for `sanitize_for_github()` covering each sanitization rule
- **Unit tests** for commentability validation (hunk parsing, line classification)
- **Unit tests** for repo URL parsing (HTTPS, SSH, GitHub Enterprise)
- **Unit tests** for marker parsing (find marker, extract metadata)
- **Integration test** for review body construction (findings -> Markdown)
- **Integration test** for the full `post_review()` flow with a mock HTTP server
- **No live GitHub API tests in CI** — mock all HTTP interactions

## Security Considerations

1. **Fork safety**: `quorum report` must run in a privileged context (Stage 2
   or internal PRs). Document this requirement prominently.
2. **Prompt injection**: Existing `prompt_sanitize.rs` handles LLM prompt
   safety. The new `sanitize_for_github()` handles output safety. These are
   separate concerns at separate boundaries.
3. **Secret redaction**: Existing `redact.rs` ensures no secrets appear in
   findings. This applies before GitHub posting.
4. **Artifact poisoning**: Stage 2 resolves PR number from GitHub API, not
   from artifact files. Findings JSON from Stage 1 is untrusted input for
   display purposes only — it cannot influence which PR gets commented on.
5. **Token scope**: `GITHUB_TOKEN` is hidden from clap help via
   `hide_env_values = true`. The token is only used for GitHub API calls,
   never logged or included in findings.
6. **SSRF**: GitHub API calls go to `api.github.com` (hardcoded). No
   user-controlled URL construction for the GitHub API path.
