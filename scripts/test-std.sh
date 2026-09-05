#!/usr/bin/env bash
# Runs the runtime tests with lest.
#
#   scripts/test-std.sh [lest arguments]
#
# The specs live in std/tests/*.spec.luau and require the runtime as
# ../alloy. lest's native VM has no `task` and no `game`, so the runtime
# takes its coroutine fallback there, and the specs cover that path.
set -euo pipefail

export PATH="$HOME/.ember/bin:$HOME/.cargo/bin:$PATH"
root="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v lest >/dev/null; then
  echo "lest is not installed; see https://github.com/lest-luau/lest" >&2
  exit 2
fi

cd "$root/std"
exec lest "$@"
