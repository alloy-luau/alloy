//! `alloy self <command>` manages the installed binaries.
//!
//! The editor extension starts `alloy-lsp` from PATH, so the install
//! copies the server too when it sits beside the running `alloy`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::help;
use crate::ui::{self, Painter};

fn fail(message: &str) {
    eprintln!("{}", Painter::for_stderr().fail(message));
}

/// The two binaries that the install copies.
const BINARIES: [&str; 2] = ["alloy", "alloy-lsp"];

pub fn run(args: &[String]) -> ExitCode {
    let dir = match args.iter().position(|a| a == "--dir") {
        Some(i) => match args.get(i + 1) {
            Some(d) => PathBuf::from(d),

            None => {
                fail("--dir needs a path");
                return ExitCode::FAILURE;
            }
        },

        None => match default_dir() {
            Some(d) => d,

            None => {
                fail("cannot find your home directory; pass --dir <path>");
                return ExitCode::FAILURE;
            }
        },
    };

    match args.first().map(String::as_str) {
        Some("install") => install(&dir),

        Some("uninstall") => uninstall(&dir),

        Some("--help" | "-h" | "help") | None => {
            print!("{}", help::render_plain(help::SELF_TEXT, ui::want_color()));
            ExitCode::SUCCESS
        }

        Some(other) => {
            fail(&format!("unknown self command `{other}`"));
            eprint!("{}", help::render_plain(help::SELF_TEXT, false));
            ExitCode::FAILURE
        }
    }
}

/// `~/.alloy/bin`, the directory that goes on PATH.
fn default_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".alloy").join("bin"))
}

fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn install(dir: &Path) -> ExitCode {
    let me = match std::env::current_exe() {
        Ok(p) => p,

        Err(e) => {
            fail(&format!("cannot locate the running executable: {e}"));
            return ExitCode::FAILURE;
        }
    };

    // The debug build of the server answers every keystroke, so it is
    // worth one line; the install itself is the development loop.
    let p = Painter::for_stdout();

    if cfg!(debug_assertions) {
        eprintln!(
            "{}",
            Painter::for_stderr()
                .note("this is a debug build; a release build serves the editor faster")
        );
    }

    if let Err(e) = std::fs::create_dir_all(dir) {
        fail(&format!("cannot create {}: {e}", dir.display()));
        return ExitCode::FAILURE;
    }

    let from_dir = me.parent().unwrap_or(Path::new("."));
    let mut failed = false;

    for name in BINARIES {
        let source = if name == "alloy" {
            me.clone()
        } else {
            from_dir.join(exe_name(name))
        };

        if !source.is_file() {
            eprintln!(
                "{}",
                Painter::for_stderr().warn(&format!(
                    "{} is not beside this binary, so the editor has no server until it is installed",
                    exe_name(name)
                ))
            );
            continue;
        }

        let target = dir.join(exe_name(name));

        if same_file(&source, &target) {
            println!(
                "{}",
                p.note(&format!(
                    "{name} is already installed at {}",
                    target.display()
                ))
            );
            continue;
        }

        match replace_exe(&source, &target) {
            Ok(()) => println!(
                "{}",
                p.ok(&format!("installed {name} → {}", target.display()))
            ),

            Err(e) => {
                fail(&format!(
                    "cannot install {name} to {}: {e}",
                    target.display()
                ));
                failed = true;
            }
        }
    }

    if failed {
        return ExitCode::FAILURE;
    }

    path_hint(dir);

    ExitCode::SUCCESS
}

fn uninstall(dir: &Path) -> ExitCode {
    let p = Painter::for_stdout();
    let mut removed = 0;

    for name in BINARIES {
        let target = dir.join(exe_name(name));

        if !target.exists() {
            continue;
        }

        match std::fs::remove_file(&target) {
            Ok(()) => {
                println!("{}", p.ok(&format!("removed {}", target.display())));
                removed += 1;
            }

            Err(e) => {
                fail(&format!("cannot remove {}: {e}", target.display()));
                return ExitCode::FAILURE;
            }
        }
    }

    if removed == 0 {
        println!(
            "{}",
            p.note(&format!("nothing to remove in {}", dir.display()))
        );
    }

    // An empty bin directory has no reason to stay.
    let _ = std::fs::remove_dir(dir);

    ExitCode::SUCCESS
}

/// Copies through a staged file and a rename, so a binary in use is
/// replaced in one step and a failed copy leaves the old one intact.
fn replace_exe(from: &Path, to: &Path) -> std::io::Result<()> {
    let dir = to.parent().unwrap_or(Path::new("."));
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let staged = dir.join(format!(".{name}.new"));
    let stale = dir.join(format!(".{name}.old"));

    let _ = std::fs::remove_file(&stale);
    std::fs::copy(from, &staged)?;

    // Windows refuses to overwrite a running file, but lets it move.
    if cfg!(windows) && to.exists() {
        std::fs::rename(to, &stale)?;
    }

    if let Err(e) = std::fs::rename(&staged, to) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }

    Ok(())
}

/// A same file check through canonicalize; false when either is missing.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,

        _ => false,
    }
}

/// Says how to put the directory on PATH when it is not there yet.
fn path_hint(dir: &Path) {
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|entry| entry == dir))
        .unwrap_or(false);

    if on_path {
        return;
    }

    let p = Painter::for_stdout();
    println!();
    println!(
        "{}",
        p.warn(&format!(
            "{} is not on your PATH. Add this to your shell profile:",
            dir.display()
        ))
    );

    if cfg!(windows) {
        println!(
            "  {}",
            p.paint(ui::LILAC, &format!("set PATH={};%PATH%", dir.display()))
        );
    } else {
        println!(
            "  {}",
            p.paint(
                ui::LILAC,
                &format!("export PATH=\"{}:$PATH\"", dir.display())
            )
        );
    }
}
