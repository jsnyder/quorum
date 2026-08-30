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


# --- control hits must not credit precision ---------------------------------
# judge_auto() assigns verdict="tp" on a line match without consulting
# `expected`, so a control hit arrived here as a tp: excluded from recall,
# counted overconfident, and STILL raising precision and tp_count. The
# scorecard scolded the tool in one column and paid it in two others.

def test_control_hit_does_not_credit_precision():
    m = compute_metrics(
        [_v("t", "tp", gt_id="gt-1"), _v("t", "tp", gt_id="ctl-1"),
         _v("t", "fp"), _v("t", "fp")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
    )
    # Judgeable claims are 1 tp + 2 fp. The control is measured by
    # `overconfident`, not by precision, so it is 1/3 -- not 2/4.
    assert m.precision == 1 / 3
    assert m.tp_count == 1
    assert m.overconfident == 1


def test_control_hit_does_not_credit_partial_precision():
    m = compute_metrics(
        [_v("t", "partial", gt_id="ctl-1"), _v("t", "fp")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
    )
    assert m.partial_count == 0   # the control's partial leaves the tally
    assert m.precision == 0.0     # 0 effective tp over 1 judged fp
    assert m.overconfident == 1


def test_judge_rejected_hit_still_credits_precision():
    # Deliberate asymmetry: a judge-rejected rule IS supposed to fire, so the
    # finding is a legitimate claim and keeps its precision credit. Only
    # controls -- claims the source cannot support -- are withdrawn.
    m = compute_metrics(
        [_v("t", "tp", gt_id="rej-1"), _v("t", "fp")],
        total_known_bugs=1,
        control_ids=set(),
        excluded_ids={"rej-1"},
    )
    assert m.precision == 0.5
    assert m.tp_count == 1


def test_controls_are_excluded_even_if_caller_omits_them():
    # The comment on `excluded` promises controls are always part of it. Passing
    # an excluded_ids that omits a control used to leave that control in the
    # recall numerator while still counting it overconfident -- the exact
    # contradictory state the partition exists to prevent.
    m = compute_metrics(
        [_v("t", "tp", gt_id="ctl-1")],
        total_known_bugs=1,
        control_ids={"ctl-1"},
        excluded_ids={"rej-1"},   # caller forgot the control
    )
    assert m.recall == 0.0
    assert m.overconfident == 1


# --- _find_unique exclusions ------------------------------------------------
# These had zero coverage: stripping the exclusion filter from _find_unique
# passed the entire suite, so a silent regression there would change a reported
# number with nothing to catch it.

def test_unique_finds_ignores_controls():
    from scorecard import _find_unique
    # "t" is alone in reporting the control. That is the overconfidence signal,
    # counted there -- not a unique strength.
    verdicts = [
        _v("t", "tp", gt_id="ctl-1"),
        _v("t", "tp", gt_id="gt-1"),
        _v("other", "tp", gt_id="gt-1"),
    ]
    assert _find_unique("t", verdicts, {"ctl-1"}) == 0


def test_unique_finds_ignores_judge_rejected():
    from scorecard import _find_unique
    # A judge-rejected entry is not a bug, so being alone on it is not a find.
    verdicts = [_v("t", "tp", gt_id="rej-1")]
    assert _find_unique("t", verdicts, {"rej-1"}) == 0


def test_unique_finds_counts_scoreable_solo_hits():
    from scorecard import _find_unique
    verdicts = [
        _v("t", "tp", gt_id="gt-solo"),
        _v("t", "tp", gt_id="gt-shared"),
        _v("other", "tp", gt_id="gt-shared"),
    ]
    assert _find_unique("t", verdicts, {"ctl-1"}) == 1


# --- corpus/loader validation -----------------------------------------------
# _partition_corpus classifies with an if/elif/else whose last branch is a
# catch-all, so an unrecognised `expected` lands silently in `scoreable`,
# inflating the recall denominator with an unfindable bug.

def test_invalid_expected_is_rejected_at_load():
    import pytest
    from schema import GroundTruthEntry
    with pytest.raises(ValueError, match="expected="):
        GroundTruthEntry(
            id="x", type="real", title="t", category="c", severity="high",
            line_start=1, line_end=2, description="d", expected="Miss",
        )


def test_invalid_expected_verdict_is_rejected_at_load():
    import pytest
    from schema import GroundTruthEntry
    with pytest.raises(ValueError, match="expected_verdict="):
        GroundTruthEntry(
            id="x", type="real", title="t", category="c", severity="high",
            line_start=1, line_end=2, description="d",
            expected_verdict="Rejected",
        )


def test_valid_expected_values_still_load():
    from schema import GroundTruthEntry
    for exp in ("hit", "miss"):
        e = GroundTruthEntry(
            id="x", type="real", title="t", category="c", severity="high",
            line_start=1, line_end=2, description="d", expected=exp,
        )
        assert e.expected == exp


def test_duplicate_ids_across_files_are_rejected(tmp_path):
    import json, pytest
    from scorecard import _partition_corpus
    entry = {
        "id": "dup-1", "type": "real", "title": "t", "category": "c",
        "severity": "high", "line_start": 1, "line_end": 2, "description": "d",
    }
    (tmp_path / "a.ground_truth.json").write_text(json.dumps([entry]))
    (tmp_path / "b.ground_truth.json").write_text(json.dumps([entry]))
    with pytest.raises(ValueError, match="duplicate ground-truth id"):
        _partition_corpus(tmp_path)


def test_per_file_tp_agrees_with_summary_tp(tmp_path):
    # The per-file table counted raw verdicts while the summary withdrew control
    # hits, so a tool that hit a control reported a different TP in the two
    # tables of the same report.
    import json
    from scorecard import generate_scorecard
    entries = [
        {"id": "gt-1", "type": "real", "title": "t", "category": "c",
         "severity": "high", "line_start": 1, "line_end": 2, "description": "d"},
        {"id": "ctl-1", "type": "real", "title": "t", "category": "c",
         "severity": "high", "line_start": 3, "line_end": 4, "description": "d",
         "expected": "miss"},
    ]
    (tmp_path / "x.ground_truth.json").write_text(json.dumps(entries))
    verdicts = [_v("t", "tp", gt_id="gt-1"), _v("t", "tp", gt_id="ctl-1")]
    out = generate_scorecard(verdicts, {"t": []}, tmp_path)

    summary_tp = out["data"]["tools"]["t"]["tp_count"]
    assert summary_tp == 1
    per_file_row = [ln for ln in out["markdown"].splitlines()
                    if ln.startswith("| t |")][-1]
    assert per_file_row.split("|")[2].strip() == str(summary_tp)
