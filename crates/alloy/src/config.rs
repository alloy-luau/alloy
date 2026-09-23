//! `alloy.toml`: the project file.
//!
//! The shape follows `luaux.toml`, so a project that has one reads the other
//! without surprise. Every key has a default, and a missing file means the
//! defaults, so `alloy build` works in a bare folder with a `src`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The whole file. Unknown tables and keys are errors, so a typo in a key
/// never passes as a default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub build: Build,
    pub emit: Emit,
    pub lint: LintConfig,
    pub fmt: FmtConfig,
    pub flux: FluxConfig,
    pub roblox: RobloxConfig,
    pub test: TestConfig,
    pub project: Project,
    /// The `[mount]` table: alias to `[path, mount]`. The folder at
    /// `path` lands at `mount` in the DataModel. The table is the tree
    /// when the project writes one, over any project file at the root.
    /// See `crate::project`.
    pub mount: BTreeMap<String, Mount>,
    /// The `[ingots]` table: name to source. An ingot is an extension
    /// that ships as an executable; see `crate::ingot`.
    pub ingots: BTreeMap<String, IngotSource>,
    /// The `[ingot.<name>]` tables: the options of one ingot, over the
    /// defaults its manifest declares. The compiler passes them through
    /// and never reads them.
    pub ingot: BTreeMap<String, toml::Table>,
    /// The `[alx]` table: how markup lowers, the factory it calls and
    /// the names it maps. A project with `.alx` files sets it here; a
    /// `luaux.toml` beside the file still reads when the table is empty.
    pub alx: Alx,
}

/// The `[alx]` table: the markup settings, the shape `luaux.toml` has,
/// so a project needs no second file for them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Alx {
    pub factory: AlxFactory,
    /// Element name aliases, `Frame = "frame"`, or `all = "camel"`.
    pub elements: BTreeMap<String, toml::Value>,
    /// Property aliases, `Text = "text"`, per class as a table.
    pub properties: BTreeMap<String, toml::Value>,
    pub lints: AlxLints,
}

/// `[alx.factory]`: the functions the lowered markup calls.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct AlxFactory {
    pub backend: Option<String>,
    pub create: Option<String>,
    pub children: Option<String>,
    pub event: Option<String>,
    pub compute: Option<String>,
    #[serde(rename = "use")]
    pub use_fn: Option<String>,
    pub fragment: Option<String>,
    pub interpolate: Option<String>,
    pub merge: Option<String>,
}

/// A markup lint's level as the markup compiler spells it.
pub(crate) fn markup_level(level: crate::lint::Level) -> luaux::config::LintLevel {
    match level {
        crate::lint::Level::Allow => luaux::config::LintLevel::Off,
        crate::lint::Level::Warn => luaux::config::LintLevel::Warn,
        crate::lint::Level::Deny => luaux::config::LintLevel::Error,
    }
}

/// `[alx.lints]`: the levels of the markup lints. Deprecated; write
/// `[lint.rules] alx.<name> = "<level>"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct AlxLints {
    pub static_conditional_child: Option<String>,
}

impl Alx {
    /// Whether the project wrote anything under `[alx]`.
    pub fn is_set(&self) -> bool {
        *self != Self::default()
    }

    /// The table as the markup compiler's own config.
    pub fn to_markup(&self) -> Result<luaux::Config, String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;

        luaux::Config::parse(&text)
            .map_err(|e| format!("[alx]: {}", e.message.trim_start_matches("luaux.toml: ")))
    }
}

/// Where an ingot comes from: a directory that holds `ingot.toml` and
/// the binary, or a GitHub release pinned by version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum IngotSource {
    /// `name = "ingots/tailwind"`: a path relative to the root.
    Path(String),
    Table(IngotTable),
}

/// The expanded form of an ingot source.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct IngotTable {
    /// A directory relative to the root.
    pub path: Option<String>,
    /// `owner/repo` on GitHub; the release `v<version>` holds the zip.
    pub repo: Option<String>,
    /// The release to install. `^`, the default, is the latest release
    /// at install time, and `.alloy/ingots.lock` records what it
    /// resolved to. A version, `1.2.3` or `v1.2.3`, pins that release:
    /// `alloy ingot update` leaves it where it is.
    pub version: Option<String>,
    /// The asset name in the release. Unset means
    /// `<name>-ingot-<target>.zip`, then `<name>-ingot.zip`.
    pub asset: Option<String>,
    /// The pass the ingot's transform runs in, over the manifest's word.
    /// A lower number runs first; the compiler's own desugar sits after
    /// every pass.
    pub order: Option<i64>,
    /// Lints of this ingot switched on or off by name, over the
    /// manifest's defaults and under the `[lint]` table.
    pub lints: BTreeMap<String, bool>,
}

impl IngotSource {
    pub fn table(&self) -> IngotTable {
        match self {
            IngotSource::Path(p) => IngotTable {
                path: Some(p.clone()),
                ..IngotTable::default()
            },

            IngotSource::Table(t) => t.clone(),
        }
    }
}

/// One mount: the path on disk, relative to the root, and the DataModel
/// location, `@game/Service/Folder`. A bare string in place of the pair
/// is the alias-only form: the path takes the alias and no location, so
/// the folder has to sit under another mount already.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Mount(pub String, pub String);

impl Mount {
    /// Whether the entry names a path alone: an alias with no DataModel
    /// location, for a folder another mount already carries.
    pub fn alias_only(&self) -> bool {
        self.1.is_empty()
    }
}

impl<'de> Deserialize<'de> for Mount {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Placed(String, String),
            AliasOnly(String),
        }

        Ok(match Raw::deserialize(d)? {
            Raw::Placed(path, place) => Mount(path, place),

            Raw::AliasOnly(path) => Mount(path, String::new()),
        })
    }
}

/// Where `alloy.luau` lands when neither `[project] runtime` nor the
/// project file says.
pub const DEFAULT_RUNTIME: &str = "@game/ReplicatedStorage/Alloy";

/// The `[project]` table: the DataModel tree and what Alloy writes from
/// it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct Project {
    /// The name in the project files Alloy writes. A project file at
    /// the root carries its own name, which wins.
    pub name: String,
    /// The Rojo project file to read, relative to the root. Unset means
    /// `default.project.json`, then the one `*.project.json` at the
    /// root.
    pub file: Option<String>,
    /// Where `alloy.luau` lands. Unset means the place the project file
    /// already gives it, then the node that mounts `[build] out`, then
    /// `@game/ReplicatedStorage/Alloy`.
    pub runtime: Option<String>,
    /// The `[mount]` table is the tree, and `alloy build` writes
    /// `default.project.json` and `.alloy/build.project.json` from it.
    /// Off, Alloy writes neither file: the table then only rewrites an
    /// `@alias` require into an instance path in the ship artifact, and
    /// the sync tool of the project owns the tree.
    pub source_of_truth: bool,
    /// The `[mount]` table names aliases too: the compiler and the
    /// language server serve them beside the ones `.config.luau` or
    /// `.luaurc` declares. A name in the Luau configuration wins. Off,
    /// only the Luau configuration names aliases.
    pub mount_aliases: bool,
    /// Write `sourcemap.json` at the root on every build, over a
    /// file another tool wrote there.
    pub sourcemap: bool,
}

impl Project {
    /// The runtime's place: what the project set, else the default.
    pub fn runtime(&self) -> &str {
        self.runtime.as_deref().unwrap_or(DEFAULT_RUNTIME)
    }
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "game".to_string(),
            file: None,
            runtime: None,
            source_of_truth: true,
            mount_aliases: true,
            sourcemap: true,
        }
    }
}

/// The `[fmt]` table: how Anneal, the formatter behind `alloy fmt`,
/// lays code out. The names follow larvae and stylua where the option is
/// theirs, so a config ports over; the Alloy-only options sit last.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct FmtConfig {
    /// Apply the recommended layout: the defaults below. Off, the
    /// formatter preserves what the file already does, and only the
    /// keys the project sets change that. See `FmtConfig::preserving`.
    pub recommended: bool,
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
    /// Read the indent of each file from the file itself. Set by
    /// `recommended = false` when the project names no indent, and
    /// never a key of the table.
    #[serde(skip)]
    pub detect_indent: bool,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self {
            recommended: true,
            column_width: 100,
            line_endings: LineEndings::Input,
            indent_type: IndentType::Spaces,
            indent_width: 2,
            quote_style: QuoteStyle::ForceSingle,
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
            detect_indent: false,
        }
    }
}

/// The width `recommended = false` puts on the formatter. It is a
/// number and not `usize::MAX` because the layout adds one to it when
/// it measures a markup hole, and that must not overflow.
pub const NO_REFLOW_WIDTH: usize = 1_000_000;

impl FmtConfig {
    /// The layout with nothing recommended: no line is reflowed, and
    /// every option that can keep what the author wrote does. The
    /// indent comes from each file. `[fmt]` keys the project sets
    /// apply over this.
    pub fn preserving() -> Self {
        Self {
            recommended: false,
            column_width: NO_REFLOW_WIDTH,
            quote_style: QuoteStyle::Preserve,
            leading_zero: LeadingZero::Preserve,
            call_parentheses: CallParentheses::Input,
            block_newline_gaps: BlockGaps::Preserve,
            detect_indent: true,
            alx: AlxFmt {
                attribute_quotes: AttributeQuotes::Preserve,
                text_wrap: TextWrap::Preserve,
                ..AlxFmt::default()
            },
            ..Self::default()
        }
    }

    /// The options for one file. Under `detect_indent` the indent is
    /// the file's own; otherwise the table's.
    pub fn for_source(&self, src: &str) -> Self {
        if !self.detect_indent {
            return self.clone();
        }

        let mut out = self.clone();

        if let Some((indent_type, width)) = detect_indent(src) {
            out.indent_type = indent_type;
            out.indent_width = width;
        }

        out
    }
}

/// The preserving profile with the keys of a written `[fmt]` table
/// over it. The caller has already parsed the same table once, so
/// every value here is one `FmtConfig` accepts.
fn over_preserving(written: &toml::Table) -> FmtConfig {
    let mut table = match toml::Value::try_from(FmtConfig::preserving()) {
        Ok(toml::Value::Table(t)) => t,
        _ => unreachable!("the layout serializes as a table"),
    };
    merge(&mut table, written);

    let mut out: FmtConfig = toml::Value::Table(table)
        .try_into()
        .expect("the [fmt] table already parsed");
    // An indent the project names wins over the file's own.
    out.detect_indent =
        !written.contains_key("indent_type") && !written.contains_key("indent_width");

    out
}

/// Writes every key of `over` into `base`, table by table, so
/// `[fmt.alx]` with one key keeps the rest of the profile.
fn merge(base: &mut toml::Table, over: &toml::Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),

            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// The indent of a source: tabs when a line starts with one, else the
/// smallest step between the leading spaces of two lines. `None` for a
/// file that indents nothing.
pub fn detect_indent(src: &str) -> Option<(IndentType, usize)> {
    let mut widths: Vec<usize> = Vec::new();

    for line in src.lines() {
        if line.trim().is_empty() {
            continue;
        }

        if line.starts_with('\t') {
            return Some((IndentType::Tabs, 4));
        }

        let spaces = line.len() - line.trim_start_matches(' ').len();

        if spaces > 0 {
            widths.push(spaces);
        }
    }

    // The step is the smallest gap between two indent levels, so a
    // block nested three deep does not read as one level of twelve.
    widths.sort_unstable();
    widths.dedup();

    let step = widths
        .windows(2)
        .map(|w| w[1] - w[0])
        .chain(widths.first().copied())
        .min()?;

    (step > 0).then_some((IndentType::Spaces, step))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LineEndings {
    /// The endings the file already uses, CRLF when its first line
    /// ends in one. A checkout on Windows keeps its endings.
    Input,
    Unix,
    Windows,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndentType {
    Spaces,
    Tabs,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteStyle {
    AutoPreferDouble,
    AutoPreferSingle,
    ForceDouble,
    ForceSingle,
    Preserve,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LeadingZero {
    Add,
    Strip,
    Preserve,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CallParentheses {
    Always,
    NoSingleString,
    NoSingleTable,
    None,
    Input,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FunctionNameSpace {
    Never,
    Definitions,
    Calls,
    Always,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Collapse {
    Never,
    FunctionOnly,
    ConditionalOnly,
    Always,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BlockGaps {
    Never,
    Preserve,
}

/// How a chain of method calls lays out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CallChainStyle {
    Preserve,
    Method,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields, default)]
pub struct SortRequires {
    pub enabled: bool,
    pub grouping: RequireGrouping,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RequireGrouping {
    #[default]
    Flat,
    ByKind,
}

/// The `[fmt.alx]` table: the markup of `.alx` files, after luaux-worm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct AlxFmt {
    /// The quotes of an attribute's string; `quote_style` does not govern it.
    pub attribute_quotes: AttributeQuotes,
    /// The `>` of a tag that breaks goes after the last attribute.
    pub bracket_same_line: bool,
    /// A tag that breaks puts one attribute per line, the way Prettier
    /// lays JSX out; off, it packs as many as fit.
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
            attribute_per_line: true,
            self_closing_space: true,
            text_wrap: TextWrap::Fill,
            blank_lines: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AttributeQuotes {
    Double,
    Single,
    Preserve,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TextWrap {
    Fill,
    Preserve,
}

/// The `[lint]` table: the two modes, and `[lint.rules]` under it.
///
/// The modes say where every lint starts. `recommended` applies the
/// level each lint declares, and `strict` raises the pedantic group to
/// `warn`. `[lint.rules]` then names a lint or a group and gives it a
/// level; a name beats its group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LintConfig {
    /// Apply the level each lint declares. Off, every lint starts at
    /// `allow` and `[lint.rules]` alone turns one on.
    pub recommended: bool,
    /// Turns the `pedantic` group on, at `warn`.
    pub strict: bool,
    /// `[lint.rules]`: a lint name, a group name, or `alx.<name>` for a
    /// markup lint, at `allow`, `warn`, or `deny`.
    pub rules: Rules,
    /// Deprecated: lints that fail the run. Write
    /// `[lint.rules] <name> = "deny"`.
    pub deny: Vec<String>,
    /// Deprecated: lints that print and pass.
    pub warn: Vec<String>,
    /// Deprecated: lints that stay silent.
    pub allow: Vec<String>,
}

/// The `[lint.rules]` table: a name to a level.
///
/// `alx.static_conditional_child = "warn"` is a nested table in TOML,
/// so the reader flattens what it reads back to the dotted name the
/// user wrote. A quoted key, `"enamel/no_effect" = "deny"`, arrives
/// flat already.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Rules(pub BTreeMap<String, crate::lint::Level>);

impl FromIterator<(String, crate::lint::Level)> for Rules {
    fn from_iter<I: IntoIterator<Item = (String, crate::lint::Level)>>(iter: I) -> Self {
        Rules(iter.into_iter().collect())
    }
}

impl std::ops::Deref for Rules {
    type Target = BTreeMap<String, crate::lint::Level>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Rules {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The report for a lint level that names no lint. The nearest name
/// answers a typo; the list answers the rest.
pub fn unknown_rule_message(key: &str) -> String {
    let near = crate::lint::LINTS
        .iter()
        .map(|l| l.name)
        .chain(crate::lint::Group::ALL.iter().map(|g| g.name()))
        .map(|n| (crate::typecheck::edit_distance(n, key), n))
        .filter(|(d, _)| *d > 0 && *d <= 3 && *d < key.len())
        .min();

    match near {
        Some((_, n)) => format!("`{key}` is not a lint; did you mean `{n}`?"),

        None => format!("`{key}` is not a lint; `alloy lint --list` has them"),
    }
}

impl<'de> Deserialize<'de> for Rules {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        fn walk<E: serde::de::Error>(
            prefix: &str,
            value: &toml::Value,
            out: &mut BTreeMap<String, crate::lint::Level>,
        ) -> Result<(), E> {
            match value {
                toml::Value::String(text) => {
                    let level = crate::lint::Level::from_name(text).ok_or_else(|| {
                        E::custom(format!(
                            "`{prefix} = \"{text}\"` is not one of allow, warn, deny"
                        ))
                    })?;

                    out.insert(prefix.to_string(), level);

                    Ok(())
                }

                toml::Value::Table(t) => {
                    for (k, v) in t {
                        walk(&format!("{prefix}.{k}"), v, out)?;
                    }

                    Ok(())
                }

                _ => Err(E::custom(format!(
                    "`{prefix}` takes a level: allow, warn, or deny"
                ))),
            }
        }

        let raw = BTreeMap::<String, toml::Value>::deserialize(d)?;
        let mut out = BTreeMap::new();

        for (k, v) in &raw {
            walk(k, v, &mut out)?;
        }

        Ok(Rules(out))
    }
}

impl LintConfig {
    /// The same modes with `strict` off: every lint at the level it
    /// declares, and the pedantic group silent.
    pub fn without_strict(&self) -> Self {
        Self {
            strict: false,
            ..self.clone()
        }
    }
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            recommended: true,
            strict: true,
            rules: Rules::default(),
            deny: Vec::new(),
            warn: Vec::new(),
            allow: Vec::new(),
        }
    }
}

/// The `[flux]` table: what `alloy flux` runs beyond the lints, and the
/// thresholds of the complexity lints. The levels of the lints stay in
/// `[lint]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Run the checker's new solver. `false` runs the old one, which
    /// evaluates no type function; a package whose type functions fail
    /// under the new solver checks clean there, as it does in an editor
    /// that runs the old one.
    pub new_solver: bool,
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
            new_solver: true,
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

/// The `[roblox]` table: what the place is like.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct RobloxConfig {
    /// The rig of a player's character, `R15` or `R6`. `Player.Character`
    /// types as `R15Character?` or `R6Character?` in the editor and in
    /// `alloy flux`.
    pub rig: String,
}

impl Default for RobloxConfig {
    fn default() -> Self {
        Self {
            rig: "R15".to_string(),
        }
    }
}

/// The `[test]` table: where `alloy test` writes the specs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct TestConfig {
    /// The folder the specs land in, relative to the root. Each source
    /// with a `@test` writes `<out>/<path>.spec.luau`.
    pub out: PathBuf,
    /// The suite name in `lest.toml`.
    pub suite: String,
    /// Write `lest.toml` and the `@lest` alias when the root has none.
    pub lest: bool,
    /// Load the engine doubles before each spec: `Vector3`, `Color3`,
    /// `Enum`, `task`, `game`, and a small Instance tree, so shared
    /// code runs on a plain VM. In Studio the doubles do nothing.
    pub shim: bool,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            out: PathBuf::from("tests"),
            suite: "alloy".to_string(),
            lest: true,
            shim: true,
        }
    }
}

/// The `[emit]` table: the few knobs that change what emitted code does.
/// Each one is a named exception to the razor, so the list stays short.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Emit {
    /// Seconds passed to every `WaitForChild` that `=>` emits. Unset means
    /// no timeout: the engine waits forever and warns after five seconds.
    /// With a timeout the call can return nil, so `=>` guards like `->`.
    /// The default is unset, so a project that never wrote the key keeps
    /// the non-optional `=>`. `TEMPLATE` writes five seconds, so a new
    /// project gets the guarded form.
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

/// The file `alloy init` writes. Every value here is a default but
/// one: `wait_timeout` is the opinion the template holds, so a new
/// project gets a five second `WaitForChild` and `=>` reads as
/// optional. A project that leaves the key out waits forever, which
/// is what `Emit::wait_timeout` defaults to.
pub const TEMPLATE: &str = r#"#:schema .alloy/alloy.schema.json
[build]
in = "src"
out = "build"
exclude = []
clean = false
artifact = "ship"

[emit]
wait_timeout = 5
# std_require = "@alloy"
# erase_type_imports = false

[fmt]
recommended = true
column_width = 100
indent_type = "spaces"
indent_width = 2
quote_style = "force-single"

[lint]
recommended = true
strict = true

[lint.rules]
# raw_require = "allow"

[flux]
typecheck = true
definitions = []

[test]
out = "tests"
suite = "alloy"
lest = true
shim = true

[project]
name = "game"
sourcemap = true
source_of_truth = true
mount_aliases = true

# [mount]
# alias = [path, mount]: the folder at path lands at mount in the DataModel
# shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
# server = ["src/server", "@game/ServerScriptService/Server"]
# alias = path: a name for a folder another mount already carries
# types = "src/shared/types"

# [ingots]
# an extension that ships as an executable: a path relative to this file,
# or a GitHub release pinned by version
# tailwind = "ingots/tailwind"
# tailwind = { repo = "alloy-luau/tailwind-ingot", version = "0.1.0" }
"#;

/// The `.luaurc` that `alloy init` writes into a root that already has
/// one: strict mode for every file, and the `@alloy` alias for the
/// runtime the build writes.
pub const LUAURC_TEMPLATE: &str = r#"{
  "languageMode": "strict",
  "aliases": {
    "alloy": "./build/alloy"
  }
}
"#;

/// The same configuration as `.config.luau`, the Luau-syntax form, and
/// the file `alloy init` writes into a root that has neither.
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
    /// A `.config.aly` that did not run, or gave a table that does not
    /// fit: the message says which.
    Script(PathBuf, String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(p, e) => write!(f, "cannot read {}: {e}", p.display()),

            ConfigError::Parse(p, e) => write!(f, "{}: {e}", p.display()),

            ConfigError::Script(p, e) if e.starts_with(&p.display().to_string()) => {
                write!(f, "{e}")
            }

            ConfigError::Script(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}

impl Config {
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut config: Self =
            toml::from_str(text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?;

        // `[fmt] recommended = false` moves the base under the table,
        // so the keys the project wrote have to be applied again, this
        // time over the preserving profile.
        if !config.fmt.recommended {
            let raw: toml::Table =
                toml::from_str(text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?;
            let written = raw
                .get("fmt")
                .and_then(|v| v.as_table())
                .cloned()
                .unwrap_or_default();

            config.fmt = over_preserving(&written);
        }

        Ok(config)
    }

    /// The keys of the file that still parse and no longer belong: the
    /// old `[lint]` lists and the old `[alx.lints]` table. Each line
    /// names the key that replaces it.
    pub fn deprecations(&self) -> Vec<String> {
        let mut out = Vec::new();

        for (list, level) in [
            (&self.lint.deny, "deny"),
            (&self.lint.warn, "warn"),
            (&self.lint.allow, "allow"),
        ] {
            if let Some(name) = list.first() {
                out.push(format!(
                    "`[lint] {level}` is deprecated; write `[lint.rules] {name} = \"{level}\"`"
                ));
            }
        }

        if let Some(level) = &self.alx.lints.static_conditional_child {
            let level = match level.as_str() {
                "off" => "allow",
                "error" => "deny",
                _ => "warn",
            };
            out.push(format!(
                "`[alx.lints]` is deprecated; write `[lint.rules] alx.static_conditional_child = \"{level}\"`"
            ));
        }

        out
    }

    /// Every lint level that names no lint: the `[lint.rules]` keys and
    /// the deprecated lists. An ingot's `<ingot>/<lint>` stands, since
    /// the ingot registers it after this file is read.
    pub fn unknown_rules(&self) -> Vec<String> {
        crate::lint::unknown_names(&self.lint)
            .iter()
            .filter(|name| !name.contains('/'))
            .map(|name| unknown_rule_message(name))
            .collect()
    }

    /// Reads `alloy.toml`, or runs `.config.aly`: the file's extension
    /// says which.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if path.extension().is_some_and(|e| e == "aly") {
            let table = crate::config_aly::evaluate(path)
                .map_err(|e| ConfigError::Script(path.to_path_buf(), e))?;

            return Self::from_table(table, path);
        }

        let text =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Read(path.to_path_buf(), e))?;

        Self::parse(&text, path)
    }

    /// A configuration from the table a `.config.aly` gives. It reads
    /// as `alloy.toml` reads, key for key.
    pub fn from_table(table: toml::Table, path: &Path) -> Result<Self, ConfigError> {
        let script =
            |e: toml::de::Error| ConfigError::Script(path.to_path_buf(), e.message().to_string());
        let mut config: Self = toml::Value::Table(table.clone())
            .try_into()
            .map_err(script)?;

        if !config.fmt.recommended {
            let written = table
                .get("fmt")
                .and_then(|v| v.as_table())
                .cloned()
                .unwrap_or_default();

            config.fmt = over_preserving(&written);
        }

        Ok(config)
    }

    /// The configuration file of a folder, or the `alloy.toml` path a
    /// report names when the folder holds none.
    pub fn file_of(dir: &Path) -> PathBuf {
        Self::file_in(dir).unwrap_or_else(|| dir.join(FILE_NAME))
    }

    /// The configuration file of a folder: `alloy.toml`, else
    /// `.config.aly`. A folder with both reads `alloy.toml`.
    pub fn file_in(dir: &Path) -> Option<PathBuf> {
        [FILE_NAME, crate::config_aly::FILE_NAME]
            .iter()
            .map(|name| dir.join(name))
            .find(|p| p.is_file())
    }

    /// The markup config of the project: the `[alx]` table when it
    /// holds anything, else a `luaux.toml` at the root, else the
    /// defaults.
    pub fn markup(&self, root: &Path) -> Result<luaux::Config, String> {
        let mut markup = match self.alx.is_set() {
            true => self.alx.to_markup()?,

            false => luaux::Config::load(root).map_err(|e| e.message)?,
        };

        // `[lint.rules]` owns the levels now; `[alx.lints]`, which the
        // branch above still reads, is the deprecated form and loses
        // to it. With neither written, the markup compiler keeps the
        // level its own backend picked, unless nothing is recommended.
        let name = "static_conditional_child";

        if self
            .lint
            .rules
            .contains_key(&format!("{}{name}", crate::lint::ALX_PREFIX))
            || !self.lint.recommended
        {
            markup.static_conditional_child =
                markup_level(crate::lint::alx_level_of(&self.lint, name));
        }

        Ok(markup)
    }

    /// Finds `alloy.toml` or `.config.aly` in `start` or the nearest ancestor. The project
    /// root is the directory that holds it, and every path in the file is
    /// relative to that root.
    pub fn find(start: &Path) -> Option<PathBuf> {
        // A relative start has no parents to walk: `src` ends at `src`.
        let start = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
        let mut dir = Some(start.as_path());

        while let Some(d) = dir {
            if let Some(found) = Self::file_in(d) {
                return Some(found);
            }

            dir = d.parent();
        }

        None
    }

    /// The nearest configuration file at or above `start`, with the walk
    /// stopped at `stop`. The language server passes the workspace root
    /// as `stop`, so one project never reads the configuration of
    /// another that shares a parent directory.
    pub fn find_within(start: &Path, stop: &Path) -> Option<PathBuf> {
        let start = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
        let stop = std::path::absolute(stop).unwrap_or_else(|_| stop.to_path_buf());
        let mut dir = Some(start.as_path());

        while let Some(d) = dir {
            if let Some(found) = Self::file_in(d) {
                return Some(found);
            }

            if d == stop {
                return None;
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

    /// `[lint.rules]` takes a lint, a group, `alx.<name>`, one of the
    /// checker's, or an ingot's `<ingot>/<lint>`. Anything else is a
    /// report, next to the deprecations every run prints.
    #[test]
    fn an_unknown_lint_rule_reports() {
        let parse = |text: &str| Config::parse(text, Path::new("alloy.toml")).expect("the config");
        let c = parse("[lint.rules]\ntotally_unknown_lint_name = \"deny\"\n");
        let rules = c.unknown_rules();

        assert_eq!(rules.len(), 1, "{rules:?}");
        assert!(
            rules[0].contains("`totally_unknown_lint_name` is not a lint"),
            "{}",
            rules[0]
        );
        assert!(rules[0].contains("alloy lint --list"), "{}", rules[0]);

        // A typo names the lint it is near.
        let near = parse("[lint.rules]\nunused_variabl = \"deny\"\n").unknown_rules();

        assert_eq!(
            near,
            vec!["`unused_variabl` is not a lint; did you mean `unused_variable`?"]
        );

        // The names that stand, an ingot's among them.
        let known = parse(
            "[lint.rules]\nunused_variable = \"deny\"\nstyle = \"allow\"\nluau = \"warn\"\nLocalUnused = \"allow\"\n\"enamel/no_effect\" = \"deny\"\n\n[lint.rules.alx]\nstatic_conditional_child = \"warn\"\n",
        );

        assert!(
            known.unknown_rules().is_empty(),
            "{:?}",
            known.unknown_rules()
        );
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
        assert_eq!(c.project.runtime, None);
        assert_eq!(c.project.runtime(), "@game/ReplicatedStorage/Alloy");
    }

    #[test]
    fn a_bare_string_mount_is_an_alias_with_no_place() {
        let c = Config::parse(
            "[mount]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\ntypes = \"src/shared/types\"\n",
            Path::new("alloy.toml"),
        )
        .unwrap();
        assert_eq!(
            c.mount["types"],
            Mount("src/shared/types".into(), String::new())
        );
        assert!(c.mount["types"].alias_only());
        assert!(!c.mount["shared"].alias_only());
    }

    #[test]
    fn an_ingot_is_a_path_or_a_pinned_release() {
        let c = Config::parse(
            "[ingots]\na = \"ingots/a\"\nb = { repo = \"o/r\", version = \"1.0.0\", order = -1 }\n\n[ingot.a]\nprefix = \"tw-\"\n",
            Path::new("alloy.toml"),
        )
        .unwrap();
        assert_eq!(c.ingots["a"].table().path.as_deref(), Some("ingots/a"));
        let b = c.ingots["b"].table();
        assert_eq!(b.repo.as_deref(), Some("o/r"));
        assert_eq!(b.order, Some(-1));
        assert_eq!(c.ingot["a"]["prefix"].as_str(), Some("tw-"));
    }

    /// The template writes one value that is not a default, and the
    /// test names it: a new project waits five seconds for a child.
    /// Every other key still has to match, so a second opinion that
    /// creeps into the file fails here.
    #[test]
    fn the_template_holds_one_opinion_and_the_defaults() {
        let c = Config::parse(TEMPLATE, Path::new("alloy.toml")).unwrap();

        assert_eq!(c.emit.wait_timeout, Some(5.0));
        assert_eq!(Emit::default().wait_timeout, None);

        let mut want = Config::default();
        want.emit.wait_timeout = Some(5.0);

        assert_eq!(c, want);
    }
}
