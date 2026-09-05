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

        Some("update") => update(&dir, option(args, "--version")),

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

fn option<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

const REPO: &str = "alloy-luau/alloy";

/// The release target this binary was built for, as the release names
/// its zips: `x86_64-unknown-linux-gnu`.
fn target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// `alloy self update`: the latest release from GitHub, or the one
/// named, unpacked into the install directory. curl fetches and
/// `unzip` or `tar` unpacks, so the binary carries no HTTP client.
fn update(dir: &Path, version: Option<&str>) -> ExitCode {
    let p = Painter::for_stdout();
    let Some(triple) = target_triple() else {
        fail(&format!(
            "no release is built for {} {}; build from source",
            std::env::consts::ARCH,
            std::env::consts::OS
        ));

        return ExitCode::FAILURE;
    };

    let url = match version {
        Some(v) => format!(
            "https://api.github.com/repos/{REPO}/releases/tags/v{}",
            v.trim_start_matches('v')
        ),

        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    };
    let release = match std::process::Command::new("curl")
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json"])
        .arg(&url)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),

        Ok(_) => {
            fail(&format!(
                "no release at {url}; the project may not have one yet"
            ));

            return ExitCode::FAILURE;
        }

        Err(e) => {
            fail(&format!("cannot run curl: {e}"));

            return ExitCode::FAILURE;
        }
    };
    let release: serde_json::Value = match serde_json::from_str(&release) {
        Ok(v) => v,

        Err(e) => {
            fail(&format!("cannot read the release: {e}"));

            return ExitCode::FAILURE;
        }
    };
    let tag = release["tag_name"].as_str().unwrap_or("").to_string();
    let wanted = tag.trim_start_matches('v');

    if version.is_none() && wanted == crate::alloy_version() {
        println!("{}", p.ok(&format!("alloy {wanted} is the latest")));

        return ExitCode::SUCCESS;
    }

    let Some(asset) = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| {
            a["name"]
                .as_str()
                .is_some_and(|n| n.contains(triple) && n.ends_with(".zip"))
        })
        .and_then(|a| a["browser_download_url"].as_str())
    else {
        fail(&format!("release {tag} has no zip for {triple}"));

        return ExitCode::FAILURE;
    };

    let work = std::env::temp_dir().join(format!("alloy-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);

    if let Err(e) = std::fs::create_dir_all(&work) {
        fail(&format!("cannot create {}: {e}", work.display()));

        return ExitCode::FAILURE;
    }

    let zip = work.join("alloy.zip");
    println!("{}", p.note(&format!("fetching {asset}")));

    let fetched = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&zip)
        .arg(asset)
        .status()
        .is_ok_and(|s| s.success());

    if !fetched {
        fail("the download failed");

        return ExitCode::FAILURE;
    }

    let unpacked = if cfg!(windows) {
        std::process::Command::new("tar")
            .arg("-xf")
            .arg(&zip)
            .current_dir(&work)
            .status()
    } else {
        std::process::Command::new("unzip")
            .args(["-qo"])
            .arg(&zip)
            .current_dir(&work)
            .status()
    };

    if !unpacked.is_ok_and(|s| s.success()) {
        fail("cannot unpack the zip; `unzip` (or `tar` on Windows) is needed");

        return ExitCode::FAILURE;
    }

    if let Err(e) = std::fs::create_dir_all(dir) {
        fail(&format!("cannot create {}: {e}", dir.display()));

        return ExitCode::FAILURE;
    }

    let mut copied = 0;

    for name in BINARIES {
        let file = exe_name(name);
        let Some(source) = find_file(&work, &file) else {
            eprintln!(
                "{}",
                Painter::for_stderr().warn(&format!("{file} is not in the zip"))
            );

            continue;
        };
        let target = dir.join(&file);

        // The running binary cannot be overwritten in place on Windows;
        // it moves aside first.
        let aside = dir.join(format!("{file}.old"));
        let _ = std::fs::remove_file(&aside);
        let _ = std::fs::rename(&target, &aside);

        match std::fs::copy(&source, &target) {
            Ok(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
                }

                let _ = std::fs::remove_file(&aside);
                copied += 1;
                println!("{}", p.wrote(&target.display().to_string()));
            }

            Err(e) => {
                let _ = std::fs::rename(&aside, &target);
                fail(&format!("cannot write {}: {e}", target.display()));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&work);

    if copied == BINARIES.len() {
        println!(
            "{}",
            p.ok(&format!("alloy {wanted} installed in {}", dir.display()))
        );

        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// A file by name anywhere under `dir`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();

        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
    }

    None
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
