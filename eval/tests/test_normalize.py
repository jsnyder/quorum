from normalize import normalize_quorum, normalize_pal, normalize_third_opinion

QUORUM_OUTPUT = [
    {
        "id": "01JTEST000000000000000001",
        "title": "Unsafe block without safety comment",
        "description": "unsafe block lacks a SAFETY comment",
        "severity": "high",
        "category": "correctness",
        "source": {"Llm": "gpt-5.4"},
        "line_start": 42,
        "line_end": 45,
        "evidence": [],
        "calibrator_action": None,
        "similar_precedent": [],
    }
]

def test_normalize_quorum_basic():
    findings = normalize_quorum(QUORUM_OUTPUT, "quorum-v0.21.0", "rust/index_writer.rs")
    assert len(findings) == 1
    f = findings[0]
    assert f.tool == "quorum-v0.21.0"
    assert f.file == "rust/index_writer.rs"
    assert f.severity == "high"
    assert f.category == "correctness"
    assert f.line_start == 42

def test_normalize_quorum_grouped_by_file():
    grouped = {
        "src/main.rs": QUORUM_OUTPUT,
        "src/lib.rs": [],
    }
    findings = normalize_quorum(grouped, "quorum-v0.21.0")
    assert len(findings) == 1
    assert findings[0].file == "src/main.rs"

PAL_FINDINGS = [
    {"severity": "high", "title": "MAX_NUM_THREAD not enforced in IndexWriter::new()"},
    {"severity": "medium", "title": "rollback() silently discards enqueued operations"},
]

def test_normalize_pal():
    findings = normalize_pal(PAL_FINDINGS, "rust/index_writer.rs")
    assert len(findings) == 2
    f = findings[0]
    assert f.tool == "pal"
    assert f.file == "rust/index_writer.rs"
    assert f.severity == "high"
    assert f.line_start == 0  # PAL doesn't provide line numbers
    assert f.category == "unknown"

THIRD_OPINION_OUTPUT = {
    "total": 2,
    "score": 87,
    "findings": [
        {"severity": "medium", "title": "MAX_NUM_THREAD declared but never enforced"},
        {"severity": "low", "title": "Thread join error handling inconsistent"},
    ],
}

def test_normalize_third_opinion():
    findings = normalize_third_opinion(THIRD_OPINION_OUTPUT, "rust/index_writer.rs")
    assert len(findings) == 2
    f = findings[0]
    assert f.tool == "third-opinion"
    assert f.severity == "medium"
    assert f.line_start == 0
