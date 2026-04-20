#!/usr/bin/env bash
# Fixture: toctou-lock-touch
LOCK=/tmp/app.lock

# match: classic TOCTOU check-then-touch
if [ ! -f "$LOCK" ]; then
  touch "$LOCK"
  echo "work"
fi

# no-match: atomic mkdir lock
if mkdir /tmp/app.lockdir 2>/dev/null; then
  trap 'rmdir /tmp/app.lockdir' EXIT
  echo "work"
fi
