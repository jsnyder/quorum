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
