#!/bin/bash
# Five-model recall run over cases.json. Spends money (live, uncached reviews).
# Claude runs at lower parallelism with a longer total timeout: at --jobs 3 the
# first run lost 8-12% of axis calls (opus: instant failures under load; fable:
# the 300 s QUORUM_HTTP_TIMEOUT total cap).
set -euo pipefail
cd "$(dirname "$0")"
Q=${QUORUM_BIN:-../../target/release/quorum}
OUT=${OUT:-out}
mkdir -p "$OUT"
for m in gpt-5.6 gpt-6.1-sol gpt-6.1-terra; do
  python3 run_cases.py cases.json --model $m --quorum "$Q" --jobs 4 > "$OUT/$m.json" 2> "$OUT/$m.err" &
done
wait
for m in claude-opus-5-5 claude-fable-5-2; do
  QUORUM_HTTP_TIMEOUT=900 QUORUM_HTTP_READ_TIMEOUT=600 \
    python3 run_cases.py cases.json --model $m --quorum "$Q" --jobs 1 > "$OUT/$m.json" 2> "$OUT/$m.err"
done
tail -qn1 "$OUT"/*.err
