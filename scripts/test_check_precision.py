#!/usr/bin/env python3
"""Tests for check_precision.py. Run: python3 scripts/test_check_precision.py"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from check_precision import (  # noqa: E402
    MIN_WEEK_COUNT,
    classify_drop,
    extract_recent_precision,
    load_baseline,
    save_baseline,
)


def make_stats(precision, trend, tp=100, fp=30):
    return {
        "precision": precision,
        "tp": tp,
        "fp": fp,
        "feedback_count": tp + fp,
        "precision_trend": trend,
    }


class ExtractRecentPrecisionTests(unittest.TestCase):
    def test_uses_latest_week_with_enough_samples(self):
        trend = [
            {"count": 50, "precision": 0.80, "week_start": "2026-04-11T00:00:00+00:00"},
            {"count": 100, "precision": 0.72, "week_start": "2026-04-18T00:00:00+00:00"},
        ]
        stats = make_stats(0.76, trend)
        r = extract_recent_precision(stats)
        self.assertEqual(r.source, "week:2026-04-18")
        self.assertAlmostEqual(r.precision, 0.72)
        self.assertEqual(r.sample_count, 100)

    def test_skips_thin_recent_weeks(self):
        # Most recent week has too few samples — fall back to the previous week.
        trend = [
            {"count": 50, "precision": 0.80, "week_start": "2026-04-11T00:00:00+00:00"},
            {"count": 5, "precision": 0.40, "week_start": "2026-04-18T00:00:00+00:00"},
        ]
        stats = make_stats(0.76, trend)
        r = extract_recent_precision(stats)
        self.assertEqual(r.source, "week:2026-04-11")
        self.assertAlmostEqual(r.precision, 0.80)

    def test_falls_back_to_overall_when_no_rich_week(self):
        trend = [
            {"count": 3, "precision": 0.80, "week_start": "2026-04-18T00:00:00+00:00"},
        ]
        stats = make_stats(0.76, trend, tp=100, fp=30)
        r = extract_recent_precision(stats)
        self.assertEqual(r.source, "overall")
        self.assertAlmostEqual(r.precision, 0.76)
        self.assertEqual(r.sample_count, 130)

    def test_empty_trend_falls_back_to_overall(self):
        stats = make_stats(0.76, [], tp=100, fp=30)
        r = extract_recent_precision(stats)
        self.assertEqual(r.source, "overall")

    def test_min_week_count_boundary_inclusive(self):
        # A week with exactly MIN_WEEK_COUNT should qualify.
        trend = [
            {"count": MIN_WEEK_COUNT, "precision": 0.70,
             "week_start": "2026-04-18T00:00:00+00:00"},
        ]
        stats = make_stats(0.76, trend)
        r = extract_recent_precision(stats)
        self.assertEqual(r.source, "week:2026-04-18")


class ClassifyDropTests(unittest.TestCase):
    def test_ok_when_current_above_baseline(self):
        code, label = classify_drop(0.75, 0.78)
        self.assertEqual(code, 0)
        self.assertEqual(label, "OK")

    def test_ok_for_small_drop_within_warn_threshold(self):
        # 2pp drop — below 3pp warn.
        code, _ = classify_drop(0.75, 0.73)
        self.assertEqual(code, 0)

    def test_warn_at_3pp_drop(self):
        code, label = classify_drop(0.75, 0.72)
        self.assertEqual(code, 1)
        self.assertEqual(label, "WARN")

    def test_critical_at_5pp_drop(self):
        code, label = classify_drop(0.75, 0.70)
        self.assertEqual(code, 2)
        self.assertEqual(label, "CRITICAL")

    def test_critical_beats_warn_when_both_would_match(self):
        # 10pp drop qualifies for both; must return CRITICAL.
        code, _ = classify_drop(0.80, 0.70)
        self.assertEqual(code, 2)

    def test_zero_drop_is_ok(self):
        code, _ = classify_drop(0.75, 0.75)
        self.assertEqual(code, 0)


class BaselinePersistenceTests(unittest.TestCase):
    def test_save_and_load_roundtrip(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "nested" / "baseline.json"
            data = {"precision": 0.76, "feedback_count": 1643}
            save_baseline(path, data)
            self.assertEqual(load_baseline(path), data)

    def test_load_missing_returns_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "nope.json"
            self.assertIsNone(load_baseline(path))

    def test_load_malformed_returns_none(self):
        # Corrupt baseline must not crash — act as if absent so operator can reset.
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bad.json"
            path.write_text("{not valid json")
            self.assertIsNone(load_baseline(path))


if __name__ == "__main__":
    unittest.main(verbosity=2)
