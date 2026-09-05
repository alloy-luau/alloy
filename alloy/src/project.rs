//! The `[mount]` table: where each folder lands in the DataModel.
//!
//! One table drives three files and one rewrite. `default.project.json`
//! is a Rojo project over the sources, for the tools that read one.
//! `.alloy/build.project.json` is the same tree over the compiled
//! output, the one `rojo serve` and `rojo build` take. `.alloy/
//! sourcemap.json` is the instance tree with the source paths, which
//! the language server maps onto its mirror. And a `require("@alias/
//! x")` in the ship artifact becomes a relative instance path, because
//! Roblox reads no `.luaurc`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::config::{Config, Mount};

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

/// A mount whose path holds `rel`, the longest such path, with the
/// remainder of `rel` under it.
fn mount_of<'a>(config: &'a Config, rel: &Path) -> Option<(&'a str, &'a Mount, PathBuf)> {
    let mut best: Option<(&str, &Mount, PathBuf)> = None;

    for (alias, m) in &config.mount {
        let base = Path::new(&m.0);

        if let Ok(rest) = rel.strip_prefix(base)
            && best.as_ref().is_none_or(|(_, b, _)| {
                Path::new(&b.0).components().count() < base.components().count()
            })
        {
            best = Some((alias.as_str(), m, rest.to_path_buf()));
        }
    }

    best
}

/// The instance name of a script file: the stem with `.server`,
/// `.client`, and `.d` removed. `init` names its directory.
fn instance_name(file: &str) -> Option<String> {
    let stem = file
        .strip_suffix(".aly")
        .or_else(|| file.strip_suffix(".alx"))
        .or_else(|| file.strip_suffix(".luau"))
        .or_else(|| file.strip_suffix(".lua"))?;
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

/// The class of a node between a service and a leaf: a folder, except
/// the containers Roblox names, which are their own class.
fn container_class(name: &str) -> &str {
    match name {
        "StarterPlayerScripts" | "StarterCharacterScripts" | "StarterCharacter" => name,

        _ => "Folder",
    }
}

/// The Roblox class of a script file.
fn script_class(file: &str) -> &'static str {
    if file.contains(".server.") {
        "Script"
    } else if file.contains(".client.") {
        "LocalScript"
    } else {
        "ModuleScript"
    }
}

/// The DataModel path of a source file, relative to the project root:
/// the mount's segments, the directories under it, and the instance
/// name. `None` when no mount holds the file.
pub fn instance_path(config: &Config, rel: &Path) -> Option<Vec<String>> {
    let (_, m, rest) = mount_of(config, rel)?;
    let mut out = segments(&m.1)?;

    for c in rest.components() {
        let Component::Normal(n) = c else { continue };
        let n = n.to_string_lossy();

        if Some(n.as_ref()) == rest.file_name().and_then(|f| f.to_str()) {
            if let Some(name) = instance_name(&n) {
                out.push(name);
            }
        } else {
            out.push(n.into_owned());
        }
    }

    Some(out)
}

/// The relative require path from the parent of `from` to `to`: `./`
/// for a sibling, `../` per level up, then the rest of the way down.
/// `..` above a service reaches the DataModel, and a path down from
/// there names the service.
fn relative(from_parent: &[String], to: &[String]) -> String {
    let common = from_parent
        .iter()
        .zip(to)
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_parent.len() - common;
    let down = &to[common..];
    let mut out = if ups == 0 {
        ".".to_string()
    } else {
        vec![".."; ups].join("/")
    };

    for seg in down {
        out.push('/');
        out.push_str(seg);
    }

    out
}

/// The parent of a file's instance, for a relative require from it.
fn parent_of(config: &Config, rel: &Path) -> Option<Vec<String>> {
    let mut path = instance_path(config, rel)?;

    if instance_name(rel.file_name()?.to_str()?).is_some() {
        path.pop();
    } else {
        // An `init` file is its directory; requires resolve from the
        // directory's parent.
        path.pop();
    }

    Some(path)
}

/// The require string for the runtime from a file, through the mounts.
/// `None` when the file is under no mount, or the runtime mount is not
/// under `@game/`.
pub fn std_require_for(config: &Config, rel: &Path) -> Option<String> {
    let parent = parent_of(config, rel)?;
    let runtime = segments(&config.project.runtime)?;

    Some(relative(&parent, &runtime))
}

/// The require string for `@alias/rest` from a file, through the mounts.
pub fn resolve_alias(config: &Config, rel: &Path, alias: &str, rest: &str) -> Option<String> {
    let parent = parent_of(config, rel)?;
    let m = config.mount.get(alias)?;
    let mut target = segments(&m.1)?;

    for part in rest.split('/').filter(|p| !p.is_empty()) {
        let name = instance_name(part).or_else(|| {
            if part.contains('.') {
                None
            } else {
                Some(part.to_string())
            }
        });

        // `init` at the end names the directory already pushed.
        if let Some(n) = name {
            target.push(n);
        }
    }

    Some(relative(&parent, &target))
}

/// Rewrites every `require("@alias/...")` in an emitted text to the
/// relative instance path, for the aliases the mount table names. The
/// text keeps its line count: a replacement holds no newline.
pub fn rewrite_requires(config: &Config, rel: &Path, text: &str) -> String {
    if config.mount.is_empty() {
        return text.to_string();
    }

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

            resolve_alias(config, rel, alias, tail)
        });

        out.push_str(&rest[..i + "require(".len()]);
        out.push(q);
        out.push_str(replaced.as_deref().unwrap_or(path));
        out.push(q);
        rest = &body[end + 1..];
    }

    out.push_str(rest);
    out
}

/// A path as the project file sees it: relative to `base`, the
/// directory the file lives in, given both are under `root`.
fn from_base(root: &Path, base: &Path, path: &Path) -> String {
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
fn insert(tree: &mut Map<String, Value>, segs: &[String], leaf: Value) {
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

/// A Rojo project over the mounts. `compiled` points the paths at the
/// build output for a source under `[build] in`; `base` is the
/// directory the file will live in.
pub fn rojo_project(config: &Config, root: &Path, base: &Path, compiled: bool) -> Value {
    let mut tree = Map::new();
    tree.insert("$className".into(), Value::String("DataModel".into()));

    for m in config.mount.values() {
        let Some(segs) = segments(&m.1) else { continue };
        let path = Path::new(&m.0);
        let shown = match (compiled, path.strip_prefix(&config.build.input)) {
            (true, Ok(rest)) => config.build.out.join(rest),

            _ => path.to_path_buf(),
        };
        let leaf = json!({ "$path": from_base(root, base, &shown) });
        insert(&mut tree, &segs, leaf);
    }

    if let Some(segs) = segments(&config.project.runtime) {
        let runtime = config.build.out.join("alloy.luau");
        insert(
            &mut tree,
            &segs,
            json!({ "$path": from_base(root, base, &runtime) }),
        );
    }

    json!({ "name": config.project.name, "tree": Value::Object(tree) })
}

/// One node of a sourcemap.
fn node(name: &str, class: &str, file: Option<String>) -> Map<String, Value> {
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
fn dir_node(root: &Path, dir: &Path, name: &str) -> std::io::Result<Map<String, Value>> {
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

        if fname.starts_with('.') || fname == "node_modules" {
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

/// The sourcemap of the mounts: the instance tree, each script with the
/// path of its source, relative to `root`.
pub fn sourcemap(config: &Config, root: &Path) -> std::io::Result<Value> {
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

    for m in config.mount.values() {
        let Some(segs) = segments(&m.1) else { continue };
        let path = root.join(&m.0);
        let name = segs.last().cloned().unwrap_or_default();

        let leaf = if path.is_dir() {
            dir_node(root, &path, &name)?
        } else if path.is_file() {
            let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            let mut n = node(&name, script_class(fname), Some(m.0.clone()));
            n.insert("children".into(), json!([]));
            n
        } else {
            continue;
        };

        place(&segs, leaf);
    }

    if let Some(segs) = segments(&config.project.runtime) {
        let file = config
            .build
            .out
            .join("alloy.luau")
            .to_string_lossy()
            .replace('\\', "/");
        let mut n = node(
            segs.last().map(String::as_str).unwrap_or("Alloy"),
            "ModuleScript",
            Some(file),
        );
        n.insert("children".into(), json!([]));
        place(&segs, n);
    }

    game.insert("children".into(), Value::Array(children));

    Ok(Value::Object(game))
}

/// The files `alloy build` writes for a project with mounts, as
/// (path relative to root, text).
pub fn files(config: &Config, root: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    if config.mount.is_empty() {
        return Ok(Vec::new());
    }

    let alloy_dir = root.join(".alloy");
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
    let mut out = vec![
        (
            PathBuf::from("default.project.json"),
            pretty(&rojo_project(config, root, root, false)),
        ),
        (
            PathBuf::from(".alloy/build.project.json"),
            pretty(&rojo_project(config, root, &alloy_dir, true)),
        ),
        (
            PathBuf::from(".alloy/.gitignore"),
            "sourcemap.json\n".to_string(),
        ),
    ];

    if config.project.sourcemap {
        out.push((
            PathBuf::from(".alloy/sourcemap.json"),
            pretty(&sourcemap(config, root)?),
        ));
    }

    Ok(out)
}

/// The aliases the Luau configuration should carry for the mounts:
/// each alias to its path. Returns the ones the root's `.luaurc` lacks
/// after adding them, and, for a `.config.luau`, the ones to add by
/// hand.
pub fn sync_aliases(config: &Config, root: &Path) -> std::io::Result<Vec<String>> {
    let mut notes = Vec::new();

    if config.mount.is_empty() {
        return Ok(notes);
    }

    let wanted: BTreeMap<&str, &str> = config
        .mount
        .iter()
        .map(|(k, m)| (k.as_str(), m.0.as_str()))
        .collect();
    let rc = root.join(".luaurc");

    // A root with no Luau configuration gets one, strict, with the
    // aliases; `alloy init` would have written the same.
    if !crate::luau_config::has_config(root) {
        let c = crate::luau_config::LuauConfig {
            language_mode: Some("strict".to_string()),
            aliases: wanted
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        };
        std::fs::write(&rc, crate::luau_config::render_luaurc(&c))?;
        notes.push("wrote .luaurc: strict mode and the mount aliases".to_string());

        return Ok(notes);
    }

    if rc.is_file() {
        let text = std::fs::read_to_string(&rc)?;
        let mut json: Value = match serde_json::from_str(&text) {
            Ok(v) => v,

            Err(_) => return Ok(notes),
        };
        let mut added = Vec::new();

        if let Some(map) = json.as_object_mut() {
            let aliases = map
                .entry("aliases")
                .or_insert_with(|| Value::Object(Map::new()));

            if let Some(aliases) = aliases.as_object_mut() {
                for (k, v) in &wanted {
                    if !aliases.contains_key(*k) {
                        aliases.insert((*k).to_string(), Value::String((*v).to_string()));
                        added.push(*k);
                    }
                }
            }
        }

        if !added.is_empty() {
            let mut text = serde_json::to_string_pretty(&json).unwrap_or(text);
            text.push('\n');
            std::fs::write(&rc, text)?;
            notes.push(format!(
                "added {} to .luaurc",
                added
                    .iter()
                    .map(|a| format!("@{a}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    let luau = root.join(".config.luau");

    if luau.is_file()
        && let Ok(text) = std::fs::read_to_string(&luau)
        && let Some(c) = crate::luau_config::parse_config_luau(&text)
    {
        let missing: Vec<String> = wanted
            .iter()
            .filter(|(k, _)| !c.aliases.iter().any(|(a, _)| a == *k))
            .map(|(k, v)| format!("{k} = \"{v}\""))
            .collect();

        if !missing.is_empty() {
            notes.push(format!(
                ".config.luau lacks the mount aliases; add to its `aliases`: {}",
                missing.join(", ")
            ));
        }
    }

    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::parse(
            r#"
[project]
name = "demo"

[mount]
server = ["src/server", "@game/ServerScriptService/Server"]
shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
pkg = ["Packages", "@game/ReplicatedStorage/Packages"]
"#,
            Path::new("alloy.toml"),
        )
        .unwrap()
    }

    #[test]
    fn a_file_under_a_mount_has_an_instance_path() {
        let c = config();
        assert_eq!(
            instance_path(&c, Path::new("src/server/combat/hit.aly")).unwrap(),
            vec!["ServerScriptService", "Server", "combat", "hit"]
        );
        assert_eq!(
            instance_path(&c, Path::new("src/server/init.server.aly")).unwrap(),
            vec!["ServerScriptService", "Server"]
        );
        assert!(instance_path(&c, Path::new("src/other.aly")).is_none());
    }

    #[test]
    fn the_runtime_require_walks_the_tree() {
        let c = config();
        assert_eq!(
            std_require_for(&c, Path::new("src/server/combat/hit.aly")).unwrap(),
            "../../../ReplicatedStorage/Alloy"
        );
        assert_eq!(
            std_require_for(&c, Path::new("src/shared/util.aly")).unwrap(),
            "../Alloy"
        );
    }

    #[test]
    fn an_alias_require_becomes_a_relative_path() {
        let c = config();
        let text = "local jecs = require(\"@pkg/jecs\") local u = require(\"@shared/util\") local x = require(\"./x\")";
        let out = rewrite_requires(&c, Path::new("src/server/main.server.aly"), text);
        assert_eq!(
            out,
            "local jecs = require(\"../../ReplicatedStorage/Packages/jecs\") local u = require(\"../../ReplicatedStorage/Shared/util\") local x = require(\"./x\")"
        );
        assert_eq!(
            rewrite_requires(&c, Path::new("src/shared/a.aly"), "require(\"@shared/b\")"),
            "require(\"./b\")"
        );
    }

    #[test]
    fn the_two_projects_point_at_sources_and_output() {
        let c = config();
        let root = Path::new("/p");
        let src = rojo_project(&c, root, root, false);
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

        let build = rojo_project(&c, root, &root.join(".alloy"), true);
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
    fn the_sourcemap_names_scripts_by_suffix() {
        let dir = std::env::temp_dir().join(format!("alloy-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/server/combat")).unwrap();
        std::fs::create_dir_all(dir.join("src/shared")).unwrap();
        std::fs::create_dir_all(dir.join("Packages")).unwrap();
        std::fs::write(dir.join("src/server/init.server.aly"), "").unwrap();
        std::fs::write(dir.join("src/server/combat/hit.aly"), "").unwrap();
        std::fs::write(dir.join("src/shared/util.aly"), "").unwrap();
        std::fs::write(dir.join("src/shared/ui.client.aly"), "").unwrap();
        std::fs::write(dir.join("Packages/jecs.luau"), "").unwrap();

        let map = sourcemap(&config(), &dir).unwrap();
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
}
