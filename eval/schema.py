from dataclasses import dataclass, field, asdict
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
