#!/usr/bin/env sh
# Builds the two wasm modules the playground loads and copies them into
# the website beside this repo (../docs/public/play). Needs wasm-pack,
# emscripten on the PATH (source emsdk_env.sh), a Luau checkout at
# $LUAU_SRC, and its emscripten build at $LUAU_WASM (see the README of
# alloy-web).
set -eu
cd "$(dirname "$0")/.."
out="../docs/public/play"
mkdir -p "$out"

# 1. The compiler.
(cd alloy-web && wasm-pack build --target web --release --out-dir pkg)
cp alloy-web/pkg/alloy_web.js alloy-web/pkg/alloy_web_bg.wasm "$out/"

# 2. The analyzer and the VM.
: "${LUAU_SRC:?set LUAU_SRC to a Luau checkout}"
: "${LUAU_WASM:?set LUAU_WASM to its emscripten build directory}"
em++ -O2 -std=c++17 -fexceptions alloy-web/luau/alloy_luau.cpp \
  -I "$LUAU_SRC/Analysis/include" -I "$LUAU_SRC/Ast/include" -I "$LUAU_SRC/Config/include" \
  -I "$LUAU_SRC/Common/include" -I "$LUAU_SRC/VM/include" -I "$LUAU_SRC/Compiler/include" \
  "$LUAU_WASM/libLuau.Analysis.a" "$LUAU_WASM/libLuau.Compiler.a" "$LUAU_WASM/libLuau.VM.a" \
  "$LUAU_WASM/libLuau.Config.a" "$LUAU_WASM/libLuau.Ast.a" "$LUAU_WASM/libLuau.Bytecode.a" "$LUAU_WASM/libLuau.Common.a" \
  -sMODULARIZE=1 -sEXPORT_ES6=1 -sEXPORT_NAME=createLuau -sENVIRONMENT=web,node \
  -sEXPORTED_FUNCTIONS='["_alloy_init","_alloy_set_module","_alloy_check","_alloy_autocomplete","_alloy_hover","_alloy_run","_malloc","_free"]' \
  -sEXPORTED_RUNTIME_METHODS='["ccall","cwrap"]' \
  -sALLOW_MEMORY_GROWTH=1 -sINITIAL_MEMORY=134217728 -sSTACK_SIZE=4194304 \
  -o "$out/luau.js"

# 3. The definitions the analyzer reads.
cp tools/types/globalTypes.d.luau "$out/globalTypes.d.luau"
echo "wrote $out"
