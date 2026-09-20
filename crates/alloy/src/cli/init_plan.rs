//! The answers of `alloy init --interactive`, and the files they write.
//!
//! The prompts are a thin shell over `plan`: the wizard fills an
//! `Answers`, `plan` turns it into the files, the folders, and the
//! closing summary, and the command writes them. Nothing is written
//! until every question is answered, so a cancelled wizard writes
//! nothing.

use std::path::{Path, PathBuf};

use alloy::config;

/// What the project is. A game lands in a DataModel; a package is
/// required by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Game,
    Package,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kind::Game => f.write_str("game: client, server, and shared folders with mounts"),

            Kind::Package => f.write_str("package: one src folder, no mounts"),
        }
    }
}

/// The package manager the project uses. Alloy runs none of them and
/// depends on none of them; it writes the manifest it can and names the
/// command the author runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Manager {
    Ember,
    Pesde,
    Wally,
    None,
}

impl std::fmt::Display for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Manager::Ember => f.write_str("ember: writes ember.toml with the wally index"),

            Manager::Pesde => f.write_str("pesde: run `pesde init` yourself"),

            Manager::Wally => f.write_str("wally: run `wally init` yourself"),

            Manager::None => f.write_str("none: no manifest"),
        }
    }
}

/// The test runner. `@test` writes specs either way; a runner executes
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Runner {
    Lest,
    None,
}

impl std::fmt::Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Runner::Lest => f.write_str("lest: `alloy test --run` runs the specs"),

            Runner::None => f.write_str("none: the specs are written and nothing runs them"),
        }
    }
}

/// `anneal`: soft. The formatter's own layout, every lint off but the
/// ones that name broken code, and nothing pedantic.
pub(crate) const ANNEAL: &str = "\
[fmt]
recommended = true

[lint]
recommended = false
strict = false

[lint.rules]
correctness = \"deny\"
";

/// `temper`: balanced. What `alloy init` writes today.
pub(crate) const TEMPER: &str = "\
[fmt]
recommended = true
column_width = 100
indent_type = \"spaces\"
indent_width = 4
quote_style = \"auto-prefer-double\"

[lint]
recommended = true
strict = true

[lint.rules]
# raw_require = \"allow\"
";

/// `forge`: hard. The narrowest layout the `[fmt]` keys allow, the
/// pedantic group on, and both documentation lints on.
pub(crate) const FORGE: &str = "\
[fmt]
recommended = true
column_width = 80
indent_type = \"spaces\"
indent_width = 4
quote_style = \"force-double\"
leading_zero = \"add\"
call_parentheses = \"always\"
collapse_simple_statement = \"never\"
trailing_comma = true
align_struct_fields = true
expand_imports = true

[fmt.call_chains]
style = \"method\"
min_calls = 3

[fmt.sort_requires]
enabled = true
grouping = \"by-kind\"

[lint]
recommended = true
strict = true

[lint.rules]
correctness = \"deny\"
naming = \"warn\"
missing_doc = \"warn\"
missing_doc_type = \"warn\"
";

/// One style preset: the name the wizard shows, the `[fmt]` and
/// `[lint]` tables it writes, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preset {
    pub name: String,
    /// The TOML the preset splices into `alloy.toml`.
    pub toml: String,
    /// One line for the list: what the preset is for.
    pub about: String,
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.about.is_empty() {
            f.write_str(&self.name)
        } else {
            write!(f, "{}: {}", self.name, self.about)
        }
    }
}

impl Preset {
    fn built_in(name: &str, about: &str, toml: &str) -> Self {
        Self {
            name: name.to_string(),
            toml: toml.to_string(),
            about: about.to_string(),
        }
    }
}

/// The three built-in presets, in order from soft to hard. `temper` is
/// the one the wizard preselects.
pub(crate) fn built_in_presets() -> Vec<Preset> {
    vec![
        Preset::built_in("anneal", "soft; for a port or a spike", ANNEAL),
        temper(),
        Preset::built_in("forge", "hard; strict and pedantic", FORGE),
    ]
}

/// The preset the wizard preselects, and the one the recommended setup
/// writes.
pub(crate) fn temper() -> Preset {
    Preset::built_in("temper", "balanced; what alloy init writes", TEMPER)
}

/// The directory of the author's own presets: `~/.alloy/presets`,
/// beside the toolchain `alloy self install` writes.
pub(crate) fn user_preset_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".alloy").join("presets"))
}

/// The directory of the project's own presets.
pub(crate) fn project_preset_dir(root: &Path) -> PathBuf {
    root.join(".alloy").join("presets")
}

/// One preset file. The file name is the name shown, and the text is
/// the `[fmt]` and `[lint]` tables. A file that holds another table is
/// refused: the wizard splices the text into `alloy.toml`, and a second
/// `[build]` there would not parse.
fn read_preset(path: &Path) -> Option<Preset> {
    let text = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;

    if !table.keys().all(|k| k == "fmt" || k == "lint") {
        return None;
    }

    // The tables of a preset are `alloy.toml` tables, so the project
    // reader is the one that checks the values.
    config::Config::parse(&text, path).ok()?;

    Some(Preset {
        name: path.file_stem()?.to_str()?.to_string(),
        toml: format!("{}\n", text.trim_end()),
        about: String::new(),
    })
}

/// Every `.toml` under a preset directory, by name.
fn presets_in(dir: &Path) -> Vec<Preset> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Preset> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| read_preset(&p))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));

    out
}

/// The presets the wizard lists: the three built in, then the author's
/// own from `~/.alloy/presets` and the project's `.alloy/presets`. A
/// file that takes a built-in name replaces that preset, so a team can
/// redefine `temper` and still see one entry.
pub(crate) fn presets(root: &Path) -> Vec<Preset> {
    let mut out = built_in_presets();
    let found = user_preset_dir()
        .map(|d| presets_in(&d))
        .unwrap_or_default()
        .into_iter()
        .chain(presets_in(&project_preset_dir(root)));

    for preset in found {
        match out.iter().position(|p| p.name == preset.name) {
            Some(i) => out[i] = preset,

            None => out.push(preset),
        }
    }

    out
}

/// Every answer of the wizard. One struct so `plan` is a pure function
/// of it and a test can drive every combination.
#[derive(Debug, Clone)]
pub(crate) struct Answers {
    pub kind: Kind,
    /// `[project] name`, the name in the project files Alloy writes.
    pub name: String,
    pub manager: Manager,
    pub runner: Runner,
    pub preset: Preset,
    /// Strict mode in the Luau configuration the wizard writes.
    pub strict: bool,
    /// The folder already has a `.config.luau` or a `.luaurc`, so the
    /// wizard edits that file instead of writing one.
    pub has_luau_config: bool,
    /// The author took the recommended setup and answered nothing else.
    /// The report then names every answer, so nothing is guessed at.
    pub recommended: bool,
}

impl Answers {
    /// The recommended setup: a game, since that is the common case on
    /// Roblox, ember, lest, `temper`, and strict mode. The first confirm
    /// of the wizard writes exactly this, and so does `--yes`.
    pub fn recommended(name: &str, has_luau_config: bool) -> Self {
        Self {
            kind: Kind::Game,
            name: name.to_string(),
            manager: Manager::Ember,
            runner: Runner::Lest,
            preset: temper(),
            strict: true,
            has_luau_config,
            recommended: true,
        }
    }
}

/// What the wizard writes, and what it says afterwards.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Folders to create, before the files.
    pub dirs: Vec<PathBuf>,
    /// Files to write, each path relative to the project root.
    pub files: Vec<(PathBuf, String)>,
    /// Lines of the closing report, after the written files.
    pub notes: Vec<String>,
    /// The commands the author runs next, in order.
    pub next: Vec<String>,
    /// The last line of the report, under the next command.
    pub last: Option<String>,
}

/// The `[mount]` table of a game: the three folders and where each one
/// lands in the DataModel.
const GAME_MOUNTS: &str = "\
[mount]
client = [\"src/client\", \"@game/StarterPlayer/StarterPlayerScripts/Client\"]
server = [\"src/server\", \"@game/ServerScriptService/Server\"]
shared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]
";

/// The `ember.toml` the wizard writes, in the shape of the manifest in
/// `examples/test`. The manifest carries no dependency yet; `embr add`
/// writes the first one.
const EMBER_TOML: &str = "\
# Manifest for this project. See https://luaupm.com/docs
# Add dependencies with `embr add <scope/name>`, then run `embr install`.

[dependencies]

[indices]
wally = \"https://github.com/UpliftGames/wally-index\"
";

/// The `alloy.toml` of one set of answers. The preset supplies the
/// `[fmt]` and `[lint]` tables; every other table holds a default, as
/// `config::TEMPLATE` does, so the file says what the project can
/// change.
fn alloy_toml(a: &Answers) -> String {
    let game = a.kind == Kind::Game;
    let mut out = String::from(
        "#:schema .alloy/alloy.schema.json\n\
         [build]\n\
         in = \"src\"\n\
         out = \"build\"\n\
         exclude = []\n\
         clean = false\n\
         artifact = \"ship\"\n\
         \n\
         [emit]\n\
         # wait_timeout = 5\n\
         # std_require = \"@alloy\"\n\
         # erase_type_imports = false\n\
         \n",
    );
    out.push_str(a.preset.toml.trim_end());
    out.push_str("\n\n[flux]\ntypecheck = true\ndefinitions = []\n\n[test]\nout = \"tests\"\nsuite = \"alloy\"\n");
    out.push_str(if a.runner == Runner::Lest {
        "lest = true\n"
    } else {
        "lest = false\n"
    });
    out.push_str("shim = true\n\n[project]\n");
    out.push_str(&format!("name = \"{}\"\n", a.name.replace('"', "\\\"")));

    if game {
        out.push_str("sourcemap = true\nsource_of_truth = true\nmount_aliases = true\n\n");
        out.push_str(GAME_MOUNTS);
    } else {
        // A package has no DataModel tree of its own, so Alloy writes
        // no project file and no sourcemap for it.
        out.push_str("sourcemap = false\nsource_of_truth = false\n");
    }

    out
}

/// The `.gitignore` of a game: the build output and the sourcemap.
/// `.alloy` carries its own, which the build writes.
fn gitignore(a: &Answers) -> String {
    let mut out = String::from("# Written by `alloy init`.\nbuild/\nsourcemap.json\n");

    if a.manager == Manager::Ember {
        out.push_str("packages/\n.ember-patch/\n");
    }

    out
}

/// The files, the folders, and the report of one set of answers.
pub(crate) fn plan(a: &Answers) -> Plan {
    let mut plan = Plan::default();

    plan.files
        .push((PathBuf::from(config::FILE_NAME), alloy_toml(a)));

    match a.kind {
        Kind::Game => {
            for folder in ["client", "server", "shared"] {
                plan.dirs.push(PathBuf::from("src").join(folder));
            }

            plan.files.push((PathBuf::from(".gitignore"), gitignore(a)));
        }

        Kind::Package => plan.dirs.push(PathBuf::from("src")),
    }

    // A folder that already has a Luau configuration keeps it; the
    // command adds the mode and the alias it lacks.
    if !a.has_luau_config {
        plan.files
            .push((PathBuf::from(".config.luau"), config_luau(a.strict)));
    }

    match a.manager {
        Manager::Ember => {
            plan.files
                .push((PathBuf::from("ember.toml"), EMBER_TOML.to_string()));
            plan.next.push("embr add <scope/name>".to_string());
        }

        // Alloy does not know the shape of either manifest, so it
        // writes none: the manager's own command writes one that is
        // right.
        Manager::Pesde => {
            plan.notes
                .push("pesde writes its own manifest; alloy init writes none".to_string());
            plan.next.push("pesde init".to_string());
        }

        Manager::Wally => {
            plan.notes
                .push("wally writes its own manifest; alloy init writes none".to_string());
            plan.next.push("wally init".to_string());
        }

        Manager::None => {}
    }

    if a.runner == Runner::None {
        plan.notes.push(
            "`[test] lest = false`: `alloy test` still writes a spec per @test, and no runner runs them"
                .to_string(),
        );
    }

    plan.notes.push(format!("style preset `{}`", a.preset.name));

    // The fast path answered every question, so the report names what it
    // answered rather than leaving the author to guess.
    if a.recommended {
        plan.notes.push(
            "the recommended setup answered every question: a game, ember, lest, the `temper` preset, and strict mode"
                .to_string(),
        );
        plan.last = Some(
            "this made a game; run `alloy init --interactive` again and answer no to make a package"
                .to_string(),
        );
    }

    plan.next.push("alloy build".to_string());

    plan
}

/// The `.config.luau` of a fresh folder. Strict mode is the default;
/// the wizard writes `nonstrict` when the author asks for it, which a
/// port needs.
pub(crate) fn config_luau(strict: bool) -> String {
    if strict {
        config::CONFIG_LUAU_TEMPLATE.to_string()
    } else {
        config::CONFIG_LUAU_TEMPLATE.replace("\"strict\"", "\"nonstrict\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(kind: Kind, manager: Manager, runner: Runner, preset: &str) -> Answers {
        let preset = built_in_presets()
            .into_iter()
            .find(|p| p.name == preset)
            .expect("a built-in preset");

        Answers {
            kind,
            name: "game".to_string(),
            manager,
            runner,
            preset,
            strict: true,
            has_luau_config: false,
            recommended: false,
        }
    }

    fn written(plan: &Plan, name: &str) -> Option<String> {
        plan.files
            .iter()
            .find(|(p, _)| p == Path::new(name))
            .map(|(_, text)| text.clone())
    }

    /// Every preset, in every kind, assembles an `alloy.toml` the
    /// reader accepts. A preset that names a key wrong would pass the
    /// wizard and fail the next command.
    #[test]
    fn every_preset_writes_a_file_the_reader_accepts() {
        for preset in built_in_presets() {
            for kind in [Kind::Game, Kind::Package] {
                let a = answers(kind, Manager::None, Runner::Lest, &preset.name);
                let text = written(&plan(&a), "alloy.toml").expect("alloy.toml");
                let config = config::Config::parse(&text, Path::new("alloy.toml"))
                    .unwrap_or_else(|e| panic!("{} / {kind:?}: {e}", preset.name));

                assert!(
                    config.unknown_rules().is_empty(),
                    "{:?}",
                    config.unknown_rules()
                );
                assert!(
                    config.deprecations().is_empty(),
                    "{:?}",
                    config.deprecations()
                );
            }
        }
    }

    /// The three presets set the two tables, and nothing else.
    #[test]
    fn the_presets_differ_where_they_say_they_do() {
        let of = |name: &str| {
            let a = answers(Kind::Package, Manager::None, Runner::Lest, name);
            let text = written(&plan(&a), "alloy.toml").expect("alloy.toml");

            config::Config::parse(&text, Path::new("alloy.toml")).expect("the config")
        };

        // anneal: the formatter's own layout, every lint off but the
        // group that names broken code.
        let anneal = of("anneal");
        assert!(anneal.fmt.recommended);
        assert_eq!(anneal.fmt.column_width, 100);
        assert!(!anneal.lint.recommended);
        assert!(!anneal.lint.strict);
        assert_eq!(
            anneal.lint.rules.get("correctness"),
            Some(&alloy::lint::Level::Deny)
        );

        // temper: what the non-interactive path writes.
        let temper = of("temper");
        assert_eq!(temper.fmt, config::FmtConfig::default());
        assert_eq!(temper.lint, config::LintConfig::default());

        // forge: the narrowest layout, and both documentation lints.
        let forge = of("forge");
        assert_eq!(forge.fmt.column_width, 80);
        assert_eq!(forge.fmt.quote_style, config::QuoteStyle::ForceDouble);
        assert!(forge.fmt.align_struct_fields);
        assert!(forge.fmt.expand_imports);
        assert!(forge.fmt.sort_requires.enabled);
        assert_eq!(
            forge.fmt.sort_requires.grouping,
            config::RequireGrouping::ByKind
        );
        assert_eq!(forge.fmt.call_chains.style, config::CallChainStyle::Method);
        assert!(forge.lint.strict);

        for lint in ["missing_doc", "missing_doc_type", "naming"] {
            assert!(forge.lint.rules.contains_key(lint), "{lint} is missing");
        }
    }

    /// A game gets the three folders, the mounts, and a `.gitignore`.
    #[test]
    fn a_game_mounts_three_folders() {
        let plan = plan(&answers(Kind::Game, Manager::None, Runner::Lest, "temper"));

        assert_eq!(
            plan.dirs,
            vec![
                PathBuf::from("src/client"),
                PathBuf::from("src/server"),
                PathBuf::from("src/shared"),
            ]
        );

        let text = written(&plan, "alloy.toml").expect("alloy.toml");
        let config = config::Config::parse(&text, Path::new("alloy.toml")).expect("the config");

        assert_eq!(
            config.mount["client"],
            config::Mount(
                "src/client".into(),
                "@game/StarterPlayer/StarterPlayerScripts/Client".into()
            )
        );
        assert_eq!(
            config.mount["server"],
            config::Mount(
                "src/server".into(),
                "@game/ServerScriptService/Server".into()
            )
        );
        assert_eq!(
            config.mount["shared"],
            config::Mount("src/shared".into(), "@game/ReplicatedStorage/Shared".into())
        );
        assert!(config.project.sourcemap);
        assert!(config.project.source_of_truth);

        let ignore = written(&plan, ".gitignore").expect(".gitignore");
        assert!(ignore.contains("build/\n"));
        assert!(ignore.contains("sourcemap.json\n"));
        assert!(!ignore.contains("packages/"));
    }

    /// A package gets one folder, no mounts, and the `[project]` keys
    /// that say Alloy describes no DataModel tree for it.
    #[test]
    fn a_package_writes_no_mounts() {
        let plan = plan(&answers(
            Kind::Package,
            Manager::None,
            Runner::Lest,
            "temper",
        ));

        assert_eq!(plan.dirs, vec![PathBuf::from("src")]);
        assert!(written(&plan, ".gitignore").is_none());

        let text = written(&plan, "alloy.toml").expect("alloy.toml");
        let config = config::Config::parse(&text, Path::new("alloy.toml")).expect("the config");

        assert!(config.mount.is_empty());
        assert!(!config.project.sourcemap);
        assert!(!config.project.source_of_truth);
    }

    /// The manager writes what its shape is known for, and names the
    /// command the author runs next.
    #[test]
    fn each_manager_writes_what_it_can() {
        let of = |manager| plan(&answers(Kind::Package, manager, Runner::Lest, "temper"));

        let ember = of(Manager::Ember);
        let manifest = written(&ember, "ember.toml").expect("ember.toml");
        assert!(manifest.contains("[dependencies]"));
        assert!(manifest.contains("wally = \"https://github.com/UpliftGames/wally-index\""));
        assert!(toml::from_str::<toml::Table>(&manifest).is_ok());
        assert_eq!(
            ember.next,
            vec![
                "embr add <scope/name>".to_string(),
                "alloy build".to_string()
            ]
        );

        // Neither shape is known here, so neither writes a manifest.
        for (manager, command) in [
            (Manager::Pesde, "pesde init"),
            (Manager::Wally, "wally init"),
        ] {
            let p = of(manager);
            assert_eq!(p.files.len(), 2, "{manager:?} wrote a manifest");
            assert!(p.next.iter().any(|c| c == command), "{:?}", p.next);
            assert!(
                p.notes
                    .iter()
                    .any(|n| n.contains("writes its own manifest")),
                "{:?}",
                p.notes
            );
        }

        let none = of(Manager::None);
        assert_eq!(none.files.len(), 2);
        assert_eq!(none.next, vec!["alloy build".to_string()]);

        // A game with ember also ignores the folder embr installs into.
        let ignore = written(
            &plan(&answers(Kind::Game, Manager::Ember, Runner::Lest, "temper")),
            ".gitignore",
        )
        .expect(".gitignore");
        assert!(ignore.contains("packages/\n"));
        assert!(ignore.contains(".ember-patch/\n"));
    }

    /// With no runner, `[test] lest` is off and the summary says the
    /// specs are still written.
    #[test]
    fn no_runner_still_writes_the_specs() {
        let p = plan(&answers(
            Kind::Package,
            Manager::None,
            Runner::None,
            "temper",
        ));
        let text = written(&p, "alloy.toml").expect("alloy.toml");
        let config = config::Config::parse(&text, Path::new("alloy.toml")).expect("the config");

        assert!(!config.test.lest);
        assert!(
            p.notes.iter().any(|n| n.contains("no runner runs them")),
            "{:?}",
            p.notes
        );

        let on = plan(&answers(
            Kind::Package,
            Manager::None,
            Runner::Lest,
            "temper",
        ));
        let text = written(&on, "alloy.toml").expect("alloy.toml");
        assert!(
            config::Config::parse(&text, Path::new("alloy.toml"))
                .expect("the config")
                .test
                .lest
        );
    }

    /// The mode answer reaches the Luau configuration, and a folder
    /// that has one already gets no second file.
    #[test]
    fn the_mode_answer_writes_the_luau_configuration() {
        let mut a = answers(Kind::Package, Manager::None, Runner::Lest, "temper");
        assert_eq!(
            written(&plan(&a), ".config.luau").as_deref(),
            Some(config::CONFIG_LUAU_TEMPLATE)
        );

        a.strict = false;
        let text = written(&plan(&a), ".config.luau").expect(".config.luau");
        let read = alloy::luau_config::parse_config_luau(&text).expect("a table");
        assert_eq!(read.language_mode.as_deref(), Some("nonstrict"));
        assert_eq!(
            read.aliases,
            vec![("alloy".to_string(), "./build/alloy".to_string())]
        );

        a.has_luau_config = true;
        assert!(written(&plan(&a), ".config.luau").is_none());
    }

    /// The recommended setup is a game with ember, lest, and `temper`,
    /// and the report names every answer and the way to a package.
    #[test]
    fn the_recommended_setup_names_what_it_answered() {
        let a = Answers::recommended("starfall", false);

        assert_eq!(a.kind, Kind::Game);
        assert_eq!(a.manager, Manager::Ember);
        assert_eq!(a.runner, Runner::Lest);
        assert_eq!(a.preset.name, "temper");
        assert!(a.strict);

        let p = plan(&a);
        let text = written(&p, "alloy.toml").expect("alloy.toml");
        let config = config::Config::parse(&text, Path::new("alloy.toml")).expect("the config");

        assert_eq!(config.project.name, "starfall");
        assert_eq!(config.fmt, config::FmtConfig::default());
        assert_eq!(config.lint, config::LintConfig::default());
        assert_eq!(config.mount.len(), 3);
        assert!(config.test.lest);
        assert!(written(&p, "ember.toml").is_some());
        assert!(written(&p, ".gitignore").is_some());

        assert!(
            p.notes.iter().any(|n| n.contains("a game, ember, lest")),
            "{:?}",
            p.notes
        );
        assert!(
            p.last
                .as_deref()
                .is_some_and(|l| l.contains("answer no to make a package")),
            "{:?}",
            p.last
        );

        // The full question list says nothing about a recommendation.
        let asked = plan(&answers(Kind::Game, Manager::Ember, Runner::Lest, "temper"));
        assert!(asked.last.is_none());
        assert!(!asked.notes.iter().any(|n| n.contains("recommended setup")));
    }

    /// A preset file is named by its file name and holds the two
    /// tables. A file that holds another table, or a value the reader
    /// refuses, is not a preset.
    #[test]
    fn a_preset_file_holds_the_two_tables_alone() {
        let dir = std::env::temp_dir().join(format!("alloy-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder");

        std::fs::write(dir.join("house.toml"), "[fmt]\ncolumn_width = 72\n").expect("the file");
        std::fs::write(dir.join("wide.toml"), "[build]\nout = \"dist\"\n").expect("the file");
        std::fs::write(dir.join("bad.toml"), "[fmt]\nquote_style = \"nope\"\n").expect("the file");
        std::fs::write(dir.join("notes.md"), "[fmt]\n").expect("the file");
        // A file that takes a built-in name replaces it.
        std::fs::write(dir.join("temper.toml"), "[fmt]\ncolumn_width = 60\n").expect("the file");

        let found = presets_in(&dir);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["house", "temper"], "{names:?}");
        assert_eq!(found[0].toml, "[fmt]\ncolumn_width = 72\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The list is the three built in, then the project's own. A file
    /// that takes a built-in name replaces that preset in place, so
    /// `temper` stays the second entry the wizard preselects.
    #[test]
    fn a_project_preset_joins_the_built_in_three() {
        let root = std::env::temp_dir().join(format!("alloy-preset-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(project_preset_dir(&root)).expect("the folder");
        std::fs::write(
            project_preset_dir(&root).join("house.toml"),
            "[fmt]\ncolumn_width = 72\n",
        )
        .expect("the file");
        std::fs::write(
            project_preset_dir(&root).join("temper.toml"),
            "[fmt]\ncolumn_width = 60\n",
        )
        .expect("the file");

        let list = presets(&root);
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();

        assert_eq!(
            names,
            vec!["anneal", "temper", "forge", "house"],
            "{names:?}"
        );
        assert_eq!(list[1].toml, "[fmt]\ncolumn_width = 60\n");

        let _ = std::fs::remove_dir_all(&root);
    }
}
