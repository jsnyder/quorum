#!/usr/bin/env python3
"""Judge evaluation harness for issue #520 part 2.

Replays quorum's judge over a labelled finding set and reports per-rule
precision/recall. Findings come from ast-grep (the same scanner quorum uses);
the prompt is a byte-for-byte reconstruction of `judge::build_judge_prompt`,
verified once against the Rust implementation via `--dump-prompt` (see
docs/judge-eval-520.md, "Instrument calibration").

Usage:
    run_eval.py --manifest manifest.json --out results/baseline.jsonl
    run_eval.py --manifest manifest.json --variant strict --out results/strict.jsonl
    run_eval.py --manifest manifest.json --dump-prompt <file>   # no API calls
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
# The six rules under evaluation were deleted from the bundled set by this
# same change. They are vendored here at their 438c04b contents so the
# measurement behind that deletion can still be re-run.
RULES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "rules")

# --- prompt construction: mirrors src/judge.rs -------------------------------

JUDGE_SYSTEM_PROMPT = (
    "You are a code review judge. Respond with ONLY a JSON array, no other text."
)

# src/judge.rs::build_judge_prompt, verbatim.
BASELINE_HEADER = (
    "You are a code review judge. For each AST-detected finding below, "
    "determine if it is a true positive (tp), false positive (fp), or "
    "uncertain based on the surrounding code context.\n\n"
)

BASELINE_FOOTER = (
    '\n\nRespond with ONLY a JSON array. Each element must include the index field: '
    '{"index": N, "rule_id": "...", "verdict": "tp"|"fp"|"uncertain", '
    '"confidence": 0.0-1.0, "reason": "..."}\n'
)


def pick_fence_for(body: str) -> str:
    """src/prompt_sanitize.rs::pick_fence_for"""
    runs = [len(m) for m in re.findall(r"`+", body)]
    return "`" * max((max(runs) if runs else 0) + 1, 3)


def build_prompt(source: str, items: list, variant: str, rule_ctx: dict) -> str:
    """items: list of (rule_id, title, line_start, line_end, evidence)"""
    if variant == "baseline":
        header, footer = BASELINE_HEADER, BASELINE_FOOTER
    else:
        header, footer = VARIANTS[variant](rule_ctx)

    prompt = header
    fence = pick_fence_for(source)
    prompt += "Source code:\n" + fence + "\n" + source + "\n" + fence
    prompt += "\n\nFindings to judge:\n"
    objs = [
        {
            "evidence": ev,
            "index": i,
            "lines": f"{ls}-{le}",
            "rule_id": rid,
            "title": title,
        }
        for i, (rid, title, ls, le, ev) in enumerate(items)
    ]
    # serde_json::to_string_pretty: 2-space indent, BTreeMap key order (alpha).
    prompt += json.dumps(objs, indent=2, sort_keys=True)
    prompt += footer
    return prompt


# --- prompt variants under test ----------------------------------------------


def _strict(rule_ctx: dict):
    """Variant 'strict': supplies the rule's precision tier and its recorded
    track record, and asks for a bar rather than a neutral opinion."""
    lines = [
        "You are a code review judge. Each finding below was emitted by a "
        "SPECULATIVE pattern rule -- a rule that matches syntax and cannot see "
        "intent. Most of what these rules emit is noise.",
        "",
        "For each finding, answer one question: does the surrounding code show "
        "a concrete way this goes wrong at runtime?",
        "",
        '  "tp"  -- you can name the input or state that makes it fail.',
        '  "fp"  -- the pattern matched but the context makes it safe or intended.',
        '  "uncertain" -- the file does not contain enough to tell.',
        "",
        "Do not answer tp merely because the pattern is a real category of bug. "
        "The default answer is fp; tp must be earned by evidence in this file.",
    ]
    if rule_ctx:
        lines += ["", "Rule context:"]
        for rid, ctx in sorted(rule_ctx.items()):
            lines.append(f"  {rid}: {ctx}")
    return "\n".join(lines) + "\n\n", BASELINE_FOOTER


def _strict_noctx(_rule_ctx):
    """Variant 'strict-noctx': the same framing as 'strict' but with no rule
    track record. Isolates the framing change from the leaked prior."""
    return _strict({})


VARIANTS = {"strict": _strict, "strict-noctx": _strict_noctx}

# 'strict-noctx' is what shipped: src/judge.rs::build_judge_prompt now emits
# exactly this header. 'baseline' is preserved as the pre-#520-part-2 wording
# so the A/B stays reproducible. Verify with:
#   run_eval.py --manifest ... --variant strict-noctx --dump-prompt /tmp/py.txt
# and diff against a prompt dumped from build_judge_prompt.


# --- ast-grep scanning --------------------------------------------------------

RULE_FILES = {
    "discarded-result": "rust/discarded-result.yml",
    "string-byte-slice-broad": "rust/string-byte-slice-broad.yml",
    "logging-debug-leak": "python/logging-debug-leak.yml",
    "nullish-coalescing-broad": "typescript/nullish-coalescing-broad.yml",
    "string-format-sql": "typescript/string-format-sql.yml",
    "jinja-loop-variable-scoping": "yaml/jinja-loop-variable-scoping.yml",
}


def scan(path: str, rule: str) -> list:
    """Return findings shaped like quorum's Finding: (rule_id, title, ls, le, evidence)."""
    ry = os.path.join(RULES, RULE_FILES[rule])
    out = subprocess.run(
        ["ast-grep", "scan", "--rule", ry, "--json=compact", path],
        capture_output=True, text=True,
    )
    if out.returncode not in (0, 1):
        # A scanner that cannot run is not a file with no findings. Silently
        # conflating the two yields an evaluation that looks complete and is
        # not -- the exact failure this harness exists to avoid.
        raise RuntimeError(
            f"ast-grep failed on {path} with {ry} (exit {out.returncode}): "
            f"{out.stderr.strip()[:400]}"
        )
    if not out.stdout.strip():
        return []
    matches = json.loads(out.stdout)
    msg = None
    with open(ry, encoding="utf8") as fh:
        for line in fh:
            if line.startswith("message:"):
                msg = line.split(":", 1)[1].strip().strip('"')
                break
    lang = RULE_FILES[rule].split("/", 1)[0]
    rid = f"ast-grep:{lang}/{rule}"                     # src/ast_grep.rs
    title = f"{rule}: {msg}"                             # src/ast_grep.rs:358
    return [
        (rid, title, m["range"]["start"]["line"] + 1, m["range"]["end"]["line"] + 1,
         m["text"])
        for m in matches
    ]


# --- LLM call -----------------------------------------------------------------


def call_judge(prompt: str, model: str) -> tuple:
    base = os.environ["QUORUM_BASE_URL"].rstrip("/")
    if not base.startswith("https://"):
        # llm_client::validate_base_url requires HTTPS in production; this
        # request carries the same bearer key and the same source code.
        raise SystemExit(f"QUORUM_BASE_URL must be https, got {base!r}")
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": JUDGE_SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ],
        "max_tokens": 2048,
        "temperature": 0,
        # judge_completion() omits this; we add it so an A/B is not a cache replay.
        "cache": {"no-cache": True},
    }
    req = urllib.request.Request(
        base + "/chat/completions",
        data=json.dumps(body).encode(),
        headers={
            "Authorization": "Bearer " + os.environ["QUORUM_API_KEY"],
            "Content-Type": "application/json",
        },
    )
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=300) as r:
        data = json.load(r)
    ms = int((time.time() - t0) * 1000)
    usage = data.get("usage") or {}
    return (
        data["choices"][0]["message"]["content"],
        ms,
        usage.get("prompt_tokens", 0),
        usage.get("completion_tokens", 0),
        data["choices"][0].get("finish_reason"),
    )


def extract_json_array(text: str):
    """src/judge.rs::extract_json_array -- first '[' to last ']'."""
    i, j = text.find("["), text.rfind("]")
    if i == -1 or j == -1 or j < i:
        return None
    try:
        return json.loads(text[i:j + 1])
    except json.JSONDecodeError:
        return None


# --- main ---------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--out")
    ap.add_argument("--variant", default="baseline",
                    choices=["baseline"] + sorted(VARIANTS))
    ap.add_argument("--model", default=os.environ.get("QUORUM_JUDGE_MODEL", "gpt-4.1-mini"))
    ap.add_argument("--dump-prompt", help="write the first prompt here and exit (no API calls)")
    args = ap.parse_args()

    with open(args.manifest, encoding="utf8") as fh:
        manifest = json.load(fh)
    rule_ctx = manifest.get("rule_context", {}) if args.variant != "baseline" else {}

    results, totals = [], {"calls": 0, "ms": 0, "tin": 0, "tout": 0}
    for entry in manifest["files"]:
        path = entry["path"]
        if not os.path.isabs(path):
            path = os.path.join(REPO, path)
        if not os.path.exists(path):
            print(f"  SKIP missing {path}", file=sys.stderr)
            continue
        source = open(path, encoding="utf8", errors="replace").read()

        items, meta = [], []
        for rule in entry["rules"]:
            for f in scan(path, rule):
                items.append(f)
                meta.append({"rule": rule, "line": f[2], "evidence": f[4]})
        if not items:
            continue

        prompt = build_prompt(source, items, args.variant, rule_ctx)
        if args.dump_prompt:
            with open(args.dump_prompt, "w", encoding="utf8") as fh:
                fh.write(prompt)
            print(f"wrote prompt for {entry['path']} ({len(items)} findings) "
                  f"-> {args.dump_prompt}")
            return

        content, ms, tin, tout, finish = call_judge(prompt, args.model)
        totals["calls"] += 1
        totals["ms"] += ms
        totals["tin"] += tin
        totals["tout"] += tout
        verdicts = extract_json_array(content) or []
        by_index = {v.get("index"): v for v in verdicts if isinstance(v, dict)}

        for i, m in enumerate(meta):
            v = by_index.get(i, {})
            if i not in by_index:
                # No verdict came back for this finding (truncated response or a
                # dropped index). quorum leaves judge_verdict = None, and
                # enforce_judge_required then drops it. Not the same as
                # "uncertain", which is deliberately kept.
                judged = "unjudged"
            else:
                raw = str(v.get("verdict", "")).lower()
                judged = {"tp": "approved", "fp": "rejected"}.get(raw, "uncertain")
            label = entry.get("labels", {}).get(f"{m['rule']}:{m['line']}") \
                or entry.get("rule_default_label", {}).get(m["rule"])
            results.append({
                "file": entry["path"], "rule": m["rule"], "line": m["line"],
                "evidence": m["evidence"][:200], "label": label,
                "judge": judged, "confidence": v.get("confidence"),
                "reason": v.get("reason", ""), "label_source": entry["label_source"],
                "finish_reason": finish,
            })
        print(f"  {entry['path']}: {len(items)} findings, {ms}ms, "
              f"{tin}+{tout} tok, finish={finish}", file=sys.stderr)

    if args.out:
        if os.path.dirname(args.out):
            os.makedirs(os.path.dirname(args.out), exist_ok=True)
        with open(args.out, "w", encoding="utf8") as fh:
            for r in results:
                fh.write(json.dumps(r) + "\n")
            fh.write(json.dumps({"_totals": totals, "_variant": args.variant,
                                 "_model": args.model}) + "\n")
    print(json.dumps({"totals": totals, "n": len(results)}))


if __name__ == "__main__":
    main()
