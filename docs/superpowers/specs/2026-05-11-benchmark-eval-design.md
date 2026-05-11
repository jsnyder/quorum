# Quorum Benchmark & Evaluation Harness — Design Spec

## Goal

Measure quorum's precision, recall, and positioning relative to peer tools (PAL, third-opinion) on a curated multi-language corpus. Track regression across versions (v0.18.4 vs v0.21.0) and identify where each tool adds unique value.

## Corpus

### Existing files (3, unchanged)

| File | Language | Lines | Source | Patterns |
|------|----------|------:|--------|----------|
| `index_writer.rs` | Rust | 2588 | tantivy | concurrency, unsafe, error handling |
| `client.py` | Python | 2019 | httpx | async/sync, HTTP, resource management |
| `router.ts` | TypeScript | 565 | tRPC | type guards, lazy loading, any casts |

### New files to curate (6-8)

| Language | Count | Source strategy | Target patterns |
|----------|------:|-----------------|-----------------|
| Rust | 1-2 | Pre-fix CVE commit from public crate | unsafe, unwrap on fallible, error swallowing, silent conversion |
| Python | 1-2 | Real CVE + synthetic plants | SQL injection, subprocess shell=True, eval, hardcoded secrets, mutable defaults |
| TypeScript | 1 | Real CVE or synthetic | innerHTML/XSS, prototype pollution, as any, tautological .length |
| YAML | 1-2 | Synthetic HA automation + docker-compose | secrets in env, insecure defaults, Jinja template issues, duplicate keys |
| Bash | 1 | Synthetic script with planted bugs | unquoted vars, eval, curl\|bash, predictable /tmp, chmod 777 |

**Total: ~10-12 files across 5 languages.**

### Ground truth

Each corpus file gets a companion `<filename>.ground_truth.json`:

```json
[
  {
    "id": "plant-001",
    "type": "planted|cve|real",
    "title": "SQL injection via f-string",
    "category": "security",
    "severity": "high",
    "location": {"line_start": 42, "line_end": 45},
    "description": "User input interpolated into SQL query without parameterization",
    "cve": null
  }
]
```

For planted bugs, the ground truth is exact. For CVE files, the known vulnerability is documented plus any other real issues identified during curation. For existing corpus files (tantivy/httpx/tRPC), ground truth is established via independent human review and source analysis — **not** by running the benchmarked tools. This avoids circular reasoning where a tool's recall is measured against bugs it helped discover. Ground truth must be frozen before any benchmark run.

## Architecture

```
eval/
  run_benchmark.py          # main orchestrator
  judge.py                  # model-panel judging + auto-verdict for plants
  scorecard.py              # precision/recall/F1, render Markdown + JSON
  build_binaries.sh         # build quorum at tagged versions
  corpus/                   # challenge files + ground truth
    rust/
      index_writer.rs
      index_writer.ground_truth.json
      ...
    python/
    typescript/
    yaml/
    bash/
  results/                  # output from each run (gitignored)
    <timestamp>/
      quorum-v0.18.4.json
      quorum-v0.21.0.json
      pal.json
      third-opinion.json
      verdicts.json
      scorecard.md
  binaries/                 # pre-built quorum binaries (gitignored)
    quorum-v0.18.4
    quorum-v0.21.0
```

### Orchestrator flow (run_benchmark.py)

1. Validate binaries exist in `eval/binaries/`, prompt to run `build_binaries.sh` if missing.
2. Iterate corpus files by language directory.
3. For each file, run each tool and capture JSON output:
   - **quorum v0.18.4**: `QUORUM_HOME=$(mktemp -d) ./binaries/quorum-v0.18.4 review <file> --json --parallel 1`
   - **quorum v0.21.0**: `QUORUM_HOME=$(mktemp -d) ./binaries/quorum-v0.21.0 review <file> --json --parallel 1 --skip-context7`
   - **PAL**: MCP `mcp__pal__codereview` or `pal codereview <file>`
   - **third-opinion**: `third-opinion review <file> --json` (skip if not available)
4. **Normalize** all tool outputs to a common finding schema (see Normalization below).
5. Pass normalized findings to `judge.py`.
6. Pass verdicts to `scorecard.py` to generate report.

### Output normalization

Each tool produces different JSON schemas. A normalization layer (`normalize.py`) maps every tool's output to a canonical finding schema before judging:

```json
{
  "tool": "quorum-v0.21.0",
  "file": "rust/index_writer.rs",
  "title": "...",
  "category": "security|correctness|quality|reliability",
  "severity": "critical|high|medium|low|info",
  "line_start": 42,
  "line_end": 45,
  "description": "..."
}
```

Per-tool adapters handle: severity name mapping, category normalization, missing line numbers (default to 0), multi-location findings (split into one entry per location), and stripping tool-specific metadata.

### Flag compatibility

`build_binaries.sh` records a compatibility matrix of supported CLI flags per version. The orchestrator checks this before invocation — e.g., v0.18.4 does not support `--skip-context7`, so that flag is omitted for older binaries.

### Environment isolation

- Every quorum run uses `QUORUM_HOME=$(mktemp -d)` — empty feedback store, no calibrator influence. Measures raw analysis quality.
- Optional `--with-calibration` flag runs a second pass with the real `~/.quorum/feedback.jsonl` to quantify calibrator value-add.
- `--skip-context7` removes network dependency for determinism.
- `--parallel 1` for deterministic finding order.
- `QUORUM_MODEL=gpt-5.4` for LLM consistency.

### CLI interface

```bash
# Full benchmark (all tools, all files)
python eval/run_benchmark.py

# Single tool
python eval/run_benchmark.py --tool quorum-v0.21.0

# Single language
python eval/run_benchmark.py --lang rust

# Re-score existing results (no re-running tools)
python eval/scorecard.py eval/results/latest/

# With calibrator influence
python eval/run_benchmark.py --with-calibration
```

## Judging

### Tier 1 — Automatic (planted/CVE bugs)

For each ground truth entry, fuzzy-match against tool findings:
- File path must match.
- Category must be compatible (e.g., "security" matches "sql-injection").
- Location must be within 5 lines of ground truth line_start.
- Title similarity via simple substring/keyword matching.

Match = TP for that tool. No match = miss (hurts recall).

### Tier 2 — Model panel (remaining findings)

Findings not matching any ground truth entry go to a 2-model judge panel. Each judge receives:
- Source file (or relevant excerpt around the finding location, ~50 lines)
- The finding (title, category, severity, line, description)
- The ground truth bug list for that file
- Prompt: "Is this finding a genuine bug, vulnerability, or quality issue in this code? Verdict: tp, fp, or partial. One sentence reason."

Models: Claude + Gemini (via LiteLLM). GPT-5.4 is excluded from the judge panel because quorum uses it as the default review model — judging findings with the same model family that generated them introduces bias. Agreement = verdict. Disagreement = flagged for human spot-check.

**Blinded human audit:** In addition to spot-checking disagreements, a random 10-20% sample of *agreements* is audited by the human operator. This detects correlated judge bias (both models agreeing on a wrong verdict).

### Verdict schema

```json
{
  "file": "rust/index_writer.rs",
  "tool": "quorum-v0.21.0",
  "finding_title": "...",
  "verdict": "tp|fp|partial",
  "judge": "auto|panel|human",
  "reason": "...",
  "matched_ground_truth_id": "plant-001|null"
}
```

## Metrics

### Per-tool scorecard

| Metric | Definition |
|--------|-----------|
| Precision | TP / (TP + FP) — partial counts as 0.5 |
| Recall | known bugs found / total known bugs |
| F1 | 2 * (P * R) / (P + R) |
| Unique finds | TPs that no other tool caught (see dedup below) |
| Noise rate | FP count / files reviewed |

### Cross-tool deduplication

Two findings from different tools are considered "the same bug" if:
- Same normalized file path
- Line ranges overlap or are within ±5 lines
- Normalized category matches (e.g., "sql-injection" ≈ "security")

A canonical dedup key is assigned to each cluster. "Unique finds" = TPs whose dedup key appears in only one tool's output.

### Breakdowns

- Per-language (Rust, Python, TS, YAML, Bash)
- Per-severity (critical, high, medium, low)
- Per-category (security, correctness, quality, reliability)
- Per-source (AST-only finds vs LLM-only vs both)
- Per-ground-truth-type (planted vs CVE vs real) — prevents synthetic plants from masking real-world recall gaps

### Output

`scorecard.md` — human-readable summary with tables and per-file detail.
`scorecard.json` — programmatic access for trend tracking.

## Cost tracking

Record LiteLLM spend (via API or dashboard) before and after each benchmark run. Report total cost per tool and per judging phase in the scorecard.

## Tools in comparison

| Tool | Version | Invocation |
|------|---------|-----------|
| quorum | v0.18.4 | pre-built binary, empty QUORUM_HOME |
| quorum | v0.21.0 | pre-built binary, empty QUORUM_HOME |
| PAL | latest | MCP or CLI |
| third-opinion | latest | CLI (skip if unavailable) |

## Framing & limitations

This is a **pilot-scale benchmark** — 10-12 files, ~50-100 findings. Results are directional, not statistically definitive. All metrics should be reported with confidence intervals or bootstrap ranges. Avoid declaring winners on small deltas where intervals overlap.

## Non-goals

- CI integration (this is a manual benchmark, not a gate)
- Measuring latency or throughput (focus is quality)
- Testing with context7 enrichment (isolated in a separate optional run)
- Testing calibrator tuning (measured via optional `--with-calibration` flag, not default)
