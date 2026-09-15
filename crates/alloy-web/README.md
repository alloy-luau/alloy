# alloy-web

The playground's compiler: the Alloy desugar, the completion contexts,
the keyword and declaration hovers, and the shape folds, built to wasm
for the browser. The site pairs it with Luau's analyzer and VM, built
to wasm from `luau/alloy_luau.cpp`.

## Build

```sh
cargo install wasm-pack
git clone --depth 1 https://github.com/luau-lang/luau.git /path/to/luau
# emscripten: https://emscripten.org/docs/getting_started/downloads.html
source /path/to/emsdk/emsdk_env.sh
mkdir /path/to/luau-wasm && cd /path/to/luau-wasm
emcmake cmake /path/to/luau -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DLUAU_BUILD_CLI=OFF -DLUAU_BUILD_TESTS=OFF -DCMAKE_CXX_FLAGS=-fexceptions
ninja Luau.Analysis Luau.Ast Luau.Config Luau.Compiler Luau.VM Luau.Bytecode Luau.Common

cd crates
LUAU_SRC=/path/to/luau LUAU_WASM=/path/to/luau-wasm scripts/playground-wasm.sh
```

The script writes `alloy_web.js`, `alloy_web_bg.wasm`, `luau.js`,
`luau.wasm`, and `globalTypes.d.luau` into `../docs/public/play`, which
the site commits, so a Pages build needs neither Rust nor emscripten.

## What each side answers

- `set_source` compiles and keeps the text; `to_source`, `to_check`,
  and `generated_at` speak the span map, so the analyzer's positions in
  the check artifact turn back into the source's.
- `complete` gives the items Alloy owns (attributes, macros, `@derive`
  and `@cfg` arguments, a remote's side, the import forms, its
  keywords) and says whether the analyzer's list belongs beside them.
- `hover` answers a keyword, an operator, or a declaration of the file;
  `fold` reads an analyzer type the way the source does.
- The C++ side: `alloy_init(definitions)`, `alloy_set_module(name,
  source)`, `alloy_check()`, `alloy_autocomplete(line, col)`,
  `alloy_hover(line, col)`, and `alloy_run(runtime, source)`, each
  answering JSON or text through `ccall`.
