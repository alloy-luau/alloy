//! The DataModel tree: where each folder lands, and what follows.
//!
//! Two things can describe the tree. A `[mount]` table in alloy.toml
//! names the folders and their places, for a sync tool with its own
//! format; a Rojo or Argon project file at the root, read by
//! `crate::rojo`, names them for a tool that reads one. The table wins
//! when the project writes one.
//!
//! Either way the tree drives the same four things. `.alloy/
//! build.project.json` is the tree over the compiled output, the one
//! `rojo serve` and `rojo build` take. `sourcemap.json` at the root is
//! the instance tree with the source paths, under the name Rojo and
//! luau-lsp read, which the language server maps onto its mirror. A `require("@alias/x")` in the ship artifact
//! becomes an instance path, because Roblox reads no `.luaurc`. And
//! `alloy.luau` lands at the runtime's place. The mount table also
//! writes `default.project.json`; a root with a project file keeps the
//! one it wrote.

use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::rojo::{Mounted, ProjectFile};

/// What `.alloy/.gitignore` holds: the files under `.alloy` that no
/// repository wants. The ingot store is build output; the lock file
/// beside it is not, so it stays in version control.
pub const ALLOY_DIR_IGNORE: &str = "sourcemap.json\ningots/\noutputs.txt\n";

/// The DataModel path of a mount, split: `@game/A/B` is `["A", "B"]`.
/// `None` when the string does not start with `@game/`.
pub fn segments(mount: &str) -> Option<Vec<String>> {
    let rest = mount.strip_prefix("@game/")?;
    let parts: Vec<String> = rest
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();

    if parts.is_empty() { None } else { Some(parts) }
}

/// Where the runtime lands when `[project] runtime` names no place:
/// where a node already mounts `out/alloy.luau`, else inside the node
/// that mounts the output folder, or the input folder, which the build
/// project points at the output. That folder carries the file, and a
/// node of its own would put a second copy beside it. Else the default
/// place.
fn runtime_place(mounts: &[Mounted], input: &Path, out: &Path) -> Vec<String> {
    let at = |disk: &Path| {
        mounts
            .iter()
            .find(|m| m.disk == disk)
            .map(|m| m.place.clone())
    };

    at(&out.join("alloy.luau"))
        .or_else(|| {
            at(out).or_else(|| at(input)).map(|mut place| {
                place.push("alloy".to_string());
                place
            })
        })
        .or_else(|| segments(crate::config::DEFAULT_RUNTIME))
        .unwrap_or_default()
}

/// Whether a folder of `disks` already carries the runtime at
/// `runtime`: `alloy` inside the node that mounts one of them. A
/// project file that writes a node for it too holds two copies.
pub(crate) fn carries_runtime(mounts: &[Mounted], disks: &[&Path], runtime: &[String]) -> bool {
    runtime.split_last().is_some_and(|(last, parent)| {
        last == "alloy"
            && mounts
                .iter()
                .any(|m| m.place == parent && disks.contains(&m.disk.as_path()))
    })
}

/// The tree of one project, read once per build.
#[derive(Debug, Clone, Default)]
pub struct Tree {
    /// The name in the generated project files.
    pub name: String,
    /// Each folder or file on disk, relative to the root, with the
    /// instance path it lands at.
    pub mounts: Vec<Mounted>,
    /// Where `alloy.luau` lands, as instance names. Empty when the
    /// project names no place for it.
    pub runtime: Vec<String>,
    /// Alias to the folder it names, relative to the root: the Luau
    /// configuration of the root, then the `[mount]` table while
    /// `[project] mount_aliases` stays on. A name in the Luau
    /// configuration wins.
    pub aliases: Vec<(String, PathBuf)>,
    /// The project file, when it is the tree.
    pub project: Option<ProjectFile>,
    /// Whether `alloy build` writes the Rojo projects of the mount
    /// table. A project file always writes the build project alone.
    source_of_truth: bool,
    input: PathBuf,
    out: PathBuf,
}

impl Tree {
    /// Reads the tree of a root: the `[mount]` table when the project
    /// wrote one, else the project file at the root, else nothing.
    pub fn load(root: &Path, config: &Config) -> Self {
        let input = config.build.input.clone();
        let out = config.build.out.clone();
        let luau: Vec<(String, PathBuf)> = crate::luau_config::read_dir(root)
            .map(|(_, c)| {
                c.aliases
                    .into_iter()
                    .map(|(a, p)| (a, PathBuf::from(p.replace('\\', "/"))))
                    .collect()
            })
            .unwrap_or_default();

        // The Luau configuration names the aliases; the mount table adds
        // the names it lacks, so `@shared/x` still resolves in a project
        // that declares its tree in alloy.toml alone.
        let mut aliases = luau;

        if config.project.mount_aliases {
            for (name, m) in &config.mount {
                if !aliases.iter().any(|(a, _)| a == name) {
                    aliases.push((name.clone(), PathBuf::from(m.0.replace('\\', "/"))));
                }
            }
        }

        // An alias-only entry names no place, so a table of those alone
        // is not a tree: the project file at the root still is.
        if config.mount.values().any(|m| !m.alias_only()) {
            let mounts: Vec<Mounted> = config
                .mount
                .values()
                .filter_map(|m| {
                    Some(Mounted {
                        place: segments(&m.1)?,
                        disk: PathBuf::from(m.0.replace('\\', "/")),
                    })
                })
                .collect();
            let runtime = match &config.project.runtime {
                Some(r) => segments(r).unwrap_or_default(),

                None => runtime_place(&mounts, &input, &out),
            };

            return Self {
                name: config.project.name.clone(),
                mounts,
                runtime,
                aliases,
                project: None,
                source_of_truth: config.project.source_of_truth,
                input,
                out,
            };
        }

        let project = crate::rojo::load(root, config.project.file.as_deref());
        let mounts = project
            .as_ref()
            .map(ProjectFile::mounts)
            .unwrap_or_default();
        // The runtime lands where the project says, else as
        // `runtime_place` finds it.
        let runtime = match &config.project.runtime {
            Some(r) => segments(r).unwrap_or_default(),

            None => runtime_place(&mounts, &input, &out),
        };

        Self {
            name: project
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| config.project.name.clone()),
            mounts,
            runtime,
            aliases,
            project,
            source_of_truth: true,
            input,
            out,
        }
    }

    /// The output folders the build project names, relative to the root:
    /// each folder mount under `[build] in`, moved under `[build] out`.
    /// A mount with no source in it has no output, and `rojo build`
    /// stops at the missing path, so the build makes each folder.
    pub fn out_dirs(&self, root: &Path) -> Vec<PathBuf> {
        if self.project.is_none() && !self.source_of_truth {
            return Vec::new();
        }

        self.mounts
            .iter()
            .filter(|m| m.disk.extension().is_none() && !root.join(&m.disk).is_file())
            .filter_map(|m| m.disk.strip_prefix(&self.input).ok())
            .map(|rest| self.out.join(rest))
            .collect()
    }

    /// The mount that holds `rel`, the one with the longest disk path,
    /// with the rest of `rel` under it.
    fn holder(&self, rel: &Path) -> Option<(&Mounted, PathBuf)> {
        let mut best: Option<(&Mounted, PathBuf)> = None;

        for m in &self.mounts {
            if let Ok(rest) = rel.strip_prefix(&m.disk)
                && best
                    .as_ref()
                    .is_none_or(|(b, _)| b.disk.components().count() < m.disk.components().count())
            {
                best = Some((m, rest.to_path_buf()));
            }
        }

        best
    }
}

/// The instance name of a script file: the stem with `.server`,
/// `.client`, and `.d` removed. `init` names its directory.
fn instance_name(file: &str) -> Option<String> {
    // A data file is a ModuleScript to Rojo and to the build alike; a
    // project file beside the sources is neither.
    if crate::data::is_project_file(Path::new(file)) {
        return None;
    }

    let stem = file
        .strip_suffix(".aly")
        .or_else(|| file.strip_suffix(".alx"))
        .or_else(|| file.strip_suffix(".luau"))
        .or_else(|| file.strip_suffix(".lua"))
        .or_else(|| file.strip_suffix(".json"))
        .or_else(|| file.strip_suffix(".toml"))?;
    let stem = stem
        .strip_suffix(".server")
        .or_else(|| stem.strip_suffix(".client"))
        .or_else(|| stem.strip_suffix(".d"))
        .unwrap_or(stem);

    if stem == "init" {
        None
    } else {
        Some(stem.to_string())
    }
}

/// The instance name of the last part of a path: a script's name, or
/// the part itself when it names a folder. `None` for an `init` file,
/// which names the folder that is already there.
fn leaf_name(part: &str) -> Option<String> {
    instance_name(part).or_else(|| {
        if part.contains('.') {
            None
        } else {
            Some(part.to_string())
        }
    })
}

/// The class of a node between a service and a leaf: a folder, except
/// the containers Roblox names, which are their own class.
pub(crate) fn container_class(name: &str) -> &str {
    match name {
        "StarterPlayerScripts" | "StarterCharacterScripts" | "StarterCharacter" => name,

        _ => "Folder",
    }
}

/// The Roblox class of a script file.
pub(crate) fn script_class(file: &str) -> &'static str {
    // The side is the last word of the stem. `main.server.globals.luau`
    // is a module the build wrote beside a script, not a script.
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);

    if stem.ends_with(".server") {
        "Script"
    } else if stem.ends_with(".client") {
        "LocalScript"
    } else {
        "ModuleScript"
    }
}

/// The instance path of a path under a mount: the mount's place, the
/// folders under it, and the leaf's instance name.
fn place_of(tree: &Tree, rel: &Path) -> Option<Vec<String>> {
    let (m, rest) = tree.holder(rel)?;
    let mut out = m.place.clone();
    let parts: Vec<&str> = rest
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => n.to_str(),

            _ => None,
        })
        .collect();

    for (i, part) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            if let Some(name) = leaf_name(part) {
                out.push(name);
            }
        } else {
            out.push((*part).to_string());
        }
    }

    Some(out)
}

/// The DataModel path of a source file, relative to the project root.
/// `None` when the tree holds no folder above the file.
pub fn instance_path(tree: &Tree, rel: &Path) -> Option<Vec<String>> {
    place_of(tree, rel)
}

/// The side the place of a file gives it. Code under
/// `ServerScriptService` or `ServerStorage` runs on the server alone, and
/// code under `StarterPlayerScripts` or `StarterGui` on the client alone.
/// Any other place is shared.
pub fn place_side(tree: &Tree, rel: &Path) -> Option<crate::directives::Side> {
    let place = place_of(tree, rel)?;
    let names: Vec<&str> = place.iter().map(String::as_str).collect();

    match names.as_slice() {
        ["ServerScriptService" | "ServerStorage", ..] => Some(crate::directives::Side::Server),

        ["StarterGui", ..] | ["StarterPlayer", "StarterPlayerScripts", ..] => {
            Some(crate::directives::Side::Client)
        }

        _ => None,
    }
}

/// The require string for the runtime in the ship artifact of a file in
/// the tree: the runtime's own `@game/...` path, which Luau takes as it
/// is. `None` when the file is outside the tree, or the tree names no
/// place for the runtime.
pub fn std_require_for(tree: &Tree, rel: &Path) -> Option<String> {
    instance_path(tree, rel)?;

    if tree.runtime.is_empty() {
        return None;
    }

    Some(format!("@game/{}", tree.runtime.join("/")))
}

/// The require string for `@alias/rest`: the alias names a folder on
/// disk, the tree says where that folder lands, and the rest of the way
/// down becomes instance names. `from` is the file the require starts
/// from.
///
/// A script in a folder that Roblox copies runs from its copy, so the
/// `@game/...` place of the module is the template, a second module
/// with its own state. From a file of the same mount, a relative path
/// finds the copy the script runs in. The instance path of the copy
/// holds the player's name, so no other file has a path to it.
pub fn resolve_alias(tree: &Tree, from: &Path, alias: &str, rest: &str) -> Option<String> {
    let (_, dir) = tree.aliases.iter().find(|(a, _)| a == alias)?;
    let joined = normalize(&dir.join(rest.trim_start_matches('/')));
    let place = format!("@game/{}", place_of(tree, &joined)?.join("/"));
    let same_mount = tree
        .holder(from)
        .zip(tree.holder(&joined))
        .is_some_and(|((a, _), (b, _))| std::ptr::eq(a, b));

    if same_mount && in_copied_folder(&place) {
        return Some(crate::build::relative_require(from, &joined));
    }

    Some(place)
}

/// Whether a `@game/...` require names a place in a folder that Roblox
/// copies for each player or character. A script there runs from its
/// copy, so a require of the place loads a second module.
pub fn in_copied_folder(require: &str) -> bool {
    let Some(place) = require.strip_prefix("@game/") else {
        return false;
    };
    let names: Vec<&str> = place.split('/').collect();

    matches!(
        names.as_slice(),
        [
            "StarterPlayer",
            "StarterPlayerScripts" | "StarterCharacterScripts",
            ..
        ] | ["StarterGui" | "StarterPack", ..]
    )
}

/// A path with `.` and `..` folded, no file system access.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in path.components() {
        match c {
            Component::CurDir => {}

            Component::ParentDir => {
                out.pop();
            }

            other => out.push(other),
        }
    }

    out
}

/// Rewrites every `require("@alias/...")` in an emitted text to the
/// `@game/...` instance path, for the aliases the Luau configuration
/// names, and drops the extension of a data path, `./x.json`, since the
/// build writes it as `x.luau`. The text keeps its line count: a
/// replacement holds no newline.
///
/// `source` is the file's path from the root. A relative require that
/// leaves the file's mount becomes an instance path too, since the
/// folders on disk and the instances past a mount differ; `@alloy` is
/// the runtime's place.
pub fn rewrite_requires(tree: &Tree, source: &Path, text: &str) -> String {
    map_requires(text, |path| {
        let replaced = match path.strip_prefix('@') {
            Some("alloy") if !tree.runtime.is_empty() => {
                Some(format!("@game/{}", tree.runtime.join("/")))
            }

            Some(p) => {
                let (alias, tail) = p.split_once('/').unwrap_or((p, ""));

                resolve_alias(tree, &crate::build::module_base(source), alias, tail)
            }

            None => cross_mount(tree, source, &crate::build::module_base(source), path),
        };

        Some(crate::data::strip_spec(replaced.as_deref().unwrap_or(path)).to_string())
    })
}

/// The require path of a relative spec that leaves the mount of the
/// file at `source`. The spec starts from the folder of `from`. A target
/// in another mount takes its `@game/...` place. A spec that climbs out
/// of its own mount and back in takes the path inside the mount. The
/// instance of a mount has another name than its folder, `Shared` for
/// `src/shared`, so the spec as written finds nothing. `None` for any
/// other path.
fn cross_mount(tree: &Tree, source: &Path, from: &Path, path: &str) -> Option<String> {
    if !path.starts_with("./") && !path.starts_with("../") {
        return None;
    }

    let spec = crate::data::strip_spec(path);
    let dir = from.parent().unwrap_or(Path::new(""));
    let target = normalize(&dir.join(spec));
    let home = tree.holder(source)?.0;
    let there = tree.holder(&target)?.0;

    if !std::ptr::eq(home, there) {
        return place_of(tree, &target).map(|p| format!("@game/{}", p.join("/")));
    }

    let mut depth = dir.strip_prefix(&home.disk).ok()?.components().count();
    let climbs_out = Path::new(spec).components().any(|c| match c {
        Component::ParentDir if depth == 0 => true,

        Component::ParentDir => {
            depth -= 1;

            false
        }

        Component::Normal(_) => {
            depth += 1;

            false
        }

        _ => false,
    });

    climbs_out.then(|| crate::build::relative_require(from, &target))
}

/// Each import spec of `text` that leaves the mount of the file at
/// `source`, with the path its `require` writes: see `cross_mount`. The
/// check artifact writes that path, as the ship does: luau-lsp reads a
/// relative path in a file the sourcemap holds as a place in the tree,
/// where two mounts are no siblings. An alias into a folder that Roblox
/// copies takes its path here too: see `resolve_alias`.
pub fn mount_requires(tree: &Tree, source: &Path, text: &str) -> Vec<(String, String)> {
    crate::modules::import_specs(text)
        .into_iter()
        .filter_map(|spec| {
            let place = match spec.strip_prefix('@').and_then(|p| p.split_once('/')) {
                Some((alias, tail)) => resolve_alias(tree, source, alias, tail)
                    .filter(|p| !p.starts_with("@game/") || in_copied_folder(p))?,

                None => cross_mount(tree, source, source, &spec)?,
            };

            Some((spec, place))
        })
        .collect()
}

/// Rewrites the path of every `require("...")` of a text through `f`,
/// which answers `None` for a path it leaves alone. The text keeps its
/// line count, so `f` returns no newline.
pub fn map_requires(text: &str, f: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(i) = rest.find("require(") {
        let after = &rest[i + "require(".len()..];
        let quote = after.chars().next();

        if !matches!(quote, Some('"' | '\'')) {
            out.push_str(&rest[..i + "require(".len()]);
            rest = after;

            continue;
        }

        let q = quote.unwrap_or('"');
        let body = &after[1..];
        let Some(end) = body.find(q) else {
            out.push_str(&rest[..i + "require(".len()]);
            rest = after;

            continue;
        };
        let path = &body[..end];

        out.push_str(&rest[..i + "require(".len()]);
        out.push(q);
        out.push_str(f(path).as_deref().unwrap_or(path));
        out.push(q);
        rest = &body[end + 1..];
    }

    out.push_str(rest);

    out
}

/// A path as the project file sees it: relative to `base`, the
/// directory the file lives in, given both are under `root`.
pub(crate) fn from_base(root: &Path, base: &Path, path: &Path) -> String {
    let depth = base
        .strip_prefix(root)
        .map(|r| r.components().count())
        .unwrap_or(0);
    let mut out = PathBuf::new();

    for _ in 0..depth {
        out.push("..");
    }

    out.push(path);
    out.to_string_lossy().replace('\\', "/")
}

/// Inserts `leaf` at the DataModel path `segs` of a Rojo tree. The
/// first segment is a service, the ones between are folders.
pub(crate) fn insert(tree: &mut Map<String, Value>, segs: &[String], leaf: Value) {
    let mut node = tree;

    for (i, seg) in segs.iter().enumerate() {
        let last = i + 1 == segs.len();
        let entry = node
            .entry(seg.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(map) = entry else { return };

        if last {
            if let Value::Object(leaf) = &leaf {
                for (k, v) in leaf {
                    map.insert(k.clone(), v.clone());
                }
            }
        } else if !map.contains_key("$className") && !map.contains_key("$path") {
            let class = if i == 0 {
                seg.as_str()
            } else {
                container_class(seg)
            };
            map.insert("$className".into(), Value::String(class.to_string()));
        }

        node = map;
    }
}

/// A Rojo project over the mount table. `compiled` points the paths at
/// the build output for a folder under `[build] in`; `base` is the
/// directory the file will live in.
pub fn rojo_project(tree: &Tree, root: &Path, base: &Path, compiled: bool) -> Value {
    let mut out = Map::new();
    out.insert("$className".into(), Value::String("DataModel".into()));

    for m in &tree.mounts {
        let shown = match (compiled, m.disk.strip_prefix(&tree.input)) {
            (true, Ok(rest)) => tree.out.join(rest),

            _ => m.disk.clone(),
        };
        let leaf = json!({ "$path": from_base(root, base, &shown) });
        insert(&mut out, &m.place, leaf);
    }

    // The build project points a mount of `[build] in` at the output,
    // so that folder carries the runtime there too.
    let carriers: &[&Path] = match compiled {
        true => &[&tree.out, &tree.input],

        false => &[&tree.out],
    };
    let runtime = tree.out.join("alloy.luau");
    let mounted = tree.mounts.iter().any(|m| m.disk == runtime)
        || carries_runtime(&tree.mounts, carriers, &tree.runtime);

    if !tree.runtime.is_empty() && !mounted {
        insert(
            &mut out,
            &tree.runtime,
            json!({ "$path": from_base(root, base, &runtime) }),
        );
    }

    json!({ "name": tree.name, "tree": Value::Object(out) })
}

/// One node of a sourcemap.
pub(crate) fn node(name: &str, class: &str, file: Option<String>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("name".into(), Value::String(name.to_string()));
    m.insert("className".into(), Value::String(class.to_string()));

    if let Some(f) = file {
        m.insert("filePaths".into(), json!([f]));
    }

    m
}

/// The sourcemap node for a directory on disk, with the source paths
/// relative to `root`. An `init` file makes the directory a script.
pub(crate) fn dir_node(root: &Path, dir: &Path, name: &str) -> std::io::Result<Map<String, Value>> {
    let mut children: Vec<Value> = Vec::new();
    let mut class = "Folder".to_string();
    let mut file: Option<String> = None;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();

    for path in entries {
        let fname = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();

        // `.ember` holds the packages a `packages/` stub requires by
        // relative path, so it belongs in the tree.
        if (fname.starts_with('.') && fname != ".ember") || fname == "node_modules" {
            continue;
        }

        if path.is_dir() {
            children.push(Value::Object(dir_node(root, &path, &fname)?));

            continue;
        }

        if fname.contains(".d.") {
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        match instance_name(&fname) {
            Some(n) => {
                let mut m = node(&n, script_class(&fname), Some(rel));
                m.insert("children".into(), json!([]));
                children.push(Value::Object(m));
            }

            None if fname.starts_with("init.") => {
                class = script_class(&fname).to_string();
                file = Some(rel);
            }

            None => {}
        }
    }

    let mut m = node(name, &class, file);
    m.insert("children".into(), Value::Array(children));

    Ok(m)
}

/// The sourcemap of the tree: the instance tree, each script with the
/// path of its source, relative to `root`.
pub fn sourcemap(tree: &Tree, root: &Path) -> std::io::Result<Value> {
    if let Some(project) = &tree.project {
        return project.sourcemap(root, &tree.runtime, &tree.out);
    }

    let mut game = node("game", "DataModel", None);
    let mut children: Vec<Value> = Vec::new();

    let mut place = |segs: &[String], leaf: Map<String, Value>| {
        // Walks or creates the services and folders down to the leaf.
        fn descend<'a>(list: &'a mut Vec<Value>, name: &str, class: &str) -> &'a mut Vec<Value> {
            let at = list.iter().position(|c| c["name"] == name);
            let at = match at {
                Some(i) => i,

                None => {
                    let mut m = node(name, class, None);
                    m.insert("children".into(), json!([]));
                    list.push(Value::Object(m));
                    list.len() - 1
                }
            };

            list[at]["children"]
                .as_array_mut()
                .expect("children is an array")
        }

        let mut list = &mut children;

        for (i, seg) in segs.iter().enumerate() {
            if i + 1 == segs.len() {
                let mut leaf = leaf.clone();
                leaf.insert("name".into(), Value::String(seg.clone()));

                if let Some(at) = list.iter().position(|c| c["name"] == seg.as_str()) {
                    list[at] = Value::Object(leaf);
                } else {
                    list.push(Value::Object(leaf));
                }

                return;
            }

            let class = if i == 0 {
                seg.as_str()
            } else {
                container_class(seg)
            };
            list = descend(list, seg, class);
        }
    };

    for m in &tree.mounts {
        let path = root.join(&m.disk);
        let name = m.place.last().cloned().unwrap_or_default();

        let leaf = if path.is_dir() {
            dir_node(root, &path, &name)?
        } else if path.is_file() {
            let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            let mut n = node(
                &name,
                script_class(fname),
                Some(m.disk.to_string_lossy().replace('\\', "/")),
            );
            n.insert("children".into(), json!([]));
            n
        } else {
            continue;
        };

        place(&m.place, leaf);
    }

    if !tree.runtime.is_empty() {
        let file = tree
            .out
            .join("alloy.luau")
            .to_string_lossy()
            .replace('\\', "/");
        let mut n = node(
            tree.runtime.last().map(String::as_str).unwrap_or("Alloy"),
            "ModuleScript",
            Some(file),
        );
        n.insert("children".into(), json!([]));
        place(&tree.runtime, n);
    }

    game.insert("children".into(), Value::Array(children));

    Ok(Value::Object(game))
}

/// The sourcemap luau-lsp reads for the project, before its paths move
/// into a mirror. A tree that mounts a folder writes it, as `alloy
/// build` does, so a file added since the last build has its place.
/// Any other root reads the file the last build or Rojo left there.
/// The language server and `alloy flux` both start from this text.
pub fn luau_sourcemap(root: &Path, config: &Config) -> Option<String> {
    let tree = Tree::load(root, config);

    if !tree.mounts.is_empty() {
        let map = sourcemap(&tree, root).ok()?;

        return Some(serde_json::to_string_pretty(&map).ok()? + "\n");
    }

    // A root that still holds the `.alloy/sourcemap.json` an older build
    // wrote uses that one.
    ["sourcemap.json", ".alloy/sourcemap.json"]
        .iter()
        .find_map(|name| std::fs::read_to_string(root.join(name)).ok())
}

/// A sourcemap with each script path passed through `f`, as luau-lsp
/// reads it. A text that does not parse comes back as it is.
pub fn map_sourcemap(text: &str, f: &dyn Fn(&str) -> String) -> String {
    fn walk(v: &mut Value, f: &dyn Fn(&str) -> String) {
        match v {
            Value::Array(items) => items.iter_mut().for_each(|i| walk(i, f)),

            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    match (k.as_str(), v) {
                        ("filePaths", Value::Array(paths)) => {
                            for p in paths.iter_mut() {
                                if let Value::String(s) = p {
                                    *s = f(s);
                                }
                            }
                        }

                        ("children", Value::Array(kids)) => {
                            kids.iter_mut().for_each(|k| walk(k, f));
                            distinct_scripts(kids);
                        }

                        (_, v) => walk(v, f),
                    }
                }
            }

            _ => {}
        }
    }

    let Ok(mut json) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    walk(&mut json, f);

    serde_json::to_string(&json).unwrap_or_else(|_| text.to_string())
}

/// luau-lsp names a module by its instance path, so two siblings of one
/// name are one module to it: `duel.client.aly` then checks the text of
/// `duel.server.aly`. No code requires a script, so a script that shares
/// its name with a sibling takes its side as well, `duel.client`.
fn distinct_scripts(kids: &mut [Value]) {
    let names: Vec<Option<String>> = kids
        .iter()
        .map(|k| k["name"].as_str().map(str::to_string))
        .collect();

    for (kid, name) in kids.iter_mut().zip(&names) {
        let side = match kid["className"].as_str() {
            Some("Script") => "server",

            Some("LocalScript") => "client",

            _ => continue,
        };

        if let Some(name) = name
            && names.iter().filter(|n| n.as_ref() == Some(name)).count() > 1
        {
            kid["name"] = Value::String(format!("{name}.{side}"));
        }
    }
}

/// The Luau file luau-lsp reads for a script path of a sourcemap. An
/// Alloy source compiles to `.luau`, and a data file becomes a module,
/// as in the build. Any other path stays.
pub fn luau_script_path(path: &str) -> String {
    if let Some(b) = path.strip_suffix(".d.aly") {
        format!("{b}.d.luau")
    } else if let Some(b) = path
        .strip_suffix(".aly")
        .or_else(|| path.strip_suffix(".alx"))
        .or_else(|| path.strip_suffix(".json"))
        .or_else(|| path.strip_suffix(".toml"))
    {
        format!("{b}.luau")
    } else {
        path.to_string()
    }
}

/// The files `alloy build` writes for the tree, as (path relative to
/// the root, text). A root whose tree is its own project file keeps
/// that file: Alloy writes only the build project and the sourcemap.
pub fn files(tree: &Tree, config: &Config, root: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    if tree.mounts.is_empty() {
        return Ok(Vec::new());
    }

    let alloy_dir = root.join(".alloy");
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
    let mut out = Vec::new();

    match &tree.project {
        // The root wrote the project file, so Alloy writes only the
        // build project beside it.
        Some(p) => out.push((
            PathBuf::from(".alloy/build.project.json"),
            pretty(&p.build_tree(root, &alloy_dir, &tree.input, &tree.out, &tree.runtime)),
        )),

        // The mount table is the tree only while the project says so; a
        // sync tool with its own format writes the project files itself.
        None if tree.source_of_truth => {
            out.push((
                PathBuf::from("default.project.json"),
                pretty(&rojo_project(tree, root, root, false)),
            ));
            out.push((
                PathBuf::from(".alloy/build.project.json"),
                pretty(&rojo_project(tree, root, &alloy_dir, true)),
            ));
        }

        None => {}
    }

    out.push((
        PathBuf::from(".alloy/.gitignore"),
        ALLOY_DIR_IGNORE.to_string(),
    ));

    // The sourcemap sits at the root under the name Rojo writes and
    // luau-lsp reads, so a tool that looks for one finds this one.
    // `[project] sourcemap = false` writes none, and leaves a file
    // another tool wrote as it is.
    if config.project.sourcemap {
        out.push((
            PathBuf::from("sourcemap.json"),
            pretty(&sourcemap(tree, root)?),
        ));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = r#"
[project]
name = "demo"

[mount]
server = ["src/server", "@game/ServerScriptService/Server"]
shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
pkg = ["Packages", "@game/ReplicatedStorage/Packages"]
"#;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alloy-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        let path = dir.join(name);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        std::fs::write(path, text).unwrap();
    }

    /// The tree of the mount table above, with the aliases it carries.
    fn mounted() -> Tree {
        let config = Config::parse(MOUNTS, Path::new("alloy.toml")).unwrap();

        Tree::load(Path::new("/does-not-exist"), &config)
    }

    #[test]
    fn a_file_under_a_mount_has_an_instance_path() {
        let t = mounted();
        assert_eq!(
            instance_path(&t, Path::new("src/server/combat/hit.aly")).unwrap(),
            vec!["ServerScriptService", "Server", "combat", "hit"]
        );
        assert_eq!(
            instance_path(&t, Path::new("src/server/init.server.aly")).unwrap(),
            vec!["ServerScriptService", "Server"]
        );
        assert!(instance_path(&t, Path::new("src/other.aly")).is_none());
    }

    #[test]
    fn the_runtime_require_is_the_game_path() {
        let t = mounted();
        assert_eq!(
            std_require_for(&t, Path::new("src/server/combat/hit.aly")).unwrap(),
            "@game/ReplicatedStorage/Alloy"
        );
        assert!(std_require_for(&t, Path::new("src/other.aly")).is_none());
    }

    #[test]
    fn an_alias_require_becomes_a_game_path() {
        let t = mounted();
        let text = "local jecs = require(\"@pkg/jecs\") local u = require(\"@shared/util\") local x = require(\"./x\")";
        let out = rewrite_requires(&t, Path::new(""), text);
        assert_eq!(
            out,
            "local jecs = require(\"@game/ReplicatedStorage/Packages/jecs\") local u = require(\"@game/ReplicatedStorage/Shared/util\") local x = require(\"./x\")"
        );
        assert_eq!(
            rewrite_requires(
                &Tree::default(),
                Path::new(""),
                "local d = require(\"./data.json\") local c = require('../cfg.toml')\n"
            ),
            "local d = require(\"./data\") local c = require('../cfg')\n"
        );
        assert_eq!(
            rewrite_requires(&t, Path::new(""), "require(\"@shared/b\")"),
            "require(\"@game/ReplicatedStorage/Shared/b\")"
        );
        // A data path under an alias keeps the module name.
        assert_eq!(
            resolve_alias(&t, Path::new(""), "shared", "data/config.json").unwrap(),
            "@game/ReplicatedStorage/Shared/data/config"
        );
    }

    /// A LocalScript runs from the copy of StarterPlayerScripts in the
    /// player, so the `@game/...` place of a client module is a second
    /// module. Its own mount reaches the copy by a relative path, and
    /// the compile reports a require from another mount.
    #[test]
    fn an_alias_into_a_copied_folder_takes_the_copy() {
        let config = Config::parse(
            &format!("{MOUNTS}client = [\"src/client\", \"@game/StarterPlayer/StarterPlayerScripts/Client\"]\n"),
            Path::new("alloy.toml"),
        )
        .unwrap();
        let t = Tree::load(Path::new("/does-not-exist"), &config);
        let text = "import { a } from '@client/state'\nimport { b } from '@shared/util'\n";

        assert_eq!(
            mount_requires(&t, Path::new("src/client/ui/hud.aly"), text),
            [("@client/state".to_string(), "../state".to_string())]
        );
        assert_eq!(
            rewrite_requires(
                &t,
                Path::new("src/client/main.client.aly"),
                "require('@client/state')"
            ),
            "require('./state')"
        );

        let place = "@game/StarterPlayer/StarterPlayerScripts/Client/state";
        let requires = mount_requires(&t, Path::new("src/shared/util.aly"), text);
        assert_eq!(requires, [("@client/state".to_string(), place.to_string())]);

        let options = crate::EmitOptions {
            mount_requires: requires,
            ..Default::default()
        };
        let out = crate::compile_with(text, &options).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages[0].starts_with("\"@client/state\" is in a folder that Roblox copies"),
            "{messages:?}"
        );
        assert_eq!(
            out.diagnostics[0].start as usize,
            text.find("'@client").unwrap()
        );
    }

    #[test]
    fn the_two_projects_point_at_sources_and_output() {
        let t = mounted();
        let root = Path::new("/p");
        let src = rojo_project(&t, root, root, false);
        assert_eq!(src["name"], "demo");
        assert_eq!(
            src["tree"]["ServerScriptService"]["$className"],
            "ServerScriptService"
        );
        assert_eq!(
            src["tree"]["ServerScriptService"]["Server"]["$path"],
            "src/server"
        );
        assert_eq!(
            src["tree"]["ReplicatedStorage"]["Packages"]["$path"],
            "Packages"
        );
        assert_eq!(
            src["tree"]["ReplicatedStorage"]["Alloy"]["$path"],
            "build/alloy.luau"
        );

        let build = rojo_project(&t, root, &root.join(".alloy"), true);
        assert_eq!(
            build["tree"]["ServerScriptService"]["Server"]["$path"],
            "../build/server"
        );
        assert_eq!(
            build["tree"]["ReplicatedStorage"]["Packages"]["$path"],
            "../Packages"
        );
        assert_eq!(
            build["tree"]["ReplicatedStorage"]["Alloy"]["$path"],
            "../build/alloy.luau"
        );
    }

    #[test]
    fn the_sourcemap_lands_at_the_root() {
        let dir = temp("sourcemap-root");
        write(&dir, "src/shared/util.aly", "");
        let mut config = Config::parse(MOUNTS, Path::new("alloy.toml")).unwrap();
        let tree = Tree::load(&dir, &config);
        let names = |config: &Config| -> Vec<String> {
            files(&tree, config, &dir)
                .unwrap()
                .iter()
                .map(|(p, _)| p.to_string_lossy().replace('\\', "/"))
                .collect()
        };
        // Rojo and luau-lsp read `sourcemap.json` at the root, so the
        // build writes that name and no other.
        assert!(names(&config).contains(&"sourcemap.json".to_string()));
        assert!(!names(&config).contains(&".alloy/sourcemap.json".to_string()));

        config.project.sourcemap = false;
        assert!(!names(&config).contains(&"sourcemap.json".to_string()));
    }

    #[test]
    fn the_sourcemap_names_scripts_by_suffix() {
        let dir = temp("sourcemap");
        write(&dir, "src/server/init.server.aly", "");
        write(&dir, "src/server/combat/hit.aly", "");
        write(&dir, "src/shared/util.aly", "");
        write(&dir, "src/shared/ui.client.aly", "");
        write(&dir, "Packages/jecs.luau", "");

        let config = Config::parse(MOUNTS, Path::new("alloy.toml")).unwrap();
        let map = sourcemap(&Tree::load(&dir, &config), &dir).unwrap();
        let services = map["children"].as_array().unwrap();
        let sss = services
            .iter()
            .find(|s| s["name"] == "ServerScriptService")
            .unwrap();
        let server = &sss["children"][0];
        assert_eq!(server["className"], "Script");
        assert_eq!(server["filePaths"][0], "src/server/init.server.aly");
        let combat = &server["children"][0];
        assert_eq!(combat["className"], "Folder");
        assert_eq!(combat["children"][0]["name"], "hit");
        assert_eq!(
            combat["children"][0]["filePaths"][0],
            "src/server/combat/hit.aly"
        );

        let rs = services
            .iter()
            .find(|s| s["name"] == "ReplicatedStorage")
            .unwrap();
        let names: Vec<&str> = rs["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"Shared") && names.contains(&"Packages") && names.contains(&"Alloy")
        );
        let shared = rs["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "Shared")
            .unwrap();
        let ui = shared["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "ui")
            .unwrap();
        assert_eq!(ui["className"], "LocalScript");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// luau-lsp keys a module on its instance path, so `duel.client`
    /// and `duel.server` must not share one. A module keeps its name.
    #[test]
    fn scripts_of_one_stem_reach_luau_lsp_apart() {
        let text = r#"{"name":"game","children":[
            {"name":"duel","className":"LocalScript","filePaths":["src/duel.client.aly"]},
            {"name":"duel","className":"Script","filePaths":["src/duel.server.aly"]},
            {"name":"duel","className":"ModuleScript","filePaths":["src/duel.aly"]},
            {"name":"hud","className":"LocalScript","filePaths":["src/hud.client.aly"]}]}"#;
        let map: Value = serde_json::from_str(&map_sourcemap(text, &luau_script_path)).unwrap();
        let names: Vec<&str> = map["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["duel.client", "duel.server", "duel", "hud"]);
        assert_eq!(map["children"][0]["filePaths"][0], "src/duel.client.luau");
    }

    /// A root whose tree is its own project file, with the aliases in
    /// `.luaurc`.
    fn with_project_file(dir: &Path) -> Tree {
        write(
            dir,
            "default.project.json",
            r#"{
  "name": "place",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "$className": "ReplicatedStorage",
      "Shared": { "$path": "src/shared" },
      "Packages": { "$path": "Packages" },
      "Alloy": { "$path": "build/alloy.luau" }
    },
    "ServerScriptService": {
      "$className": "ServerScriptService",
      "Server": { "$path": "src/server" }
    },
    "StarterPlayer": {
      "$className": "StarterPlayer",
      "StarterPlayerScripts": {
        "$className": "StarterPlayerScripts",
        "Client": { "$path": "src/client" }
      }
    }
  }
}"#,
        );
        write(
            dir,
            ".luaurc",
            r#"{ "languageMode": "strict", "aliases": {
                 "server": "src/server", "shared": "src/shared",
                 "client": "src/client", "pkg": "Packages",
                 "lest": ".lest/core" } }"#,
        );

        Tree::load(dir, &Config::default())
    }

    #[test]
    fn a_project_file_is_the_tree() {
        let dir = temp("file");
        let t = with_project_file(&dir);
        assert_eq!(t.name, "place");
        assert!(t.project.is_some());
        assert_eq!(
            instance_path(&t, Path::new("src/shared/util.aly")).unwrap(),
            vec!["ReplicatedStorage", "Shared", "util"]
        );
        assert_eq!(
            instance_path(&t, Path::new("src/client/ui/hud.client.aly")).unwrap(),
            vec![
                "StarterPlayer",
                "StarterPlayerScripts",
                "Client",
                "ui",
                "hud"
            ]
        );
        assert_eq!(
            instance_path(&t, Path::new("src/server/init.server.aly")).unwrap(),
            vec!["ServerScriptService", "Server"]
        );
        // A folder outside `[build] in` still lands where the file says.
        assert_eq!(
            instance_path(&t, Path::new("Packages/jecs.luau")).unwrap(),
            vec!["ReplicatedStorage", "Packages", "jecs"]
        );
        assert!(instance_path(&t, Path::new("tools/gen.aly")).is_none());
        // The tree already mounts `build/alloy.luau`, so that is the
        // runtime's place.
        assert_eq!(t.runtime, vec!["ReplicatedStorage", "Alloy"]);
        assert_eq!(
            std_require_for(&t, Path::new("src/shared/util.aly")).unwrap(),
            "@game/ReplicatedStorage/Alloy"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_alias_resolves_through_the_luau_configuration() {
        let dir = temp("alias-luaurc");
        let t = with_project_file(&dir);
        // A relative require that leaves its mount, and the runtime's
        // alias, write instance paths; one inside the mount stays.
        let main = Path::new("src/client/main.aly");
        assert_eq!(
            rewrite_requires(
                &t,
                main,
                "require(\"../shared/util\") require(\"./ui\") require(\"@alloy\")"
            ),
            "require(\"@game/ReplicatedStorage/Shared/util\") require(\"./ui\") require(\"@game/ReplicatedStorage/Alloy\")"
        );
        assert_eq!(
            rewrite_requires(
                &t,
                Path::new(""),
                "require(\"@shared/economy\") require(\"@pkg/jecs\") require(\"@shared/data/config.json\") require(\"@lest/core\")"
            ),
            "require(\"@game/ReplicatedStorage/Shared/economy\") require(\"@game/ReplicatedStorage/Packages/jecs\") require(\"@game/ReplicatedStorage/Shared/data/config\") require(\"@lest/core\")"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_luau_carries_the_aliases_too() {
        let dir = temp("alias-config-luau");
        write(
            &dir,
            "default.project.json",
            r#"{ "name": "p", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": { "$className": "ReplicatedStorage",
                   "Shared": { "$path": "src/shared" } } } }"#,
        );
        write(
            &dir,
            ".config.luau",
            "return {\n    luau = {\n        languagemode = \"strict\",\n        aliases = {\n            shared = \"src/shared\",\n        },\n    },\n}\n",
        );
        let t = Tree::load(&dir, &Config::default());
        assert_eq!(
            rewrite_requires(&t, Path::new(""), "require(\"@shared/util\")"),
            "require(\"@game/ReplicatedStorage/Shared/util\")"
        );
        // No mount table and no `[project] runtime`: the default place.
        assert_eq!(t.runtime, vec!["ReplicatedStorage", "Alloy"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_runtime_follows_the_node_that_mounts_the_output() {
        let dir = temp("runtime-out");
        write(
            &dir,
            "default.project.json",
            r#"{ "name": "p", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": { "$className": "ReplicatedStorage",
                   "Build": { "$path": "build" } } } }"#,
        );
        let t = Tree::load(&dir, &Config::default());
        assert_eq!(t.runtime, vec!["ReplicatedStorage", "Build", "alloy"]);
        // The folder carries the file, so the build project adds no
        // second node for it.
        let built = t.project.as_ref().unwrap().build_tree(
            &dir,
            &dir.join(".alloy"),
            &t.input,
            &t.out,
            &t.runtime,
        );
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Build"],
            json!({ "$path": "../build" })
        );

        // `[project] runtime` wins over the tree.
        let config = Config::parse(
            "[project]\nruntime = \"@game/ServerStorage/Rt\"\n",
            Path::new("alloy.toml"),
        )
        .unwrap();
        assert_eq!(
            Tree::load(&dir, &config).runtime,
            vec!["ServerStorage", "Rt"]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A node that mounts `[build] in` mounts the output in the build
    /// project, and so carries `build/alloy.luau`. The runtime went to
    /// `ReplicatedStorage/Alloy` too, so Rojo made two copies of it,
    /// and nothing required the one under the node.
    #[test]
    fn the_runtime_lands_once_under_a_node_that_mounts_the_input() {
        let dir = temp("runtime-in");
        write(
            &dir,
            "default.project.json",
            r#"{ "name": "p", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": { "$className": "ReplicatedStorage",
                   "Game": { "$path": "src" } } } }"#,
        );
        let t = Tree::load(&dir, &Config::default());
        assert_eq!(t.runtime, vec!["ReplicatedStorage", "Game", "alloy"]);
        assert_eq!(
            std_require_for(&t, Path::new("src/a.aly")).unwrap(),
            "@game/ReplicatedStorage/Game/alloy"
        );
        let built = t.project.as_ref().unwrap().build_tree(
            &dir,
            &dir.join(".alloy"),
            &t.input,
            &t.out,
            &t.runtime,
        );
        assert_eq!(
            built["tree"]["ReplicatedStorage"],
            json!({ "$className": "ReplicatedStorage", "Game": { "$path": "../build/" } })
        );

        // A mount table does the same.
        let config = Config::parse(
            "[mount]\ngame = [\"src\", \"@game/ReplicatedStorage/Game\"]\n",
            Path::new("alloy.toml"),
        )
        .unwrap();
        let t = Tree::load(&dir, &config);
        assert_eq!(t.runtime, vec!["ReplicatedStorage", "Game", "alloy"]);
        let built = rojo_project(&t, &dir, &dir.join(".alloy"), true);
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Game"],
            json!({ "$path": "../build/" })
        );
        // The source project mounts `src`, which holds no runtime.
        let source = rojo_project(&t, &dir, &dir, false);
        assert_eq!(
            source["tree"]["ReplicatedStorage"]["Game"]["alloy"],
            json!({ "$path": "build/alloy.luau" })
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_mount_table_wins_over_the_project_file() {
        let dir = temp("both");
        write(
            &dir,
            "default.project.json",
            r#"{ "name": "place", "tree": { "$className": "DataModel",
                 "ServerStorage": { "$className": "ServerStorage",
                   "Only": { "$path": "src/shared" } } } }"#,
        );
        let config = Config::parse(MOUNTS, Path::new("alloy.toml")).unwrap();
        let t = Tree::load(&dir, &config);
        assert!(t.project.is_none(), "the table is the tree");
        assert_eq!(t.name, "demo");
        assert_eq!(
            instance_path(&t, Path::new("src/shared/util.aly")).unwrap(),
            vec!["ReplicatedStorage", "Shared", "util"]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_file_keeps_its_own_default_project_json() {
        let dir = temp("files");
        let t = with_project_file(&dir);
        let written = files(&t, &Config::default(), &dir).unwrap();
        let names: Vec<String> = written
            .iter()
            .map(|(p, _)| p.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            names,
            vec![
                ".alloy/build.project.json",
                ".alloy/.gitignore",
                "sourcemap.json"
            ]
        );

        let built: Value = serde_json::from_str(&written[0].1).unwrap();
        assert_eq!(built["name"], "place");
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Shared"]["$path"],
            "../build/shared"
        );
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Packages"]["$path"],
            "../Packages"
        );

        // The mount table writes the source project too.
        let config = Config::parse(MOUNTS, Path::new("alloy.toml")).unwrap();
        let mounted = files(&Tree::load(&dir, &config), &config, &dir).unwrap();
        assert_eq!(mounted[0].0, PathBuf::from("default.project.json"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
