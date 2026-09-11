//! An ingot from a GitHub release, unpacked once into
//! `.alloy/ingots/<name>/<version>/`, and the lock file that records
//! which release a build reads.
//!
//! A build never fetches. `alloy ingot install` and `alloy ingot
//! update` do the network; a build resolves what is on disk and
//! reports the command when an ingot is missing.
//!
//! `version` in alloy.toml is either `^`, the default, which means the
//! latest release at install time, or a version, which pins that
//! release for good. The lock file, `.alloy/ingots.lock`, holds what a
//! `^` resolved to, so a later build reads the same one until an
//! update runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::IngotTable;

/// The version word for the latest release. It resolves once, at
/// install, and the lock file holds the answer.
pub const LATEST: &str = "^";

/// The lock file's name, beside the store under `.alloy`.
pub const LOCK_NAME: &str = "ingots.lock";

/// The version a project asks for: the `version` key, `^` without one.
pub fn requested(table: &IngotTable) -> &str {
    table.version.as_deref().unwrap_or(LATEST)
}

/// Whether a requested version names one release for good.
pub fn is_pinned(version: &str) -> bool {
    version != LATEST
}

/// A version without its tag's `v`: `v1.2.0` and `1.2.0` are one.
pub fn plain(version: &str) -> &str {
    version.trim_start_matches('v')
}

/// The store: `<root>/.alloy/ingots`.
pub fn store_root(root: &Path) -> PathBuf {
    root.join(".alloy").join("ingots")
}

/// The directory one version of one ingot unpacks into.
pub fn store(root: &Path, name: &str, version: &str) -> PathBuf {
    store_root(root).join(name).join(plain(version))
}

/// The lock file: `<root>/.alloy/ingots.lock`.
pub fn lock_path(root: &Path) -> PathBuf {
    root.join(".alloy").join(LOCK_NAME)
}

/// Whether a store directory holds an unpacked ingot.
fn installed(dir: &Path) -> bool {
    dir.join(super::manifest::FILE_NAME).is_file()
}

/// One installed ingot, as the lock file records it.
///
/// No path is in here. The store's path comes from the name and the
/// version, so the file reads the same on Windows and on unix.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct Locked {
    /// `owner/repo` the release came from.
    pub repo: String,
    /// The release it is installed from, without the tag's `v`.
    pub version: String,
    /// What alloy.toml asked for: `^` or a pinned version.
    pub requested: String,
    /// The asset the release gave.
    pub asset: String,
}

/// The lock file: one entry per installed ingot, by name.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Lock {
    pub ingots: BTreeMap<String, Locked>,
}

const LOCK_HEADER: &str = "\
# The ingots this project has installed, and the release each one came
# from. `alloy ingot install` writes it; a build reads it and fetches
# nothing. Keep it in version control so every machine builds against
# the same releases.
";

impl Lock {
    /// The lock of a project. A missing or unreadable file is an empty
    /// lock: an install writes a new one over it.
    pub fn load(root: &Path) -> Lock {
        std::fs::read_to_string(lock_path(root))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let path = lock_path(root);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }

        let body = toml::to_string(self).map_err(|e| format!("cannot write the lock: {e}"))?;

        std::fs::write(&path, format!("{LOCK_HEADER}\n{body}"))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// The entry for a name, when it answers the same question the
    /// project is asking now.
    pub fn entry(&self, name: &str, repo: &str, want: &str) -> Option<&Locked> {
        self.ingots
            .get(name)
            .filter(|l| l.repo == repo && l.requested == want)
    }
}

/// The directory a build reads, with no network. The error names the
/// command that fetches the ingot.
pub fn resolve(root: &Path, name: &str, table: &IngotTable) -> Result<PathBuf, String> {
    let repo = table
        .repo
        .as_deref()
        .ok_or_else(|| "names neither a path nor a repo".to_string())?;
    let want = requested(table);
    let version = if is_pinned(want) {
        plain(want).to_string()
    } else {
        match Lock::load(root).entry(name, repo, want) {
            Some(locked) => locked.version.clone(),

            None => {
                return Err(format!(
                    "is not installed; `alloy ingot install {name}` fetches the latest release of {repo} and records it in .alloy/{LOCK_NAME}"
                ));
            }
        }
    };
    let dir = store(root, name, &version);

    if installed(&dir) {
        Ok(dir)
    } else {
        Err(format!(
            "{version} is not in {}; run `alloy ingot install {name}`",
            dir.display()
        ))
    }
}

/// The asset names an ingot's release may use, best first. An `asset`
/// key in alloy.toml names one outright.
pub fn asset_names(name: &str, table: &IngotTable, target: &str) -> Vec<String> {
    match &table.asset {
        Some(a) => vec![a.clone()],

        None => vec![
            format!("{name}-ingot-{target}.zip"),
            format!("{name}-ingot.zip"),
        ],
    }
}

/// What an install or an update did to one ingot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A path ingot: nothing to fetch.
    Local,
    /// Installed already, at this version.
    Present(String),
    /// Pinned at this version, so an update leaves it.
    Pinned(String),
    /// Fetched this version now.
    Fetched(String),
}

/// Where releases come from. GitHub is the one the commands use; a
/// test gives its own, so no test reaches the network.
pub trait Source {
    /// The release JSON of a repo: the latest, or the one a tag names.
    fn release(&self, repo: &str, tag: Option<&str>) -> Result<Value, String>;

    /// Puts the files of one asset into `dir`, which exists already.
    fn unpack(&self, asset: &Value, dir: &Path) -> Result<(), String>;
}

/// GitHub over `crate::net`, which uses curl, wget, or PowerShell.
pub struct GitHub;

impl Source for GitHub {
    fn release(&self, repo: &str, tag: Option<&str>) -> Result<Value, String> {
        let url = match tag {
            Some(t) => format!("https://api.github.com/repos/{repo}/releases/tags/{t}"),

            None => format!("https://api.github.com/repos/{repo}/releases/latest"),
        };
        let body = crate::net::get(&url).map_err(|e| format!("no release at {url}: {e}"))?;

        serde_json::from_str(&body).map_err(|e| format!("cannot read the release: {e}"))
    }

    fn unpack(&self, asset: &Value, dir: &Path) -> Result<(), String> {
        let name = asset["name"].as_str().unwrap_or("the asset");
        let url = asset["browser_download_url"]
            .as_str()
            .ok_or_else(|| format!("{name} carries no download link"))?;
        let work = std::env::temp_dir().join(format!("alloy-ingot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)
            .map_err(|e| format!("cannot create {}: {e}", work.display()))?;
        let zip = work.join("ingot.zip");
        let got = crate::net::download(url, &zip).map_err(|e| format!("{name}: {e}"))?;
        let out = crate::net::check_size(got, asset["size"].as_u64().unwrap_or(0))
            .map_err(|e| format!("{name}: {e}"))
            .and_then(|()| extract(&zip, dir));
        let _ = std::fs::remove_dir_all(&work);

        out
    }
}

/// `unzip` on unix, `tar` on Windows, which carries it in the box.
fn extract(zip: &Path, into: &Path) -> Result<(), String> {
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

/// Installs one ingot. `refresh` is `alloy ingot update`: it asks the
/// repository for the latest release again, and leaves a pinned
/// version where it is.
pub fn install(
    root: &Path,
    name: &str,
    table: &IngotTable,
    source: &dyn Source,
    refresh: bool,
) -> Result<Outcome, String> {
    if table.path.is_some() {
        return Ok(Outcome::Local);
    }

    let repo = table
        .repo
        .as_deref()
        .ok_or_else(|| "names neither a path nor a repo".to_string())?;
    let want = requested(table);

    if is_pinned(want) {
        let version = plain(want).to_string();

        if installed(&store(root, name, &version)) {
            record(root, name, repo, &version, want, None)?;

            return Ok(if refresh {
                Outcome::Pinned(version)
            } else {
                Outcome::Present(version)
            });
        }

        // A pinned ingot that is missing is fetched by both commands:
        // an update that left a hole would break the next build.
        let tag = format!("v{version}");
        let release = source.release(repo, Some(&tag))?;

        return fetch(root, name, table, source, &release, repo, want);
    }

    let lock = Lock::load(root);
    let locked = lock.entry(name, repo, want).cloned();

    if !refresh
        && let Some(l) = &locked
        && installed(&store(root, name, &l.version))
    {
        return Ok(Outcome::Present(l.version.clone()));
    }

    let release = source.release(repo, None)?;
    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| format!("the latest release of {repo} carries no tag"))?;
    let version = plain(tag).to_string();

    if installed(&store(root, name, &version)) {
        record(root, name, repo, &version, want, None)?;

        return Ok(Outcome::Present(version));
    }

    fetch(root, name, table, source, &release, repo, want)
}

/// Unpacks one release into the store and records it.
fn fetch(
    root: &Path,
    name: &str,
    table: &IngotTable,
    source: &dyn Source,
    release: &Value,
    repo: &str,
    want: &str,
) -> Result<Outcome, String> {
    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| format!("the release of {repo} carries no tag"))?;
    let version = plain(tag).to_string();
    let target = crate::target::label().unwrap_or("unknown");
    let wanted = asset_names(name, table, target);
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let asset = wanted
        .iter()
        .find_map(|w| {
            assets
                .iter()
                .find(|a| a["name"].as_str() == Some(w.as_str()))
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "release {tag} of {repo} has none of {}; set `asset`",
                wanted.join(", ")
            )
        })?;
    let dir = store(root, name, &version);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    if let Err(e) = source.unpack(&asset, &dir) {
        clear(&dir);

        return Err(e);
    }

    flatten(&dir);

    if !installed(&dir) {
        clear(&dir);

        return Err(format!("the asset holds no {}", super::manifest::FILE_NAME));
    }

    ignore_the_store(root);

    let asset_name = asset["name"].as_str().unwrap_or_default().to_string();
    record(root, name, repo, &version, want, Some(asset_name))?;

    Ok(Outcome::Fetched(version))
}

/// Keeps the store out of the repository. The lock file sits beside
/// it and stays in, so a build on another machine reads the same
/// releases. `alloy build` writes the same file.
fn ignore_the_store(root: &Path) {
    let path = root.join(".alloy").join(".gitignore");
    let held = std::fs::read_to_string(&path).unwrap_or_default();

    if held.lines().any(|l| l.trim() == "ingots/") {
        return;
    }

    let _ = if held.trim().is_empty() {
        std::fs::write(&path, crate::project::ALLOY_DIR_IGNORE)
    } else {
        std::fs::write(&path, format!("{}ingots/\n", newline_ended(&held)))
    };
}

/// Text that ends with one newline.
fn newline_ended(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

/// Drops a half unpacked version, and the ingot's folder with it when
/// that was the only version in it. A failed install leaves the store
/// as it found it.
fn clear(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);

    if let Some(parent) = dir.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// A zip with one top folder unpacks into it; the manifest decides.
fn flatten(dir: &Path) {
    if installed(dir) {
        return;
    }

    let Some(nested) = std::fs::read_dir(dir).ok().and_then(|r| {
        r.flatten()
            .map(|e| e.path())
            .find(|p| p.join(super::manifest::FILE_NAME).is_file())
    }) else {
        return;
    };

    for entry in std::fs::read_dir(&nested).into_iter().flatten().flatten() {
        let _ = std::fs::rename(entry.path(), dir.join(entry.file_name()));
    }

    let _ = std::fs::remove_dir(&nested);
}

/// Writes one ingot into the lock file, keeping the rest.
fn record(
    root: &Path,
    name: &str,
    repo: &str,
    version: &str,
    want: &str,
    asset: Option<String>,
) -> Result<(), String> {
    let mut lock = Lock::load(root);
    let held = lock.ingots.get(name).map(|l| l.asset.clone());
    lock.ingots.insert(
        name.to_string(),
        Locked {
            repo: repo.to_string(),
            version: version.to_string(),
            requested: want.to_string(),
            asset: asset.or(held).unwrap_or_default(),
        },
    );

    lock.save(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A release index in memory: no network, and the unpack writes
    /// the manifest a real asset would carry.
    struct Fake {
        /// `repo` to the tags it has, oldest first.
        tags: Vec<(&'static str, &'static str)>,
        name: &'static str,
    }

    impl Fake {
        fn release(&self, tag: &str) -> Value {
            json!({
                "tag_name": tag,
                "assets": [{
                    "name": format!("{}-ingot-{}.zip", self.name, crate::target::label().unwrap_or("unknown")),
                    "size": 10,
                    "browser_download_url": format!("https://example/{tag}"),
                }],
            })
        }
    }

    impl Source for Fake {
        fn release(&self, repo: &str, tag: Option<&str>) -> Result<Value, String> {
            let mine: Vec<&str> = self
                .tags
                .iter()
                .filter(|(r, _)| *r == repo)
                .map(|(_, t)| *t)
                .collect();

            match tag {
                Some(t) => mine
                    .iter()
                    .find(|have| **have == t)
                    .map(|t| Fake::release(self, t))
                    .ok_or_else(|| format!("no release {t} at {repo}")),

                None => mine
                    .last()
                    .map(|t| Fake::release(self, t))
                    .ok_or_else(|| format!("{repo} has no release")),
            }
        }

        fn unpack(&self, asset: &Value, dir: &Path) -> Result<(), String> {
            let tag = asset["browser_download_url"]
                .as_str()
                .and_then(|u| u.rsplit('/').next())
                .unwrap_or("v0");
            // A real asset carries the manifest and the binary; the
            // version in it is what the test reads back.
            std::fs::write(
                dir.join(super::super::manifest::FILE_NAME),
                format!(
                    "name = \"{}\"\napi = 1\nhooks = [\"lint\"]\ndescription = \"{tag}\"\n",
                    self.name
                ),
            )
            .map_err(|e| e.to_string())
        }
    }

    fn temp(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "alloy-fetch-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        dir
    }

    fn table(version: Option<&str>) -> IngotTable {
        IngotTable {
            repo: Some("someone/shout-ingot".to_string()),
            version: version.map(str::to_string),
            ..IngotTable::default()
        }
    }

    fn source() -> Fake {
        Fake {
            tags: vec![
                ("someone/shout-ingot", "v0.1.0"),
                ("someone/shout-ingot", "v0.2.0"),
            ],
            name: "shout",
        }
    }

    #[test]
    fn a_missing_version_means_the_latest() {
        assert_eq!(requested(&table(None)), LATEST);
        assert_eq!(requested(&table(Some("1.2.3"))), "1.2.3");
        assert!(!is_pinned(LATEST));
        assert!(is_pinned("1.2.3"));
        assert_eq!(plain("v1.2.3"), "1.2.3");
        assert_eq!(plain("1.2.3"), "1.2.3");
    }

    #[test]
    fn the_store_path_is_built_from_parts() {
        // A Windows root has backslashes in it and no segment of the
        // store may carry a separator of its own.
        let root = Path::new("C:\\work\\game");
        let dir = store(root, "shout", "v0.2.0");
        let tail: Vec<String> = dir
            .components()
            .rev()
            .take(4)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(tail, vec!["0.2.0", "shout", "ingots", ".alloy"]);
        assert!(dir.starts_with(root));
        assert!(dir.ends_with(Path::new("shout").join("0.2.0")));
        assert_eq!(lock_path(root).file_name().unwrap(), LOCK_NAME);
        assert!(lock_path(root).starts_with(store_root(root).parent().unwrap()));
    }

    #[test]
    fn the_lock_file_round_trips() {
        let root = temp("lock");
        let mut lock = Lock::default();
        lock.ingots.insert(
            "shout".to_string(),
            Locked {
                repo: "someone/shout-ingot".to_string(),
                version: "0.2.0".to_string(),
                requested: LATEST.to_string(),
                asset: "shout-ingot.zip".to_string(),
            },
        );
        lock.save(&root).unwrap();

        let text = std::fs::read_to_string(lock_path(&root)).unwrap();
        assert!(text.starts_with("# The ingots"), "{text}");
        assert!(!text.contains('\\'), "the lock holds no path: {text}");

        let back = Lock::load(&root);
        assert_eq!(back, lock);
        assert_eq!(
            back.entry("shout", "someone/shout-ingot", LATEST),
            lock.ingots.get("shout")
        );
        // The entry answers one question: another repo is not a hit.
        assert!(back.entry("shout", "other/repo", LATEST).is_none());
        assert!(
            back.entry("shout", "someone/shout-ingot", "0.1.0")
                .is_none()
        );
        // A missing file is an empty lock, not an error.
        assert_eq!(Lock::load(&temp("empty")), Lock::default());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_build_never_fetches_and_names_the_command() {
        let root = temp("resolve");
        let err = resolve(&root, "shout", &table(None)).unwrap_err();
        assert!(err.contains("alloy ingot install shout"), "{err}");
        let err = resolve(&root, "shout", &table(Some("0.2.0"))).unwrap_err();
        assert!(err.contains("alloy ingot install shout"), "{err}");
        assert!(err.contains("0.2.0"), "{err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_latest_resolves_once_and_the_lock_holds_it() {
        let root = temp("latest");
        let table = table(None);
        let source = source();

        assert_eq!(
            install(&root, "shout", &table, &source, false),
            Ok(Outcome::Fetched("0.2.0".to_string()))
        );
        assert_eq!(
            resolve(&root, "shout", &table).unwrap(),
            store(&root, "shout", "0.2.0")
        );

        // The store is ignored; the lock file beside it is not.
        let ignore = std::fs::read_to_string(root.join(".alloy").join(".gitignore")).unwrap();
        assert!(ignore.lines().any(|l| l == "ingots/"), "{ignore}");
        assert!(!ignore.contains(LOCK_NAME), "{ignore}");

        let locked = Lock::load(&root);
        let entry = locked
            .entry("shout", "someone/shout-ingot", LATEST)
            .unwrap();
        assert_eq!(entry.version, "0.2.0");
        assert_eq!(entry.requested, LATEST);
        assert!(entry.asset.ends_with(".zip"));

        // A second install fetches nothing, even when a newer release
        // is out: the lock decides until an update runs.
        let newer = Fake {
            tags: vec![
                ("someone/shout-ingot", "v0.2.0"),
                ("someone/shout-ingot", "v0.3.0"),
            ],
            name: "shout",
        };
        assert_eq!(
            install(&root, "shout", &table, &newer, false),
            Ok(Outcome::Present("0.2.0".to_string()))
        );
        assert_eq!(Lock::load(&root).ingots["shout"].version, "0.2.0");

        // The update takes the newer one and moves the lock with it.
        assert_eq!(
            install(&root, "shout", &table, &newer, true),
            Ok(Outcome::Fetched("0.3.0".to_string()))
        );
        assert_eq!(Lock::load(&root).ingots["shout"].version, "0.3.0");
        assert_eq!(
            resolve(&root, "shout", &table).unwrap(),
            store(&root, "shout", "0.3.0")
        );
        // With no newer release the update says it is current.
        assert_eq!(
            install(&root, "shout", &table, &newer, true),
            Ok(Outcome::Present("0.3.0".to_string()))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pinned_version_never_moves() {
        let root = temp("pinned");
        let pinned = table(Some("0.1.0"));
        let source = source();

        assert_eq!(
            install(&root, "shout", &pinned, &source, false),
            Ok(Outcome::Fetched("0.1.0".to_string()))
        );
        assert_eq!(
            resolve(&root, "shout", &pinned).unwrap(),
            store(&root, "shout", "0.1.0")
        );

        // The repository has 0.2.0 and the update leaves this one.
        assert_eq!(
            install(&root, "shout", &pinned, &source, true),
            Ok(Outcome::Pinned("0.1.0".to_string()))
        );
        assert_eq!(Lock::load(&root).ingots["shout"].version, "0.1.0");
        assert!(!store(&root, "shout", "0.2.0").exists());

        // A tag written with its `v` names the same release.
        let with_v = table(Some("v0.1.0"));
        assert_eq!(
            install(&root, "shout", &with_v, &source, false),
            Ok(Outcome::Present("0.1.0".to_string()))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pinned_release_that_is_missing_is_reported() {
        let root = temp("gone");
        let err = install(&root, "shout", &table(Some("9.9.9")), &source(), false).unwrap_err();
        assert!(err.contains("9.9.9"), "{err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A release whose asset holds no manifest.
    struct Empty;

    impl Source for Empty {
        fn release(&self, _repo: &str, _tag: Option<&str>) -> Result<Value, String> {
            Ok(json!({
                "tag_name": "v1.0.0",
                "assets": [{
                    "name": format!("shout-ingot-{}.zip", crate::target::label().unwrap_or("unknown")),
                    "size": 1,
                    "browser_download_url": "https://example/v1.0.0",
                }],
            }))
        }

        fn unpack(&self, _asset: &Value, _dir: &Path) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn an_asset_without_a_manifest_leaves_the_store_empty() {
        let root = temp("empty-asset");
        let err = install(&root, "shout", &table(None), &Empty, false).unwrap_err();
        assert!(err.contains(super::super::manifest::FILE_NAME), "{err}");
        assert!(!store_root(&root).join("shout").exists());
        assert!(Lock::load(&root).ingots.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_path_ingot_fetches_nothing() {
        let root = temp("local");
        let table = IngotTable {
            path: Some("ingots/shout".to_string()),
            ..IngotTable::default()
        };
        assert_eq!(
            install(&root, "shout", &table, &source(), true),
            Ok(Outcome::Local)
        );
        assert!(!lock_path(&root).exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_asset_names_follow_the_target() {
        let plain = IngotTable::default();
        let names = asset_names("shout", &plain, "x86_64-pc-windows-msvc");
        assert_eq!(
            names,
            vec!["shout-ingot-x86_64-pc-windows-msvc.zip", "shout-ingot.zip"]
        );

        let named = IngotTable {
            asset: Some("shout.zip".to_string()),
            ..IngotTable::default()
        };
        assert_eq!(
            asset_names("shout", &named, "x86_64-unknown-linux-gnu"),
            vec!["shout.zip"]
        );
    }
}
