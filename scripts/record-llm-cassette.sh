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

# The API key is sent as a Bearer token to whatever this points at, so hold it
# to the same bar quorum's own validate_base_url does: https, or explicit
# loopback for a local gateway. Without this, a stale QUORUM_BASE_URL sends the
# key somewhere in cleartext.
#
# The host is PARSED, not glob-matched. A previous version tested
# `http://127.0.0.1:*` against the whole URL, which `*` made trivially
# bypassable: in `http://127.0.0.1:80@attacker.example` everything before the
# `@` is RFC 3986 userinfo, so the real host is attacker.example -- and the
# check meant to prevent cleartext key disclosure was admitting exactly that.
# Glob-matching a URL cannot be made safe; authority parsing can.
authority="${base_url#*://}"
authority="${authority%%/*}"

# Embedded credentials are rejected outright, mirroring the always-on rule in
# validate_base_url (src/llm_client.rs). This kills the userinfo class rather
# than special-casing it, and no legitimate endpoint needs them here.
case "$authority" in
    *@*)
        echo "error: base_url must not contain embedded credentials (user@host)." >&2
        echo "       Pass the API key via QUORUM_API_KEY instead." >&2
        exit 2
        ;;
esac

# Strip the port to get the bare host. Bracketed IPv6 literals keep their
# brackets, so `[::1]:9` yields `[::1]` rather than `[`.
case "$authority" in
    \[*) host="${authority%%\]*}]" ;;
    *) host="${authority%%:*}" ;;
esac

case "$base_url" in
    https://*) ;;
    http://*)
        case "$host" in
            127.0.0.1|localhost|'[::1]')
                echo "warning: recording over plaintext http to a local endpoint" >&2
                ;;
            *)
                echo "error: refusing to send QUORUM_API_KEY to '$host' over plaintext http." >&2
                echo "       Use https, or a loopback address for a local gateway." >&2
                exit 2
                ;;
        esac
        ;;
    *)
        echo "error: base_url must use http or https, got '$base_url'." >&2
        exit 2
        ;;
esac

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
#
# Built into a temp file and moved into place only on success, so a jq failure
# or a rejected redaction cannot destroy a cassette that was already there.
#
# The temp file lives in out_dir, NOT $TMPDIR: `mv` is only atomic within a
# single filesystem, and $TMPDIR is frequently on a different one. Across
# filesystems mv degrades to copy-then-remove, so an interruption would leave
# exactly the truncated cassette this pattern exists to prevent.
tmp_out="$(mktemp "$out_dir/.quorum-cassette.XXXXXX")"
trap 'rm -f "$tmp_out"' EXIT

printf '%s' "$raw" | jq '
    del(.request, .echo, .organization, .system_fingerprint, .service_tier)
    | .id = "chatcmpl-quorum-test-cassette"
    | .created = 1746000000
' > "$tmp_out"

# A 200 carrying an error payload is not a usable cassette. Catch it here
# rather than at `cargo test` time, where the failure is far less obvious.
if ! jq -e '.choices[0].message.content | strings' "$tmp_out" >/dev/null; then
    echo "error: response has no choices[0].message.content; not a usable cassette." >&2
    # The raw body is deliberately NOT printed. This point is reached before
    # the redaction and key checks below have run, and the body can carry
    # echoed prompt data or credentials. In a script whose job is redaction,
    # this is the last place to leak one. Only structural facts are safe here.
    echo "       Response was $(printf '%s' "$raw" | wc -c | tr -d ' ') bytes with top-level keys:" >&2
    printf '%s' "$raw" | jq -r 'if type == "object" then (keys | join(", ")) else type end' >&2 2>/dev/null \
        || echo "       (unparseable as JSON)" >&2
    echo "       Re-run with the endpoint directly if you need to inspect the body." >&2
    exit 1
fi

# Fail loudly rather than commit a credential. Two checks, because the shape
# heuristic alone misses provider keys that do not look like `sk-...`:
#   1. the literal key that was just used, matched with -F so its own
#      metacharacters cannot alter the pattern
#   2. common key shapes, for anything echoed back that is not our key
if grep -qF "$QUORUM_API_KEY" "$tmp_out"; then
    echo "error: cassette contains the API key verbatim; refusing to keep it" >&2
    exit 1
fi
if grep -Eq 'sk-[A-Za-z0-9]{16,}|Bearer [A-Za-z0-9._-]{16,}' "$tmp_out"; then
    echo "error: recorded cassette still contains something key-shaped; refusing to keep it" >&2
    exit 1
fi

mv "$tmp_out" "$out"
trap - EXIT

echo "Wrote $out" >&2
echo "Review it by eye for anything from your prompt before committing." >&2
