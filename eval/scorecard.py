from dataclasses import dataclass, asdict
from pathlib import Path

from schema import CanonicalFinding, Verdict, load_ground_truth


@dataclass
class ToolMetrics:
    tool: str = ""
    tp_count: int = 0
    fp_count: int = 0
    partial_count: int = 0
    total_findings: int = 0
    total_known_bugs: int = 0
    precision: float = 0.0
    recall: float = 0.0
    f1: float = 0.0
    unique_finds: int = 0
    noise_rate: float = 0.0


def compute_metrics(
    verdicts: list[Verdict],
    total_known_bugs: int,
) -> ToolMetrics:
    tp = sum(1 for v in verdicts if v.verdict == "tp")
    fp = sum(1 for v in verdicts if v.verdict == "fp")
    partial = sum(1 for v in verdicts if v.verdict == "partial")
    total = len(verdicts)

    effective_tp = tp + 0.5 * partial
    precision = effective_tp / total if total > 0 else 0.0
    # Recall = unique known bugs found / total known bugs
    gt_matched = {v.matched_ground_truth_id for v in verdicts
                  if v.verdict in ("tp", "partial") and v.matched_ground_truth_id}
    recall = len(gt_matched) / total_known_bugs if total_known_bugs > 0 else 0.0
    f1 = (2 * precision * recall / (precision + recall)) if (precision + recall) > 0 else 0.0

    files = set(v.file for v in verdicts)
    noise = fp / len(files) if files else 0.0

    return ToolMetrics(
        tp_count=tp,
        fp_count=fp,
        partial_count=partial,
        total_findings=total,
        total_known_bugs=total_known_bugs,
        precision=precision,
        recall=recall,
        f1=f1,
        noise_rate=round(noise, 2),
    )


def _count_known_bugs(corpus_dir: Path) -> int:
    count = 0
    for gt_file in corpus_dir.rglob("*.ground_truth.json"):
        gt = load_ground_truth(gt_file)
        count += len(gt)
    return count


def _find_unique(
    tool: str,
    all_verdicts: list[Verdict],
) -> int:
    tool_tps = {
        (v.file, v.matched_ground_truth_id)
        for v in all_verdicts
        if v.tool == tool and v.verdict == "tp" and v.matched_ground_truth_id
    }
    other_tps = {
        (v.file, v.matched_ground_truth_id)
        for v in all_verdicts
        if v.tool != tool and v.verdict == "tp" and v.matched_ground_truth_id
    }
    return len(tool_tps - other_tps)


def generate_scorecard(
    verdicts: list[Verdict],
    all_findings: dict[str, list[CanonicalFinding]],
    corpus_dir: Path,
) -> dict:
    total_known = _count_known_bugs(corpus_dir)
    tools = sorted(all_findings.keys())

    tool_metrics: dict[str, ToolMetrics] = {}
    for tool in tools:
        tool_verdicts = [v for v in verdicts if v.tool == tool]
        m = compute_metrics(tool_verdicts, total_known)
        m.tool = tool
        m.unique_finds = _find_unique(tool, verdicts)
        tool_metrics[tool] = m

    lines = [
        "# Benchmark Scorecard",
        "",
        f"Corpus: {total_known} known bugs",
        f"Tools: {', '.join(tools)}",
        "",
        "## Summary",
        "",
        "| Tool | Findings | TP | FP | Partial | Precision | Recall | F1 | Unique | Noise/file |",
        "|------|---------|----|----|---------|-----------|--------|----|--------|------------|",
    ]
    for tool in tools:
        m = tool_metrics[tool]
        lines.append(
            f"| {m.tool} | {m.total_findings} | {m.tp_count} | {m.fp_count} | "
            f"{m.partial_count} | {m.precision:.1%} | {m.recall:.1%} | "
            f"{m.f1:.1%} | {m.unique_finds} | {m.noise_rate:.1f} |"
        )

    files = sorted(set(v.file for v in verdicts))
    if files:
        lines.extend(["", "## Per-file breakdown", ""])
        for file in files:
            lines.append(f"### {file}")
            lines.append("")
            lines.append("| Tool | TP | FP | Partial |")
            lines.append("|------|----|----|---------|")
            for tool in tools:
                fv = [v for v in verdicts if v.file == file and v.tool == tool]
                tp = sum(1 for v in fv if v.verdict == "tp")
                fp = sum(1 for v in fv if v.verdict == "fp")
                p = sum(1 for v in fv if v.verdict == "partial")
                if tp + fp + p > 0:
                    lines.append(f"| {tool} | {tp} | {fp} | {p} |")
            lines.append("")

    markdown = "\n".join(lines)
    data = {
        "total_known_bugs": total_known,
        "tools": {t: asdict(m) for t, m in tool_metrics.items()},
    }

    return {"markdown": markdown, "data": data}
