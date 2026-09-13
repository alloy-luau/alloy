//! The JSON Schema of `alloy.toml`, for the editor.
//!
//! One data table here names every table and key of `crate::config`,
//! with its type, default, and documentation. `alloy self schema` prints
//! it, and a TOML language server (Even Better TOML, taplo, Tombi)
//! completes and checks the file from it. The tests hold the table to
//! the `Config` struct, so a new key fails a test until it is listed.

use serde_json::{Map, Value, json};

use crate::lint::{Group, LINTS, LUAU_GROUP};

/// The type of one key, as the schema states it.
#[derive(Debug, Clone, Copy)]
pub enum Ty {
    Bool,
    /// A whole number, zero or more.
    Int,
    /// A number with a fraction.
    Number,
    Str,
    StrList,
    /// A string from a fixed set.
    Choice(&'static [&'static str]),
}

/// One key of a table.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub name: &'static str,
    pub ty: Ty,
    /// The default as JSON text; `None` for a key that is unset by default.
    pub default: Option<&'static str>,
    pub doc: &'static str,
    /// Names to offer for a `StrList`, without rejecting others.
    pub suggest: Option<fn() -> Vec<String>>,
}

/// One table of the file. A dotted name, `fmt.alx`, nests.
#[derive(Debug, Clone, Copy)]
pub struct Table {
    pub name: &'static str,
    pub doc: &'static str,
    pub keys: &'static [Key],
    /// The table takes any key, each with this value schema.
    pub open: Option<fn() -> Value>,
}

const fn key(name: &'static str, ty: Ty, default: &'static str, doc: &'static str) -> Key {
    Key {
        name,
        ty,
        default: Some(default),
        doc,
        suggest: None,
    }
}

const fn unset(name: &'static str, ty: Ty, doc: &'static str) -> Key {
    Key {
        name,
        ty,
        default: None,
        doc,
        suggest: None,
    }
}

const fn lint_list(name: &'static str, doc: &'static str) -> Key {
    Key {
        name,
        ty: Ty::StrList,
        default: Some("[]"),
        doc,
        suggest: Some(lint_names),
    }
}

/// The groups, then every lint, as a `[lint]` list accepts them.
pub fn lint_names() -> Vec<String> {
    Group::ALL
        .iter()
        .map(|g| g.name().to_string())
        .chain(std::iter::once(LUAU_GROUP.to_string()))
        .chain(LINTS.iter().map(|l| l.name.to_string()))
        .collect()
}

/// The names a `[lint.rules]` key takes: the list names, plus the
/// markup lints under their `alx.` prefix.
pub fn rule_names() -> Vec<String> {
    lint_names()
        .into_iter()
        .chain(
            crate::lint::ALX_LINTS
                .iter()
                .map(|l| format!("{}{}", crate::lint::ALX_PREFIX, l.name)),
        )
        .collect()
}

/// The value of a `[lint.rules]` entry: a level, or a table of them
/// for a dotted name such as `alx.static_conditional_child`.
fn rule_value() -> Value {
    let level = json!({
        "type": "string",
        "enum": ["allow", "warn", "deny"],
        "description": "What the lint does when it fires: `allow` is silent, `warn` prints and passes, `deny` prints and fails the run."
    });

    json!({
        "description": "The level of one lint, one group, or a table of them under a prefix: `alx.static_conditional_child = \"warn\"`.",
        "oneOf": [
            level,
            { "type": "object", "additionalProperties": level }
        ]
    })
}

/// The key completion of `[lint.rules]`: the known names first, and
/// any string after them, so a lint newer than the schema still
/// passes.
fn rule_key_schema(extra: &[String]) -> Value {
    let mut names = rule_names();
    // TOML nests a dotted key, so `alx` is a key of its own too.
    names.push(crate::lint::ALX_PREFIX.trim_end_matches('.').to_string());
    names.extend(extra.iter().cloned());

    json!({
        "anyOf": [
            { "type": "string", "enum": names },
            { "type": "string" }
        ]
    })
}

fn mount_value() -> Value {
    json!({
        "type": "array",
        "description": "The path on disk, relative to this file, and the DataModel location as `@game/Service/Folder`. A `.server.` or `.client.` file name picks the script class; `init` names its directory.",
        "items": [
            { "type": "string", "description": "The folder on disk, relative to this file." },
            { "type": "string", "description": "The DataModel location: `@game/Service/Folder`.", "pattern": "^@game/" }
        ],
        "minItems": 2,
        "maxItems": 2,
        "examples": [["src/server", "@game/ServerScriptService/Server"]]
    })
}

fn ingot_value() -> Value {
    json!({
        "description": "Where the ingot comes from: a path relative to this file that holds `ingot.toml` and the binary, or a GitHub release. `alloy ingot install` fetches it; a build never does.",
        "oneOf": [
            { "type": "string", "description": "A directory relative to this file." },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": { "type": "string", "description": "A directory relative to this file." },
                    "repo": { "type": "string", "description": "`owner/repo` on GitHub; the release `v<version>` holds the zip." },
                    "version": { "type": "string", "description": "The release to install. `^`, the default, is the latest release at install time, and `.alloy/ingots.lock` records what it resolved to. A version, `1.2.3` or `v1.2.3`, pins that release, and `alloy ingot update` leaves it alone.", "default": "^" },
                    "asset": { "type": "string", "description": "The asset name in the release. Unset means `<name>-ingot-<target>.zip`, then `<name>-ingot.zip`." },
                    "order": { "type": "integer", "description": "The pass the ingot's transform runs in, over the manifest's word. A lower number runs first." },
                    "lints": {
                        "type": "object",
                        "description": "Lints of this ingot switched on or off by name, over the manifest's defaults.",
                        "additionalProperties": { "type": "boolean" }
                    }
                }
            }
        ],
        "examples": ["ingots/tailwind", { "repo": "alloy-luau/tailwind-ingot" }, { "repo": "alloy-luau/tailwind-ingot", "version": "0.1.0" }]
    })
}

fn ingot_options_value() -> Value {
    json!({
        "type": "object",
        "description": "The options of one ingot, over the defaults its manifest declares. The key is the ingot's name under `[ingots]`; a key the manifest does not declare is an error.",
        "additionalProperties": true
    })
}

/// Where the published schema lives: the copy the VS Code extension
/// ships, on the main branch of the extensions repository.
pub const URL: &str =
    "https://raw.githubusercontent.com/alloy-luau/extensions/main/vscode/schemas/alloy.toml.json";

const BOOL: Ty = Ty::Bool;
const INT: Ty = Ty::Int;
const STR: Ty = Ty::Str;

pub const TABLES: &[Table] = &[
    Table {
        name: "build",
        doc: "What compiles, and where the output goes.",
        keys: &[
            key(
                "in",
                STR,
                r#""src""#,
                "The source root. Every `.aly` and `.alx` under it compiles, relative to the folder that holds this file.",
            ),
            key(
                "out",
                STR,
                r#""build""#,
                "The output root. The tree under `in` is mirrored under it, and the runtime is written beside it as `alloy.luau`.",
            ),
            key(
                "exclude",
                Ty::StrList,
                "[]",
                "Glob patterns, relative to `in`, of sources to skip.",
            ),
            key(
                "clean",
                BOOL,
                "false",
                "Delete an output whose source is gone.",
            ),
            key(
                "artifact",
                Ty::Choice(&["ship", "check"]),
                r#""ship""#,
                "Which artifact to write. `ship` runs on Roblox; `check` is what luau-lsp sees, with the types kept.",
            ),
        ],
        open: None,
    },
    Table {
        name: "emit",
        doc: "The few knobs that change what emitted code does.",
        keys: &[
            unset(
                "wait_timeout",
                Ty::Number,
                "Seconds passed to every `WaitForChild` that `=>` emits. Unset means no timeout: the engine waits forever and warns after five seconds. With a timeout the call can return nil, so `=>` guards like `->`.",
            ),
            unset(
                "std_require",
                STR,
                "The string emitted code passes to `require` for the runtime. Unset means a relative path to the `alloy.luau` the build writes, or the runtime's instance path when the tree holds the file.",
            ),
            key(
                "erase_type_imports",
                BOOL,
                "false",
                "Blank `import type` lines in the output so they add no runtime dependency. The output is then untyped for anyone who analyzes it directly.",
            ),
        ],
        open: None,
    },
    Table {
        name: "lint",
        doc: "Where every lint starts under `alloy flux` and `alloy lint`. `[lint.rules]` then sets one lint or one group. `alloy doc lints` names them.",
        keys: &[
            key(
                "recommended",
                BOOL,
                "true",
                "Apply the level each lint declares. Off, every lint starts at `allow`, and `[lint.rules]` alone turns one on.",
            ),
            key(
                "strict",
                BOOL,
                "true",
                "Turns the pedantic group on, at warn: `implicit_any`, `missing_return_type`, `explicit_any`, `todo_comment`, `print_debug`, `missing_doc`.",
            ),
            lint_list(
                "deny",
                "Deprecated: lints or groups that fail the run. Write `[lint.rules] <name> = \"deny\"`.",
            ),
            lint_list(
                "warn",
                "Deprecated: lints or groups that print and pass. Write `[lint.rules] <name> = \"warn\"`.",
            ),
            lint_list(
                "allow",
                "Deprecated: lints or groups that stay silent. Write `[lint.rules] <name> = \"allow\"`.",
            ),
        ],
        open: None,
    },
    Table {
        name: "lint.rules",
        doc: "The level of one lint: a lint name, a group name (correctness, suspicious, style, complexity, perf, roblox, pedantic, naming, or luau for the type checker's own), an ingot's `<ingot>/<lint>`, or `alx.<name>` for a markup lint. A name beats its group.",
        keys: &[],
        open: Some(rule_value),
    },
    Table {
        name: "flux",
        doc: "What `alloy flux` runs beyond the lints, and the limits of the complexity lints. The levels of the lints stay in `[lint]`. `alloy doc flux` explains it.",
        keys: &[
            key(
                "typecheck",
                BOOL,
                "true",
                "Run luau-lsp over the check artifact and report its type errors on the source lines.",
            ),
            key(
                "definitions",
                Ty::StrList,
                "[]",
                "Definitions files for the type check, `.d.luau` or `.d.aly`, relative to this file. The project's `.d.aly` files join them on their own.",
            ),
            key(
                "roblox_types",
                BOOL,
                "true",
                "Load the Roblox globals. The file comes from the luau-lsp extension's storage, or downloads once into `~/.alloy/types`.",
            ),
            key(
                "security_level",
                Ty::Choice(&[
                    "PluginSecurity",
                    "LocalUserSecurity",
                    "RobloxScriptSecurity",
                    "None",
                ]),
                r#""PluginSecurity""#,
                "The security level of the Roblox globals.",
            ),
            unset(
                "luau_lsp",
                STR,
                "The luau-lsp binary. Unset means `luau-lsp` on the PATH, then `~/.alloy/bin` and `~/.ember/bin`.",
            ),
            key(
                "new_solver",
                BOOL,
                "true",
                "Run the checker's new solver. `false` runs the old one, which evaluates no type function, the way an editor on the old solver does.",
            ),
            key(
                "too_many_arguments",
                INT,
                "7",
                "`too_many_arguments` fires past this many parameters; `self` does not count.",
            ),
            key(
                "too_many_lines",
                INT,
                "100",
                "`too_many_lines` fires past this many lines in one function.",
            ),
            key(
                "max_nesting",
                INT,
                "5",
                "`deep_nesting` fires past this many nested blocks.",
            ),
            key(
                "cognitive_complexity",
                INT,
                "25",
                "`cognitive_complexity` fires past this score: one per branch, loop, `and`, `or`, and ternary, plus the depth of each branch.",
            ),
        ],
        open: None,
    },
    Table {
        name: "test",
        doc: "Where `alloy test` writes the specs: one lest spec per source with a `@test`, with everything the tests reach. `alloy doc test` explains it.",
        keys: &[
            key(
                "out",
                STR,
                r#""tests""#,
                "The folder the specs land in, relative to this file. Each source with a `@test` writes `<out>/<path>.spec.luau`.",
            ),
            key("suite", STR, r#""alloy""#, "The suite name in `lest.toml`."),
            key(
                "lest",
                BOOL,
                "true",
                "Write `lest.toml` and the `@lest` alias of `.luaurc` when the root has none.",
            ),
            key(
                "shim",
                BOOL,
                "true",
                "Load engine doubles before each spec: `Vector3`, `Color3`, `Enum`, `task`, `game`, and a small Instance tree, so shared code runs on a plain VM. In Studio they do nothing.",
            ),
        ],
        open: None,
    },
    Table {
        name: "fmt",
        doc: "How Anneal, `alloy fmt`, lays code out. The names follow larvae and stylua where the option is theirs; `alloy doc fmt` explains each.",
        keys: &[
            key(
                "recommended",
                BOOL,
                "true",
                "Apply the recommended layout, the defaults below. Off, the formatter preserves what the file does: no line is reflowed, quotes and blank lines stay as written, and the indent comes from the file. The keys set here still apply over that.",
            ),
            key(
                "column_width",
                INT,
                "100",
                "The width a bracket group breaks past: a call, a table, or an array that does not fit goes one element per line.",
            ),
            key(
                "line_endings",
                Ty::Choice(&["input", "unix", "windows"]),
                r#""input""#,
                "The line ending of the written file. `input` keeps the endings the file already uses.",
            ),
            key(
                "indent_type",
                Ty::Choice(&["spaces", "tabs"]),
                r#""spaces""#,
                "What one indentation level is.",
            ),
            key(
                "indent_width",
                INT,
                "4",
                "Spaces per level, when `indent_type` is spaces.",
            ),
            key(
                "quote_style",
                Ty::Choice(&[
                    "auto-prefer-double",
                    "auto-prefer-single",
                    "force-double",
                    "force-single",
                    "preserve",
                ]),
                r#""auto-prefer-double""#,
                "The quotes of a string literal. An `auto` style keeps the other quote for a string that holds the preferred one; `force` escapes instead.",
            ),
            key(
                "leading_zero",
                Ty::Choice(&["add", "strip", "preserve"]),
                r#""add""#,
                "`.5` and `0.5`: add the zero, strip it, or leave the literal.",
            ),
            key(
                "call_parentheses",
                Ty::Choice(&[
                    "always",
                    "no-single-string",
                    "no-single-table",
                    "none",
                    "input",
                ]),
                r#""always""#,
                "The parentheses of a call with one string or one table argument: `f(\"x\")` and `f \"x\"`. `input` keeps what the author wrote.",
            ),
            key(
                "space_after_function_names",
                Ty::Choice(&["never", "definitions", "calls", "always"]),
                r#""never""#,
                "Where a space goes before the `(` of a function: `function f ()` in definitions, `f ()` in calls.",
            ),
            key(
                "collapse_simple_statement",
                Ty::Choice(&["never", "function-only", "conditional-only", "always"]),
                r#""never""#,
                "Whether `if c then return end` or a function with one statement may sit on one line.",
            ),
            key(
                "block_newline_gaps",
                Ty::Choice(&["never", "preserve"]),
                r#""never""#,
                "A blank line right after a block opener or right before its closer: dropped, or kept.",
            ),
            key(
                "magic_trailing_comma",
                BOOL,
                "true",
                "A trailing comma in the source keeps its group expanded, one element per line, whatever the width.",
            ),
            key(
                "space_inside_braces",
                BOOL,
                "true",
                "`{ a = 1 }` rather than `{a = 1}`.",
            ),
            key(
                "space_inside_parens",
                BOOL,
                "false",
                "`f( a )` rather than `f(a)`.",
            ),
            key(
                "space_inside_brackets",
                BOOL,
                "false",
                "`t[ k ]` rather than `t[k]`.",
            ),
            key(
                "trailing_comma",
                BOOL,
                "true",
                "An expanded table or array ends its last element with a comma.",
            ),
            key(
                "space_inside_array",
                BOOL,
                "true",
                "Alloy's own: `[ 1, 2 ]` rather than `[1, 2]` in an array literal.",
            ),
            key(
                "align_struct_fields",
                BOOL,
                "false",
                "Alloy's own: the `:` of a struct's fields line up.",
            ),
            key(
                "expand_imports",
                BOOL,
                "false",
                "Alloy's own: an `import { }` or `export { }` list with more than one name breaks one name per line. Off, a trailing comma in the list asks for the same.",
            ),
            key(
                "exclude",
                Ty::StrList,
                "[]",
                "Paths the formatter leaves alone. A `*` matches any run of characters: `\"vendor/*\"`, `\"*.gen.aly\"`.",
            ),
        ],
        open: None,
    },
    Table {
        name: "fmt.call_chains",
        doc: "How a chain of method calls breaks.",
        keys: &[
            key(
                "style",
                Ty::Choice(&["preserve", "method", "full"]),
                r#""preserve""#,
                "`method` breaks before each call past the first, `full` before every call, once the chain holds `min_calls` calls. `preserve` keeps the author's lines.",
            ),
            key(
                "min_calls",
                INT,
                "3",
                "The number of calls a chain needs before it breaks; 0 breaks only what runs past the width.",
            ),
        ],
        open: None,
    },
    Table {
        name: "fmt.sort_requires",
        doc: "Sorting of the `import` lines at the top of a file.",
        keys: &[
            key(
                "enabled",
                BOOL,
                "false",
                "Sort the run of `import` statements at the top of the file by path.",
            ),
            key(
                "grouping",
                Ty::Choice(&["flat", "by-kind"]),
                r#""flat""#,
                "`by-kind` orders `@alias` paths first, then absolute ones, then relative ones; `flat` sorts by path alone.",
            ),
        ],
        open: None,
    },
    Table {
        name: "fmt.alx",
        doc: "The markup of `.alx` files, after luaux-worm. The code around it formats like any `.aly` file.",
        keys: &[
            key(
                "attribute_quotes",
                Ty::Choice(&["double", "single", "preserve"]),
                r#""double""#,
                "The quotes of a string attribute: `Name=\"x\"`. `quote_style` does not govern it.",
            ),
            key(
                "bracket_same_line",
                BOOL,
                "false",
                "The `>` of a tag that broke its attributes sits on the last attribute's line rather than its own.",
            ),
            key(
                "attribute_per_line",
                BOOL,
                "true",
                "A tag that breaks its attributes puts every attribute on its own line, the way Prettier lays JSX out; `false` packs as many as fit.",
            ),
            key(
                "self_closing_space",
                BOOL,
                "true",
                "`<Frame />` rather than `<Frame/>`.",
            ),
            key(
                "text_wrap",
                Ty::Choice(&["fill", "preserve"]),
                r#""fill""#,
                "`fill` reflows text children to the column width; `preserve` keeps the author's line breaks.",
            ),
            key(
                "blank_lines",
                BOOL,
                "true",
                "A blank line between two children stays.",
            ),
        ],
        open: None,
    },
    Table {
        name: "project",
        doc: "The DataModel tree: which project file describes it, and what `alloy build` writes from it.",
        keys: &[
            key(
                "name",
                STR,
                r#""game""#,
                "The name in the project files Alloy writes. A project file at the root carries its own name, which wins.",
            ),
            unset(
                "file",
                STR,
                "The Rojo or Argon project file to read, relative to this file. Unset means `default.project.json`, then the one `*.project.json` at the root. Its tree says where each folder lands; a `[mount]` table here wins over it.",
            ),
            unset(
                "runtime",
                STR,
                "Where `alloy.luau` lands, as `@game/Service/Folder`. Emitted code requires it by an instance path. Unset means the place the project file already gives it, then the node that mounts `[build] out`, then `@game/ReplicatedStorage/Alloy`.",
            ),
            key(
                "source_of_truth",
                BOOL,
                "true",
                "The `[mount]` table is the tree: `alloy build` writes `default.project.json` and `.alloy/build.project.json` from it. Off, Alloy writes neither file, and the table only rewrites an `@alias` require into an instance path in the ship artifact, for a sync tool that owns the tree itself.",
            ),
            key(
                "mount_aliases",
                BOOL,
                "true",
                "The `[mount]` table names aliases too: the compiler and the language server serve them beside the ones `.config.luau` or `.luaurc` declares, so `@shared/x` completes and resolves. A name in the Luau configuration wins. Off, only the Luau configuration names aliases.",
            ),
            key(
                "sourcemap",
                BOOL,
                "true",
                "Write `sourcemap.json` at the root on every build, the name Rojo and luau-lsp read. The language server reads it for `@game/` completion and instance types. On, the build writes over a `sourcemap.json` another tool wrote; off, it writes none.",
            ),
        ],
        open: None,
    },
    Table {
        name: "mount",
        doc: "Where each folder lands in the DataModel: `alias = [path, mount]`. A tool that reads a Rojo or Argon project file needs no table here; Alloy reads `default.project.json`. A tool with its own format describes the tree here, and this table then wins over any project file. With it, `alloy build` also writes `default.project.json` over the sources.",
        keys: &[],
        open: Some(mount_value),
    },
    Table {
        name: "ingots",
        doc: "The extensions of the project, name to source. An ingot is an executable beside an `ingot.toml`; it edits source before the desugar, lints, formats, and answers the editor. A path is read where it is; a repo is fetched by `alloy ingot install` into `.alloy/ingots` and recorded in `.alloy/ingots.lock`. `alloy doc ingots` explains them.",
        keys: &[],
        open: Some(ingot_value),
    },
    Table {
        name: "ingot",
        doc: "One table per ingot, `[ingot.<name>]`: its options, over the defaults its manifest declares.",
        keys: &[],
        open: Some(ingot_options_value),
    },
    Table {
        name: "alx",
        doc: "How `.alx` markup lowers: the factory it calls and the names it maps. The shape `luaux.toml` has, kept in this file so a project needs no second one; a `luaux.toml` beside it still reads when this table is empty. `alloy doc markup` explains the keys.",
        keys: &[],
        open: None,
    },
    Table {
        name: "alx.factory",
        doc: "The functions the lowered markup calls. With no key set, the markup picks a UI library on its own; with any key set, it assumes nothing.",
        keys: &[
            unset(
                "backend",
                Ty::Choice(&["table", "element"]),
                "The arrangement of a lowered element: `table` passes children in the props table, `element` as a third argument.",
            ),
            unset(
                "create",
                STR,
                "The expression that constructs an element, called with the class name.",
            ),
            unset(
                "children",
                STR,
                "The expression that attaches children, when the backend takes one.",
            ),
            unset(
                "event",
                STR,
                "The expression that connects an event handler.",
            ),
            unset(
                "compute",
                STR,
                "The expression that wraps a computed value.",
            ),
            unset("use", STR, "The expression that reads a state value."),
            unset(
                "fragment",
                STR,
                "The expression that groups children without a parent.",
            ),
            unset(
                "interpolate",
                Ty::Choice(&["plain", "compute"]),
                "How `{expr}` in text lowers: `plain` inserts the value, `compute` wraps it.",
            ),
            unset(
                "merge",
                STR,
                "The expression that merges a spread props table.",
            ),
        ],
        open: None,
    },
    Table {
        name: "alx.elements",
        doc: "Element name aliases: `Frame = \"frame\"` renames one class in markup, and `all = \"camel\"` renames every class by a scheme: PascalCase, camelCase, snake_case, or flatcase.",
        keys: &[],
        open: Some(alias_value),
    },
    Table {
        name: "alx.properties",
        doc: "Property and event aliases: `Text = \"text\"` for every class, `all = \"camel\"` for a scheme, or a class table with its own.",
        keys: &[],
        open: Some(property_alias_value),
    },
    Table {
        name: "alx.lints",
        doc: "Deprecated: the levels of the markup lints. Write `[lint.rules] alx.<name> = \"allow\" | \"warn\" | \"deny\"`.",
        keys: &[unset(
            "static_conditional_child",
            Ty::Choice(&["off", "warn", "error"]),
            "Deprecated: write `[lint.rules] alx.static_conditional_child`. Markup in a child expression that no function encloses: it is built once, not on each render.",
        )],
        open: None,
    },
];

fn alias_value() -> Value {
    json!({
        "type": "string",
        "description": "The name markup uses for this class, or a casing scheme for `all`."
    })
}

fn property_alias_value() -> Value {
    json!({
        "description": "The name markup uses for this property in every class, or a table of aliases for one class.",
        "oneOf": [
            { "type": "string" },
            { "type": "object", "additionalProperties": { "type": "string" } }
        ]
    })
}

/// The schema of one key.
fn key_schema(k: &Key) -> Value {
    let mut s = Map::new();

    match k.ty {
        Ty::Bool => {
            s.insert("type".into(), json!("boolean"));
        }

        Ty::Int => {
            s.insert("type".into(), json!("integer"));
            s.insert("minimum".into(), json!(0));
        }

        Ty::Number => {
            s.insert("type".into(), json!("number"));
            s.insert("minimum".into(), json!(0));
        }

        Ty::Str => {
            s.insert("type".into(), json!("string"));
        }

        Ty::Choice(values) => {
            s.insert("type".into(), json!("string"));
            s.insert("enum".into(), json!(values));
        }

        Ty::StrList => {
            s.insert("type".into(), json!("array"));

            // `anyOf` offers the names in completion and still accepts any
            // string: a lint that is newer than the schema must not error.
            let items = match k.suggest {
                Some(names) => json!({
                    "anyOf": [
                        { "type": "string", "enum": names() },
                        { "type": "string" }
                    ]
                }),

                None => json!({ "type": "string" }),
            };

            s.insert("items".into(), items);
        }
    }

    s.insert("description".into(), json!(k.doc));

    if let Some(d) = k.default {
        let value: Value = serde_json::from_str(d).expect("a default is JSON text");
        s.insert("default".into(), value);
    }

    Value::Object(s)
}

/// The schema of one table, with its own keys only; nested tables are
/// added by `schema`.
fn table_schema(t: &Table) -> Value {
    let mut properties = Map::new();

    for k in t.keys {
        properties.insert(k.name.to_string(), key_schema(k));
    }

    let mut s = Map::new();
    s.insert("type".into(), json!("object"));
    s.insert("title".into(), json!(format!("[{}]", t.name)));
    s.insert("description".into(), json!(t.doc));
    s.insert("properties".into(), Value::Object(properties));

    match t.open {
        Some(value) => {
            s.insert("additionalProperties".into(), value());
        }

        None => {
            s.insert("additionalProperties".into(), json!(false));
        }
    }

    Value::Object(s)
}

/// The whole schema, draft-07.
pub fn schema() -> Value {
    let mut root = Map::new();
    root.insert(
        "$schema".into(),
        json!("http://json-schema.org/draft-07/schema#"),
    );
    root.insert("$id".into(), json!(URL));
    root.insert("title".into(), json!("alloy.toml"));
    root.insert(
        "description".into(),
        json!(
            "The project file of Alloy. Every key has a default, and an unknown key is an error."
        ),
    );
    root.insert("type".into(), json!("object"));
    root.insert("additionalProperties".into(), json!(false));
    root.insert("properties".into(), Value::Object(Map::new()));

    // A parent table is listed before its children, so `fmt.alx` finds
    // `fmt` in place.
    for t in TABLES {
        let mut node = root.get_mut("properties").expect("the root has properties");
        let mut parts = t.name.split('.').peekable();

        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                node.as_object_mut()
                    .expect("properties is an object")
                    .insert(part.to_string(), table_schema(t));
            } else {
                node = node
                    .get_mut(part)
                    .and_then(|v| v.get_mut("properties"))
                    .unwrap_or_else(|| panic!("the table {} has no parent {part}", t.name));
            }
        }
    }

    let mut root = Value::Object(root);

    if let Some(rules) = root.pointer_mut("/properties/lint/properties/rules") {
        rules["propertyNames"] = rule_key_schema(&[]);
    }

    root
}

/// The schema of one project: the general one, plus what its ingots
/// declare. `[ingot.<name>]` gets the options of the manifest, each
/// with its default and its doc, and the `[lint]` lists complete the
/// ingots' lint names and groups. The build writes it to
/// `.alloy/alloy.schema.json`, and a `#:schema` line at the top of
/// alloy.toml points the editor at it.
pub fn project(manifests: &[&crate::ingot::Manifest]) -> Value {
    let mut root = schema();
    let mut names: Vec<String> = Vec::new();

    for m in manifests {
        names.push(m.name.clone());
        names.extend(m.lints.keys().map(|l| format!("{}/{l}", m.name)));

        let mut props = Map::new();

        for (key, default) in &m.options {
            let mut s = Map::new();
            let ty = match default {
                toml::Value::String(_) => "string",
                toml::Value::Integer(_) => "integer",
                toml::Value::Float(_) => "number",
                toml::Value::Boolean(_) => "boolean",
                toml::Value::Array(_) => "array",
                _ => "object",
            };
            s.insert("type".into(), json!(ty));
            s.insert("default".into(), toml_to_json(default));

            if let Some(doc) = m.option_docs.get(key) {
                s.insert("description".into(), json!(doc));
            }

            props.insert(key.clone(), Value::Object(s));
        }

        let table = json!({
            "type": "object",
            "title": format!("[ingot.{}]", m.name),
            "description": if m.description.is_empty() {
                format!("The options of the `{}` ingot.", m.name)
            } else {
                m.description.clone()
            },
            "additionalProperties": false,
            "properties": Value::Object(props),
        });

        if let Some(ingot) = root.pointer_mut("/properties/ingot/properties") {
            ingot[m.name.as_str()] = table;
        }

        if let Some(ingots) = root.pointer_mut("/properties/ingots/properties") {
            ingots[m.name.as_str()] = ingot_value();
        }
    }

    if names.is_empty() {
        return root;
    }

    for list in ["deny", "warn", "allow"] {
        if let Some(e) = root.pointer_mut(&format!(
            "/properties/lint/properties/{list}/items/anyOf/0/enum"
        )) && let Some(all) = e.as_array_mut()
        {
            all.extend(names.iter().map(|n| json!(n)));
        }
    }

    if let Some(rules) = root.pointer_mut("/properties/lint/properties/rules") {
        rules["propertyNames"] = rule_key_schema(&names);
    }

    root
}

fn toml_to_json(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The schema as pretty JSON with a final newline, as `alloy self schema`
/// prints it.
pub fn to_string() -> String {
    serde_json::to_string_pretty(&schema()).expect("the schema is plain JSON") + "\n"
}

/// Every key path of the schema, `build.in`, `fmt.alx.text_wrap`. A table
/// with open keys, `mount`, is one path.
pub fn key_paths() -> Vec<String> {
    let mut out = Vec::new();

    for t in TABLES {
        if t.open.is_some() {
            out.push(t.name.to_string());
        }

        for k in t.keys {
            out.push(format!("{}.{}", t.name, k.name));
        }
    }

    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TEMPLATE};
    use std::collections::BTreeSet;

    /// Every leaf path of a TOML value. A table named in `open` counts as
    /// one leaf, whatever it holds.
    fn toml_paths(value: &toml::Value, prefix: &str, open: &[&str], out: &mut BTreeSet<String>) {
        if let toml::Value::Table(t) = value {
            if !prefix.is_empty() && open.contains(&prefix) {
                out.insert(prefix.to_string());
                return;
            }

            for (k, v) in t {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };

                toml_paths(v, &path, open, out);
            }
        } else {
            out.insert(prefix.to_string());
        }
    }

    fn open_tables() -> Vec<&'static str> {
        TABLES
            .iter()
            .filter(|t| t.open.is_some())
            .map(|t| t.name)
            .collect()
    }

    #[test]
    fn the_schema_and_the_config_name_the_same_keys() {
        // The optional keys are absent from a serialized default, so the
        // test sets each one.
        let mut config = Config::default();
        config.emit.wait_timeout = Some(5.0);
        config.emit.std_require = Some("@alloy".into());
        config.flux.luau_lsp = Some("luau-lsp".into());
        config.project.file = Some("default.project.json".into());
        config.project.runtime = Some("@game/ReplicatedStorage/Alloy".into());
        config.alx.factory = crate::config::AlxFactory {
            backend: Some("table".into()),
            create: Some("create".into()),
            children: Some("children".into()),
            event: Some("event".into()),
            compute: Some("compute".into()),
            use_fn: Some("use".into()),
            fragment: Some("fragment".into()),
            interpolate: Some("plain".into()),
            merge: Some("merge".into()),
        };
        config.alx.lints.static_conditional_child = Some("warn".into());

        let value = toml::Value::try_from(&config).unwrap();
        let mut from_config = BTreeSet::new();
        toml_paths(&value, "", &open_tables(), &mut from_config);

        let from_schema: BTreeSet<String> = key_paths().into_iter().collect();

        let missing: Vec<_> = from_config.difference(&from_schema).collect();
        assert!(
            missing.is_empty(),
            "keys of Config without a schema entry: {missing:?}"
        );

        let extra: Vec<_> = from_schema.difference(&from_config).collect();
        assert!(
            extra.is_empty(),
            "schema keys that Config rejects: {extra:?}"
        );
    }

    #[test]
    fn a_project_schema_carries_its_ingots() {
        let m = crate::ingot::Manifest::parse(
            "name = \"enamel\"\napi = 1\nhooks = [\"lint\"]\n[options]\nhelper = { default = \"__enamel\", doc = \"The helper's name.\" }\nsort = true\n[lints.no_effect]\ndefault = \"warn\"\nsummary = \"nothing\"\n",
        )
        .unwrap();
        let s = project(&[&m]);
        assert_eq!(
            s["properties"]["ingot"]["properties"]["enamel"]["properties"]["helper"]["default"],
            json!("__enamel")
        );
        assert_eq!(
            s["properties"]["ingot"]["properties"]["enamel"]["properties"]["helper"]["description"],
            json!("The helper's name.")
        );
        assert_eq!(
            s["properties"]["ingot"]["properties"]["enamel"]["properties"]["sort"]["type"],
            json!("boolean")
        );
        let names = s["properties"]["lint"]["properties"]["deny"]["items"]["anyOf"][0]["enum"]
            .as_array()
            .unwrap();
        assert!(names.contains(&json!("enamel/no_effect")) && names.contains(&json!("enamel")));
    }

    #[test]
    fn the_template_uses_schema_keys_only() {
        let from_schema: BTreeSet<String> = key_paths().into_iter().collect();

        let value: toml::Value = toml::from_str(TEMPLATE).unwrap();
        let mut used = BTreeSet::new();
        toml_paths(&value, "", &open_tables(), &mut used);

        // The commented lines, `# key = value` and `# [table]`, are the
        // keys the template suggests; they must be real too.
        let mut table = String::new();

        for line in TEMPLATE.lines() {
            let line = line.trim();

            if let Some(name) = line
                .strip_prefix('[')
                .or_else(|| line.strip_prefix("# ["))
                .and_then(|l| l.strip_suffix(']'))
            {
                table = name.to_string();
                continue;
            }

            if let Some(rest) = line.strip_prefix("# ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                    .collect();

                if !name.is_empty() && rest[name.len()..].starts_with(" = ") {
                    used.insert(format!("{table}.{name}"));
                }
            }
        }

        // An inline table, `sort_requires = { ... }`, names a nested
        // table, and any key sits under an open table.
        let open = open_tables();
        let unknown: Vec<_> = used
            .iter()
            .filter(|p| !from_schema.contains(*p))
            .filter(|p| !TABLES.iter().any(|t| t.name == p.as_str()))
            .filter(|p| !open.iter().any(|o| p.starts_with(&format!("{o}."))))
            .collect();
        assert!(
            unknown.is_empty(),
            "template keys the schema lacks: {unknown:?}"
        );
    }

    #[test]
    fn nested_tables_sit_under_their_parent() {
        let s = schema();
        let alx = &s["properties"]["fmt"]["properties"]["alx"];
        assert_eq!(alx["type"], "object");
        assert_eq!(
            alx["properties"]["text_wrap"]["enum"],
            json!(["fill", "preserve"])
        );
        assert_eq!(
            s["properties"]["build"]["properties"]["in"]["default"],
            "src"
        );
        assert_eq!(s["properties"]["build"]["additionalProperties"], false);
        assert_eq!(
            s["properties"]["mount"]["additionalProperties"]["type"],
            "array"
        );
    }

    #[test]
    fn a_lint_list_offers_every_group_and_lint() {
        let s = schema();
        let names = &s["properties"]["lint"]["properties"]["deny"]["items"]["anyOf"][0]["enum"];
        let names: Vec<&str> = names
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        for g in Group::ALL {
            assert!(names.contains(&g.name()), "{} is missing", g.name());
        }

        assert!(names.contains(&"luau"));

        for l in LINTS {
            assert!(names.contains(&l.name), "{} is missing", l.name);
        }
    }

    #[test]
    fn the_schema_prints_as_json() {
        let text = to_string();
        assert!(text.ends_with('\n'));
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["$schema"], "http://json-schema.org/draft-07/schema#");
    }
}
