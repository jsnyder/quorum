# Per-axis eval

Does each skill axis find the defects it exists to find, when they are there, and stay quiet about everything else?

`corpus/<axis>/` holds two or three files per axis (Rust, Python, TypeScript) with defects planted from that axis's own prompt priority list, each anchored to a line range in `<file>.ground_truth.json`, plus **decoys**: code that looks like a defect but is one the prompt says not to report (a documented cold path, a parameterised query, an injected clock, validation at a trust boundary). A `redacted` entry is a planted secret the egress redaction strips before any model sees it; it is not scored.

```
eval/axes/run_axes.py                      # every axis on every file, quorum's default model
eval/axes/run_axes.py --model claude-opus-5
eval/axes/run_axes.py --quorum target/debug/quorum --in-lane-only   # a branch build, own corpora only
eval/axes/run_axes.py --score results/<raw>.json                     # re-score saved output after a ground-truth edit
```

Each cell is `quorum review <file> --json --no-cache --skip-context7 --axes <axis>` under an isolated `HOME`, so verdicts recorded on real code cannot suppress planted ones. Only findings the axis itself produced count; AST and ast-grep rule hits are tallied separately.

Scoring: a finding hits a planted defect when its anchor is within 2 lines of the range or its span (up to 40 lines) overlaps it; each defect is claimed once, further findings on it are duplicates. A finding on a decoy range is a decoy hit; anything else on the axis's own corpus is noise. The axis on the other axes' corpora yields out-of-lane findings, and how many of those land on the other axis's planted defects.

`results/` keeps a scorecard (`.txt`) and the raw findings (`.json`, titles and lines only) per run. Compare against the baseline there before and after a prompt or input change; `tests/test_run_axes.py` pins the scorer.

The first run (2026-10-01) found #652: whole-file input carried no line numbers, and every model anchor drifted 2-4 lines early. The before/after scorecards in `results/` are that fix measured.
