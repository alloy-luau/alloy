//! `[std] globals`: what a file writes with no import, and the std
//! import that brings the rest. See the config-std-imports RFC.

use alloy::std_names::Globals;

fn compile(src: &str, globals: Globals) -> alloy::Output {
    let options = alloy::EmitOptions {
        std_globals: globals,
        ..alloy::EmitOptions::default()
    };

    alloy::compile_with(src, &options).unwrap()
}

fn messages(src: &str, globals: Globals) -> Vec<String> {
    compile(src, globals)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn a_std_name_needs_its_import_once_per_name() {
    let src = "local a = new HashMap<<string, number>>()\nlocal b = new HashMap<<string, number>>()\nlocal i: Iter<number>? = nil\nprint(a, b, i)\n";

    assert_eq!(
        messages(src, Globals::None),
        [
            "`HashMap` is in the std; write `import { HashMap } from \"@alloy/std/collections\"`",
            "`Iter` is in the std; write `import { Iter } from \"@alloy/std/iter\"`",
        ]
    );
    assert!(messages(src, Globals::All).is_empty());
    assert_eq!(
        messages(src, Globals::List(vec!["HashMap".into()])),
        ["`Iter` is in the std; write `import { Iter } from \"@alloy/std/iter\"`"]
    );
}

/// The import writes nothing: the name renders as `__alloy.Name` the
/// way an ambient one does, so the two modes emit the same program.
#[test]
fn an_import_emits_what_the_ambient_name_emits() {
    let body = "local m = new HashMap<<string, number>>()\nprint(m)\n";
    let imported = compile(
        &format!("import {{ HashMap }} from \"@alloy/std/collections\"\n{body}"),
        Globals::None,
    );
    let ambient = compile(&format!("\n{body}"), Globals::All);

    // The runtime require takes the import's line; the program is the same.
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        imported.diagnostics.is_empty(),
        "{:?}",
        imported.diagnostics
    );
    assert_eq!(words(&imported.ship), words(&ambient.ship));
    assert!(imported.imports.is_empty(), "the std is no module on disk");

    // The facade holds every name.
    let facade = compile(
        &format!("import {{ HashMap }} from \"@alloy/std\"\n{body}"),
        Globals::None,
    );
    assert!(facade.diagnostics.is_empty(), "{:?}", facade.diagnostics);
}

#[test]
fn the_language_owns_its_names() {
    let src = "local function f(): Result<number, string>\n    return Ok(1)\nend\nlocal xs: Array<number> = [1]\n@derive(Eq, Clone, Debug)\nstruct P as\n    x: number\nend\nlocal function g<T: Ord>(a: T, b: T) return a < b end\nprint(f(), xs, g(1, 2))\n";

    assert!(messages(src, Globals::None).is_empty());
}

#[test]
fn a_std_import_names_what_the_module_holds() {
    let src = "import { Iter } from \"@alloy/std/collections\"\nimport std from \"@alloy/std\"\nimport { X } from \"@alloy/std/nope\"\nprint(Iter, std, X)\n";
    let got = messages(src, Globals::None);

    assert_eq!(
        got[0],
        "\"@alloy/std/collections\" has no `Iter`; it is in \"@alloy/std/iter\""
    );
    assert!(
        got[1].starts_with("the std has no default export"),
        "{got:?}"
    );
    assert!(
        got[2].starts_with("\"@alloy/std/nope\" is no std module"),
        "{got:?}"
    );
}

/// An alias writes a local and a type under it; a star import binds the
/// runtime, so the path reaches the value and the type.
#[test]
fn an_alias_and_a_star_import_bind_the_runtime() {
    let out = compile(
        "import { HashMap as Map } from \"@alloy/std/collections\"\nimport * as col from \"@alloy/std/collections\"\nlocal a: Map<string, number> = Map.new()\nlocal s = col.Set.new()\nprint(a, s)\n",
        Globals::None,
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship
            .contains("local Map = __alloy.HashMap type Map<K, V> = __alloy.HashMap<K, V>"),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains("local col = require(\"@alloy\")"),
        "{}",
        out.ship
    );
}

/// `Serialize` writes the table and `Deserialize` reads it back; each
/// is a std name from `@alloy/std/serde`, a bound included.
#[test]
fn serde_splits_and_imports() {
    let src = "@derive(Serialize)\nstruct Out as\n    x: number\nend\n@derive(Deserialize)\nstruct In as\n    x: number\nend\nlocal function save<T: Serialize>(v: T) return v:serialize() end\nprint(save(new Out { x = 1 }), In.from_table({ x = 1 }))\n";
    let out = compile(src, Globals::All);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.ship.contains("function Out.to_table("), "{}", out.ship);
    assert!(
        !out.ship.contains("function Out.from_table("),
        "{}",
        out.ship
    );
    assert!(out.ship.contains("function In.from_table("), "{}", out.ship);
    assert!(!out.ship.contains("function In.to_table("), "{}", out.ship);

    assert_eq!(
        messages(src, Globals::None),
        [
            "`Serialize` is in the std; write `import { Serialize } from \"@alloy/std/serde\"`",
            "`Deserialize` is in the std; write `import { Deserialize } from \"@alloy/std/serde\"`",
        ]
    );
    let imported = format!("import {{ Serialize, Deserialize }} from \"@alloy/std/serde\"\n{src}");
    assert!(messages(&imported, Globals::None).is_empty());
}

/// serde's options live in `@alloy/std/serde` beside the derives. A bare
/// one needs its import; a star import reaches each as `@serde.name`,
/// and a derive as `serde.Serialize`.
#[test]
fn a_serde_option_needs_its_import_or_a_star_path() {
    let bare = "import { Serialize } from \"@alloy/std/serde\"\n@derive(Serialize)\n@rename_all(\"camelCase\")\nstruct S\n    @skip\n    x: number = 1\nend\nprint(S)\n";

    assert_eq!(
        messages(bare, Globals::None),
        [
            "`rename_all` is in the std; write `import { rename_all } from \"@alloy/std/serde\"`",
            "`skip` is in the std; write `import { skip } from \"@alloy/std/serde\"`",
        ]
    );
    assert!(messages(bare, Globals::All).is_empty());

    let star = "import * as serde from \"@alloy/std/serde\"\n@derive(serde.Serialize, serde.Deserialize)\n@serde.rename_all(\"camelCase\")\n@serde.deny_unknown_fields\nstruct S\n    @serde.rename(\"y\")\n    long_name: number\n    @serde.skip\n    x: number = 1\nend\nprint(S.from_table({ y = 1 }):to_table())\n";
    let out = compile(star, Globals::None);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains("return { y = self.long_name }"),
        "{}",
        out.ship
    );
    assert!(out.ship.contains("for k in pairs(t) do"), "{}", out.ship);

    let named = "import { Deserialize, deny_unknown_fields } from \"@alloy/std/serde\"\n@derive(Deserialize)\n@deny_unknown_fields\nstruct S\n    x: number\nend\nprint(S)\n";
    assert!(messages(named, Globals::None).is_empty());
}
