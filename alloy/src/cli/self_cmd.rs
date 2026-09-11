//! `alloy self <command>` manages the installed binaries.
//!
//! The editor extension starts `alloy-lsp` from PATH, so the install
//! copies the server too when it sits beside the running `alloy`, and
//! fetches it from the release when it does not.
//!
//! A release carries one zip per binary per target, each with the
//! binary at the zip's root. `alloy::net` does the download with
//! whatever the machine has, so no HTTP client is linked in.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::help;
use crate::self_code;
use crate::ui::{self, Painter};

fn fail(message: &str) {
    eprintln!("{}", Painter::for_stderr().fail(message));
}

/// The two binaries that the install copies.
const BINARIES: [&str; 2] = ["alloy", "alloy-lsp"];

pub(crate) fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("schema") => {
            print!("{}", alloy::schema::to_string());
            return ExitCode::SUCCESS;
        }

        Some("--help" | "-h" | "help") | None => {
            print!("{}", help::render_plain(help::SELF_TEXT, ui::want_color()));
            return ExitCode::SUCCESS;
        }

        _ => {}
    }

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

        Some("code") => self_code::run(&dir, args.iter().any(|a| a == "--dry-run")),

        other => {
            fail(&format!(
                "unknown self command `{}`",
                other.unwrap_or_default()
            ));
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
/// its zips. The pair is an argument so a test can ask for a target
/// this machine is not.
fn triple_for(arch: &str, os: &str) -> Option<&'static str> {
    Some(match (arch, os) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

fn target_triple() -> Option<&'static str> {
    triple_for(std::env::consts::ARCH, std::env::consts::OS)
}

/// The zip that carries one binary: `alloy-lsp-0.1.0-rc-<triple>.zip`.
/// The release builds one per binary, each with the binary at its root.
fn asset_name(binary: &str, version: &str, triple: &str) -> String {
    format!("{binary}-{}-{triple}.zip", version.trim_start_matches('v'))
}

/// The asset of a release that carries one binary. The name matches
/// whole, so the `alloy` zip never picks the `alloy-lsp` one up.
fn pick_asset<'a>(
    assets: &'a [serde_json::Value],
    binary: &str,
    version: &str,
    triple: &str,
) -> Option<&'a serde_json::Value> {
    let wanted = asset_name(binary, version, triple);

    assets
        .iter()
        .find(|a| a["name"].as_str() == Some(wanted.as_str()))
}

/// The release from GitHub: the latest, or the one a version names.
fn release_json(version: Option<&str>) -> Result<serde_json::Value, String> {
    let url = match version {
        Some(v) => format!(
            "https://api.github.com/repos/{REPO}/releases/tags/v{}",
            v.trim_start_matches('v')
        ),

        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    };
    let body = alloy::net::get(&url).map_err(|e| format!("no release at {url}: {e}"))?;

    serde_json::from_str(&body).map_err(|e| format!("cannot read the release: {e}"))
}

/// Downloads and unpacks the zips of one release under `work`, and
/// answers where each binary landed. Nothing is replaced here: a
/// download that fails leaves the installed binaries as they were.
fn stage(
    release: &serde_json::Value,
    version: &str,
    triple: &str,
    which: &[&str],
    work: &Path,
) -> Result<Vec<(String, PathBuf)>, String> {
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let mut staged = Vec::new();

    for binary in which.iter().copied() {
        let name = asset_name(binary, version, triple);
        let asset = pick_asset(&assets, binary, version, triple)
            .ok_or_else(|| format!("the release has no {name}"))?;
        let url = asset["browser_download_url"]
            .as_str()
            .ok_or_else(|| format!("{name} carries no download link"))?;
        let into = work.join(binary);
        std::fs::create_dir_all(&into)
            .map_err(|e| format!("cannot create {}: {e}", into.display()))?;
        let zip = into.join("asset.zip");
        let got = alloy::net::download(url, &zip).map_err(|e| format!("{name}: {e}"))?;
        // The API reports the asset's size; a cut short download stops
        // here instead of landing on PATH.
        alloy::net::check_size(got, asset["size"].as_u64().unwrap_or(0))
            .map_err(|e| format!("{name}: {e}"))?;
        unpack(&zip, &into)?;
        let file = exe_name(binary);
        let found = find_file(&into, &file).ok_or_else(|| format!("{name} holds no {file}"))?;
        staged.push((binary.to_string(), found));
    }

    Ok(staged)
}

/// `unzip` on unix, `tar` on Windows, which carries it in the box.
fn unpack(zip: &Path, into: &Path) -> Result<(), String> {
    let status = if cfg!(windows) {
        std::process::Command::new("tar")
            .arg("-xf")
            .arg(zip)
            .current_dir(into)
            .status()
    } else {
        std::process::Command::new("unzip")
            .arg("-qo")
            .arg(zip)
            .current_dir(into)
            .status()
    };

    if status.is_ok_and(|s| s.success()) {
        Ok(())
    } else {
        Err("cannot unpack the zip; `unzip` (or `tar` on Windows) is needed".to_string())
    }
}

/// `alloy self update`: the latest release from GitHub, or the one
/// named, into the install directory. Every zip is downloaded and
/// unpacked before anything is replaced.
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
    let release = match release_json(version) {
        Ok(r) => r,

        Err(e) => {
            fail(&e);

            return ExitCode::FAILURE;
        }
    };
    let tag = release["tag_name"].as_str().unwrap_or("").to_string();
    let wanted = tag.trim_start_matches('v').to_string();

    if version.is_none() && wanted == crate::alloy_version() {
        println!("{}", p.ok(&format!("alloy {wanted} is the latest")));

        return ExitCode::SUCCESS;
    }

    println!(
        "{}",
        p.note(&format!("fetching alloy {wanted} for {triple}"))
    );

    match fetch_into(dir, &release, &wanted, triple, &BINARIES) {
        Ok(names) => {
            println!(
                "{}",
                p.ok(&format!(
                    "alloy {wanted}: {} in {}",
                    names.join(" and "),
                    dir.display()
                ))
            );

            ExitCode::SUCCESS
        }

        Err(e) => {
            fail(&e);

            ExitCode::FAILURE
        }
    }
}

/// Stages the named binaries of a release and puts them in `dir`. The
/// names that landed come back; a failure names what is in place and
/// what is not, and every binary it did not reach is untouched.
fn fetch_into(
    dir: &Path,
    release: &serde_json::Value,
    version: &str,
    triple: &str,
    which: &[&str],
) -> Result<Vec<String>, String> {
    let work = std::env::temp_dir().join(format!("alloy-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;

    let staged = stage(release, version, triple, which, &work).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&work);
    })?;

    if let Err(e) = std::fs::create_dir_all(dir) {
        let _ = std::fs::remove_dir_all(&work);

        return Err(format!("cannot create {}: {e}", dir.display()));
    }

    let mut done: Vec<String> = Vec::new();

    for (name, file) in &staged {
        if let Err(e) = replace_exe(file, &dir.join(exe_name(name))) {
            let _ = std::fs::remove_dir_all(&work);
            let placed = if done.is_empty() {
                "nothing was replaced".to_string()
            } else {
                format!("{} is already the new one", done.join(" and "))
            };

            return Err(format!(
                "cannot write {name} to {}: {e}; {placed}, and the old binary is beside it as .{}.old",
                dir.display(),
                exe_name(name)
            ));
        }

        done.push(name.clone());
    }

    let _ = std::fs::remove_dir_all(&work);

    Ok(done)
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
    let mut absent: Vec<&str> = Vec::new();

    for name in BINARIES {
        let source = if name == "alloy" {
            me.clone()
        } else {
            from_dir.join(exe_name(name))
        };

        if !source.is_file() {
            absent.push(name);
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

    // What is not beside the running binary comes from the release of
    // this version, so an install from a single downloaded `alloy`
    // still puts the server on PATH.
    if !absent.is_empty() && !from_release(dir, &absent) {
        failed = true;
    }

    if failed {
        return ExitCode::FAILURE;
    }

    println!(
        "{}",
        p.note("`alloy self code` gives alloy.toml completion in VS Code and Zed")
    );
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

/// Fetches the named binaries of this version's release into `dir`.
/// True when they all landed; a failure is a warning, because the
/// binaries that were beside the running one are installed already.
fn from_release(dir: &Path, which: &[&str]) -> bool {
    let version = crate::alloy_version();
    let p = Painter::for_stdout();
    let warn = Painter::for_stderr();
    let Some(triple) = target_triple() else {
        eprintln!(
            "{}",
            warn.warn(&format!(
                "no release is built for {} {}, so {} stays missing",
                std::env::consts::ARCH,
                std::env::consts::OS,
                which.join(" and ")
            ))
        );

        return false;
    };

    println!(
        "{}",
        p.note(&format!(
            "{} is not beside this binary; fetching it from the {version} release",
            which.join(" and ")
        ))
    );

    let release = match release_json(Some(version)) {
        Ok(r) => r,

        Err(e) => {
            eprintln!("{}", warn.warn(&format!("{e}; the editor has no server")));

            return false;
        }
    };

    match fetch_into(dir, &release, version, triple, which) {
        Ok(names) => {
            println!(
                "{}",
                p.ok(&format!(
                    "installed {} {version} → {}",
                    names.join(" and "),
                    dir.display()
                ))
            );

            true
        }

        Err(e) => {
            eprintln!("{}", warn.warn(&format!("{e}; the editor has no server")));

            false
        }
    }
}

/// Copies through a staged file and a rename, so a binary in use is
/// replaced in one step.
///
/// The old file moves aside to `.<name>.old` first and comes back when
/// the rename fails, so a half done replace still leaves a binary that
/// runs. Windows refuses to delete a running image, so that file may
/// outlive the call; the next install removes it.
fn replace_exe(from: &Path, to: &Path) -> std::io::Result<()> {
    let dir = to.parent().unwrap_or(Path::new("."));
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let staged = dir.join(format!(".{name}.new"));
    let stale = dir.join(format!(".{name}.old"));

    let _ = std::fs::remove_file(&stale);
    std::fs::copy(from, &staged)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }

    let moved = to.exists() && std::fs::rename(to, &stale).is_ok();

    if let Err(e) = std::fs::rename(&staged, to) {
        let _ = std::fs::remove_file(&staged);

        if moved {
            let _ = std::fs::rename(&stale, to);
        }

        return Err(e);
    }

    if moved {
        let _ = std::fs::remove_file(&stale);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every target the release builds for.
    const TARGETS: [(&str, &str, &str); 5] = [
        ("x86_64", "linux", "x86_64-unknown-linux-gnu"),
        ("aarch64", "linux", "aarch64-unknown-linux-gnu"),
        ("x86_64", "macos", "x86_64-apple-darwin"),
        ("aarch64", "macos", "aarch64-apple-darwin"),
        ("x86_64", "windows", "x86_64-pc-windows-msvc"),
    ];

    /// The assets of one release, as the workflow names them.
    fn assets(version: &str) -> Vec<serde_json::Value> {
        let mut out = Vec::new();

        for (_, _, triple) in TARGETS {
            for binary in BINARIES {
                out.push(json!({
                    "name": format!("{binary}-{version}-{triple}.zip"),
                    "size": 1234,
                    "browser_download_url": format!("https://example/{binary}-{triple}"),
                }));
            }
        }

        out
    }

    #[test]
    fn every_target_resolves_to_its_triple() {
        for (arch, os, triple) in TARGETS {
            assert_eq!(triple_for(arch, os), Some(triple));
        }

        assert_eq!(triple_for("riscv64", "linux"), None);
        assert_eq!(triple_for("x86_64", "freebsd"), None);
        // This machine is one of them, or the update says so.
        assert!(target_triple().is_some() || triple_for("x86_64", "linux").is_some());
    }

    #[test]
    fn an_asset_name_carries_one_binary() {
        assert_eq!(
            asset_name("alloy", "0.1.0-rc", "x86_64-pc-windows-msvc"),
            "alloy-0.1.0-rc-x86_64-pc-windows-msvc.zip"
        );
        // A tag passed with its v names the same zip.
        assert_eq!(
            asset_name("alloy-lsp", "v0.1.0-rc", "aarch64-apple-darwin"),
            "alloy-lsp-0.1.0-rc-aarch64-apple-darwin.zip"
        );
    }

    #[test]
    fn each_target_picks_its_own_zip() {
        let assets = assets("0.1.0-rc");

        for (_, _, triple) in TARGETS {
            for binary in BINARIES {
                let picked = pick_asset(&assets, binary, "0.1.0-rc", triple)
                    .and_then(|a| a["name"].as_str())
                    .unwrap_or("");
                assert_eq!(picked, format!("{binary}-0.1.0-rc-{triple}.zip"));
            }
        }
    }

    /// A stored (uncompressed) zip, written by hand so the test needs
    /// no zip tool and no crate. `unzip` reads it like any other.
    #[cfg(unix)]
    fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;

            for byte in bytes {
                crc ^= u32::from(*byte);

                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xedb8_8320 & mask);
                }
            }

            !crc
        }

        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();

        for (name, data) in entries {
            let offset = out.len() as u32;
            let crc = crc32(data);
            let size = data.len() as u32;
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend([20u8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend(crc.to_le_bytes());
            out.extend(size.to_le_bytes());
            out.extend(size.to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(*data);

            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend([20u8, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend(crc.to_le_bytes());
            central.extend(size.to_le_bytes());
            central.extend(size.to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0u8; 8]);
            // The unix mode, 0o755, in the high half of the attributes.
            central.extend(0x81ed_0000u32.to_le_bytes());
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }

        let count = entries.len() as u16;
        let cd_at = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend(count.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd_at.to_le_bytes());
        out.extend(0u16.to_le_bytes());

        out
    }

    /// The whole path of an update with no network and no release:
    /// download, size check, unpack, and the replace. `file://` stands
    /// in for the download, and `unzip` does the rest.
    #[cfg(unix)]
    #[test]
    fn a_release_zip_reaches_the_install_directory() {
        if !matches!(
            alloy::net::tool(),
            Some(alloy::net::Tool::Curl) | Some(alloy::net::Tool::Wget)
        ) || !Path::new("/usr/bin/unzip").is_file()
        {
            eprintln!("no curl, wget, or unzip; the update path is not checked");

            return;
        }

        let triple = "x86_64-unknown-linux-gnu";
        let version = "0.1.0-rc";
        let root = std::env::temp_dir().join(format!("alloy-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let serve = root.join("serve");
        let work = root.join("work");
        let bin = root.join("bin");
        std::fs::create_dir_all(&serve).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&bin).unwrap();

        let mut assets = Vec::new();

        for binary in BINARIES {
            let zip = stored_zip(&[
                (binary, format!("the {binary} binary").as_bytes()),
                ("README.md", b"docs"),
            ]);
            let at = serve.join(asset_name(binary, version, triple));
            std::fs::write(&at, &zip).unwrap();
            assets.push(json!({
                "name": asset_name(binary, version, triple),
                "size": zip.len(),
                "browser_download_url": format!("file://{}", at.display()),
            }));
        }

        let release = json!({ "tag_name": "v0.1.0-rc", "assets": assets });
        let staged = stage(&release, version, triple, &BINARIES, &work).expect("the zips unpack");
        assert_eq!(staged.len(), 2);

        // An install that is already there is replaced, and the old one
        // does not survive as a stray file.
        let target = bin.join("alloy");
        std::fs::write(&target, "the old binary").unwrap();

        for (name, file) in &staged {
            replace_exe(file, &bin.join(exe_name(name))).expect("the replace lands");
        }

        assert_eq!(
            std::fs::read_to_string(bin.join("alloy")).unwrap(),
            "the alloy binary"
        );
        assert_eq!(
            std::fs::read_to_string(bin.join("alloy-lsp")).unwrap(),
            "the alloy-lsp binary"
        );
        assert!(!bin.join(".alloy.old").exists());

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(bin.join("alloy"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "the installed binary runs");

        // A release that reports a bigger asset than it serves is a cut
        // short download, and nothing is staged.
        let mut lying = release.clone();
        lying["assets"][0]["size"] = json!(1_000_000);
        let err = stage(&lying, version, triple, &BINARIES, &root.join("again")).unwrap_err();
        assert!(err.contains("cut short"), "{err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_alloy_zip_is_never_the_server_zip() {
        // Both names start with `alloy-`, so a contains match would
        // install the server as the compiler.
        let assets = assets("0.1.0-rc");
        let triple = "x86_64-unknown-linux-gnu";
        let picked = pick_asset(&assets, "alloy", "0.1.0-rc", triple).unwrap();
        assert_eq!(
            picked["name"].as_str(),
            Some("alloy-0.1.0-rc-x86_64-unknown-linux-gnu.zip")
        );
        // A release of another version carries nothing for this one.
        assert!(pick_asset(&assets, "alloy", "0.2.0", triple).is_none());
        assert!(pick_asset(&assets, "alloy", "0.1.0-rc", "sparc-sun-solaris").is_none());
    }
}
