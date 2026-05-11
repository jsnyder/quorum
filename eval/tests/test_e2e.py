import json
import tempfile
from pathlib import Path

from schema import CanonicalFinding, GroundTruthEntry, save_verdicts
from judge import judge_auto
from scorecard import compute_metrics, generate_scorecard


def test_full_pipeline_mock():
    """Smoke test: auto-judge + scorecard on synthetic data."""
    gt = [
        GroundTruthEntry(
            id="t-001", type="planted", title="SQL injection",
            category="security", severity="high",
            line_start=10, line_end=15, description="...",
        ),
        GroundTruthEntry(
            id="t-002", type="planted", title="Hardcoded password",
            category="security", severity="critical",
            line_start=50, line_end=52, description="...",
        ),
    ]

    tool_a_findings = [
        CanonicalFinding(
            tool="tool-a", file="python/app.py",
            title="SQL injection via string format",
            category="security", severity="high",
            line_start=12, line_end=14, description="...",
        ),
        CanonicalFinding(
            tool="tool-a", file="python/app.py",
            title="Unused import os",
            category="quality", severity="low",
            line_start=1, line_end=1, description="...",
        ),
    ]

    tool_b_findings = [
        CanonicalFinding(
            tool="tool-b", file="python/app.py",
            title="SQL injection risk",
            category="security", severity="high",
            line_start=11, line_end=13, description="...",
        ),
        CanonicalFinding(
            tool="tool-b", file="python/app.py",
            title="Hardcoded password in config",
            category="security", severity="critical",
            line_start=50, line_end=52, description="...",
        ),
    ]

    # Auto-judge both tools
    verdicts_a, unmatched_a = judge_auto(tool_a_findings, gt)
    verdicts_b, unmatched_b = judge_auto(tool_b_findings, gt)

    assert len(verdicts_a) == 1  # matched SQL injection
    assert verdicts_a[0].matched_ground_truth_id == "t-001"
    assert len(unmatched_a) == 1  # "Unused import" unmatched

    assert len(verdicts_b) == 2  # matched both
    assert {v.matched_ground_truth_id for v in verdicts_b} == {"t-001", "t-002"}

    # Scorecard
    all_verdicts = verdicts_a + verdicts_b
    all_findings = {"tool-a": tool_a_findings, "tool-b": tool_b_findings}

    # Create a temp corpus dir with ground truth
    with tempfile.TemporaryDirectory() as tmpdir:
        corpus = Path(tmpdir)
        py_dir = corpus / "python"
        py_dir.mkdir()
        gt_path = py_dir / "app.ground_truth.json"
        with open(gt_path, "w") as f:
            json.dump([
                {"id": g.id, "type": g.type, "title": g.title,
                 "category": g.category, "severity": g.severity,
                 "line_start": g.line_start, "line_end": g.line_end,
                 "description": g.description}
                for g in gt
            ], f)

        report = generate_scorecard(all_verdicts, all_findings, corpus)
        assert "tool-a" in report["markdown"]
        assert "tool-b" in report["markdown"]
        assert report["data"]["total_known_bugs"] == 2
