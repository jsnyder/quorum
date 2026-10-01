#!/usr/bin/env python3
"""Per-axis recall and precision against planted defects.

Each `corpus/<axis>/` directory holds files with defects planted in that
axis's domain, each anchored to a line range in the sibling
`<file>.ground_truth.json`, plus decoys: code that looks like a defect but
is one the axis's own prompt says not to report. The runner reviews every
file with every axis and scores:

  in-lane   the axis on its own corpus -- recall of planted defects
            (a `redacted` entry is a secret the egress redaction strips
            before the model sees it, and is not scored),
            precision of what it emitted, decoys it fell for
  out-of-lane  the axis on the other axes' corpora -- findings emitted
            where the prompt says the subject belongs to another axis

A finding hits a planted defect when its anchor line falls inside the
defect's range (with a small tolerance), or its line span (if not too
wide) overlaps the range, and no other finding has already claimed it.
Everything else the axis emitted on its own corpus is noise.

Usage:
  eval/axes/run_axes.py [--model M] [--axes a,b] [--quorum PATH] [--out DIR]
  eval/axes/run_axes.py --score results/<file>.json   # re-score saved raw output
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORPUS = HERE / "corpus"
RESULTS = HERE / "results"
AXES = ["correctness", "security", "testing-antipatterns", "architecture", "simplicity", "performance"]
SOURCE_SUFFIXES = (".rs", ".py", ".ts", ".tsx", ".js", ".go")
LINE_TOLERANCE = 2
# A finding's [line_start, line_end] may cover the defect without the anchor
# sitting on it (the model often anchors at the enclosing function). Overlap
# counts, up to this span; wider spans match by anchor only, so a finding
# that covers half the file cannot claim whatever is unclaimed.
MAX_OVERLAP_SPAN = 40


def corpus_files() -> list[tuple[str, Path, list[dict]]]:
    """(axis, source path, ground truth) for every corpus file."""
    out = []
    for axis in AXES:
        d = CORPUS / axis
        if not d.is_dir():
            continue
        for f in sorted(d.iterdir()):
            if f.suffix not in SOURCE_SUFFIXES:
                continue
            gt = f.with_name(f.stem + ".ground_truth.json")
            if not gt.exists():
                raise SystemExit(f"{f} has no ground truth file")
            out.append((axis, f, json.loads(gt.read_text())))
    return out


def anchor_line(finding: dict) -> int | None:
    cited = finding.get("cited_lines")
    if isinstance(cited, dict) and isinstance(cited.get("start"), int):
        return cited["start"]
    if isinstance(cited, list) and cited and isinstance(cited[0], int):
        return cited[0]
    ls = finding.get("line_start")
    return ls if isinstance(ls, int) else None


def score_file(findings: list[dict], ground_truth: list[dict]) -> dict:
    """Match findings to planted defects and decoys by anchor line.

    Each planted defect is claimed by at most one finding (the first whose
    anchor falls in range); the rest of the findings on that range count as
    duplicates, not hits. Decoy ranges that any finding lands on count as
    decoy hits. Findings that land on neither are noise.
    """
    reals = [g for g in ground_truth if g.get("type") == "real"]
    decoys = [g for g in ground_truth if g.get("type") == "decoy"]
    # `redacted`: a planted secret the redaction chokepoint strips before the
    # model sees the file (CLAUDE.md: the tool's own review is unreliable
    # where its input is transformed). A finding landing there is neither a
    # hit nor noise.
    redacted = [g for g in ground_truth if g.get("type") == "redacted"]
    claimed: set[str] = set()
    hits, decoy_hits, noise, duplicates = [], [], [], []
    for f in findings:
        line = anchor_line(f)
        if line is None:
            noise.append(f)
            continue

        def within(g: dict) -> bool:
            lo, hi = g["line_start"] - LINE_TOLERANCE, g["line_end"] + LINE_TOLERANCE
            if lo <= line <= hi:
                return True
            ls, le = f.get("line_start"), f.get("line_end")
            if not (isinstance(ls, int) and isinstance(le, int) and le >= ls):
                return False
            return le - ls + 1 <= MAX_OVERLAP_SPAN and ls <= hi and le >= lo

        real = next((g for g in reals if within(g)), None)
        if real is not None:
            if real["id"] in claimed:
                duplicates.append(f)
            else:
                claimed.add(real["id"])
                hits.append({"id": real["id"], "kind": real.get("kind"), "title": f.get("title")})
            continue
        decoy = next((g for g in decoys if within(g)), None)
        if decoy is not None:
            decoy_hits.append({"id": decoy["id"], "kind": decoy.get("kind"), "title": f.get("title")})
            continue
        if any(within(g) for g in redacted):
            continue
        noise.append(f)
    missed = [{"id": g["id"], "kind": g.get("kind"), "title": g["title"]} for g in reals if g["id"] not in claimed]
    return {
        "planted": len(reals),
        "hits": hits,
        "missed": missed,
        "duplicates": len(duplicates),
        "decoy_hits": decoy_hits,
        "noise": [{"line": anchor_line(f), "title": f.get("title")} for f in noise],
        "emitted": len(findings),
    }


def run_quorum(quorum: str, file: Path, axis: str, model: str | None, home: Path) -> tuple[list[dict], dict]:
    """Review one file with one axis; returns (findings, run meta)."""
    env = os.environ.copy()
    env["HOME"] = str(home)  # isolate feedback/calibrator state so verdicts on real code cannot suppress planted ones
    env["QUORUM_HOME"] = str(home / ".quorum")
    env["QUORUM_DISABLE_EMBEDDINGS"] = "1"
    cmd = [quorum, "review", str(file), "--json", "--no-cache", "--skip-context7", "--axes", axis, "--caller", f"axes-eval-{axis}"]
    if model:
        cmd += ["--model", model]
    t0 = time.monotonic()
    p = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=600)
    secs = round(time.monotonic() - t0, 1)
    meta = {"exit": p.returncode, "secs": secs, "stderr_tail": p.stderr.strip()[-300:]}
    if p.returncode == 3:
        return [], {**meta, "error": "tool error"}
    try:
        payload = json.loads(p.stdout)
    except json.JSONDecodeError:
        return [], {**meta, "error": "non-JSON output"}
    findings: list[dict] = []
    incomplete = None
    for entry in payload if isinstance(payload, list) else []:
        if "_meta" in entry:
            incomplete = entry["_meta"].get("incomplete")
            continue
        findings.extend(entry.get("findings", []))
    if incomplete and incomplete.get("axes_failed"):
        meta["error"] = f"{incomplete['axes_failed']} of {incomplete['axes_total']} cells failed"
    # Only the axis's own findings are the prompt's doing. AST and ast-grep
    # rule findings ride along in the same review; count them, score them
    # separately, and keep them out of the axis's recall and precision.
    axis_findings = [f for f in findings if f.get("originating_skill") == axis]
    meta["rule_findings"] = len(findings) - len(axis_findings)
    return axis_findings, meta


def aggregate(raw: dict) -> dict:
    """Per-axis table from raw per-(axis, file) results."""
    table: dict[str, dict] = {}
    for axis in AXES:
        inlane = [r for r in raw["runs"] if r["axis"] == axis and r["corpus_axis"] == axis]
        outlane = [r for r in raw["runs"] if r["axis"] == axis and r["corpus_axis"] != axis]
        planted = sum(r["score"]["planted"] for r in inlane)
        hits = sum(len(r["score"]["hits"]) for r in inlane)
        emitted = sum(r["score"]["emitted"] for r in inlane)
        noise = sum(len(r["score"]["noise"]) for r in inlane)
        decoy_hits = sum(len(r["score"]["decoy_hits"]) for r in inlane)
        duplicates = sum(r["score"]["duplicates"] for r in inlane)
        errors = [r for r in inlane + outlane if r["meta"].get("error")]
        table[axis] = {
            "files": len(inlane),
            "planted": planted,
            "hits": hits,
            "recall": round(hits / planted, 2) if planted else None,
            "emitted_in_lane": emitted,
            "precision": round(hits / emitted, 2) if emitted else None,
            "decoy_hits": decoy_hits,
            "noise": noise,
            "duplicates": duplicates,
            "out_of_lane_findings": sum(r["score"]["emitted"] for r in outlane),
            "out_of_lane_files": len(outlane),
            "errors": len(errors),
            "secs": round(sum(r["meta"].get("secs", 0) for r in inlane + outlane), 1),
            "missed": [m for r in inlane for m in r["score"]["missed"]],
            "fell_for": [d for r in inlane for d in r["score"]["decoy_hits"]],
        }
    return table


def render(table: dict, model: str) -> str:
    lines = [f"axes eval -- model {model}", ""]
    lines.append(f"{'axis':22s} files planted hits recall  emitted precision decoys noise dups  out-of-lane  errors  secs")
    for axis, t in table.items():
        rec = "-" if t["recall"] is None else f"{t['recall']:.2f}"
        prec = "-" if t["precision"] is None else f"{t['precision']:.2f}"
        lines.append(
            f"{axis:22s} {t['files']:5d} {t['planted']:7d} {t['hits']:4d} {rec:>6s}  {t['emitted_in_lane']:7d} {prec:>9s} {t['decoy_hits']:6d} {t['noise']:5d} {t['duplicates']:4d}  "
            f"{t['out_of_lane_findings']:4d}/{t['out_of_lane_files']:<3d}    {t['errors']:6d}  {t['secs']:5.0f}"
        )
    lines.append("")
    for axis, t in table.items():
        if t["missed"]:
            lines.append(f"{axis} missed: " + "; ".join(f"{m['id']} [{m['kind']}] {m['title']}" for m in t["missed"]))
        if t["fell_for"]:
            lines.append(f"{axis} fell for decoys: " + "; ".join(f"{d['id']} [{d['kind']}] -> \"{d['title']}\"" for d in t["fell_for"]))
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", default=None, help="reviewer model (default: quorum's own default)")
    ap.add_argument("--axes", default=",".join(AXES), help="axes to run (comma-separated)")
    ap.add_argument("--quorum", default=shutil.which("quorum") or "quorum")
    ap.add_argument("--out", type=Path, default=RESULTS)
    ap.add_argument("--in-lane-only", action="store_true", help="skip the cross-corpus runs")
    ap.add_argument("--score", type=Path, help="re-score a saved results file instead of running")
    args = ap.parse_args()

    if args.score:
        raw = json.loads(args.score.read_text())
        gt_by_file = {str(f): gt for _, f, gt in corpus_files()}
        for r in raw["runs"]:
            r["score"] = score_file(r["findings"], gt_by_file[r["file"]])
        table = aggregate(raw)
        print(render(table, raw["model"]))
        return 0

    axes = [a.strip() for a in args.axes.split(",") if a.strip()]
    files = corpus_files()
    model_label = args.model or "default"
    raw = {"model": model_label, "started": datetime.now(timezone.utc).isoformat(), "quorum": args.quorum, "runs": []}
    home = Path(tempfile.mkdtemp(prefix="axes-eval-home-"))
    try:
        total = sum(1 for a in axes for ca, _, _ in files if ca == a or not args.in_lane_only)
        n = 0
        for axis in axes:
            for corpus_axis, f, gt in files:
                if args.in_lane_only and corpus_axis != axis:
                    continue
                n += 1
                print(f"[{n}/{total}] {axis} on {corpus_axis}/{f.name}", file=sys.stderr, flush=True)
                findings, meta = run_quorum(args.quorum, f, axis, args.model, home)
                raw["runs"].append({
                    "axis": axis,
                    "corpus_axis": corpus_axis,
                    "file": str(f),
                    "findings": findings,
                    "meta": meta,
                    "score": score_file(findings, gt),
                })
    finally:
        shutil.rmtree(home, ignore_errors=True)
    raw["finished"] = datetime.now(timezone.utc).isoformat()
    args.out.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out = args.out / f"{stamp}-{model_label.replace('/', '_')}.json"
    out.write_text(json.dumps(raw, indent=1))
    table = aggregate(raw)
    print(render(table, model_label))
    print(f"\nraw results: {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
