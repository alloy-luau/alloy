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

/// The `[lint]` table: which lints `alloy lint` runs, and at what level.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LintConfig {
    /// Turns the strict-only lints on: `implicit_any` and
    /// `missing_return_type`.
    pub strict: bool,
    /// Lints that fail the run.
    pub deny: Vec<String>,
    /// Lints that print and pass.
    pub warn: Vec<String>,
    /// Lints that stay silent.
    pub allow: Vec<String>,
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

[lint]
# turn on the strict-only lints: implicit_any, missing_return_type
strict = false
# lints that fail `alloy lint`; `alloy doc lints` names them all
deny = []
warn = []
allow = []

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
