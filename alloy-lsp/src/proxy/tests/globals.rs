//! The editor's reading of a project global: the workspace index, the
//! completion lists, the hover, and go to definition.

use super::super::*;
use super::support::files;

/// The two files of every case: a module that declares the globals and
/// a file that names them.
fn workspace() -> State {
    files(&[
        (
            "file:///shared/log.aly",
            "--- Writes a line.\nglobal function log(msg: string)\n    print(msg)\nend\n\nglobal struct Vec2 as\n    x: number\n    y: number\nend\n\nglobal const MAX = 10\n",
        ),
        (
            "file:///main.aly",
            "local n: Vec2 = nil :: any\nlog(`{MAX}`)\nprint(n)\n",
        ),
    ])
}

/// Every document says which globals it declares, and the workspace
/// set is the sum.
#[test]
fn the_workspace_index_holds_every_global() {
    let st = workspace();
    let mut names: Vec<String> = st.project_globals().into_iter().map(|g| g.name).collect();
    names.sort();
    assert_eq!(names, ["MAX", "Vec2", "log"]);
}

/// A global completes in the value scope, with the file that declares
/// it in the detail.
#[test]
fn a_global_completes_as_a_value() {
    let st = workspace();
    let items = st.global_completions("file:///main.aly", &[], false);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"log"), "{labels:?}");
    assert!(labels.contains(&"MAX"), "{labels:?}");
    assert!(labels.contains(&"Vec2"), "{labels:?}");

    let item = items
        .iter()
        .find(|i| i["label"] == "log")
        .expect("the item");
    // The detail says what the name is; the popup says where it lives.
    assert_eq!(item["detail"], json!("global function log"));
    assert!(
        item["documentation"]["value"]
            .as_str()
            .unwrap_or_default()
            .contains("shared/log.aly"),
        "{item}"
    );
}

/// A global type completes in a type slot, and a global function does
/// not: a function is no type.
#[test]
fn a_global_type_completes_in_a_type_slot() {
    let st = workspace();
    let items = st.global_completions("file:///main.aly", &[], true);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert_eq!(labels, ["Vec2"], "{labels:?}");
}

/// The file that declares a global does not offer it back to itself:
/// the name is already there, and the child lists it.
#[test]
fn the_declaring_file_offers_no_global_of_its_own() {
    let st = workspace();
    let items = st.global_completions("file:///shared/log.aly", &[], false);
    assert!(items.is_empty(), "{items:?}");
}

/// A name the answer already holds is not offered twice.
#[test]
fn a_label_the_child_answered_is_not_repeated() {
    let st = workspace();
    let items = st.global_completions("file:///main.aly", &["log"], false);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(!labels.contains(&"log"), "{labels:?}");
}

/// The hover of a global const reads the declaration in the file that
/// wrote it, not the local the emit binds on the first line.
#[test]
fn a_global_const_hovers_as_its_declaration() {
    let st = workspace();
    let doc = &st.docs["file:///shared/log.aly"];
    let text = super::super::hover::const_hover_of(&doc.source, "MAX").expect("a hover");
    assert!(text.contains("global const MAX"), "{text}");
}

/// A `global const` with no annotation still hovers with its type.
/// The declaring file knows it; the file that reads the name would
/// otherwise see the name alone.
#[test]
fn a_global_const_hovers_with_the_type_its_value_writes() {
    let st = workspace();
    let doc = &st.docs["file:///shared/log.aly"];
    let text = super::super::hover::const_hover_of(&doc.source, "MAX").expect("a hover");
    assert!(text.contains("global const MAX: number"), "{text}");
}

/// The completion detail of a global carries the type too.
#[test]
fn a_global_completes_with_its_type() {
    let st = workspace();
    let items = st.global_completions("file:///main.aly", &[], false);
    let item = items
        .iter()
        .find(|i| i["label"] == "MAX")
        .expect("the item");
    assert_eq!(item["detail"], json!("global const MAX: number"));
}

/// The child types a global off the binding the first line of the emit
/// writes and calls it a `local`. The keywords come from the file that
/// declared it, so the type the checker inferred reaches every file.
#[test]
fn a_global_keeps_its_inferred_type_in_another_file() {
    let st = files(&[
        (
            "file:///shared/log.aly",
            "global local counter = 0
global const MAX = 10
",
        ),
        (
            "file:///main.aly",
            "print(counter, MAX)
",
        ),
    ]);
    let doc = &st.docs["file:///main.aly"];
    let restyled = |word: &str, from_child: &str| {
        let at = doc.source.find(word).expect("the word") as u32;

        super::super::hover::restyle_global_hover(from_child, doc, &st, "file:///main.aly", 0, at)
    };
    assert_eq!(
        restyled(
            "counter",
            "```luau
local counter: number
```"
        ),
        Some(
            "```alloy
global local counter: number
```"
            .to_string()
        )
    );
    assert_eq!(
        restyled(
            "MAX",
            "```luau
local MAX: number
```"
        ),
        Some(
            "```alloy
global const MAX: number
```"
            .to_string()
        )
    );
}

/// The file that declares the name keeps its own keywords: the global
/// restyle is for the files that only read it.
#[test]
fn the_declaring_file_takes_no_global_restyle() {
    let st = files(&[
        (
            "file:///shared/log.aly",
            "global local counter = 0
",
        ),
        (
            "file:///main.aly",
            "print(counter)
",
        ),
    ]);
    let doc = &st.docs["file:///shared/log.aly"];
    let at = doc.source.find("counter").expect("the word") as u32;
    assert_eq!(
        super::super::hover::restyle_global_hover(
            "```luau
local counter: number
```",
            doc,
            &st,
            "file:///shared/log.aly",
            0,
            at,
        ),
        None
    );
}

/// The declaration of a global struct hovers with the modifier the
/// source wrote, so `export` and `global` read apart.
#[test]
fn a_global_struct_hovers_with_its_modifier() {
    let st = workspace();
    let doc = &st.docs["file:///shared/log.aly"];
    let decl = doc
        .decls
        .iter()
        .find(|d| d.name == "Vec2")
        .expect("the declaration");
    assert!(decl.hover.contains("global struct Vec2"), "{}", decl.hover);
}

/// Go to definition on a global lands on the name the declaring file
/// binds, not on the require the emit writes.
#[test]
fn a_global_definition_lands_on_the_declaration() {
    let st = workspace();
    let target = st
        .docs
        .iter()
        .find_map(|(u, d)| d.globals.iter().find(|g| g.name == "log").map(|g| (u, g)))
        .expect("the declaring file");
    assert_eq!(target.0, "file:///shared/log.aly");

    let source = &st.docs[target.0].source;
    let offset = target.1.offset as usize;
    assert_eq!(&source[offset..offset + 3], "log");
}

/// A space opens the side words after a side directive, and nothing
/// anywhere else.
#[test]
fn a_space_after_a_side_directive_lists_the_sides() {
    let st = files(&[(
        "file:///t.aly",
        "--@alloy-side \n--@alloy-file-side \nlocal x = \n",
    )]);
    let labels = |line: u32, character: u32| -> Vec<String> {
        st.side_word_completions("file:///t.aly", line, character)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(labels(0, 14), ["client", "server", "shared"]);
    assert_eq!(labels(1, 19), ["client", "server", "shared"]);
    assert!(labels(2, 10).is_empty());
}

/// Nine combinations: a client, a server, and a shared global, each
/// asked for from a client, a server, and a shared file. A file sees
/// its own side and the shared names, and nothing else.
#[test]
fn a_completion_offers_the_globals_of_the_asking_side() {
    let st = files(&[
        ("file:///c.client.aly", "global const CLIENT_ONE = 1\n"),
        ("file:///s.server.aly", "global const SERVER_ONE = 2\n"),
        ("file:///h.aly", "global const SHARED_ONE = 3\n"),
        ("file:///use.client.aly", "print(1)\n"),
        ("file:///use.server.aly", "print(1)\n"),
        ("file:///use.aly", "print(1)\n"),
    ]);
    let labels = |uri: &str| -> Vec<String> {
        let mut out: Vec<String> = st
            .global_completions(uri, &[], false)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect();
        out.sort();

        out
    };

    assert_eq!(
        labels("file:///use.client.aly"),
        ["CLIENT_ONE", "SHARED_ONE"]
    );
    assert_eq!(
        labels("file:///use.server.aly"),
        ["SERVER_ONE", "SHARED_ONE"]
    );
    assert_eq!(labels("file:///use.aly"), ["SHARED_ONE"]);
}

/// The same rule for the definition, the hover, and the references: a
/// global of the other side is out of scope, so nothing answers for it.
#[test]
fn the_other_side_answers_no_global() {
    let st = files(&[
        (
            "file:///c.client.aly",
            "--- The client's own.\nglobal const THEME = 1\n",
        ),
        ("file:///use.server.aly", "print(THEME)\n"),
        ("file:///use.client.aly", "print(THEME)\n"),
    ]);
    let reaches = |uri: &str| {
        st.docs.iter().any(|(u, d)| {
            d.globals
                .iter()
                .any(|g| g.name == "THEME" && st.global_reaches(uri, u, g))
        })
    };

    assert!(reaches("file:///use.client.aly"));
    assert!(!reaches("file:///use.server.aly"));
}

/// A global a script declares moves into a module the build writes
/// beside it. The popup names the script, which is the file the reader
/// can open.
#[test]
fn the_popup_names_the_script_that_declares_the_global() {
    let st = files(&[
        ("file:///main.server.aly", "global const LIMIT = 1\n"),
        ("file:///other.server.aly", "print(LIMIT)\n"),
    ]);
    let item = st
        .global_completions("file:///other.server.aly", &[], false)
        .into_iter()
        .find(|i| i["label"] == "LIMIT")
        .expect("the item");

    assert_eq!(item["detail"], json!("global const LIMIT: number"));
    assert_eq!(
        item["documentation"]["value"],
        json!("Declared in `main.server.aly`.")
    );
}
