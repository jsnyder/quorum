# Real-bug recall eval

Real defects from merged open-source PRs: a review bot commented on some lines, and a later commit in the same PR changed those lines. The diff the bot saw (merge-base..commented commit) is the input; the commented lines are the ground truth. None of it overlaps `~/.quorum/feedback.jsonl`, unlike `eval/axes`, which planted its own defects and is saturated (every model scores 42-49 of 49).

## Set

29 cases, 39 defects, PRs merged July-November 2026:

| repo | bot | defects |
|---|---|---:|
| mastra-ai/mastra (TS) | coderabbitai | 25 |
| biomejs/biome (Rust) | coderabbitai | 12 |
| BerriAI/litellm (Python) | greptile-apps | 1 |
| twentyhq/twenty (TS) | sentry | 1 |

Difficulty 17 hard / 17 medium / 5 easy; 23 need context from other files.

## Files

| file | what |
|---|---|
| `mine.py` | candidates for one repo/bot; `--until` pins the comment window so a re-run is identical |
| `mine_all.sh` | rebuilds `all.jsonl` (95 candidates) as mined on 2026-11-14, then `triaged.jsonl` and `cases.json` |
| `triage.py` | keep/drop judgment per candidate, by line index in `all.jsonl` |
| `make_cases.py` | groups kept defects into cases |
| `run_cases.py` | reviews each case with one model, position-matches findings (spends money) |
| `run_models.sh` | five-model run into `out/` (spends money) |

`all.jsonl`, `triaged.jsonl` and `cases.json` are committed so a run needs no GitHub mining. Clones and worktrees go to `~/.cache/quorum-eval-real`.

## Scoring

`run_cases.py` counts a finding as a hit when it is on the defect's file within 3 lines. That is a position match: shifting every defect 40 lines still matched 2-6 per model, and rule findings (a stray `console.log`) land on defect lines by accident. Quote only judged hits, where the finding describes the same bug.

## Results, 2026-11-14 (one run per model, judged by hand)

| model | judged hits / 39 | position hits | model findings |
|---|---:|---:|---:|
| gpt-6.1-terra | 13 | 22 | 244 |
| gpt-6.1-sol | 8 | 18 | 83 |
| gpt-5.6 | 7 | 16 | 181 |
| claude-fable-5-2 | 7 | 16 | 183 |
| claude-opus-5-5 | 3 | 11 | 60 |

Caveats: mastra is 25 of 39 defects. The Claude runs lost 8-12% of axis calls (opus to concurrency, fable to the 300 s total timeout; `run_models.sh` now runs them at `--jobs 1` with `QUORUM_HTTP_TIMEOUT=900`). Claude models get the `anthropic` prompt family, so prompt and model are confounded. The ±3-line window misses correct findings anchored elsewhere.
