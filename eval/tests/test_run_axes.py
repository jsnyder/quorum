"""The per-axis scorer: what counts as a hit, a decoy hit, a duplicate, noise."""

import importlib.util
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("run_axes", HERE / "axes" / "run_axes.py")
assert spec is not None and spec.loader is not None
run_axes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run_axes)

GT = [
    {"id": "r1", "type": "real", "kind": "k", "title": "planted one", "line_start": 10, "line_end": 12},
    {"id": "r2", "type": "real", "kind": "k", "title": "planted two", "line_start": 30, "line_end": 30},
    {"id": "d1", "type": "decoy", "kind": "k", "title": "looks wrong, is not", "line_start": 50, "line_end": 55},
]


def finding(line, title="x", cited=None):
    f = {"title": title, "line_start": line, "line_end": line}
    if cited is not None:
        f["cited_lines"] = cited
    return f


def test_hit_decoy_noise_and_duplicate_are_told_apart():
    s = run_axes.score_file(
        [finding(11), finding(12, "second on same defect"), finding(52), finding(80)],
        GT,
    )
    assert [h["id"] for h in s["hits"]] == ["r1"]
    assert s["duplicates"] == 1
    assert [d["id"] for d in s["decoy_hits"]] == ["d1"]
    assert [n["line"] for n in s["noise"]] == [80]
    assert [m["id"] for m in s["missed"]] == ["r2"]
    assert s["planted"] == 2 and s["emitted"] == 4


def test_redacted_entries_are_neither_hit_nor_noise():
    gt = GT + [{"id": "x1", "type": "redacted", "title": "secret", "line_start": 70, "line_end": 70}]
    s = run_axes.score_file([finding(70)], gt)
    assert s["planted"] == 2 and s["hits"] == [] and s["noise"] == []


def test_a_span_covering_the_defect_hits_unless_it_is_very_wide():
    covering = {"title": "x", "line_start": 20, "line_end": 31}  # anchor 20, span reaches r2 at 30
    assert [h["id"] for h in run_axes.score_file([covering], GT)["hits"]] == ["r2"]
    wide = {"title": "x", "line_start": 20, "line_end": 70}  # 51 lines: anchor only, so no hit
    assert run_axes.score_file([wide], GT)["hits"] == []


def test_matching_does_not_depend_on_finding_order():
    gt = [
        {"id": "a", "type": "real", "kind": "k", "title": "a", "line_start": 28, "line_end": 31},
        {"id": "b", "type": "real", "kind": "k", "title": "b", "line_start": 34, "line_end": 36},
    ]
    # 33 could be either (26..33 and 32..38); 28 can only be `a`. Listed
    # ambiguous-first, greedy would give 33 -> a and strand 28.
    s = run_axes.score_file([finding(33), finding(28)], gt)
    assert sorted(h["id"] for h in s["hits"]) == ["a", "b"]
    assert s["duplicates"] == 0


def test_exit_3_is_not_retried(monkeypatch, tmp_path):
    calls = []

    class P:
        returncode, stdout, stderr = 3, "", "error: cannot load config"

    monkeypatch.setattr(run_axes.subprocess, "run", lambda cmd, **kw: calls.append(cmd) or P())
    _, meta = run_axes.run_quorum("quorum", tmp_path / "x.rs", "security", None, tmp_path)
    assert len(calls) == 1 and meta["retried"] is False and meta["error"] == "tool error"


def test_a_finding_near_two_defects_claims_the_unclaimed_one():
    gt = [
        {"id": "a", "type": "real", "kind": "k", "title": "a", "line_start": 28, "line_end": 31},
        {"id": "b", "type": "real", "kind": "k", "title": "b", "line_start": 34, "line_end": 36},
    ]
    # 33 is inside a's tolerance window (26..33) and b's (32..38); a is claimed first.
    s = run_axes.score_file([finding(28), finding(33)], gt)
    assert [h["id"] for h in s["hits"]] == ["a", "b"]
    assert s["duplicates"] == 0


def test_lane_violations_count_only_findings_on_the_other_axis_plants():
    raw = {"runs": [
        {"axis": "security", "corpus_axis": "performance", "meta": {"secs": 1},
         "score": {"planted": 3, "hits": [{"id": "p"}], "missed": [], "duplicates": 0,
                   "decoy_hits": [{"id": "d"}], "noise": [{"line": 1}, {"line": 2}], "emitted": 4}},
    ]}
    t = run_axes.aggregate(raw)["security"]
    assert (t["out_of_lane_findings"], t["lane_violations"]) == (4, 2)


def test_tolerance_is_two_lines_each_side():
    assert len(run_axes.score_file([finding(32)], GT)["hits"]) == 1
    assert len(run_axes.score_file([finding(33)], GT)["hits"]) == 0


def test_cited_lines_anchor_beats_line_start():
    # The finding's span starts far away but it cites the planted line.
    f = finding(1, cited={"start": 30, "end": 30})
    assert [h["id"] for h in run_axes.score_file([f], GT)["hits"]] == ["r2"]


def test_aggregate_separates_in_lane_from_out_of_lane():
    raw = {
        "runs": [
            {"axis": "security", "corpus_axis": "security", "meta": {"secs": 1},
             "score": {"planted": 2, "hits": [{"id": "a"}], "missed": [{"id": "b"}], "duplicates": 0,
                       "decoy_hits": [], "noise": [{"line": 9}], "emitted": 2}},
            {"axis": "security", "corpus_axis": "performance", "meta": {"secs": 1, "error": "x"},
             "score": {"planted": 3, "hits": [{"id": "p"}], "missed": [], "duplicates": 0,
                       "decoy_hits": [], "noise": [], "emitted": 4}},
        ]
    }
    t = run_axes.aggregate(raw)["security"]
    assert (t["planted"], t["hits"], t["recall"]) == (2, 1, 0.5)
    assert (t["emitted_in_lane"], t["precision"], t["noise"]) == (2, 0.5, 1)
    assert (t["out_of_lane_findings"], t["out_of_lane_files"], t["errors"]) == (4, 1, 1)
    assert t["lane_violations"] == 1
    assert run_axes.aggregate(raw)["performance"]["recall"] is None


def test_every_corpus_file_has_well_formed_ground_truth():
    files = run_axes.corpus_files()
    assert len(files) >= 12
    seen = set()
    for _axis, path, gt in files:
        n = len(path.read_text().splitlines())
        reals = [g for g in gt if g["type"] == "real"]
        assert reals, f"{path} plants nothing"
        for g in gt:
            assert g["type"] in ("real", "decoy", "redacted"), g
            assert g["id"] not in seen, f"duplicate id {g['id']}"
            seen.add(g["id"])
            assert 1 <= g["line_start"] <= g["line_end"] <= n, f"{g['id']} out of range for {path.name} ({n} lines)"
            if g["type"] == "real":
                assert g["category"], g["id"]


def test_a_network_error_cell_is_retried_once(monkeypatch, tmp_path):
    calls = []

    class P:
        def __init__(self, rc, out, err):
            self.returncode, self.stdout, self.stderr = rc, out, err

    def fake_run(cmd, **kw):
        calls.append(cmd)
        if len(calls) == 1:
            return P(1, "[]", "Warning: 1 of 1 skill axes failed on x.rs: security/m (network_error)")
        return P(0, '[{"file": "x.rs", "findings": [{"title": "t", "line_start": 1, "line_end": 1, "originating_skill": "security"}]}]', "")

    monkeypatch.setattr(run_axes.subprocess, "run", fake_run)
    findings, meta = run_axes.run_quorum("quorum", tmp_path / "x.rs", "security", None, tmp_path)
    assert len(calls) == 2 and meta["retried"] is True
    assert [f["title"] for f in findings] == ["t"] and "error" not in meta


def test_results_re_score_regardless_of_the_checkout_they_were_recorded_in():
    assert run_axes.corpus_key("/old/worktree/eval/axes/corpus/security/auth.rs") == "security/auth.rs"
    assert run_axes.corpus_key("security/auth.rs") == "security/auth.rs"


def test_a_hung_cell_is_a_per_file_error_not_a_crash(monkeypatch, tmp_path):
    def hang(cmd, **kw):
        raise run_axes.subprocess.TimeoutExpired(cmd, kw.get("timeout", 600))

    monkeypatch.setattr(run_axes.subprocess, "run", hang)
    findings, meta = run_axes.run_quorum("quorum", tmp_path / "x.rs", "security", None, tmp_path)
    assert findings == [] and meta["error"] == "timeout"
