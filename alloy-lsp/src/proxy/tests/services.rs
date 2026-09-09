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

/// `import { | } from "game"`: the names in braces are the services.
#[test]
pub(crate) fn the_braces_of_a_game_import_list_the_services() {
    let src = "import {  } from \"game\"\n";
    let offset = src.find('{').unwrap() + 2;
    let items = items_at(src, offset);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();

    for name in ["Players", "ReplicatedStorage", "TweenService"] {
        assert!(labels.contains(&name), "{name} missing");
    }

    // No module name and no `type` keyword: `"game"` exports neither.
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

/// `import P from "game:|"`: the path after the colon is one service.
#[test]
pub(crate) fn a_game_colon_path_lists_the_services() {
    let src = "import P from \"game:\"\n";
    let offset = src.find("game:").unwrap() + "game:".len();
    let items = items_at(src, offset);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or(""))
        .collect();

    for name in ["Players", "RunService"] {
        assert!(labels.contains(&name), "{name} missing: {labels:?}");
    }

    // The edit replaces the segment after the colon, not the whole path.
    let players = items
        .iter()
        .find(|i| i["label"] == "Players")
        .expect("Players");
    assert_eq!(players["textEdit"]["newText"], "Players");
    assert_eq!(players["textEdit"]["range"]["start"]["character"], 20);
}

/// `import P from "|"`: the path list offers the services beside the
/// aliases and the directories.
#[test]
pub(crate) fn the_import_path_list_offers_game() {
    let src = "import P from \"\"\n";
    let offset = src.rfind('"').unwrap();
    let labels = labels_at(src, offset);

    assert!(labels.contains(&"game".to_string()), "{labels:?}");
    assert!(labels.contains(&"game:".to_string()), "{labels:?}");

    // A sibling module takes a `./`; `game` names no file, so it does
    // not.
    let items = items_at(src, offset);
    let game = items.iter().find(|i| i["label"] == "game").expect("game");
    assert_eq!(game["textEdit"]["newText"], "game");
}

/// The hover on the binding and on the path reads the import line and
/// what the service is.
#[test]
pub(crate) fn a_service_hover_reads_the_import_line() {
    let src = "import Players from 'game:Players'\nimport { RunService as Run } from 'game'\n\nprint(Players, Run)\n";
    let first = src.lines().next().unwrap();
    let second = src.lines().nth(1).unwrap();
    let on_binding = service_hover(src, "Players", None).expect("the binding");

    assert_eq!(
        on_binding,
        "```alloy\nimport Players from 'game:Players'\n```\n\
         `Players`: a Roblox service. The class extends `Instance`."
    );

    // On the path, `game` and the service name both answer, and the
    // line the caret sits on picks the import.
    assert_eq!(service_hover(src, "game", Some(first)), Some(on_binding));
    assert_eq!(
        service_hover(src, "game", Some(second)),
        Some(
            "```alloy\nimport { RunService as Run } from 'game'\n```\n\
             `RunService`: a Roblox service. The class extends `Instance`."
                .to_string()
        )
    );

    let alias = service_hover(src, "Run", None).expect("the alias");
    assert!(alias.starts_with("```alloy\nimport { RunService as Run } from 'game'"));
    assert!(alias.ends_with("`RunService`: a Roblox service. The class extends `Instance`."));

    assert_eq!(service_hover(src, "print", None), None);
}

/// The child offers a service as `local X = game:GetService("X")`.
/// Alloy writes the import instead: a line of its own, or the name in
/// the braces of an `import { ... } from "game"` the file already has.
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
        "import Players from \"game:Players\"\n"
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
        "import Players from \"game:Players\"\n"
    );
    assert_eq!(
        result[0]["additionalTextEdits"][0]["range"]["start"]["line"],
        1
    );

    // A `from "game"` line already there takes the name into its braces.
    let src = "import { RunService } from \"game\"\n\nprint(RunService)\n";
    let (st, uri) = one_file(src);
    let mut result = child("Players");
    st.rewrite_child_auto_imports(uri, &mut result);
    assert_eq!(
        result[0]["additionalTextEdits"][0]["newText"],
        "import { RunService, Players } from \"game\""
    );
    assert_eq!(
        result[0]["additionalTextEdits"][0]["range"]["start"]["line"],
        0
    );

    // A service the file imports is no offer, in either form.
    for src in [
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
    let src = "import Players from 'game:Players'\nimport { RunService as Run } from 'game'\n\nprint(Players, Run)\n";
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
