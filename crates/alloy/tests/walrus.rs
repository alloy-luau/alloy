//! `:=`, the append operator, and the `table` library as methods: what
//! each artifact holds under its flags, and what the source reports
//! with a flag off.

use alloy::EmitOptions;

fn compiled(src: &str, fflags: &str) -> alloy::Output {
    let options = EmitOptions {
        fflags: toml::from_str(fflags).unwrap(),
        ..EmitOptions::default()
    };

    alloy::compile_with(src, &options).unwrap()
}

fn messages(out: &alloy::Output) -> Vec<&str> {
    out.diagnostics.iter().map(|d| d.message.as_str()).collect()
}

const SYNTAX: &str = "AlloyExperimentalWalrusSyntax = true\n";
const BOTH: &str = "AlloyExperimentalWalrusSyntax = true\nAlloyExperimentalWalrusAnalysis = true\n";
const TABLE: &str = "AlloyExperimentalTableStdInheritance = true\n";

const APPEND: &str = "local t = { items = { 1 } }\nt.items := 2\nt.items:=3\nprint(t)\n";

#[test]
fn an_append_calls_the_runtime_and_keeps_its_line() {
    let out = compiled(APPEND, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(
        out.ship.contains("__alloy.append(t.items, 2)"),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains("__alloy.append(t.items, 3)"),
        "{}",
        out.ship
    );
    assert_eq!(
        out.ship.lines().count(),
        APPEND.lines().count(),
        "{}",
        out.ship
    );
}

/// The analysis flag changes the check artifact alone: the typed call
/// takes the target and the value apart.
#[test]
fn the_analysis_flag_types_the_check_artifact() {
    let typed = compiled(APPEND, BOTH);
    assert!(
        typed.check.contains("__alloy.append_checked(t.items)(2)"),
        "{}",
        typed.check
    );
    assert!(
        typed.ship.contains("__alloy.append(t.items, 2)"),
        "{}",
        typed.ship
    );

    let untyped = compiled(APPEND, SYNTAX);
    assert!(
        untyped.check.contains("__alloy.append(t.items, 2)"),
        "{}",
        untyped.check
    );
    assert!(
        !untyped.check.contains("append_checked"),
        "{}",
        untyped.check
    );
}

#[test]
fn an_append_with_the_flag_off_names_the_flag() {
    // The analysis flag alone turns nothing on.
    for fflags in ["", "AlloyExperimentalWalrusAnalysis = true\n"] {
        let out = compiled(APPEND, fflags);
        let messages = messages(&out);

        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].contains("AlloyExperimentalWalrusSyntax = true"),
            "{messages:?}"
        );
        assert!(!out.ship.contains("append"), "{}", out.ship);
    }
}

#[test]
fn an_append_takes_one_target_and_one_value() {
    let out = compiled(
        "local a, b = {}, {}\na, b := 1\na := 1, 2\nprint(a, b)\n",
        SYNTAX,
    );
    assert_eq!(
        messages(&out),
        [
            "`:=` takes one target and one value",
            "`:=` takes one target and one value"
        ]
    );
}

/// An append writes into the value and leaves the name, so a `const`
/// takes it. A plain assignment to the same name still reports.
#[test]
fn a_const_takes_an_append() {
    let out = compiled("const xs = { 1 }\nxs := 2\nprint(xs)\n", SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);

    let out = compiled("const xs = { 1 }\nxs = { 2 }\nprint(xs)\n", SYNTAX);
    assert_eq!(messages(&out).len(), 1);
}

/// `:=` leaves a method call, a ternary, and an annotation as they are.
#[test]
fn the_colon_of_other_syntax_stays() {
    let src = "local s: string = \"a\"\nlocal n = #s > 0 ? s:upper() : s\nlocal xs: { string } = {}\nxs := n\nprint(xs)\n";
    let out = compiled(src, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(out.ship.contains("__alloy.append(xs, n)"), "{}", out.ship);
    assert!(out.ship.contains("s:upper()"), "{}", out.ship);
}

#[test]
fn the_formatter_spaces_an_append() {
    assert_eq!(
        alloy::fmt::format("xs:=1\nxs  :=  2\n").unwrap(),
        "xs := 1\nxs := 2\n"
    );
}

const METHODS: &str = "local xs = { 2, 1 }\nxs:insert(3)\nxs:sort()\nlocal last = xs:remove()\nprint(xs:concat(\", \"), last, xs:len())\n";

#[test]
fn a_table_method_goes_through_the_dispatcher() {
    let out = compiled(METHODS, TABLE);
    assert_eq!(messages(&out), [] as [&str; 0]);

    for call in [
        "__alloy.table_call(xs, \"insert\", 3)",
        "__alloy.table_call(xs, \"sort\")",
        "local last = __alloy.table_call(xs, \"remove\")",
        "__alloy.table_call(xs, \"concat\", \", \")",
        // No function of the library has this name.
        "xs:len()",
    ] {
        assert!(out.ship.contains(call), "{call}\n{}", out.ship);
    }

    assert!(
        out.check.contains("__alloy.table_std(xs):insert(3)"),
        "{}",
        out.check
    );
    assert_eq!(
        out.ship.lines().count(),
        METHODS.lines().count(),
        "{}",
        out.ship
    );
}

#[test]
fn a_table_method_with_the_flag_off_stays_as_written() {
    let out = compiled(METHODS, "");
    assert!(out.ship.contains("xs:insert(3)"), "{}", out.ship);
    assert!(!out.ship.contains("table_call"), "{}", out.ship);
    assert!(!out.check.contains("table_std"), "{}", out.check);
}
