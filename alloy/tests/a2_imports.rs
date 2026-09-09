//! `alloy check` reports what an import got wrong: a module that names
//! no file, a name the module does not export, and a name imported
//! twice. Agent A2's cases; the file name keeps them apart from the
//! golden suite.

use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-a2-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    dir
}

#[test]
fn a_module_lists_what_it_exports() {
    let source = "export struct Point as\n    x: number\nend\n\nexport enum Kind as A, B end\n\nexport function make(): number\n    return 1\nend\n\nexport type Alias = number\n\nlocal hidden = 2\nexport { hidden }\n\nlocal private_one = 3\n";
    assert_eq!(
        alloy::modules::exported_names(source),
        vec!["Point", "Kind", "make", "Alias", "hidden"]
    );
}

#[test]
fn an_import_names_the_module_and_what_it_exports() {
    let dir = scratch("imports");
    std::fs::write(
        dir.join("lib.aly"),
        "export function used_one(n: number): number\n    return n\nend\n",
    )
    .unwrap();
    let source = "import { missing_name } from \"./lib\"\nimport { used_one } from \"./nowhere\"\nimport { used_one } from \"./lib\"\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert_eq!(problems.len(), 3, "{messages:?}");
    assert_eq!(problems[0].kind, "ImportError");
    assert_eq!(
        problems[0].message,
        "\"./lib\" does not export `missing_name`; it exports `used_one`"
    );
    // The range covers the name, not the whole statement.
    assert_eq!(
        &source[problems[0].start as usize..problems[0].end as usize],
        "missing_name"
    );
    assert_eq!(problems[1].kind, "UnknownModule");
    assert!(
        problems[1]
            .message
            .starts_with("\"./nowhere\" names no module")
    );
    assert_eq!(problems[2].kind, "ImportError");
    assert_eq!(
        problems[2].message,
        "`used_one` is already imported in this file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_plain_luau_module_is_not_checked_for_names() {
    let dir = scratch("plain");
    std::fs::write(dir.join("util.luau"), "return { f = function() end }\n").unwrap();
    let source = "import { f } from \"./util\"\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    assert!(alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_default_import_needs_a_default_export() {
    let dir = scratch("default");
    std::fs::write(
        dir.join("test.aly"),
        "export interface Test as\n    a: number\nend\n",
    )
    .unwrap();
    let source = "import Test2 from \"./test\"\nimport Test from \"./test\"\nprint(Test2, Test)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert_eq!(problems.len(), 2, "{messages:?}");
    assert_eq!(problems[0].kind, "ImportError");
    // Nothing is named `Test2`, so the one export the module has is the
    // name the author meant.
    assert_eq!(
        problems[0].message,
        "\"./test\" has no default export; write `import { Test } from \"./test\"` \
         or add `export default` to it"
    );
    // The range covers the binding, not the whole statement.
    assert_eq!(
        &source[problems[0].start as usize..problems[0].end as usize],
        "Test2"
    );
    // A named export of the same name is the one the author meant.
    assert_eq!(
        problems[1].message,
        "\"./test\" has no default export; write `import { Test } from \"./test\"` \
         or add `export default` to it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_default_export_answers_a_bare_import() {
    let dir = scratch("has-default");
    std::fs::write(
        dir.join("test.aly"),
        "export interface Test as\n    a: number\nend\n\nexport default function make()\nend\n",
    )
    .unwrap();
    let source = "import Test2, { Test } from \"./test\"\nprint(Test2, Test)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    assert!(alloy::modules::exports_default(
        &std::fs::read_to_string(dir.join("test.aly")).unwrap()
    ));
    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert!(problems.is_empty(), "{messages:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_plain_luau_module_needs_no_default_export() {
    let dir = scratch("plain-default");
    std::fs::write(dir.join("util.luau"), "return { helper = 1 }\n").unwrap();
    let source =
        "import util from \"./util\"\nimport * as Util from \"./util\"\nprint(util, Util)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    assert!(alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]).is_empty());
    // The spec carries no extension, so the emit reads the resolved
    // file: a plain module's value is what a bare import binds.
    assert_eq!(
        alloy::modules::plain_modules(source, &main, &[]),
        vec!["./util".to_string()]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_with_several_exports_lists_them() {
    let dir = scratch("many");
    std::fs::write(
        dir.join("lib.aly"),
        "export function one() end\n\nexport function two() end\n",
    )
    .unwrap();
    let source = "import Lib from \"./lib\"\nprint(Lib)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);

    assert_eq!(problems.len(), 1);
    assert_eq!(
        problems[0].message,
        "\"./lib\" has no default export; it exports `one` and `two`, \
         or add `export default` to it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Compiles one file and gives its diagnostic messages back.
fn messages(src: &str) -> Vec<String> {
    let options = alloy::EmitOptions {
        file_name: "t.aly".to_string(),
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_with(src, &options).unwrap();

    out.diagnostics.iter().map(|d| d.message.clone()).collect()
}

/// The export table takes one entry per name. Two `export local x`
/// lines put `x` in it twice and the second won with nothing said.
#[test]
fn a_name_exported_twice_reports() {
    let hits = messages("export local x = 1\nexport local x = 2\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("`x` is exported twice; a module sends one binding per name"),
        "{hits:?}"
    );
}

/// `export { z }` of a name the module has not got. The list knows the
/// module's bindings, so the report is Alloy's, not Luau's.
#[test]
fn an_export_list_names_a_binding_of_the_module() {
    let hits = messages("export { z }\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("`z` is not a binding of this module"),
        "{hits:?}"
    );

    // A local, an import, and a declaration all count.
    assert!(
        messages("local a = 1\n\nstruct S as\n    x: number\nend\n\nexport { a, S }\n").is_empty()
    );
}

/// A module whose only export is `export type { T }` binds no value.
/// Luau needs a module to return one, so the alias takes the `export`
/// word and the module returns an empty table.
#[test]
fn a_types_only_export_list_returns_an_empty_table() {
    let options = alloy::EmitOptions {
        file_name: "t.aly".to_string(),
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_with("type T = number\nexport type { T }\n", &options).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.ship.contains("export type T = number"), "{}", out.ship);
    assert!(!out.ship.contains("export type T = T"), "{}", out.ship);
    assert!(out.ship.contains("return {}"), "{}", out.ship);
}
