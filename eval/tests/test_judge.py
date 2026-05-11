from judge import match_ground_truth, judge_auto, _parse_verdict
from schema import CanonicalFinding, GroundTruthEntry

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

def test_parse_verdict_with_match():
    v, r, m = _parse_verdict("VERDICT: tp MATCH: iw-001 REASON: Matches known bug")
    assert v == "tp"
    assert m == "iw-001"
    assert "Matches known bug" in r

def test_parse_verdict_match_none():
    v, r, m = _parse_verdict("VERDICT: fp MATCH: none REASON: Not a real bug")
    assert v == "fp"
    assert m is None

def test_parse_verdict_no_match_field():
    v, r, m = _parse_verdict("VERDICT: tp REASON: Genuine issue")
    assert v == "tp"
    assert m is None

def test_parse_verdict_fallback():
    v, r, m = _parse_verdict("tp This is a real bug")
    assert v == "tp"
    assert m is None
