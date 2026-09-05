#!/usr/bin/env bash
# Proves the build output is Luau the engine accepts.
#
#   scripts/check-build.sh [build-dir] [--strict]
#
# luau-compile parses every file, so a syntax error always fails. Then
# luau-lsp analyze runs with the Roblox definitions and the new solver;
# type errors are counted, and --strict makes them fail too. Lints are
# not counted. A `.d.luau` output joins the definitions instead of the
# sources.
set -uo pipefail

export PATH="$HOME/.cargo/bin:$HOME/.ember/bin:$PATH"
root="$(cd "$(dirname "$0")/.." && pwd)"
dir="$root/build"
strict=0
for a in "$@"; do
  case "$a" in
    --strict) strict=1 ;;
    *) dir="$a" ;;
  esac
done

for tool in luau-compile luau-lsp; do
  if ! command -v "$tool" >/dev/null; then
    echo "$tool is not installed" >&2
    exit 2
  fi
done

mapfile -t files < <(find "$dir" -name '*.luau' | sort)
if [ "${#files[@]}" = 0 ]; then
  echo "no .luau files under $dir" >&2
  exit 2
fi

syntax=0
for f in "${files[@]}"; do
  # A definitions file is not Luau for the compiler; luau-lsp reads it.
  case "$f" in *.d.luau) continue ;; esac
  if ! luau-compile --binary "$f" >/dev/null 2>"$dir/.err"; then
    grep -E 'SyntaxError' "$dir/.err" | head -3
    syntax=$((syntax + 1))
  fi
done
rm -f "$dir/.err"

# The line guarantee: an output has the line count of its source.
lines=0
for f in "${files[@]}"; do
  rel="${f#"$dir"/}"
  case "$rel" in alloy.luau) continue ;; esac
  src="$root/examples/${rel%.luau}.aly"
  [ -f "$src" ] || src="$root/examples/${rel%.luau}.alx"
  [ -f "$src" ] || continue
  a=$(wc -l <"$src"); b=$(wc -l <"$f")
  if [ "$a" != "$b" ]; then
    echo "$f: $b lines, source has $a"
    lines=$((lines + 1))
  fi
done

sources=()
defs=(--definitions="$root/tools/types/globalTypes.d.luau")
for f in "${files[@]}"; do
  case "$f" in
    *.d.luau) defs+=(--definitions="$f") ;;
    *) sources+=("$f") ;;
  esac
done

# A definitions file that fails to load reports without a position, and
# the analyzer then drops it; both lines count.
analyze() {
  luau-lsp analyze --flag:LuauSolverV2=true "${defs[@]}" "${sources[@]}" 2>&1 \
    | grep -E 'TypeError|SyntaxError|\[ERROR\]' | grep -v 'Unknown require'
}
types=$(analyze | wc -l)
if [ "$strict" = 1 ]; then
  analyze | head -40
fi

echo "check-build: ${#files[@]} files, $syntax with syntax errors, $lines with a lost line, $types type errors"
[ "$syntax" = 0 ] || exit 1
[ "$lines" = 0 ] || exit 1
[ "$strict" = 0 ] || [ "$types" = 0 ] || exit 1
exit 0
