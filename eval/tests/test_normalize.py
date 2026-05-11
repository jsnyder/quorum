from normalize import normalize_quorum, normalize_pal, normalize_third_opinion, _norm_severity

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
    {"severity": "high", "title": "MAX_NUM_THREAD not enforced in IndexWriter::new()",
     "category": "correctness", "line": 42, "description": "Thread limit declared but never checked"},
    {"severity": "medium", "title": "rollback() silently discards enqueued operations",
     "category": "reliability", "line": 180, "description": "Pending ops dropped on rollback"},
]

def test_normalize_pal():
    findings = normalize_pal(PAL_FINDINGS, "rust/index_writer.rs")
    assert len(findings) == 2
    f = findings[0]
    assert f.tool == "pal"
    assert f.file == "rust/index_writer.rs"
    assert f.severity == "high"
    assert f.line_start == 42
    assert f.category == "correctness"

THIRD_OPINION_OUTPUT = {
    "score": 87,
    "findings": [
        {"severity": "medium", "category": "correctness",
         "message": "MAX_NUM_THREAD declared but never enforced", "line": 42},
        {"severity": "low", "category": "error handling",
         "message": "Thread join error handling inconsistent", "line": 100},
    ],
}

def test_normalize_third_opinion():
    findings = normalize_third_opinion(THIRD_OPINION_OUTPUT, "rust/index_writer.rs")
    assert len(findings) == 2
    f = findings[0]
    assert f.tool == "third-opinion"
    assert f.severity == "medium"
    assert f.category == "correctness"
    assert f.line_start == 42
    assert "MAX_NUM_THREAD" in f.title

QUORUM_GROUPED_JSON = [
    {
        "file": "/abs/path/to/index_writer.rs",
        "findings": [
            {
                "id": "01KTEST",
                "title": "UB on oversized len",
                "description": "bounds not checked",
                "severity": "critical",
                "category": "security",
                "source": {"llm": "gpt-5.4"},
                "line_start": 44,
                "line_end": 49,
            },
            {
                "id": "01KTEST2",
                "title": "Panic in Drop",
                "description": "unwrap in drop",
                "severity": "high",
                "category": "correctness",
                "source": {"llm": "gpt-5.4"},
                "line_start": 160,
                "line_end": 165,
            },
        ],
    }
]

def test_normalize_quorum_grouped_json():
    findings = normalize_quorum(QUORUM_GROUPED_JSON, "quorum-v0.21.0", "rust/index_writer.rs")
    assert len(findings) == 2
    assert findings[0].title == "UB on oversized len"
    assert findings[0].severity == "critical"
    assert findings[0].file == "rust/index_writer.rs"
    assert findings[1].line_start == 160

def test_norm_severity_non_string():
    """Fix 5: non-string severity (int, None) should not crash."""
    assert _norm_severity(42) == "info"
    assert _norm_severity(None) == "info"


def test_norm_severity_normal():
    assert _norm_severity("high") == "high"
    assert _norm_severity("WARNING") == "medium"


def test_normalize_quorum_with_meta():
    data = [
        {"_meta": {"linters": {"enabled": ["shellcheck"]}}},
        {
            "file": "/abs/path/deploy.sh",
            "findings": [
                {
                    "title": "Pipe to bash",
                    "severity": "high",
                    "category": "security",
                    "line_start": 60,
                    "line_end": 60,
                    "description": "curl | bash",
                },
            ],
        },
    ]
    findings = normalize_quorum(data, "quorum-v0.21.0", "bash/deploy.sh")
    assert len(findings) == 1
    assert findings[0].title == "Pipe to bash"
    assert findings[0].file == "bash/deploy.sh"
