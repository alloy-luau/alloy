//! `alloy.toml`: the project file.
//!
//! The shape follows `luaux.toml`, so a project that has one reads the other
//! without surprise. Every key has a default, and a missing file means the
//! defaults, so `alloy build` works in a bare folder with a `src`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The whole file. Unknown tables and keys are errors, so a typo in a key
/// never passes as a default.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub build: Build,
    pub emit: Emit,
    pub lint: LintConfig,
    pub fmt: FmtConfig,
    pub flux: FluxConfig,
    pub test: TestConfig,
    pub project: Project,
    /// The `[mount]` table: alias to `[path, mount]`. The folder at
    /// `path` lands at `mount` in the DataModel, and `@alias/...`
    /// requires it. See `crate::project`.
    pub mount: BTreeMap<String, Mount>,
}

/// One mount: the path on disk, relative to the root, and the DataModel
/// location, `@game/Service/Folder`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Mount(pub String, pub String);

/// The `[project]` table: the Rojo project the mounts describe.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct Project {
    /// The name in the generated project files.
    pub name: String,
    /// Where `alloy.luau` mounts. Emitted code requires it by a
    /// relative instance path from each file's mount.
    pub runtime: String,
    /// Write `.alloy/sourcemap.json` on every build.
    pub sourcemap: bool,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "game".to_string(),
            runtime: "@game/ReplicatedStorage/Alloy".to_string(),
            sourcemap: true,
        }
    }
}

/// The `[fmt]` table: how Anneal, the formatter behind `alloy fmt`,
/// lays code out. The names follow larvae and stylua where the option is
/// theirs, so a config ports over; the Alloy-only options sit last.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct FmtConfig {
    /// The width a bracket group breaks past.
    pub column_width: usize,
    pub line_endings: LineEndings,
    pub indent_type: IndentType,
    /// Spaces per level, when `indent_type` is spaces.
    pub indent_width: usize,
    pub quote_style: QuoteStyle,
    /// `.5` and `0.5`: add the zero, strip it, or leave the literal.
    pub leading_zero: LeadingZero,
    /// The parentheses of a call with one string or one table argument.
    pub call_parentheses: CallParentheses,
    /// `function f ()` and `function ()`: where a space goes.
    pub space_after_function_names: FunctionNameSpace,
    /// Whether `if c then return end` may sit on one line.
    pub collapse_simple_statement: Collapse,
    /// A blank line right after a block opener or before its closer.
    pub block_newline_gaps: BlockGaps,
    /// A trailing comma in the source keeps its group expanded.
    pub magic_trailing_comma: bool,
    /// `{ a }` rather than `{a}`.
    pub space_inside_braces: bool,
    /// `f( a )` rather than `f(a)`.
    pub space_inside_parens: bool,
    /// `t[ k ]` rather than `t[k]`.
    pub space_inside_brackets: bool,
    /// An expanded group ends its last element with a comma.
    pub trailing_comma: bool,
    pub call_chains: CallChains,
    pub sort_requires: SortRequires,
    /// Alloy's own: `[ 1, 2 ]` rather than `[1, 2]` in an array literal.
    pub space_inside_array: bool,
    /// Alloy's own: the `:` of a struct's fields line up.
    pub align_struct_fields: bool,
    /// Alloy's own: an `import { }` or `export { }` list with more than
    /// one name breaks one name per line, whatever its width.
    pub expand_imports: bool,
    /// Paths the formatter leaves alone. A `*` matches any run of
    /// characters: `"vendor/*"`, `"*.gen.aly"`.
    pub exclude: Vec<String>,
    /// The markup of `.alx` files.
    pub alx: AlxFmt,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self {
            column_width: 100,
            line_endings: LineEndings::Unix,
            indent_type: IndentType::Spaces,
            indent_width: 4,
            quote_style: QuoteStyle::AutoPreferDouble,
            leading_zero: LeadingZero::Add,
            call_parentheses: CallParentheses::Always,
            space_after_function_names: FunctionNameSpace::Never,
            collapse_simple_statement: Collapse::Never,
            block_newline_gaps: BlockGaps::Never,
            magic_trailing_comma: true,
            space_inside_braces: true,
            space_inside_parens: false,
            space_inside_brackets: false,
            trailing_comma: true,
            call_chains: CallChains::default(),
            sort_requires: SortRequires::default(),
            space_inside_array: true,
            align_struct_fields: false,
            expand_imports: false,
            exclude: Vec::new(),
            alx: AlxFmt::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LineEndings {
    Unix,
    Windows,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndentType {
    Spaces,
    Tabs,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteStyle {
    AutoPreferDouble,
    AutoPreferSingle,
    ForceDouble,
    ForceSingle,
    Preserve,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LeadingZero {
    Add,
    Strip,
    Preserve,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CallParentheses {
    Always,
    NoSingleString,
    NoSingleTable,
    None,
    Input,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FunctionNameSpace {
    Never,
    Definitions,
    Calls,
    Always,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Collapse {
    Never,
    FunctionOnly,
    ConditionalOnly,
    Always,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BlockGaps {
    Never,
    Preserve,
}

/// How a chain of method calls lays out.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct CallChains {
    pub style: CallChainStyle,
    /// A chain with this many calls breaks even when it fits; 0 breaks
    /// only what runs past the width.
    pub min_calls: usize,
}

impl Default for CallChains {
    fn default() -> Self {
        Self {
            style: CallChainStyle::Preserve,
            min_calls: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CallChainStyle {
    Preserve,
    Method,
    Full,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields, default)]
pub struct SortRequires {
    pub enabled: bool,
    pub grouping: RequireGrouping,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RequireGrouping {
    #[default]
    Flat,
    ByKind,
}

/// The `[fmt.alx]` table: the markup of `.alx` files, after luaux-worm.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct AlxFmt {
    /// The quotes of an attribute's string; `quote_style` does not govern it.
    pub attribute_quotes: AttributeQuotes,
    /// The `>` of a tag that breaks goes after the last attribute.
    pub bracket_same_line: bool,
    /// One attribute per line at all times.
    pub attribute_per_line: bool,
    /// The space in `<Frame />`.
    pub self_closing_space: bool,
    /// Fill each text line, or break where the author broke.
    pub text_wrap: TextWrap,
    /// A blank line between two children stays.
    pub blank_lines: bool,
}

impl Default for AlxFmt {
    fn default() -> Self {
        Self {
            attribute_quotes: AttributeQuotes::Double,
            bracket_same_line: false,
            attribute_per_line: false,
            self_closing_space: true,
            text_wrap: TextWrap::Fill,
            blank_lines: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AttributeQuotes {
    Double,
    Single,
    Preserve,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TextWrap {
    Fill,
    Preserve,
}

/// The `[lint]` table: the level of each lint. A list takes a lint name
/// or a group name, `pedantic`; a name beats its group.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LintConfig {
    /// Turns the `pedantic` group on, at `warn`.
    pub strict: bool,
    /// Lints that fail the run.
    pub deny: Vec<String>,
    /// Lints that print and pass.
    pub warn: Vec<String>,
    /// Lints that stay silent.
    pub allow: Vec<String>,
}

/// The `[flux]` table: what `alloy flux` runs beyond the lints, and the
/// thresholds of the complexity lints. The levels of the lints stay in
/// `[lint]`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct FluxConfig {
    /// Run luau-lsp over the check artifact and report its type errors
    /// on the source lines.
    pub typecheck: bool,
    /// Definitions files for the type check, `.d.luau` or `.d.aly`,
    /// relative to the root. The `.d.aly` files of the project join
    /// them on their own.
    pub definitions: Vec<String>,
    /// Load the Roblox globals. The file comes from the luau-lsp
    /// extension's storage, or downloads once into `~/.alloy/types`.
    pub roblox_types: bool,
    /// The security level of the Roblox globals: `PluginSecurity`,
    /// `LocalUserSecurity`, `RobloxScriptSecurity`, or `None`.
    pub security_level: String,
    /// The luau-lsp binary. Unset means `luau-lsp` on the PATH, then
    /// `~/.alloy/bin` and `~/.ember/bin`.
    pub luau_lsp: Option<String>,
    /// `too_many_arguments` fires past this many parameters.
    pub too_many_arguments: usize,
    /// `too_many_lines` fires past this many lines in one function.
    pub too_many_lines: usize,
    /// `deep_nesting` fires past this many nested blocks.
    pub max_nesting: usize,
    /// `cognitive_complexity` fires past this score.
    pub cognitive_complexity: usize,
}

impl Default for FluxConfig {
    fn default() -> Self {
        let t = crate::lint::Thresholds::default();

        Self {
            typecheck: true,
            definitions: Vec::new(),
            roblox_types: true,
            security_level: "PluginSecurity".to_string(),
            luau_lsp: None,
            too_many_arguments: t.too_many_arguments,
            too_many_lines: t.too_many_lines,
            max_nesting: t.max_nesting,
            cognitive_complexity: t.cognitive_complexity,
        }
    }
}

impl FluxConfig {
    /// The thresholds the complexity lints read.
    pub fn thresholds(&self) -> crate::lint::Thresholds {
        crate::lint::Thresholds {
            too_many_arguments: self.too_many_arguments,
            too_many_lines: self.too_many_lines,
            max_nesting: self.max_nesting,
            cognitive_complexity: self.cognitive_complexity,
        }
    }
}

/// The `[test]` table: where `alloy test` writes the specs.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct TestConfig {
    /// The folder the specs land in, relative to the root. Each source
    /// with a `@test` writes `<out>/<path>.spec.luau`.
    pub out: PathBuf,
    /// The suite name in `lest.toml`.
    pub suite: String,
    /// Write `lest.toml` and the `@lest` alias when the root has none.
    pub lest: bool,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            out: PathBuf::from("tests"),
            suite: "alloy".to_string(),
            lest: true,
        }
    }
}

/// The `[emit]` table: the few knobs that change what emitted code does.
/// Each one is a named exception to the razor, so the list stays short.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Emit {
    /// Seconds passed to every `WaitForChild` that `=>` emits. Unset means
    /// no timeout: the engine waits forever and warns after five seconds.
    /// With a timeout the call can return nil, so `=>` guards like `->`.
    pub wait_timeout: Option<f64>,
    /// The string emitted code passes to `require` for the runtime. The
    /// default is the `@alloy` alias; the build writes the runtime next to
    /// the output as `alloy.luau`, so a `.luaurc` alias can point at it.
    pub std_require: Option<String>,
    /// Blank `import type` lines in the output so they add no runtime
    /// dependency. Off by default: the output is then untyped for anyone
    /// who analyzes it directly.
    pub erase_type_imports: bool,
}

/// The `[build]` table.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct Build {
    /// The source root. Every `.aly` under it compiles.
    #[serde(rename = "in")]
    pub input: PathBuf,
    /// The output root. The tree under `in` is mirrored under it.
    pub out: PathBuf,
    /// Glob patterns, relative to `in`, of sources to skip.
    pub exclude: Vec<String>,
    /// Delete an output whose source is gone.
    pub clean: bool,
    /// Which artifact to write: `ship` runs on Roblox, `check` is what
    /// luau-lsp sees.
    pub artifact: Artifact,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Artifact {
    Ship,
    Check,
}

impl Default for Build {
    fn default() -> Self {
        Self {
            input: PathBuf::from("src"),
            out: PathBuf::from("build"),
            exclude: Vec::new(),
            clean: false,
            artifact: Artifact::Ship,
        }
    }
}

/// The file name the CLI looks for.
pub const FILE_NAME: &str = "alloy.toml";

/// A default file, written by `alloy init`.
pub const TEMPLATE: &str = r#"[build]
in = "src"
out = "build"
exclude = []
clean = false
# ship runs on Roblox; check is what luau-lsp sees
artifact = "ship"

[emit]
# seconds for every WaitForChild that `=>` emits; unset waits forever
# wait_timeout = 5
# what emitted code requires for the runtime; unset means a relative
# require of the alloy.luau the build writes into the output root
# std_require = "@alloy"
# blank `import type` lines in the output so they add no dependency
# erase_type_imports = false

[fmt]
# Anneal, the formatter. Every key has a default; these are the ones a
# project changes most. `alloy doc fmt` lists them all.
column_width = 100
indent_type = "spaces"
indent_width = 4
quote_style = "auto-prefer-double"
# call_parentheses = "always"
# exclude = ["vendor/*"]
# sort_requires = { enabled = true, grouping = "by-kind" }
# [fmt.alx]
# attribute_per_line = false

[lint]
# the levels of the lints; a list takes a lint or a group: correctness,
# suspicious, style, complexity, perf, roblox, pedantic, luau
# turn the pedantic group on
strict = false
# lints that fail the run; `alloy doc lints` names them all
deny = []
warn = []
allow = []

[flux]
# `alloy flux`: the type check and the thresholds; `alloy doc flux`
# run luau-lsp over the check artifact
typecheck = true
# extra definitions files; the project's .d.aly files join on their own
definitions = []
# too_many_arguments = 7
# too_many_lines = 100
# max_nesting = 5
# cognitive_complexity = 25

[test]
# `alloy test` writes one lest spec per source with a @test
out = "tests"
suite = "alloy"
# write lest.toml and the @lest alias when the root has none
lest = true

[project]
# the name in default.project.json and .alloy/build.project.json
name = "game"
# where build/alloy.luau mounts; emitted code requires it from there
runtime = "@game/ReplicatedStorage/Alloy"
# write .alloy/sourcemap.json on every build
sourcemap = true

[mount]
# alias = [path, mount]: the folder at path lands at mount in the
# DataModel, and require("@alias/x") resolves through it. With one or
# more mounts, `alloy build` writes default.project.json over the
# sources and .alloy/build.project.json over the output.
# server = ["src/server", "@game/ServerScriptService/Server"]
# client = ["src/client", "@game/StarterPlayer/StarterPlayerScripts/Client"]
# shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
# pkg = ["Packages", "@game/ReplicatedStorage/Packages"]
"#;

/// The `.luaurc` that `alloy init` writes: strict mode for every file,
/// and the `@alloy` alias for the runtime the build writes.
pub const LUAURC_TEMPLATE: &str = r#"{
  "languageMode": "strict",
  "aliases": {
    "alloy": "./build/alloy"
  }
}
"#;

/// The same configuration as `.config.luau`, the Luau-syntax form.
pub const CONFIG_LUAU_TEMPLATE: &str = r#"return {
    luau = {
        languagemode = "strict",
        aliases = {
            alloy = "./build/alloy",
        },
    },
}
"#;

#[derive(Debug)]
pub enum ConfigError {
    Read(PathBuf, std::io::Error),
    Parse(PathBuf, toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(p, e) => write!(f, "cannot read {}: {e}", p.display()),

            ConfigError::Parse(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}

impl Config {
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Read(path.to_path_buf(), e))?;

        Self::parse(&text, path)
    }

    /// Finds `alloy.toml` in `start` or the nearest ancestor. The project
    /// root is the directory that holds it, and every path in the file is
    /// relative to that root.
    pub fn find(start: &Path) -> Option<PathBuf> {
        let mut dir = Some(start);

        while let Some(d) = dir {
            let candidate = d.join(FILE_NAME);

            if candidate.is_file() {
                return Some(candidate);
            }

            dir = d.parent();
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_key_takes_its_default() {
        let c = Config::parse("[build]\nout = \"dist\"\n", Path::new("alloy.toml")).unwrap();
        assert_eq!(c.build.input, PathBuf::from("src"));
        assert_eq!(c.build.out, PathBuf::from("dist"));
        assert_eq!(c.build.artifact, Artifact::Ship);
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        assert!(Config::parse("[build]\noutput = \"dist\"\n", Path::new("alloy.toml")).is_err());
    }

    #[test]
    fn a_mount_is_an_alias_to_a_path_and_a_place() {
        let c = Config::parse(
            "[mount]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n",
            Path::new("alloy.toml"),
        )
        .unwrap();
        assert_eq!(
            c.mount["pkg"],
            Mount("Packages".into(), "@game/ReplicatedStorage/Packages".into())
        );
        assert_eq!(c.project.runtime, "@game/ReplicatedStorage/Alloy");
    }

    #[test]
    fn the_template_parses_to_the_defaults() {
        let c = Config::parse(TEMPLATE, Path::new("alloy.toml")).unwrap();
        assert_eq!(c, Config::default());
    }
}
