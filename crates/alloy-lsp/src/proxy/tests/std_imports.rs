//! `[std] globals = "none"` in the editor: a std row names its module
//! and writes the import the file lacks, and the report takes a fix.

use super::super::*;
use alloy::std_names::Globals;

/// One document compiled the way a project on the default reads it.
fn none_file(src: &str) -> (State, &'static str) {
    let uri = "file:///t.aly";
    let mut st = State {
        root: Some(PathBuf::from("/")),
        mirror: PathBuf::from("/m"),
        snippets: true,
        ..State::default()
    };
    let options = EmitOptions {
        std_globals: Globals::None,
        ..EmitOptions::default()
    };
    st.docs.insert(
        uri.to_string(),
        Doc::new(
            src.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        ),
    );

    (st, uri)
}

fn row<'a>(items: &'a [Value], label: &str) -> &'a Value {
    items
        .iter()
        .find(|i| i["label"] == label)
        .unwrap_or_else(|| panic!("no row {label}"))
}

#[test]
fn a_std_row_names_its_module_and_writes_its_import() {
    let child = json!([{ "label": "print", "kind": 3 }]);
    let (st, uri) = none_file("local x = \n");
    let items = st.std_completions(uri, 0, 10, &child);
    let map = row(&items, "HashMap");

    assert_eq!(map["detail"], "alloy:std:collections");
    assert_eq!(
        map["additionalTextEdits"][0]["newText"],
        "import { HashMap } from '@alloy/std/collections'\n"
    );

    // The language owns `Ok`: no import.
    let ok = row(&items, "Ok");
    assert_eq!(ok["detail"], "alloy:std:result");
    assert!(ok.get("additionalTextEdits").is_none(), "{ok}");

    // An import the file has, and a project that keeps every name
    // ambient, write nothing.
    let (st, uri) = none_file("import { HashMap } from \"@alloy/std/collections\"\nlocal x = \n");
    let items = st.std_completions(uri, 1, 10, &child);
    assert!(row(&items, "HashMap").get("additionalTextEdits").is_none());

    let (st, uri) = super::support::one_file("local x = \n");
    let items = st.std_completions(uri, 0, 10, &child);
    assert!(row(&items, "HashMap").get("additionalTextEdits").is_none());
    assert_eq!(row(&items, "HashMap")["detail"], "alloy:std:collections");
}

#[test]
fn a_derive_name_writes_its_serde_import() {
    let src = "@derive(Eq, Se\nstruct P as\n    x: number\nend\n";
    let at = src.find("Se\n").unwrap() + 2;
    let (st, uri) = none_file(src);
    let ctx = context::detect(src, at).expect("a context");
    let items = st.context_items(uri, at, &ctx);
    let ser = row(&items, "Serialize");

    assert_eq!(ser["detail"], "alloy:std:serde");
    assert_eq!(
        ser["additionalTextEdits"][0]["newText"],
        "import { Serialize } from '@alloy/std/serde'\n"
    );
    assert!(
        row(&items, "Deserialize")
            .get("additionalTextEdits")
            .is_some()
    );
    assert!(row(&items, "Eq").get("additionalTextEdits").is_none());
}

/// A type slot rebuilds each row for its range, and the import edit
/// rides along: `local m: HashM|` writes the import a value slot does.
#[test]
fn a_type_slot_std_row_writes_its_import() {
    for src in [
        "local m: HashM",
        "type P = Parti",
        "local function f(x: Ite",
    ] {
        let at = src.len();
        let (st, uri) = none_file(src);
        let ctx = context::detect(src, at).expect("a context");
        let items = st.context_items(uri, at, &ctx);
        let name = match src.chars().last() {
            Some('M') => "HashMap",
            Some('i') => "Partial",
            _ => "Iter",
        };
        let item = row(&items, name);
        let module = alloy::std_names::module_of(name).unwrap();

        assert_eq!(item["detail"], format!("alloy:std:{module}"), "{src}");
        assert_eq!(
            item["additionalTextEdits"][0]["newText"],
            format!("import {{ {name} }} from '@alloy/std/{module}'\n"),
            "{src}"
        );
    }
}

#[test]
fn a_std_import_completes_its_modules_and_names() {
    let labels = |src: &str, at: usize| {
        let (st, uri) = none_file(src);
        let ctx = context::detect(src, at).expect("a context");

        st.context_items(uri, at, &ctx)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect::<Vec<_>>()
    };

    let src = "import {  } from \"@alloy/std/serde\"\n";
    let names = labels(src, src.find("{ ").unwrap() + 2);
    assert!(names.contains(&"Serialize".to_string()), "{names:?}");
    assert!(names.contains(&"Deserialize".to_string()), "{names:?}");
    assert!(!names.contains(&"HashMap".to_string()), "{names:?}");

    let src = "import { HashMap } from \"@alloy/std/\"\n";
    let modules = labels(src, src.find("std/").unwrap() + 4);
    for m in ["collections", "serde", "iter"] {
        assert!(modules.contains(&m.to_string()), "{m}: {modules:?}");
    }
}

#[test]
fn the_report_takes_a_fix_and_one_for_every_name() {
    let src =
        "local m = new HashMap<<string, number>>()\nlocal i: Iter<number>? = nil\nprint(m, i)\n";
    let (st, uri) = none_file(src);
    let actions = st.compiler_actions(uri, ((0, 0), (99, 0)));
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();

    assert!(
        titles.contains(&"Import `HashMap` from \"@alloy/std/collections\""),
        "{titles:?}"
    );
    assert!(
        titles.contains(&"Import every std name this file uses"),
        "{titles:?}"
    );

    let every = actions
        .iter()
        .find(|a| a["title"] == "Import every std name this file uses")
        .unwrap();
    assert_eq!(
        every["edit"]["changes"][uri][0]["newText"],
        "import { HashMap } from '@alloy/std/collections'\nimport { Iter } from '@alloy/std/iter'\n"
    );
}

/// A hint's edit writes the annotation, and the import of a std type the
/// file does not reach, so the accepted text compiles.
#[test]
fn a_hint_writes_the_std_import_it_needs() {
    let (st, uri) = none_file("local i = nil\nprint(i)\n");
    let doc = st.docs.get(uri).expect("doc");
    let at = json!({ "line": 0, "character": 7 });
    let mut hints = vec![json!({
        "position": at,
        "kind": 1,
        "label": ": Iter<number>?",
        "textEdits": [{ "range": { "start": at, "end": at }, "newText": ": Iter<number>?" }]
    })];
    clean_hints(&mut hints, doc);

    let edits = hints[0]["textEdits"].as_array().expect("edits");
    assert_eq!(edits[0]["newText"], ": Iter<number>?");
    assert_eq!(
        edits[1]["newText"],
        "import { Iter } from '@alloy/std/iter'\n"
    );
}

/// Luau prints a std collection by its metatable, `HashMap`, and an
/// `Iter` as the table of its methods. The hint reads the arguments the
/// `new` wrote, and a collection whose arguments nothing names inserts
/// nothing: `: HashMap` does not compile.
#[test]
fn a_std_collection_hint_keeps_its_arguments() {
    let src = "local m = new HashMap<<string, number>>()\nlocal f = HashMap.from({ a = 1 })\nlocal i = Iter.from({ 1 })\n";
    let (st, uri) = none_file(src);
    let doc = st.docs.get(uri).expect("doc");
    let hint = |line: u32, label: &str| {
        let at = json!({ "line": line, "character": 7 });

        json!({
            "position": at,
            "kind": 1,
            "label": label,
            "textEdits": [{ "range": { "start": at, "end": at }, "newText": label }]
        })
    };
    let mut hints = vec![
        hint(0, ": HashMap"),
        hint(1, ": HashMap"),
        hint(
            2,
            ": { all: (self: Iter<number>, (number) -> boolean) -> boolean, ... 16 more ... }",
        ),
    ];
    clean_hints(&mut hints, doc);

    let labels: Vec<String> = hints.iter().map(hint_label).collect();
    assert_eq!(
        labels,
        [": HashMap<string, number>", ": HashMap", ": Iter<number>"]
    );
    assert_eq!(
        hints[0]["textEdits"][1]["newText"],
        "import { HashMap } from '@alloy/std/collections'\n"
    );
    assert!(hints[1].get("textEdits").is_none(), "{}", hints[1]);
    assert_eq!(hints[2]["textEdits"][0]["newText"], ": Iter<number>");
}

/// A std name under its own name is no binding of the file, so its
/// hover reads the std doc; under an alias it is the file's own.
#[test]
fn a_std_import_binds_its_aliases_alone() {
    assert_eq!(
        crate::imports::bound_names(
            "import { HashMap, Set as S } from \"@alloy/std/collections\"\nimport * as sig from \"@alloy/std/signal\"\nimport { a } from \"./a\"\n"
        ),
        ["S", "sig", "a"]
    );
}

/// serde's options sit in `@alloy/std/serde`: the `@` row writes the
/// import, `@serde.` lists them through a star import, and a name in
/// the import list hovers as the attribute or the derive it is.
#[test]
fn a_serde_option_imports_and_reads_through_its_module() {
    let src = "import * as serde from \"@alloy/std/serde\"\nimport { Serialize, rename } from \"@alloy/std/serde\"\n\n@serde.\nstruct S\n    @ren\n    x: number\nend\n";
    let (st, uri) = none_file(src);
    let at = |needle: &str| src.find(needle).unwrap() + needle.len();

    let offset = at("@serde.");
    let ctx = context::detect(src, offset).expect("a path context");
    let items = st.context_items(uri, offset, &ctx);
    let mut labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        ["deny_unknown_fields", "rename", "rename_all", "skip"]
    );

    // `rename` is imported; `skip` is not, so its row writes the import.
    let offset = at("    @ren");
    let ctx = context::detect(src, offset).expect("an attribute context");
    let items = st.context_items(uri, offset, &ctx);
    assert!(row(&items, "@rename").get("additionalTextEdits").is_none());
    assert_eq!(
        row(&items, "@skip")["additionalTextEdits"][0]["newText"],
        ", skip"
    );

    for word in ["Serialize", "rename"] {
        let offset = src
            .find(&format!("{word},"))
            .or_else(|| src.find(&format!("{word} }}")))
            .unwrap();
        let (_, _, text) = keywords::hover(src, offset).expect("a hover in the import list");
        assert!(
            text.contains(&format!(
                "@{}",
                if word == "rename" {
                    "rename"
                } else {
                    "derive(Serialize"
                }
            )),
            "{word}: {text}"
        );
    }
}
