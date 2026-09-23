//! `alloy init`: scaffolds `alloy.toml` and a Luau configuration.
//!
//! The command opens the wizard when a terminal runs it. A script, a
//! pipe and a CI job have no terminal, so they get the plain path and
//! read the same behaviour they read today. `--yes` writes the
//! recommended setup with no prompt, and `--non-interactive` forces the
//! plain path over both.

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use alloy::config;

use crate::cli::init_plan::{Answers, Kind, Manager, Runner, plan, presets};
use crate::fail;
use crate::ui::{self, Painter};

/// The paths of `alloy init`. `--non-interactive` is the strongest: it
/// always writes today's two files, so a script that passes it reads
/// one thing whatever else is on the line. With no flag the wizard runs
/// where a terminal can answer it, and the plain path runs where none
/// can, so a pipe or a CI job never waits for an answer.
pub(crate) fn init(args: &[String]) -> ExitCode {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let dir = Path::new(".");

    if has("--non-interactive") || has("-n") {
        return init_at(dir);
    }

    if has("--yes") || has("-y") {
        return recommended_init(dir);
    }

    if has("--interactive") || has("-i") || std::io::stdin().is_terminal() {
        return wizard_init(dir);
    }

    init_at(dir)
}

/// The name the wizard offers for `[project] name`: the folder's own.
fn folder_name(dir: &Path) -> String {
    std::path::absolute(dir)
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "game".to_string())
}

/// Whether the folder already carries a Luau configuration.
fn has_luau_config(dir: &Path) -> bool {
    dir.join(".config.luau").is_file() || dir.join(".luaurc").is_file()
}

/// `alloy init --yes`: the recommended setup with no prompt. It needs
/// no terminal, so a bootstrap script reaches the same scaffold the
/// wizard's first confirm writes.
fn recommended_init(dir: &Path) -> ExitCode {
    let p = Painter::for_stdout();
    let path = dir.join(config::FILE_NAME);

    if path.exists() {
        fail(&format!("{} already exists", path.display()));

        return ExitCode::FAILURE;
    }

    let answers = Answers::recommended(&folder_name(dir), has_luau_config(dir));

    write_plan(dir, &answers, &p)
}

/// Writes the schema that the `#:schema` line of `alloy.toml` names, so
/// the editor finds it before the first build. The build rewrites it
/// with the ingots' tables.
fn write_schema(dir: &Path, p: &Painter) {
    let path = dir.join(".alloy/alloy.schema.json");

    if path.exists() {
        return;
    }

    let text =
        serde_json::to_string_pretty(&alloy::schema::project(&[])).unwrap_or_default() + "\n";
    let written =
        std::fs::create_dir_all(dir.join(".alloy")).and_then(|()| std::fs::write(&path, text));

    if written.is_ok() {
        println!("{}", p.wrote(".alloy/alloy.schema.json"));
    }
}

/// Writes `alloy.toml` and one Luau configuration. A folder with
/// neither file gets a `.config.luau` with strict mode and the `@alloy`
/// alias. A folder that already has `.config.luau` or `.luaurc` keeps
/// that one file, and gains only the mode and the alias it lacks.
fn init_at(dir: &Path) -> ExitCode {
    let path = dir.join(config::FILE_NAME);

    let p = Painter::for_stdout();

    if path.exists() {
        fail(&format!("{} already exists", path.display()));
        return ExitCode::FAILURE;
    }

    if let Err(e) = std::fs::write(&path, config::TEMPLATE) {
        fail(&format!("cannot write {}: {e}", path.display()));
        return ExitCode::FAILURE;
    }

    println!("{}", p.wrote(&path.display().to_string()));
    write_schema(dir, &p);

    if !init_luau_config(dir, &p) {
        return ExitCode::FAILURE;
    }

    println!(
        "{}",
        p.ok("ready; put sources under src and run `alloy build`")
    );

    ExitCode::SUCCESS
}

/// The Luau configuration of `alloy init`. `.config.luau` wins when the
/// folder has both, as it does for Luau itself.
fn init_luau_config(dir: &Path, p: &Painter) -> bool {
    let luau = dir.join(".config.luau");
    let rc = dir.join(".luaurc");
    let name = if luau.is_file() {
        ".config.luau"
    } else if rc.is_file() {
        ".luaurc"
    } else {
        // Neither file: one new `.config.luau` carries both keys.
        match std::fs::write(&luau, config::CONFIG_LUAU_TEMPLATE) {
            Ok(()) => println!("{}", p.wrote(".config.luau")),

            Err(e) => {
                fail(&format!("cannot write .config.luau: {e}"));

                return false;
            }
        }

        return true;
    };

    let path = dir.join(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,

        Err(e) => {
            fail(&format!("cannot read {name}: {e}"));

            return false;
        }
    };
    let edited = if name == ".luaurc" {
        alloy::luau_config::add_defaults_luaurc(&text).ok()
    } else {
        alloy::luau_config::add_defaults_config_luau(&text)
    };
    let Some((text, added)) = edited else {
        println!(
            "{}",
            p.note(&format!(
                "{name} is not a table this reader understands; add `alloy = \"./build/alloy\"` to its aliases by hand"
            ))
        );

        return true;
    };

    if added.is_empty() {
        println!(
            "{}",
            p.note(&format!("{name} already has strict mode and @alloy"))
        );

        return true;
    }

    match std::fs::write(&path, text) {
        Ok(()) => {
            println!("{}", p.wrote(&format!("{name}: {}", added.join(", "))));

            true
        }

        Err(e) => {
            fail(&format!("cannot write {name}: {e}"));

            false
        }
    }
}

/// The wizard needs a terminal to draw on and keys to read. With stdin
/// on a pipe it does not open at all: a wizard in a script or in CI is
/// a hang. Colour is not required, so `NO_COLOR` still gets the
/// questions, in the plain style the `Painter` falls back to.
fn can_prompt() -> bool {
    std::io::stdin().is_terminal()
}

/// The prompt colors, the `Painter` palette rather than the crate's
/// defaults: the marks of `ok`, `wrote`, `note`, and `fail`, and a bold
/// question.
fn render_config() -> inquire::ui::RenderConfig<'static> {
    use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};

    let rgb = |(r, g, b): (u8, u8, u8)| Color::rgb(r, g, b);
    let lilac = rgb(ui::LILAC);
    let dim = rgb(ui::DIM);

    let mut config = RenderConfig::empty()
        .with_prompt_prefix(Styled::new("?").with_fg(lilac))
        .with_answered_prompt_prefix(Styled::new("\u{2713}").with_fg(lilac))
        .with_highlighted_option_prefix(Styled::new("\u{2192}").with_fg(lilac))
        .with_selected_checkbox(Styled::new("[x]").with_fg(lilac))
        .with_unselected_checkbox(Styled::new("[ ]").with_fg(dim))
        .with_scroll_up_prefix(Styled::new("^").with_fg(dim))
        .with_scroll_down_prefix(Styled::new("v").with_fg(dim))
        .with_canceled_prompt_indicator(Styled::new("cancelled").with_fg(rgb(ui::AMBER)))
        .with_answer(StyleSheet::empty().with_fg(lilac))
        .with_help_message(StyleSheet::empty().with_fg(dim))
        .with_default_value(StyleSheet::empty().with_fg(dim))
        .with_selected_option(Some(StyleSheet::empty().with_fg(lilac)))
        .with_error_message(
            inquire::ui::ErrorMessageRenderConfig::empty()
                .with_prefix(Styled::new("\u{2717}").with_fg(rgb(ui::RED)))
                .with_message(StyleSheet::empty().with_fg(rgb(ui::RED))),
        );
    // The question itself, bold, as `Painter::bold` writes a label.
    config.prompt = StyleSheet::empty().with_attr(Attributes::BOLD);

    config
}

/// One prompt's answer. `None` is a cancel: esc, or ctrl-c, which the
/// crossterm backend hands back rather than raising a signal, so the
/// terminal is restored and the folder stays untouched.
fn answered<T>(r: inquire::error::InquireResult<T>) -> Result<Option<T>, String> {
    match r {
        Ok(value) => Ok(Some(value)),

        Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => Ok(None),

        Err(e) => Err(e.to_string()),
    }
}

const KEYS: &str = "arrows to move, enter to pick, esc to cancel";

/// The wizard. The first question offers the recommended setup, and
/// `yes` there answers every other one. `no` opens the list below, each
/// question already on the recommended answer, so `no` means "let me
/// change some of it". `None` means the author cancelled, and nothing
/// is written.
fn ask(dir: &Path) -> Result<Option<Answers>, String> {
    use inquire::{Confirm, Select, Text};

    let folder = folder_name(dir);
    let luau = has_luau_config(dir);

    let Some(fast) = answered(
        Confirm::new("Use the recommended setup?")
            .with_default(true)
            .with_help_message("a game, ember, lest, the `temper` preset, and strict mode")
            .prompt(),
    )?
    else {
        return Ok(None);
    };

    if fast {
        return Ok(Some(Answers::recommended(&folder, luau)));
    }

    // Every list below starts on the recommended answer, so enter alone
    // walks the same setup the confirm writes.
    let Some(kind) = answered(
        Select::new("What is this project?", vec![Kind::Game, Kind::Package])
            .with_help_message(KEYS)
            .prompt(),
    )?
    else {
        return Ok(None);
    };

    let Some(name) = answered(
        Text::new("Project name?")
            .with_default(&folder)
            .with_help_message(
                "`[project] name`: the name in the project files Alloy writes. `.` is this folder",
            )
            .prompt(),
    )?
    else {
        return Ok(None);
    };
    // `.` is what a reader types for "this folder", the way every other
    // command reads it.
    let name = if name.trim() == "." {
        folder.clone()
    } else {
        name
    };

    let Some(manager) = answered(
        Select::new(
            "Package manager?",
            vec![
                Manager::Ember,
                Manager::Pesde,
                Manager::Wally,
                Manager::None,
            ],
        )
        .with_help_message("alloy runs none of them; it writes the manifest and names the command")
        .prompt(),
    )?
    else {
        return Ok(None);
    };

    let Some(runner) = answered(
        Select::new("Test runner?", vec![Runner::Lest, Runner::None])
            .with_help_message("`[test] lest`; @test writes a spec either way")
            .prompt(),
    )?
    else {
        return Ok(None);
    };

    let list = presets(dir);
    // `temper` is the one preselected; it sits second in the built-in
    // three, and a preset file of that name replaces it in place.
    let start = list.iter().position(|p| p.name == "temper").unwrap_or(0);
    let Some(preset) = answered(
        Select::new("Style preset?", list)
            .with_starting_cursor(start)
            .with_help_message("sets the [fmt] and [lint] tables; `alloy doc init` lists them")
            .prompt(),
    )?
    else {
        return Ok(None);
    };

    let Some(strict) = answered(
        Confirm::new("Strict mode for every file?")
            .with_default(true)
            .with_help_message("`languagemode` in .config.luau; a port often wants nonstrict")
            .prompt(),
    )?
    else {
        return Ok(None);
    };

    let name = match name.trim() {
        "" => folder,

        trimmed => trimmed.to_string(),
    };

    Ok(Some(Answers {
        kind,
        name,
        manager,
        runner,
        preset,
        strict,
        has_luau_config: luau,
        recommended: false,
    }))
}

/// `alloy init --interactive`: the questions, then the files. A folder
/// that already has an `alloy.toml` is refused before the first
/// question, so no answer is asked for nothing.
fn wizard_init(dir: &Path) -> ExitCode {
    let p = Painter::for_stdout();
    let path = dir.join(config::FILE_NAME);

    if path.exists() {
        fail(&format!("{} already exists", path.display()));

        return ExitCode::FAILURE;
    }

    if !can_prompt() {
        fail(
            "the wizard needs a terminal to read; run `alloy init --non-interactive` for the plain path, or `alloy init --yes` for the recommended setup",
        );

        return ExitCode::FAILURE;
    }

    inquire::set_global_render_config(render_config());

    let answers = match ask(dir) {
        Ok(Some(answers)) => answers,

        Ok(None) => {
            println!("{}", p.note("cancelled; nothing written"));

            return ExitCode::FAILURE;
        }

        Err(e) => {
            fail(&e);

            return ExitCode::FAILURE;
        }
    };

    write_plan(dir, &answers, &p)
}

/// Writes what the answers plan, and prints the report the other
/// commands print: a line per file, the notes, then the next command.
fn write_plan(dir: &Path, answers: &Answers, p: &Painter) -> ExitCode {
    let plan = plan(answers);

    for folder in &plan.dirs {
        if let Err(e) = std::fs::create_dir_all(dir.join(folder)) {
            fail(&format!("cannot create {}: {e}", folder.display()));

            return ExitCode::FAILURE;
        }

        println!("{}", p.wrote(&format!("{}/", folder.display())));
    }

    for (rel, text) in &plan.files {
        let target = dir.join(rel);

        // A file the author already wrote stays as it is; only
        // `alloy.toml` is refused outright, and that is checked first.
        if target.exists() {
            println!(
                "{}",
                p.note(&format!(
                    "{} is already there; left as it is",
                    rel.display()
                ))
            );

            continue;
        }

        if let Err(e) = std::fs::write(&target, text) {
            fail(&format!("cannot write {}: {e}", rel.display()));

            return ExitCode::FAILURE;
        }

        println!("{}", p.wrote(&rel.display().to_string()));
    }

    write_schema(dir, p);

    // The folder has a Luau configuration already: it keeps it, and
    // gains the mode and the alias it lacks.
    if answers.has_luau_config && !init_luau_config(dir, p) {
        return ExitCode::FAILURE;
    }

    for note in &plan.notes {
        println!("{}", p.note(note));
    }

    let next: Vec<String> = plan.next.iter().map(|c| format!("`{c}`")).collect();
    println!("{}", p.ok(&format!("ready; run {}", next.join(", then "))));

    if let Some(line) = &plan.last {
        println!("{}", p.note(line));
    }

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alloy-init-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder");

        dir
    }

    /// `alloy init` in each of the three states a folder can be in.
    #[test]
    fn init_writes_one_luau_configuration() {
        let p = Painter::for_stdout();

        // Neither file: a new `.config.luau`, and no `.luaurc`.
        let bare = temp("bare");
        assert!(init_luau_config(&bare, &p));
        assert!(!bare.join(".luaurc").exists());
        let written = std::fs::read_to_string(bare.join(".config.luau")).expect("the file");
        assert_eq!(written, config::CONFIG_LUAU_TEMPLATE);

        // A `.luaurc` alone: the mode and the alias join it, the other
        // keys stay, and no `.config.luau` is written.
        let rc = temp("luaurc");
        std::fs::write(
            rc.join(".luaurc"),
            "{ \"aliases\": { \"pkg\": \"Packages\" } }\n",
        )
        .expect("the file");
        assert!(init_luau_config(&rc, &p));
        assert!(!rc.join(".config.luau").exists());
        let read = alloy::luau_config::parse_luaurc(
            &std::fs::read_to_string(rc.join(".luaurc")).expect("the file"),
        )
        .expect("a table");
        assert_eq!(read.language_mode.as_deref(), Some("strict"));
        assert_eq!(
            read.aliases,
            vec![
                ("alloy".to_string(), "./build/alloy".to_string()),
                ("pkg".to_string(), "Packages".to_string()),
            ]
        );

        // A `.config.luau` alone: the same, in place.
        let luau = temp("config-luau");
        std::fs::write(
            luau.join(".config.luau"),
            "return {\n    luau = {\n        languagemode = \"nonstrict\",\n    },\n}\n",
        )
        .expect("the file");
        assert!(init_luau_config(&luau, &p));
        assert!(!luau.join(".luaurc").exists());
        let read = alloy::luau_config::parse_config_luau(
            &std::fs::read_to_string(luau.join(".config.luau")).expect("the file"),
        )
        .expect("a table");
        // The user's own mode is never rewritten.
        assert_eq!(read.language_mode.as_deref(), Some("nonstrict"));
        assert_eq!(
            read.aliases,
            vec![("alloy".to_string(), "./build/alloy".to_string())]
        );

        // A second run adds nothing.
        let before = std::fs::read_to_string(luau.join(".config.luau")).expect("the file");
        assert!(init_luau_config(&luau, &p));
        assert_eq!(
            std::fs::read_to_string(luau.join(".config.luau")).expect("the file"),
            before
        );

        for dir in [bare, rc, luau] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// With both files, `.config.luau` is the one edited, as Luau reads
    /// it first.
    #[test]
    fn init_edits_the_file_luau_reads_first() {
        let dir = temp("both");
        std::fs::write(dir.join(".luaurc"), "{}\n").expect("the file");
        std::fs::write(dir.join(".config.luau"), "return { luau = {} }\n").expect("the file");
        assert!(init_luau_config(&dir, &Painter::for_stdout()));

        assert_eq!(
            std::fs::read_to_string(dir.join(".luaurc")).expect("the file"),
            "{}\n"
        );
        let read = alloy::luau_config::parse_config_luau(
            &std::fs::read_to_string(dir.join(".config.luau")).expect("the file"),
        )
        .expect("a table");
        assert_eq!(read.language_mode.as_deref(), Some("strict"));
        assert_eq!(
            read.aliases,
            vec![("alloy".to_string(), "./build/alloy".to_string())]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The non-interactive path writes the template and the Luau
    /// configuration byte for byte, whatever the wizard writes beside
    /// it. A script reads the same two files it read before.
    #[test]
    fn the_plain_path_writes_the_template_byte_for_byte() {
        let dir = temp("plain");
        let _ = init_at(&dir);

        assert_eq!(
            std::fs::read_to_string(dir.join("alloy.toml")).expect("alloy.toml"),
            config::TEMPLATE
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(".config.luau")).expect(".config.luau"),
            config::CONFIG_LUAU_TEMPLATE
        );

        // Nothing else: no src, no .gitignore, no manifest.
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("the folder")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();

        assert_eq!(
            names,
            vec![".config.luau".to_string(), "alloy.toml".to_string()]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--yes` writes the recommended scaffold with no prompt, and
    /// `--non-interactive` still writes today's two files over it.
    #[test]
    fn the_yes_flag_writes_the_recommended_setup() {
        let dir = temp("yes");
        let _ = recommended_init(&dir);

        let text = std::fs::read_to_string(dir.join("alloy.toml")).expect("alloy.toml");
        let config = config::Config::parse(&text, &dir.join("alloy.toml")).expect("the config");

        assert_eq!(config.mount.len(), 3);
        assert_eq!(config.fmt, config::FmtConfig::default());
        assert!(config.test.lest);
        assert!(dir.join("ember.toml").is_file());
        assert!(dir.join("src/client").is_dir());
        assert_eq!(config.project.name, folder_name(&dir));

        // A second run refuses, as the plain path does.
        let before = std::fs::read_to_string(dir.join("alloy.toml")).expect("alloy.toml");
        let _ = recommended_init(&dir);
        assert_eq!(
            std::fs::read_to_string(dir.join("alloy.toml")).expect("alloy.toml"),
            before
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The wizard writes what the plan holds, and leaves a file the
    /// author already wrote as it is.
    #[test]
    fn the_wizard_writes_its_plan_and_clobbers_nothing() {
        let dir = temp("wizard");
        std::fs::write(dir.join(".gitignore"), "mine\n").expect("the file");

        let preset = crate::cli::init_plan::built_in_presets()
            .into_iter()
            .find(|p| p.name == "forge")
            .expect("forge");
        let answers = Answers {
            kind: Kind::Game,
            name: "starfall".to_string(),
            manager: Manager::Ember,
            runner: Runner::None,
            preset,
            strict: false,
            has_luau_config: false,
            recommended: false,
        };
        let _ = write_plan(&dir, &answers, &Painter::for_stdout());

        for folder in ["src/client", "src/server", "src/shared"] {
            assert!(dir.join(folder).is_dir(), "{folder}");
        }

        let text = std::fs::read_to_string(dir.join("alloy.toml")).expect("alloy.toml");
        let config = config::Config::parse(&text, &dir.join("alloy.toml")).expect("the config");

        assert_eq!(config.project.name, "starfall");
        assert_eq!(config.fmt.column_width, 80);
        assert!(!config.test.lest);
        assert_eq!(config.mount.len(), 3);
        assert!(dir.join("ember.toml").is_file());

        let mode = alloy::luau_config::parse_config_luau(
            &std::fs::read_to_string(dir.join(".config.luau")).expect(".config.luau"),
        )
        .expect("a table");
        assert_eq!(mode.language_mode.as_deref(), Some("nonstrict"));

        // The author's own `.gitignore` is not overwritten.
        assert_eq!(
            std::fs::read_to_string(dir.join(".gitignore")).expect(".gitignore"),
            "mine\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
