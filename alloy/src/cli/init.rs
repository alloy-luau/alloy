//! `alloy init`: scaffolds `alloy.toml` and a Luau configuration.

use std::path::Path;
use std::process::ExitCode;

use alloy::config;

use crate::fail;
use crate::ui::Painter;

/// Writes `alloy.toml` and one Luau configuration. A folder with
/// neither file gets a `.config.luau` with strict mode and the `@alloy`
/// alias. A folder that already has `.config.luau` or `.luaurc` keeps
/// that one file, and gains only the mode and the alias it lacks.
pub(crate) fn init() -> ExitCode {
    let path = Path::new(config::FILE_NAME);

    let p = Painter::for_stdout();

    if path.exists() {
        fail(&format!("{} already exists", path.display()));
        return ExitCode::FAILURE;
    }

    if let Err(e) = std::fs::write(path, config::TEMPLATE) {
        fail(&format!("cannot write {}: {e}", path.display()));
        return ExitCode::FAILURE;
    }

    println!("{}", p.wrote(&path.display().to_string()));

    if !init_luau_config(Path::new("."), &p) {
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
}
