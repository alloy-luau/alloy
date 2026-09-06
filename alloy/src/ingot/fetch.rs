//! A pinned ingot from a GitHub release, unpacked once into
//! `.alloy/ingots/<name>/<version>/`. A later build does no network.
//! curl fetches and `unzip` or `tar` unpacks, so the binary carries no
//! HTTP client, the same as `alloy self update`.

use std::path::{Path, PathBuf};

use crate::config::IngotTable;

/// The release target this binary was built for, as a release names
/// its zips: `x86_64-unknown-linux-gnu`.
pub fn target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// The directory a pinned ingot unpacks into.
pub fn store(root: &Path, name: &str, version: &str) -> PathBuf {
    root.join(".alloy")
        .join("ingots")
        .join(name)
        .join(version.trim_start_matches('v'))
}

/// The directory of a pinned ingot, fetched when it is not there yet.
pub fn ensure(root: &Path, name: &str, table: &IngotTable) -> Result<PathBuf, String> {
    let repo = table
        .repo
        .as_deref()
        .ok_or_else(|| format!("ingot `{name}` names neither a path nor a repo"))?;
    let version = table
        .version
        .as_deref()
        .ok_or_else(|| format!("ingot `{name}` pins no version; add `version = \"x.y.z\"`"))?;
    let dir = store(root, name, version);

    if dir.join(super::manifest::FILE_NAME).is_file() {
        return Ok(dir);
    }

    let tag = format!("v{}", version.trim_start_matches('v'));
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/{tag}");
    let release = run_curl(&["-fsSL", "-H", "Accept: application/vnd.github+json", &url])
        .map_err(|e| format!("ingot `{name}`: no release {tag} at {repo}: {e}"))?;
    let release: serde_json::Value = serde_json::from_str(&release)
        .map_err(|e| format!("ingot `{name}`: cannot read the release: {e}"))?;
    let triple = target_triple().unwrap_or("unknown");
    let wanted: Vec<String> = match &table.asset {
        Some(a) => vec![a.clone()],

        None => vec![
            format!("{name}-ingot-{triple}.zip"),
            format!("{name}-ingot.zip"),
        ],
    };
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let asset = wanted
        .iter()
        .find_map(|w| {
            assets
                .iter()
                .find(|a| a["name"].as_str() == Some(w))
                .and_then(|a| a["browser_download_url"].as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            format!(
                "ingot `{name}`: release {tag} of {repo} has none of {}; set `asset`",
                wanted.join(", ")
            )
        })?;
    let work = std::env::temp_dir().join(format!("alloy-ingot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let zip = work.join("ingot.zip");
    run_curl(&["-fsSL", "-o", &zip.to_string_lossy(), &asset])
        .map_err(|e| format!("ingot `{name}`: the download failed: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let unpacked = if cfg!(windows) {
        std::process::Command::new("tar")
            .arg("-xf")
            .arg(&zip)
            .current_dir(&dir)
            .status()
    } else {
        std::process::Command::new("unzip")
            .args(["-qo"])
            .arg(&zip)
            .current_dir(&dir)
            .status()
    };
    let _ = std::fs::remove_dir_all(&work);

    if !unpacked.is_ok_and(|s| s.success()) {
        let _ = std::fs::remove_dir_all(&dir);

        return Err(format!(
            "ingot `{name}`: cannot unpack the zip; `unzip` (or `tar` on Windows) is needed"
        ));
    }

    // A zip with one top folder unpacks into it; the manifest decides.
    if !dir.join(super::manifest::FILE_NAME).is_file()
        && let Some(nested) = std::fs::read_dir(&dir).ok().and_then(|r| {
            r.flatten()
                .map(|e| e.path())
                .find(|p| p.join(super::manifest::FILE_NAME).is_file())
        })
    {
        for entry in std::fs::read_dir(&nested).into_iter().flatten().flatten() {
            let _ = std::fs::rename(entry.path(), dir.join(entry.file_name()));
        }

        let _ = std::fs::remove_dir(&nested);
    }

    let ignore = root.join(".alloy").join(".gitignore");

    if !ignore.exists() {
        let _ = std::fs::write(&ignore, "*\n");
    }

    Ok(dir)
}

fn run_curl(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
