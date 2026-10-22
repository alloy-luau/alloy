//! `alloy add`, `alloy install`, and `alloy publish`. They run ember's own
//! commands through its library, so an Alloy project needs no second
//! package manager. Its packages share the registry, the lockfile, and the
//! store with every Luau project.

use std::path::Path;
use std::process::{Command, ExitCode};

use alloy::{config, config_aly};
use embr::api::{Allow, Environment, Language};

use crate::cli::support::positionals;
use crate::fail;
use crate::ui::Painter;

/// What an Alloy project installs: Alloy packages for every environment,
/// and Luau packages for Roblox or a Roblox server. Runtime packages and
/// the other languages stay out.
const ALLOW: Allow = Allow {
    host: "alloy",
    summary: "Alloy packages, and Luau packages for Roblox or a Roblox server",
    languages: &[Language::Luau, Language::Alloy],
    environments: &[Environment::Roblox, Environment::Server],
};

/// `alloy add <scope/name>[@<version>]...` writes each one into ember.toml,
/// then installs once, so the project is ready to build.
pub(crate) fn add(args: &[String]) -> ExitCode {
    let packages = positionals(args);
    if packages.is_empty() {
        fail("name a package: alloy add <scope/name>[@<version>]");

        return ExitCode::FAILURE;
    }

    for package in &packages {
        let (name, version) = match package.split_once('@') {
            Some((name, version)) => (name, Some(version)),
            None => (package.as_str(), None),
        };
        if let Err(error) = embr::api::add(name, version, Some(ALLOW)) {
            return failed(&error);
        }
    }

    install_and_build(false)
}

/// `alloy install [--locked]`, the install `embr install` runs, held to
/// what an Alloy project takes.
pub(crate) fn install(args: &[String]) -> ExitCode {
    install_and_build(args.iter().any(|a| a == "--locked"))
}

/// `alloy publish [--dry-run]` builds with this binary, not the first
/// `alloy` on the PATH, so the package ships the output of the compiler
/// that published it.
pub(crate) fn publish(args: &[String]) -> ExitCode {
    let dry_run = args.iter().any(|a| a == "--dry-run");

    match embr::api::publish(dry_run, std::env::current_exe().ok()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed(&error),
    }
}

fn install_and_build(locked: bool) -> ExitCode {
    if let Err(error) = embr::api::install(locked, Some(ALLOW)) {
        return failed(&error);
    }

    build_alloy_packages()
}

/// An Alloy package ships its .aly and .alx sources beside the Luau its
/// publisher built. An Alloy project builds every one again from those
/// sources with this compiler, so the project runs code from the Alloy
/// it reads, not from another compiler version.
fn build_alloy_packages() -> ExitCode {
    let installed = match embr::api::installed() {
        Ok(installed) => installed,
        Err(error) => return failed(&error),
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

fn failed(error: &embr::api::Error) -> ExitCode {
    fail(&error.to_string());

    ExitCode::FAILURE
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
}
