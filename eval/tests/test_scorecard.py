from scorecard import compute_metrics
from schema import Verdict

def _v(tool: str, verdict: str, file: str = "rust/index_writer.rs",
       gt_id: str | None = None) -> Verdict:
    return Verdict(
        file=file, tool=tool, finding_title="test",
        verdict=verdict, judge="auto", reason="test",
        matched_ground_truth_id=gt_id,
    )

def test_precision_all_tp():
    m = compute_metrics(
        [_v("q", "tp", gt_id="gt-1"), _v("q", "tp", gt_id="gt-2")],
        total_known_bugs=3,
    )
    assert m.precision == 1.0
    assert m.recall == 2 / 3

def test_precision_with_fp():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "fp"), _v("t", "fp")],
        total_known_bugs=2,
    )
    assert m.precision == 1 / 3
    assert m.tp_count == 1
    assert m.fp_count == 2

def test_partial_counts_half():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "partial")],
        total_known_bugs=2,
    )
    assert m.precision == 0.75  # (1 + 0.5) / 2
    assert m.tp_count == 1

def test_recall_counts_unique_gt():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "tp", gt_id="gt-1"),
         _v("t", "tp")],
        total_known_bugs=3,
    )
    assert m.recall == 1 / 3  # only 1 unique gt match despite 3 TPs
    assert m.tp_count == 3

def test_unknown_excluded_from_metrics():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "unknown"), _v("t", "unknown")],
        total_known_bugs=2,
    )
    assert m.tp_count == 1
    assert m.precision == 1.0  # 1 TP out of 1 judged (unknowns excluded)
    assert m.total_findings == 3

def test_empty_findings():
    m = compute_metrics([], total_known_bugs=5)
    assert m.precision == 0.0
    assert m.recall == 0.0
    assert m.f1 == 0.0
