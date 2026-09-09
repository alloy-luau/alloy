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

    let detail = items
        .iter()
        .find(|i| i["label"] == "log")
        .and_then(|i| i["detail"].as_str())
        .unwrap_or_default();
    assert!(detail.contains("shared/log.aly"), "{detail}");
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
