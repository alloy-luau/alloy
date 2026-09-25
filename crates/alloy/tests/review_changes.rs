//! The changes the language review asked for: headers without `as`, a
//! guard written `where`, a value name in a pattern, `in` on a `{ }`
//! literal, `[[` as Luau's long string, and arms that run statements.

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
fn a_body_on_the_next_line_needs_no_as() {
    let out = compile(
        "trait Named\n    function name(self): string\nend\nstruct P\n    label: string\nend\nimpl Named for P\n    function name(self): string return self.label end\nend\nenum Phase\n    Lobby\n    Playing(number)\nend\nnamespace Geo\n    function sq(x: number) return x * x end\nend\nprint(new P { label = \"a\" }:name(), Phase.Lobby, Geo.sq(2))\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(!out.ship.contains(" as\n"), "{}", out.ship);
}

#[test]
fn a_guard_takes_where_and_the_old_and_lints() {
    let src = "local function f(x: number): string\n    return match x with\n        case n where n > 5 then \"big\"\n        case n and n > 2 then \"mid\"\n        default \"small\"\n    end\nend\nprint(f(1))\n";
    let out = compile(src);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let guards: Vec<_> = out
        .lints
        .iter()
        .filter(|l| l.name == "match_guard_and")
        .collect();
    assert_eq!(guards.len(), 1, "{:?}", out.lints);
    assert_eq!(
        &src[guards[0].start as usize..guards[0].end as usize],
        "and"
    );
}

#[test]
fn a_value_name_in_a_pattern_reports() {
    let got = messages(
        "const MAX = 10\nlocal function f(x: number): string\n    return match x with\n        case MAX then \"max\"\n        default \"other\"\n    end\nend\nprint(f(1))\n",
    );

    assert_eq!(
        got,
        [
            "`MAX` names a value, and a bare name in a pattern binds a new one, so this arm takes every value; compare in a guard: `case n where n == MAX`"
        ]
    );
    // A lowercase binding is the catch-all it has always been.
    assert!(messages("local function f(x: number)\n    match x with\n        case 1 then print(1)\n        case n then print(n)\n    end\nend\nf(1)\n").is_empty());
}

#[test]
fn in_on_a_table_literal_with_items_reports() {
    assert_eq!(
        messages("print(2 in { 5, 6 })\n"),
        [
            "`in` on a `{ }` literal searches its keys, 1, 2, and on, not its items; write `[ 5, 6 ]` to search the items"
        ]
    );
    // A parenthesized literal is the same literal.
    assert_eq!(messages("print(k in ({ \"a\", \"b\" }))\n").len(), 1);
    assert!(messages("print(2 in [ 5, 6 ], \"a\" in { a = true })\n").is_empty());
}

#[test]
fn a_long_string_is_luau() {
    let out = compile(
        "local s = [[hello]]\nlocal grid = [ [1, 2], [3] ]\nlocal m = $map[[\"a\", 1]]\nprint(s, grid, m)\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.ship.contains("local s = [[hello]]"), "{}", out.ship);
    assert!(
        out.ship.contains("HashMap.from({ [\"a\"] = 1 }"),
        "{}",
        out.ship
    );
}

/// A struct another file declares names the wire layout only when this
/// file imports it. A `Player` struct elsewhere made `target: Player` a
/// table layout, and the remote refused every real Player.
#[test]
fn a_struct_elsewhere_shapes_a_remote_only_when_imported() {
    let options = alloy::EmitOptions {
        shapes: vec![alloy::StructShape {
            name: "Player".into(),
            fields: vec![alloy::WireField {
                name: "x".into(),
                ty: "number".into(),
                width: None,
            }],
            derives: Vec::new(),
        }],
        ..alloy::EmitOptions::default()
    };
    let remote = "remote Hit(target: Player) from client\nprint(Hit)\n";

    let out = alloy::compile_with(remote, &options).unwrap();
    assert!(!out.ship.contains("struct = Player"), "{}", out.ship);

    let imported = format!("import {{ Player }} from \"./p\"\n{remote}");
    let out = alloy::compile_with(&imported, &options).unwrap();
    assert!(out.ship.contains("struct = Player"), "{}", out.ship);
}

/// A macro call is a statement, so it is the body of a one-line arm.
#[test]
fn a_macro_call_is_a_statement_arm() {
    assert!(messages("local function f(x: number)\n    match x with\n        case 1 then $assert(x > 0)\n        default $unreachable()\n    end\nend\nf(1)\n").is_empty());
}

/// serde's case styles: `lowercase` and `UPPERCASE` change the case and
/// keep the underscores.
#[test]
fn rename_all_case_styles_follow_serde() {
    for (style, key) in [
        ("lowercase", "max_hp"),
        ("UPPERCASE", "MAX_HP"),
        ("camelCase", "maxHp"),
    ] {
        let src = format!(
            "import {{ Serialize }} from \"@alloy/std/serde\"\n@derive(Serialize)\n@rename_all(\"{style}\")\nstruct P\n    max_hp: number\nend\nprint(P)\n"
        );
        let out = compile(&src);
        assert!(
            out.ship.contains(&format!("{key} = self.max_hp")),
            "{style}: {}",
            out.ship
        );
    }
}

/// A half-typed arm, one the author is still typing, reports and the
/// compile goes on: the server compiles on every keystroke.
#[test]
fn a_half_typed_arm_reports_and_does_not_crash() {
    for src in [
        "local r = 1\nmatch r with\n  case 1\nend\n",
        "local r = 1\nlocal v = match r with\n  case 1\nend\n",
        "local r = 1\nmatch r with\n  case 1 then print(1)\n  case 2\nend\n",
        "local x = 3\nlocal r = match x with\n  case 1 then 0\n  case n\n  default 0\n",
    ] {
        let got = alloy::compile(src)
            .map(|o| o.diagnostics.len())
            .unwrap_or(1);
        assert!(got >= 1, "{src:?}");
    }
    // A declaration word as a match head is a name.
    assert!(messages("local trait = 1\nmatch trait with\n  case 1 then print(1)\n  case _ then print(2)\nend\n").is_empty());
}

/// An arm that runs statements ends in its value. The match lifts into
/// the statement that takes the value, so `return` in an arm leaves the
/// function and `continue` goes to the loop, and every line stays put.
#[test]
fn a_block_arm_lifts_into_its_statement() {
    let src = "local function f(x: number): number?\n    local v = match x with\n        case 1 then\n            print(\"one\")\n            10\n        case 2 then\n            return nil\n        default\n            local y = x * 2\n            -y\n    end\n    print(v)\n    return v\nend\nlocal function g(xs: {number}): number\n    local total = 0\n    for _, x in xs do\n        total = match x with\n            case 0 then continue\n            default\n                print(x)\n                total + x\n        end\n    end\n    return total\nend\nlocal function h(x: number): string\n    return match x with\n        case 1 then\n            print(\"h\")\n            \"one\"\n        default \"other\"\n    end\nend\nprint(f(1), f(2), f(3), g({ 0, 1, 2 }), h(1))\n";
    let out = compile(src);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(out.ship.lines().count(), src.lines().count());
    for line in [
        "            v = 10",
        "            return nil",
        "            v = -y",
        "            if _m1 == 0 then continue",
        "                total = total + x",
        "            return \"one\"",
        "        else return \"other\"",
    ] {
        assert!(out.ship.contains(line), "{line:?} in {}", out.ship);
    }

    // The checker reads a bare `local v` as nil until an arm sets it;
    // `never` keeps the hover at the arms' type.
    let options = alloy::EmitOptions {
        check: true,
        ..alloy::EmitOptions::default()
    };
    let check = alloy::compile_with(src, &options).unwrap().check;
    assert!(
        check.contains("    local v = nil :: never do local _m1 = x"),
        "{check}"
    );
}

#[test]
fn a_block_arm_needs_a_statement_to_lift_into() {
    assert_eq!(
        messages(
            "print(match 1 with\n    case 1 then\n        print(\"x\")\n        1\n    default 2\nend)\n"
        ),
        [
            "a match whose arms run statements stands after `local x =`, `x =`, or `return`; bind it to a local first"
        ]
    );
    assert_eq!(
        messages(
            "local v = match 1 with\n    case 1 then\n        print(\"x\")\n        local y = 1\n    default 2\nend\nprint(v)\n"
        ),
        ["this arm gives no value: end it with the value, or leave with `return`"]
    );
}

/// `-y` on its own line starts a value; fmt keeps it off the line above.
#[test]
fn fmt_keeps_a_block_arm_value() {
    let src = "local v = match 1 with\n  case 1 then\n    print('x')\n    -1\n  default 2\nend\nprint(v)\n";

    assert_eq!(alloy::fmt::format(src).unwrap(), src);
}

/// The cast that keeps Luau off a header function's `return` is the
/// check artifact's alone, and a `return` of the loop body keeps its own.
#[test]
fn a_loop_header_function_casts_in_the_check_artifact_only() {
    let src = "local xs = [ 1 ]\nlocal function f(): number\n    for x in xs:filter(function(v) return v > 2 end) do\n        return x\n    end\n    return 0\nend\nprint(f())\n";
    let options = alloy::EmitOptions {
        check: true,
        ..alloy::EmitOptions::default()
    };
    let check = alloy::compile_with(src, &options).unwrap().check;

    assert!(
        check.contains("function(v) return ((v > 2) :: any) end"),
        "{check}"
    );
    assert!(check.contains("        return x\n"), "{check}");
    assert!(compile(src).ship.contains("function(v) return v > 2 end"));
}
