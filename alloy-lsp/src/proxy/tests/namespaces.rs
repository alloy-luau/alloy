//! A `namespace` in the editor: the list a member completes in, the
//! types a type slot takes, and the name a hover reads.

use super::super::*;
use super::support::{files, one_file};

fn labels(result: &Value) -> Vec<String> {
    result
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap_or("").to_string())
        .collect()
}

const SRC: &str = concat!(
    "namespace Math as\n",
    "    const PI = 3.14\n",
    "    private const seed = 7\n",
    "    struct Vec2 as\n",
    "        x: number\n",
    "    end\n",
    "    function unit(): number\n",
    "        return \n",
    "    end\n",
    "end\n",
    "\n",
    "print(Math.PI)\n",
);

/// Inside the namespace the child offers the emitted names; the list
/// reads them the way the source wrote them.
#[test]
fn a_member_completes_without_the_prefix_inside() {
    let (st, uri) = one_file(SRC);
    let mut result = json!([
        { "label": "Math_PI", "kind": 6 },
        { "label": "Math_seed", "kind": 6 },
        { "label": "Math_Vec2", "kind": 7 },
        { "label": "print", "kind": 3 },
    ]);
    // Line 7, the empty `return` inside `unit`.
    st.mark_namespaces(uri, 7, 15, &mut result);
    let mut got = labels(&result);
    got.sort();
    assert_eq!(got, ["PI", "Vec2", "print", "seed"]);

    let pi = result
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "PI")
        .unwrap();
    assert_eq!(pi["insertText"], json!("PI"));
    assert!(
        pi["documentation"]["value"]
            .as_str()
            .unwrap_or("")
            .contains("Math.PI"),
        "{pi}"
    );
}

/// Outside the namespace the emitted names are nobody's: the list
/// drops them and the reader writes `Math.` instead.
#[test]
fn a_member_leaves_the_list_outside() {
    let (st, uri) = one_file(SRC);
    let mut result = json!([
        { "label": "Math_PI", "kind": 6 },
        { "label": "Math_Vec2", "kind": 7 },
        { "label": "Math", "kind": 6 },
        { "label": "print", "kind": 3 },
    ]);
    // Line 11, inside `print(Math.PI)`, past the call.
    st.mark_namespaces(uri, 11, 0, &mut result);
    let mut got = labels(&result);
    got.sort();
    assert_eq!(got, ["Math", "print"]);
}

/// After `Math.` the child answers off the table; the proxy hangs the
/// declaration on each member.
#[test]
fn a_member_after_the_dot_carries_its_docs() {
    let src = "namespace Math as\n    const PI = 3.14\nend\n\nprint(Math.)\n";
    let (st, uri) = one_file(src);
    let mut result = json!([{ "label": "PI", "kind": 6 }]);
    st.mark_namespaces(uri, 4, 11, &mut result);
    let pi = &result.as_array().unwrap()[0];
    assert!(
        pi["documentation"]["value"]
            .as_str()
            .unwrap_or("")
            .contains("Math.PI"),
        "{pi}"
    );
}

/// `local p: Math.|` lists the types of the namespace and nothing
/// else. A private one stays out.
#[test]
fn a_type_slot_after_the_dot_lists_the_types() {
    let src = concat!(
        "namespace Math as\n",
        "    const PI = 3.14\n",
        "    struct Vec2 as\n",
        "        x: number\n",
        "    end\n",
        "    private struct Hidden as\n",
        "        y: number\n",
        "    end\n",
        "    type Scale = number\n",
        "end\n",
        "\n",
        "local p: Math.\n",
    );
    let (st, uri) = one_file(src);
    let offset = src.find("Math.\n").unwrap() + "Math.".len();
    let ctx = context::detect(&st.docs.get(uri).unwrap().source, offset).expect("a type slot");
    let items = st.context_items(uri, offset, &ctx);
    let mut got: Vec<String> = items
        .iter()
        .map(|i| i["label"].as_str().unwrap_or("").to_string())
        .collect();
    got.sort();
    assert_eq!(got, ["Scale", "Vec2"]);
    assert_eq!(items[0]["detail"], json!("struct Math.Vec2"));
}

/// A type of a namespace reads by its path everywhere the analyzer
/// prints the name the emit gave it.
#[test]
fn the_fold_reads_a_namespaced_type_by_its_path() {
    let src = "namespace Math as\n    struct Vec2 as\n        x: number\n    end\nend\n";
    let known = crate::shapes::Known {
        namespaces: alloy::declarations::namespace_names(src),
        ..Default::default()
    };
    assert_eq!(
        crate::shapes::fold("local v: Math_Vec2", &known),
        "local v: Math.Vec2"
    );
    // A whole word only: a name that begins with one keeps its own.
    assert_eq!(
        crate::shapes::fold("local v: Math_Vec2Extra", &known),
        "local v: Math_Vec2Extra"
    );
}

/// A namespace hovers the same at its declaration and where an import
/// binds it: the header with the members it carries.
#[test]
fn an_exported_namespace_hovers_at_the_import() {
    let st = files(&[
        (
            "file:///geom.aly",
            "export namespace Geom as\n    const ORIGIN = 0\n    struct Point as\n        x: number\n    end\nend\n",
        ),
        (
            "file:///main.aly",
            "import { Geom } from \"./geom\"\n\nprint(Geom.ORIGIN)\n",
        ),
    ]);
    let hover = st
        .docs
        .values()
        .flat_map(|d| d.decls.iter())
        .find(|d| d.name == "Geom")
        .expect("the namespace declaration");
    assert!(
        hover
            .hover
            .starts_with("```alloy\nexport namespace Geom as\nend\n```"),
        "{}",
        hover.hover
    );
    assert!(
        hover.hover.ends_with("Members: `ORIGIN`, `Point`."),
        "{}",
        hover.hover
    );

    // The member reads under its path too.
    let point = st
        .docs
        .values()
        .flat_map(|d| d.decls.iter())
        .find(|d| d.name == "Geom.Point")
        .expect("the member declaration");
    assert!(
        point.hover.contains("struct Geom.Point as"),
        "{}",
        point.hover
    );
}
