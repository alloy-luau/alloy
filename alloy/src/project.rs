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

        if !config.mount.is_empty() {
            return Self {
                name: config.project.name.clone(),
                mounts: config
                    .mount
                    .values()
                    .filter_map(|m| {
                        Some(Mounted {
                            place: segments(&m.1)?,
                            disk: PathBuf::from(m.0.replace('\\', "/")),
                        })
                    })
                    .collect(),
                runtime: segments(config.project.runtime()).unwrap_or_default(),
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
        // The runtime lands where the project says, else where the tree
        // already puts `alloy.luau`, else inside the node that mounts
        // the output folder, else at the default place.
        let runtime = match (&config.project.runtime, &project) {
            (Some(r), _) => segments(r).unwrap_or_default(),

            (None, Some(p)) => p
                .place_of(&out.join("alloy.luau"))
                .or_else(|| {
                    p.place_of(&out).map(|mut place| {
                        place.push("alloy".to_string());
                        place
                    })
                })
                .or_else(|| segments(crate::config::DEFAULT_RUNTIME))
                .unwrap_or_default(),

            (None, None) => segments(crate::config::DEFAULT_RUNTIME).unwrap_or_default(),
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
/// down becomes instance names.
pub fn resolve_alias(tree: &Tree, alias: &str, rest: &str) -> Option<String> {
    let (_, dir) = tree.aliases.iter().find(|(a, _)| a == alias)?;
    let joined = normalize(&dir.join(rest.trim_start_matches('/')));
    let place = place_of(tree, &joined)?;

    Some(format!("@game/{}", place.join("/")))
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
pub fn rewrite_requires(tree: &Tree, text: &str) -> String {
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
        let replaced = path.strip_prefix('@').and_then(|p| {
            let (alias, tail) = p.split_once('/').unwrap_or((p, ""));

            resolve_alias(tree, alias, tail)
        });

        out.push_str(&rest[..i + "require(".len()]);
        out.push(q);
        out.push_str(crate::data::strip_spec(replaced.as_deref().unwrap_or(path)));
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

    if !tree.runtime.is_empty() {
        let runtime = tree.out.join("alloy.luau");
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
        "sourcemap.json\n".to_string(),
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
        let out = rewrite_requires(&t, text);
        assert_eq!(
            out,
            "local jecs = require(\"@game/ReplicatedStorage/Packages/jecs\") local u = require(\"@game/ReplicatedStorage/Shared/util\") local x = require(\"./x\")"
        );
        assert_eq!(
            rewrite_requires(
                &Tree::default(),
                "local d = require(\"./data.json\") local c = require('../cfg.toml')\n"
            ),
            "local d = require(\"./data\") local c = require('../cfg')\n"
        );
        assert_eq!(
            rewrite_requires(&t, "require(\"@shared/b\")"),
            "require(\"@game/ReplicatedStorage/Shared/b\")"
        );
        // A data path under an alias keeps the module name.
        assert_eq!(
            resolve_alias(&t, "shared", "data/config.json").unwrap(),
            "@game/ReplicatedStorage/Shared/data/config"
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
        assert_eq!(
            rewrite_requires(
                &t,
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
            rewrite_requires(&t, "require(\"@shared/util\")"),
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
