from judge import match_ground_truth, judge_auto
from schema import CanonicalFinding, GroundTruthEntry, Verdict

GT = [
    GroundTruthEntry(
        id="iw-001",
        type="real",
        title="MAX_NUM_THREAD not enforced",
        category="correctness",
        severity="high",
        line_start=40,
        line_end=50,
        description="...",
    ),
]

def test_auto_match_exact_title():
    f = CanonicalFinding(
        tool="quorum-v0.21.0",
        file="rust/index_writer.rs",
        title="MAX_NUM_THREAD not enforced in IndexWriter::new",
        category="correctness",
        severity="high",
        line_start=42,
        line_end=45,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is not None
    assert match.id == "iw-001"

def test_auto_match_no_match():
    f = CanonicalFinding(
        tool="quorum-v0.21.0",
        file="rust/index_writer.rs",
        title="Completely unrelated finding",
        category="performance",
        severity="low",
        line_start=500,
        line_end=510,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is None

def test_auto_match_line_within_tolerance():
    f = CanonicalFinding(
        tool="pal",
        file="rust/index_writer.rs",
        title="MAX_NUM_THREAD declared but never enforced",
        category="unknown",
        severity="medium",
        line_start=0,  # PAL has no line numbers
        line_end=0,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is not None  # title match should still work

def test_auto_match_title_keyword_overlap():
    f = CanonicalFinding(
        tool="third-opinion",
        file="rust/index_writer.rs",
        title="MAX_NUM_THREAD declared but never enforced",
        category="unknown",
        severity="medium",
        line_start=0,
        line_end=0,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is not None
    assert match.id == "iw-001"

def test_judge_auto_matches_and_unmatched():
    findings = [
        CanonicalFinding(
            tool="quorum-v0.21.0", file="rust/index_writer.rs",
            title="MAX_NUM_THREAD not enforced in IndexWriter::new",
            category="correctness", severity="high",
            line_start=42, line_end=45, description="...",
        ),
        CanonicalFinding(
            tool="quorum-v0.21.0", file="rust/index_writer.rs",
            title="Completely unrelated finding about style",
            category="quality", severity="low",
            line_start=100, line_end=105, description="...",
        ),
    ]
    verdicts, unmatched = judge_auto(findings, GT)
    assert len(verdicts) == 1
    assert verdicts[0].verdict == "tp"
    assert verdicts[0].matched_ground_truth_id == "iw-001"
    assert len(unmatched) == 1
    assert unmatched[0].title == "Completely unrelated finding about style"
