//! `luaux.toml` — element and property aliases (PLAN.md §8).
//!
//! ```toml
//! [elements]              # overrides for default (Roblox) element names
//! TextLabel = "text"
//!
//! [properties]            # applies to all elements
//! TextColor3 = "textColor"
//!
//! [properties.Frame]      # per-class; beats the global table
//! BackgroundColor3 = "bgColor"
//! ```
//!
//! Overrides are **exclusive**: renaming a name retires the original, so once
//! `TextLabel = "text"` is declared, `<TextLabel>` is an error and `<text>` is
//! the spelling. That is what makes a project's vocabulary consistent rather
//! than offering two ways to write everything.
//!
//! A per-class entry can map a name to itself — `[properties.TextLabel]
//! TextColor3 = "TextColor3"` — to opt one class back out of a global rename.

use crate::roblox;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    elements: HashMap<String, String>,
    #[serde(default)]
    properties: HashMap<String, PropertyEntry>,
    #[serde(default)]
    lints: RawLints,
    #[serde(default)]
    factory: RawFactory,

    #[serde(default)]
    build: RawBuild,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    #[serde(rename = "in")]
    input: Option<String>,
    #[serde(rename = "out")]
    output: Option<String>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    clean: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFactory {
    backend: Option<String>,
    create: Option<String>,
    children: Option<String>,
    event: Option<String>,
    compute: Option<String>,
    #[serde(rename = "use")]
    use_fn: Option<String>,
    fragment: Option<String>,
    interpolate: Option<String>,
    merge: Option<String>,
}

impl RawFactory {
    /// Whether the project wrote a `[factory]` block with anything in it.
    ///
    /// The dividing line for defaults: with no block, luaux picks a library.
    /// With one, it assumes nothing — see [`Config::default`].
    fn is_set(&self) -> bool {
        self.backend.is_some()
            || self.create.is_some()
            || self.children.is_some()
            || self.event.is_some()
            || self.compute.is_some()
            || self.use_fn.is_some()
            || self.fragment.is_some()
            || self.interpolate.is_some()
            || self.merge.is_some()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLints {
    static_conditional_child: Option<String>,
}

/// A blanket renaming scheme, set with `all` in `[elements]` or `[properties]`.
///
/// `Pascal` is the identity: Roblox's own spelling, and the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Casing {
    #[default]
    Pascal,
    Camel,
    Snake,
    Flat,
}

impl Casing {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "PascalCase" => Some(Self::Pascal),
            "camelCase" => Some(Self::Camel),
            "snake_case" => Some(Self::Snake),
            "flatcase" => Some(Self::Flat),
            _ => None,
        }
    }

    /// Rewrites a Roblox name into this scheme.
    ///
    /// Word boundaries are taken from the canonical PascalCase spelling, so
    /// `UICorner` splits as `UI` + `Corner` rather than per capital, giving
    /// `ui_corner` instead of `u_i_corner`.
    pub fn apply(&self, name: &str) -> String {
        if *self == Casing::Pascal {
            return name.to_string();
        }

        if *self == Casing::Flat {
            return name.to_lowercase();
        }

        let mut words: Vec<String> = Vec::new();
        let chars: Vec<char> = name.chars().collect();
        let mut start = 0;

        for index in 1..chars.len() {
            let previous = chars[index - 1];
            let current = chars[index];
            let next = chars.get(index + 1).copied();

            // lower→upper ends a word: `textLabel` -> `text` `Label`.
            // upper→upper→lower ends one too: `UICorner` -> `UI` `Corner`.
            let boundary = (previous.is_lowercase() || previous.is_numeric())
                && current.is_uppercase()
                || previous.is_uppercase()
                    && current.is_uppercase()
                    && next.is_some_and(char::is_lowercase);

            if boundary {
                words.push(chars[start..index].iter().collect());
                start = index;
            }
        }

        words.push(chars[start..].iter().collect());

        match self {
            Casing::Snake => words
                .iter()
                .map(|word| word.to_lowercase())
                .collect::<Vec<_>>()
                .join("_"),
            Casing::Camel => words
                .iter()
                .enumerate()
                .map(|(index, word)| {
                    if index == 0 {
                        word.to_lowercase()
                    } else {
                        let mut chars = word.chars();
                        match chars.next() {
                            Some(first) => {
                                first.to_uppercase().collect::<String>()
                                    + &chars.as_str().to_lowercase()
                            }
                            None => String::new(),
                        }
                    }
                })
                .collect(),
            Casing::Pascal | Casing::Flat => unreachable!("handled above"),
        }
    }
}

/// Picks the canonical name when several collapse onto one under a casing.
///
/// Roblox ships deprecated spellings beside modern ones (`brickColor` beside
/// `BrickColor`), and they differ only by case. Prefer whatever is not
/// deprecated; if that leaves no single winner — `FormFactor`/`formFactor` are
/// both deprecated — fall back to byte order, which puts the uppercase form
/// first. See docs/adr/0001-casing-key.md.
pub fn preferred<'a>(candidates: &mut Vec<&'a str>) -> Option<&'a str> {
    if candidates.len() > 1 && candidates.iter().any(|name| !roblox::is_deprecated(name)) {
        candidates.retain(|name| !roblox::is_deprecated(name));
    }

    candidates.sort_unstable();
    candidates.first().copied()
}

/// How a lint reports. `Warn` is the default for `static_conditional_child`: the
/// pattern it catches is usually a mistake, but a conditional on a constant is
/// legitimate and would otherwise be unsilenceable (PLAN.md §11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LintLevel {
    Off,
    #[default]
    Warn,
    Error,
}

impl LintLevel {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "off" => Some(Self::Off),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PropertyEntry {
    /// `TextColor3 = "textColor"` — a global rename.
    Alias(String),
    /// `[properties.Frame]` — a per-class table.
    PerClass(HashMap<String, String>),
}

/// Resolved alias tables, keyed for lookup by what the user *writes*.
#[derive(Debug, Clone)]
pub struct Config {
    /// alias → canonical class name
    element_alias: HashMap<String, String>,
    /// canonical class name → alias that replaced it
    element_renamed: HashMap<String, String>,
    /// canonical property → alias, applying to every class
    global_properties: HashMap<String, String>,
    /// class → (canonical property → alias)
    class_properties: HashMap<String, HashMap<String, String>>,
    /// `[elements] all` — a blanket scheme, beaten by any explicit entry.
    element_casing: Casing,
    /// `[properties] all` — likewise, for properties and events.
    property_casing: Casing,
    /// §11.1 — LuauX in a child expression that no function encloses.
    pub static_conditional_child: LintLevel,
    /// Paths and file selection.
    pub build: Build,
    /// Expression called to construct an element — TypeScript's `jsxFactory`.
    ///
    /// Naming, not shape: whatever this points at must still match the
    /// arrangement the selected backend emits. A different *arrangement* —
    /// children in a third argument, say — needs a backend, not a name
    /// (backend-plan.md §2).
    ///
    /// Trimmed and checked to lower to Luau when it came from
    /// [`Config::parse`]. [`Config::with_create`] does neither, so a caller
    /// building a config by hand owns that.
    pub create: String,
    /// `[factory] children` — the table key an element's children go under.
    ///
    /// Unset, children are numeric entries in the props table, which is Vide's
    /// convention. Set, they become one `[E] = { … }` entry, which is Fusion's.
    ///
    /// Deliberately a *key expression* and not a general placement mechanism:
    /// a sentinel that moved children out of the table entirely would change the
    /// call's arrangement, and that is a backend's job (backend-plan.md §5.5).
    pub children: Option<String>,
    /// `[factory] event` — how an event name becomes a table key.
    ///
    /// Unset, an event is an ordinary string key. Set, it is wrapped — called
    /// for Fusion's `OnEvent`, indexed for React's `React.Event`.
    ///
    /// Applies only to intrinsics, because that is the only place LuauX knows
    /// an attribute *is* an event. A component's props are arbitrary.
    pub event: Option<EventKey>,
    /// `[factory] compute` — the wrapper for interpolated text.
    ///
    /// Unset, interpolated text is a thunk reading each hole through the inlined
    /// `__luaux_read`. Set, it is `E(function(use) return … end)` and no helper
    /// is inlined — the reader comes from the callback.
    pub compute: Option<String>,
    /// `[factory] use` — the reader's name inside `compute`.
    ///
    /// Resolved to `use` when `compute` is set, and `None` otherwise, so an
    /// emission never has to ask whether the name is meaningful.
    pub use_fn: Option<String>,
    /// `[factory] backend` — which constructor arrangement to emit.
    pub backend: BackendKind,
    /// `[factory] fragment` — the component a fragment is constructed with.
    ///
    /// Unset, a fragment is a plain table, which is what a one-table library
    /// recurses. Required by the element backend, where a bare table is not an
    /// element and there is nothing sensible to guess.
    pub fragment: Option<String>,
    /// `[factory] interpolate` — how interpolated text is encoded.
    pub interpolate: Interpolate,
    /// Alloy patch: names the caller knows are bound, on top of what the
    /// resolver finds by parsing. Alloy's parser sees `import` and `struct`
    /// bindings that full_moon does not.
    pub extra_bound: HashSet<String>,
    /// `[factory] merge` — how spread groups combine.
    ///
    /// Unset, the inlined `__luaux_merge`: string keys last-wins so source order
    /// decides precedence, numeric keys concatenate. Set, that expression is
    /// called instead and nothing is inlined.
    ///
    /// Exists because `children` changes what the default helper means — under a
    /// children key the numeric branch no longer sees children, and a spread
    /// carrying its own children collides last-wins rather than concatenating.
    /// That is defensible and not obviously right, so there is somewhere to say
    /// otherwise (factory-plan.md §3.5).
    pub merge: Option<String>,
}

/// Which constructor arrangement a backend emits (backend-plan.md §2).
///
/// Named for the shape rather than the library, for the same reason the backend
/// itself is: a value named `react` would make every library without a value of
/// its own second-class, which is the argument against a `preset` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendKind {
    /// `F(class)(props)` — one curried constructor, children in the props
    /// table. Vide and Fusion.
    #[default]
    Table,
    /// `F(class, props, children)` — children in a third positional argument.
    /// React.
    Element,
}

impl BackendKind {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "table" => Some(Self::Table),
            "element" => Some(Self::Element),
            _ => None,
        }
    }
}

/// How interpolated text is encoded.
///
/// Its own key rather than something the backend implies, because the two are
/// independent: a one-table library with no reactivity would want `plain` too,
/// and nothing about an arrangement says anything about strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interpolate {
    /// Wrapped so the library re-runs it when a hole changes — a thunk by
    /// default, or `[factory] compute` when set.
    #[default]
    Wrap,
    /// A plain interpolated string, with each hole emitted bare. For a library
    /// with no per-prop reactivity, where a hole is already a value.
    Plain,
}

impl Interpolate {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "wrap" => Some(Self::Wrap),
            "plain" => Some(Self::Plain),
            _ => None,
        }
    }
}

/// How an event name is spelled as a table key.
///
/// Two forms, because the libraries disagree and the difference is not
/// cosmetic: Fusion's `OnEvent` is *called* with the name, React's
/// `React.Event` is *indexed* by it (backend-plan.md §5.3).
///
/// Spelled in `luaux.toml` by whether the value ends in a dot:
///
/// ```toml
/// event = "OnEvent"        # [OnEvent("Activated")]
/// event = "React.Event."   # [React.Event.Activated]
/// ```
///
/// A trailing dot is how the index form is already written in Luau, so nothing
/// is invented — the alternative was a second key, or a `%s` placeholder inside
/// a string meant to hold an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKey {
    /// `[E("Activated")]`
    Call(String),
    /// `[E.Activated]`
    Index(String),
}

impl EventKey {
    /// Reads the `luaux.toml` spelling. A trailing `.` selects the index form.
    fn parse(value: &str) -> Self {
        match value.strip_suffix('.') {
            Some(expression) => Self::Index(expression.trim_end().to_string()),
            None => Self::Call(value.to_string()),
        }
    }

    /// The bracketed table key for one canonical event name.
    pub fn key(&self, event: &str) -> String {
        match self {
            Self::Call(expression) => format!("[{expression}(\"{event}\")]"),
            Self::Index(expression) => format!("[{expression}.{event}]"),
        }
    }

    /// The expression itself, for the in-scope check in [`crate::imports`].
    pub fn expression(&self) -> &str {
        match self {
            Self::Call(expression) | Self::Index(expression) => expression,
        }
    }
}

/// The zero-config element factory.
///
/// React, because JSX is React's syntax and a `.luaux` that lowers to it needs
/// no explanation. A project using anything else writes a `[factory]` block, and
/// the moment it does, nothing is assumed — see [`Config::default`].
pub const DEFAULT_CREATE: &str = "React.createElement";

/// The factory for the one-table arrangement, matching the common
/// `local create = vide.create`.
pub const BARE_CREATE: &str = "create";

/// The reader's name inside a `compute` callback, matching Fusion's own docs.
pub const DEFAULT_USE: &str = "use";

/// Which files a build considers, and where they go.
#[derive(Debug, Clone)]
pub struct Build {
    pub input: Option<PathBuf>,
    pub output: Option<PathBuf>,
    /// Globs a file must match to be considered. A pattern with no `/` matches
    /// at any depth, as in gitignore.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Delete outputs whose source is gone, so a renamed file leaves no stale
    /// twin for rojo to keep syncing.
    pub clean: bool,
}

impl Default for Build {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            include: vec!["**".to_string()],
            exclude: Vec::new(),
            clean: false,
        }
    }
}

/// The mode defaults an arrangement implies.
///
/// Split from the *name* defaults deliberately. Writing `[factory]` turns off
/// every assumption about names — that is the rule that stops a Vide project
/// inheriting React's `create`. It cannot turn off what the arrangement itself
/// decides: an element-shaped library has no per-prop reactivity, so interpolated
/// text has nothing to wrap, and its component body re-runs, so a conditional
/// child built once is ordinary rather than a mistake. Both follow from
/// `backend`, which the project *did* name.
///
/// One function because its two callers diverged once already. The lint was
/// defaulted off in the parsed path only, so a project with no `luaux.toml` at
/// all — the arrangement the React default exists to serve — got React *and* a
/// warning telling it to wrap a conditional child in a function, which is advice
/// that makes React render the function.
const fn arrangement_defaults(backend: BackendKind) -> (Interpolate, LintLevel) {
    match backend {
        BackendKind::Element => (Interpolate::Plain, LintLevel::Off),
        BackendKind::Table => (Interpolate::Wrap, LintLevel::Warn),
    }
}

/// The zero-config default: React.
///
/// This is the *only* place luaux names a library, and it applies only when a
/// project has written no `[factory]` block at all. Writing one turns every
/// assumption off and makes `backend` required, because the alternative is the
/// failure mode this compiler avoids everywhere else: a Vide project that set
/// only `create` would inherit React's arrangement, pass the in-scope check
/// because its own name really is in scope, and emit the wrong shape in
/// silence.
///
/// That is a correction to backend-plan.md §6, which argued the in-scope check
/// made a flipped default safe on its own. It makes a *wrong name* loud. It says
/// nothing about a right name in the wrong arrangement.
impl Default for Config {
    fn default() -> Self {
        Self {
            element_alias: HashMap::new(),
            element_renamed: HashMap::new(),
            global_properties: HashMap::new(),
            class_properties: HashMap::new(),
            element_casing: Casing::default(),
            property_casing: Casing::default(),
            static_conditional_child: arrangement_defaults(BackendKind::Element).1,
            build: Build::default(),
            create: DEFAULT_CREATE.to_string(),
            children: None,
            event: Some(EventKey::Index("React.Event".to_string())),
            compute: None,
            use_fn: None,
            backend: BackendKind::Element,
            fragment: Some("React.Fragment".to_string()),
            interpolate: arrangement_defaults(BackendKind::Element).0,
            extra_bound: HashSet::new(),
            merge: None,
        }
    }
}

impl Config {
    /// The blanket scheme for element names, for tooling that has to render or
    /// complete names in the project's own spelling.
    pub fn element_casing(&self) -> Casing {
        self.element_casing
    }

    /// The blanket scheme for properties and events.
    pub fn property_casing(&self) -> Casing {
        self.property_casing
    }

    /// A config whose only non-default setting is the element factory.
    ///
    /// Takes the factory as given — no trimming, and none of the checking
    /// [`Config::parse`] does. A value that will not lower to Luau still
    /// reaches the backend from here and still comes back as luaux's own
    /// "please report it" internal error, so a caller taking this from user
    /// input wants `parse` instead.
    pub fn with_create(create: impl Into<String>) -> Self {
        Self {
            create: create.into(),
            ..Self::bare()
        }
    }

    /// The one-table arrangement with nothing assumed.
    ///
    /// What a project gets the moment it writes a `[factory]` block: bare
    /// `create`, no children key, no event wrapper, no fragment, wrapped text.
    /// Every difference from here is something the project asked for.
    pub fn bare() -> Self {
        let (interpolate, static_conditional_child) = arrangement_defaults(BackendKind::Table);

        Self {
            create: BARE_CREATE.to_string(),
            children: None,
            event: None,
            compute: None,
            use_fn: None,
            backend: BackendKind::Table,
            fragment: None,
            merge: None,
            // Both derived rather than restated, because `..Self::default()`
            // below inherits from the *React* config: every field this one does
            // not name explicitly is a React value in a table-shaped config, and
            // the lint level reached here that way.
            interpolate,
            static_conditional_child,
            ..Self::default()
        }
    }

    /// Loads `luaux.toml` from `directory`, or returns an empty config if there
    /// is none. Absent config is not an error; luaux works without one.
    pub fn load(directory: &Path) -> Result<Self, ConfigError> {
        Self::load_reporting(directory).map(|(config, _)| config)
    }

    /// Loads, also returning notes about settings that are accepted but inert.
    pub fn load_reporting(directory: &Path) -> Result<(Self, Vec<String>), ConfigError> {
        let path = directory.join("luaux.toml");

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), Vec::new()))
            }
            Err(error) => {
                return Err(ConfigError {
                    message: format!("{}: {error}", path.display()),
                })
            }
        };

        Self::parse_reporting(&text)
    }

    /// Parses, discarding warnings. For callers that only need the config.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Self::parse_reporting(text).map(|(config, _)| config)
    }

    /// Parses, also returning notes about settings that are accepted but inert.
    pub fn parse_reporting(text: &str) -> Result<(Self, Vec<String>), ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|error| ConfigError {
            message: format!("luaux.toml: {}", error.message()),
        })?;

        // With no `[factory]` block luaux picks a library; with one it assumes
        // nothing. Everything below then *adds* to whichever base that is.
        let configured = raw.factory.is_set();
        let mut config = match configured {
            true => Config::bare(),
            false => Config::default(),
        };

        if let Some(level) = &raw.lints.static_conditional_child {
            config.static_conditional_child =
                LintLevel::parse(level).ok_or_else(|| ConfigError {
                    message: format!(
                        "luaux.toml: [lints] static_conditional_child = \"{level}\" is not one of \
                         off, warn, error"
                    ),
                })?;
        }

        let mut warnings = Vec::new();

        config.build.input = raw.build.input.map(PathBuf::from);
        config.build.output = raw.build.output.map(PathBuf::from);
        if let Some(include) = raw.build.include {
            config.build.include = include;
        }
        if let Some(exclude) = raw.build.exclude {
            config.build.exclude = exclude;
        }
        config.build.clean = raw.build.clean.unwrap_or(false);

        // Required once anything else in the block is set, so an arrangement is
        // never inherited by accident. One loud line to add beats output that
        // compiles and is wrong.
        if configured && raw.factory.backend.is_none() {
            return Err(ConfigError {
                message: "luaux.toml: [factory] needs a backend: \"table\" for Vide, Fluid or Fusion, \"element\" for React"
                    .to_string(),
            });
        }

        if let Some(backend) = &raw.factory.backend {
            config.backend = BackendKind::parse(backend.trim()).ok_or_else(|| ConfigError {
                message: format!(
                    "luaux.toml: [factory] backend = \"{backend}\" is not one of table, element"
                ),
            })?;
        }

        // Modes follow the arrangement; names do not. Applied here rather than
        // in the defaults so an explicitly named backend gets the modes that
        // belong to it, and applied before the explicit keys below so either can
        // still override.
        let (interpolate, lint) = arrangement_defaults(config.backend);
        config.interpolate = interpolate;
        if raw.lints.static_conditional_child.is_none() {
            config.static_conditional_child = lint;
        }

        if let Some(interpolate) = &raw.factory.interpolate {
            config.interpolate =
                Interpolate::parse(interpolate.trim()).ok_or_else(|| ConfigError {
                    message: format!(
                        "luaux.toml: [factory] interpolate = \"{interpolate}\" is not one of \
                         wrap, plain"
                    ),
                })?;
        }

        if let Some(create) = raw.factory.create {
            let create = factory_value("create", &create)?;
            validate_create(create)?;
            config.create = create.to_string();
        }

        if let Some(merge) = raw.factory.merge {
            let merge = factory_value("merge", &merge)?;
            validate_factory("merge", merge, &format!("local _ = {merge}({{}}, {{}})"))?;
            config.merge = Some(merge.to_string());
        }

        if let Some(fragment) = raw.factory.fragment {
            let fragment = factory_value("fragment", &fragment)?;
            // Emitted as the constructor's first argument, so that is the shape
            // it is checked in.
            validate_factory(
                "fragment",
                fragment,
                &format!("local _ = f({fragment}, nil, {{}})"),
            )?;
            config.fragment = Some(fragment.to_string());
        }

        if let Some(children) = raw.factory.children {
            let children = factory_value("children", &children)?;
            // Emitted as a table key, so that is the shape it is checked in.
            validate_factory(
                "children",
                children,
                &format!("local _ = {{ [{children}] = {{}} }}"),
            )?;
            config.children = Some(children.to_string());
        }

        if let Some(event) = raw.factory.event {
            let event = factory_value("event", &event)?;
            let key = EventKey::parse(event);

            // A bare `.` leaves nothing to index, and `EventKey::parse` would
            // hand back an empty expression that probes as `[.X]`.
            if key.expression().is_empty() {
                return Err(ConfigError {
                    message: format!(
                        "luaux.toml: [factory] event = \"{event}\" has a `.` with no name beside it"
                    ),
                });
            }

            validate_factory(
                "event",
                event,
                &format!("local _ = {{ {} = f }}", key.key("X")),
            )?;
            config.event = Some(key);
        }

        if let Some(compute) = raw.factory.compute {
            let compute = factory_value("compute", &compute)?;
            validate_factory(
                "compute",
                compute,
                &format!("local _ = {compute}(function(use) return \"\" end)"),
            )?;
            config.compute = Some(compute.to_string());
        }

        // Checked even when it turns out to be inert: a malformed value is worth
        // reporting whether or not anything reads it, and staying silent would
        // make the key look accepted right up until `compute` is added.
        let use_fn = match &raw.factory.use_fn {
            Some(value) => {
                let value = factory_value("use", value)?;
                validate_use(value)?;
                Some(value.to_string())
            }
            None => None,
        };

        // `use` names the reader inside `compute`'s callback, so it means
        // nothing without one. Resolved here rather than at emission, so the
        // backend never has to ask whether the name is meaningful — it is
        // `Some` exactly when `compute` is.
        config.use_fn = match (config.compute.is_some(), use_fn) {
            (true, Some(name)) => Some(name),
            (true, None) => Some(DEFAULT_USE.to_string()),
            (false, Some(name)) => {
                warnings.push(format!(
                    "luaux.toml: [factory] use = \"{name}\" does nothing without [factory] \
                     compute, whose callback is what it names the reader of"
                ));
                None
            }
            (false, None) => None,
        };

        // Cross-key rules. A key that is meaningless under the selected backend
        // is rejected rather than ignored: a config that silently does nothing
        // is the hardest kind to debug, because the output looks deliberate.
        if config.backend == BackendKind::Element {
            if config.fragment.is_none() {
                return Err(ConfigError {
                    message: "luaux.toml: [factory] backend = \"element\" needs a fragment"
                        .to_string(),
                });
            }

            // `children` is a *table key*, and the element backend has no table
            // to put it in — children are a positional argument there. A
            // sentinel that moved them would change the call's arrangement,
            // which is a backend's job, not a key's (backend-plan.md §5.5).
            if config.children.is_some() {
                return Err(ConfigError {
                    message: "luaux.toml: [factory] children is a table key, and the element \
                              backend passes children as an argument instead"
                        .to_string(),
                });
            }
        }

        // A fragment is a plain table under this arrangement, and the key is
        // never read — so a project that set it would get a fragment built the
        // way it always was, from a config naming something else. Rejected for
        // the same reason `children` is rejected the other way round.
        if config.backend == BackendKind::Table && config.fragment.is_some() {
            return Err(ConfigError {
                message: "luaux.toml: [factory] fragment is only read by the element backend; a fragment is a plain table under this one".to_string(),
            });
        }

        // `compute` names the wrapper for interpolated text, and `plain` says
        // there is no wrapper. Together they are a contradiction, and the
        // emission resolved it by dropping the wrapper — so a project that set
        // both got text that silently never updated, from a config that named
        // the thing meant to update it.
        //
        // This is the one place two `[factory]` keys steer the same decision,
        // which is why it is the one place they can contradict each other.
        if config.interpolate == Interpolate::Plain && config.compute.is_some() {
            return Err(ConfigError {
                message: "luaux.toml: [factory] interpolate = \"plain\" leaves nothing for \
                          compute to wrap — drop one of them"
                    .to_string(),
            });
        }

        // `all` is reserved in both tables. No Roblox class or member is named
        // `all`, so claiming it costs nothing.
        if let Some(value) = raw.elements.get("all") {
            config.element_casing = Casing::parse(value).ok_or_else(|| ConfigError {
                message: format!(
                    "luaux.toml: [elements] all = \"{value}\" is not one of PascalCase, \
                     camelCase, snake_case, flatcase"
                ),
            })?;
        }

        for (class, alias) in raw.elements.iter().filter(|(key, _)| *key != "all") {
            if !roblox::is_class(class) {
                return Err(ConfigError {
                    message: format!(
                        "luaux.toml: [elements] {class} is not a creatable Roblox class{}",
                        suggest_class(class)
                    ),
                });
            }

            if let Some(existing) = config.element_alias.get(alias) {
                return Err(ConfigError {
                    message: format!(
                        "luaux.toml: [elements] {existing} and {class} both claim the alias \
                         \"{alias}\""
                    ),
                });
            }

            config.element_alias.insert(alias.clone(), class.clone());
            config.element_renamed.insert(class.clone(), alias.clone());
        }

        if let Some(PropertyEntry::Alias(value)) = raw.properties.get("all") {
            config.property_casing = Casing::parse(value).ok_or_else(|| ConfigError {
                message: format!(
                    "luaux.toml: [properties] all = \"{value}\" is not one of PascalCase, \
                     camelCase, snake_case, flatcase"
                ),
            })?;
        }

        for (key, entry) in raw.properties.iter().filter(|(key, _)| *key != "all") {
            match entry {
                PropertyEntry::Alias(alias) => {
                    if !roblox::is_member_name(key) {
                        return Err(ConfigError {
                            message: format!(
                                "luaux.toml: [properties] {key} is not a property or event of any \
                                 Roblox class{}",
                                suggest_member_anywhere(key)
                            ),
                        });
                    }
                    config.global_properties.insert(key.clone(), alias.clone());
                }
                PropertyEntry::PerClass(entries) => {
                    if !roblox::is_class(key) {
                        return Err(ConfigError {
                            message: format!(
                                "luaux.toml: [properties.{key}] is not a creatable Roblox class{}",
                                suggest_class(key)
                            ),
                        });
                    }

                    for property in entries.keys() {
                        if !roblox::has_property(key, property) && !roblox::is_event(key, property)
                        {
                            return Err(ConfigError {
                                message: format!(
                                    "luaux.toml: [properties.{key}] {key} has no property or event \
                                     named {property}{}",
                                    suggest_member(key, property)
                                ),
                            });
                        }
                    }

                    config.class_properties.insert(key.clone(), entries.clone());
                }
            }
        }

        Ok((config, warnings))
    }

    /// Maps a written tag to a canonical class name.
    ///
    /// `Ok(None)` means the tag is not an alias and should be resolved normally.
    pub fn resolve_element(&self, written: &str) -> Result<Option<&str>, String> {
        if let Some(class) = self.element_alias.get(written) {
            return Ok(Some(class));
        }

        // Writing the original name of something that was renamed.
        if let Some(alias) = self.element_renamed.get(written) {
            if alias != written {
                return Err(format!(
                    "<{written}> was renamed by luaux.toml; use <{alias}>"
                ));
            }
        }

        if self.element_casing != Casing::Pascal {
            // An explicit entry already had its chance above, so `all` only
            // ever answers for names it did not claim.
            let mut matches: Vec<&str> = roblox::creatable_classes()
                .filter(|class| !self.element_renamed.contains_key(*class))
                .filter(|class| self.element_casing.apply(class) == written)
                .collect();

            if let Some(class) = preferred(&mut matches) {
                return Ok(Some(class));
            }

            // Under a blanket rename the canonical spelling retires, exactly as
            // a single override retires the name it replaces.
            if roblox::is_class(written) {
                return Err(format!(
                    "<{written}> was renamed by [elements] all; use <{}>",
                    self.element_casing.apply(written)
                ));
            }
        }

        Ok(None)
    }

    /// How `class` is spelled in this project — its alias, if one was configured.
    ///
    /// The inverse of [`Config::resolve_element`], and the question tooling asks:
    /// offering `<TextLabel>` in a project that renamed it to `text` would be
    /// offering the one spelling that is an error, since overrides are exclusive.
    pub fn element_name<'a>(&'a self, class: &'a str) -> &'a str {
        self.element_renamed
            .get(class)
            .map_or(class, String::as_str)
    }

    /// How a canonical property or event is spelled on `class`.
    ///
    /// Per-class entries beat the global table, matching
    /// [`Config::resolve_property`] — including the identity override a class
    /// uses to opt back out of a global rename.
    pub fn property_name<'a>(&'a self, class: &str, canonical: &'a str) -> &'a str {
        if let Some(alias) = self
            .class_properties
            .get(class)
            .and_then(|entries| entries.get(canonical))
        {
            return alias;
        }

        self.global_properties
            .get(canonical)
            .map_or(canonical, String::as_str)
    }

    /// Maps a written attribute on `class` to its canonical property name.
    pub fn resolve_property(&self, class: &str, written: &str) -> Result<String, String> {
        // Per-class entries beat the global table, so a class can opt back out
        // of a global rename by mapping a name to itself.
        let effective: HashMap<&str, &str> = self
            .global_properties
            .iter()
            .map(|(canonical, alias)| (canonical.as_str(), alias.as_str()))
            .chain(
                self.class_properties
                    .get(class)
                    .into_iter()
                    .flatten()
                    .map(|(canonical, alias)| (canonical.as_str(), alias.as_str())),
            )
            .collect();

        for (canonical, alias) in &effective {
            if *alias == written {
                return Ok((*canonical).to_string());
            }
        }

        if let Some(alias) = effective.get(written) {
            if *alias != written {
                return Err(format!("{written} was renamed by luaux.toml; use {alias}"));
            }
        }

        if self.property_casing != Casing::Pascal {
            let mut matches: Vec<&str> = roblox::properties(class)
                .chain(roblox::events(class))
                .filter(|member| !effective.contains_key(*member))
                .filter(|member| self.property_casing.apply(member) == written)
                .collect();

            if let Some(member) = preferred(&mut matches) {
                return Ok(member.to_string());
            }

            if roblox::has_property(class, written) || roblox::is_event(class, written) {
                return Err(format!(
                    "{written} was renamed by [properties] all; use {}",
                    self.property_casing.apply(written)
                ));
            }
        }

        Ok(written.to_string())
    }
}

/// Trims a `[factory]` value and rejects an empty one.
///
/// Every key in the table names a Luau expression, and an empty string names
/// nothing. Caught by key so the message says which setting is blank rather
/// than failing later as a parse error on `local _ = { [] = {} }`.
fn factory_value<'a>(key: &str, value: &'a str) -> Result<&'a str, ConfigError> {
    let value = value.trim();

    if value.is_empty() {
        return Err(ConfigError {
            message: format!("luaux.toml: [factory] {key} cannot be empty"),
        });
    }

    // `--` opens a Luau comment, and every key is checked by parsing a probe
    // built around its value. A value ending in `) --` closes the probe's own
    // parenthesis and comments away the rest of it, so the probe parses and the
    // value reaches emission anyway — surfacing as luaux's "this is a luaux bug,
    // please report it", which is the one thing that check exists to prevent.
    if value.contains("--") {
        return Err(ConfigError {
            message: format!("luaux.toml: [factory] {key} cannot contain a comment"),
        });
    }

    // A newline survives trimming and would be emitted verbatim, shifting every
    // line below it and silently costing the file its line-for-line mapping.
    if value.contains(['\n', '\r']) {
        return Err(ConfigError {
            message: format!("luaux.toml: [factory] {key} has to be one line"),
        });
    }

    Ok(value)
}

/// Rejects a `[factory]` value that will not lower to Luau, checked in the
/// shape it is *emitted* into.
///
/// The shapes are not interchangeable, which is why each caller supplies its
/// own probe rather than this asking "is it an expression?". `scope:New` is a
/// legal factory and not a legal expression on its own; `React.Event.` is a
/// legal event key and not a legal anything on its own.
///
/// Same reasoning as [`validate_create`]: without this, a typo in `luaux.toml`
/// surfaces from [`crate::compile_verified`] as luaux's own "please report it"
/// internal error, which sends someone to the issue tracker over their config.
fn validate_factory(key: &str, value: &str, probe: &str) -> Result<(), ConfigError> {
    // `..` is Luau's concatenation operator, so `React..Event` *parses* — as a
    // string, which is not a table key anyone meant. Caught by name first, for
    // the same reason `create` catches it.
    if value.contains("..") {
        return Err(ConfigError {
            message: format!(
                "luaux.toml: [factory] {key} = \"{value}\" has a `.` with no name beside it"
            ),
        });
    }

    if full_moon::parse_fallible(probe, full_moon::LuaVersion::luau())
        .into_result()
        .is_err()
    {
        return Err(ConfigError {
            message: format!(
                "luaux.toml: [factory] {key} = \"{value}\" is not something luaux can emit; \
                 it lowers to `{}`",
                probe.trim_start_matches("local _ = ")
            ),
        });
    }

    Ok(())
}

/// Rejects a `[factory] use` that is not a plain identifier.
///
/// It is a *binding* — the parameter of the `compute` callback — not an
/// expression, so a dotted or called value cannot work. A keyword parses as a
/// keyword and would silently change what the callback means.
fn validate_use(name: &str) -> Result<(), ConfigError> {
    const KEYWORDS: &[&str] = &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];

    let reject = |reason: &str| {
        Err(ConfigError {
            message: format!(
                "luaux.toml: [factory] use = \"{name}\" {reason}; it names the reader inside \
                 the compute callback, so it has to be a plain identifier"
            ),
        })
    };

    let mut characters = name.chars();
    let valid = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_');

    if !valid {
        return reject("is not an identifier");
    }

    if KEYWORDS.contains(&name) {
        return reject("is a Luau keyword");
    }

    Ok(())
}

/// Rejects a `[factory] create` that will not lower to Luau.
///
/// Checked in the shape it is *emitted* into — `create("Frame")({})` — rather
/// than as an expression on its own, because `scope:New` is a legal factory and
/// not a legal expression: Luau requires a `:` call's arguments to follow
/// immediately. Checking the call shape also keeps the curried and indexed
/// forms that already worked — `vide.create()`, `ui.factories[1]` — so this
/// rejects nothing that used to compile.
///
/// Checked here because the alternative is not a wrong error but a misdirected
/// one. A factory that does not lower to Luau reaches the backend, emits, and
/// comes back out of [`crate::compile_verified`] as *"internal error: the table
/// backend emitted invalid Luau — this is a luaux bug; please report it"*,
/// which sends someone to the issue tracker over a typo in their own config.
///
/// Not everything wrong is catchable here, and it does not need to be.
/// `(vide.create)` lowers perfectly well and is still refused — by the in-scope
/// check in [`crate::imports`], which is the other half of the pair. This asks
/// whether the value lowers to Luau; that asks whether it names something the
/// file can reach.
fn validate_create(create: &str) -> Result<(), ConfigError> {
    let reject = |reason: &str| {
        Err(ConfigError {
            message: format!(
                "luaux.toml: [factory] create = \"{create}\" {reason}; it has to name a \
                 function, as create, vide.create, or scope:New"
            ),
        })
    };

    // `..` is Luau's concatenation operator, so `vide..create` *parses* — as a
    // string, which is not callable. The probe below would wave it through and
    // the typo would survive to runtime, so it is caught by name first.
    if create.contains("..") {
        return reject("has a `.` with no name beside it");
    }

    // Parenthesised so the value has to be *one* expression. Bare, `vide create`
    // parses as two statements — `local _ = vide`, then a call — because Luau
    // needs no separator between them. The probe would pass, and the emitted
    // `local e = vide create("Frame")({})` would split the same way and quietly
    // build nothing.
    let probe = format!("local _ = ({create}(\"Frame\")({{}}))");

    if full_moon::parse_fallible(&probe, full_moon::LuaVersion::luau())
        .into_result()
        .is_err()
    {
        return reject("is not something luaux can call");
    }

    Ok(())
}

fn suggest_class(name: &str) -> String {
    match roblox::closest_class(name) {
        Some(class) => format!("; did you mean {class}?"),
        None => String::new(),
    }
}

fn suggest_member(class: &str, name: &str) -> String {
    match roblox::closest_members(class, name).as_slice() {
        [] => String::new(),
        [one] => format!("; did you mean {one}?"),
        [rest @ .., last] => format!("; did you mean {} or {last}?", rest.join(", ")),
    }
}

fn suggest_member_anywhere(name: &str) -> String {
    match roblox::closest_member_anywhere(name) {
        Some(member) => format!("; did you mean {member}?"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        Config::parse(text).expect("parse")
    }

    fn parse_err(text: &str) -> String {
        Config::parse(text).expect_err("should fail").message
    }

    #[test]
    fn lints_default_to_warn_and_are_configurable() {
        // The zero-config default is React, where the lint's premise is false
        // and it defaults off. Under the one-table arrangement it warns.
        assert_eq!(
            parse("[factory]\nbackend = \"table\"\n").static_conditional_child,
            LintLevel::Warn
        );
        assert_eq!(
            parse("[lints]\nstatic_conditional_child = \"off\"\n").static_conditional_child,
            LintLevel::Off
        );
        assert_eq!(
            parse("[lints]\nstatic_conditional_child = \"error\"\n").static_conditional_child,
            LintLevel::Error
        );
    }

    #[test]
    fn rejects_an_unknown_lint_level() {
        let error = parse_err("[lints]\nstatic_conditional_child = \"loud\"\n");
        assert!(error.contains("not one of off, warn, error"), "{error}");
    }

    #[test]
    fn the_element_factory_defaults_to_bare_create() {
        assert_eq!(parse("").create, DEFAULT_CREATE);
        assert_eq!(
            parse("[factory]\nbackend = \"table\"\ncreate = \"vide.create\"\n").create,
            "vide.create"
        );
    }

    #[test]
    fn rejects_an_empty_factory() {
        assert!(parse_err("[factory]\nbackend = \"table\"\ncreate = \"\"\n")
            .contains("cannot be empty"));
        assert!(
            parse_err("[factory]\nbackend = \"table\"\ncreate = \"   \"\n")
                .contains("cannot be empty")
        );
    }

    /// Anything that lowers to a call is accepted, not only a dotted name. The
    /// curried and indexed forms compiled before this check existed, so they
    /// have to keep compiling.
    #[test]
    fn accepts_every_factory_that_lowers_to_a_call() {
        for create in [
            "create",
            "vide.create",
            "scope:New",
            "a.b.c",
            "_v1.x2",
            "vide.create()",
            "ui.factories[1]",
        ] {
            let config = parse(&format!(
                "[factory]\nbackend = \"table\"\ncreate = \"{create}\"\n"
            ));
            assert_eq!(config.create, create);

            // Acceptance has to mean the emitted call parses, not merely that
            // the string survived the config.
            let probe = format!("local _ = ({create}(\"Frame\")({{}}))");
            assert!(
                full_moon::parse_fallible(&probe, full_moon::LuaVersion::luau())
                    .into_result()
                    .is_ok(),
                "{create}"
            );
        }
    }

    /// Each of these used to reach the backend, emit, and come back out of
    /// `compile_verified` as "internal error … this is a luaux bug; please
    /// report it" — sending someone to the issue tracker over their own typo.
    #[test]
    fn rejects_a_factory_that_will_not_lower_to_luau() {
        for (create, reason) in [
            ("vide..create", "with no name beside it"),
            ("vide.", "not something luaux can call"),
            (".create", "not something luaux can call"),
            ("create(", "not something luaux can call"),
            // Two statements, not one expression: `local _ = vide` then a call.
            ("vide create", "not something luaux can call"),
            ("1 + 1", "not something luaux can call"),
            // A colon has to be the last separator: `a:b` must be followed by
            // its arguments, so `a:b.c` has no callable spelling.
            ("scope:New:Now", "not something luaux can call"),
            ("scope:New.Now", "not something luaux can call"),
            ("scope:", "not something luaux can call"),
            // Reserved words are not field names — `t.end` is a syntax error.
            ("end", "not something luaux can call"),
            ("a.end", "not something luaux can call"),
        ] {
            let error = parse_err(&format!(
                "[factory]\nbackend = \"table\"\ncreate = \"{create}\"\n"
            ));
            assert!(error.contains("[factory] create"), "{create}: {error}");
            // The reason, not just the fixed wrapper `reject` puts around every
            // one — otherwise a single blanket message would satisfy the lot.
            assert!(error.contains(reason), "{create}: {error}");
        }
    }

    /// The half this check deliberately does not cover, recorded so the split
    /// stays visible: `(vide.create)` lowers to perfectly good Luau, and it is
    /// the in-scope check in `imports` that refuses it — for `(vide`, which is
    /// not a binding.
    #[test]
    fn a_factory_that_lowers_but_names_nothing_is_left_to_the_scope_check() {
        assert_eq!(
            parse("[factory]\nbackend = \"table\"\ncreate = \"(vide.create)\"\n").create,
            "(vide.create)"
        );
    }

    #[test]
    fn a_factory_is_trimmed() {
        // Otherwise the stray bytes reach both the in-scope check and the
        // emitted call.
        assert_eq!(
            parse("[factory]\nbackend = \"table\"\ncreate = \" vide.create \"\n").create,
            "vide.create"
        );
    }

    #[test]
    fn reads_build_paths_and_selection() {
        let config = parse(
            "[build]\nin = \"src\"\nout = \"build\"\ninclude = [\"**\"]\nexclude = [\"**/*.spec.luaux\"]\nclean = true\n",
        );

        assert_eq!(config.build.input.unwrap().to_str(), Some("src"));
        assert_eq!(config.build.output.unwrap().to_str(), Some("build"));
        assert_eq!(config.build.exclude, ["**/*.spec.luaux"]);
        assert!(config.build.clean);
    }

    #[test]
    fn build_defaults_are_inert() {
        let config = parse("");
        assert!(config.build.input.is_none());
        assert!(config.build.output.is_none());
        assert!(!config.build.clean);
        // Everything is considered unless narrowed.
        assert_eq!(config.build.include, ["**"]);
    }

    #[test]
    fn casing_splits_on_canonical_word_boundaries() {
        // Boundaries come from the PascalCase spelling, so an acronym stays
        // whole: UICorner is UI + Corner, never U + I + Corner.
        for (name, snake, camel, flat) in [
            ("TextLabel", "text_label", "textLabel", "textlabel"),
            ("UICorner", "ui_corner", "uiCorner", "uicorner"),
            (
                "UIAspectRatioConstraint",
                "ui_aspect_ratio_constraint",
                "uiAspectRatioConstraint",
                "uiaspectratioconstraint",
            ),
            (
                "BackgroundColor3",
                "background_color3",
                "backgroundColor3",
                "backgroundcolor3",
            ),
            ("Frame", "frame", "frame", "frame"),
        ] {
            assert_eq!(Casing::Snake.apply(name), snake, "{name}");
            assert_eq!(Casing::Camel.apply(name), camel, "{name}");
            assert_eq!(Casing::Flat.apply(name), flat, "{name}");
            assert_eq!(Casing::Pascal.apply(name), name, "{name}");
        }
    }

    #[test]
    fn casing_is_injective_over_every_class() {
        // If two classes collapsed onto one spelling, `all` would be ambiguous
        // for elements. Measured at zero; this guards against an API dump that
        // introduces one.
        for casing in [Casing::Snake, Casing::Camel, Casing::Flat] {
            let mut seen: HashMap<String, &str> = HashMap::new();
            for class in roblox::creatable_classes() {
                if let Some(other) = seen.insert(casing.apply(class), class) {
                    panic!("{casing:?}: {other} and {class} collide");
                }
            }
        }
    }

    #[test]
    fn an_explicit_entry_beats_the_blanket_scheme() {
        let config = parse("[elements]\nall = \"camelCase\"\nTextLabel = \"text\"\n");

        // The override wins, and takes its class out of the scheme entirely.
        assert_eq!(config.resolve_element("text").unwrap(), Some("TextLabel"));
        assert!(config.resolve_element("textLabel").unwrap().is_none());

        // Everything it did not claim still follows `all`.
        assert_eq!(config.resolve_element("frame").unwrap(), Some("Frame"));
        assert_eq!(
            config.resolve_element("uiCorner").unwrap(),
            Some("UICorner")
        );
    }

    #[test]
    fn a_blanket_scheme_retires_the_canonical_spelling() {
        let config = parse("[elements]\nall = \"snake_case\"\n");
        let error = config.resolve_element("Frame").expect_err("retired");
        assert!(error.contains("use <frame>"), "{error}");
    }

    #[test]
    fn a_collision_prefers_the_name_roblox_has_not_deprecated() {
        // ChildAdded and childAdded are both on Instance, so this pair is
        // inherited by every class (docs/adr/0001-casing-key.md).
        let config = parse("[properties]\nall = \"snake_case\"\n");
        assert_eq!(
            config.resolve_property("Frame", "child_added").unwrap(),
            "ChildAdded"
        );

        let camel = parse("[properties]\nall = \"camelCase\"\n");
        assert_eq!(
            camel.resolve_property("Part", "brickColor").unwrap(),
            "BrickColor"
        );
    }

    #[test]
    fn a_collision_with_no_undeprecated_name_is_still_deterministic() {
        // FormFactor and formFactor are both deprecated, so the tiebreak is byte
        // order, which puts the uppercase spelling first.
        let config = parse("[properties]\nall = \"camelCase\"\n");
        let resolved = config.resolve_property("Part", "formFactor");
        assert_eq!(resolved.unwrap(), "FormFactor");
    }

    #[test]
    fn properties_follow_their_own_scheme_and_overrides() {
        let config = parse("[properties]\nall = \"snake_case\"\nBackgroundColor3 = \"bg\"\n");
        assert_eq!(
            config
                .resolve_property("Frame", "background_transparency")
                .unwrap(),
            "BackgroundTransparency"
        );
        assert_eq!(
            config.resolve_property("Frame", "bg").unwrap(),
            "BackgroundColor3"
        );
        // The override removed it from the scheme.
        assert_eq!(
            config
                .resolve_property("Frame", "background_color3")
                .unwrap(),
            "background_color3"
        );
    }

    #[test]
    fn rejects_an_unknown_casing() {
        assert!(parse_err("[elements]\nall = \"kebab-case\"\n").contains("PascalCase"));
        assert!(parse_err("[properties]\nall = \"KEBAB\"\n").contains("snake_case"));
    }

    #[test]
    fn a_genuinely_unknown_section_still_errors() {
        // So a typo is caught rather than silently ignored.
        assert!(parse_err("[buld]\nclean = true\n").contains("unknown field"));
    }

    #[test]
    fn an_absent_config_is_not_an_error() {
        let config = Config::load(Path::new("/nonexistent-directory-for-luaux"));
        assert!(config.is_ok());
    }

    #[test]
    fn resolves_element_aliases() {
        let config = parse("[elements]\nTextLabel = \"text\"\n");
        assert_eq!(config.resolve_element("text"), Ok(Some("TextLabel")));
        // Unaliased names pass through untouched.
        assert_eq!(config.resolve_element("Frame"), Ok(None));
    }

    #[test]
    fn an_override_retires_the_original_name() {
        let config = parse("[elements]\nTextLabel = \"text\"\n");
        let error = config.resolve_element("TextLabel").expect_err("retired");
        assert!(error.contains("use <text>"), "{error}");
    }

    #[test]
    fn rejects_duplicate_element_aliases() {
        let error = parse_err("[elements]\nTextLabel = \"text\"\nTextButton = \"text\"\n");
        assert!(error.contains("both claim the alias"), "{error}");
    }

    #[test]
    fn rejects_unknown_element_keys() {
        let error = parse_err("[elements]\nFrmae = \"frame\"\n");
        assert!(error.contains("not a creatable Roblox class"), "{error}");
        assert!(error.contains("did you mean Frame?"), "{error}");
    }

    #[test]
    fn resolves_global_property_aliases() {
        let config = parse("[properties]\nTextColor3 = \"textColor\"\n");
        assert_eq!(
            config.resolve_property("TextLabel", "textColor"),
            Ok("TextColor3".into())
        );
        // And retires the original spelling.
        assert!(config.resolve_property("TextLabel", "TextColor3").is_err());
    }

    #[test]
    fn per_class_entries_beat_the_global_table() {
        let config = parse(
            "[properties]\nBackgroundColor3 = \"bg\"\n\n[properties.Frame]\nBackgroundColor3 = \"bgColor\"\n",
        );
        assert_eq!(
            config.resolve_property("Frame", "bgColor"),
            Ok("BackgroundColor3".into())
        );
        // The global alias no longer applies to Frame...
        assert_eq!(config.resolve_property("Frame", "bg"), Ok("bg".into()));
        // ...but still applies elsewhere.
        assert_eq!(
            config.resolve_property("TextLabel", "bg"),
            Ok("BackgroundColor3".into())
        );
    }

    #[test]
    fn a_class_can_opt_out_of_a_global_rename() {
        // PROPOSAL.md's identity-override trick.
        let config = parse(
            "[properties]\nTextColor3 = \"textColor\"\n\n[properties.TextLabel]\nTextColor3 = \"TextColor3\"\n",
        );
        assert_eq!(
            config.resolve_property("TextLabel", "TextColor3"),
            Ok("TextColor3".into())
        );
        assert_eq!(
            config.resolve_property("TextButton", "textColor"),
            Ok("TextColor3".into())
        );
    }

    /// Every spelling offered must be one that resolves, or a project's
    /// completions would suggest the errors its own config created.
    #[test]
    fn the_offered_spelling_is_the_one_that_resolves() {
        let config = parse(
            "[elements]\nTextLabel = \"text\"\n\n[properties]\nTextColor3 = \"textColor\"\n\n\
             [properties.Frame]\nBackgroundColor3 = \"bgColor\"\n",
        );

        assert_eq!(config.element_name("TextLabel"), "text");
        assert_eq!(config.element_name("Frame"), "Frame");
        assert_eq!(
            config.resolve_element(config.element_name("TextLabel")),
            Ok(Some("TextLabel"))
        );

        assert_eq!(config.property_name("TextLabel", "TextColor3"), "textColor");
        assert_eq!(config.property_name("Frame", "BackgroundColor3"), "bgColor");
        // Untouched names are spelled as themselves.
        assert_eq!(config.property_name("Frame", "Name"), "Name");

        for (class, canonical) in [("TextLabel", "TextColor3"), ("Frame", "BackgroundColor3")] {
            assert_eq!(
                config.resolve_property(class, config.property_name(class, canonical)),
                Ok(canonical.to_string()),
                "{class}.{canonical}"
            );
        }
    }

    #[test]
    fn an_identity_override_is_offered_as_the_original_name() {
        let config = parse(
            "[properties]\nTextColor3 = \"textColor\"\n\n[properties.TextLabel]\nTextColor3 = \"TextColor3\"\n",
        );

        assert_eq!(
            config.property_name("TextLabel", "TextColor3"),
            "TextColor3"
        );
        assert_eq!(
            config.property_name("TextButton", "TextColor3"),
            "textColor"
        );
    }

    #[test]
    fn rejects_unknown_property_keys() {
        let error = parse_err("[properties]\nTextColour3 = \"textColor\"\n");
        assert!(error.contains("not a property or event of any"), "{error}");
        assert!(error.contains("TextColor3"), "{error}");
    }

    #[test]
    fn rejects_per_class_keys_the_class_does_not_have() {
        let error = parse_err("[properties.Frame]\nText = \"label\"\n");
        assert!(
            error.contains("has no property or event named Text"),
            "{error}"
        );
    }

    #[test]
    fn rejects_unknown_per_class_tables() {
        let error = parse_err("[properties.Frmae]\nName = \"id\"\n");
        assert!(error.contains("not a creatable Roblox class"), "{error}");
    }

    /// The `[factory]` variables beyond `create` (factory-plan.md §3,
    /// backend-plan.md §5.3).
    mod factory {
        use super::*;

        #[test]
        /// Writing a `[factory]` block turns every assumption off. That is the
        /// rule that makes the React default safe: a project that configures
        /// anything is configuring everything that matters.
        fn a_factory_block_assumes_nothing() {
            let config = parse("[factory]\nbackend = \"table\"\n");

            assert_eq!(config.create, BARE_CREATE);
            assert_eq!(config.children, None);
            assert_eq!(config.event, None);
            assert_eq!(config.compute, None);
            assert_eq!(config.use_fn, None);
            assert_eq!(config.fragment, None);
            assert_eq!(config.interpolate, Interpolate::Wrap);
        }

        #[test]
        fn children_round_trips() {
            assert_eq!(
                parse("[factory]\nbackend = \"table\"\nchildren = \"Children\"\n")
                    .children
                    .as_deref(),
                Some("Children")
            );
        }

        /// Fusion's spelling: the expression is *called* with the event name.
        #[test]
        fn an_event_without_a_trailing_dot_is_called() {
            let config = parse("[factory]\nbackend = \"table\"\nevent = \"OnEvent\"\n");

            assert_eq!(config.event, Some(EventKey::Call("OnEvent".to_string())));
            assert_eq!(
                config.event.expect("event").key("Activated"),
                "[OnEvent(\"Activated\")]"
            );
        }

        /// React's spelling: `React.Event.Activated` is a field access, not a
        /// call, and a trailing dot is how that is already written in Luau.
        #[test]
        fn an_event_with_a_trailing_dot_is_indexed() {
            let config = parse("[factory]\nbackend = \"table\"\nevent = \"React.Event.\"\n");

            assert_eq!(
                config.event,
                Some(EventKey::Index("React.Event".to_string()))
            );
            assert_eq!(
                config.event.expect("event").key("Activated"),
                "[React.Event.Activated]"
            );
        }

        /// The root of the *expression*, not of the spelling — the trailing dot
        /// is syntax for which form to emit and is not part of the name the
        /// file has to have in scope.
        #[test]
        fn an_indexed_event_exposes_its_expression_without_the_dot() {
            let config = parse("[factory]\nbackend = \"table\"\nevent = \"React.Event.\"\n");

            assert_eq!(config.event.expect("event").expression(), "React.Event");
        }

        #[test]
        fn compute_round_trips() {
            assert_eq!(
                parse("[factory]\nbackend = \"table\"\ncompute = \"scope:Computed\"\n")
                    .compute
                    .as_deref(),
                Some("scope:Computed")
            );
        }

        /// Inert without `compute`, and *reported* rather than silently kept.
        /// A value nothing reads is a question about the config, and
        /// `parse_reporting` is the channel built for exactly that.
        #[test]
        fn use_without_compute_is_reported_as_inert() {
            let (config, warnings) =
                Config::parse_reporting("[factory]\nbackend = \"table\"\nuse = \"peek\"\n")
                    .expect("config");

            assert_eq!(config.use_fn.as_deref(), None);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(warnings[0].contains("does nothing without"), "{warnings:?}");
        }

        /// A malformed `use` is rejected whether or not anything reads it.
        /// Staying quiet would make the key look accepted right up until
        /// `compute` is added and the callback stops parsing.
        #[test]
        fn an_inert_use_is_still_checked() {
            assert!(
                parse_err("[factory]\nbackend = \"table\"\nuse = \"end\"\n").contains("keyword")
            );
        }

        /// `use` names the reader inside `compute`'s callback, so it is inert
        /// without one — and resolving the default here means the backend never
        /// has to ask whether the name means anything.
        #[test]
        fn use_resolves_only_alongside_compute() {
            assert_eq!(
                parse("[factory]\nbackend = \"table\"\nuse = \"peek\"\n")
                    .use_fn
                    .as_deref(),
                None
            );

            assert_eq!(
                parse("[factory]\nbackend = \"table\"\ncompute = \"c\"\n")
                    .use_fn
                    .as_deref(),
                Some(DEFAULT_USE)
            );

            assert_eq!(
                parse("[factory]\nbackend = \"table\"\ncompute = \"c\"\nuse = \"peek\"\n")
                    .use_fn
                    .as_deref(),
                Some("peek")
            );
        }

        #[test]
        fn every_key_rejects_an_empty_value() {
            for key in ["children", "event", "compute", "use"] {
                let error = parse_err(&format!("[factory]\nbackend = \"table\"\n{key} = \"\"\n"));
                assert!(error.contains("cannot be empty"), "{key}: {error}");
                assert!(error.contains(key), "{key}: {error}");
            }
        }

        /// Each value is checked in the shape it is *emitted* into, because the
        /// shapes are not interchangeable. Without this a typo surfaces from
        /// `compile_verified` as luaux's own "please report it" internal error.
        #[test]
        fn each_key_is_checked_in_its_own_emission_shape() {
            assert!(
                parse_err("[factory]\nbackend = \"table\"\nchildren = \"Chil dren\"\n")
                    .contains("children")
            );
            assert!(
                parse_err("[factory]\nbackend = \"table\"\nevent = \"On Event\"\n")
                    .contains("event")
            );
            assert!(
                parse_err("[factory]\nbackend = \"table\"\ncompute = \"a b c\"\n")
                    .contains("compute")
            );
        }

        /// `..` is Luau's concatenation operator, so `React..Event` parses — as
        /// a string, which is not a table key anyone meant. Caught by name
        /// before the probe waves it through.
        #[test]
        fn a_doubled_dot_is_caught_by_name() {
            assert!(
                parse_err("[factory]\nbackend = \"table\"\nchildren = \"a..b\"\n")
                    .contains("has a `.` with no name beside it")
            );
        }

        /// A bare dot selects the index form and leaves nothing to index with.
        #[test]
        fn an_event_that_is_only_a_dot_is_rejected() {
            assert!(parse_err("[factory]\nbackend = \"table\"\nevent = \".\"\n")
                .contains("has a `.` with no name beside it"));
        }

        /// `use` is a *binding* — the callback's parameter — not an expression,
        /// so a dotted or called value cannot work, and a keyword would silently
        /// change what the callback means.
        #[test]
        fn use_has_to_be_a_plain_identifier() {
            assert!(
                parse_err("[factory]\nbackend = \"table\"\ncompute = \"c\"\nuse = \"end\"\n")
                    .contains("keyword")
            );
            assert!(parse_err(
                "[factory]\nbackend = \"table\"\ncompute = \"c\"\nuse = \"scope.use\"\n"
            )
            .contains("not an identifier"));
            assert!(
                parse_err("[factory]\nbackend = \"table\"\ncompute = \"c\"\nuse = \"2nd\"\n")
                    .contains("not an identifier")
            );
        }

        /// Accepted spellings, so the check does not reject something ordinary.
        #[test]
        fn accepts_the_shapes_the_libraries_actually_use() {
            parse("[factory]\nbackend = \"table\"\ncreate = \"scope:New\"\nchildren = \"Children\"\nevent = \"OnEvent\"\ncompute = \"scope:Computed\"\n");
            parse("[factory]\nbackend = \"table\"\ncreate = \"React.createElement\"\nevent = \"React.Event.\"\n");
            parse("[factory]\nbackend = \"table\"\nchildren = \"Fusion.Children\"\n");
        }
    }

    /// Backend selection and the keys only the element arrangement uses
    /// (backend-plan.md §5.1, §5.2, §5.4, §5.5).
    mod backend {
        use super::*;

        const REACT: &str = "[factory]\nbackend = \"element\"\nfragment = \"React.Fragment\"\n";

        #[test]
        /// With no `[factory]` block at all, luaux picks React — the one place
        /// it names a library.
        fn the_zero_config_default_is_react() {
            let config = parse("");

            assert_eq!(config.backend, BackendKind::Element);
            assert_eq!(config.create, "React.createElement");
            assert_eq!(config.fragment.as_deref(), Some("React.Fragment"));
            assert_eq!(config.interpolate, Interpolate::Plain);
            assert_eq!(
                config.event.expect("event").key("Activated"),
                "[React.Event.Activated]"
            );
        }

        /// The rule that keeps the flipped default from ever being silent. A
        /// Vide project that set only `create` would otherwise inherit React's
        /// arrangement and pass the in-scope check, because its own name really
        /// is in scope — and emit the wrong shape without a word.
        #[test]
        fn a_factory_block_has_to_name_its_backend() {
            let error = parse_err("[factory]\ncreate = \"vide.create\"\n");

            assert!(error.contains("needs a backend"), "{error}");
            assert!(error.contains("element"), "{error}");
        }

        #[test]
        fn backend_round_trips() {
            assert_eq!(
                parse("[factory]\nbackend = \"table\"\n").backend,
                BackendKind::Table
            );
            assert_eq!(parse(REACT).backend, BackendKind::Element);
        }

        #[test]
        fn an_unknown_backend_lists_the_two() {
            let error = parse_err("[factory]\nbackend = \"react\"\n");
            assert!(error.contains("table, element"), "{error}");
        }

        #[test]
        fn interpolate_round_trips() {
            assert_eq!(
                parse("[factory]\nbackend = \"table\"\ninterpolate = \"plain\"\n").interpolate,
                Interpolate::Plain
            );
            assert!(
                parse_err("[factory]\nbackend = \"table\"\ninterpolate = \"none\"\n")
                    .contains("wrap, plain")
            );
        }

        /// A bare table is not an element, and there is nothing sensible to
        /// guess — so this is refused at load rather than at the first fragment.
        #[test]
        fn the_element_backend_needs_a_fragment() {
            let error = parse_err("[factory]\nbackend = \"element\"\n");
            assert!(error.contains("needs a fragment"), "{error}");
        }

        /// `children` is a *table key*, and this arrangement has no table to put
        /// one in. Rejected rather than ignored: a key that silently does
        /// nothing is the hardest kind to debug, because the output looks
        /// deliberate (backend-plan.md §5.5).
        #[test]
        fn the_element_backend_rejects_a_children_key() {
            let error = parse_err(&format!("{REACT}children = \"Children\"\n"));
            assert!(error.contains("passes children as an argument"), "{error}");
        }

        /// The one pair of `[factory]` keys that steer the same decision, and
        /// so the one pair that can contradict each other. Before this rule the
        /// emission resolved it by dropping the wrapper, which handed a Fusion
        /// project text that silently never updated — from a config that named
        /// the very thing meant to update it.
        #[test]
        fn plain_interpolation_and_a_compute_wrapper_contradict() {
            let error = parse_err(
                "[factory]\nbackend = \"table\"\ncompute = \"scope:Computed\"\n\
                 interpolate = \"plain\"\n",
            );

            assert!(error.contains("nothing for compute to wrap"), "{error}");
        }

        /// Either alone is ordinary: `plain` is React's, `compute` is Fusion's.
        #[test]
        fn either_alone_is_fine() {
            parse("[factory]\nbackend = \"table\"\ncompute = \"scope:Computed\"\n");
            parse("[factory]\nbackend = \"table\"\ninterpolate = \"plain\"\n");
        }

        /// A fragment is a plain table under this arrangement and the key is
        /// never read, so setting it would hand a project the fragment it always
        /// had from a config naming something else. The mirror of the `children`
        /// rule, and the same reason.
        #[test]
        fn the_table_backend_rejects_a_fragment() {
            let error =
                parse_err("[factory]\nbackend = \"table\"\nfragment = \"Fusion.Fragment\"\n");

            assert!(
                error.contains("only read by the element backend"),
                "{error}"
            );
        }

        /// Modes follow the arrangement; names do not. A React block that names
        /// its backend and omits `interpolate` must not inherit the one-table
        /// default — that hands React a *function* as the Text prop, plus an
        /// inlined reader, from a config that never asked for either.
        #[test]
        fn modes_follow_the_named_backend() {
            let react = parse(
                "[factory]\nbackend = \"element\"\ncreate = \"React.createElement\"\n\
                 fragment = \"React.Fragment\"\n",
            );

            assert_eq!(react.interpolate, Interpolate::Plain);
            assert_eq!(react.static_conditional_child, LintLevel::Off);

            let table = parse("[factory]\nbackend = \"table\"\n");
            assert_eq!(table.interpolate, Interpolate::Wrap);
            assert_eq!(table.static_conditional_child, LintLevel::Warn);
        }

        /// The two constructors are the two ways a config comes into existence,
        /// and every field one leaves to `..Self::default()` is a React value.
        /// The lint level reached `bare()` that way once.
        #[test]
        fn the_two_constructors_do_not_leak_into_each_other() {
            let bare = Config::bare();
            assert_eq!(bare.backend, BackendKind::Table);
            assert_eq!(bare.interpolate, Interpolate::Wrap);
            assert_eq!(bare.static_conditional_child, LintLevel::Warn);
            assert_eq!(bare.fragment, None);
            assert_eq!(bare.event, None);
            assert_eq!(bare.create, BARE_CREATE);

            let react = Config::default();
            assert_eq!(react.backend, BackendKind::Element);
            assert_eq!(react.interpolate, Interpolate::Plain);
            assert_eq!(react.static_conditional_child, LintLevel::Off);
        }

        /// A project with no `luaux.toml` at all is the arrangement the React
        /// default exists to serve, and it never passes through `parse`. The
        /// lint was defaulted off in the parsed path only, so that project got
        /// React *and* a warning telling it to wrap a conditional child in a
        /// function — advice that makes React render the function.
        #[test]
        fn the_no_file_path_gets_the_same_modes_as_the_parsed_one() {
            let absent = Config::default();
            let empty = parse("");

            assert_eq!(absent.backend, empty.backend);
            assert_eq!(absent.interpolate, empty.interpolate);
            assert_eq!(
                absent.static_conditional_child,
                empty.static_conditional_child
            );
        }

        /// Every key is checked by parsing a probe built around its value, and
        /// `--` opens a Luau comment. A value ending in `) --` closed the probe's
        /// own parenthesis and commented away the rest, so it parsed and reached
        /// emission anyway — surfacing as luaux's "please report it" internal
        /// error, which is the one thing the check exists to prevent.
        #[test]
        fn a_factory_value_cannot_comment_out_its_own_probe() {
            for key in [
                "create", "children", "event", "compute", "fragment", "merge",
            ] {
                let error = parse_err(&format!(
                    "[factory]\nbackend = \"table\"\n{key} = \"a) --\"\n"
                ));

                assert!(error.contains("cannot contain a comment"), "{key}: {error}");
            }
        }

        /// A newline survives trimming and is emitted verbatim, shifting every
        /// line below it — the file keeps compiling and quietly stops lining up
        /// with its source.
        #[test]
        fn a_factory_value_has_to_be_one_line() {
            let error = parse_err("[factory]\nbackend = \"table\"\ncreate = \"vide.\\ncreate\"\n");
            assert!(error.contains("has to be one line"), "{error}");
        }

        #[test]
        fn fragment_is_checked_in_its_emission_shape() {
            assert!(
                parse_err("[factory]\nbackend = \"element\"\nfragment = \"a b\"\n")
                    .contains("fragment")
            );
        }

        /// The lint's premise — a child built once can never update — is false
        /// where the component body re-runs. Left on, it would call the single
        /// most idiomatic thing in JSX a mistake.
        #[test]
        fn the_static_child_lint_defaults_off_under_the_element_backend() {
            assert_eq!(parse(REACT).static_conditional_child, LintLevel::Off);
            assert_eq!(
                parse("[factory]\nbackend = \"table\"\n").static_conditional_child,
                LintLevel::Warn
            );
        }

        /// Defaulted off, not removed. A project that wants the check can ask.
        #[test]
        fn an_explicit_lint_level_still_wins() {
            let config = parse(&format!(
                "{REACT}\n[lints]\nstatic_conditional_child = \"error\"\n"
            ));

            assert_eq!(config.static_conditional_child, LintLevel::Error);
        }

        /// The block a React project would actually write.
        #[test]
        fn accepts_the_react_shape() {
            let config = parse(
                "[factory]\n\
                 backend = \"element\"\n\
                 create = \"React.createElement\"\n\
                 event = \"React.Event.\"\n\
                 fragment = \"React.Fragment\"\n\
                 interpolate = \"plain\"\n",
            );

            assert_eq!(config.backend, BackendKind::Element);
            assert_eq!(config.create, "React.createElement");
            assert_eq!(config.fragment.as_deref(), Some("React.Fragment"));
            assert_eq!(config.interpolate, Interpolate::Plain);
            assert_eq!(
                config.event.expect("event").key("Activated"),
                "[React.Event.Activated]"
            );
        }
    }
}
