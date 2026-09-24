//! The emit of shapes that once built clean and shipped wrong Luau.

fn ship(src: &str) -> String {
    let out = alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    out.ship
}

fn messages(src: &str) -> Vec<String> {
    alloy::compile_with(src, &alloy::EmitOptions::default())
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn a_rename_that_is_no_name_goes_in_brackets() {
    let out = ship(
        "@derive(Serialize)\nstruct P as\n    @rename('regen-per-second')\n    regen: number\n    @rename(\"end\")\n    stop: number\nend\n",
    );
    assert!(out.contains("[\"regen-per-second\"] = self.regen"), "{out}");
    assert!(out.contains("regen = t[\"regen-per-second\"]"), "{out}");
    assert!(out.contains("[\"end\"] = self.stop"), "{out}");
}

/// A key is the text the literal stands for, so the quote style the
/// formatter picks and an escape change nothing.
#[test]
fn a_rename_reads_the_text_of_its_literal() {
    let single =
        ship("@derive(Serialize)\nstruct P as\n    @rename('it\\'s')\n    a: number\nend\n");
    let double =
        ship("@derive(Serialize)\nstruct P as\n    @rename(\"it's\")\n    a: number\nend\n");
    for out in [single, double] {
        assert!(out.contains("{ [\"it's\"] = self.a }"), "{out}");
        assert!(out.contains("P({ a = t[\"it's\"] })"), "{out}");
    }
}

/// A field of a struct that derives Serialize goes through that
/// struct's own pair, so the table nests and reads back with its
/// metatable.
#[test]
fn a_nested_struct_serializes_through_its_own_pair() {
    let out = ship(
        "@derive(Serialize)\nstruct Inner as\n    v: number\nend\n@derive(Serialize)\nstruct Outer as\n    inner: Inner\n    maybe: Inner?\nend\n",
    );
    assert!(out.contains("inner = Inner.to_table(self.inner)"), "{out}");
    assert!(out.contains("inner = Inner.from_table(t.inner)"), "{out}");
    assert!(
        out.contains("maybe = if self.maybe == nil then nil else Inner.to_table(self.maybe)"),
        "{out}"
    );
}

#[test]
fn a_derive_reports_a_key_or_a_name_it_cannot_hold() {
    assert_eq!(
        messages(
            "@derive(Serialize, Clone)\nstruct D as\n    @rename(\"a\")\n    x: number\n    @rename(\"a\")\n    y: number\n    clone: number\nend\n"
        ),
        vec![
            "`x` and `y` serialize under one key, `a`, and the derived table keeps one; give one of them another `@rename`",
            "`clone` is a field of `D` and a method `@derive(Clone)` writes; one name holds one of the two",
        ]
    );
}

#[test]
fn an_enum_reports_the_names_its_emit_takes() {
    assert_eq!(
        messages("enum E as\n    is(number)\n    Other\nend\n"),
        vec!["a variant cannot be named `is`; the enum's type test `E.is(v)` takes that name"]
    );
    assert_eq!(
        messages("enum E as\n    A\n    A\nend\n"),
        vec!["`A` is already a variant of this enum"]
    );
    assert_eq!(
        messages(
            "enum Ev as\n    Hit\nend\nimpl Ev as\n    function tag(self)\n        return 1\n    end\nend\n"
        ),
        vec![
            "an enum method cannot be named `tag`; each variant keeps its name in the field `tag`"
        ]
    );
}

#[test]
fn a_temp_skips_the_names_the_source_uses() {
    let out = ship("local _1 = 5\nlocal t = {}\nlocal x = t?.a ?? 0\nprint(_1, x)\n");
    assert_eq!(out.matches("local _1").count(), 1, "{out}");
}

#[test]
fn a_default_that_reads_a_field_reports() {
    assert_eq!(
        messages("struct P as\n    w: number = 1\n    h: number = w * 3\nend\n"),
        vec![
            "a default cannot read the field `w`; the constructor fills defaults before the fields exist"
        ]
    );
    assert!(
        messages("local w = 2\nstruct P as\n    w: number = 1\n    h: number = w * 3\nend\n")
            .is_empty()
    );
}

#[test]
fn a_deprecated_message_goes_in_the_reason_table() {
    let out = ship("@deprecated(\"use g\")\nlocal function f() end\nf()\n");
    assert!(out.contains("@[deprecated {reason = \"use g\"}]"), "{out}");
}

#[test]
fn a_method_call_on_a_mixed_enum_names_the_string_variant() {
    assert_eq!(
        messages(
            "enum State as\n    Idle\n    Running(number)\nend\nimpl State as\n    function label(self): string\n        return \"x\"\n    end\nend\nlocal function describe(s: State): string\n    return s:label()\nend\nprint(describe(State.Idle))\n"
        ),
        vec!["`State.Idle` is a unit variant, a string at runtime; call `State.label(s)`"]
    );
}

/// A `.d.aly` declares globals. Luau makes a type of a definitions file
/// global only when it says `export`, so `interface Iface` stayed out of
/// every file's reach. An enum wrote its runtime table into the file,
/// and a trailing `return` followed.
#[test]
fn a_definitions_file_declares_every_type_global_and_runs_nothing() {
    let options = alloy::EmitOptions {
        file_name: "g.d.aly".to_string(),
        definitions: true,
        ..alloy::EmitOptions::default()
    };
    let src = "type Plain = number\nexport type Id = string\ninterface Iface as\n    a: number\nend\nstruct Rec as\n    c: number\nend\nenum Mode as Fast, Slow end\nenum Shape as\n    Circle(number),\n    Dot,\nend\ndeclare function area(s: Shape): Plain\n";
    let out = alloy::compile_with(src, &options).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        out.check.lines().count(),
        src.lines().count(),
        "{}",
        out.check
    );

    for line in [
        "export type Plain = number",
        "export type Id = string",
        "export type Iface = { a: number }",
        "export type Rec = { c: number }",
        "export type Mode = \"Fast\" | \"Slow\"",
        "export type Shape = { tag: \"Circle\", _1: number } | \"Dot\"",
    ] {
        assert!(out.check.contains(line), "{line}\n{}", out.check);
    }

    assert!(!out.check.contains("local "), "{}", out.check);
    assert!(!out.check.contains("return"), "{}", out.check);
}
