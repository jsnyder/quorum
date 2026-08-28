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


# --- control entries (expected == "miss") -----------------------------------
# A control is a real defect that is NOT identifiable from the source alone.
# Silence is the correct answer, so it must not sit in the recall denominator,
# and reporting it is an overconfidence signal rather than a bug found.

def test_control_hit_does_not_count_toward_recall():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "tp", gt_id="ctl-1")],
        total_known_bugs=2,          # caller already excluded the control
        control_ids={"ctl-1"},
    )
    assert m.recall == 0.5           # gt-1 only, NOT 2/2
    assert m.overconfident == 1


def test_control_hit_cannot_push_recall_above_one():
    # The regression this guards: leaving control matches in gt_matched while
    # the denominator omits them yields recall > 100%.
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "tp", gt_id="ctl-1")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
    )
    assert m.recall == 1.0
    assert m.overconfident == 1


def test_silence_on_control_is_not_penalised():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
    )
    assert m.recall == 1.0           # perfect: found the bug, ignored the control
    assert m.overconfident == 0


def test_control_ids_default_preserves_existing_behaviour():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1")],
        total_known_bugs=2,
    )
    assert m.recall == 0.5
    assert m.overconfident == 0


def test_partial_on_control_also_counts_as_overconfident():
    m = compute_metrics(
        [_v("t", "partial", gt_id="ctl-1")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
    )
    assert m.recall == 0.0
    assert m.overconfident == 1


# --- judge-rejected entries (expected_verdict == "rejected") ----------------
# Same arithmetic as a control, DIFFERENT attribution: the rule is supposed to
# fire and the judge is supposed to kill it. Counting a hit here as
# overconfidence would make a rule that fires correctly indistinguishable from
# a rule that should never have fired.

def test_judge_rejected_excluded_from_recall_but_not_overconfident():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "tp", gt_id="rej-1")],
        total_known_bugs=1,
        control_ids=set(),
        excluded_ids={"rej-1"},
    )
    assert m.recall == 1.0        # rej-1 does not inflate it
    assert m.overconfident == 0   # and is not attributed as overconfidence


def test_control_and_rejected_attributed_separately():
    m = compute_metrics(
        [_v("t", "tp", gt_id="ctl-1"), _v("t", "tp", gt_id="rej-1")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
        excluded_ids={"ctl-1", "rej-1"},
    )
    assert m.recall == 0.0
    assert m.overconfident == 1   # the control only


def test_excluded_ids_defaults_to_controls():
    # Older callers pass control_ids only; excluded must fall back to it so
    # control matches still leave the numerator.
    m = compute_metrics(
        [_v("t", "tp", gt_id="ctl-1")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
    )
    assert m.recall == 0.0
    assert m.overconfident == 1
