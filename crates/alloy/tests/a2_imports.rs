//! `alloy check` reports what an import got wrong: a module that names
//! no file, a name the module does not export, and a name imported
//! twice. Agent A2's cases; the file name keeps them apart from the
//! golden suite.

use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-a2-{name}-{}", std::process::id()));
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

/// An `export { ... }` list names the types of the declarations it
/// holds, so an import of one binds the type as `export struct` does.
#[test]
fn an_export_list_names_the_types_it_holds() {
    let source = "struct Named as\n    n: number\nend\n\nenum Kind<T> as A, B end\n\ninterface Shape as\n    area: number\nend\n\ntype Id = number\n\nfunction make(): number\n    return 1\nend\n\nexport { Named, Kind, Shape, Id, make, Named as Other }\n";
    assert_eq!(
        alloy::modules::exported_types(source),
        vec!["Named", "Other", "Kind<T>", "Shape=", "Id="]
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

/// A `.luau` module with no `return` gives the import nothing, and an
/// `export type` adds no value there. `alloy check` reports it the way
/// the Luau checker does, so the two runs agree.
#[test]
fn a_luau_module_with_no_return_is_an_error() {
    let dir = scratch("noreturn");
    std::fs::write(
        dir.join("mod2.luau"),
        "export type Config = { name: string }\n",
    )
    .unwrap();
    let source = "import { Config } from \"./mod2\"\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert_eq!(problems.len(), 1, "{messages:?}");
    assert_eq!(problems[0].kind, "UnknownModule");
    assert_eq!(
        problems[0].message,
        "\"./mod2\" returns nothing to import; add a `return`"
    );

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

/// An import resolves where it stands. One inside a function binds its
/// names for that scope, a std import included, and one under code
/// requires after the code above it runs, as Luau reads the lines.
#[test]
fn an_import_resolves_where_it_stands() {
    let options = alloy::EmitOptions {
        file_name: "t.aly".to_string(),
        std_globals: alloy::std_names::Globals::None,
        ..alloy::EmitOptions::default()
    };
    let src = "local function f()\n    import { x } from \"./a\"\n    import { Signal } from \"@alloy/std/signal\"\n    return x, Signal.new()\nend\n\nprint(\"hello\")\nimport { y } from \"./a\"\nprint(f, y)\n";
    let out = alloy::compile_with(src, &options).unwrap();

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.lints.iter().all(|l| l.name == "print_debug"),
        "{:?}",
        out.lints
    );

    let lines: Vec<&str> = out.ship.lines().collect();
    assert!(
        lines[1]
            .trim_start()
            .starts_with("local _m1 = require(\"./a\")"),
        "{}",
        out.ship
    );
    assert!(
        lines[7].starts_with("local _m2 = require(\"./a\")"),
        "{}",
        out.ship
    );
}

/// The markup lowering prepends its helpers in front of the file, and
/// the lints read the lowered text. A spread attribute pulls one in, and
/// `import_order` counted it as code above the imports.
#[test]
fn a_markup_helper_is_no_code_above_an_import() {
    let dir = scratch("markup-helper");
    std::fs::write(
        dir.join("card.alx"),
        "export function Card(props: { x: number }): any
    return <TextLabel Text={tostring(props.x)} />
end
",
    )
    .unwrap();
    let path = dir.join("page.alx");
    let src = "import * as React from \"@packages/react\" --@alloy-ignore\nimport * as UI from \"./card\"\n\nlocal function Spread(props: { x: number })\n    return <TextButton {props} />\nend\n\nreturn Spread, UI\n";
    std::fs::write(&path, src).unwrap();
    let options = alloy::EmitOptions {
        file_name: "page.alx".to_string(),
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_file(
        path.to_str().unwrap(),
        src,
        &options,
        Some(&luaux::Config::default()),
        None,
    )
    .unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        !out.lints.iter().any(|l| l.name == "import_order"),
        "{:?}",
        out.lints
    );

    // The helpers themselves drew lints, `explicit_any` and
    // `manual_type_test` among them, and the rewrite landed on line 1 of
    // the author's source.
    let src = "-- A card page.\nimport * as UI from \"./card\"\nimport * as React from \"@packages/react\" --@alloy-ignore\n\nlocal function Spread(props: { x: number })\n    return <TextButton {props} />\nend\n\nreturn Spread, UI\n";
    std::fs::write(&path, src).unwrap();
    let out = alloy::compile_file(
        path.to_str().unwrap(),
        src,
        &options,
        Some(&luaux::Config::default()),
        None,
    )
    .unwrap();
    let first_line: Vec<&alloy::lint::Lint> = out
        .lints
        .iter()
        .filter(|l| src[..l.start as usize].matches('\n').count() == 0)
        .collect();
    assert!(first_line.is_empty(), "{first_line:?}");
    assert!(out.lints.iter().all(|l| l.fix.is_none()), "{:?}", out.lints);
}

/// A module that ends in `return <expr>` and exports nothing has no
/// export table. The returned value is the module: a bare name binds
/// it, `* as` binds it, and a name in braces reads one key of it.
#[test]
fn a_returning_module_is_its_own_default() {
    let dir = scratch("returning");
    std::fs::write(
        dir.join("palette.aly"),
        "local Palette = { dark = \"#111111\" }\n\nfunction Palette.tint(hex: string): string\n    return hex\nend\n\nreturn Palette\n",
    )
    .unwrap();
    let source = "import Palette from \"./palette\"\nimport * as All from \"./palette\"\nimport { tint, dark } from \"./palette\"\nprint(Palette, All, tint, dark)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
    assert!(problems.is_empty(), "{messages:?}");
    // The module reads the way a plain Luau module does, so the emit
    // binds the value itself and reads no `default` field.
    assert_eq!(
        alloy::modules::plain_modules(source, &main, &[]),
        vec!["./palette".to_string()]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A markup module returns its component the same way. The parser has
/// no reading for a tag, so the markup blanks before the read; without
/// it the trailing `return` was lost and the import reported.
#[test]
fn a_markup_module_returns_its_component() {
    let dir = scratch("returning_markup");
    std::fs::write(
        dir.join("panel.alx"),
        "import * as React from \"@packages/react\" --@alloy-ignore\n\nlocal function Panel()\n    return (\n        <Frame>\n            <TextLabel Text=\"hi\" />\n        </Frame>\n    )\nend\n\nreturn Panel\n",
    )
    .unwrap();
    let source = "import Panel from \"./panel\"\nprint(Panel)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
    assert!(problems.is_empty(), "{messages:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The keys of a returned table the compiler can read: a literal, a
/// local the file fills in by name, and a struct the file constructs.
#[test]
fn a_named_import_reads_the_returned_keys() {
    let dir = scratch("returned-keys");
    std::fs::write(dir.join("flat.aly"), "return { a = 1, b = 2 }\n").unwrap();
    std::fs::write(
        dir.join("shaped.aly"),
        "struct Config as\n    speed: number\n    label: string\nend\n\nreturn new Config { speed = 1, label = \"a\" }\n",
    )
    .unwrap();

    assert_eq!(
        alloy::modules::returned_keys(&std::fs::read_to_string(dir.join("flat.aly")).unwrap()),
        Some(vec!["a".to_string(), "b".to_string()])
    );
    assert_eq!(
        alloy::modules::returned_keys(&std::fs::read_to_string(dir.join("shaped.aly")).unwrap()),
        Some(vec!["speed".to_string(), "label".to_string()])
    );

    let source = "import { a, c } from \"./flat\"\nimport { speed, missing } from \"./shaped\"\nprint(a, c, speed, missing)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert_eq!(problems.len(), 2, "{messages:?}");
    assert_eq!(
        problems[0].message,
        "the module \"./flat\" returns a table with no `c`; it has `a` and `b`"
    );
    assert_eq!(
        problems[1].message,
        "the module \"./shaped\" returns a table with no `missing`; it has `speed` and `label`"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A module cannot name its value twice. `return` says one value and
/// `export` says another, and no reader can tell which an import binds.
#[test]
fn a_module_returns_or_exports_but_not_both() {
    let dir = scratch("returns-and-exports");
    std::fs::write(
        dir.join("mixed.aly"),
        "export function one() end\n\nlocal M = { a = 1 }\n\nreturn M\n",
    )
    .unwrap();
    // A module of types alone has no value to export, so its own
    // `return` stands.
    std::fs::write(
        dir.join("typed.aly"),
        "export type Id = number\n\nreturn { make = 1 }\n",
    )
    .unwrap();
    let source = "import M from \"./mixed\"\nimport T from \"./typed\"\nprint(M, T)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert_eq!(problems.len(), 1, "{messages:?}");
    assert_eq!(
        problems[0].message,
        "`./mixed` returns a value and exports names; use one"
    );
    // The report sits on the path, which is what has to change.
    assert_eq!(
        &source[problems[0].start as usize..problems[0].end as usize],
        "\"./mixed\""
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/*
An attribute in an import list is written `@name`, the way it is written
where it is applied. The list reports either spelling that disagrees
with the declaration, and an attribute is not a type.
*/
#[test]
fn an_attribute_is_imported_with_its_sigil() {
    let dir = scratch("attribute-sigil");
    std::fs::write(
        dir.join("lib.aly"),
        "export attribute tagged(name: string) on struct\n\nexport const version = 1\n",
    )
    .unwrap();
    let main = dir.join("main.aly");
    let report = |source: &str| -> Vec<String> {
        std::fs::write(&main, source).unwrap();

        alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[])
            .into_iter()
            .map(|p| p.message)
            .collect()
    };

    assert_eq!(
        report("import { @tagged } from \"./lib\"\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        report("import { @tagged as t } from \"./lib\"\n"),
        Vec::<String>::new()
    );
    // The bare name imports the attribute too.
    assert_eq!(
        report("import { tagged } from \"./lib\"\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        report("import { @version } from \"./lib\"\n"),
        vec!["`version` is not an attribute; import it as `version`"]
    );
    assert_eq!(
        report("import type { tagged } from \"./lib\"\n"),
        vec!["`tagged` is an attribute, not a type; import it as `@tagged` in a value list"]
    );
    // A list mixes the two, and each name answers for itself.
    assert_eq!(
        report("import { @tagged, version } from \"./lib\"\n"),
        Vec::<String>::new()
    );

    // The report on a wrong `@` covers the sigil, which is the part to
    // delete.
    let source = "import { @version } from \"./lib\"\n";
    std::fs::write(&main, source).unwrap();
    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    assert_eq!(
        &source[problems[0].start as usize..problems[0].end as usize],
        "@version"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `import * as M, { a }` binds the whole module and names from it, the
/// way `import M, { a }` binds the default and names from it.
#[test]
fn a_namespace_import_takes_a_name_list_too() {
    let dir = scratch("namespace-list");
    std::fs::write(
        dir.join("lib.aly"),
        "export const version = 1\n\nexport function helper(): number\n    return 1\nend\n",
    )
    .unwrap();
    let source = "import * as Lib, { version } from \"./lib\"\nprint(Lib, version)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
    assert!(problems.is_empty(), "{messages:?}");

    // The alias and a name in braces are both bindings of the file, so
    // one written twice reports.
    let source = "import * as version, { version } from \"./lib\"\nprint(version)\n";
    std::fs::write(&main, source).unwrap();
    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    assert_eq!(problems.len(), 1);
    assert_eq!(
        problems[0].message,
        "`version` is already imported in this file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `export type` and `export interface` have no value at run time. A
/// bare import of one binds the type alone; a `local` would read a
/// key the module's table lacks, and the checker would report it.
#[test]
fn a_bare_import_of_a_type_only_export_binds_no_value() {
    let dir = scratch("type-only");
    std::fs::write(
        dir.join("types.aly"),
        "export type Size = { w: number }\nexport interface Shape as\n    w: number\nend\nexport struct Box as\n    w: number\nend\n",
    )
    .unwrap();
    let source = "import { Size, Shape, Box } from \"./types\"\n";
    let options = alloy::EmitOptions {
        import_types: alloy::modules::import_types(source, &dir.join("main.aly"), &[]),
        ..Default::default()
    };
    let out = alloy::compile_with(source, &options).unwrap();
    let line = out.check.lines().next().unwrap();

    assert!(line.contains("local Box = _m1.Box"), "{line}");
    assert!(line.contains("type Size = _m1.Size"), "{line}");
    assert!(line.contains("type Shape = _m1.Shape"), "{line}");
    assert!(
        !line.contains("Size, ") && !line.contains("Shape, "),
        "{line}"
    );
    assert!(
        !line.contains("local Size") && !line.contains("local Shape"),
        "{line}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/*
A name a module exports as a type alone has no value at run time, so a
bare import of it binds the type and the returned table needs no key.
The check follows the emit, for a plain Luau module and for an Alloy one.
*/
#[test]
fn a_bare_import_of_a_type_only_export_needs_no_key() {
    let dir = scratch("type-only-key");
    std::fs::write(
        dir.join("luaumod.luau"),
        "export type P = number\n\nreturn {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("types.aly"), "export type Size = number\n").unwrap();
    let source = "import { P } from \"./luaumod\"\nimport { Size } from \"./types\"\nlocal x: P = 5\nlocal y: Size = 6\nprint(x, y)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert!(problems.is_empty(), "{messages:?}");

    // A name the module neither returns nor exports as a type is still
    // a missing key.
    let bad = "import { Q } from \"./luaumod\"\nprint(Q)\n";
    std::fs::write(&main, bad).unwrap();

    let problems = alloy::modules::import_problems(bad, Path::new("main.aly"), &main, &[]);

    assert_eq!(problems.len(), 1);
    assert_eq!(
        problems[0].message,
        "the module \"./luaumod\" returns a table with no `Q`; it has nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An `impl` of an imported struct reaches the struct's private
/// members: the declaring file's check artifact exports the full view,
/// and the import aliases it. A call from outside any impl still
/// reports, because the project index names the private method.
#[test]
fn an_impl_of_an_imported_struct_reaches_its_private_members() {
    let dir = scratch("private-view");
    let cat = "export struct Cat as\n    name: string\nend\n\nimpl Cat as\n    private function helper(self): string\n        return \"x\"\n    end\nend\n";
    std::fs::write(dir.join("cat.aly"), cat).unwrap();
    let source = "import { Cat } from \"./cat\"\n\nimpl Cat as\n    public function greet(self): string\n        return self:helper()\n    end\nend\n";
    let main = dir.join("other.aly");
    std::fs::write(&main, source).unwrap();

    let declaring = alloy::compile_with(cat, &alloy::EmitOptions::default()).unwrap();

    assert!(
        declaring.check.contains("export type Cat__all ="),
        "{}",
        declaring.check
    );

    let options = alloy::EmitOptions::default().imports(source, &main, &[]);
    let out = alloy::compile_with(source, &options).unwrap();

    assert!(
        out.check.contains("type Cat__all = _m1.Cat__all"),
        "{}",
        out.check
    );
    assert!(
        out.check.contains("local self = (self :: any) :: Cat__all"),
        "{}",
        out.check
    );
    // The ship artifact has one view: the private method sits on the
    // class table there.
    assert!(!out.ship.contains("Cat__all"), "{}", out.ship);

    let project = alloy::extensions::project_impls(&[cat.to_string()]);

    assert_eq!(
        project.privates,
        vec![("Cat".to_string(), vec!["helper".to_string()])]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/*
An `export macro` travels as source. `import { name }` brings the
definition in, `$name(...)` expands it here, and the module's table
carries no key for the name, so the emit binds no local.
*/
/// An exported macro may call a private macro of its own module. The
/// body expands in the importing file, so the private one has to travel
/// with it; the importing file still cannot call it by name.
#[test]
fn an_imported_macro_calls_a_private_macro_of_its_module() {
    let dir = scratch("macro-private");
    std::fs::write(
        dir.join("m.aly"),
        "macro helper(x) print(\"helper:\", x) end\nexport macro wrapper(x) $helper(x) end\n",
    )
    .unwrap();
    let source = "import { wrapper } from \"./m\"\n$wrapper(\"x\")\n";
    let main = dir.join("m_use.aly");
    std::fs::write(&main, source).unwrap();

    let macros = alloy::modules::import_macros(source, &main, &[]);
    let options = alloy::EmitOptions {
        macros: macros.clone(),
        ..Default::default()
    };
    let out = alloy::compile_with(source, &options).unwrap();
    let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

    assert!(out.diagnostics.is_empty(), "{messages:?}");
    // The body keeps the spacing the declaration wrote.
    assert!(
        out.ship.contains("print(\"helper:\", \"x\")"),
        "{}",
        out.ship
    );

    // `$helper` is the module's own; a call here is unknown.
    let direct = alloy::compile_with(
        "import { wrapper } from \"./m\"\n$helper(\"x\")\n",
        &options,
    )
    .unwrap();
    let messages: Vec<&str> = direct
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();

    assert!(
        messages.iter().any(|m| m.contains("unknown macro")),
        "{messages:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_imported_macro_expands_where_it_is_called() {
    let dir = scratch("macro-import");
    std::fs::write(
        dir.join("mac.aly"),
        "export macro logit(x) print(tostring(x)) end\nexport macro sum(a, b = 2) return a + b end\nmacro local_only(x) print(x) end\n",
    )
    .unwrap();
    let source = "import { logit, sum as add } from \"./mac\"\n$logit(\"hi\")\nlocal n = $add(1)\nprint(n)\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert!(problems.is_empty(), "{messages:?}");

    let macros = alloy::modules::import_macros(source, &main, &[]);
    let names: Vec<&str> = macros.iter().map(|m| m.name.as_str()).collect();

    // The alias renames the macro. A macro the module keeps to itself
    // travels hidden: an expansion can call it, this file cannot.
    assert_eq!(names, vec!["logit", "add", "local_only"]);
    let hidden: Vec<&str> = macros
        .iter()
        .filter(|m| m.hidden)
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(hidden, vec!["local_only"]);

    let options = alloy::EmitOptions {
        macros,
        ..Default::default()
    };
    let out = alloy::compile_with(source, &options).unwrap();
    let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

    assert!(out.diagnostics.is_empty(), "{messages:?}");
    assert!(!out.ship.contains('$'), "{}", out.ship);
    assert!(!out.ship.contains("local logit"), "{}", out.ship);
    assert!(out.ship.contains("print(tostring(\"hi\"))"), "{}", out.ship);
    // The default of the second parameter fills the argument that is
    // not given.
    assert!(out.ship.contains("1 + 2"), "{}", out.ship);

    let _ = std::fs::remove_dir_all(&dir);
}

/// `import "./fx"` is Luau's call sugar for `import("./fx")`, so it
/// lowers to the same `require` and runs the module for its effects. It
/// stayed a call of a global named `import`, which the checker reported.
#[test]
fn an_import_of_a_bare_string_is_a_require() {
    let out = alloy::compile("import \"./fx\"\nlocal m = import './fx'\nprint(m)\n").unwrap();

    for text in [&out.check, &out.ship] {
        assert!(text.starts_with("require(\"./fx\")\n"), "{text}");
        assert!(text.contains("local m = require('./fx')"), "{text}");
        assert!(!text.contains("import"), "{text}");
    }
}
