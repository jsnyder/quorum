#!/usr/bin/env python3
"""Precision regression check for quorum feedback calibration.

Reads current feedback precision from `quorum stats --json` and compares against
a stored baseline at ~/.quorum/precision_baseline.json. Flags drops so operators
(or CI) can catch calibration regressions before they compound.

Exit codes:
    0  healthy (precision within tolerance of baseline)
    1  warn (drop >= WARN_DROP, typically 3pp)
    2  critical (drop >= CRIT_DROP, typically 5pp)
    3  usage / data error

Usage:
    check_precision.py                 # evaluate against saved baseline
    check_precision.py --set-baseline  # record current precision as new baseline
    check_precision.py --json          # machine-readable output

Design notes:
- Baseline stores {precision, feedback_count, set_at, quorum_version}.
- "Recent precision" = most recent weekly bucket from precision_trend
  that has at least MIN_WEEK_COUNT samples. Thin weeks (<20 verdicts) are
  statistically noisy, so we fall back to overall precision in that case.
- Comparison is absolute (percentage-point) not relative, because 75% -> 70%
  is meaningful regardless of the base.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

WARN_DROP = 0.03
CRIT_DROP = 0.05
MIN_WEEK_COUNT = 20


@dataclass
class PrecisionReading:
    """Most-recent signal to compare against baseline."""
    precision: float
    source: str  # "week:2026-04-18" or "overall"
    sample_count: int


def extract_recent_precision(stats: dict) -> PrecisionReading:
    """Prefer the latest weekly bucket with enough samples; else overall."""
    trend = stats.get("precision_trend") or []
    for bucket in reversed(trend):
        if bucket.get("count", 0) >= MIN_WEEK_COUNT:
            return PrecisionReading(
                precision=float(bucket["precision"]),
                source=f"week:{bucket['week_start'][:10]}",
                sample_count=int(bucket["count"]),
            )
    # Fallback: overall precision. Happens when no recent week has enough data.
    return PrecisionReading(
        precision=float(stats["precision"]),
        source="overall",
        sample_count=int(stats.get("tp", 0)) + int(stats.get("fp", 0)),
    )


def classify_drop(baseline: float, current: float) -> tuple[int, str]:
    """Return (exit_code, status_label) for the drop magnitude."""
    drop = baseline - current  # positive = regression
    if drop >= CRIT_DROP:
        return 2, "CRITICAL"
    if drop >= WARN_DROP:
        return 1, "WARN"
    return 0, "OK"


def load_baseline(path: Path) -> Optional[dict]:
    """Load baseline, returning None if missing, corrupt, or shape-invalid.

    Downstream code relies on `baseline["precision"]` being a finite float,
    so we validate that shape here instead of crashing at the call site.
    Anything that fails validation is treated like a missing baseline —
    operator sees NO_BASELINE and can reset with --set-baseline.
    """
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return None
    if not isinstance(data, dict):
        return None
    precision = data.get("precision")
    if not isinstance(precision, (int, float)) or isinstance(precision, bool):
        return None
    return data


def save_baseline(path: Path, data: dict) -> None:
    """Atomically write the baseline file.

    Direct writes can leave a truncated file if the process is interrupted
    or disk fills mid-write. os.replace is atomic within a single filesystem,
    so readers see either the old file or the new one — never partial.
    """
    import os
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    try:
        tmp.write_text(json.dumps(data, indent=2))
        os.replace(tmp, path)
    finally:
        # Clean up the tempfile if os.replace didn't consume it (e.g. on error
        # between write_text and replace). Safe no-op if already moved.
        if tmp.exists():
            try:
                tmp.unlink()
            except OSError:
                pass


def fetch_stats() -> dict:
    """Invoke quorum stats and parse JSON.

    Raises RuntimeError with a clear message for all common external-failure
    modes: binary missing, non-zero exit, malformed stdout. Callers should
    treat RuntimeError uniformly (reported to stderr, exit code 3).
    """
    try:
        result = subprocess.run(
            ["quorum", "stats", "--json"],
            capture_output=True,
            text=True,
            check=False,
        )
    except (FileNotFoundError, OSError) as e:
        raise RuntimeError(
            f"could not run quorum binary (is it on PATH?): {e}"
        ) from e
    if result.returncode != 0:
        raise RuntimeError(
            f"quorum stats failed (exit {result.returncode}): {result.stderr.strip()}"
        )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"quorum stats returned non-JSON output: {e}"
        ) from e


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Alert on quorum calibration precision regressions"
    )
    parser.add_argument(
        "--baseline-path",
        default=str(Path.home() / ".quorum" / "precision_baseline.json"),
        help="Baseline file location",
    )
    parser.add_argument(
        "--set-baseline",
        action="store_true",
        help="Record current precision as the new baseline and exit",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Machine-readable output instead of human text",
    )
    args = parser.parse_args()

    try:
        stats = fetch_stats()
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        return 3

    reading = extract_recent_precision(stats)
    baseline_path = Path(args.baseline_path)

    if args.set_baseline:
        record = {
            "precision": reading.precision,
            "feedback_count": stats.get("feedback_count"),
            "source": reading.source,
            "sample_count": reading.sample_count,
            "model": stats.get("model"),
        }
        save_baseline(baseline_path, record)
        if args.json:
            print(json.dumps({"action": "set_baseline", "baseline": record}))
        else:
            print(f"baseline set: {reading.precision:.1%} from {reading.source} "
                  f"(n={reading.sample_count})")
        return 0

    baseline = load_baseline(baseline_path)
    if baseline is None:
        msg = (f"no baseline at {baseline_path} — run with --set-baseline "
               "to record one")
        if args.json:
            print(json.dumps({"status": "NO_BASELINE", "reading": reading.__dict__,
                              "hint": msg}))
        else:
            print(msg, file=sys.stderr)
        return 3

    code, label = classify_drop(baseline["precision"], reading.precision)
    drop_pp = (baseline["precision"] - reading.precision) * 100

    if args.json:
        print(json.dumps({
            "status": label,
            "exit_code": code,
            "baseline_precision": baseline["precision"],
            "current_precision": reading.precision,
            "drop_pp": round(drop_pp, 2),
            "source": reading.source,
            "sample_count": reading.sample_count,
            "feedback_count": stats.get("feedback_count"),
        }))
    else:
        arrow = "↓" if drop_pp > 0 else "↑" if drop_pp < 0 else "→"
        print(f"[{label}] precision {arrow} {abs(drop_pp):.1f}pp  "
              f"(baseline {baseline['precision']:.1%} → current {reading.precision:.1%}, "
              f"source={reading.source}, n={reading.sample_count})")
        if code == 2:
            print("CRITICAL: calibration may have regressed. Check recent "
                  "feedback entries, threshold tuning, or retrieval changes.",
                  file=sys.stderr)

    return code


if __name__ == "__main__":
    sys.exit(main())
