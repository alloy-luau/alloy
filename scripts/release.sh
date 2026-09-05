#!/usr/bin/env bash
# Bumps one package, commits, tags, and pushes. CI builds the release.
#
#   scripts/release.sh alloy-syntax 0.2.0 -> tag alloy-syntax-v0.2.0
#   scripts/release.sh alloy 0.2.0       -> tag alloy-v0.2.0
#   scripts/release.sh alloy-lsp 0.2.0   -> tag alloy-lsp-v0.2.0
# The VS Code extension releases from the extensions repo.
set -euo pipefail

target="${1:-}"
version="${2:-}"
if [ -z "$target" ] || [ -z "$version" ]; then
  echo "usage: scripts/release.sh <alloy-syntax|alloy|alloy-lsp> <version>" >&2
  exit 1
fi
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "version must look like 1.2.3 or 1.2.3-beta.1" >&2
  exit 1
fi

root="$(git rev-parse --show-toplevel)"
cd "$root"

if [ -n "$(git status --porcelain)" ]; then
  echo "working tree is not clean; commit or stash first" >&2
  exit 1
fi

case "$target" in
  alloy-syntax|alloy|alloy-lsp)
    manifest="$target/Cargo.toml"
    # Only the first version line is the package version.
    sed -i "0,/^version = \".*\"/s//version = \"$version\"/" "$manifest"
    cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace
    git add "$manifest" Cargo.lock
    ;;
  *)
    echo "unknown target: $target" >&2
    exit 1
    ;;
esac

tag="$target-v$version"
git commit -m "release: $target $version"
git tag -a "$tag" -m "$target $version"
git push origin HEAD
git push origin "$tag"
echo "pushed $tag; CI builds the release"
