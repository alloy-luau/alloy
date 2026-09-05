# Alloy

Alloy is a strict superset of Luau. It transpiles to plain Luau for Roblox.
Every valid Luau file is a valid Alloy file, unless it binds one of the
words Alloy reserves, such as `new`, `await`, `match`, or `struct`.

## Layout

This is one of three repositories that sit side by side in one folder,
which `alloy.code-workspace` in that folder opens as a VS Code workspace:

| Repository                        | Folder       | What it is                                  |
|-----------------------------------|--------------|---------------------------------------------|
| `alloy-luau/alloy`                | `crates`     | This one: the compiler, the server, the std |
| `alloy-luau/extensions`           | `extensions` | The VS Code extension, and Zed to come      |
| `alloy-luau/alloy-luau.github.io` | `docs`       | The website: landing page and the book      |
| `alloy-luau/examples`             | `examples`   | One file per feature, and the test project  |

Inside this repository:

| Path            | What it is                         | Package          | Tag prefix       |
|-----------------|------------------------------------|------------------|------------------|
| `alloy-syntax`  | Lexer, parser, lossless printer    | `alloy-syntax`   | `alloy-syntax-v` |
| `alloy`         | Compiler library and `alloy` CLI   | `alloy-luau`     | `alloy-v`        |
| `luaux`         | luaux 0.2.0 fork for `.alx` markup | `alloy-luaux`    | none, path only  |
| `alloy-lsp`     | Language server, a larvae-lsp fork | `alloy-luau-lsp` | `alloy-lsp-v`    |
| `std`           | The runtime module and its specs   |                  |                  |

`scripts/build.sh` builds the crates and, when the extensions repository
is checked out beside this one, the extension too. `scripts/docs-content.sh`
writes the website's reference data into `../docs`. The tests compile
every file of `../examples`, so that repository must be beside this one.

## Develop

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
alloy build && scripts/check-build.sh --strict

cd extensions/vscode
npm ci
npm run lint
npm run compile
npm run package
```

CI runs the cargo and npm commands on every push to `main` and on every
pull request.

`scripts/test-std.sh` runs the runtime's own tests with `lest`, from
`std/lest.toml`: the specs under `std/tests` require `../alloy` and cover
every std type on lest's native VM. That VM has no `task` and no `game`,
so the runtime's coroutine fallback runs there, and the specs cover it.
Pass lest's arguments through, such as `-t Signal`.

`scripts/build.sh` runs the same build in one step: every crate, then the
extension. `--release` uses the release profile and `--no-package` skips
the `.vsix`.

## Install

```sh
scripts/build.sh --release --install
```

`--install` runs `alloy self install` on the fresh build. The command
copies `alloy` and `alloy-lsp` to `~/.alloy/bin` and prints the PATH
line to add when the directory is not on it. `--dir <path>` picks
another directory. `alloy self uninstall` removes the two binaries. The
extension starts `alloy-lsp` from the PATH, so the editor finds the
server after the install.

`scripts/check-build.sh` proves a build output is Luau: `luau-compile`
parses every file, each output has the line count of its source, and
`luau-lsp analyze` with the new solver and the Roblox definitions reports
the type errors. `--strict` fails on a type error. The script needs
`luau-compile` and `luau-lsp` on the PATH. A `.d.luau` output joins the
definitions instead of the sources.

## Build a project

`alloy build` with no file reads the nearest `alloy.toml`, compiles every
`.aly`, `.d.aly`, and `.alx` under `in`, and mirrors the tree under `out`.
`alloy init` writes the default file:

```toml
[build]
in = "src"
out = "build"
exclude = []          # globs relative to `in`, e.g. "**/*.spec.aly"
clean = false         # delete an output whose source is gone
artifact = "ship"     # ship runs on Roblox; check is what luau-lsp sees

[emit]
# wait_timeout = 5           # seconds for every WaitForChild that `=>` emits
# std_require = "@alloy"     # the require path of the runtime module
# erase_type_imports = true  # blank `import type` lines in the ship artifact

[lint]
strict = false        # turn on implicit_any and missing_return_type
deny = []             # lints that fail `alloy lint`
warn = []
allow = []
```

When the folder has no Luau configuration, `alloy init` also writes
`.luaurc` and `.config.luau` with the same content: `languageMode =
"strict"` and the `@alloy` alias for the runtime the build writes. Alloy
reads both files wherever it reads one, and `.config.luau` wins when both
exist, as in Luau.

With `wait_timeout` set, `=>` passes the seconds to `WaitForChild` and
guards the chain like `->`, since a timed wait can return nil. Unset, the
engine waits forever. `--wait-timeout <s>` overrides it for one run.

The build writes the runtime to `<out>/alloy.luau`. A file that uses a
runtime name requires it by a relative path, `./alloy` or `../alloy` by
depth, unless `std_require` names the path. `erase_type_imports` blanks
each `import type` line in the ship artifact; it is off by default, so the
output stays typed for anyone who analyzes it directly.

A `.alx` file runs through luaux first, which lowers the markup to calls
for the UI library named in `luaux.toml` beside `alloy.toml`. Without that
file the target is React. Every luaux setting applies.

`x.aly` becomes `x.luau`, `x.d.aly` becomes `x.d.luau`, and `x.alx`
becomes `x.luau`, each in the same subdirectory. An unchanged output is not rewritten, so rojo does not
resync it. `--out <dir>` and `--check` override the file for one run, and
`--config <path>` picks a file that is not in an ancestor directory.
`alloy build <file>` compiles one `.aly`, `.d.aly`, or `.alx` file to
stdout, or into `--out`.

## Mounts and project files

A project needs one description of where its folders land in the
DataModel, and the same description twice over: once for the sources the
editor sees and once for the compiled output Rojo serves. The `[mount]`
table is that description, written once:

```toml
[project]
name = "game"
runtime = "@game/ReplicatedStorage/Alloy"   # where build/alloy.luau mounts

[mount]
# alias = [path, mount]
server = ["src/server", "@game/ServerScriptService/Server"]
client = ["src/client", "@game/StarterPlayer/StarterPlayerScripts/Client"]
shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
pkg    = ["Packages", "@game/ReplicatedStorage/Packages"]
```

With one or more mounts, `alloy build` writes `default.project.json`, a
Rojo project over the sources, and `.alloy/build.project.json`, the same
tree over `build`, which `rojo serve .alloy/build.project.json` takes. It
also writes `.alloy/sourcemap.json`, which the language server maps onto
its mirror, and adds an alias per mount to `.luaurc`, so `@pkg/jecs`
resolves in the editor. Roblox reads no `.luaurc`, so the ship artifact
rewrites each `require("@pkg/jecs")` into the relative instance path from
the file's own mount, and requires the runtime the same way. `.alloy` is
Alloy's folder for generated files; `sourcemap.json` in it is ignored by
git through the `.gitignore` the build writes there.

## Check, lint, format, document

`alloy --help` is one short screen: the commands, and `alloy <command>
--help` for a command's options. Every command reports in one style: a
lilac check for a clean run, a red cross for a failed one, an amber mark
for a warning, and a summary line of counts. With no terminal or with
`NO_COLOR` set, the marks become the words `ok:`, `error:`, and
`warning:`. `alloy build -W` builds again after every change under `in`
until ctrl-c.

```sh
alloy check              # compile everything, write nothing, report errors and lints
alloy lint               # the lints of the [lint] table; --strict, --deny-warnings, --list
alloy fmt                # format the sources in place; --check writes nothing
alloy doc strict         # an article; `alloy doc` lists every topic
```

Each command takes a file instead of the project. `alloy check` exits
with one on any diagnostic or denied lint, so it fits a pre-commit hook.

The lints are advice: the code runs, and the lint names a habit that
costs bugs. `optional_access` finds a `T?` parameter, or the result of a
function that returns `T?`, indexed with nothing guarding it.
`unreachable_default` finds a `default` arm under arms that cover every
variant, and `empty_default` one with nothing in it. `deprecated_global`
finds `wait`, `spawn`, and `delay`. `unused_import` finds an imported name
the file never uses. With `strict = true`, `implicit_any` finds a named
function parameter with no type and `missing_return_type` an exported
function or an impl method with no return annotation. The language server
shows the same lints as warnings in the editor.

`alloy fmt` changes whitespace and nothing else: four-space indentation
from the block structure, no trailing spaces, tabs to spaces, one blank
line at most in a row, one newline at the end. A long string or a long
comment keeps its lines. The editor's Format Document runs the same code
through the server.

`alloy doc <topic>` prints the same text the editor shows on hover, for a
keyword, an operator, an intrinsic, an attribute, a std name, or a lint,
plus the articles: `strict`, `exhaustive`, `wire`, `lint`, `fmt`, `check`,
`build`, `config`, `luaurc`, and `directives`.

## Strict by default

Every `.aly` file checks in Luau strict mode unless the project says
otherwise: `alloy init` writes the mode into `.luaurc` and `.config.luau`,
and the server gives a workspace with neither file the same setting. On
top of the checker, the compiler holds these contracts at compile time:

- A `match` with no `default` covers every variant, and the error names
  the variant with no arm.
- `new Name { ... }` sets every field without a default and names no field
  the struct lacks. A spread turns the check off.
- `impl Trait for Name` writes every method the trait declares without a
  body, with the trait's arity.
- `@sealed` on a struct rejects a write to an undeclared field, at
  runtime through `__newindex` and at check time through the table type.
- A `remote` parameter cannot be a function, a `thread`, a `Future`, or a
  `Signal`: a remote carries data.

## Editor support

`alloy-lsp` is the language server. It runs `luau-lsp` as a child and
keeps a shadow `.luau` document for every Alloy file. The shadow is the
check artifact, compiled in memory on each edit. The child resolves a
`require` only to a file on disk, so the shadows live in a mirror of the
workspace under the temp directory, beside a copy of every plain Luau
file, and the child's root is the mirror. Nothing is written into the
project. Every URI and position that crosses in either direction maps
through the span map. The editor only sees its own files. An error in
generated code points at the construct that produced it; a warning whose
range touches generated text describes the emit and is dropped, as is an
unused-variable lint for a name that `$nameof` or `$stringify` consumed.
A `.d.aly` reaches the child only as a definitions file, never as a
document. Alloy's own diagnostics join the child's.

What the server adds on top of the child:

- Inlay hints for variables, loop variables, parameters, and return
  types. A double click on a type hint writes it into the file. A hint
  that would describe a temp of the emit is dropped. The child computes
  hints against the current text on every request, so a hint never
  splits a word after an edit.
- Auto-imports in completion: a name another Alloy file exports offers
  itself, and accepting it writes `import { name } from "./path"`, joining
  an existing import from that file when there is one.
- Rename follow-up: when a file or folder moves, the server asks once,
  and on "Update imports" rewrites every relative `import` and `require`
  that named the old path, in the moved file too.
- Every `.d.aly` under the workspace loads as a definitions file, so a
  `declare` in it is a global everywhere.
- `.alx` intellisense inside markup: hover on a tag shows the Roblox class
  and its ancestry, hover on an attribute says whether it is a property
  or an event, and completion offers classes, bound components,
  properties, and events. Outside markup the child answers as usual.
- Semantic tokens move to source columns; tokens in generated text go.
- Hover shows Alloy, not the emit. A struct, interface, enum, or trait
  name shows its declaration as written, with what an interface extends
  and the traits and methods a struct has. A `const`, an `export`, or an
  `async function` keeps those words in front of its name. A std name
  such as `HashMap` shows its doc, and every type reads `Future<T>`, not
  the runtime's `__alloy.Future<T>`. The std names join every plain
  completion, except one the editor triggers with a newline: that is the
  child's `end` trigger, and Enter must not pop a list of names.
- An async function without a return type infers one: hover shows
  `Future<T>`, and the insertable return type hint is the inner `T`,
  since that is what the source declares.
- A conditional binding is narrowed. `if local player = f() then`
  declares `player` inside the branch from the temp the test refined,
  so it is `Player` there, not `Player?`, on the header and in a
  closure alike; `while local` and the expression form do the same.
- A trait's signatures without a body are typed nils on the trait table,
  so `Shape.` completes `area` beside the defaults and hovers its type.
  Assigning nil adds no key, so the runtime table holds the defaults only.
- A doc comment reaches hover: the `--` or `---` lines right above a
  struct, interface, enum, trait, variant, type alias, `declare`, or any
  binding show under its declaration. An enum variant hovers from
  `Msg.Join` and from a `match` pattern, where the emit has only a string.
  In a `.d.aly`, a `declare` hovers as its own text.
- ALX markup has its own grammar: an element owns its head, children,
  holes, and comments up to its matching closing tag, so text between
  tags is plain and `<!-- -->` is a comment. A closing tag hovers as the
  element does.
- `delete x` picks the method at runtime through `__alloy.delete`: a
  connection disconnects (`Disconnect`, then `disconnect`), anything else
  destroys (`Destroy`, then `destroy`), so a user class with either
  spelling works. The check artifact types the argument as `Deletable`.
- A struct constructs through `new` alone, so construction never reads
  as a call. `new Name { ... }` is the fields form, with defaults filling
  in; a struct whose `impl` writes `new` or `New` constructs with
  `new Name(...)` instead, calling whichever it wrote, and keeps the
  fields form for its own constructor. Bare `Name { ... }` and
  `Name(...)` are diagnostics, as is `new Name(...)` on a struct that
  writes no constructor. A foreign class has no class call of Alloy's,
  so `new` stays optional there.
- Go to definition lands on an Alloy declaration: a struct, interface,
  enum or variant, trait, type alias, macro, or attribute, in the file
  first and then anywhere in the workspace. Everything the emit keeps as
  written, locals and Luau types included, the child resolves.
- After Enter on a line that opens a block, the editor inserts the `end`
  a line below the cursor with the opener's indentation, for functions,
  `if`, loops, `do`, `repeat`, `struct`, `enum`, `interface`, `trait`,
  `impl`, `macro`, and `match`; `alloy-luau.completion.autocompleteEnd`
  turns it off. The server answers `alloy/blockEnd` from the lexer's
  tokens, so a keyword in a string or a comment never counts.
- After a type colon or `->`, completion lists the workspace's structs,
  interfaces, enums, traits, and type aliases, the std types, and the
  primitives, beside the classes the child knows. `attribute name(...)`
  completes `on`, then its targets.
- `@rename` and `@skip` are built-in attributes: `@derive(Serialize)`
  reads them. A user attribute hovers like a built-in one, its use and
  then what it goes on. A macro with a long body hovers as its header.
- `.alx` gets no semantic tokens: the lowered code's columns are not the
  source's, and the grammar colors markup on its own.
- Completion after `@` lists the attributes that fit the position: the
  wire widths in a remote's parameters, `@rename`, `@skip`, and the
  widths in a struct body, the variant ones in an enum body, and at the
  top level the ones for whatever the next declaration is, a function,
  a struct, an enum, or a remote. A declared attribute joins the list
  where its `on` targets allow. Inside `@derive( )` the derive names; after `$` the
  intrinsics and the macros; after a remote's closed parameter list the
  `from`, then `client` or `server`, then `or` and the other side, and
  nowhere else; in an `import`,
  `{`, `type`, `* as`, `as`, `from`, and the target module's exports.
  The proxy answers these itself, since the emit has none of them. A
  plain completion also offers the Alloy keywords.
- A user attribute hovers as `attribute name(params) on targets`, a macro
  as `macro name(params)` with its body, and a derive name inside
  `@derive( )` as what it generates. A hover keeps the annotation the
  source wrote, `items: Item[]`, instead of the child's expansion.
- An attribute on a `local`, or a wire width on a plain function's
  parameter, is a diagnostic, and the text leaves the emit so the output
  stays Luau. A wire width on a struct field is recorded, as on a remote
  parameter.
- Inside the string of an `import`, a `require`, or an `import(...)`, the
  completion lists the modules and directories the path can continue
  with: relative from the file, `@self/`, every alias of the nearest
  `.luaurc`, and `@game/` walked through `sourcemap.json` when the
  project has one. The mirror carries `.luaurc` and `sourcemap.json`, so
  the child types a `@game` or alias require the way it types a plain
  one.
- `alloy-luau.sourcemap.file` names the sourcemap, relative to the
  project root; the server reads it for `@game/` and passes it to the
  child. `alloy-luau.sourcemap.autogenerate` runs
  `alloy-luau.sourcemap.generatorCommand` in the root on every script
  save, `rojo sourcemap --output sourcemap.json` by default; the command
  also runs from the palette as Alloy: Generate Sourcemap.
- `alloy-luau.studioPlugin.enabled` accepts the luau-lsp Studio companion
  plugin for the DataModel tree, off by default, on
  `alloy-luau.studioPlugin.port` (3667). The Alloy setting decides, over
  any `luau-lsp.studioPlugin` value, so two servers never share a port
  by accident.
- `import("./m")` is the expression form of `import`: a `require`, typed
  from the module when the path is a string or an instance chain such as
  `script.Parent.Module`. `import<<T>>(expr)` gives a dynamic path the
  type `T`; without it the value is `unknown`.
- `c ? a : b` is the ternary, emitted as `if c then a else b` in
  parentheses, and it nests. Inside a branch a `:` that touches its
  receiver and a name is a method call, so `n > 0 ? tostring(n):upper()
  : "none"` works; the else marker has a space before it.
- `--@alloy-ignore` on its own line turns Alloy's own diagnostics off for
  the next line, and at the end of a line for that line. `--@alloy-nocheck`
  among a file's leading comments turns them off for the whole file. Syntax
  errors stay in both cases, and the child's Luau diagnostics are not
  affected.
- The words that start an Alloy statement or expression are reserved as
  names: `struct`, `enum`, `trait`, `impl`, `interface`, `remote`,
  `macro`, `attribute`, `match`, `const`, `async`, `await`, `try`, `new`,
  `delete`, `import`, and `export`. A local, a parameter, a plain
  function, or a bare expression named so is a diagnostic. After `.` or
  `:` each is a field, so `Instance.new` and an `impl`'s `function new`
  keep working. `client`, `server`, `from`, `as`, and the other words
  with a meaning only inside a construct stay free.
- `local part = new Instance("Part") { ... }` declares `part` on its own
  line and assigns each field through the name, so the binding hovers
  with its class, and the hover lists the fields it was initialized
  with. A key in a struct's raw constructor hovers as the struct's field.
- A std value's type reads as its name: `Future<T>`, and `T[]` for an
  array, instead of the shape the runtime builds. A directive such as
  `--!strict` is never a doc comment.
- `Partial<T>`, `Readonly<T>`, and `Sink<T>` are language-level types:
  the std builds each with a type function. A file that declares a type
  of the same name keeps its own, in the emit and in hover. Hover never
  shows a `__mapped_` or `__alloy.` name, and a type function hovers as
  `type function Name<...>(...)`.
- Extension methods on foreign types type like built-ins. At startup the
  server reads every `impl` on a foreign type under the root. A method
  on an Instance class or a datatype joins the definitions that declare
  the target, so `v:flat()` types, completes after `v:`, and hovers; the
  check artifact keeps that call as written and types `self`. A
  primitive such as `string` has no class block, so the server declares
  a helper table, `__alloy_string`, and the check artifact calls
  `__alloy_string.trim(s)`; hover and types come from the helper, and
  the server adds the names to the completion list after `:` on a
  string and after `string.`. The ship artifact routes every extension
  call through the dispatcher. The set is project wide: the build and
  the server collect it before any file compiles. A method added after
  startup needs a server restart.
- Hover on Alloy-only syntax is answered by the server: every keyword,
  operator, intrinsic, and attribute has its own text. A hover on a
  replaced name, such as a struct name, goes to the child at the place
  the declaration landed. Other replaced punctuation gets no hover.
- Formatting is not offered, since it would describe the shadow.

```sh
alloy-lsp --luau-lsp ~/.ember/bin/luau-lsp --definitions globalTypes.d.luau
```

- `--luau-lsp <path>`: the child binary. Default: `ALLOY_LUAU_LSP`, then
  `luau-lsp` on the PATH.
- `--definitions <path>`: a definitions file for the child. A `.d.aly`
  compiles to `.d.luau` in the temp directory first. Repeatable.
- `--docs <path>`: the API docs JSON for the child, for hover text.
- `--old-solver`: do not pass `--flag:LuauSolverV2=true` to the child.
- `--log-level <level>`: what stderr shows: `off`, `error`, `warn`,
  `info`, `debug`, or `trace`. `trace` names each message direction.
  Default: `ALLOY_LSP_LOG`, then `warn`. `--log` means `trace`.
- Arguments after `--` go to the child as they are.

The child asks for its settings over `workspace/configuration`, and the
server answers from what the editor sent as `initializationOptions` or
`workspace/didChangeConfiguration`: a `luauLsp` object holds a whole
luau-lsp section, and an `inlayHints` object overlays it. The `fflags`
part of that section never reaches the child as settings, since the
server rejects it and falls back to its defaults, where every inlay
hint is off; it becomes `--flag:Name=value` arguments on the child's
command line instead, and `enableNewSolver = false` drops the new
solver flag. The VS Code
extension sends the user's `luau-lsp` settings plus the
`alloy-luau.inlayHints.*` settings, and starts the server with
`alloy-luau.server.path`, `alloy-luau.server.luauLspPath`,
`alloy-luau.types.definitionFiles`, and `alloy-luau.server.args`.
`luaux.toml` picks the `.alx` target as in a build.

## Release

Each package releases on its own. A tag names the package and the version:

```sh
scripts/release.sh alloy-syntax 0.2.0 # tag alloy-syntax-v0.2.0
scripts/release.sh alloy 0.2.0       # tag alloy-v0.2.0
scripts/release.sh alloy-lsp 0.2.0   # tag alloy-lsp-v0.2.0
scripts/release.sh vscode 0.2.0      # tag vscode-v0.2.0
```

The script bumps the manifest, commits, tags, and pushes. The tag starts a
workflow that builds the artifacts and creates a GitHub release. The
workflow fails when the tag and the manifest disagree on the version.

Crate releases build binaries for Linux (x86_64, aarch64), macOS (x86_64,
aarch64), and Windows (x86_64). The extension release attaches the `.vsix`.

Optional secrets:

- `CARGO_REGISTRY_TOKEN`: publish the crate to crates.io.
- `VSCE_PAT`: publish the extension to the Marketplace.

Without a secret the matching publish step prints a notice and skips.
