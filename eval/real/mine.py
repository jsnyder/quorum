#!/usr/bin/env python3
"""Mine bot review comments that a later commit in the same PR acted on.

A candidate: a top-level review comment by a review bot, on a merged PR, whose
lines a later commit in that PR changed. The diff the bot saw (base..commented
commit) is the eval input; the comment's lines are the ground truth.

The comment window is the newest `--pages`*100 review comments (any author)
created at or before `--until`, and only PRs merged by then count, so a re-run
with the same `--until` rebuilds the same candidates in the same order.

usage: mine.py OWNER/REPO BOT_LOGIN --until ISO8601 [--pages N] > candidates.jsonl
"""
import json, re, subprocess, sys, time
from datetime import datetime, timezone
from collections import Counter

SINCE = "2026-07-01T00:00:00Z"
SLACK = 3
CODE = re.compile(r"\.(rs|py|ts|tsx|js|jsx|go|mjs|cjs)$")


def gh(path, tries=4):
    # Large comment pages intermittently come back truncated ("unexpected end
    # of JSON input"); a missing page would silently shrink the window.
    for i in range(tries):
        out = subprocess.run(["gh", "api", path], capture_output=True, text=True)
        if not out.returncode:
            return json.loads(out.stdout)
        time.sleep(2 ** i)
    print(f"gh api {path}: {out.stderr.strip()[:200]}", file=sys.stderr)
    return None


def old_side_ranges(patch):
    """Old-file line ranges each hunk of a unified patch touches."""
    for m in re.finditer(r"^@@ -(\d+)(?:,(\d+))? ", patch or "", re.M):
        start, n = int(m.group(1)), int(m.group(2) or 1)
        yield start, start + max(n, 1) - 1


def utc(ts):
    """ISO-8601 with Z or any offset, as an aware UTC datetime."""
    d = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    return (d if d.tzinfo else d.replace(tzinfo=timezone.utc)).astimezone(timezone.utc)


def arg(name, default=None):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def main():
    repo, bot = sys.argv[1], sys.argv[2]
    if not arg("--until"):
        sys.exit("--until is required: without it the window drifts as new comments arrive")
    until = utc(arg("--until"))
    window = int(arg("--pages", 10)) * 100
    prs, st, seen, p = {}, Counter(), 0, 0
    while seen < window:
        p += 1
        batch = gh(f"repos/{repo}/pulls/comments?since={SINCE}&per_page=100&page={p}"
                   "&sort=created&direction=desc")
        if batch is None:
            # A lost page shrinks the window silently and shifts every index triage.py keys on.
            sys.exit(f"{repo}: page {p} failed after retries; refusing a partial window")
        if not batch:
            break
        for c in batch:
            if utc(c["created_at"]) > until:
                continue
            seen += 1
            if seen > window:
                break
            if c["user"]["login"] != bot or c.get("in_reply_to_id"):
                continue
            line = c.get("original_line") or c.get("line")
            if not line:
                continue
            st["comments"] += 1
            pr = int(c["pull_request_url"].rsplit("/", 1)[1])
            prs.setdefault(pr, []).append(c)
    for pr, comments in prs.items():
        info = gh(f"repos/{repo}/pulls/{pr}")
        if not info or not info.get("merged_at") or utc(info["merged_at"]) > until:
            st["pr_not_merged"] += len(comments)
            continue
        head = info["head"]["sha"]
        for c in comments:
            sha = c["original_commit_id"]
            if sha == head:
                st["last_commit"] += 1
                continue  # nothing came after it
            cmp = gh(f"repos/{repo}/compare/{sha}...{head}")
            if not cmp:
                st["compare_failed"] += 1
                continue
            line = c.get("original_line") or c["line"]
            lo = (c.get("original_start_line") or line) - SLACK
            hi = line + SLACK
            f = next((f for f in cmp.get("files", []) if f["filename"] == c["path"]), None)
            if not f or not any(a <= hi and b >= lo for a, b in old_side_ranges(f.get("patch"))):
                st["lines_untouched"] += 1
                continue
            if not CODE.search(c["path"]):
                st["not_code"] += 1
                continue
            st["candidates"] += 1
            body = c["body"]
            sev = re.search(r"</picture>\s*(\w+)\*\*", body)
            title = re.search(r"^\*\*([^<*][^\n]*?)\*\*\s*$", body, re.M) or re.search(r"^#+\s*(.+)$", body, re.M)
            print(json.dumps({
                "title": title.group(1).strip() if title else body.strip().split("\n")[0][:160],
                "severity": sev.group(1) if sev else None,
                "repo": repo, "pr": pr, "bot": bot, "commit": sha,
                "base": info["base"]["sha"], "merged_at": info["merged_at"],
                "path": c["path"], "start_line": c.get("original_start_line") or line,
                "line": line, "comment": c["body"][:4000], "url": c["html_url"],
                "fix_patch": (f.get("patch") or "")[:6000],
            }), flush=True)
    print(f"{repo} {bot}: {dict(st)}", file=sys.stderr)


if __name__ == "__main__":
    main()
