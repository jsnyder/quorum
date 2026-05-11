from scorecard import compute_metrics
from schema import Verdict

def _v(tool: str, verdict: str, file: str = "rust/index_writer.rs") -> Verdict:
    return Verdict(
        file=file, tool=tool, finding_title="test",
        verdict=verdict, judge="auto", reason="test",
    )

def test_precision_all_tp():
    m = compute_metrics(
        [_v("quorum-v0.21.0", "tp"), _v("quorum-v0.21.0", "tp")],
        total_known_bugs=3,
    )
    assert m.precision == 1.0
    assert m.recall == 2 / 3

def test_precision_with_fp():
    m = compute_metrics(
        [_v("t", "tp"), _v("t", "fp"), _v("t", "fp")],
        total_known_bugs=2,
    )
    assert m.precision == 1 / 3
    assert m.tp_count == 1
    assert m.fp_count == 2

def test_partial_counts_half():
    m = compute_metrics(
        [_v("t", "tp"), _v("t", "partial")],
        total_known_bugs=2,
    )
    assert m.precision == 0.75  # (1 + 0.5) / 2
    assert m.tp_count == 1

def test_empty_findings():
    m = compute_metrics([], total_known_bugs=5)
    assert m.precision == 0.0
    assert m.recall == 0.0
    assert m.f1 == 0.0
