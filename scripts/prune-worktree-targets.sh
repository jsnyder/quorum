#!/usr/bin/env bash
# Reclaim disk from stale worktree build artifacts.
#
# Each `git worktree add` starts with an empty target/, so every worktree
# grows its own 5-10GB copy of the same dependency builds. Fifteen of them
# reached 80GB before anyone looked.
#
# Removes target/ from worktrees whose HEAD commit is older than MAX_AGE_DAYS.
# target/ is rebuildable by definition -- no source, branch, or uncommitted
# file is ever touched. Skips the primary checkout and anything with a live
# cargo process.
#
# Usage:
#   scripts/prune-worktree-targets.sh            # dry run, prints the plan
#   scripts/prune-worktree-targets.sh --apply    # actually delete
#   MAX_AGE_DAYS=7 scripts/prune-worktree-targets.sh --apply
set -euo pipefail

MAX_AGE_DAYS="${MAX_AGE_DAYS:-14}"
APPLY=0
[ "${1:-}" = "--apply" ] && APPLY=1

root=$(git rev-parse --path-format=absolute --git-common-dir)
root=$(dirname "$root")
cd "$root"

# Worktrees with a cargo process running inside them are in active use.
busy_paths=$(ps -eo args 2>/dev/null | grep -oE '/[^ ]*/\.?worktrees/[^ /]+' | sort -u || true)

now=$(date +%s)
total=0

# `git worktree list --porcelain` emits a "worktree <path>" line per entry.
# The first is always the primary checkout, which we never touch.
git worktree list --porcelain | awk '/^worktree /{print $2}' | tail -n +2 | while read -r wt; do
  [ -d "$wt/target" ] || continue

  if printf '%s\n' "$busy_paths" | grep -qxF "$wt"; then
    printf '  skip   %-44s (cargo running)\n' "$(basename "$wt")"
    continue
  fi

  # Age from the checked-out commit, not mtime: mtime changes when anything
  # in the tree is read or built, so it reports every worktree as fresh.
  last=$(git -C "$wt" log -1 --format=%ct 2>/dev/null || echo "$now")
  age=$(( (now - last) / 86400 ))
  if [ "$age" -lt "$MAX_AGE_DAYS" ]; then
    printf '  keep   %-44s (%sd old)\n' "$(basename "$wt")" "$age"
    continue
  fi

  size=$(du -sm "$wt/target" 2>/dev/null | cut -f1)
  total=$(( total + size ))
  if [ "$APPLY" = 1 ]; then
    rm -rf "$wt/target"
    printf '  freed  %-44s %sM (%sd old)\n' "$(basename "$wt")" "$size" "$age"
  else
    printf '  would free %-40s %sM (%sd old)\n' "$(basename "$wt")" "$size" "$age"
  fi
done

[ "$APPLY" = 1 ] || echo "(dry run -- pass --apply to delete)"
