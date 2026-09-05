#!/usr/bin/env sh
# Regenerates the website's content/docs.json from the compiler's doc
# table. The website repo (alloy-luau.github.io) is expected beside
# this one as ../docs. Run it after a change to alloy/src/docs.rs or
# lint.rs.
set -eu
cd "$(dirname "$0")/.."
out="../docs/content/docs.json"
if [ ! -d "../docs/content" ]; then
  echo "no docs checkout beside this repo (../docs)" >&2
  exit 1
fi
cargo run -q --bin alloy -- doc --json > "$out"
echo "wrote $out"
