#!/usr/bin/env bash
# Builds every crate in the workspace and, when the extensions repo is
# checked out beside this one, the VS Code extension.
#
#   scripts/build.sh [--release] [--no-package] [--install]
#
# The crates build in debug mode; --release builds them with the release
# profile. The extension installs its dependencies when node_modules is
# missing or older than package-lock.json, compiles the TypeScript, and
# packages a .vsix. --no-package stops after the compile step.
#
# The extension does not bundle alloy-lsp. It starts the binary from
# PATH or from the `alloy-luau.server.path` setting. --install runs
# `alloy self install` on the fresh build, which copies alloy and
# alloy-lsp to ~/.alloy/bin.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
ext="$root/../extensions/vscode"
profile=debug
package=1
install=0
for a in "$@"; do
  case "$a" in
    --release) profile=release ;;
    --no-package) package=0 ;;
    --install) install=1 ;;
    *)
      echo "usage: scripts/build.sh [--release] [--no-package] [--install]" >&2
      exit 1
      ;;
  esac
done

for tool in cargo npm; do
  if ! command -v "$tool" >/dev/null; then
    echo "$tool is not installed" >&2
    exit 2
  fi
done

echo "==> crates ($profile)"
cargo_args=(build --workspace --all-targets)
[ "$profile" = release ] && cargo_args+=(--release)
(cd "$root" && cargo "${cargo_args[@]}")

if [ "$install" = 1 ]; then
  echo "==> install"
  "$root/target/$profile/alloy" self install
fi

if [ ! -d "$ext" ]; then
  echo "no extensions checkout beside this repo; skipping the extension"
  echo
  echo "binaries: $root/target/$profile/alloy, $root/target/$profile/alloy-lsp"
  exit 0
fi

echo "==> extension: dependencies"
if [ ! -d "$ext/node_modules" ] || [ "$ext/package-lock.json" -nt "$ext/node_modules" ]; then
  (cd "$ext" && npm ci)
else
  echo "node_modules is current"
fi

echo "==> extension: compile"
(cd "$ext" && npm run compile)

if [ "$package" = 1 ]; then
  echo "==> extension: package"
  (cd "$ext" && npm run package)
fi

echo
echo "binaries: $root/target/$profile/alloy, $root/target/$profile/alloy-lsp"
if [ "$package" = 1 ]; then
  vsix="$(ls -t "$ext"/*.vsix | head -1)"
  echo "extension: $vsix"
fi
