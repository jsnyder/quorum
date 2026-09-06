#!/usr/bin/env bash
#
# Record an LLM response cassette for tests/fixtures/llm/ (issue #501).
#
# Cassettes let the integration suite exercise quorum's LLM path without
# spending money on every `cargo test`. Replay is the product; recording is
# this script, run by hand on the rare occasion a cassette needs refreshing.
# There is deliberately no record mode wired into the test harness -- it would
# run about twice a year and would itself have to be money-safe.
#
# THIS SCRIPT SPENDS REAL MONEY. That is the whole point of it being separate,
# manual, and never invoked by `cargo test`.
#
# Usage:
#   scripts/record-llm-cassette.sh <cassette-name> <prompt-file>
#
# Example:
#   scripts/record-llm-cassette.sh rust_unwrap_finding /tmp/prompt.txt
#
# Requires QUORUM_API_KEY and (optionally) QUORUM_BASE_URL / QUORUM_MODEL.

set -euo pipefail

usage() {
    echo "usage: $0 <cassette-name> <prompt-file>" >&2
    exit 2
}

[ $# -eq 2 ] || usage

name="$1"
prompt_file="$2"

case "$name" in
    *[!a-zA-Z0-9_-]*|"")
        echo "error: cassette name must be [a-zA-Z0-9_-]+, got '$name'" >&2
        exit 2
        ;;
esac

[ -r "$prompt_file" ] || { echo "error: cannot read prompt file '$prompt_file'" >&2; exit 2; }

: "${QUORUM_API_KEY:?set QUORUM_API_KEY to record a cassette}"
base_url="${QUORUM_BASE_URL:-https://api.openai.com/v1}"
model="${QUORUM_MODEL:-gpt-5.6}"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out_dir="$repo_root/tests/fixtures/llm"
out="$out_dir/$name.json"
mkdir -p "$out_dir"

echo "Recording '$name' from $base_url (model: $model)." >&2
echo "This is a real, billed request." >&2

request=$(jq -n \
    --arg model "$model" \
    --rawfile prompt "$prompt_file" \
    '{model: $model, messages: [{role: "user", content: $prompt}]}')

raw=$(curl -sS --fail-with-body \
    -X POST "$base_url/chat/completions" \
    -H "Authorization: Bearer $QUORUM_API_KEY" \
    -H "Content-Type: application/json" \
    -d "$request")

# Redaction. A cassette is committed to a public repo, so everything that
# could carry a credential or the reviewed source is stripped before it lands
# on disk -- not after, and not "usually".
#
#   - The Authorization header never appears in a response body, but some
#     gateways echo the request back under `request` or `echo`; drop those.
#   - Provider responses can carry account-scoped ids (`organization`,
#     `system_fingerprint`) that identify the recorder.
#   - `choices[].message.content` is the model's reply and is the point of the
#     cassette, so it is KEPT -- review it by eye before committing. If the
#     prompt embedded proprietary source, the reply may quote it back.
printf '%s' "$raw" | jq '
    del(.request, .echo, .organization, .system_fingerprint, .service_tier)
    | .id = "chatcmpl-quorum-test-cassette"
    | .created = 1746000000
' > "$out"

# Fail loudly rather than commit something key-shaped.
if grep -Eq 'sk-[A-Za-z0-9]{16,}|Bearer [A-Za-z0-9._-]{16,}' "$out"; then
    echo "error: recorded cassette still contains something key-shaped; refusing to keep it" >&2
    rm -f "$out"
    exit 1
fi

echo "Wrote $out" >&2
echo "Review it by eye for anything from your prompt before committing." >&2
