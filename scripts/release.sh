#!/usr/bin/env bash
# Bumps every crate to one version, commits, tags, and pushes. CI builds
# the zips, the GitHub release, and the crates.io publish.
#
#   scripts/release.sh 0.2.0    -> tag v0.2.0
set -euo pipefail

version="${1:-}"
if [ -z "$version" ]; then
  echo "usage: scripts/release.sh <version>" >&2
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

for crate in alloy-syntax luaux alloy alloy-lsp; do
  # Only the first version line is the package version.
  sed -i "0,/^version = \".*\"/s//version = \"$version\"/" "$crate/Cargo.toml"
done
# The path dependencies name a version too.
sed -i "s/\(alloy-syntax = { version = \)\"[^\"]*\"/\1\"$version\"/" alloy/Cargo.toml alloy-lsp/Cargo.toml
sed -i "s/\(package = \"alloy-luau\", version = \)\"[^\"]*\"/\1\"$version\"/" alloy-lsp/Cargo.toml
cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace
git add ./*/Cargo.toml Cargo.lock

tag="v$version"
git commit -m "chore(release): $version"
git tag -a "$tag" -m "Alloy $version"
git push origin HEAD
git push origin "$tag"
echo "pushed $tag; CI builds the release"
