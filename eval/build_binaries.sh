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
