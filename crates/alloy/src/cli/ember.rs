//! Ember's commands under `alloy`: add, install, publish, yank, deprecate,
//! scripts, tools, patches, and the rest. They run through ember's library,
//! so an Alloy project needs no second package manager. Its packages share
//! the registry, the lockfile, and the store with every Luau project, and
//! the output wears Alloy's colours.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitCode};

use alloy::{config, config_aly};
use embr::api::{Allow, Environment, Host, Language, Outcome, Palette};

use crate::cli::support::positionals;
use crate::fail;
use crate::ui::{self, Painter};

/// The ember commands `alloy` runs as they are. Alloy owns `build`, `test`,
/// `fmt`, `init`, `self`, and `mcp`; their ember scripts run through
/// `alloy run <script>`.
pub(crate) const COMMANDS: &[&str] = &[
    "yank",
    "deprecate",
    "run",
    "start",
    "serve",
    "patch",
    "tool",
    "index",
    "cache",
    "studio",
    "execute",
    "x",
];

/// What an Alloy project installs: Alloy packages for every environment,
/// and Luau packages for Roblox or a Roblox server. Runtime packages and
/// the other languages stay out.
const ALLOW: Allow = Allow {
    host: "alloy",
    summary: "Alloy packages, and Luau packages for Roblox or a Roblox server",
    languages: &[Language::Luau, Language::Alloy],
    environments: &[Environment::Roblox, Environment::Server],
};

/// Ember's marks, bars, prompts, and help in Alloy's brand: the accent is
/// the logo purple, and the ramp runs from the logo's lilac to a deep violet.
const PALETTE: Palette = Palette {
    accent: ui::BRAND,
    top: (0xD2, 0xBC, 0xFF),
    bottom: (0x6A, 0x2B, 0xD8),
};

/// Runs one ember command. `args` starts at the command name.
pub(crate) fn run(args: &[String]) -> ExitCode {
    finish(ember(args.iter().map(OsString::from)))
}

/// `alloy add <scope/name>[@<version>]...` writes each one into ember.toml,
/// then installs, so the project is ready to build. With an ember flag,
/// such as `--index`, the words go to ember's add as they are.
pub(crate) fn add(args: &[String]) -> ExitCode {
    let packages = positionals(args);
    if packages.is_empty() {
        fail("name a package: alloy add <scope/name>[@<version>]");

        return ExitCode::FAILURE;
    }

    if args.iter().any(|a| a.starts_with('-')) {
        let words = std::iter::once("add".to_string()).chain(args.iter().cloned());
        if let Err(code) = finish_quiet(ember(words.map(OsString::from))) {
            return code;
        }
    } else {
        for package in &packages {
            let mut words = vec!["add".to_string()];
            match package.split_once('@') {
                Some((name, version)) => {
                    words.extend([name.to_string(), "--version".into(), version.to_string()])
                }
                None => words.push(package.clone()),
            }
            if let Err(code) = finish_quiet(ember(words.into_iter().map(OsString::from))) {
                return code;
            }
        }
    }

    install(&[])
}

/// `alloy install [--locked]`, the install `embr install` runs, held to
/// what an Alloy project takes. Each Alloy package then builds again from
/// its sources.
pub(crate) fn install(args: &[String]) -> ExitCode {
    let words = std::iter::once("install".to_string()).chain(args.iter().cloned());
    if let Err(code) = finish_quiet(ember(words.map(OsString::from))) {
        return code;
    }

    build_alloy_packages()
}

/// `alloy publish [--dry-run]` builds with this binary, not the first
/// `alloy` on the PATH, so the package ships the output of the compiler
/// that published it.
pub(crate) fn publish(args: &[String]) -> ExitCode {
    let words = std::iter::once("publish".to_string()).chain(args.iter().cloned());

    finish(ember(words.map(OsString::from)))
}

/// An ember error as Alloy reports it: the message, and the exit code
/// ember gives it, so a failed script's code passes through `alloy run`.
struct Failure {
    message: String,
    code: i32,
}

fn ember(words: impl IntoIterator<Item = OsString>) -> Result<Outcome, Failure> {
    embr::api::set_palette(PALETTE);
    let host = Host {
        name: "alloy",
        allow: Some(ALLOW),
        compiler: std::env::current_exe().ok(),
    };

    embr::api::run(&host, words).map_err(|error| Failure {
        message: error.to_string(),
        code: error.exit_code(),
    })
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn finish(result: Result<Outcome, Failure>) -> ExitCode {
    match result {
        Ok(Outcome::Done | Outcome::Quiet) => ExitCode::SUCCESS,
        Ok(Outcome::Exit(code)) => exit_code(code),
        Err(failure) => failed(&failure),
    }
}

/// `finish` for a step that more work follows: success is `Ok`.
fn finish_quiet(result: Result<Outcome, Failure>) -> Result<(), ExitCode> {
    result.map(|_| ()).map_err(|failure| failed(&failure))
}

/// An Alloy package ships its .aly, .alx, and .d.aly sources beside the
/// Luau its publisher built. An Alloy project builds every one again from
/// those sources with this compiler, so the project runs code from the
/// Alloy it reads, not from another compiler version. The build checks the
/// package's .d.aly declarations too, since they sit under its own `in`.
fn build_alloy_packages() -> ExitCode {
    let installed = match embr::api::installed() {
        Ok(installed) => installed,
        Err(error) => {
            fail(&error.to_string());

            return ExitCode::FAILURE;
        }
    };
    let Ok(alloy) = std::env::current_exe() else {
        fail("cannot find the running alloy to build the Alloy packages with");

        return ExitCode::FAILURE;
    };
    let p = Painter::for_stdout();

    for package in installed.iter().filter(|package| is_alloy(&package.dir)) {
        let id = format!("{}@{}", package.name, package.version);
        println!(
            "{}",
            p.note(&format!("building {id} from its Alloy sources"))
        );

        let built = Command::new(&alloy)
            .arg("build")
            .current_dir(&package.dir)
            .status()
            .is_ok_and(|status| status.success());
        if !built {
            fail(&format!("{id} does not build from its sources"));

            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}

/// A package whose ember.toml says `language = "alloy"` and that ships an
/// Alloy config to build with. Without the config, the published Luau
/// stays as it is.
fn is_alloy(dir: &Path) -> bool {
    let has_config =
        dir.join(config::FILE_NAME).is_file() || dir.join(config_aly::FILE_NAME).is_file();
    let language = std::fs::read_to_string(dir.join("ember.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|manifest| {
            manifest
                .get("target")?
                .get("language")?
                .as_str()
                .map(str::to_string)
        });

    has_config && language.as_deref() == Some("alloy")
}

fn failed(failure: &Failure) -> ExitCode {
    fail(&failure.message);

    exit_code(failure.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_alloy_packages_with_a_config_build_again() {
        let dir = std::env::temp_dir().join(format!("alloy-ember-is-alloy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).unwrap();

        write("ember.toml", "[target]\nenvironment = \"roblox\"\n");
        assert!(!is_alloy(&dir), "a Luau package");

        write(
            "ember.toml",
            "[target]\nenvironment = \"roblox\"\nlanguage = \"alloy\"\n",
        );
        assert!(
            !is_alloy(&dir),
            "an Alloy package with no config keeps its Luau"
        );

        write(config::FILE_NAME, "[build]\nin = \"src\"\nout = \"out\"\n");
        assert!(is_alloy(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_allow_list_is_alloy_and_roblox() {
        assert!(ALLOW.admits(Language::Alloy, Some(Environment::Luau)));
        assert!(ALLOW.admits(Language::Luau, Some(Environment::Server)));
        assert!(!ALLOW.admits(Language::Luau, Some(Environment::Lune)));
        assert!(!ALLOW.admits(Language::Loom, Some(Environment::Roblox)));
    }

    /// Alloy's own commands stay Alloy's: none of them goes to ember.
    #[test]
    fn ember_never_takes_an_alloy_command() {
        for own in [
            "build", "check", "flux", "lint", "fmt", "test", "doc", "init", "migrate", "ingot",
            "self", "mcp", "help",
        ] {
            assert!(!COMMANDS.contains(&own), "{own} belongs to alloy");
        }
    }
}
