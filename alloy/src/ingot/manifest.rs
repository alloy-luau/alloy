//! `ingot.toml`: what an ingot declares about itself.
//!
//! The manifest sits beside the binary. It names the ingot, the protocol
//! revision, the hooks the host may send, the options with their
//! defaults, and the lints with their levels. Validation refuses a
//! manifest at load, with a sentence, rather than at the first file.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// The protocol revision the host speaks; see `alloy_ingot::API`.
pub const API: u32 = 1;

/// The file name the host looks for.
pub const FILE_NAME: &str = "ingot.toml";

/// The hooks an ingot may declare. The host sends an operation only when
/// the manifest lists it, so an ingot pays for nothing it does not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hook {
    /// Edits to the Alloy source before the desugar.
    Transform,
    /// Edits to the ship Luau after the desugar.
    Output,
    Lint,
    Format,
    Hover,
    Complete,
    Actions,
    /// The colors a file names, and the labels for a picked color.
    Colors,
}

impl Hook {
    pub const ALL: &[Hook] = &[
        Hook::Transform,
        Hook::Output,
        Hook::Lint,
        Hook::Format,
        Hook::Hover,
        Hook::Complete,
        Hook::Actions,
        Hook::Colors,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Hook::Transform => "transform",
            Hook::Output => "output",
            Hook::Lint => "lint",
            Hook::Format => "format",
            Hook::Hover => "hover",
            Hook::Complete => "complete",
            Hook::Actions => "actions",
            Hook::Colors => "colors",
        }
    }
}

/// The pass an ingot's transform runs in. A word reads better than a
/// number in a manifest; the number is for an ingot that must sit
/// between two others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Run {
    Word(RunWord),
    Number(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunWord {
    /// Before every ingot that says nothing.
    First,
    /// After every ingot that says nothing.
    Last,
}

impl Run {
    pub fn order(self) -> i64 {
        match self {
            Run::Word(RunWord::First) => -100,
            Run::Word(RunWord::Last) => 100,
            Run::Number(n) => n,
        }
    }
}

/// One lint the ingot declares.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintDecl {
    /// `allow`, `warn`, or `deny`.
    #[serde(default = "default_level")]
    pub default: String,
    pub summary: String,
    #[serde(default)]
    pub detail: String,
}

fn default_level() -> String {
    "warn".to_string()
}

/// One prop the ingot reads on a markup tag. The editor completes the
/// name on a tag of a file kind the ingot wants and shows `doc` beside
/// it. A bare string is the doc.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum PropDecl {
    Doc(String),
    Table {
        #[serde(default)]
        doc: String,
        /// The snippet the editor inserts, `ClassName="$1"` for a prop
        /// that holds a string. Unset inserts `name={$1}`, the form
        /// every other prop takes.
        #[serde(default)]
        insert: Option<String>,
    },
}

impl PropDecl {
    pub fn doc(&self) -> &str {
        match self {
            PropDecl::Doc(d) => d,
            PropDecl::Table { doc, .. } => doc,
        }
    }

    /// The snippet the item inserts; empty means the `name={$1}` form.
    pub fn insert(&self) -> &str {
        match self {
            PropDecl::Table {
                insert: Some(i), ..
            } => i,

            _ => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The name; it must equal the key under `[ingots]`.
    pub name: String,
    pub api: u32,
    #[serde(default)]
    pub description: String,
    /// The executable, relative to the manifest. Unset means
    /// `<name>-ingot` with the platform's suffix.
    #[serde(default)]
    pub binary: Option<String>,
    /// The pass the transform runs in.
    #[serde(default)]
    pub run: Option<Run>,
    /// The file kinds the ingot wants: `aly`, `alx`, `d.aly`. Empty means all.
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
    /// The options with their defaults; `[ingot.<name>]` overrides them.
    /// An option may be written as `{ default = ..., doc = "..." }`; the
    /// parse keeps the default here and the doc in `option_docs`.
    #[serde(default)]
    pub options: toml::Table,
    /// What each option is for, for the editor's completion.
    #[serde(skip)]
    pub option_docs: BTreeMap<String, String>,
    #[serde(default)]
    pub lints: BTreeMap<String, LintDecl>,
    /// The props the ingot reads on a markup tag, `ClassName` for a
    /// styling ingot. Neither the class nor the component declares
    /// them, so the editor takes the names from here.
    #[serde(default)]
    pub props: BTreeMap<String, PropDecl>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        // The full report names the line and the column, the way
        // `alloy.toml` reports; `message()` alone drops them.
        let mut m: Manifest = toml::from_str(text).map_err(|e| e.to_string())?;

        for (key, value) in m.options.iter_mut() {
            if let toml::Value::Table(t) = value
                && let Some(default) = t.get("default").cloned()
            {
                if let Some(doc) = t.get("doc").and_then(|d| d.as_str()) {
                    m.option_docs.insert(key.clone(), doc.to_string());
                }

                *value = default;
            }
        }

        m.validate()?;

        Ok(m)
    }

    pub fn load(path: &Path) -> Result<Manifest, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

        Manifest::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn validate(&self) -> Result<(), String> {
        if self.api != API {
            return Err(format!(
                "`{}` speaks api {}, this alloy speaks api {API}",
                self.name, self.api
            ));
        }

        if self.name.is_empty()
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "the name `{}` may hold letters, digits, `_`, and `-` only",
                self.name
            ));
        }

        if self.hooks.is_empty() {
            return Err(format!("`{}` declares no hooks", self.name));
        }

        if self.run.is_some() && !self.hooks.contains(&Hook::Transform) {
            return Err(format!(
                "`{}` sets `run` and declares no transform hook to order",
                self.name
            ));
        }

        if !self.lints.is_empty() && !self.hooks.contains(&Hook::Lint) {
            return Err(format!("`{}` declares lints and no lint hook", self.name));
        }

        for kind in &self.kinds {
            if !matches!(kind.as_str(), "aly" | "alx" | "d.aly") {
                return Err(format!(
                    "`{}` wants the kind `{kind}`; the kinds are aly, alx, d.aly",
                    self.name
                ));
            }
        }

        for name in self.props.keys() {
            let mut chars = name.chars();

            if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(format!(
                    "prop `{name}` of `{}`: a prop name is a letter and then letters, digits, or `_`",
                    self.name
                ));
            }
        }

        for (name, lint) in &self.lints {
            if !matches!(lint.default.as_str(), "allow" | "warn" | "deny") {
                return Err(format!(
                    "lint `{name}` of `{}` has the level `{}`; the levels are allow, warn, deny",
                    self.name, lint.default
                ));
            }

            if !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!(
                    "lint `{name}` of `{}`: a lint name is snake_case",
                    self.name
                ));
            }
        }

        Ok(())
    }

    /// The executable's file name.
    pub fn binary_name(&self) -> String {
        match &self.binary {
            Some(b) => b.clone(),

            None if cfg!(windows) => format!("{}-ingot.exe", self.name),

            None => format!("{}-ingot", self.name),
        }
    }

    pub fn has(&self, hook: Hook) -> bool {
        self.hooks.contains(&hook)
    }

    /// Whether the ingot wants a file of this kind.
    pub fn wants(&self, kind: &str) -> bool {
        self.kinds.is_empty() || self.kinds.iter().any(|k| k == kind)
    }
}

/// A manifest for `alloy ingot new`.
pub fn template(name: &str) -> String {
    format!(
        r#"name = "{name}"
api = 1
description = "An Alloy ingot."
# the executable beside this file; unset means `{name}-ingot`
# binary = "target/release/{name}-ingot"
# the pass the transform runs in: "first", "last", or a number
# run = "first"
# the file kinds it wants; unset means all of aly, alx, d.aly
# kinds = ["aly", "alx"]
# the operations the host sends
hooks = ["transform", "lint", "hover"]

[options]
# defaults; `[ingot.{name}]` in alloy.toml overrides them
greeting = "hello"

[lints.example_lint]
default = "warn"
summary = "an example the scaffold ships"
detail = "Replace this lint with your own, or remove it."

# the props the ingot reads on a markup tag; the editor completes them
# [props]
# ClassName = {{ doc = "what it holds", insert = "ClassName=\"$1\"" }}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_parses() {
        let m = Manifest::parse(&template("hello")).unwrap();

        assert_eq!(m.name, "hello");
        assert_eq!(
            m.binary_name(),
            format!("hello-ingot{}", if cfg!(windows) { ".exe" } else { "" })
        );
        assert!(m.has(Hook::Lint));
        assert!(!m.has(Hook::Output));
        assert_eq!(m.options["greeting"].as_str(), Some("hello"));
        assert_eq!(m.lints["example_lint"].default, "warn");
        assert!(m.wants("alx"));
    }

    #[test]
    fn the_api_and_the_hooks_are_checked() {
        let e = Manifest::parse("name = \"x\"\napi = 2\nhooks = [\"lint\"]\n").unwrap_err();
        assert!(e.contains("api 2"), "{e}");

        let e = Manifest::parse("name = \"x\"\napi = 1\n").unwrap_err();
        assert!(e.contains("no hooks"), "{e}");

        let e = Manifest::parse("name = \"x\"\napi = 1\nrun = \"first\"\nhooks = [\"lint\"]\n")
            .unwrap_err();
        assert!(e.contains("no transform hook"), "{e}");

        let e = Manifest::parse(
            "name = \"x\"\napi = 1\nhooks = [\"lint\"]\n[lints.Bad]\nsummary = \"s\"\n",
        )
        .unwrap_err();
        assert!(e.contains("snake_case"), "{e}");
    }

    #[test]
    fn a_prop_declaration_carries_its_doc() {
        let m = Manifest::parse(
            "name = \"x\"\napi = 1\nhooks = [\"complete\"]\n[props]\nClassName = { doc = \"the utility list\" }\nStyle = \"a table of properties\"\n",
        )
        .unwrap();
        assert_eq!(m.props["ClassName"].doc(), "the utility list");
        assert_eq!(m.props["Style"].doc(), "a table of properties");

        let e = Manifest::parse(
            "name = \"x\"\napi = 1\nhooks = [\"complete\"]\n[props]\n\"class-name\" = \"no\"\n",
        )
        .unwrap_err();
        assert!(e.contains("a prop name is a letter"), "{e}");
    }

    #[test]
    fn a_syntax_error_names_its_line_and_column() {
        let e = Manifest::parse("name = \"badingot\napi = 1\n").unwrap_err();
        assert!(e.contains("TOML parse error at line 1, column"), "{e}");
    }

    #[test]
    fn run_words_order_around_the_middle() {
        assert!(Run::Word(RunWord::First).order() < Run::Number(0).order());
        assert!(Run::Number(0).order() < Run::Word(RunWord::Last).order());
    }
}
