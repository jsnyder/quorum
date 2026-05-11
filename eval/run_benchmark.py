#!/usr/bin/env python3
import argparse
import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

from normalize import normalize_quorum, normalize_pal, normalize_third_opinion
from schema import CanonicalFinding, save_verdicts
from judge import judge_findings
from scorecard import generate_scorecard

EVAL_DIR = Path(__file__).parent
CORPUS_DIR = EVAL_DIR / "corpus"
BIN_DIR = EVAL_DIR / "binaries"
RESULTS_DIR = EVAL_DIR / "results"

TOOLS = ["quorum-v0.18.4", "quorum-v0.21.0", "pal", "third-opinion"]

COMPAT: dict = {}

def load_compat() -> dict:
    global COMPAT
    path = BIN_DIR / "compat.json"
    if path.exists():
        with open(path) as f:
            COMPAT = json.load(f)
    return COMPAT

def corpus_files() -> list[tuple[str, Path]]:
    files = []
    for lang_dir in sorted(CORPUS_DIR.iterdir()):
        if not lang_dir.is_dir():
            continue
        for f in sorted(lang_dir.iterdir()):
            if f.suffix in (".rs", ".py", ".ts", ".tsx", ".yaml", ".yml", ".sh", ".bash"):
                rel = f"{lang_dir.name}/{f.name}"
                files.append((rel, f))
    return files

def run_quorum(binary: Path, file_path: Path, version: str) -> list[dict]:
    env = os.environ.copy()
    env["QUORUM_HOME"] = tempfile.mkdtemp()
    env.setdefault("QUORUM_MODEL", "gpt-5.4")
    env.setdefault("QUORUM_ALLOWED_BASE_URL_HOSTS", "litellm.5745.house")

    cmd = [str(binary), "review", str(file_path), "--json", "--parallel", "1"]
    flags = COMPAT.get(version, {}).get("flags", [])
    if "--skip-context7" in flags:
        cmd.append("--skip-context7")

    result = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=300)
    if result.returncode == 3:
        print(f"    WARN: {version} tool error on {file_path.name}: {result.stderr.strip()[:200]}", file=sys.stderr)
        return []
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        print(f"    WARN: {version} non-JSON output on {file_path.name}", file=sys.stderr)
        return []

def run_pal(file_path: Path, rel_path: str) -> list[dict]:
    cache_file = EVAL_DIR / "pal_cache" / f"{rel_path}.json"
    if cache_file.exists():
        with open(cache_file) as f:
            return json.load(f)
    print("    PAL: no cached results (run pal_runner.py first)", file=sys.stderr)
    return []

def run_third_opinion(file_path: Path) -> dict | None:
    if not _tool_available("third-opinion"):
        print(f"    third-opinion: not installed, skipping", file=sys.stderr)
        return None
    result = subprocess.run(
        ["third-opinion", "review", str(file_path)],
        capture_output=True, text=True, timeout=300,
    )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return None

def _tool_available(name: str) -> bool:
    try:
        subprocess.run(["which", name], capture_output=True, check=True)
        return True
    except (subprocess.CalledProcessError, FileNotFoundError):
        return False

def run_all(
    tools: list[str],
    lang_filter: str | None = None,
) -> dict[str, list[CanonicalFinding]]:
    load_compat()
    all_findings: dict[str, list[CanonicalFinding]] = {t: [] for t in tools}
    files = corpus_files()

    for rel_path, abs_path in files:
        lang = rel_path.split("/")[0]
        if lang_filter and lang != lang_filter:
            continue

        print(f"  {rel_path}:")

        for tool in tools:
            if tool.startswith("quorum-"):
                version = tool.replace("quorum-", "")
                binary = BIN_DIR / f"quorum-{version}"
                if not binary.exists():
                    print(f"    {tool}: binary not found, skipping")
                    continue
                raw = run_quorum(binary, abs_path, version)
                normalized = normalize_quorum(raw, tool, rel_path)
            elif tool == "pal":
                raw = run_pal(abs_path, rel_path)
                normalized = normalize_pal(raw, rel_path)
            elif tool == "third-opinion":
                raw_dict = run_third_opinion(abs_path)
                if raw_dict is None:
                    continue
                normalized = normalize_third_opinion(raw_dict, rel_path)
            else:
                continue

            all_findings[tool].extend(normalized)
            print(f"    {tool}: {len(normalized)} findings")

    return all_findings

def main():
    parser = argparse.ArgumentParser(description="Quorum benchmark harness")
    parser.add_argument("--tool", choices=TOOLS, help="Run a single tool only")
    parser.add_argument("--lang", help="Filter to a single language (e.g., rust)")
    parser.add_argument("--with-calibration", action="store_true",
                        help="Second pass with real feedback store")
    parser.add_argument("--output-dir", type=Path,
                        help="Override results directory")
    args = parser.parse_args()

    tools = [args.tool] if args.tool else TOOLS
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M")
    out_dir = args.output_dir or RESULTS_DIR / timestamp
    out_dir.mkdir(parents=True, exist_ok=True)

    print(f"Benchmark run: {timestamp}")
    print(f"Tools: {', '.join(tools)}")
    print(f"Output: {out_dir}")
    print()

    # Phase 1: Run tools
    all_findings = run_all(tools, args.lang)

    # Save raw normalized findings per tool
    for tool, findings in all_findings.items():
        path = out_dir / f"{tool}.json"
        with open(path, "w") as f:
            json.dump([fd.to_dict() for fd in findings], f, indent=2)

    # Phase 2: Judge
    verdicts = judge_findings(all_findings, CORPUS_DIR)
    save_verdicts(verdicts, out_dir / "verdicts.json")

    # Phase 3: Scorecard
    report = generate_scorecard(verdicts, all_findings, CORPUS_DIR)
    (out_dir / "scorecard.md").write_text(report["markdown"])
    with open(out_dir / "scorecard.json", "w") as f:
        json.dump(report["data"], f, indent=2)

    print(f"\nResults written to {out_dir}/")
    print(report["markdown"])

if __name__ == "__main__":
    main()
