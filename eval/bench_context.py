#!/usr/bin/env python3
"""Benchmark the effect of quorum's context injection feature.

Runs reviews in 4 configurations and compares findings quality, count,
and token usage:
  1. baseline:      --skip-context7, no local context
  2. context7_only: context7 enabled, no local context
  3. local_only:    --skip-context7, local context from sources.toml
  4. both:          context7 + local context

Usage:
    python eval/bench_context.py
    python eval/bench_context.py --targets src/pipeline.rs src/calibrator.rs
    python eval/bench_context.py --configs baseline both
    python eval/bench_context.py --model gpt-5.4
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path

QUORUM_HOME_REAL = Path.home() / ".quorum"
QUORUM_BIN = shutil.which("quorum") or "quorum"
EVAL_DIR = Path(__file__).parent
PROJECT_ROOT = EVAL_DIR.parent

# Default target files: a mix of complex source files that import from
# other modules (benefits from local context) and simpler eval corpus
# files (control group).
DEFAULT_TARGETS = [
    # Complex pipeline files — heavy cross-module imports
    "src/pipeline.rs",
    "src/calibrator.rs",
    "src/context/bootstrap.rs",
    "src/judge.rs",
    # Simpler utility files
    "src/redact.rs",
    "src/merge.rs",
    # Eval corpus files (standalone, control group)
    "eval/corpus/rust/unsafe_parser.rs",
    "eval/corpus/python/api_handler.py",
    "eval/corpus/typescript/api_routes.ts",
]

CONFIGS = {
    "baseline": {
        "skip_context7": True,
        "local_context": False,
        "description": "No context7, no local context",
    },
    "context7_only": {
        "skip_context7": False,
        "local_context": False,
        "description": "Context7 framework docs only",
    },
    "local_only": {
        "skip_context7": True,
        "local_context": True,
        "description": "Local context injection only",
    },
    "both": {
        "skip_context7": False,
        "local_context": True,
        "description": "Context7 + local context",
    },
}


@dataclass
class FileResult:
    file: str
    config: str
    finding_count: int = 0
    findings_by_severity: dict = field(default_factory=dict)
    categories: list = field(default_factory=list)
    finding_titles: list = field(default_factory=list)
    tokens_in: int = 0
    tokens_out: int = 0
    duration_ms: int = 0
    context_chunks_retrieved: int = 0
    context_chunks_injected: int = 0
    context_tokens_injected: int = 0
    context7_resolved: int = 0
    error: str = ""
    raw_findings: list = field(default_factory=list)


def make_quorum_home(local_context: bool) -> Path:
    """Create a temporary QUORUM_HOME, optionally copying context indexes."""
    tmp = Path(tempfile.mkdtemp(prefix="quorum-bench-"))

    if local_context:
        # Copy sources.toml
        src_toml = QUORUM_HOME_REAL / "sources.toml"
        if src_toml.exists():
            shutil.copy2(src_toml, tmp / "sources.toml")

        # Symlink sources/ directory (indexes are read-only during review)
        src_dir = QUORUM_HOME_REAL / "sources"
        if src_dir.exists():
            os.symlink(src_dir, tmp / "sources")

    return tmp


def run_review(file_path: Path, config_name: str, config: dict, model: str) -> FileResult:
    """Run a single quorum review and capture results."""
    result = FileResult(
        file=str(file_path.relative_to(PROJECT_ROOT)),
        config=config_name,
    )

    qhome = make_quorum_home(config["local_context"])
    try:
        env = os.environ.copy()
        env["QUORUM_HOME"] = str(qhome)
        env.setdefault("QUORUM_MODEL", model)
        env.setdefault("QUORUM_ALLOWED_BASE_URL_HOSTS", "litellm.5745.house")
        env.setdefault("QUORUM_ALLOW_PRIVATE_BASE_URL", "1")

        cmd = [QUORUM_BIN, "review", str(file_path), "--json", "--parallel", "1"]
        if config["skip_context7"]:
            cmd.append("--skip-context7")

        t0 = time.monotonic()
        proc = subprocess.run(
            cmd, capture_output=True, text=True, env=env, timeout=300,
            cwd=str(PROJECT_ROOT),
        )
        elapsed_ms = int((time.monotonic() - t0) * 1000)
        result.duration_ms = elapsed_ms

        if proc.returncode == 3:
            result.error = proc.stderr.strip()[:300]
            return result

        # Parse JSON findings — output is [_meta, {file, findings: [...]}]
        try:
            raw = json.loads(proc.stdout)
        except json.JSONDecodeError:
            result.error = f"non-JSON output: {proc.stdout[:200]}"
            return result

        result.raw_findings = raw

        # Flatten nested file groups into a single list of findings
        findings = []
        for item in raw:
            if "findings" in item:
                findings.extend(item["findings"])
            elif "title" in item:
                findings.append(item)

        result.finding_count = len(findings)

        # Tally by severity
        sev_counts = {}
        cats = []
        titles = []
        for f in findings:
            sev = f.get("severity", "unknown")
            sev_counts[sev] = sev_counts.get(sev, 0) + 1
            cat = f.get("category", "")
            if cat:
                cats.append(cat)
            titles.append(f.get("title", ""))

        result.findings_by_severity = sev_counts
        result.categories = sorted(set(cats))
        result.finding_titles = titles

        # Parse review telemetry from the QUORUM_HOME
        reviews_log = qhome / "reviews.jsonl"
        if reviews_log.exists():
            lines = reviews_log.read_text().strip().split("\n")
            for line in reversed(lines):
                try:
                    rec = json.loads(line)
                    result.tokens_in = rec.get("tokens_in", 0)
                    result.tokens_out = rec.get("tokens_out", 0)
                    ctx = rec.get("context", {})
                    result.context_chunks_retrieved = ctx.get("retrieved_chunk_count", 0)
                    result.context_chunks_injected = ctx.get("injected_chunk_count", 0)
                    result.context_tokens_injected = ctx.get("injected_tokens", 0)
                    break
                except json.JSONDecodeError:
                    continue

        # Parse context7 telemetry
        telem_log = qhome / "telemetry.jsonl"
        if telem_log.exists():
            lines = telem_log.read_text().strip().split("\n")
            for line in reversed(lines):
                try:
                    rec = json.loads(line)
                    result.context7_resolved = rec.get("context7_resolved", 0)
                    break
                except json.JSONDecodeError:
                    continue

    except subprocess.TimeoutExpired:
        result.error = "timeout (300s)"
    finally:
        shutil.rmtree(qhome, ignore_errors=True)

    return result


def generate_report(results: list[FileResult], timestamp: str) -> str:
    """Generate a markdown comparison report."""
    lines = [
        "# Context Injection Benchmark",
        "",
        f"**Date:** {timestamp}",
        f"**Model:** {results[0].config if not results else 'N/A'}",
        f"**Files:** {len(set(r.file for r in results))}",
        f"**Configurations:** {len(set(r.config for r in results))}",
        "",
    ]

    # Summary table
    configs = sorted(set(r.config for r in results),
                     key=lambda c: list(CONFIGS.keys()).index(c) if c in CONFIGS else 99)
    files = sorted(set(r.file for r in results))

    lines.extend([
        "## Configuration Summary",
        "",
        "| Config | Description | Avg Findings | Avg Tokens In | Avg Duration (ms) | Ctx Chunks Injected |",
        "|--------|-------------|-------------|---------------|-------------------|---------------------|",
    ])

    for cfg in configs:
        cfg_results = [r for r in results if r.config == cfg and not r.error]
        if not cfg_results:
            continue
        avg_findings = sum(r.finding_count for r in cfg_results) / len(cfg_results)
        avg_tokens = sum(r.tokens_in for r in cfg_results) / len(cfg_results)
        avg_duration = sum(r.duration_ms for r in cfg_results) / len(cfg_results)
        avg_chunks = sum(r.context_chunks_injected for r in cfg_results) / len(cfg_results)
        desc = CONFIGS.get(cfg, {}).get("description", cfg)
        lines.append(
            f"| {cfg} | {desc} | {avg_findings:.1f} | {avg_tokens:.0f} | "
            f"{avg_duration:.0f} | {avg_chunks:.1f} |"
        )

    # Per-file comparison
    lines.extend(["", "## Per-File Comparison", ""])

    for file in files:
        file_results = {r.config: r for r in results if r.file == file}
        lines.append(f"### {file}")
        lines.append("")
        lines.append("| Config | Findings | Severity | Tokens In | Duration | Ctx Chunks | Error |")
        lines.append("|--------|----------|----------|-----------|----------|------------|-------|")

        for cfg in configs:
            r = file_results.get(cfg)
            if not r:
                continue
            sev_str = ", ".join(f"{k}:{v}" for k, v in sorted(r.findings_by_severity.items()))
            err_str = r.error[:50] if r.error else ""
            lines.append(
                f"| {cfg} | {r.finding_count} | {sev_str} | "
                f"{r.tokens_in} | {r.duration_ms}ms | "
                f"{r.context_chunks_injected} | {err_str} |"
            )
        lines.append("")

    # Finding diff: what's unique to each config
    lines.extend(["## Finding Differences", ""])

    for file in files:
        file_results = {r.config: r for r in results if r.file == file}
        baseline_titles = set(file_results.get("baseline", FileResult(file="", config="")).finding_titles)
        for cfg in configs:
            if cfg == "baseline":
                continue
            r = file_results.get(cfg)
            if not r:
                continue
            cfg_titles = set(r.finding_titles)
            added = cfg_titles - baseline_titles
            removed = baseline_titles - cfg_titles
            if added or removed:
                lines.append(f"**{file}** ({cfg} vs baseline):")
                if added:
                    for t in sorted(added):
                        lines.append(f"  + {t}")
                if removed:
                    for t in sorted(removed):
                        lines.append(f"  - {t}")
                lines.append("")

    # Token cost analysis
    lines.extend(["## Token Cost Analysis", ""])
    lines.append("| Config | Total Tokens In | Total Tokens Out | Est. Cost ($) |")
    lines.append("|--------|----------------|-----------------|---------------|")

    for cfg in configs:
        cfg_results = [r for r in results if r.config == cfg and not r.error]
        total_in = sum(r.tokens_in for r in cfg_results)
        total_out = sum(r.tokens_out for r in cfg_results)
        # Rough cost estimate at ~$2/1M input, ~$8/1M output
        cost = (total_in * 2 + total_out * 8) / 1_000_000
        lines.append(f"| {cfg} | {total_in:,} | {total_out:,} | ${cost:.4f} |")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Benchmark context injection effects")
    parser.add_argument("--targets", nargs="+", help="Files to review (relative to project root)")
    parser.add_argument("--configs", nargs="+", choices=list(CONFIGS.keys()),
                        help="Configurations to test (default: all)")
    parser.add_argument("--model", default="gpt-5.4", help="LLM model to use")
    parser.add_argument("--output-dir", type=Path, help="Override output directory")
    parser.add_argument("--dry-run", action="store_true", help="Print plan without running")
    args = parser.parse_args()

    targets = args.targets or DEFAULT_TARGETS
    configs_to_run = args.configs or list(CONFIGS.keys())
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M")

    out_dir = args.output_dir or (EVAL_DIR / "results" / f"context-bench-{timestamp}")
    out_dir.mkdir(parents=True, exist_ok=True)

    # Resolve target paths
    target_paths = []
    for t in targets:
        p = PROJECT_ROOT / t
        if not p.exists():
            print(f"WARN: target not found: {p}", file=sys.stderr)
            continue
        target_paths.append(p)

    total_runs = len(target_paths) * len(configs_to_run)
    print(f"Context Injection Benchmark")
    print(f"  Model:   {args.model}")
    print(f"  Targets: {len(target_paths)} files")
    print(f"  Configs: {', '.join(configs_to_run)}")
    print(f"  Runs:    {total_runs}")
    print(f"  Output:  {out_dir}")
    print()

    if args.dry_run:
        for t in target_paths:
            print(f"  {t.relative_to(PROJECT_ROOT)}")
        return

    results: list[FileResult] = []
    run_idx = 0

    for target in target_paths:
        rel = target.relative_to(PROJECT_ROOT)
        for cfg_name in configs_to_run:
            run_idx += 1
            cfg = CONFIGS[cfg_name]
            print(f"  [{run_idx}/{total_runs}] {rel} ({cfg_name})...", end=" ", flush=True)
            result = run_review(target, cfg_name, cfg, args.model)
            results.append(result)
            if result.error:
                print(f"ERROR: {result.error[:60]}")
            else:
                chunks_str = f", {result.context_chunks_injected} chunks" if result.context_chunks_injected else ""
                print(f"{result.finding_count} findings, {result.tokens_in} tok_in, "
                      f"{result.duration_ms}ms{chunks_str}")

    # Save raw results
    raw_path = out_dir / "raw_results.json"
    with open(raw_path, "w") as f:
        json.dump([asdict(r) for r in results], f, indent=2)

    # Generate report
    report = generate_report(results, timestamp)
    report_path = out_dir / "report.md"
    report_path.write_text(report)

    print(f"\nResults: {out_dir}/")
    print()
    print(report)


if __name__ == "__main__":
    main()
