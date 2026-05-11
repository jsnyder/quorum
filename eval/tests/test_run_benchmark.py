import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from run_benchmark import run_quorum, run_pal


def test_quorum_home_tempdir_cleaned_up():
    """Fix 2: tempfile.mkdtemp() in run_quorum must be cleaned up after use."""
    created_dirs = []
    original_mkdtemp = tempfile.mkdtemp

    def tracking_mkdtemp(**kwargs):
        d = original_mkdtemp(**kwargs)
        created_dirs.append(d)
        return d

    fake_binary = Path("/usr/bin/true")
    fake_file = Path("/dev/null")

    with patch("run_benchmark.tempfile.mkdtemp", side_effect=tracking_mkdtemp), \
         patch("run_benchmark.subprocess.run") as mock_run:
        mock_run.return_value = type("R", (), {
            "returncode": 0, "stdout": "[]", "stderr": ""
        })()
        run_quorum(fake_binary, fake_file, "v0.21.0")

    assert len(created_dirs) == 1
    assert not Path(created_dirs[0]).exists(), \
        f"QUORUM_HOME temp dir was not cleaned up: {created_dirs[0]}"


def test_quorum_exit_code_1_returns_findings():
    """Fix 3: exit code 1 (warnings) should still parse stdout, not return []."""
    findings_json = json.dumps([{
        "file": "test.rs",
        "findings": [{"title": "test", "severity": "high",
                       "category": "correctness", "line_start": 1,
                       "line_end": 1, "description": "test"}]
    }])

    with patch("run_benchmark.tempfile.mkdtemp", return_value=tempfile.mkdtemp()), \
         patch("run_benchmark.subprocess.run") as mock_run:
        mock_run.return_value = type("R", (), {
            "returncode": 1, "stdout": findings_json, "stderr": ""
        })()
        result = run_quorum(Path("/usr/bin/true"), Path("/dev/null"), "v0.21.0")

    assert len(result) > 0, "exit code 1 should still return parsed findings"


def test_quorum_exit_code_2_returns_findings():
    """Fix 3: exit code 2 (critical) should still parse stdout, not return []."""
    findings_json = json.dumps([{
        "file": "test.rs",
        "findings": [{"title": "critical bug", "severity": "critical",
                       "category": "security", "line_start": 5,
                       "line_end": 5, "description": "critical"}]
    }])

    with patch("run_benchmark.tempfile.mkdtemp", return_value=tempfile.mkdtemp()), \
         patch("run_benchmark.subprocess.run") as mock_run:
        mock_run.return_value = type("R", (), {
            "returncode": 2, "stdout": findings_json, "stderr": ""
        })()
        result = run_quorum(Path("/usr/bin/true"), Path("/dev/null"), "v0.21.0")

    assert len(result) > 0, "exit code 2 should still return parsed findings"


def test_pal_cache_corrupted_json():
    """Fix 7: corrupted cache file should not crash, should return []."""
    with tempfile.TemporaryDirectory() as tmpdir:
        cache_dir = Path(tmpdir) / "pal_cache" / "rust"
        cache_dir.mkdir(parents=True)
        cache_file = cache_dir / "test.rs.json"
        cache_file.write_text("NOT VALID JSON {{{")

        with patch("run_benchmark.EVAL_DIR", Path(tmpdir)):
            result = run_pal(Path("/dev/null"), "rust/test.rs")

    assert result == [], "corrupted cache should return [] not crash"
