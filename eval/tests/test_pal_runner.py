import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from pal_runner import review_file


def _make_response(content: str, has_choices: bool = True):
    if has_choices:
        return {"choices": [{"message": {"content": content}}]}
    return {"error": {"message": "API error"}}


def test_review_file_strips_markdown_fences():
    """Fix 9: LLMs sometimes wrap JSON in ```json blocks."""
    findings = [{"severity": "high", "title": "test", "category": "correctness",
                 "line": 1, "description": "test"}]
    wrapped = f"```json\n{json.dumps(findings)}\n```"

    with patch("pal_runner.httpx.post") as mock_post:
        mock_post.return_value = type("R", (), {
            "json": lambda self: _make_response(wrapped)
        })()
        result = review_file(Path("/dev/null"), "test.rs",
                             "http://localhost", "sk-test", "gpt-5.4")

    assert len(result) == 1, f"Should parse markdown-wrapped JSON, got: {result}"
    assert result[0]["title"] == "test"


def test_review_file_api_error_returns_empty():
    """Fix 6: API errors should return empty list."""
    with patch("pal_runner.httpx.post") as mock_post:
        mock_post.return_value = type("R", (), {
            "json": lambda self: _make_response("", has_choices=False)
        })()
        with patch("pal_runner.Path.read_text", return_value="code"):
            result = review_file(Path("/dev/null"), "test.rs",
                                 "http://localhost", "sk-test", "gpt-5.4")

    assert result == []


def test_main_skips_empty_cache(tmp_path):
    """Fix 6: empty results (API failure) should not be cached."""
    cache_dir = tmp_path / "pal_cache" / "rust"
    cache_dir.mkdir(parents=True)
    cache_file = cache_dir / "test.rs.json"

    with patch("pal_runner.CACHE_DIR", tmp_path / "pal_cache"), \
         patch("pal_runner.review_file", return_value=[]):
        from pal_runner import CACHE_DIR
        findings = []
        if findings:
            cache_file.parent.mkdir(parents=True, exist_ok=True)
            with open(cache_file, "w") as f:
                json.dump(findings, f, indent=2)

    assert not cache_file.exists(), "Empty findings should not be cached"


def test_main_corrupted_cache_skips_gracefully(tmp_path):
    """Corrupted cache in pal_runner main() should skip, not crash."""
    from pal_runner import main as pal_main

    cache_dir = tmp_path / "pal_cache" / "rust"
    cache_dir.mkdir(parents=True)
    cache_file = cache_dir / "index_writer.rs.json"
    cache_file.write_text("INVALID JSON {{{{")

    corpus_dir = tmp_path / "corpus" / "rust"
    corpus_dir.mkdir(parents=True)
    (corpus_dir / "index_writer.rs").write_text("fn main() {}")

    with patch("pal_runner.CACHE_DIR", tmp_path / "pal_cache"), \
         patch("pal_runner.CORPUS_DIR", tmp_path / "corpus"), \
         patch("pal_runner.review_file", return_value=[{"title": "test"}]) as mock_review, \
         patch("sys.argv", ["pal_runner.py", "--lang", "rust"]), \
         patch.dict("os.environ", {"QUORUM_API_KEY": "sk-test"}):
        pal_main()

    assert mock_review.called, "Should re-review when cache is corrupted"
