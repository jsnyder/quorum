#!/usr/bin/env python3
"""Run quorum over mined real-bug cases and score recall.

A case is one (repo, pr, reviewed commit): the PR diff at that commit is the
input, every kept bot comment on it is a known defect. A finding hits a defect
when it is on the same file within SLACK lines of the commented range. That is
a position match, not a judgment: shift every defect by ~40 lines to get the
chance baseline, and judge hits by hand or with a model before quoting recall.

Clones and worktrees live under $QUORUM_EVAL_REAL_CACHE (default
~/.cache/quorum-eval-real), never in this repository.

This spends money: every case is a live review with --no-cache.

usage: run_cases.py cases.json --model M [--quorum BIN] [--jobs N] > results.json
"""
import json, os, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

CACHE = Path(os.environ.get("QUORUM_EVAL_REAL_CACHE", Path.home() / ".cache" / "quorum-eval-real"))
REPOS = CACHE / "repos"
WORK = CACHE / "work"
SLACK = 3
MAX_FILES = 12
CODE = (".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".go")


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True).stdout


def prepare(case):
    """Worktree at the reviewed commit plus the PR diff up to it."""
    repo = REPOS / case["repo"].replace("/", "_")
    if not repo.exists():
        REPOS.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", "-q", "--filter=blob:none", "--no-checkout",
                        f"https://github.com/{case['repo']}.git", str(repo)], check=True)
    wt = WORK / f'{case["repo"].replace("/", "_")}-{case["pr"]}-{case["commit"][:10]}'
    patch = wt.parent / (wt.name + ".patch")  # with_suffix would cut at a dot in the repo name
    if not wt.exists():
        git(repo, "fetch", "-q", "origin", case["commit"], case["base"])
        git(repo, "worktree", "add", "-q", "--detach", str(wt), case["commit"])
    if not patch.exists():
        mb = git(repo, "merge-base", case["base"], case["commit"]).strip()
        patch.write_text(git(repo, "diff", mb, case["commit"]))
    files = [l[6:] for l in patch.read_text().splitlines() if l.startswith("+++ b/")]
    files = [f for f in files if f.endswith(CODE)]
    # Every defect's file first, then the rest of the diff up to the cap.
    must = list(dict.fromkeys(d["path"] for d in case["defects"]))
    files = must + [f for f in files if f not in must][: max(0, MAX_FILES - len(must))]
    return wt, patch, [f for f in files if (wt / f).exists()]


def review(case, model, quorum):
    wt, patch, files = prepare(case)
    home = Path(tempfile.mkdtemp(prefix="qcase-"))
    env = dict(os.environ, HOME=str(home), QUORUM_HOME=str(home / ".quorum"),
               QUORUM_DISABLE_EMBEDDINGS="1")
    cmd = [quorum, "review", *files, "--diff-file", str(patch), "--json", "--no-cache",
           "--skip-context7", "--show-out-of-diff", "--model", model]
    p = subprocess.run(cmd, cwd=wt, capture_output=True, text=True, env=env, timeout=1800)
    try:
        out = json.loads(p.stdout)
    except json.JSONDecodeError:
        return {"case": case["id"], "error": p.stderr[-500:], "hits": [], "findings": 0}
    findings = []
    for f in out:
        for x in f.get("findings", []) if isinstance(f, dict) else []:
            # Rule findings are kept too; `source` and the `rule-name:` title prefix tell them apart.
            findings.append({"file": os.path.relpath(f["file"], wt) if os.path.isabs(f["file"]) else f["file"],
                             "line": x.get("line_start") or x.get("line"),
                             "end": x.get("line_end"), "in_diff": x.get("in_diff"),
                             "title": x.get("title", "")[:160], "source": x.get("source")})
    hits = []
    for d in case["defects"]:
        lo, hi = d["start_line"] - SLACK, d["line"] + SLACK
        m = [x for x in findings if (x["file"] == d["path"] or x["file"].endswith("/" + d["path"])) and x["line"]
             and x["line"] <= hi and (x["end"] or x["line"]) >= lo]
        hits.append({"url": d["url"], "hit": bool(m), "shown": any(x["in_diff"] is not False for x in m),
                     "by": [x["title"] for x in m][:3]})
    return {"case": case["id"], "files": len(files), "findings": len(findings), "all": findings,
            "hits": hits, "audit": str(home / ".quorum"), "stderr_tail": p.stderr[-300:]}


def main():
    cases = json.loads(Path(sys.argv[1]).read_text())
    model = sys.argv[sys.argv.index("--model") + 1]
    quorum = sys.argv[sys.argv.index("--quorum") + 1] if "--quorum" in sys.argv else "quorum"
    jobs = int(sys.argv[sys.argv.index("--jobs") + 1]) if "--jobs" in sys.argv else 4
    WORK.mkdir(parents=True, exist_ok=True)
    for c in cases:  # worktrees serially: git locks the repo
        prepare(c)
    with ThreadPoolExecutor(jobs) as ex:
        results = list(ex.map(lambda c: review(c, model, quorum), cases))
    total = sum(len(r["hits"]) for r in results)
    hit = sum(h["hit"] for r in results for h in r["hits"])
    shown = sum(h["shown"] for r in results for h in r["hits"])
    print(f"{model}: {hit}/{total} defects position-matched ({shown} shown by default), "
          f"{sum(r['findings'] for r in results)} findings, "
          f"{sum('error' in r for r in results)} errored cases", file=sys.stderr)
    json.dump({"model": model, "results": results}, sys.stdout, indent=1)


if __name__ == "__main__":
    main()
