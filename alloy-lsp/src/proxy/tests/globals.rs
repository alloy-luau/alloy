//! The editor's side of the `global` removal: the report's quick fix,
//! the hover on the word, and the auto import that replaces it.

use super::super::*;
use super::support::files;

/// The declaration reports on the word, and the quick fix writes
/// `export` over it.
#[test]
fn a_global_declaration_offers_the_export_fix() {
    let st = files(&[("file:///a.aly", "--- A count.\nglobal local counter = 0\n")]);
    let actions = st.global_actions("file:///a.aly", ((0, 0), (5, 0)));
    assert_eq!(actions.len(), 1, "{actions:?}");
    let action = &actions[0];
    assert_eq!(action["title"], json!("replace `global` with `export`"));
    let edit = &action["edit"]["changes"]["file:///a.aly"][0];
    assert_eq!(edit["newText"], json!("export"));
    // The range covers the keyword alone, on the line it sits on.
    assert_eq!(edit["range"]["start"], json!({ "line": 1, "character": 0 }));
    assert_eq!(edit["range"]["end"], json!({ "line": 1, "character": 6 }));
    assert!(
        action["diagnostics"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .starts_with("ImportError: `global` is removed;"),
        "{action}"
    );
}

/// A line the range does not cover offers nothing, so the light bulb
/// stays where the caret is.
#[test]
fn a_declaration_outside_the_range_offers_nothing() {
    let st = files(&[(
        "file:///a.aly",
        "local ok = true\n\n\n\nglobal const LIMIT = 5\n",
    )]);

    assert!(
        st.global_actions("file:///a.aly", ((0, 0), (1, 0)))
            .is_empty()
    );
    assert_eq!(
        st.global_actions("file:///a.aly", ((4, 0), (4, 0))).len(),
        1
    );
}

/// The word hovers as the removal notice, with the two forms that
/// replace it.
#[test]
fn the_keyword_hovers_as_the_removal_notice() {
    let src = "global const LIMIT = 5\n";
    let (_, _, text) = crate::keywords::hover(src, 0).expect("the hover");
    assert!(text.contains("`global` is removed"), "{text}");
    assert!(text.contains("export local"), "{text}");
    assert!(text.contains("import {"), "{text}");
}

/// The keyword completion never offers the word.
#[test]
fn the_keyword_list_holds_no_global() {
    assert!(!crate::keywords::ALLOY_KEYWORDS.contains(&"global"));
    assert!(!crate::keywords::is_keyword("global"));
    assert!(crate::keywords::starting_with("glo").is_empty());
}

/// A name that was global elsewhere is an unknown name now, and the
/// report carries the auto import that writes the line.
#[test]
fn a_name_that_was_global_offers_its_import() {
    let st = files(&[
        ("file:///a.aly", "--- A count.\nexport local counter = 0\n"),
        ("file:///b.aly", "print(counter)\n"),
    ]);
    let reported = vec![json!({
        "range": {
            "start": { "line": 0, "character": 6 },
            "end": { "line": 0, "character": 13 },
        },
        "message": "TypeError: Unknown global 'counter'; consider assigning to it first",
    })];
    let actions = st.import_actions("file:///b.aly", &reported);
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert_eq!(
        titles,
        ["Add `import { counter } from \"./a\"`"],
        "{actions:?}"
    );
    let edit = &actions[0]["edit"]["changes"]["file:///b.aly"][0];
    assert_eq!(
        edit["newText"],
        json!("import { counter } from \"./a\"\n"),
        "{actions:?}"
    );
}
