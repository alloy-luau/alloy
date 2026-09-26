//! The user's Rojo project file, read as the DataModel tree.
//!
//! A root with a project file needs no second description of its tree.
//! Alloy reads `default.project.json`, or the file `[project] file`
//! names, or the one `*.project.json` at the root, and derives from it
//! the map from a disk folder to an instance path. That map drives the
//! sourcemap, the `@alias` rewrite of the ship artifact, the runtime's
//! place, and `.alloy/build.project.json`. The file itself is never
//! written back.
//!
//! A `[mount]` table in alloy.toml wins over the file: a tool with its
//! own format describes the tree there, and `crate::project` keeps that
//! path.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// One `$path` node: where it lands in the DataModel, and the folder or
/// file it mounts, relative to the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mounted {
    /// The instance path under `game`: `["ReplicatedStorage", "Shared"]`.
    pub place: Vec<String>,
    pub disk: PathBuf,
}

/// A Rojo project file, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFile {
    /// The file, relative to the root.
    pub file: PathBuf,
    /// The `name` field, the place name Rojo builds.
    pub name: String,
    /// The `tree` object, as written.
    pub tree: Map<String, Value>,
}

/// The project file names Alloy reads, in order: the one `[project]
/// file` names, `default.project.json`, then the single `*.project.json`
/// at the root.
pub fn find(root: &Path, named: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = named {
        let path = root.join(name);

        return path.is_file().then_some(path);
    }

    let default = root.join("default.project.json");

    if default.is_file() {
        return Some(default);
    }

    let mut found = Vec::new();

    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();

        if path.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".project.json"))
        {
            found.push(path);
        }
    }

    match found.len() {
        1 => found.pop(),

        // Several files and no `[project] file`: Alloy will not guess
        // which place the sources belong to.
        _ => None,
    }
}

/// Reads the project file of a root, when it has one.
pub fn load(root: &Path, named: Option<&str>) -> Option<ProjectFile> {
    let path = find(root, named)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let tree = value.get("tree")?.as_object()?.clone();
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("game")
        .to_string();

    Some(ProjectFile {
        file: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
        name,
        tree,
    })
}

/// The `$path` of a node. Rojo also writes `{ "optional": "..." }`, and
/// that path counts the same here.
fn disk_path(node: &Map<String, Value>) -> Option<PathBuf> {
    match node.get("$path")? {
        Value::String(s) => Some(PathBuf::from(s.replace('\\', "/"))),

        Value::Object(m) => m
            .get("optional")
            .and_then(Value::as_str)
            .map(|s| PathBuf::from(s.replace('\\', "/"))),

        _ => None,
    }
}

/// Whether a `$path` names a script file rather than a folder.
fn names_a_script(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "luau" | "lua" | "aly" | "alx"))
}

/// The children of a node: every key that is not a `$` directive.
/// `$className`, `$properties`, and `$ignoreUnknownInstances` are
/// directives, so they never become instances.
fn children(node: &Map<String, Value>) -> impl Iterator<Item = (&String, &Map<String, Value>)> {
    node.iter().filter_map(|(k, v)| {
        if k.starts_with('$') {
            return None;
        }

        v.as_object().map(|o| (k, o))
    })
}

impl ProjectFile {
    /// Every `$path` in the tree, deepest instance path first, so the
    /// longest disk prefix of a file wins.
    pub fn mounts(&self) -> Vec<Mounted> {
        fn walk(node: &Map<String, Value>, place: &mut Vec<String>, out: &mut Vec<Mounted>) {
            if let Some(disk) = disk_path(node) {
                out.push(Mounted {
                    place: place.clone(),
                    disk,
                });
            }

            for (name, child) in children(node) {
                place.push(name.clone());
                walk(child, place, out);
                place.pop();
            }
        }

        let mut out = Vec::new();
        walk(&self.tree, &mut Vec::new(), &mut out);
        out.retain(|m| !m.place.is_empty());
        out
    }

    /// The instance path of the node that mounts `disk`, when the tree
    /// has one.
    pub fn place_of(&self, disk: &Path) -> Option<Vec<String>> {
        self.mounts()
            .into_iter()
            .find(|m| m.disk == disk)
            .map(|m| m.place)
    }

    /// The class of the node at an instance path, when it names one.
    pub fn class_at(&self, place: &[String]) -> Option<String> {
        let mut node = &self.tree;

        for seg in place {
            node = children(node).find(|(n, _)| *n == seg).map(|(_, c)| c)?;
        }

        node.get("$className")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The tree with every `$path` under `input` pointed at its output
    /// under `out`, and every path rebased on `base`, the directory the
    /// written file lives in. The runtime is placed when the tree holds
    /// no node for it.
    pub fn build_tree(
        &self,
        root: &Path,
        base: &Path,
        input: &Path,
        out: &Path,
        runtime: &[String],
    ) -> Value {
        fn rewrite(
            node: &Map<String, Value>,
            root: &Path,
            base: &Path,
            input: &Path,
            out: &Path,
        ) -> Map<String, Value> {
            let mut copy = Map::new();

            for (key, value) in node {
                if key == "$path" {
                    let Some(disk) = disk_path(node) else {
                        copy.insert(key.clone(), value.clone());

                        continue;
                    };
                    let shown = match disk.strip_prefix(input) {
                        Ok(rest) => out.join(rest),

                        Err(_) => disk,
                    };
                    let text = crate::project::from_base(root, base, &shown);

                    // An `{ optional }` path keeps its shape.
                    match value {
                        Value::Object(m) => {
                            let mut m = m.clone();
                            m.insert("optional".into(), Value::String(text));
                            copy.insert(key.clone(), Value::Object(m));
                        }

                        _ => {
                            copy.insert(key.clone(), Value::String(text));
                        }
                    }

                    continue;
                }

                match value.as_object() {
                    Some(child) if !key.starts_with('$') => {
                        copy.insert(
                            key.clone(),
                            Value::Object(rewrite(child, root, base, input, out)),
                        );
                    }

                    _ => {
                        copy.insert(key.clone(), value.clone());
                    }
                }
            }

            copy
        }

        let mut tree = rewrite(&self.tree, root, base, input, out);

        // A node that mounts the output folder, or the input folder,
        // which this tree points at the output, carries the runtime.
        let carried = self.place_of(&out.join("alloy.luau")).is_some()
            || crate::project::carries_runtime(&self.mounts(), &[out, input], runtime);

        if !runtime.is_empty() && !carried {
            let path = crate::project::from_base(root, base, &out.join("alloy.luau"));
            crate::project::insert(&mut tree, runtime, serde_json::json!({ "$path": path }));
        }

        serde_json::json!({ "name": self.name, "tree": Value::Object(tree) })
    }

    /// The sourcemap of the tree: every instance, with the source path
    /// of each script, relative to `root`.
    pub fn sourcemap(&self, root: &Path, runtime: &[String], out: &Path) -> std::io::Result<Value> {
        let mut game = self.map_node(root, "game", &self.tree)?;
        game.insert("name".into(), Value::String("game".into()));

        if game.get("className").and_then(Value::as_str) == Some("Folder") {
            game.insert("className".into(), Value::String("DataModel".into()));
        }

        if !runtime.is_empty() && self.place_of(&out.join("alloy.luau")).is_none() {
            let file = out.join("alloy.luau").to_string_lossy().replace('\\', "/");
            let mut n = crate::project::node(
                runtime.last().map(String::as_str).unwrap_or("Alloy"),
                "ModuleScript",
                Some(file),
            );
            n.insert("children".into(), serde_json::json!([]));
            place_in(&mut game, runtime, n);
        }

        Ok(Value::Object(game))
    }

    /// One node of the sourcemap: the disk contents of its `$path`,
    /// then the children the file names. `$className` wins over the
    /// class the disk suggests.
    fn map_node(
        &self,
        root: &Path,
        name: &str,
        node: &Map<String, Value>,
    ) -> std::io::Result<Map<String, Value>> {
        let disk = disk_path(node);
        let mut out = match &disk {
            Some(d) if root.join(d).is_dir() => {
                crate::project::dir_node(root, &root.join(d), name)?
            }

            // A path that names a script file is a script even before
            // the build writes it: `build/alloy.luau` on a first run.
            Some(d) if root.join(d).is_file() || names_a_script(d) => {
                let file = d.to_string_lossy().replace('\\', "/");
                let class = crate::project::script_class(&file);
                let mut m = crate::project::node(name, class, Some(file));
                m.insert("children".into(), serde_json::json!([]));
                m
            }

            _ => {
                let mut m = crate::project::node(name, crate::project::container_class(name), None);
                m.insert("children".into(), serde_json::json!([]));
                m
            }
        };

        if let Some(class) = node.get("$className").and_then(Value::as_str) {
            out.insert("className".into(), Value::String(class.to_string()));
        }

        for (child_name, child) in children(node) {
            let mapped = Value::Object(self.map_node(root, child_name, child)?);
            let list = out["children"]
                .as_array_mut()
                .expect("children is an array");

            match list.iter().position(|c| c["name"] == child_name.as_str()) {
                Some(at) => list[at] = mapped,

                None => list.push(mapped),
            }
        }

        Ok(out)
    }
}

/// Puts `leaf` at an instance path of a sourcemap node, making the
/// folders on the way.
fn place_in(parent: &mut Map<String, Value>, place: &[String], leaf: Map<String, Value>) {
    let Some((name, rest)) = place.split_first() else {
        return;
    };
    let list = match parent.get_mut("children").and_then(Value::as_array_mut) {
        Some(list) => list,

        None => {
            parent.insert("children".into(), serde_json::json!([]));
            parent["children"].as_array_mut().expect("children")
        }
    };
    let at = match list.iter().position(|c| c["name"] == name.as_str()) {
        Some(at) => at,

        None => {
            let mut m = crate::project::node(name, crate::project::container_class(name), None);
            m.insert("children".into(), serde_json::json!([]));
            list.push(Value::Object(m));
            list.len() - 1
        }
    };

    if rest.is_empty() {
        let mut leaf = leaf;
        leaf.insert("name".into(), Value::String(name.clone()));
        list[at] = Value::Object(leaf);

        return;
    }

    let Value::Object(child) = &mut list[at] else {
        return;
    };
    place_in(child, rest, leaf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        let path = dir.join(name);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        std::fs::write(path, text).unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alloy-rojo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        dir
    }

    const TREE: &str = r#"{
      "name": "demo",
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
        },
        "Workspace": {
          "$className": "Workspace",
          "$ignoreUnknownInstances": true,
          "Props": { "$className": "Folder", "$properties": { "Name": "Props" } }
        }
      }
    }"#;

    fn project() -> ProjectFile {
        let value: Value = serde_json::from_str(TREE).unwrap();

        ProjectFile {
            file: PathBuf::from("default.project.json"),
            name: value["name"].as_str().unwrap().to_string(),
            tree: value["tree"].as_object().unwrap().clone(),
        }
    }

    #[test]
    fn every_path_node_becomes_a_mount() {
        let p = project();
        let mut mounts = p.mounts();
        mounts.sort_by(|a, b| a.disk.cmp(&b.disk));
        let shown: Vec<(String, String)> = mounts
            .iter()
            .map(|m| (m.disk.to_string_lossy().into_owned(), m.place.join("/")))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("Packages".to_string(), "ReplicatedStorage/Packages".into()),
                (
                    "build/alloy.luau".to_string(),
                    "ReplicatedStorage/Alloy".into()
                ),
                (
                    "src/client".to_string(),
                    "StarterPlayer/StarterPlayerScripts/Client".into()
                ),
                (
                    "src/server".to_string(),
                    "ServerScriptService/Server".into()
                ),
                ("src/shared".to_string(), "ReplicatedStorage/Shared".into()),
            ]
        );
    }

    #[test]
    fn a_directive_is_never_an_instance() {
        let p = project();
        assert_eq!(
            p.class_at(&["Workspace".to_string()]).as_deref(),
            Some("Workspace")
        );
        assert_eq!(
            p.class_at(&["Workspace".to_string(), "Props".to_string()])
                .as_deref(),
            Some("Folder")
        );
        // `$ignoreUnknownInstances` and `$properties` name no child.
        assert!(
            p.class_at(&["Workspace".to_string(), "$properties".to_string()])
                .is_none()
        );
    }

    #[test]
    fn the_build_tree_points_at_the_output() {
        let p = project();
        let root = Path::new("/p");
        let built = p.build_tree(
            root,
            &root.join(".alloy"),
            Path::new("src"),
            Path::new("build"),
            &["ReplicatedStorage".to_string(), "Alloy".to_string()],
        );
        assert_eq!(built["name"], "demo");
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Shared"]["$path"],
            "../build/shared"
        );
        assert_eq!(
            built["tree"]["ServerScriptService"]["Server"]["$path"],
            "../build/server"
        );
        assert_eq!(
            built["tree"]["StarterPlayer"]["StarterPlayerScripts"]["Client"]["$path"],
            "../build/client"
        );
        // A folder outside `[build] in` keeps its own path.
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Packages"]["$path"],
            "../Packages"
        );
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Alloy"]["$path"],
            "../build/alloy.luau"
        );
        // The directives ride along.
        assert_eq!(built["tree"]["Workspace"]["$ignoreUnknownInstances"], true);
        assert_eq!(
            built["tree"]["Workspace"]["Props"]["$properties"]["Name"],
            "Props"
        );
    }

    #[test]
    fn the_runtime_is_placed_when_the_tree_lacks_it() {
        let value: Value = serde_json::from_str(
            r#"{ "name": "n", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": { "$className": "ReplicatedStorage",
                   "Shared": { "$path": "src/shared" } } } }"#,
        )
        .unwrap();
        let p = ProjectFile {
            file: PathBuf::from("default.project.json"),
            name: "n".into(),
            tree: value["tree"].as_object().unwrap().clone(),
        };
        let root = Path::new("/p");
        let built = p.build_tree(
            root,
            &root.join(".alloy"),
            Path::new("src"),
            Path::new("build"),
            &["ReplicatedStorage".to_string(), "Alloy".to_string()],
        );
        assert_eq!(
            built["tree"]["ReplicatedStorage"]["Alloy"]["$path"],
            "../build/alloy.luau"
        );
    }

    #[test]
    fn the_sourcemap_walks_the_tree_and_the_disk() {
        let dir = temp("sourcemap");
        write(&dir, "src/server/init.server.aly", "");
        write(&dir, "src/server/combat/hit.aly", "");
        write(&dir, "src/shared/util.aly", "");
        write(&dir, "src/client/ui.client.aly", "");
        write(&dir, "Packages/jecs.luau", "");
        write(&dir, "build/alloy.luau", "");

        let map = project()
            .sourcemap(
                &dir,
                &["ReplicatedStorage".to_string(), "Alloy".to_string()],
                Path::new("build"),
            )
            .unwrap();
        assert_eq!(map["name"], "game");
        assert_eq!(map["className"], "DataModel");

        let services = map["children"].as_array().unwrap();
        let by = |n: &str| {
            services
                .iter()
                .find(|s| s["name"] == n)
                .unwrap_or_else(|| panic!("{n} is missing"))
        };
        let rs = by("ReplicatedStorage");
        assert_eq!(rs["className"], "ReplicatedStorage");
        let alloy = rs["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "Alloy")
            .unwrap();
        assert_eq!(alloy["filePaths"][0], "build/alloy.luau");

        let server = &by("ServerScriptService")["children"][0];
        assert_eq!(server["name"], "Server");
        assert_eq!(server["className"], "Script");
        assert_eq!(server["filePaths"][0], "src/server/init.server.aly");
        assert_eq!(server["children"][0]["name"], "combat");
        assert_eq!(server["children"][0]["children"][0]["name"], "hit");

        // A container between the service and the leaf keeps its class.
        let sps = &by("StarterPlayer")["children"][0];
        assert_eq!(sps["name"], "StarterPlayerScripts");
        assert_eq!(sps["className"], "StarterPlayerScripts");
        let ui = &sps["children"][0]["children"][0];
        assert_eq!(ui["name"], "ui");
        assert_eq!(ui["className"], "LocalScript");

        // A node with only `$className` still shows up.
        let props = &by("Workspace")["children"][0];
        assert_eq!(props["name"], "Props");
        assert_eq!(props["className"], "Folder");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_is_the_named_one_then_the_default_then_the_only_one() {
        let dir = temp("find");
        write(&dir, "place.project.json", "{}");
        assert_eq!(
            find(&dir, None).unwrap().file_name().unwrap(),
            "place.project.json"
        );

        write(&dir, "default.project.json", "{}");
        assert_eq!(
            find(&dir, None).unwrap().file_name().unwrap(),
            "default.project.json"
        );
        assert_eq!(
            find(&dir, Some("place.project.json"))
                .unwrap()
                .file_name()
                .unwrap(),
            "place.project.json"
        );
        assert!(find(&dir, Some("missing.project.json")).is_none());

        // Two files and no default: Alloy will not guess.
        let other = temp("find-two");
        write(&other, "a.project.json", "{}");
        write(&other, "b.project.json", "{}");
        assert!(find(&other, None).is_none());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn a_file_that_does_not_parse_is_no_tree() {
        let dir = temp("bad");
        write(&dir, "default.project.json", "{ not json");
        assert!(load(&dir, None).is_none());

        write(&dir, "default.project.json", "{ \"name\": \"n\" }");
        assert!(load(&dir, None).is_none(), "a file with no tree is none");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
