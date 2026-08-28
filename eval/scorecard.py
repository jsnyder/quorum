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
    # Hits on control entries (expected == "miss"). Not a bug found: a claim the
    # source does not support. Reported next to precision, not buried in JSON.
    overconfident: int = 0


def compute_metrics(
    verdicts: list[Verdict],
    total_known_bugs: int,
    control_ids: set[str] | None = None,
    excluded_ids: set[str] | None = None,
) -> ToolMetrics:
    controls = control_ids or set()
    # Everything absent from the recall denominator. Controls are always part of
    # this; judge-rejected entries join them but are NOT overconfidence, so the
    # two sets stay separate. Defaults to controls alone for older callers.
    excluded = excluded_ids if excluded_ids is not None else controls

    tp = sum(1 for v in verdicts if v.verdict == "tp")
    fp = sum(1 for v in verdicts if v.verdict == "fp")
    partial = sum(1 for v in verdicts if v.verdict == "partial")
    total = len(verdicts)

    effective_tp = tp + 0.5 * partial
    judged = tp + fp + partial
    precision = effective_tp / judged if judged > 0 else 0.0

    # Recall = unique known bugs found / total known bugs.
    # Excluded entries are dropped from BOTH sides: total_known_bugs already
    # omits them, so leaving their matches in gt_matched would let recall exceed
    # 100% and would reward the exact behaviour the exclusions exist to detect.
    # Only control hits are attributed as overconfidence — a judge-rejected hit
    # means the rule fired as designed and the judge is the thing under test.
    matched = {v.matched_ground_truth_id for v in verdicts
               if v.verdict in ("tp", "partial") and v.matched_ground_truth_id}
    overconfident = len(matched & controls)
    gt_matched = matched - excluded

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
        overconfident=overconfident,
    )


@dataclass
class CorpusPartition:
    """How the corpus divides for scoring purposes.

    Two independent reasons an entry leaves the recall denominator, kept apart
    because they describe opposite health states:

    - `controls`  (expected == "miss")  measure reviewer RESTRAINT. Nothing
      should ever fire here; a hit is overconfidence.
    - `rejected`  (expected_verdict == "rejected") measure judge CORRECTNESS.
      The rule is SUPPOSED to fire and the judge is supposed to kill it.

    Collapse them and a rule that is broken and never fires looks identical to a
    rule that fires correctly and is correctly rejected.
    """
    scoreable: set[str]
    rejected: set[str]
    controls: set[str]

    @property
    def excluded(self) -> set[str]:
        return self.rejected | self.controls


def _partition_corpus(corpus_dir: Path) -> CorpusPartition:
    scoreable: set[str] = set()
    rejected: set[str] = set()
    controls: set[str] = set()
    for gt_file in corpus_dir.rglob("*.ground_truth.json"):
        for entry in load_ground_truth(gt_file):
            if entry.expected == "miss":
                controls.add(entry.id)
            elif entry.expected_verdict == "rejected":
                rejected.add(entry.id)
            else:
                scoreable.add(entry.id)
    return CorpusPartition(scoreable=scoreable, rejected=rejected, controls=controls)


def _find_unique(
    tool: str,
    all_verdicts: list[Verdict],
    control_ids: set[str] | None = None,
) -> int:
    """Bugs only this tool found. Controls are excluded — being alone in
    reporting an unsupportable claim is not a unique find, it is the
    overconfidence signal, and it is counted there instead."""
    controls = control_ids or set()
    tool_tps = {
        (v.file, v.matched_ground_truth_id)
        for v in all_verdicts
        if v.tool == tool and v.verdict == "tp" and v.matched_ground_truth_id
        and v.matched_ground_truth_id not in controls
    }
    other_tps = {
        (v.file, v.matched_ground_truth_id)
        for v in all_verdicts
        if v.tool != tool and v.verdict == "tp" and v.matched_ground_truth_id
        and v.matched_ground_truth_id not in controls
    }
    return len(tool_tps - other_tps)


def generate_scorecard(
    verdicts: list[Verdict],
    all_findings: dict[str, list[CanonicalFinding]],
    corpus_dir: Path,
) -> dict:
    part = _partition_corpus(corpus_dir)
    control_ids = part.controls
    total_known = len(part.scoreable)
    tools = sorted(all_findings.keys())

    tool_metrics: dict[str, ToolMetrics] = {}
    for tool in tools:
        tool_verdicts = [v for v in verdicts if v.tool == tool]
        m = compute_metrics(tool_verdicts, total_known, control_ids, part.excluded)
        m.tool = tool
        m.unique_finds = _find_unique(tool, verdicts, part.excluded)
        tool_metrics[tool] = m

    # Spell out the denominator on the face of the report. If a reader has to
    # open _partition_corpus() to learn why it is not the raw entry count, we
    # have rebuilt the trap this change exists to remove.
    corpus_line = f"Corpus: {total_known} scoreable"
    extras = []
    if part.rejected:
        extras.append(f"{len(part.rejected)} judge-rejected")
    if part.controls:
        extras.append(f"{len(part.controls)} control")
    if extras:
        corpus_line += " + " + " + ".join(extras) + " (excluded from recall)"

    lines = [
        "# Benchmark Scorecard",
        "",
        corpus_line,
        f"Tools: {', '.join(tools)}",
        "",
        "## Summary",
        "",
        "| Tool | Findings | TP | FP | Partial | Precision | Recall | F1 | Unique | Overconf | Noise/file |",
        "|------|---------|----|----|---------|-----------|--------|----|--------|----------|------------|",
    ]
    for tool in tools:
        m = tool_metrics[tool]
        lines.append(
            f"| {m.tool} | {m.total_findings} | {m.tp_count} | {m.fp_count} | "
            f"{m.partial_count} | {m.precision:.1%} | {m.recall:.1%} | "
            f"{m.f1:.1%} | {m.unique_finds} | {m.overconfident} | {m.noise_rate:.1f} |"
        )

    flagged = [t for t in tools if tool_metrics[t].overconfident]
    if flagged:
        lines.extend([
            "",
            "> **Overconf** counts hits on control entries — defects that are real but "
            "not identifiable from the source alone. A non-zero value is not a bug found; "
            "it is a claim the file does not support, and it is worth reading the "
            "justification before trusting that tool's other findings. Flagged: "
            + ", ".join(flagged),
        ])

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
        "control_ids": sorted(control_ids),
        "judge_rejected_ids": sorted(part.rejected),
        "tools": {t: asdict(m) for t, m in tool_metrics.items()},
    }

    return {"markdown": markdown, "data": data}
