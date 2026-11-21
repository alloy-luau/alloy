//! Comprehensions, chained comparisons, and the string methods: what
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

const SYNTAX: &str = "AlloyExperimentalComprehensionSyntax = true\n";
const BOTH: &str =
    "AlloyExperimentalComprehensionSyntax = true\nAlloyExperimentalComprehensionAnalysis = true\n";
const CHAIN: &str = "AlloyExperimentalChainedComparisonSyntax = true\n";
const STRING: &str = "AlloyExperimentalStringMethods = true\n";

/// The header is the header of a `for` loop, and `=` counts. The
/// filter guards the append.
#[test]
fn a_comprehension_is_a_loop_in_a_function_called_in_place() {
    let src = "local xs = { 1, 2 }\nlocal a = [ x * 2 for _, x in xs if x > 1 ]\nlocal b = [ k for k, v in xs where v ]\nlocal c = [ i * i for i = 1, 3 ]\nprint(a, b, c)\n";
    let out = compiled(src, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);

    for want in [
        "local a = __alloy.Array.from((function() local __list1 = {} for _, x in xs do if x > 1 then __list1[#__list1 + 1] = x * 2 end end return __list1 end)())",
        "for k, v in xs do if v then __list2[#__list2 + 1] = k end end",
        "for i = 1, 3 do __list3[#__list3 + 1] = i * i end",
    ] {
        assert!(out.ship.contains(want), "{want}\n{}", out.ship);
    }
}

/// A comprehension over several lines keeps the line count, so a
/// report after it lands on its own line.
#[test]
fn a_comprehension_keeps_the_lines_of_the_source() {
    let src =
        "local xs = { 1, 2 }\nlocal a = [\n    x + 1\n    for x in xs\n    if x > 1\n]\nprint(a)\n";
    let out = compiled(src, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );
    assert!(
        out.ship.lines().last().unwrap().starts_with("print(a)"),
        "{}",
        out.ship
    );
}

/// A body that reads `...` passes it into the function.
#[test]
fn a_comprehension_passes_the_varargs_in() {
    let src = "local function f(...)\n    return [ x + select(\"#\", ...) for x in { 1 } ]\nend\nprint(f)\n";
    let out = compiled(src, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(out.ship.contains("(function(...) "), "{}", out.ship);
    assert!(out.ship.contains("end)(...))"), "{}", out.ship);
}

/// The analysis flag changes the check artifact alone: without it the
/// list and the result are `any`.
#[test]
fn the_analysis_flag_types_the_check_artifact() {
    let src = "local xs = { 1, 2 }\nlocal a = [ x for x in xs ]\nprint(a)\n";
    let options = |fflags: &str| EmitOptions {
        check: true,
        fflags: toml::from_str(fflags).unwrap(),
        ..EmitOptions::default()
    };
    let loose = alloy::compile_with(src, &options(SYNTAX)).unwrap().check;
    let typed = alloy::compile_with(src, &options(BOTH)).unwrap().check;

    assert!(loose.contains("local __list1: { any } = {}"), "{loose}");
    assert!(loose.contains(":: any)"), "{loose}");
    assert!(typed.contains("local __list1 = {}"), "{typed}");
    assert!(!typed.contains(":: any)"), "{typed}");
}

#[test]
fn a_comprehension_needs_its_flag() {
    let out = compiled("local a = [ x for x in { 1 } ]\nprint(a)\n", "");
    assert_eq!(
        messages(&out),
        [
            "a comprehension is not final; set `AlloyExperimentalComprehensionSyntax = true` under `[fflags]` in alloy.toml to use it"
        ]
    );
}

#[test]
fn a_comprehension_takes_plain_names() {
    let out = compiled(
        "local a = [ x for { x } in { { x = 1 } } ]\nprint(a)\n",
        SYNTAX,
    );
    assert_eq!(
        messages(&out),
        ["a comprehension takes plain loop names; destructure the item in a `for` loop"]
    );
}

/// An array literal is not a comprehension, with the flag on or off.
#[test]
fn an_array_literal_stays_an_array() {
    let out = compiled("local a = [ 1, 2 ]\nprint(a)\n", SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(out.ship.contains("Array.from({ 1, 2 })"), "{}", out.ship);
}

/// A middle operand that is a name or a literal is written two times.
/// Any other chain runs in a function, so the operand runs one time.
#[test]
fn a_chain_is_its_pairs_joined_with_and() {
    let src = "local n = 3\nlocal function f() return 5 end\nprint(1 < n <= 10, 10 >= n > 1, 0 <= f() < 10)\n";
    let out = compiled(src, CHAIN);
    assert_eq!(messages(&out), [] as [&str; 0]);

    for want in [
        "(1 < n and n <= 10)",
        "(10 >= n and n > 1)",
        "(function() local __l = 0  local __r = f()  if not (__l <= __r) then return false end  __l = __r  __r = 10  return __l < __r end)()",
    ] {
        assert!(out.ship.contains(want), "{want}\n{}", out.ship);
    }
}

#[test]
fn a_chain_goes_one_way() {
    let out = compiled("local n = 3\nprint(1 < n > 0)\n", CHAIN);
    assert_eq!(
        messages(&out),
        ["a chained comparison goes one way: `<` and `<=`, or `>` and `>=`"]
    );
}

/// With the flag off the text goes to Luau as it is. Parentheses and
/// `==` never chain.
#[test]
fn a_chain_needs_its_flag_and_leaves_the_rest() {
    let src = "local n = 3\nprint(1 < n < 10)\n";
    let out = compiled(src, "");
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(out.ship.contains("print(1 < n < 10)"), "{}", out.ship);

    let src = "local n = 3\nprint((1 < n) == true, n == 3 == true)\n";
    let out = compiled(src, CHAIN);
    assert!(
        out.ship.contains("print((1 < n) == true, n == 3 == true)"),
        "{}",
        out.ship
    );
}

/// The ship artifact asks the value at run time, and the check
/// artifact wraps the receiver in the type that has the methods.
#[test]
fn a_string_method_goes_through_the_runtime() {
    let src = "local s = \" a \"\nprint(s:trim(), s:pad_start(5, \"0\"), s:upper())\n";
    let out = compiled(src, STRING);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(
        out.ship.contains(
            "print(__alloy.table_call(s, \"trim\"), __alloy.table_call(s, \"pad_start\", 5, \"0\"), s:upper())"
        ),
        "{}",
        out.ship
    );
    assert!(
        out.check.contains("__alloy.string_std(s):trim()"),
        "{}",
        out.check
    );

    let off = compiled(src, "");
    assert!(off.ship.contains("s:trim()"), "{}", off.ship);
}

/// A new function of a std module reports with its flag off. A local
/// of the same name is not the std module.
#[test]
fn a_std_addition_needs_its_flag() {
    let src = "local xs = Array.range(1, 3)\nprint(xs)\n";
    assert_eq!(
        messages(&compiled(src, "")),
        [
            "`Array.range` is not final; set `AlloyExperimentalStdAdditions = true` under `[fflags]` in alloy.toml to use it"
        ]
    );
    assert_eq!(
        messages(&compiled(src, "AlloyExperimentalStdAdditions = true\n")),
        [] as [&str; 0]
    );

    let own = "local Array = { range = print }\nArray.range(1, 3)\n";
    assert_eq!(messages(&compiled(own, "")), [] as [&str; 0]);
}

const LOGICAL: &str = "AlloyExperimentalLogicalAssignSyntax = true\n";

/// The target reads two times in the emit, so a call in it runs one
/// time, into a local.
#[test]
fn a_logical_assignment_is_the_assignment_of_the_operator() {
    let src =
        "local a, t = nil, {}\na or= 1\na and= a + 1\nt.x or= {}\nt[f()].y and= g()\nprint(a, t)\n";
    let out = compiled(src, LOGICAL);
    assert_eq!(messages(&out), [] as [&str; 0]);

    for want in [
        "a = a or 1",
        "a = a and a + 1",
        "t.x = t.x or {}",
        ".y and g()",
    ] {
        assert!(out.ship.contains(want), "{want}\n{}", out.ship);
    }

    assert_eq!(out.ship.matches("f()").count(), 1, "{}", out.ship);
    assert_eq!(out.ship.lines().count(), src.lines().count());
}

#[test]
fn a_logical_assignment_needs_its_flag() {
    let out = compiled("local a = nil\na or= 1\nprint(a)\n", "");
    assert_eq!(
        messages(&out),
        [
            "`or=` is not final; set `AlloyExperimentalLogicalAssignSyntax = true` under `[fflags]` in alloy.toml to use it"
        ]
    );
}
