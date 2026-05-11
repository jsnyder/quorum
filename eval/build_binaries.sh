#!/usr/bin/env bash
# eval/build_binaries.sh — Build quorum at tagged versions for benchmarking.
# Usage: ./build_binaries.sh [v0.18.4|v0.21.0]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$SCRIPT_DIR/binaries"
mkdir -p "$BIN_DIR"

VERSIONS="v0.18.4 v0.21.0"

build_version() {
    local ver="$1"
    local out="$BIN_DIR/quorum-${ver}"

    if [[ -f "$out" ]]; then
        echo "  $ver: already built at $out (delete to rebuild)"
        return 0
    fi

    echo "  $ver: building from ref $ver ..."
    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'git -C "$REPO_ROOT" worktree remove --force "$tmpdir" 2>/dev/null || true; rm -rf "$tmpdir" 2>/dev/null || true' EXIT

    git -C "$REPO_ROOT" worktree add --detach "$tmpdir" "$ver" 2>/dev/null
    (cd "$tmpdir" && cargo build --release --quiet)
    cp "$tmpdir/target/release/quorum" "$out"
    git -C "$REPO_ROOT" worktree remove --force "$tmpdir" 2>/dev/null || true
    rm -rf "$tmpdir" 2>/dev/null || true
    trap - EXIT

    echo "  $ver: built -> $out"
}

ONLY="${1:-}"

echo "Building quorum binaries for benchmarking..."
for ver in $VERSIONS; do
    if [[ -n "$ONLY" && "$ONLY" != "$ver" ]]; then
        continue
    fi
    build_version "$ver"
done

cat > "$BIN_DIR/compat.json" << 'COMPAT'
{
    "v0.18.4": {"flags": ["--json", "--parallel"]},
    "v0.21.0": {"flags": ["--json", "--parallel", "--skip-context7", "--ensemble", "--mode"]}
}
COMPAT

echo "Compatibility matrix written to $BIN_DIR/compat.json"
echo "Done."
