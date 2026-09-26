//! The project file as the tree: a root that keeps its own
//! `default.project.json` and no `[mount]` table builds the same
//! sourcemap, the same build project, and the same requires as the
//! table it replaces.

use std::fs;
use std::path::{Path, PathBuf};

use alloy::config::Config;
use serde_json::Value;

/// The layout of examples/test, with a source in each mounted folder.
const PROJECT_JSON: &str = r#"{
  "name": "game",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "$className": "ReplicatedStorage",
      "Alloy": { "$path": "build/alloy.luau" },
      "Packages": { "$path": "packages/roblox" },
      "Shared": { "$path": "src/shared" }
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
}
"#;

const LUAURC: &str = r#"{
  "aliases": {
    "client": "src/client",
    "pkg": "packages/roblox",
    "server": "src/server",
    "shared": "src/shared"
  },
  "languageMode": "strict"
}
"#;

const MOUNTS: &str = r#"
[mount]
server = ["src/server", "@game/ServerScriptService/Server"]
client = ["src/client", "@game/StarterPlayer/StarterPlayerScripts/Client"]
shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
pkg = ["packages/roblox", "@game/ReplicatedStorage/Packages"]
"#;

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }

    fs::write(path, text).unwrap();
}

/// A copy of the examples/test layout at `dir`. `mounts` adds the
/// `[mount]` table to alloy.toml; without it the project file is the
/// tree.
fn example(name: &str, mounts: bool) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("alloy-project-file-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let mut toml = "[build]\nin = \"src\"\nout = \"build\"\nartifact = \"ship\"\n".to_string();

    if mounts {
        toml.push_str(MOUNTS);
    }

    write(&dir, "alloy.toml", &toml);
    write(&dir, "default.project.json", PROJECT_JSON);
    write(&dir, ".luaurc", LUAURC);
    write(&dir, "packages/roblox/jecs.luau", "return {}\n");
    write(
        &dir,
        "src/shared/util.aly",
        "export const NAME = \"util\"\n",
    );
    write(&dir, "src/shared/data/config.json", "{ \"speed\": 16 }\n");
    write(
        &dir,
        "src/server/init.server.aly",
        "import { NAME } from \"@shared/util\"\nprint(NAME)\n",
    );
    write(
        &dir,
        "src/server/combat/hit.aly",
        "import jecs from \"@pkg/jecs\"\nimport { NAME } from \"@shared/util\"\nprint(jecs, NAME)\n",
    );
    write(
        &dir,
        "src/client/ui.client.aly",
        "import config from \"@shared/data/config.json\"\nimport { NAME } from \"@shared/util\"\nprint(config.speed, NAME)\n",
    );

    dir
}

/// Every node of a sourcemap or a Rojo tree, sorted by name, so two
/// trees that hold the same instances compare equal whatever order
/// their file wrote them in.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();

            for k in keys {
                out.insert(k.clone(), canonical(&map[k]));
            }

            Value::Object(out)
        }

        Value::Array(list) => {
            let mut items: Vec<Value> = list.iter().map(canonical).collect();
            items.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());

            Value::Array(items)
        }

        other => other.clone(),
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn build(dir: &Path) {
    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);
}

#[test]
fn the_project_file_gives_the_same_tree_as_the_mount_table() {
    let from_file = example("file", false);
    let from_table = example("table", true);
    build(&from_file);
    build(&from_table);

    // The tree is the same, so the sourcemap and the build project are
    // the same tree of instances.
    assert_eq!(
        canonical(&read_json(&from_file.join("sourcemap.json"))),
        canonical(&read_json(&from_table.join("sourcemap.json"))),
        "the sourcemaps differ"
    );
    assert_eq!(
        canonical(&read_json(&from_file.join(".alloy/build.project.json"))),
        canonical(&read_json(&from_table.join(".alloy/build.project.json"))),
        "the build projects differ"
    );

    // Every emitted file is byte for byte the one the table produced.
    for rel in [
        "build/server/init.server.luau",
        "build/server/combat/hit.luau",
        "build/client/ui.client.luau",
        "build/shared/util.luau",
        "build/shared/data/config.luau",
    ] {
        assert_eq!(
            fs::read_to_string(from_file.join(rel)).unwrap(),
            fs::read_to_string(from_table.join(rel)).unwrap(),
            "{rel} differs"
        );
    }

    let _ = fs::remove_dir_all(&from_file);
    let _ = fs::remove_dir_all(&from_table);
}

#[test]
fn the_ship_artifact_carries_instance_paths() {
    let dir = example("ship", false);
    build(&dir);

    let hit = fs::read_to_string(dir.join("build/server/combat/hit.luau")).unwrap();
    assert!(
        hit.contains("require(\"@game/ReplicatedStorage/Packages/jecs\")"),
        "{hit}"
    );
    assert!(
        hit.contains("require(\"@game/ReplicatedStorage/Shared/util\")"),
        "{hit}"
    );

    // A data file resolves through the same alias and drops its
    // extension, since the build writes it as a module.
    let ui = fs::read_to_string(dir.join("build/client/ui.client.luau")).unwrap();
    assert!(
        ui.contains("require(\"@game/ReplicatedStorage/Shared/data/config\")"),
        "{ui}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_users_project_file_is_never_written() {
    let dir = example("keep", false);
    let before = fs::read_to_string(dir.join("default.project.json")).unwrap();
    build(&dir);

    assert_eq!(
        fs::read_to_string(dir.join("default.project.json")).unwrap(),
        before,
        "default.project.json changed"
    );
    assert!(dir.join(".alloy/build.project.json").is_file());
    assert!(dir.join("sourcemap.json").is_file());

    // The `.luaurc` keeps every byte too: nothing writes aliases now.
    assert_eq!(fs::read_to_string(dir.join(".luaurc")).unwrap(), LUAURC);

    // The table writes `default.project.json`, since a sync tool needs
    // a file to read.
    let table = example("keep-table", true);
    fs::remove_file(table.join("default.project.json")).unwrap();
    build(&table);
    assert!(table.join("default.project.json").is_file());

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&table);
}

#[test]
fn the_build_project_points_at_the_output() {
    let dir = example("build-project", false);
    build(&dir);

    let built = read_json(&dir.join(".alloy/build.project.json"));
    assert_eq!(built["name"], "game");
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
        "../packages/roblox"
    );
    assert_eq!(
        built["tree"]["ReplicatedStorage"]["Alloy"]["$path"],
        "../build/alloy.luau"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_sourcemap_names_every_script_of_the_tree() {
    let dir = example("sourcemap", false);
    build(&dir);

    let map = read_json(&dir.join("sourcemap.json"));
    assert_eq!(map["name"], "game");
    assert_eq!(map["className"], "DataModel");

    let services = map["children"].as_array().unwrap();
    let by = |name: &str| {
        services
            .iter()
            .find(|s| s["name"] == name)
            .unwrap_or_else(|| panic!("{name} is missing"))
            .clone()
    };

    let server = &by("ServerScriptService")["children"][0];
    assert_eq!(server["name"], "Server");
    assert_eq!(server["className"], "Script");
    assert_eq!(server["filePaths"][0], "src/server/init.server.aly");
    assert_eq!(server["children"][0]["name"], "combat");
    assert_eq!(
        server["children"][0]["children"][0]["filePaths"][0],
        "src/server/combat/hit.aly"
    );

    let client = &by("StarterPlayer")["children"][0]["children"][0];
    assert_eq!(client["name"], "Client");
    assert_eq!(client["children"][0]["className"], "LocalScript");

    let rs = by("ReplicatedStorage");
    let names: Vec<&str> = rs["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Alloy", "Packages", "Shared"]);

    let _ = fs::remove_dir_all(&dir);
}

/// A root with a `[mount]` table and no project file, in one of the
/// four settings of the two `[project]` keys.
fn mounted_root(name: &str, source_of_truth: bool, mount_aliases: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-mount-keys-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let toml = format!(
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nsource_of_truth = {source_of_truth}\nmount_aliases = {mount_aliases}\n{MOUNTS}"
    );
    write(&dir, "alloy.toml", &toml);
    write(&dir, "packages/roblox/jecs.luau", "return {}\n");
    write(
        &dir,
        "src/shared/util.aly",
        "export const NAME = \"util\"\n",
    );
    write(
        &dir,
        "src/server/main.server.aly",
        "import { NAME } from \"@shared/util\"\nimport jecs from \"@pkg/jecs\"\nprint(NAME, jecs)\n",
    );

    dir
}

#[test]
fn a_mount_alias_rewrites_in_every_setting_of_the_keys() {
    // The `[mount]` table names the aliases; no Luau configuration is
    // there, so `mount_aliases` decides whether they resolve at all.
    for (truth, alias) in [(true, true), (true, false), (false, true), (false, false)] {
        let dir = mounted_root(&format!("rewrite-{truth}-{alias}"), truth, alias);
        let config = Config::load(&dir.join("alloy.toml")).unwrap();
        let report = alloy::build::run_project(&dir, &config).unwrap();
        let main =
            fs::read_to_string(dir.join("build/server/main.server.luau")).unwrap_or_default();

        if alias {
            assert!(report.is_clean(), "{:?}", report.diagnostics);
            assert!(
                main.contains("require(\"@game/ReplicatedStorage/Shared/util\")"),
                "source_of_truth={truth} mount_aliases={alias}: {main}"
            );
            assert!(
                main.contains("require(\"@game/ReplicatedStorage/Packages/jecs\")"),
                "{main}"
            );
        } else {
            // With the mount aliases off and no `.luaurc`, the spec has
            // no alias to resolve: the build reports the module it could
            // not find, and a file with an error is not written.
            assert!(main.is_empty(), "{main}");
            assert_eq!(report.diagnostics.len(), 2, "{:?}", report.diagnostics);
            assert_eq!(
                report.skipped,
                vec![PathBuf::from("server/main.server.aly")]
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }
}

/// A mount with no source yet, `src/client` here, has no output
/// folder, and `rojo build` stopped at the missing `$path`. The build
/// makes an empty folder for it.
#[test]
fn every_path_of_the_build_project_is_there() {
    let dir = mounted_root("empty-mount", true, true);
    build(&dir);

    let built = read_json(&dir.join(".alloy/build.project.json"));
    let mut stack = vec![&built["tree"]];

    while let Some(node) = stack.pop() {
        let Some(map) = node.as_object() else {
            continue;
        };

        if let Some(path) = map.get("$path").and_then(Value::as_str) {
            assert!(dir.join(".alloy").join(path).exists(), "{path}");
        }

        stack.extend(map.values());
    }

    assert!(dir.join("build/client").is_dir());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn source_of_truth_decides_whether_the_project_files_are_written() {
    let owned = mounted_root("truth-on", true, true);
    build(&owned);
    assert!(owned.join("default.project.json").is_file());
    assert!(owned.join(".alloy/build.project.json").is_file());
    assert!(owned.join("sourcemap.json").is_file());

    let synced = mounted_root("truth-off", false, true);
    build(&synced);
    assert!(
        !synced.join("default.project.json").exists(),
        "the sync tool owns the project file"
    );
    assert!(
        !synced.join(".alloy/build.project.json").exists(),
        "no build project is derived"
    );
    // The sourcemap is the language server's input, and `[project]
    // sourcemap` governs it on its own.
    assert!(synced.join("sourcemap.json").is_file());

    // The requires are the same either way.
    assert_eq!(
        fs::read_to_string(owned.join("build/server/main.server.luau")).unwrap(),
        fs::read_to_string(synced.join("build/server/main.server.luau")).unwrap()
    );

    let _ = fs::remove_dir_all(&owned);
    let _ = fs::remove_dir_all(&synced);
}

#[test]
fn a_luau_config_alias_wins_over_a_mount_of_the_same_name() {
    let dir = mounted_root("clash", true, true);
    write(
        &dir,
        ".luaurc",
        "{ \"languageMode\": \"strict\", \"aliases\": { \"pkg\": \"vendor\" } }\n",
    );
    write(&dir, "vendor/jecs.luau", "return {}\n");
    build(&dir);

    // `vendor` sits under no mount, so the require keeps its alias
    // rather than taking the mounted path of the same name.
    let main = fs::read_to_string(dir.join("build/server/main.server.luau")).unwrap();
    assert!(main.contains("require(\"@pkg/jecs\")"), "{main}");
    assert!(
        main.contains("require(\"@game/ReplicatedStorage/Shared/util\")"),
        "{main}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `@game` names the Roblox services and `@alloy` names the runtime,
/// so a project that declares either alias never reaches its own
/// folder under that name. The report names the file it came from.
#[test]
fn a_reserved_alias_is_an_error_that_names_its_file() {
    let dir = std::env::temp_dir().join(format!("alloy-reserved-alias-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "alloy.toml",
        "[build]\nin = \"src\"\nout = \"build\"\n\n[mount]\ngame = [\"src/shared\", \"@game/ReplicatedStorage/G\"]\nalloy = [\"src/shared\", \"@game/ReplicatedStorage/A\"]\n",
    );
    write(
        &dir,
        ".luaurc",
        "{ \"languageMode\": \"strict\", \"aliases\": { \"game\": \"src/shared\", \"alloy\": \"./build/alloy\" } }\n",
    );
    write(
        &dir,
        "src/shared/util.aly",
        "export const NAME = \"util\"\n",
    );

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let found = alloy::modules::alias_problems(&dir, &config);
    let said: Vec<(String, String)> = found
        .iter()
        .map(|p| {
            (
                p.file.file_name().unwrap().to_string_lossy().into_owned(),
                p.message.clone(),
            )
        })
        .collect();

    // `alloy init` writes the `@alloy` alias into the Luau
    // configuration itself, so only the mount table reserves that name.
    assert_eq!(
        said,
        vec![
            (
                ".luaurc".to_string(),
                "`game` is reserved for the Roblox services; rename this alias".to_string()
            ),
            (
                "alloy.toml".to_string(),
                "`alloy` is reserved for the runtime; rename this alias".to_string()
            ),
            (
                "alloy.toml".to_string(),
                "`game` is reserved for the Roblox services; rename this alias".to_string()
            ),
        ]
    );

    // The build reports them, so `alloy check` does.
    let report = alloy::build::check_project(&dir, &config).unwrap();

    assert!(
        report
            .failures
            .iter()
            .filter(|(_, m)| m.contains("is reserved for"))
            .count()
            == 3,
        "{:?}",
        report.failures
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A project that declares neither name is clean.
#[test]
fn the_reserved_names_leave_every_other_alias_alone() {
    let dir = mounted_root("no-reserved", true, true);
    let config = Config::load(&dir.join("alloy.toml")).unwrap();

    assert!(alloy::modules::alias_problems(&dir, &config).is_empty());

    let _ = fs::remove_dir_all(&dir);
}

/// An alias-only `[mount]` entry: a bare string names a folder another
/// mount already carries, and gives it an alias alone.
#[test]
fn an_alias_only_mount_resolves_and_adds_no_instance() {
    let dir = std::env::temp_dir().join(format!("alloy-alias-only-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "alloy.toml",
        &format!("[build]\nin = \"src\"\nout = \"build\"\n{MOUNTS}types = \"src/shared/types\"\n"),
    );
    write(&dir, "src/shared/types/ids.aly", "export const LIMIT = 5\n");
    write(
        &dir,
        "src/server/main.server.aly",
        "import { LIMIT } from \"@types/ids\"\nprint(LIMIT)\n",
    );

    let config = Config::load(&dir.join("alloy.toml")).unwrap();

    assert!(config.mount["types"].alias_only());
    assert!(alloy::modules::alias_problems(&dir, &config).is_empty());

    let report = alloy::build::run_project(&dir, &config).unwrap();

    assert!(report.is_clean(), "{:?}", report.diagnostics);

    // The ship require takes the place the `shared` mount gives the
    // folder, so the alias needs no mount of its own.
    let main = fs::read_to_string(dir.join("build/server/main.server.luau")).unwrap_or_default();

    assert!(
        main.contains("require(\"@game/ReplicatedStorage/Shared/types/ids\")"),
        "{main}"
    );

    // The entry writes no node into the project file.
    let project: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("default.project.json")).unwrap())
            .unwrap();
    let shared = &project["tree"]["ReplicatedStorage"]["Shared"];

    assert_eq!(shared["$path"], "src/shared");
    assert!(project["tree"]["ReplicatedStorage"]["types"].is_null());

    let _ = fs::remove_dir_all(&dir);
}

/// An instance path resolves through the mount that holds it, a mount of
/// sources or of plain Luau. The resolver read `@game` as an alias,
/// found none, and reported that the path names no module.
#[test]
fn an_instance_path_resolves_through_its_mount() {
    let dir = std::env::temp_dir().join(format!("alloy-instance-path-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "alloy.toml",
        &format!("[build]\nin = \"src\"\nout = \"build\"\n{MOUNTS}"),
    );
    write(
        &dir,
        "src/shared/util.aly",
        "export const NAME = \"util\"\n",
    );
    write(&dir, "packages/roblox/jecs.luau", "return { world = 1 }\n");
    write(
        &dir,
        "src/server/main.server.aly",
        "import { NAME } from \"@game/ReplicatedStorage/Shared/util\"\nimport jecs from \"@game/ReplicatedStorage/Packages/jecs\"\nprint(NAME, jecs)\n",
    );

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();

    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let main = fs::read_to_string(dir.join("build/server/main.server.luau")).unwrap_or_default();

    assert!(
        main.contains("require(\"@game/ReplicatedStorage/Shared/util\")"),
        "{main}"
    );

    // A name the module does not export still reports.
    write(
        &dir,
        "src/server/main.server.aly",
        "import { NOPE } from \"@game/ReplicatedStorage/Shared/util\"\nprint(NOPE)\n",
    );
    let report = alloy::build::check_project(&dir, &config).unwrap();
    let messages: Vec<&str> = report
        .diagnostics
        .iter()
        .map(|(_, d)| d.message.as_str())
        .collect();

    assert_eq!(
        messages,
        ["\"@game/ReplicatedStorage/Shared/util\" does not export `NOPE`; it exports `NAME`"]
    );

    let _ = fs::remove_dir_all(&dir);
}

/// An alias-only entry whose folder no mount holds is an error: the
/// name resolves here and names nothing in the DataModel.
#[test]
fn an_alias_only_mount_outside_every_mount_reports() {
    let dir = std::env::temp_dir().join(format!("alloy-alias-loose-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "alloy.toml",
        &format!("[build]\nin = \"src\"\nout = \"build\"\n{MOUNTS}types = \"vendor/types\"\n"),
    );
    write(&dir, "vendor/types/ids.aly", "export const LIMIT = 5\n");

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let found = alloy::modules::alias_problems(&dir, &config);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, "MountAlias");
    assert!(
        found[0]
            .message
            .starts_with("`vendor/types` sits under no mount.")
    );

    // `alloy check` reports it, so the build never writes a require
    // that resolves to nothing in Roblox.
    let report = alloy::build::check_project(&dir, &config).unwrap();

    assert!(
        report
            .failures
            .iter()
            .any(|(_, m)| m.contains("sits under no mount")),
        "{:?}",
        report.failures
    );

    let _ = fs::remove_dir_all(&dir);
}

/// An alias names a folder: the editor's child reads one, so a path
/// that names a file reports rather than resolving in one tool only.
#[test]
fn an_alias_only_mount_that_names_a_file_reports() {
    let dir = std::env::temp_dir().join(format!("alloy-alias-file-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "alloy.toml",
        &format!(
            "[build]\nin = \"src\"\nout = \"build\"\n{MOUNTS}types = \"src/shared/ids.aly\"\n"
        ),
    );
    write(&dir, "src/shared/ids.aly", "export const LIMIT = 5\n");

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let found = alloy::modules::alias_problems(&dir, &config);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, "MountAlias");
    assert!(
        found[0]
            .message
            .starts_with("`src/shared/ids.aly` is no folder.")
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A table of alias-only entries alone is no tree, so the project file
/// at the root still describes one.
#[test]
fn alias_only_entries_alone_leave_the_project_file_as_the_tree() {
    let dir = std::env::temp_dir().join(format!("alloy-alias-rojo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(&dir, "default.project.json", PROJECT_JSON);
    write(
        &dir,
        "alloy.toml",
        "[build]\nin = \"src\"\nout = \"build\"\n\n[mount]\ntypes = \"src/shared/types\"\n",
    );
    write(&dir, "src/shared/types/ids.aly", "export const LIMIT = 5\n");

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let tree = alloy::project::Tree::load(&dir, &config);

    assert!(tree.project.is_some());
    assert_eq!(
        alloy::project::instance_path(&tree, Path::new("src/shared/types/ids.aly")).unwrap(),
        vec!["ReplicatedStorage", "Shared", "types", "ids"]
    );
    assert!(alloy::modules::alias_problems(&dir, &config).is_empty());

    let _ = fs::remove_dir_all(&dir);
}

/// A `.config.luau` that writes the keys above the `luau` table
/// declares nothing, which reads as an alias that gives no
/// intellisense. The report names the key to move.
#[test]
fn a_top_level_config_luau_key_reports_on_its_file() {
    let dir = std::env::temp_dir().join(format!("alloy-top-level-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "alloy.toml",
        "[build]\nin = \"src\"\nout = \"build\"\n",
    );
    write(
        &dir,
        ".config.luau",
        "return {\n    languagemode = \"strict\",\n    aliases = {\n        types = \"./src/shared/types\",\n    },\n}\n",
    );

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let found = alloy::modules::alias_problems(&dir, &config);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, "LuauConfig");
    assert_eq!(found[0].alias, "aliases");
    assert!(
        found[0]
            .message
            .contains("Move the key into `luau = { ... }`.")
    );

    // The same file under `luau` is clean.
    write(
        &dir,
        ".config.luau",
        "return {\n    luau = {\n        aliases = {\n            types = \"./src/shared/types\",\n        },\n    },\n}\n",
    );

    assert!(alloy::modules::alias_problems(&dir, &config).is_empty());

    let _ = fs::remove_dir_all(&dir);
}

/// `alloy flux` hands luau-lsp the sourcemap the language server does,
/// with each script at its artifact. A child then types from the tree,
/// and a `require` of one resolves the module, so a wrong argument to
/// it reports. The runtime still resolves in a file the tree places.
#[test]
fn flux_types_a_child_from_the_sourcemap() {
    let dir = std::env::temp_dir().join(format!("alloy-flux-sourcemap-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    write(
        &dir,
        "alloy.toml",
        "[build]\nin = \"src\"\nout = \"build\"\n\n[emit]\nwait_timeout = 5\n\n[mount]\nshared = [\"src\", \"@game/ReplicatedStorage/Shared\"]\n",
    );
    write(
        &dir,
        "src/tags.aly",
        "export const DOUBLED = [ 1, 2 ]:map(function(n) return n * 2 end)\n\nexport function add(a: number, b: number): number\n    return a + b\nend\n",
    );
    write(
        &dir,
        "src/systems/combat.aly",
        "export function hit(n: number): number\n    return n - 1\nend\n",
    );
    write(
        &dir,
        "src/main.aly",
        concat!(
            "const tags = require(script.Parent=>tags)\n",
            "const combat = require(script.Parent->systems->combat)\n",
            "const found: Folder = script.Parent:FindFirstChild(\"systems\")\n",
            "print(tags.add(\"x\", 1), found)\n",
            "print(combat.hit(\"y\"))\n",
        ),
    );

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(&dir, &config).unwrap();

    assert!(report.is_clean(), "{:?}", report.diagnostics);

    if alloy::typecheck::find_luau_lsp(&config.flux).is_none() {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    }

    let analysis = alloy::typecheck::analyze(&dir, &config, &report.checks, &report.dep_artifacts)
        .expect("the type check runs");
    let errors: Vec<String> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{}:{}:{} {}", d.rel.display(), d.line, d.col, d.message))
        .collect();

    assert_eq!(
        errors,
        vec![
            "main.aly:4:16 Expected this to be 'number', but got 'string'".to_string(),
            "main.aly:5:18 Expected this to be 'number', but got 'string'".to_string(),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
}
