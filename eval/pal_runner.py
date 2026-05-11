#!/usr/bin/env python3
"""Pre-generate PAL review results for the benchmark corpus.

Calls GPT-5.4 via LiteLLM with a structured code-review prompt and
saves JSON findings to pal_cache/ for the benchmark harness to load.

Usage: uv run python pal_runner.py [--lang rust] [--model gpt-5.4]
"""
import argparse
import json
import os
import re
import sys
from pathlib import Path

import httpx

EVAL_DIR = Path(__file__).parent
CORPUS_DIR = EVAL_DIR / "corpus"
CACHE_DIR = EVAL_DIR / "pal_cache"

REVIEW_PROMPT = """\
You are a code review expert. Analyze the following source file for bugs, \
security vulnerabilities, correctness issues, and serious quality problems.

Focus on real, actionable bugs — not style, naming, or documentation issues.

Source file: {file_path}
```
{source_code}
```

Output a JSON array of findings. Each finding must have these fields:
- "severity": one of "critical", "high", "medium", "low"
- "title": short one-line description of the issue
- "category": one of "security", "correctness", "error handling", "performance", "reliability"
- "line": the line number where the issue occurs (integer, 0 if unknown)
- "description": 1-2 sentence explanation

Output ONLY the JSON array, no markdown fences, no commentary."""


def review_file(file_path: Path, rel_path: str, base_url: str, api_key: str, model: str) -> list[dict]:
    source = file_path.read_text()
    prompt = REVIEW_PROMPT.format(file_path=rel_path, source_code=source)

    resp = httpx.post(
        f"{base_url}/v1/chat/completions",
        headers={"Authorization": f"Bearer {api_key}"},
        json={
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0,
        },
        timeout=120,
    )
    body = resp.json()
    if "choices" not in body:
        err = body.get("error", {}).get("message", str(body)[:200])
        print(f"  ERROR: {err}", file=sys.stderr)
        return []

    text = body["choices"][0]["message"]["content"] or ""
    text = re.sub(r"^```(?:json)?\s*\n?", "", text.strip())
    text = re.sub(r"\n?```\s*$", "", text.strip())
    try:
        parsed = json.loads(text)
        if isinstance(parsed, list):
            return parsed
        if isinstance(parsed, dict) and "findings" in parsed:
            return parsed["findings"]
        return []
    except json.JSONDecodeError:
        print(f"  ERROR: non-JSON response for {rel_path}", file=sys.stderr)
        return []


def main():
    parser = argparse.ArgumentParser(description="Generate PAL review cache")
    parser.add_argument("--lang", help="Filter to a single language")
    parser.add_argument("--model", default="gpt-5.4")
    args = parser.parse_args()

    base_url = os.environ.get("QUORUM_BASE_URL", "https://litellm.5745.house")
    api_key = os.environ.get("QUORUM_API_KEY", "")
    if not api_key:
        print("ERROR: QUORUM_API_KEY not set", file=sys.stderr)
        sys.exit(1)

    files = []
    for lang_dir in sorted(CORPUS_DIR.iterdir()):
        if not lang_dir.is_dir():
            continue
        if args.lang and lang_dir.name != args.lang:
            continue
        for f in sorted(lang_dir.iterdir()):
            if f.suffix in (".rs", ".py", ".ts", ".tsx", ".yaml", ".yml", ".sh", ".bash"):
                rel = f"{lang_dir.name}/{f.name}"
                files.append((rel, f))

    print(f"Generating PAL reviews for {len(files)} files using {args.model}...")
    for rel_path, abs_path in files:
        cache_file = CACHE_DIR / f"{rel_path}.json"
        if cache_file.exists():
            try:
                existing = json.loads(cache_file.read_text())
                print(f"  {rel_path}: cached ({len(existing)} findings)")
                continue
            except json.JSONDecodeError:
                print(f"  {rel_path}: corrupted cache, re-reviewing...")

        print(f"  {rel_path}: reviewing...", end="", flush=True)
        findings = review_file(abs_path, rel_path, base_url, api_key, args.model)
        if findings:
            cache_file.parent.mkdir(parents=True, exist_ok=True)
            with open(cache_file, "w") as f:
                json.dump(findings, f, indent=2)
            print(f" {len(findings)} findings")
        else:
            print(f" 0 findings (not cached, will retry next run)")

    print("Done. Results in pal_cache/")


if __name__ == "__main__":
    main()
