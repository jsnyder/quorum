from schema import CanonicalFinding, Verdict, GroundTruthEntry

def test_canonical_finding_from_dict():
    d = {
        "tool": "quorum-v0.21.0",
        "file": "rust/index_writer.rs",
        "title": "Unsafe block without safety comment",
        "category": "correctness",
        "severity": "high",
        "line_start": 42,
        "line_end": 45,
        "description": "unsafe block lacks a SAFETY comment",
    }
    f = CanonicalFinding(**d)
    assert f.tool == "quorum-v0.21.0"
    assert f.severity == "high"
    assert f.line_start == 42

def test_verdict_from_dict():
    v = Verdict(
        file="rust/index_writer.rs",
        tool="quorum-v0.21.0",
        finding_title="test",
        verdict="tp",
        judge="auto",
        reason="matched ground truth",
        matched_ground_truth_id="plant-001",
    )
    assert v.verdict == "tp"

def test_ground_truth_entry():
    g = GroundTruthEntry(
        id="plant-001",
        type="planted",
        title="SQL injection via f-string",
        category="security",
        severity="high",
        line_start=42,
        line_end=45,
        description="User input interpolated without parameterization",
    )
    assert g.id == "plant-001"
    assert g.type == "planted"
