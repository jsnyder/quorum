#!/bin/bash
# Rebuild all.jsonl exactly as mined on 2026-11-14 (the --until values pin the
# comment window and the merged-PR set), then triaged.jsonl and cases.json.
# litellm and twenty use 3 and 7 pages because the original runs stopped at a
# truncated GitHub page there; these counts reproduce what triage.py judged.
set -euo pipefail
cd "$(dirname "$0")"
T=$(mktemp -d)
python3 mine.py biomejs/biome "coderabbitai[bot]" --pages 8 --until 2026-11-14T18:51:40Z > "$T/biome.jsonl"
python3 mine.py mastra-ai/mastra "coderabbitai[bot]" --pages 8 --until 2026-11-14T19:00:15Z > "$T/mastra.jsonl"
python3 mine.py BerriAI/litellm "greptile-apps[bot]" --pages 3 --until 2026-11-14T19:00:15Z > "$T/litellm.jsonl"
python3 mine.py twentyhq/twenty "sentry[bot]" --pages 7 --until 2026-11-14T19:00:15Z > "$T/twenty-s.jsonl"
cat "$T"/{biome,mastra,litellm,twenty-s}.jsonl > all.jsonl
python3 triage.py
python3 make_cases.py
