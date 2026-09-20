//! The Roblox services as imports, in the editor: the three completion
//! positions, the hover, and the auto-import edit.

use super::super::*;
use super::support::one_file;

/// The labels a completion context gives at an offset.
fn labels_at(src: &str, offset: usize) -> Vec<String> {
    let (st, uri) = one_file(src);
    let ctx = context::detect(src, offset).expect("a context");

    st.context_items(uri, offset, &ctx)
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect()
}

fn items_at(src: &str, offset: usize) -> Vec<Value> {
    let (st, uri) = one_file(src);
    let ctx = context::detect(src, offset).expect("a context");

    st.context_items(uri, offset, &ctx)
}

/// `import { | } from "@game"`: the names in braces are the services.
#[test]
pub(crate) fn the_braces_of_a_game_import_list_the_services() {
    let src = "import {  } from \"@game\"\n";
    let offset = src.find('{').unwrap() + 2;
    let items = items_at(src, offset);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();

    for name in ["Players", "ReplicatedStorage", "TweenService"] {
        assert!(labels.contains(&name), "{name} missing");
    }

    // No module name and no `type` keyword: `"@game"` exports neither.
    assert!(!labels.contains(&"type"), "{labels:?}");

    let players = items
        .iter()
        .find(|i| i["label"] == "Players")
        .expect("Players");
    assert_eq!(players["detail"], "game:GetService(\"Players\")");
    assert!(
        players["documentation"]["value"]
            .as_str()
            .unwrap_or("")
            .starts_with("`Players`: a Roblox service."),
        "{players}"
    );
}

/// `import P from "@game/|"`: the segment after the alias is one
/// service. The old `"game:|"` opens the same list.
#[test]
pub(crate) fn the_segment_after_the_alias_lists_the_services() {
    for (src, head, character) in [
        ("import P from \"@game/\"\n", "@game/", 21),
        ("import P from \"game:\"\n", "game:", 20),
    ] {
        let offset = src.find(head).unwrap() + head.len();
        let items = items_at(src, offset);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();

        for name in ["Players", "RunService"] {
            assert!(labels.contains(&name), "{name} missing: {labels:?}");
        }

        // The edit replaces the segment after the alias, not the whole
        // path, and the item carries the class summary.
        let players = items
            .iter()
            .find(|i| i["label"] == "Players")
            .expect("Players");
        assert_eq!(players["textEdit"]["newText"], "Players");
        assert_eq!(
            players["textEdit"]["range"]["start"]["character"],
            character
        );
        assert_eq!(players["detail"], "game:GetService(\"Players\")");
        assert!(
            players["documentation"]["value"]
                .as_str()
                .unwrap_or("")
                .starts_with("`Players`: a Roblox service."),
            "{players}"
        );
    }

    // A second segment names an instance, not a service, so the list
    // is the sourcemap's, which an empty project has none of.
    let src = "import P from \"@game/ReplicatedStorage/\"\n";
    let offset = src.rfind('"').unwrap();

    assert!(labels_at(src, offset).is_empty());
}

/// `import P from "|"`: the path list offers the alias beside the
/// project's own and the directories, and the old spellings are gone.
#[test]
pub(crate) fn the_import_path_list_offers_the_game_alias() {
    let src = "import P from \"\"\n";
    let offset = src.rfind('"').unwrap();
    let labels = labels_at(src, offset);

    assert!(labels.contains(&"@game".to_string()), "{labels:?}");
    assert!(labels.contains(&"@game/".to_string()), "{labels:?}");
    assert!(!labels.contains(&"game".to_string()), "{labels:?}");
    assert!(!labels.contains(&"game:".to_string()), "{labels:?}");

    // A sibling module takes a `./`; the alias names no file, so it
    // keeps the text it shows.
    let items = items_at(src, offset);
    let game = items.iter().find(|i| i["label"] == "@game").expect("@game");
    assert_eq!(game["textEdit"]["newText"], "@game");
}

/// The hover on the binding and on the path reads the import line and
/// what the service is.
#[test]
pub(crate) fn a_service_hover_reads_the_import_line() {
    let src = "import Players from '@game/Players'\nimport { RunService as Run } from '@game'\n\nprint(Players, Run)\n";
    let first = src.lines().next().unwrap();
    let second = src.lines().nth(1).unwrap();
    let on_binding = service_hover(src, "Players", None).expect("the binding");

    // The binding names its type, the way every other binding hovers.
    assert_eq!(
        on_binding,
        "```alloy\nlocal Players: Players\n```\n\
         `Players`: a Roblox service. The class extends `Instance`."
    );

    // Inside the path one word can name several services, so the line
    // the reader is on stands instead.
    assert_eq!(
        service_hover(src, "game", Some(first)),
        Some(
            "```alloy\nimport Players from '@game/Players'\n```\n\
             `Players`: a Roblox service. The class extends `Instance`."
                .to_string()
        )
    );
    assert_eq!(
        service_hover(src, "game", Some(second)),
        Some(
            "```alloy\nimport { RunService as Run } from '@game'\n```\n\
             `RunService`: a Roblox service. The class extends `Instance`."
                .to_string()
        )
    );

    // An alias keeps its own name and carries the service's type.
    let alias = service_hover(src, "Run", None).expect("the alias");
    assert!(
        alias.starts_with("```alloy\nlocal Run: RunService\n```"),
        "{alias}"
    );
    assert!(alias.ends_with("`RunService`: a Roblox service. The class extends `Instance`."));

    assert_eq!(service_hover(src, "print", None), None);

    // The old spelling still hovers the same way.
    let old = "import Players from 'game:Players'\n\nprint(Players)\n";
    assert_eq!(
        service_hover(old, "Players", None),
        Some(
            "```alloy\nlocal Players: Players\n```\n\
             `Players`: a Roblox service. The class extends `Instance`."
                .to_string()
        )
    );
}

/// The child offers a service as `local X = game:GetService("X")`.
/// Alloy writes the import instead: a line of its own, or the name in
/// the braces of an `import { ... } from "@game"` the file already has.
#[test]
pub(crate) fn a_service_auto_import_writes_an_import_line() {
    let child = |service: &str| {
        json!([{
            "label": service,
            "kind": 9,
            "detail": "Auto-import",
            "additionalTextEdits": [{
                "newText": format!("local {service} = game:GetService(\"{service}\")\n"),
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
            }],
        }])
    };

    // No import in the file: a new line at the top.
    let (st, uri) = one_file("print(1)\n");
    let mut result = child("Players");
    st.rewrite_child_auto_imports(uri, &mut result);
    assert_eq!(result[0]["label"], "Players");
    assert_eq!(result[0]["detail"], "game:GetService(\"Players\")");
    assert_eq!(
        result[0]["additionalTextEdits"][0]["newText"],
        "import Players from '@game/Players'\n"
    );
    assert_eq!(
        result[0]["additionalTextEdits"][0]["range"]["start"]["line"],
        0
    );

    // A file with imports takes the line under the last one.
    let (st, uri) = one_file("import { helper } from \"./util\"\n\nprint(helper)\n");
    let mut result = child("Players");
    st.rewrite_child_auto_imports(uri, &mut result);
    assert_eq!(
        result[0]["additionalTextEdits"][0]["newText"],
        "import Players from '@game/Players'\n"
    );
    assert_eq!(
        result[0]["additionalTextEdits"][0]["range"]["start"]["line"],
        1
    );

    // A `from "@game"` line already there takes the name into its
    // braces. A file still on the old spelling keeps its own line, so
    // the edit never opens a second list beside the one it has.
    for (src, want) in [
        (
            "import { RunService } from \"@game\"\n\nprint(RunService)\n",
            "import { RunService, Players } from \"@game\"",
        ),
        (
            "import { RunService } from \"game\"\n\nprint(RunService)\n",
            "import { RunService, Players } from \"game\"",
        ),
    ] {
        let (st, uri) = one_file(src);
        let mut result = child("Players");
        st.rewrite_child_auto_imports(uri, &mut result);
        assert_eq!(
            result[0]["additionalTextEdits"][0]["newText"], want,
            "{src}"
        );
        assert_eq!(
            result[0]["additionalTextEdits"][0]["range"]["start"]["line"],
            0
        );
    }

    // A service the file imports is no offer, in any form.
    for src in [
        "import { Players } from \"@game\"\n\nprint(Players)\n",
        "import Players from \"@game/Players\"\n\nprint(Players)\n",
        "import { Players } from \"game\"\n\nprint(Players)\n",
        "import Players from \"game:Players\"\n\nprint(Players)\n",
    ] {
        let (st, uri) = one_file(src);
        let mut result = child("Players");
        st.rewrite_child_auto_imports(uri, &mut result);
        assert_eq!(result.as_array().unwrap().len(), 0, "{src}");
    }
}

/// Go to definition on a service binding lands on the name the import
/// line binds, alias included.
#[test]
pub(crate) fn a_service_binding_goes_to_its_import() {
    let src = "import Players from '@game/Players'\nimport { RunService as Run } from '@game'\n\nprint(Players, Run)\n";
    let at = |word: &str| service_definition(src, "file:///t.aly", word);

    assert_eq!(
        at("Players"),
        Some(json!([{ "uri": "file:///t.aly", "range": {
            "start": { "line": 0, "character": 7 },
            "end": { "line": 0, "character": 14 } } }]))
    );
    // The alias, not the `RunService` it renames.
    assert_eq!(
        at("Run"),
        Some(json!([{ "uri": "file:///t.aly", "range": {
            "start": { "line": 1, "character": 23 },
            "end": { "line": 1, "character": 26 } } }]))
    );
    assert_eq!(at("print"), None);
}

/// A child auto-import row that names a module under a dot folder or an
/// `_Index` folder is dropped: the folder holds a package's own store,
/// not a module the author writes. The editor's `ignoreGlobs` name more.
#[test]
pub(crate) fn a_package_store_is_no_auto_import() {
    let row = |instance: &str, path: &str| {
        json!({
            "label": "y",
            "kind": 9,
            "detail": instance,
            "documentation": { "kind": "markdown", "value": format!(
                "```luau\nlocal y = require({instance})\n\n```\n\n{path}"
            ) },
        })
    };
    let (st, _) = one_file("print(1)\n");
    let ignored = st.import_ignore_globs();

    for (instance, path) in [
        (
            "ReplicatedStorage.Packages[\".ember\"].jecs.test.lol",
            "game/ReplicatedStorage/Packages/.ember/jecs/test/lol",
        ),
        (
            "ReplicatedStorage.Packages._Index.vide.src.mount",
            "game/ReplicatedStorage/Packages/_Index/vide/src/mount",
        ),
    ] {
        assert!(
            ignored.hides(&row(instance, path), instance),
            "{instance} must be hidden"
        );
    }

    let plain = "ReplicatedStorage.Packages.vide";
    assert!(!ignored.hides(&row(plain, "game/ReplicatedStorage/Packages/vide"), plain));

    // A glob the editor sends replaces the default one.
    let mut st = st;
    st.settings = json!({ "completion": { "imports": { "ignoreGlobs": ["**/bench/**"] } } });
    let ignored = st.import_ignore_globs();
    let bench = "ReplicatedStorage.Packages.vide.bench.run";
    assert!(ignored.hides(
        &row(bench, "game/ReplicatedStorage/Packages/vide/bench/run"),
        bench
    ));
    // The default is gone, so `_Index` now falls to the segment test,
    // which stands on its own.
    let index = "ReplicatedStorage.Packages._Index.vide";
    assert!(ignored.hides(
        &row(index, "game/ReplicatedStorage/Packages/_Index/vide"),
        index
    ));
}

/// An instance path spells a part that is no identifier in brackets.
#[test]
pub(crate) fn an_instance_path_reads_a_bracket_part() {
    use super::super::navigation::instance_segments;

    assert_eq!(
        instance_segments("ReplicatedStorage.Packages[\".ember\"].jecs"),
        ["ReplicatedStorage", "Packages", ".ember", "jecs"]
    );
    assert_eq!(
        instance_segments("A.B[\"spawn.bench\"]"),
        ["A", "B", "spawn.bench"]
    );
    assert_eq!(instance_segments("A.B"), ["A", "B"]);
    assert_eq!(instance_segments(""), Vec::<String>::new());
}

/// The whole pass over a project: the child offers a module under
/// `packages/.ember` and one beside it. Only the second reaches the
/// reader, as an Alloy import through the mount alias.
#[test]
pub(crate) fn the_auto_import_pass_drops_a_store_module() {
    let dir = std::env::temp_dir().join(format!("alloy-store-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("packages/.ember/x")).expect("temp dir");
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n\n\
         [mount]\npkg = [\"packages\", \"@game/ReplicatedStorage/Packages\"]\n",
    )
    .expect("toml");
    std::fs::write(dir.join("packages/.ember/x/y.luau"), "return {}\n").expect("store module");
    std::fs::write(dir.join("packages/vide.luau"), "return {}\n").expect("package");

    let src = "print(1)\n";
    let main = dir.join("src/main.aly");
    std::fs::write(&main, src).expect("main");

    let uri = format!("file://{}", main.display());
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        snippets: true,
        ..State::default()
    };
    let options = EmitOptions {
        file_name: main.to_string_lossy().into_owned(),
        ..EmitOptions::default()
    };
    st.docs.insert(
        uri.clone(),
        Doc::new(
            src.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        ),
    );

    // The child binds the service the require reads, so each row
    // carries two edits.
    let row = |name: &str, instance: &str, path: &str| {
        json!({
            "label": name,
            "kind": 9,
            "detail": instance,
            "documentation": { "kind": "markdown", "value": format!(
                "```luau\nlocal ReplicatedStorage = game:GetService(\"ReplicatedStorage\")\n\
                 local {name} = require({instance})\n\n```\n\n{path}"
            ) },
            "additionalTextEdits": [
                {
                    "newText": "local ReplicatedStorage = game:GetService(\"ReplicatedStorage\")\n",
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                },
                {
                    "newText": format!("local {name} = require({instance})\n"),
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                },
            ],
        })
    };
    let mut result = json!([
        row(
            "y",
            "ReplicatedStorage.Packages[\".ember\"].x.y",
            "game/ReplicatedStorage/Packages/.ember/x/y",
        ),
        row(
            "vide",
            "ReplicatedStorage.Packages.vide",
            "game/ReplicatedStorage/Packages/vide",
        ),
    ]);
    st.rewrite_child_auto_imports(&uri, &mut result);

    let items = result.as_array().expect("the rows");
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["label"], "vide");
    assert_eq!(items[0]["detail"], "@pkg/vide");
    assert_eq!(
        items[0]["additionalTextEdits"][0]["newText"],
        "import vide from '@pkg/vide'\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
