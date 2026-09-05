<p align="center">
  <img src="assets/alloy-readme-banner.png" alt="Alloy" width="100%">
</p>

<p align="center">
  <a href="https://crates.io/crates/alloy-luau"><img src="https://img.shields.io/crates/v/alloy-luau?color=7a58e0&label=crates.io" alt="crates.io"></a>
  <a href="https://alloy-luau.github.io/docs/"><img src="https://img.shields.io/badge/docs-alloy--luau.github.io-7a58e0" alt="Docs"></a>
  <a href="https://github.com/alloy-luau/alloy/actions/workflows/ci.yml"><img src="https://github.com/alloy-luau/alloy/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/alloy-luau/alloy/releases"><img src="https://img.shields.io/github/v/release/alloy-luau/alloy?color=7a58e0&label=release" alt="Release"></a>
  <a href="https://ko-fi.com/savruun"><img src="https://img.shields.io/badge/Ko--fi-support%20Alloy-ff5e5b?logo=ko-fi&logoColor=white" alt="Ko-fi"></a>
</p>

<h1 align="center">Alloy</h1>

<p align="center">
  A strict superset of Luau that compiles to plain Luau, line for line.
</p>

Every Luau file is already an Alloy file. Alloy adds structs, enums,
traits, pattern matching, safe access, async, typed remotes, and a
strict checker. Each construct compiles to fixed Luau on the same line,
so a stack trace, a breakpoint, and an analyzer diagnostic all point at
the line you wrote. One binary builds, checks, lints, formats, and
documents. One language server shows Alloy to the editor.

```luau
struct Player as
    name: string
    hp: number = 100
end

enum Msg as
    Join(Player)
    Leave(Player)
end

local function greet(m: Msg): string
    match m with
        case Join(p) then return `welcome, {p.name}`
        case Leave(p) then return `bye, {p.name}`
    end
end

local hp = workspace->Arena->Boss?.Humanoid?.Health ?? 0
```

## Install

```sh
cargo install alloy-luau alloy-luau-lsp
alloy init          # alloy.toml, .luaurc, .config.luau
alloy build -W      # compile src into build, again on every change
```

The [releases](https://github.com/alloy-luau/alloy/releases) carry a zip
per platform with both binaries. The
[VS Code extension](https://github.com/alloy-luau/extensions) starts
`alloy-lsp` from PATH.

## Learn

The book at [alloy-luau.github.io/docs](https://alloy-luau.github.io/docs/)
shows every construct beside its emitted Luau, the strictness contracts,
the lints, and the toolchain. `alloy doc <topic>` prints any of it on the
terminal, and every diagnostic names its section: `alloy doc 4.2`.

| Command                | What it does                                   |
|------------------------|------------------------------------------------|
| `alloy build`          | Compile the project; `-W` watches               |
| `alloy check`          | Compile, write nothing, report errors and lints |
| `alloy flux`           | Compile, type-check, and lint; `--fix` rewrites |
| `alloy lint`           | Run Flux, the lints, alone                      |
| `alloy fmt`            | Run Anneal, the formatter, over the sources     |
| `alloy test`           | Write a lest spec per source with a `@test`     |
| `alloy doc <topic>`    | Explain a keyword, an operator, a lint, a topic |

## This repository

| Path           | Package          | What it is                          |
|----------------|------------------|-------------------------------------|
| `alloy-syntax` | `alloy-syntax`   | Lexer, parser, lossless printer     |
| `alloy`        | `alloy-luau`     | Compiler library and `alloy` CLI    |
| `alloy-lsp`    | `alloy-luau-lsp` | Language server, a proxy over luau-lsp |
| `luaux`        | `alloy-luaux`    | Markup lowering for `.alx` files    |
| `std`          |                  | The runtime module and its specs    |

The [extensions](https://github.com/alloy-luau/extensions),
[examples](https://github.com/alloy-luau/examples), and
[website](https://github.com/alloy-luau/alloy-luau.github.io) live in
their own repositories. Check them out beside this one; the scripts and
the tests expect that layout.

## Develop

```sh
git clone https://github.com/alloy-luau/examples ../examples
cargo test --workspace
scripts/build.sh --release --install     # alloy and alloy-lsp into ~/.alloy/bin
scripts/check-build.sh --strict          # the output through luau-compile and luau-lsp
```

A release is `scripts/release.sh 1.2.3`: it bumps every crate, tags
`v1.2.3`, and CI builds the zips, the GitHub release, and publishes the
crates.
