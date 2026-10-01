//! `@derive(Default)`, the deep `Clone`, `@allow`, Luau's attribute
//! list, and serde's container and field options.

fn compile(src: &str) -> alloy::Output {
    alloy::compile(src).unwrap()
}

fn messages(src: &str) -> Vec<String> {
    compile(src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn default_starts_each_field_at_its_zero() {
    let out = compile(
        "@derive(Default)\nstruct Stats as\n    hp: number\n    name: string\n    alive: boolean\n    level: number = 1\n    tags: string[]\n    friend: Stats?\n    at: Vector3\nend\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains(
            "function Stats.default() return Stats({ hp = 0, name = \"\", alive = false, tags = __alloy.Array.from({}), at = Vector3.zero }) end"
        ),
        "{}",
        out.ship
    );

    // A type with no zero asks for a default.
    assert_eq!(
        messages("@derive(Default)\nstruct P as\n    part: Part\nend\n"),
        [
            "`@derive(Default)` needs a starting value for `part: Part`; write one, `part: Part = ...`, or derive Default on the type"
        ]
    );
}

#[test]
fn clone_clones_what_derives_clone_and_copies_arrays() {
    let out = compile(
        "@derive(Clone)\nstruct Inner as\n    v: number\nend\n@derive(Clone)\nstruct Outer as\n    inner: Inner\n    list: Inner[]\n    names: string[]\n    maybe: Inner?\n    part: Instance\nend\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains("v.inner = Inner.clone(v.inner)"),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("v.list = __alloy.Array.map(v.list, Inner.clone)"),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("v.names = __alloy.Array.from(table.clone(v.names))"),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("if v.maybe ~= nil then v.maybe = Inner.clone(v.maybe) end"),
        "{}",
        out.ship
    );
    assert!(
        !out.ship.contains("v.part ="),
        "an Instance is shared: {}",
        out.ship
    );
}

#[test]
fn allow_quiets_a_lint_over_what_it_sits_on() {
    let wide = "(a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number): number\n    return a + b + c + d + e + f + g + h\nend\n";
    let lints =
        |src: &str| -> Vec<&'static str> { compile(src).lints.iter().map(|l| l.name).collect() };

    assert!(lints(&format!("local function f{wide}")).contains(&"too_many_arguments"));
    assert!(
        !lints(&format!(
            "@allow(too_many_arguments)\nlocal function f{wide}"
        ))
        .contains(&"too_many_arguments")
    );
    assert!(
        !lints(&format!(
            "@allow(flux.too_many_arguments)\nlocal function f{wide}"
        ))
        .contains(&"too_many_arguments")
    );
    // A rustc or Clippy name reads as the Alloy lint it means.
    assert!(
        !lints(&format!(
            "@allow(clippy.too_many_arguments)\nlocal function f{wide}"
        ))
        .contains(&"too_many_arguments")
    );
    assert!(messages("@allow(unused_variables, dead_code)\nlocal function f() end\n").is_empty());
    // A group spreads into its lints.
    assert!(
        !lints(&format!("@allow(complexity)\nlocal function f{wide}"))
            .contains(&"too_many_arguments")
    );

    // A name no lint has, and an error, report.
    let got = messages("@allow(too_many_argumnts)\nlocal function f() end\n");
    assert!(
        got[0].contains("did you mean `too_many_arguments`"),
        "{got:?}"
    );
    let got = messages("@allow(luau.TypeError)\nlocal function f() end\n");
    assert!(got[0].starts_with("`TypeError` is an error"), "{got:?}");
    // A luau-lsp name is checked against luau-lsp's own lints.
    let got = messages("@allow(luau.LocalShadw)\nlocal function f() end\n");
    assert!(got[0].contains("did you mean `LocalShadow`"), "{got:?}");
    assert!(messages("@allow(luau.LocalShadow)\nlocal function f() end\n").is_empty());

    // The emit writes nothing for it.
    assert!(
        !compile("@allow(pedantic)\nlocal function f() end\n")
            .ship
            .contains("allow")
    );
}

/// An attribute declaration is an item, so `@allow` goes on it. The
/// parser read `@allow` and then asked for a function.
#[test]
fn allow_quiets_a_lint_over_an_attribute_declaration() {
    let lints =
        |src: &str| -> Vec<&'static str> { compile(src).lints.iter().map(|l| l.name).collect() };
    let decl = "export attribute Tagged on struct\n";

    assert!(lints(decl).contains(&"naming_convention"));

    let allowed = format!("@allow(naming_convention)\n{decl}");
    let out = compile(&allowed);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(!lints(&allowed).contains(&"naming_convention"));
    assert!(
        out.ship
            .contains("local Tagged = __alloy.attribute(\"Tagged\""),
        "{}",
        out.ship
    );
    assert_eq!(out.ship.lines().count(), allowed.lines().count());

    // The name list still checks, and an attribute that takes no
    // attribute declaration reports.
    let got = messages(&format!("@allow(naming_conventoin)\n{decl}"));
    assert!(
        got[0].contains("did you mean `naming_convention`"),
        "{got:?}"
    );
    assert_eq!(
        messages(&format!("@sealed\n{decl}")),
        ["the attribute `sealed` has no meaning on an attribute; it goes on `struct` and `enum`"]
    );
}

/// `@allow` writes nothing, so each statement under it lowers as it
/// does with a blank line in its place. A plain `local` or `const`
/// under it went to the Luau as written, with `a ?? b`, `new T()` and
/// `-> T` in it (LANG_BUGS 133).
#[test]
fn allow_keeps_the_lowering_of_each_statement() {
    let kinds = [
        "local c = a ?? b",
        "const c = a ?? b",
        "local c = new Vector3(1, 2, 3)",
        "local c = function(x: number) -> number\n    return x ?? 1\nend",
        "local c, d = a ?? b, a ?? b",
        "local c: number = a ?? b",
        "local { x } = { x = a ?? b }",
        "local c = match a with\n    case 1 then\n        print(1)\n        b ?? 3\n    default b ?? 1\nend",
        "const c = match a with\n    case 1 then\n        print(1)\n        b ?? 3\n    default b ?? 1\nend",
        "export local c = a ?? b",
        "export const c = a ?? b",
        "local function c(x: number) -> number\n    return x ?? 1\nend",
        "function c(x: number) -> number\n    return x ?? 1\nend",
        "export function c(x: number) -> number\n    return x ?? 1\nend",
        "print(a ?? b)",
        "a = a ?? b",
        "if (a ?? b) > 1 then\n    print(a ?? b)\nend",
        "for i = 1, a ?? b do\n    print(i)\nend",
        "do\n    print(a ?? b)\nend",
        "type T = { x: number }",
        "struct S as\n    x: number = a ?? b\nend",
        "enum E as\n    A\n    B\nend",
        "namespace N as\n    const x = a ?? b\nend",
    ];
    let head = "local a: number? = nil\nlocal b = 2\n";
    // A temp of a destructure goes in front of the statement, on the
    // line of the attribute, so the words compare and the lines do not.
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");

    for kind in kinds {
        let plain = compile(&format!("{head}\n{kind}\n"));
        let allowed = compile(&format!("{head}@allow(missing_doc)\n{kind}\n"));

        assert!(
            plain.diagnostics.is_empty(),
            "{kind}\n{:?}",
            plain.diagnostics
        );
        assert!(
            allowed.diagnostics.is_empty(),
            "{kind}\n{:?}",
            allowed.diagnostics
        );
        assert_eq!(words(&allowed.ship), words(&plain.ship), "{kind}");
        assert_eq!(words(&allowed.check), words(&plain.check), "{kind}");
        assert_eq!(
            allowed.ship.lines().count(),
            plain.ship.lines().count(),
            "{kind}"
        );
        assert!(!allowed.ship.contains("??"), "{kind}\n{}", allowed.ship);
    }
}

#[test]
fn luau_attribute_list_takes_luau_attributes_alone() {
    let ok = compile(
        "@[native, deprecated {use = \"g\", reason = \"old\"}]\nlocal function f() end\nf()\n",
    );
    assert!(ok.diagnostics.is_empty(), "{:?}", ok.diagnostics);
    assert!(
        ok.ship
            .contains("@[native, deprecated {use = \"g\", reason = \"old\"}]"),
        "{}",
        ok.ship
    );

    let got = messages(
        "@[allow(x)]\nlocal function a() end\n@[frob]\nlocal function b() end\n@[native(1)]\nlocal function c() end\n@[deprecated \"x\"]\nlocal function d() end\n@[checked]\nstruct S as\n    x: number\nend\n",
    );
    assert!(
        got.iter()
            .any(|m| m.contains("write `@allow` for the Alloy attribute")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|m| m.starts_with("Luau has no attribute `frob`")),
        "{got:?}"
    );
    assert!(
        got.iter().any(|m| m == "`native` takes no argument"),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|m| m.starts_with("`deprecated` takes a table")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|m| m.starts_with("Luau's attribute list goes on a function")),
        "{got:?}"
    );
}

#[test]
fn serde_options_shape_the_tables() {
    let out = compile(
        "@derive(Serialize, Deserialize)\n@rename_all(\"camelCase\")\n@deny_unknown_fields\nstruct Save as\n    max_health: number\n    @alias(\"hp\")\n    health: number = 100\nend\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains("maxHealth = self.max_health"),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("health = (if t.health ~= nil then t.health else t.hp)"),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("({ maxHealth = true, health = true, hp = true })[k]"),
        "{}",
        out.ship
    );
    // The alias reads and writes the field's slot on a value.
    assert!(
        out.ship.contains("local Save__alias = { hp = \"health\" }"),
        "{}",
        out.ship
    );
    assert!(out.check.contains("hp: number"), "{}", out.check);

    let got = messages(
        "@rename_all(\"camelCase\")\n@deny_unknown_fields\nstruct P as\n    @alias(\"x\")\n    x: number\n    @alias(\"z\")\n    y: number\n    @alias(\"z\")\n    w: number\nend\n",
    );
    assert!(
        got.iter()
            .any(|m| m.starts_with("`@rename_all` sets the keys")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|m| m.starts_with("`@deny_unknown_fields` checks")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|m| m.starts_with("`x` is a field of this struct")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|m| m.starts_with("two fields take the alias `z`")),
        "{got:?}"
    );
    assert!(
        messages("@derive(Serialize)\n@rename_all(\"camel\")\nstruct P as\n    x: number\nend\n")
            [0]
        .starts_with("`@rename_all` takes one style")
    );
}
