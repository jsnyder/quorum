#!/usr/bin/env python3
"""Group kept defects from all.jsonl + triaged.jsonl into cases.json: one case per (repo, pr, reviewed commit)."""
import json
from pathlib import Path

HERE = Path(__file__).parent
a = [json.loads(l) for l in open(HERE / "all.jsonl")]
t = [json.loads(l) for l in open(HERE / "triaged.jsonl")]
assert len(a) == len(t) and all(x["url"] == y["url"] for x, y in zip(a, t))
cases = {}
for x, y in zip(a, t):
    if not y["keep"]:
        continue
    k = (x["repo"], x["pr"], x["commit"])
    c = cases.setdefault(k, {"id": f"{x['repo'].split('/')[1]}-{x['pr']}-{x['commit'][:7]}", "repo": x["repo"],
                             "pr": x["pr"], "commit": x["commit"], "base": x["base"], "defects": []})
    c["defects"].append({f: x[f] for f in ("path", "start_line", "line", "url", "title")}
                        | {f: y[f] for f in ("bug", "difficulty", "needs_other_files", "category")})
cs = list(cases.values())
json.dump(cs, open(HERE / "cases.json", "w"), indent=1)
print(len(cs), "cases,", sum(len(c["defects"]) for c in cs), "defects")
