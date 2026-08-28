from dataclasses import dataclass, asdict
import json
from pathlib import Path


@dataclass
class CanonicalFinding:
    tool: str
    file: str
    title: str
    category: str
    severity: str
    line_start: int
    line_end: int
    description: str

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass
class GroundTruthEntry:
    id: str
    type: str  # "planted", "cve", "real"
    title: str
    category: str
    severity: str
    line_start: int
    line_end: int
    description: str
    cve: str | None = None
    # Present in the speculative_patterns fixtures but never declared here, so
    # load_ground_truth() raised TypeError on all four of them and any walk of
    # the whole corpus died. Declared now so the corpus loads; semantics
    # unchanged. `rule` names the speculative rule under test, `expected_verdict`
    # is what the JUDGE should decide about that rule's finding.
    rule: str | None = None
    expected_verdict: str | None = None
    # "hit"  - a defect the tool SHOULD report. Counts toward recall as usual.
    # "miss" - a control: a genuine defect that is NOT identifiable from the
    #          source alone, because recognising it needs domain knowledge the
    #          file does not carry. Silence is the correct answer, so it is
    #          excluded from the recall denominator; a tool that reports it is
    #          counted as overconfident instead.
    # Defaulted, so existing corpora keep their current scoring and
    # load_ground_truth() needs no change.
    expected: str = "hit"


@dataclass
class Verdict:
    file: str
    tool: str
    finding_title: str
    verdict: str  # "tp", "fp", "partial"
    judge: str  # "auto", "panel", "human"
    reason: str
    matched_ground_truth_id: str | None = None


def load_ground_truth(path: Path) -> list[GroundTruthEntry]:
    with open(path) as f:
        return [GroundTruthEntry(**e) for e in json.load(f)]


def save_verdicts(verdicts: list[Verdict], path: Path) -> None:
    with open(path, "w") as f:
        json.dump([asdict(v) for v in verdicts], f, indent=2)
