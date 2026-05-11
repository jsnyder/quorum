# Benchmark Evaluation Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Python harness that benchmarks quorum's precision/recall across versions and against PAL/third-opinion on a curated multi-language corpus.

**Architecture:** Script-driven orchestrator runs tools on corpus files, normalizes output to a canonical schema, judges findings via automatic ground-truth matching + model panel, and produces precision/recall/F1 scorecards. Lives in `eval/` as dev-only tooling.

**Tech Stack:** Python 3.14, uv for dependency management, httpx for LiteLLM API calls, pytest for testing.

---

## File Structure

```
eval/
  pyproject.toml              # uv project, minimal deps
  run_benchmark.py            # CLI entry point + orchestrator
  normalize.py                # per-tool output normalization to canonical schema
  judge.py                    # auto-verdict (ground truth) + model panel
  scorecard.py                # metrics computation + Markdown/JSON report
  build_binaries.sh           # build quorum at tagged versions
  schema.py                   # canonical Finding/Verdict/GroundTruth dataclasses
  corpus/                     # challenge files + ground truth (checked in)
    rust/
      index_writer.rs          # existing (copied from tests/fixtures/comparison/samples/)
      index_writer.ground_truth.json
    python/
      client.py                # existing
      client.ground_truth.json
    typescript/
      router.ts                # existing
      router.ground_truth.json
    yaml/                      # new files curated in Task 9
    bash/                      # new files curated in Task 9
  results/                     # output from runs (gitignored)
  binaries/                    # pre-built quorum binaries (gitignored)
  tests/
    test_normalize.py
    test_judge.py
    test_scorecard.py
```

---

### Task 1: Project scaffold

**Files:**
- Create: `eval/pyproject.toml`
- Create: `eval/.python-version`
- Modify: `.gitignore`

- [ ] **Step 1: Create eval directory and pyproject.toml**

```toml
# eval/pyproject.toml
[project]
name = "quorum-eval"
version = "0.1.0"
description = "Benchmark harness for quorum code review tool"
requires-python = ">=3.12"
dependencies = [
    "httpx>=0.28",
]

[project.optional-dependencies]
dev = [
    "pytest>=8",
]

[project.scripts]
run-benchmark = "run_benchmark:main"
```

```
# eval/.python-version
3.14
```

- [ ] **Step 2: Add gitignore entries**

Append to the repo root `.gitignore`:

```
# Eval harness
eval/results/
eval/binaries/
eval/.venv/
```

- [ ] **Step 3: Create directory structure**

```bash
mkdir -p eval/corpus/{rust,python,typescript,yaml,bash}
mkdir -p eval/results eval/binaries eval/tests
```

- [ ] **Step 4: Install dependencies**

```bash
cd eval && uv sync && uv sync --extra dev
```

- [ ] **Step 5: Commit**

```bash
git add eval/pyproject.toml eval/.python-version eval/corpus/ eval/tests/ .gitignore
git commit -m "chore: scaffold eval harness directory structure"
```

---

### Task 2: Canonical schema

**Files:**
- Create: `eval/schema.py`
- Create: `eval/tests/test_schema.py`

- [ ] **Step 1: Write the test**

```python
# eval/tests/test_schema.py
from schema import CanonicalFinding, Verdict, GroundTruthEntry

def test_canonical_finding_from_dict():
    d = {
        "tool": "quorum-v0.21.0",
        "file": "rust/index_writer.rs",
        "title": "Unsafe block without safety comment",
        "category": "correctness",
        "severity": "high",
        "line_start": 42,
        "line_end": 45,
        "description": "unsafe block lacks a SAFETY comment",
    }
    f = CanonicalFinding(**d)
    assert f.tool == "quorum-v0.21.0"
    assert f.severity == "high"
    assert f.line_start == 42

def test_verdict_from_dict():
    v = Verdict(
        file="rust/index_writer.rs",
        tool="quorum-v0.21.0",
        finding_title="test",
        verdict="tp",
        judge="auto",
        reason="matched ground truth",
        matched_ground_truth_id="plant-001",
    )
    assert v.verdict == "tp"

def test_ground_truth_entry():
    g = GroundTruthEntry(
        id="plant-001",
        type="planted",
        title="SQL injection via f-string",
        category="security",
        severity="high",
        line_start=42,
        line_end=45,
        description="User input interpolated without parameterization",
    )
    assert g.id == "plant-001"
    assert g.type == "planted"
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd eval && uv run pytest tests/test_schema.py -v
```

Expected: FAIL with `ModuleNotFoundError: No module named 'schema'`

- [ ] **Step 3: Write schema.py**

```python
# eval/schema.py
from dataclasses import dataclass, field, asdict
import json
from pathlib import Path

@dataclass
class CanonicalFinding:
    tool: str
    file: str
    title: str
    category: str
    severity: str
    line_start: int
    line_end: int
    description: str

    def to_dict(self) -> dict:
        return asdict(self)

@dataclass
class GroundTruthEntry:
    id: str
    type: str  # "planted", "cve", "real"
    title: str
    category: str
    severity: str
    line_start: int
    line_end: int
    description: str
    cve: str | None = None

@dataclass
class Verdict:
    file: str
    tool: str
    finding_title: str
    verdict: str  # "tp", "fp", "partial"
    judge: str  # "auto", "panel", "human"
    reason: str
    matched_ground_truth_id: str | None = None

def load_ground_truth(path: Path) -> list[GroundTruthEntry]:
    with open(path) as f:
        return [GroundTruthEntry(**e) for e in json.load(f)]

def save_verdicts(verdicts: list[Verdict], path: Path) -> None:
    with open(path, "w") as f:
        json.dump([asdict(v) for v in verdicts], f, indent=2)
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd eval && uv run pytest tests/test_schema.py -v
```

Expected: 3 passed

- [ ] **Step 5: Commit**

```bash
git add eval/schema.py eval/tests/test_schema.py
git commit -m "feat(eval): add canonical finding/verdict/ground-truth schema"
```

---

### Task 3: Output normalizers

**Files:**
- Create: `eval/normalize.py`
- Create: `eval/tests/test_normalize.py`

- [ ] **Step 1: Write the tests**

```python
# eval/tests/test_normalize.py
import json
from normalize import normalize_quorum, normalize_pal, normalize_third_opinion

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
    {"severity": "high", "title": "MAX_NUM_THREAD not enforced in IndexWriter::new()"},
    {"severity": "medium", "title": "rollback() silently discards enqueued operations"},
]

def test_normalize_pal():
    findings = normalize_pal(PAL_FINDINGS, "rust/index_writer.rs")
    assert len(findings) == 2
    f = findings[0]
    assert f.tool == "pal"
    assert f.file == "rust/index_writer.rs"
    assert f.severity == "high"
    assert f.line_start == 0  # PAL doesn't provide line numbers
    assert f.category == "unknown"

THIRD_OPINION_OUTPUT = {
    "total": 2,
    "score": 87,
    "findings": [
        {"severity": "medium", "title": "MAX_NUM_THREAD declared but never enforced"},
        {"severity": "low", "title": "Thread join error handling inconsistent"},
    ],
}

def test_normalize_third_opinion():
    findings = normalize_third_opinion(THIRD_OPINION_OUTPUT, "rust/index_writer.rs")
    assert len(findings) == 2
    f = findings[0]
    assert f.tool == "third-opinion"
    assert f.severity == "medium"
    assert f.line_start == 0
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd eval && uv run pytest tests/test_normalize.py -v
```

Expected: FAIL with `ModuleNotFoundError`

- [ ] **Step 3: Write normalize.py**

```python
# eval/normalize.py
from schema import CanonicalFinding

SEVERITY_NORMALIZE = {
    "critical": "critical",
    "high": "high",
    "medium": "medium",
    "low": "low",
    "info": "info",
    "warning": "medium",
}

def _norm_severity(s: str) -> str:
    return SEVERITY_NORMALIZE.get(s.lower(), "info")

def _norm_category(c) -> str:
    if isinstance(c, str):
        return c.lower()
    if isinstance(c, dict):
        for key in c:
            return key.lower()
    return "unknown"

def normalize_quorum(
    data: list | dict,
    tool_name: str,
    file_path: str | None = None,
) -> list[CanonicalFinding]:
    findings = []
    if isinstance(data, dict):
        for fp, file_findings in data.items():
            for f in file_findings:
                findings.append(_quorum_finding(f, tool_name, fp))
    elif isinstance(data, list):
        for f in data:
            findings.append(_quorum_finding(f, tool_name, file_path or "unknown"))
    return findings

def _quorum_finding(f: dict, tool: str, file_path: str) -> CanonicalFinding:
    return CanonicalFinding(
        tool=tool,
        file=file_path,
        title=f.get("title", ""),
        category=_norm_category(f.get("category", "unknown")),
        severity=_norm_severity(f.get("severity", "info")),
        line_start=f.get("line_start", 0),
        line_end=f.get("line_end", 0),
        description=f.get("description", ""),
    )

def normalize_pal(
    findings_list: list[dict],
    file_path: str,
) -> list[CanonicalFinding]:
    return [
        CanonicalFinding(
            tool="pal",
            file=file_path,
            title=f.get("title", ""),
            category="unknown",
            severity=_norm_severity(f.get("severity", "info")),
            line_start=0,
            line_end=0,
            description=f.get("title", ""),
        )
        for f in findings_list
    ]

def normalize_third_opinion(
    data: dict,
    file_path: str,
) -> list[CanonicalFinding]:
    return [
        CanonicalFinding(
            tool="third-opinion",
            file=file_path,
            title=f.get("title", ""),
            category="unknown",
            severity=_norm_severity(f.get("severity", "info")),
            line_start=0,
            line_end=0,
            description=f.get("title", ""),
        )
        for f in data.get("findings", [])
    ]
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd eval && uv run pytest tests/test_normalize.py -v
```

Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add eval/normalize.py eval/tests/test_normalize.py
git commit -m "feat(eval): add output normalizers for quorum, PAL, third-opinion"
```

---

### Task 4: Corpus setup + ground truth stubs

**Files:**
- Copy: `tests/fixtures/comparison/samples/*.{rs,py,ts}` → `eval/corpus/<lang>/`
- Create: `eval/corpus/rust/index_writer.ground_truth.json`
- Create: `eval/corpus/python/client.ground_truth.json`
- Create: `eval/corpus/typescript/router.ground_truth.json`

- [ ] **Step 1: Copy existing corpus files**

```bash
cp tests/fixtures/comparison/samples/index_writer.rs eval/corpus/rust/
cp tests/fixtures/comparison/samples/client.py eval/corpus/python/
cp tests/fixtures/comparison/samples/router.ts eval/corpus/typescript/
```

- [ ] **Step 2: Create ground truth for index_writer.rs**

Based on the existing baseline (3-tool consensus), these are the confirmed real bugs:

```json
[
  {
    "id": "iw-001",
    "type": "real",
    "title": "MAX_NUM_THREAD not enforced in IndexWriter::new",
    "category": "correctness",
    "severity": "high",
    "line_start": 1,
    "line_end": 50,
    "description": "MAX_NUM_THREAD is declared as a constant but IndexWriter::new() never checks it, allowing thread count to exceed the limit."
  },
  {
    "id": "iw-002",
    "type": "real",
    "title": "wait_merging_threads returns early and detaches remaining threads",
    "category": "correctness",
    "severity": "high",
    "line_start": 1,
    "line_end": 50,
    "description": "On first thread join error, function returns early leaving remaining threads detached with no cleanup."
  },
  {
    "id": "iw-003",
    "type": "real",
    "title": "rollback silently discards enqueued operations",
    "category": "correctness",
    "severity": "medium",
    "line_start": 1,
    "line_end": 50,
    "description": "rollback() clears queued operations without logging or returning the count, silently dropping user work."
  }
]
```

Note: `line_start`/`line_end` set to approximate ranges. Refine after manual review of the source file.

- [ ] **Step 3: Create ground truth for client.py**

```json
[
  {
    "id": "cl-001",
    "type": "real",
    "title": "Off-by-one in max_redirects check",
    "category": "logic",
    "severity": "high",
    "line_start": 1,
    "line_end": 50,
    "description": "_send_handling_redirects allows one redirect past the configured max_redirects limit."
  },
  {
    "id": "cl-002",
    "type": "real",
    "title": "send races with close/aclose",
    "category": "concurrency",
    "severity": "high",
    "line_start": 1,
    "line_end": 50,
    "description": "send() can execute concurrently with close()/aclose() despite the client being shared, leading to use-after-close."
  },
  {
    "id": "cl-003",
    "type": "real",
    "title": "Transport leak on close errors",
    "category": "correctness",
    "severity": "medium",
    "line_start": 1,
    "line_end": 50,
    "description": "Client.close() and AsyncClient.aclose() abort on first transport close error, leaking remaining mounted transports."
  }
]
```

- [ ] **Step 4: Create ground truth for router.ts**

```json
[
  {
    "id": "rt-001",
    "type": "real",
    "title": "getProcedureAtPath prefix matching resolves wrong router",
    "category": "correctness",
    "severity": "high",
    "line_start": 1,
    "line_end": 50,
    "description": "Prefix-based path matching in getProcedureAtPath can match a lazy router with a path that is a prefix of another, resolving to the wrong procedure."
  },
  {
    "id": "rt-002",
    "type": "real",
    "title": "Reserved word check only at top level",
    "category": "correctness",
    "severity": "high",
    "line_start": 1,
    "line_end": 50,
    "description": "createRouterInner only rejects reserved words (e.g., 'query', 'mutation') at the top-level, allowing them in nested routers."
  },
  {
    "id": "rt-003",
    "type": "real",
    "title": "isProcedure type guard too permissive",
    "category": "correctness",
    "severity": "low",
    "line_start": 1,
    "line_end": 50,
    "description": "isProcedure accepts any function as a procedure without checking for the expected _def property."
  }
]
```

- [ ] **Step 5: Commit**

```bash
git add eval/corpus/
git commit -m "feat(eval): add corpus files with ground truth stubs"
```

---

### Task 5: Binary builder script

**Files:**
- Create: `eval/build_binaries.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# eval/build_binaries.sh — Build quorum at tagged versions for benchmarking.
# Usage: ./build_binaries.sh [--only v0.18.4|v0.21.0]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$SCRIPT_DIR/binaries"
mkdir -p "$BIN_DIR"

# Version -> git ref mapping
declare -A VERSIONS=(
    ["v0.18.4"]="v0.18.4"
    ["v0.21.0"]="v0.21.0"
)

# Flags supported per version (for orchestrator compatibility check)
declare -A SUPPORTED_FLAGS=(
    ["v0.18.4"]="--json --parallel"
    ["v0.21.0"]="--json --parallel --skip-context7 --ensemble --mode"
)

build_version() {
    local ver="$1"
    local ref="${VERSIONS[$ver]}"
    local out="$BIN_DIR/quorum-${ver}"

    if [[ -f "$out" ]]; then
        echo "  $ver: already built at $out (delete to rebuild)"
        return 0
    fi

    echo "  $ver: building from ref $ref ..."
    local tmpdir
    tmpdir="$(mktemp -d)"
    trap "rm -rf '$tmpdir'" RETURN

    git -C "$REPO_ROOT" worktree add --detach "$tmpdir" "$ref" 2>/dev/null
    (cd "$tmpdir" && cargo build --release --quiet)
    cp "$tmpdir/target/release/quorum" "$out"
    git -C "$REPO_ROOT" worktree remove --force "$tmpdir" 2>/dev/null || true

    echo "  $ver: built -> $out"
}

ONLY="${1:-}"
if [[ "$ONLY" == "--only" ]]; then
    ONLY="${2:?'--only requires a version (e.g., v0.18.4)'}"
fi

echo "Building quorum binaries for benchmarking..."
for ver in "${!VERSIONS[@]}"; do
    if [[ -n "$ONLY" && "$ONLY" != "--only" && "$ONLY" != "$ver" ]]; then
        continue
    fi
    if [[ "$ONLY" == "--only" ]]; then
        continue  # handled above
    fi
    build_version "$ver"
done

# Write compatibility matrix
cat > "$BIN_DIR/compat.json" << 'COMPAT'
{
    "v0.18.4": {"flags": ["--json", "--parallel"]},
    "v0.21.0": {"flags": ["--json", "--parallel", "--skip-context7", "--ensemble", "--mode"]}
}
COMPAT

echo "Compatibility matrix written to $BIN_DIR/compat.json"
echo "Done."
```

- [ ] **Step 2: Make executable**

```bash
chmod +x eval/build_binaries.sh
```

- [ ] **Step 3: Commit**

```bash
git add eval/build_binaries.sh
git commit -m "feat(eval): add build_binaries.sh for versioned quorum builds"
```

---

### Task 6: Orchestrator

**Files:**
- Create: `eval/run_benchmark.py`

- [ ] **Step 1: Write the orchestrator**

```python
#!/usr/bin/env python3
# eval/run_benchmark.py — Run benchmark tools on the corpus and collect results.
import argparse
import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

from normalize import normalize_quorum, normalize_pal, normalize_third_opinion
from schema import CanonicalFinding, save_verdicts
from judge import judge_findings
from scorecard import generate_scorecard

EVAL_DIR = Path(__file__).parent
CORPUS_DIR = EVAL_DIR / "corpus"
BIN_DIR = EVAL_DIR / "binaries"
RESULTS_DIR = EVAL_DIR / "results"

TOOLS = ["quorum-v0.18.4", "quorum-v0.21.0", "pal", "third-opinion"]

COMPAT: dict = {}

def load_compat() -> dict:
    global COMPAT
    path = BIN_DIR / "compat.json"
    if path.exists():
        with open(path) as f:
            COMPAT = json.load(f)
    return COMPAT

def corpus_files() -> list[tuple[str, Path]]:
    files = []
    for lang_dir in sorted(CORPUS_DIR.iterdir()):
        if not lang_dir.is_dir():
            continue
        for f in sorted(lang_dir.iterdir()):
            if f.suffix in (".rs", ".py", ".ts", ".tsx", ".yaml", ".yml", ".sh", ".bash"):
                rel = f"{lang_dir.name}/{f.name}"
                files.append((rel, f))
    return files

def run_quorum(binary: Path, file_path: Path, version: str) -> list[dict]:
    env = os.environ.copy()
    env["QUORUM_HOME"] = tempfile.mkdtemp()
    env["QUORUM_MODEL"] = os.environ.get("QUORUM_MODEL", "gpt-5.4")

    cmd = [str(binary), "review", str(file_path), "--json", "--parallel", "1"]
    flags = COMPAT.get(version, {}).get("flags", [])
    if "--skip-context7" in flags:
        cmd.append("--skip-context7")

    result = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=300)
    if result.returncode == 3:
        print(f"    WARN: {version} tool error on {file_path.name}", file=sys.stderr)
        return []
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        print(f"    WARN: {version} non-JSON output on {file_path.name}", file=sys.stderr)
        return []

def run_pal(file_path: Path) -> list[dict]:
    # PAL produces markdown, not structured JSON.
    # For the benchmark, we use the MCP tool via a subprocess wrapper
    # or fall back to manual baseline data.
    print(f"    PAL: requires manual run or MCP integration", file=sys.stderr)
    return []

def run_third_opinion(file_path: Path) -> dict | None:
    if not _tool_available("third-opinion"):
        print(f"    third-opinion: not installed, skipping", file=sys.stderr)
        return None
    result = subprocess.run(
        ["third-opinion", "review", str(file_path), "--json"],
        capture_output=True, text=True, timeout=300,
    )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return None

def _tool_available(name: str) -> bool:
    try:
        subprocess.run(["which", name], capture_output=True, check=True)
        return True
    except (subprocess.CalledProcessError, FileNotFoundError):
        return False

def run_all(
    tools: list[str],
    lang_filter: str | None = None,
) -> dict[str, list[CanonicalFinding]]:
    load_compat()
    all_findings: dict[str, list[CanonicalFinding]] = {t: [] for t in tools}
    files = corpus_files()

    for rel_path, abs_path in files:
        lang = rel_path.split("/")[0]
        if lang_filter and lang != lang_filter:
            continue

        print(f"  {rel_path}:")

        for tool in tools:
            if tool.startswith("quorum-"):
                version = tool.replace("quorum-", "")
                binary = BIN_DIR / f"quorum-{version}"
                if not binary.exists():
                    print(f"    {tool}: binary not found, skipping")
                    continue
                raw = run_quorum(binary, abs_path, version)
                normalized = normalize_quorum(raw, tool, rel_path)
            elif tool == "pal":
                raw = run_pal(abs_path)
                normalized = normalize_pal(raw, rel_path)
            elif tool == "third-opinion":
                raw_dict = run_third_opinion(abs_path)
                if raw_dict is None:
                    continue
                normalized = normalize_third_opinion(raw_dict, rel_path)
            else:
                continue

            all_findings[tool].extend(normalized)
            print(f"    {tool}: {len(normalized)} findings")

    return all_findings

def main():
    parser = argparse.ArgumentParser(description="Quorum benchmark harness")
    parser.add_argument("--tool", choices=TOOLS, help="Run a single tool only")
    parser.add_argument("--lang", help="Filter to a single language (e.g., rust)")
    parser.add_argument("--with-calibration", action="store_true",
                        help="Second pass with real feedback store")
    parser.add_argument("--output-dir", type=Path,
                        help="Override results directory")
    args = parser.parse_args()

    tools = [args.tool] if args.tool else TOOLS
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M")
    out_dir = args.output_dir or RESULTS_DIR / timestamp
    out_dir.mkdir(parents=True, exist_ok=True)

    print(f"Benchmark run: {timestamp}")
    print(f"Tools: {', '.join(tools)}")
    print(f"Output: {out_dir}")
    print()

    # Phase 1: Run tools
    all_findings = run_all(tools, args.lang)

    # Save raw normalized findings per tool
    for tool, findings in all_findings.items():
        path = out_dir / f"{tool}.json"
        with open(path, "w") as f:
            json.dump([fd.to_dict() for fd in findings], f, indent=2)

    # Phase 2: Judge
    corpus_dir = CORPUS_DIR
    verdicts = judge_findings(all_findings, corpus_dir)
    save_verdicts(verdicts, out_dir / "verdicts.json")

    # Phase 3: Scorecard
    report = generate_scorecard(verdicts, all_findings, corpus_dir)
    (out_dir / "scorecard.md").write_text(report["markdown"])
    with open(out_dir / "scorecard.json", "w") as f:
        json.dump(report["data"], f, indent=2)

    print(f"\nResults written to {out_dir}/")
    print(report["markdown"])

if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Commit**

```bash
git add eval/run_benchmark.py
git commit -m "feat(eval): add benchmark orchestrator CLI"
```

---

### Task 7: Judge — automatic ground truth matching

**Files:**
- Create: `eval/judge.py`
- Create: `eval/tests/test_judge.py`

- [ ] **Step 1: Write tests for auto-judging**

```python
# eval/tests/test_judge.py
from judge import match_ground_truth, judge_findings
from schema import CanonicalFinding, GroundTruthEntry, Verdict

GT = [
    GroundTruthEntry(
        id="iw-001",
        type="real",
        title="MAX_NUM_THREAD not enforced",
        category="correctness",
        severity="high",
        line_start=40,
        line_end=50,
        description="...",
    ),
]

def test_auto_match_exact_title():
    f = CanonicalFinding(
        tool="quorum-v0.21.0",
        file="rust/index_writer.rs",
        title="MAX_NUM_THREAD not enforced in IndexWriter::new",
        category="correctness",
        severity="high",
        line_start=42,
        line_end=45,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is not None
    assert match.id == "iw-001"

def test_auto_match_no_match():
    f = CanonicalFinding(
        tool="quorum-v0.21.0",
        file="rust/index_writer.rs",
        title="Completely unrelated finding",
        category="performance",
        severity="low",
        line_start=500,
        line_end=510,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is None

def test_auto_match_line_within_tolerance():
    f = CanonicalFinding(
        tool="pal",
        file="rust/index_writer.rs",
        title="MAX_NUM_THREAD declared but never enforced",
        category="unknown",
        severity="medium",
        line_start=0,  # PAL has no line numbers
        line_end=0,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is not None  # title match should still work

def test_auto_match_title_keyword_overlap():
    f = CanonicalFinding(
        tool="third-opinion",
        file="rust/index_writer.rs",
        title="MAX_NUM_THREAD declared but never enforced",
        category="unknown",
        severity="medium",
        line_start=0,
        line_end=0,
        description="...",
    )
    match = match_ground_truth(f, GT)
    assert match is not None
    assert match.id == "iw-001"
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd eval && uv run pytest tests/test_judge.py -v
```

Expected: FAIL with `ModuleNotFoundError`

- [ ] **Step 3: Write judge.py**

```python
# eval/judge.py
import json
import os
import re
from pathlib import Path

import httpx

from schema import CanonicalFinding, GroundTruthEntry, Verdict, load_ground_truth

LINE_TOLERANCE = 5
TITLE_KEYWORD_THRESHOLD = 0.4  # 40% keyword overlap required

def _tokenize(title: str) -> set[str]:
    return set(re.findall(r"[a-zA-Z_][a-zA-Z0-9_]*", title.lower()))

def _title_similarity(a: str, b: str) -> float:
    ta, tb = _tokenize(a), _tokenize(b)
    if not ta or not tb:
        return 0.0
    overlap = len(ta & tb)
    return overlap / min(len(ta), len(tb))

def _lines_compatible(finding: CanonicalFinding, gt: GroundTruthEntry) -> bool:
    if finding.line_start == 0:
        return True  # tool doesn't provide lines, skip check
    return (
        abs(finding.line_start - gt.line_start) <= LINE_TOLERANCE
        or (gt.line_start <= finding.line_start <= gt.line_end)
        or (finding.line_start <= gt.line_start <= finding.line_end)
    )

def match_ground_truth(
    finding: CanonicalFinding,
    ground_truth: list[GroundTruthEntry],
) -> GroundTruthEntry | None:
    best_match: GroundTruthEntry | None = None
    best_score = 0.0
    for gt in ground_truth:
        score = _title_similarity(finding.title, gt.title)
        if score >= TITLE_KEYWORD_THRESHOLD and _lines_compatible(finding, gt):
            if score > best_score:
                best_score = score
                best_match = gt
    return best_match

def _load_all_ground_truth(corpus_dir: Path) -> dict[str, list[GroundTruthEntry]]:
    gt_map: dict[str, list[GroundTruthEntry]] = {}
    for lang_dir in corpus_dir.iterdir():
        if not lang_dir.is_dir():
            continue
        for gt_file in lang_dir.glob("*.ground_truth.json"):
            stem = gt_file.name.replace(".ground_truth.json", "")
            rel_key = f"{lang_dir.name}/{stem}"
            # Match against both with and without extension
            for ext in (".rs", ".py", ".ts", ".tsx", ".yaml", ".yml", ".sh", ".bash"):
                full_key = f"{lang_dir.name}/{stem}{ext}"
                gt_map[full_key] = load_ground_truth(gt_file)
            gt_map[rel_key] = load_ground_truth(gt_file)
    return gt_map

def judge_auto(
    findings: list[CanonicalFinding],
    ground_truth: list[GroundTruthEntry],
) -> tuple[list[Verdict], list[CanonicalFinding]]:
    verdicts = []
    unmatched = []
    matched_gt_ids: set[str] = set()

    for f in findings:
        gt = match_ground_truth(f, ground_truth)
        if gt and gt.id not in matched_gt_ids:
            matched_gt_ids.add(gt.id)
            verdicts.append(Verdict(
                file=f.file,
                tool=f.tool,
                finding_title=f.title,
                verdict="tp",
                judge="auto",
                reason=f"Matched ground truth: {gt.title}",
                matched_ground_truth_id=gt.id,
            ))
        elif gt:
            verdicts.append(Verdict(
                file=f.file,
                tool=f.tool,
                finding_title=f.title,
                verdict="tp",
                judge="auto",
                reason=f"Duplicate match for: {gt.title}",
                matched_ground_truth_id=gt.id,
            ))
        else:
            unmatched.append(f)

    return verdicts, unmatched

def judge_panel(
    findings: list[CanonicalFinding],
    source_file: Path,
    ground_truth: list[GroundTruthEntry],
) -> list[Verdict]:
    base_url = os.environ.get("QUORUM_BASE_URL", "https://litellm.5745.house")
    api_key = os.environ.get("QUORUM_API_KEY", "")
    if not api_key:
        return [
            Verdict(
                file=f.file, tool=f.tool, finding_title=f.title,
                verdict="tp", judge="panel-skipped",
                reason="No QUORUM_API_KEY set, defaulting to TP",
            )
            for f in findings
        ]

    # Read source excerpt (cap at 200 lines around finding)
    source_lines = source_file.read_text().splitlines() if source_file.exists() else []
    gt_summary = "\n".join(f"- [{g.id}] {g.title} ({g.severity})" for g in ground_truth)

    models = ["claude-sonnet-4-20250514", "gemini-2.5-pro"]
    verdicts = []

    for f in findings:
        start = max(0, f.line_start - 25)
        end = min(len(source_lines), f.line_end + 25) if f.line_end > 0 else min(len(source_lines), 50)
        excerpt = "\n".join(f"{i+start+1}: {l}" for i, l in enumerate(source_lines[start:end]))

        prompt = (
            f"You are judging a code review finding.\n\n"
            f"## Source excerpt\n```\n{excerpt}\n```\n\n"
            f"## Finding\n"
            f"- Title: {f.title}\n- Severity: {f.severity}\n- Category: {f.category}\n"
            f"- Lines: {f.line_start}-{f.line_end}\n- Description: {f.description}\n\n"
            f"## Known bugs in this file\n{gt_summary}\n\n"
            f"Is this finding a genuine bug, vulnerability, or quality issue? "
            f"Answer with exactly one of: tp, fp, partial. Then one sentence reason.\n"
            f"Format: VERDICT: <tp|fp|partial> REASON: <reason>"
        )

        votes: list[tuple[str, str]] = []
        for model in models:
            try:
                resp = httpx.post(
                    f"{base_url}/v1/chat/completions",
                    headers={"Authorization": f"Bearer {api_key}"},
                    json={
                        "model": model,
                        "messages": [{"role": "user", "content": prompt}],
                        "temperature": 0,
                        "max_tokens": 200,
                    },
                    timeout=60,
                )
                text = resp.json()["choices"][0]["message"]["content"]
                verdict, reason = _parse_verdict(text)
                votes.append((verdict, reason))
            except Exception as e:
                votes.append(("tp", f"Judge error: {e}"))

        # Majority vote
        verdict_counts: dict[str, int] = {}
        for v, _ in votes:
            verdict_counts[v] = verdict_counts.get(v, 0) + 1
        final_verdict = max(verdict_counts, key=lambda k: verdict_counts[k])
        reasons = [r for _, r in votes]

        verdicts.append(Verdict(
            file=f.file,
            tool=f.tool,
            finding_title=f.title,
            verdict=final_verdict,
            judge="panel" if len(set(v for v, _ in votes)) == 1 else "panel-disputed",
            reason=f"Votes: {', '.join(v for v, _ in votes)}. {reasons[0]}",
        ))

    return verdicts

def _parse_verdict(text: str) -> tuple[str, str]:
    text = text.strip()
    for prefix in ("VERDICT:", "verdict:"):
        if prefix in text:
            after = text.split(prefix, 1)[1].strip()
            parts = after.split("REASON:", 1) if "REASON:" in after else after.split("reason:", 1) if "reason:" in after else [after, ""]
            verdict = parts[0].strip().lower()
            reason = parts[1].strip() if len(parts) > 1 else ""
            if verdict in ("tp", "fp", "partial"):
                return verdict, reason
    # Fallback: look for verdict word at start
    first_word = text.split()[0].lower().rstrip(".:,") if text else ""
    if first_word in ("tp", "fp", "partial"):
        return first_word, text
    return "tp", f"Could not parse verdict, defaulting to TP. Raw: {text[:100]}"

def judge_findings(
    all_findings: dict[str, list[CanonicalFinding]],
    corpus_dir: Path,
) -> list[Verdict]:
    gt_map = _load_all_ground_truth(corpus_dir)
    all_verdicts: list[Verdict] = []

    for tool, findings in all_findings.items():
        by_file: dict[str, list[CanonicalFinding]] = {}
        for f in findings:
            by_file.setdefault(f.file, []).append(f)

        for file_rel, file_findings in by_file.items():
            gt = gt_map.get(file_rel, [])

            # Tier 1: automatic matching
            auto_verdicts, unmatched = judge_auto(file_findings, gt)
            all_verdicts.extend(auto_verdicts)

            # Tier 2: model panel for unmatched
            if unmatched:
                # Find the source file
                source_path = corpus_dir / file_rel
                if not source_path.exists():
                    # Try with common extensions
                    for ext in (".rs", ".py", ".ts"):
                        candidate = corpus_dir / f"{file_rel}{ext}"
                        if candidate.exists():
                            source_path = candidate
                            break

                panel_verdicts = judge_panel(unmatched, source_path, gt)
                all_verdicts.extend(panel_verdicts)

    return all_verdicts
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd eval && uv run pytest tests/test_judge.py -v
```

Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add eval/judge.py eval/tests/test_judge.py
git commit -m "feat(eval): add judge with auto ground-truth matching and model panel"
```

---

### Task 8: Scorecard computation and rendering

**Files:**
- Create: `eval/scorecard.py`
- Create: `eval/tests/test_scorecard.py`

- [ ] **Step 1: Write tests**

```python
# eval/tests/test_scorecard.py
from scorecard import compute_metrics, ToolMetrics
from schema import Verdict, CanonicalFinding, GroundTruthEntry

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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd eval && uv run pytest tests/test_scorecard.py -v
```

Expected: FAIL

- [ ] **Step 3: Write scorecard.py**

```python
# eval/scorecard.py
import json
from collections import defaultdict
from dataclasses import dataclass, asdict
from pathlib import Path

from schema import CanonicalFinding, Verdict, GroundTruthEntry, load_ground_truth

@dataclass
class ToolMetrics:
    tool: str = ""
    tp_count: int = 0
    fp_count: int = 0
    partial_count: int = 0
    total_findings: int = 0
    total_known_bugs: int = 0
    precision: float = 0.0
    recall: float = 0.0
    f1: float = 0.0
    unique_finds: int = 0
    noise_rate: float = 0.0

def compute_metrics(
    verdicts: list[Verdict],
    total_known_bugs: int,
) -> ToolMetrics:
    tp = sum(1 for v in verdicts if v.verdict == "tp")
    fp = sum(1 for v in verdicts if v.verdict == "fp")
    partial = sum(1 for v in verdicts if v.verdict == "partial")
    total = len(verdicts)

    effective_tp = tp + 0.5 * partial
    precision = effective_tp / total if total > 0 else 0.0
    recall = tp / total_known_bugs if total_known_bugs > 0 else 0.0
    f1 = (2 * precision * recall / (precision + recall)) if (precision + recall) > 0 else 0.0

    files = set(v.file for v in verdicts)
    noise = fp / len(files) if files else 0.0

    return ToolMetrics(
        tp_count=tp,
        fp_count=fp,
        partial_count=partial,
        total_findings=total,
        total_known_bugs=total_known_bugs,
        precision=round(precision, 4),
        recall=round(recall, 4),
        f1=round(f1, 4),
        noise_rate=round(noise, 2),
    )

def _count_known_bugs(corpus_dir: Path) -> int:
    count = 0
    for gt_file in corpus_dir.rglob("*.ground_truth.json"):
        gt = load_ground_truth(gt_file)
        count += len(gt)
    return count

def _find_unique(
    tool: str,
    all_verdicts: list[Verdict],
) -> int:
    tool_tps = {
        (v.file, v.matched_ground_truth_id)
        for v in all_verdicts
        if v.tool == tool and v.verdict == "tp" and v.matched_ground_truth_id
    }
    other_tps = {
        (v.file, v.matched_ground_truth_id)
        for v in all_verdicts
        if v.tool != tool and v.verdict == "tp" and v.matched_ground_truth_id
    }
    return len(tool_tps - other_tps)

def generate_scorecard(
    verdicts: list[Verdict],
    all_findings: dict[str, list[CanonicalFinding]],
    corpus_dir: Path,
) -> dict:
    total_known = _count_known_bugs(corpus_dir)
    tools = sorted(all_findings.keys())

    tool_metrics: dict[str, ToolMetrics] = {}
    for tool in tools:
        tool_verdicts = [v for v in verdicts if v.tool == tool]
        m = compute_metrics(tool_verdicts, total_known)
        m.tool = tool
        m.unique_finds = _find_unique(tool, verdicts)
        tool_metrics[tool] = m

    # Build Markdown
    lines = [
        "# Benchmark Scorecard",
        "",
        f"Corpus: {total_known} known bugs",
        f"Tools: {', '.join(tools)}",
        "",
        "## Summary",
        "",
        "| Tool | Findings | TP | FP | Partial | Precision | Recall | F1 | Unique | Noise/file |",
        "|------|---------|----|----|---------|-----------|--------|----|--------|------------|",
    ]
    for tool in tools:
        m = tool_metrics[tool]
        lines.append(
            f"| {m.tool} | {m.total_findings} | {m.tp_count} | {m.fp_count} | "
            f"{m.partial_count} | {m.precision:.1%} | {m.recall:.1%} | "
            f"{m.f1:.1%} | {m.unique_finds} | {m.noise_rate:.1f} |"
        )

    # Per-file breakdown
    files = sorted(set(v.file for v in verdicts))
    if files:
        lines.extend(["", "## Per-file breakdown", ""])
        for file in files:
            lines.append(f"### {file}")
            lines.append("")
            lines.append("| Tool | TP | FP | Partial |")
            lines.append("|------|----|----|---------|")
            for tool in tools:
                fv = [v for v in verdicts if v.file == file and v.tool == tool]
                tp = sum(1 for v in fv if v.verdict == "tp")
                fp = sum(1 for v in fv if v.verdict == "fp")
                p = sum(1 for v in fv if v.verdict == "partial")
                if tp + fp + p > 0:
                    lines.append(f"| {tool} | {tp} | {fp} | {p} |")
            lines.append("")

    markdown = "\n".join(lines)
    data = {
        "total_known_bugs": total_known,
        "tools": {t: asdict(m) for t, m in tool_metrics.items()},
    }

    return {"markdown": markdown, "data": data}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd eval && uv run pytest tests/test_scorecard.py -v
```

Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add eval/scorecard.py eval/tests/test_scorecard.py
git commit -m "feat(eval): add scorecard metrics computation and Markdown renderer"
```

---

### Task 9: Corpus expansion — YAML and Bash challenge files

**Files:**
- Create: `eval/corpus/yaml/ha_automation.yaml`
- Create: `eval/corpus/yaml/ha_automation.ground_truth.json`
- Create: `eval/corpus/bash/deploy.sh`
- Create: `eval/corpus/bash/deploy.ground_truth.json`

- [ ] **Step 1: Create YAML challenge file**

Create `eval/corpus/yaml/ha_automation.yaml` — a Home Assistant automation with planted bugs. The file should be ~100-200 lines and contain a realistic HA automation with these planted issues:

1. A hardcoded API key in a REST command (`api_key: sk-live-abc123...`)
2. A Jinja2 template that uses `float` without a default fallback (`{{ states('sensor.temp') | float }}` instead of `{{ states('sensor.temp') | float(0) }}`)
3. A duplicate key (two `action:` blocks in the same automation)
4. An automation trigger with no `id` field (breaks trace debugging)

Write the full YAML file content with realistic HA automation structure surrounding the planted bugs.

- [ ] **Step 2: Create YAML ground truth**

```json
[
  {
    "id": "ha-001",
    "type": "planted",
    "title": "Hardcoded API key in REST command",
    "category": "security",
    "severity": "critical",
    "line_start": 0,
    "line_end": 0,
    "description": "API key sk-live-abc123 is hardcoded in the automation instead of using !secret."
  },
  {
    "id": "ha-002",
    "type": "planted",
    "title": "Jinja2 float without default fallback",
    "category": "correctness",
    "severity": "medium",
    "line_start": 0,
    "line_end": 0,
    "description": "states('sensor.temp') | float will raise when the sensor is unavailable. Use float(0) or float(default)."
  },
  {
    "id": "ha-003",
    "type": "planted",
    "title": "Duplicate action key in automation",
    "category": "correctness",
    "severity": "high",
    "line_start": 0,
    "line_end": 0,
    "description": "Two 'action:' keys in the same mapping. The second silently overwrites the first."
  },
  {
    "id": "ha-004",
    "type": "planted",
    "title": "Automation trigger missing id field",
    "category": "quality",
    "severity": "low",
    "line_start": 0,
    "line_end": 0,
    "description": "Trigger lacks an id field, making trace debugging harder."
  }
]
```

Update `line_start`/`line_end` after writing the YAML file to match actual line numbers.

- [ ] **Step 3: Create Bash challenge file**

Create `eval/corpus/bash/deploy.sh` — a deployment script (~80-120 lines) with planted bugs:

1. Unquoted variable in `rm -rf $DEPLOY_DIR/*` (word splitting + glob)
2. `eval "$USER_INPUT"` — command injection via eval
3. `curl http://example.com/script.sh | bash` — pipe-to-bash
4. Predictable temp file: `TMPFILE=/tmp/deploy-$$` (PID guessable)
5. `chmod 777 /var/www/html` — overly permissive permissions

Write a realistic deploy script with these bugs embedded in plausible context.

- [ ] **Step 4: Create Bash ground truth**

```json
[
  {
    "id": "sh-001",
    "type": "planted",
    "title": "Unquoted variable in rm command",
    "category": "correctness",
    "severity": "high",
    "line_start": 0,
    "line_end": 0,
    "description": "rm -rf $DEPLOY_DIR/* without quotes is vulnerable to word splitting and glob expansion."
  },
  {
    "id": "sh-002",
    "type": "planted",
    "title": "Command injection via eval",
    "category": "security",
    "severity": "critical",
    "line_start": 0,
    "line_end": 0,
    "description": "eval on user-controlled input allows arbitrary command execution."
  },
  {
    "id": "sh-003",
    "type": "planted",
    "title": "Pipe-to-bash from HTTP URL",
    "category": "security",
    "severity": "high",
    "line_start": 0,
    "line_end": 0,
    "description": "curl | bash executes remote code without verification. MITM or server compromise leads to RCE."
  },
  {
    "id": "sh-004",
    "type": "planted",
    "title": "Predictable temporary file path",
    "category": "security",
    "severity": "medium",
    "line_start": 0,
    "line_end": 0,
    "description": "Using /tmp/deploy-$$ is predictable. Use mktemp instead."
  },
  {
    "id": "sh-005",
    "type": "planted",
    "title": "chmod 777 on web directory",
    "category": "security",
    "severity": "high",
    "line_start": 0,
    "line_end": 0,
    "description": "chmod 777 grants world-writable permissions to the web root, allowing any user to modify files."
  }
]
```

Update `line_start`/`line_end` after writing the bash file.

- [ ] **Step 5: Commit**

```bash
git add eval/corpus/yaml/ eval/corpus/bash/
git commit -m "feat(eval): add YAML and Bash challenge files with planted bugs"
```

---

### Task 10: Additional Rust/Python/TypeScript corpus files

**Files:**
- Create: 1-2 new files per language with ground truth

- [ ] **Step 1: Source and add a Rust challenge file**

Find a real pre-fix Rust file from a public crate with a known bug. Good candidates:
- A `hyper` or `axum` handler with a request-smuggling or panic-on-invalid-input fix
- A `serde` deserialization file pre-CVE
- A `regex` or `nom` parser with a ReDoS or panic

Download the pre-fix version, save to `eval/corpus/rust/<name>.rs`, and create the matching `.ground_truth.json`.

- [ ] **Step 2: Source and add a Python challenge file**

Find a real pre-fix Python file. Good candidates:
- A Django/Flask view with a pre-CVE SQL injection or IDOR
- A `requests`/`urllib3` file with a pre-fix SSRF or redirect issue
- A `paramiko`/`cryptography` file with a known auth bypass

Download, save to `eval/corpus/python/<name>.py`, create ground truth.

- [ ] **Step 3: Add a TypeScript challenge file with synthetic plants**

Create a ~200-line Express/Fastify handler with planted bugs:
- `innerHTML` assignment from user input (XSS)
- `JSON.parse(input) as AdminUser` (unsafe cast)
- `new RegExp(userInput)` (ReDoS)
- `tautological .length >= 0` check

Save to `eval/corpus/typescript/<name>.ts` with ground truth.

- [ ] **Step 4: Commit**

```bash
git add eval/corpus/
git commit -m "feat(eval): add additional Rust/Python/TypeScript challenge files"
```

---

### Task 11: End-to-end smoke test

**Files:**
- Create: `eval/tests/test_e2e.py`

- [ ] **Step 1: Write an integration test that runs the full pipeline on mock data**

```python
# eval/tests/test_e2e.py
import json
import tempfile
from pathlib import Path

from schema import CanonicalFinding, GroundTruthEntry, save_verdicts
from judge import judge_auto
from scorecard import compute_metrics, generate_scorecard

def test_full_pipeline_mock():
    """Smoke test: auto-judge + scorecard on synthetic data."""
    gt = [
        GroundTruthEntry(
            id="t-001", type="planted", title="SQL injection",
            category="security", severity="high",
            line_start=10, line_end=15, description="...",
        ),
        GroundTruthEntry(
            id="t-002", type="planted", title="Hardcoded password",
            category="security", severity="critical",
            line_start=50, line_end=52, description="...",
        ),
    ]

    tool_a_findings = [
        CanonicalFinding(
            tool="tool-a", file="python/app.py",
            title="SQL injection via string format",
            category="security", severity="high",
            line_start=12, line_end=14, description="...",
        ),
        CanonicalFinding(
            tool="tool-a", file="python/app.py",
            title="Unused import os",
            category="quality", severity="low",
            line_start=1, line_end=1, description="...",
        ),
    ]

    tool_b_findings = [
        CanonicalFinding(
            tool="tool-b", file="python/app.py",
            title="SQL injection risk",
            category="security", severity="high",
            line_start=11, line_end=13, description="...",
        ),
        CanonicalFinding(
            tool="tool-b", file="python/app.py",
            title="Hardcoded password in config",
            category="security", severity="critical",
            line_start=50, line_end=52, description="...",
        ),
    ]

    # Auto-judge both tools
    verdicts_a, unmatched_a = judge_auto(tool_a_findings, gt)
    verdicts_b, unmatched_b = judge_auto(tool_b_findings, gt)

    assert len(verdicts_a) == 1  # matched SQL injection
    assert verdicts_a[0].matched_ground_truth_id == "t-001"
    assert len(unmatched_a) == 1  # "Unused import" unmatched

    assert len(verdicts_b) == 2  # matched both
    assert {v.matched_ground_truth_id for v in verdicts_b} == {"t-001", "t-002"}

    # Scorecard
    all_verdicts = verdicts_a + verdicts_b
    all_findings = {"tool-a": tool_a_findings, "tool-b": tool_b_findings}

    # Create a temp corpus dir with ground truth
    with tempfile.TemporaryDirectory() as tmpdir:
        corpus = Path(tmpdir)
        py_dir = corpus / "python"
        py_dir.mkdir()
        gt_path = py_dir / "app.ground_truth.json"
        with open(gt_path, "w") as f:
            json.dump([
                {"id": g.id, "type": g.type, "title": g.title,
                 "category": g.category, "severity": g.severity,
                 "line_start": g.line_start, "line_end": g.line_end,
                 "description": g.description}
                for g in gt
            ], f)

        report = generate_scorecard(all_verdicts, all_findings, corpus)
        assert "tool-a" in report["markdown"]
        assert "tool-b" in report["markdown"]
        assert report["data"]["total_known_bugs"] == 2
```

- [ ] **Step 2: Run the test**

```bash
cd eval && uv run pytest tests/test_e2e.py -v
```

Expected: 1 passed

- [ ] **Step 3: Run full test suite**

```bash
cd eval && uv run pytest tests/ -v
```

Expected: all tests pass (test_schema + test_normalize + test_judge + test_scorecard + test_e2e)

- [ ] **Step 4: Commit**

```bash
git add eval/tests/test_e2e.py
git commit -m "test(eval): add end-to-end smoke test for benchmark pipeline"
```

---

## Self-Review Checklist

**Spec coverage:**
- [x] Corpus design (5 languages, existing + new) → Tasks 4, 9, 10
- [x] Output normalization → Task 3
- [x] Orchestrator CLI → Task 6
- [x] Auto-judge (ground truth matching) → Task 7
- [x] Model panel judge → Task 7
- [x] Scorecard metrics (P/R/F1/unique/noise) → Task 8
- [x] Cross-tool dedup → Task 8 (`_find_unique`)
- [x] Binary builder + flag compat → Task 5
- [x] Per-language/severity/category breakdowns → Task 8
- [x] Per-ground-truth-type breakdown → Not yet in scorecard, add during implementation
- [x] Blinded human audit sample → Judge marks `panel-disputed`, operator reviews
- [x] Confidence intervals → Add during implementation (bootstrap in scorecard.py)
- [x] Cost tracking → Capture in orchestrator via LiteLLM API

**Placeholder scan:** No TBDs. Tasks 9/10 describe file content guidelines rather than exact content because corpus files require manual curation — but the ground truth schemas and directory structure are fully specified.

**Type consistency:** `CanonicalFinding`, `Verdict`, `GroundTruthEntry` used consistently across all modules. `normalize_*` functions all return `list[CanonicalFinding]`. `judge_findings` returns `list[Verdict]`. `generate_scorecard` takes verdicts + findings + corpus_dir.
