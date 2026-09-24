//! A parameter as a table pattern: `function draw({ x, y }: Point)`.

fn compile(src: &str) -> alloy::Output {
    alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap()
}

fn ship(src: &str) -> String {
    let out = compile(src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );

    out.ship
}

fn messages(src: &str) -> Vec<String> {
    compile(src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

/// The pattern takes a temp and opens on the header's line.
#[test]
fn an_annotated_pattern_opens_on_the_header_line() {
    let out = ship(
        "type Point = { x: number, y: number }\nlocal function draw({ x, y = top }: Point)\n    print(x, top)\nend\n",
    );

    assert!(
        out.contains("local function draw(_p1: Point) local x, top = _p1.x, _p1.y\n"),
        "{out}"
    );
}

/// The typed fields state the parameter's type.
#[test]
fn typed_fields_state_the_type() {
    let out = ship("local function foo({ bar: string, baz: number })\n    print(bar, baz)\nend\n");

    assert!(
        out.contains("local function foo(_p1: { bar: string, baz: number }) local bar: string, baz: number = _p1.bar, _p1.baz\n"),
        "{out}"
    );
}

/// `...rest` copies the fields the pattern does not name.
#[test]
fn a_rest_takes_the_other_fields() {
    let out = ship(
        "type A = { id: string, color: string }\nlocal function tag({ id, ...rest }: A)\n    print(id, rest)\nend\nlocal { id, ...others } = { id = \"a\", color = \"b\" } :: A\nprint(id, others)\n",
    );

    assert!(
        out.contains("local id, rest: { [string]: unknown } = _p1.id, (function(t: any): any"),
        "{out}"
    );
    assert!(out.contains("if k ~= \"id\" then r[k] = v end"), "{out}");
    assert!(
        out.contains("local id, others: { [string]: unknown } = "),
        "{out}"
    );

    // A string index in the annotation is the type the rest keeps.
    let out =
        ship("local function f({ a, ...more }: { [string]: number })\n    print(a, more)\nend\n");
    assert!(out.contains("more: { [string]: number }"), "{out}");
}

/// A default settles the value before the pattern opens it.
#[test]
fn a_default_comes_before_the_pattern() {
    let out = ship(
        "type Point = { x: number, y: number }\nlocal function at({ x }: Point = { x = 0, y = 0 })\n    print(x)\nend\n",
    );

    assert!(
        out.contains("local function at(_p1: Point?) local _p1: Point = if _p1 == nil then { x = 0, y = 0 } else _p1 local x = _p1.x\n"),
        "{out}"
    );
}

/// A method, a function expression, and a macro take a pattern too.
#[test]
fn every_parameter_list_takes_a_pattern() {
    let out = ship(
        "struct Rect as\n    w: number\n    h: number\nend\nimpl Rect as\n    function scaled(self, { x, y }: { x: number, y: number }): Rect\n        return Rect.new({ w = self.w * x, h = self.h * y })\n    end\nend\nlocal f = function({ w, h }: Rect): number\n    return w * h\nend\nmacro area({ w, h }: Rect)\n    return w * h\nend\nlocal r = Rect.new({ w = 2, h = 3 })\nprint(r:scaled({ x = 2, y = 2 }), f(r), $area(r))\n",
    );

    assert!(out.contains("function Rect.scaled(self, _p1: { x: number, y: number }): Rect local x, y = _p1.x, _p1.y"), "{out}");
    assert!(
        out.contains("function(_p1: Rect): number local w, h = _p1.w, _p1.h"),
        "{out}"
    );
    assert!(out.contains("((r).w) * ((r).h)"), "{out}");
}

/// Luau's definitions take a name for each parameter.
#[test]
fn a_declaration_writes_the_temp() {
    let out = alloy::compile_with(
        "type Point = { x: number, y: number }\ndeclare function plot({ x, y }: Point): ()\ndeclare function label({ text: string, size: number }): ()\n",
        &alloy::EmitOptions {
            definitions: true,
            ..alloy::EmitOptions::default()
        },
    )
    .unwrap();

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains("declare function plot(_p1: Point): ()"),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("declare function label(_p1: { text: string, size: number }): ()"),
        "{}",
        out.ship
    );
}

#[test]
fn the_patterns_the_rfc_refuses_report() {
    let point = "struct Point as\n    x: number\n    y: number\nend\n";
    let one = |body: &str| messages(&format!("{point}{body}"));

    assert_eq!(
        one("local function a({ x, y })\n    print(x, y)\nend\n"),
        vec![
            "`{ x, y }` has no type; annotate the parameter, `{ x, y }: Options`, or type each field"
        ]
    );
    assert_eq!(
        one("local function b({ x: number }: Point)\n    print(x)\nend\n"),
        vec![
            "`{ x: number }: Point` states the shape twice; drop the field types or drop the annotation"
        ]
    );
    assert_eq!(
        one("local function c({ x, z }: Point)\n    print(x, z)\nend\n"),
        vec!["`Point` has no field `z`"]
    );
    assert_eq!(
        one("local function d({ x }: Point?)\n    print(x)\nend\n"),
        vec!["a pattern needs a value; `Point?` may be nil"]
    );
    assert_eq!(
        one("local function e({ x: number, y })\n    print(x, y)\nend\n"),
        vec![
            "`{ x: number, y }` types some fields and not others; type every field, or annotate the parameter"
        ]
    );
    assert_eq!(
        one("remote Move({ x, y }: Point) from client\n"),
        vec![
            "a remote packs no pattern: `{ x, y }` has no name the wire layout can read; name the parameter"
        ]
    );
    assert_eq!(
        one("local function f({ x, ...rest, y }: Point)\n    print(x, rest)\nend\n"),
        vec!["`...rest` takes the fields that are left; no name follows it"]
    );
}

/// A field type copies through the type edits, in a parameter and in a
/// `local`.
#[test]
fn a_field_type_takes_its_luau_form() {
    let out = ship(
        "local function f({ list: string[], id: ~nil })\n    print(list, id)\nend\nlocal { b: number[] } = { b = [1] }\nprint(b)\n",
    );

    assert!(
        out.contains("_p1: { list: __alloy.Array<string>, id: __neg<nil> }) local list: __alloy.Array<string>, id: __neg<nil> ="),
        "{out}"
    );
    assert!(out.contains("local b: __alloy.Array<number> ="), "{out}");
}

/// A pattern over several lines keeps them, so the body stays on its
/// own lines.
#[test]
fn a_pattern_over_several_lines_keeps_the_body_in_place() {
    let out = ship(
        "type Point = { x: number, y: number }\nlocal function f({\n    x,\n    y,\n}: Point)\n    print(x, y)\nend\nfor _, {\n    x = ex,\n} in { { x = 1, y = 2 } } do\n    print(ex)\nend\n",
    );
    let lines: Vec<&str> = out.lines().collect();

    assert_eq!(lines[5], "    print(x, y)", "{out}");
    assert_eq!(lines[10], "    print(ex)", "{out}");
}

/// A temp never takes a name the source already binds.
#[test]
fn a_temp_skips_a_name_the_source_binds() {
    let out = ship(
        "type B = { b: number }\nlocal function named(_p1: string, { b }: B)\n    print(_p1, b)\nend\nlocal _p2 = \"keep\"\nfor _, { b } in { { b = 1 } } do\n    print(_p2, b)\nend\n",
    );

    assert!(
        out.contains("named(_p1: string, _p3: B) local b = _p3.b"),
        "{out}"
    );
    assert!(out.contains("for _, _p3 in"), "{out}");
}

/// A trait signature and a default body take the pattern as a temp.
#[test]
fn a_trait_method_takes_a_pattern() {
    let out = ship(
        "type Point = { x: number, y: number }\ntrait Mover as\n    function move(self, { x, y }: Point): ()\n    function at(self, { x }: Point): string\n        return `at {x}`\n    end\nend\n",
    );

    assert!(out.contains("read move: (self: any, Point) -> ()"), "{out}");
    assert!(
        out.contains("function Mover.at(self, _p1: Point): string local x = _p1.x"),
        "{out}"
    );
}

/// A declaration rewrites every pattern, whatever its type spells.
#[test]
fn every_declared_pattern_writes_a_temp() {
    let out = alloy::compile_with(
        "type Vec2 = { x: number, y: number }\ndeclare function spin({ x, y }: Vec2): ()\ndeclare function on({ cb: (n: number) -> string }): ()\ndeclare function after(f: () -> (), { x }: Vec2): ()\ndeclare extern type Canvas with\n    function dot(self, { x }: Vec2): ()\n    function line(self, { x }: Vec2, { y }: Vec2): ()\nend\ndeclare class Brush\n    function paint(self, { x }: Vec2): ()\nend\n",
        &alloy::EmitOptions {
            definitions: true,
            ..alloy::EmitOptions::default()
        },
    )
    .unwrap();

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    for want in [
        "declare function spin(_p1: Vec2): ()",
        "declare function on(_p1: { cb: (n: number) -> string }): ()",
        "declare function after(f: () -> (), _p1: Vec2): ()",
        "function line(self, _p2: Vec2, _p3: Vec2): ()",
        "declare extern type Brush with\n    function paint(self, _p1: Vec2): ()",
    ] {
        assert!(out.ship.contains(want), "{want}: {}", out.ship);
    }
}

/// A declaration checks its patterns the way a function does.
#[test]
fn a_declared_pattern_reports() {
    let out = alloy::compile_with(
        "type Vec2 = { x: number, y: number }\ndeclare function a({ x, y }): ()\ndeclare function b({ x }: Vec2?): ()\n",
        &alloy::EmitOptions {
            definitions: true,
            ..alloy::EmitOptions::default()
        },
    )
    .unwrap();
    let messages: Vec<String> = out.diagnostics.into_iter().map(|d| d.message).collect();

    assert_eq!(
        messages,
        vec![
            "`{ x, y }` has no type; annotate the parameter, `{ x, y }: Options`, or type each field",
            "a pattern needs a value; `Vec2?` may be nil",
        ]
    );
}

/// A macro binds `...rest`, and a field name in its body stays.
#[test]
fn a_macro_pattern_binds_the_rest() {
    let out = ship(
        "type R = { w: number, h: number }\nmacro m({ w, ...others }: R, o: R)\n    print(w, others, o.w, { w = w })\nend\nlocal r: R = { w = 1, h = 2 }\n$m(r, r)\n",
    );

    assert!(
        out.contains("print(((r).w), ((function(t: any): any"),
        "{out}"
    );
    assert!(out.contains("end)(r)), r.w, { w = ((r).w) })"), "{out}");
}

/// A default settles an optional pattern, so its local drops the `?`.
#[test]
fn a_default_settles_an_optional_pattern() {
    let out = ship(
        "type Point = { x: number, y: number }\nlocal function at({ x }: Point? = { x = 0, y = 0 })\n    print(x)\nend\n",
    );

    assert!(
        out.contains("at(_p1: Point?) local _p1: Point = if _p1 == nil"),
        "{out}"
    );

    let out = ship(
        "type Point = { x: number, y: number }\nlocal function at({ x }: Point | nil = { x = 0, y = 0 })\n    print(x)\nend\n",
    );
    assert!(out.contains("local _p1: Point = if _p1 == nil"), "{out}");
}

/// A bound on a generic reaches a pattern local of that type through a
/// cast, since the table in the header drops it.
#[test]
fn a_bound_reaches_a_pattern_local() {
    let out = ship(
        "trait Shape as\n    function area(self): number\nend\nlocal function f<T: Shape>({ item }: { item: T, n: number }): number\n    return item:area()\nend\n",
    );

    assert!(
        out.contains("local item: (T & Shape) = (_p1.item :: (T & Shape))"),
        "{out}"
    );
}

/// The problems the first pass let through.
#[test]
fn a_pattern_reports_what_it_hides() {
    let point = "type Point = { x: number, y: number }\n";
    let one = |body: &str| messages(&format!("{point}{body}"));

    assert_eq!(
        one("local function g({ x }: Point, x: number)\n    print(x)\nend\n"),
        vec!["`x` is already a parameter; one name holds one declaration"]
    );
    assert_eq!(
        one("local function k({ x, huh }: Point)\n    print(x, huh)\nend\n"),
        vec!["`Point` has no field `huh`"]
    );
    assert_eq!(
        one("local function m({ x }: Point | nil)\n    print(x)\nend\n"),
        vec!["a pattern needs a value; `Point | nil` may be nil"]
    );
    assert_eq!(
        one("local function l([a, b])\n    print(a, b)\nend\n"),
        vec!["`[a, b]` has no type; annotate the parameter, `[a, b]: T[]`"]
    );
    assert_eq!(
        one("local function o({ x, ...rest: { [string]: number } }: Point)\n    print(x)\nend\n"),
        vec!["`...rest` takes no type; the annotation's index type is its type"]
    );
}

/// The analyzer reads the fields through the pattern's type, in a
/// `.aly` file and in a `.alx` component.
#[test]
fn the_checker_reads_the_fields() {
    use std::fs;

    let dir = std::env::temp_dir().join(format!("alloy-param-pattern-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[alx.factory]\nbackend = \"table\"\ncreate = \"create\"\ninterpolate = \"plain\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/create.aly"),
        "export function create(class: string): (props: { [any]: any }) -> Instance\n    return function(props)\n        local i = Instance.new(class)\n        for k, v in props do\n            if typeof(k) == \"string\" then\n                (i :: any)[k] = v\n            end\n        end\n        return i\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "--!strict\ntype Point = { x: number, y: number }\nlocal function draw({ x, y }: Point): number\n    return x + y\nend\nlocal function wrong({ x }: Point): string\n    return x\nend\nprint(draw({ x = 1, y = 2 }), wrong({ x = 1, y = 2 }))\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/card.alx"),
        "--!strict\nimport { create } from \"./create\"\nexport type CardProps = { title: string, count: number }\nexport function Card({ title, count }: CardProps)\n    return <TextLabel Text={`{title}: {count}`} />\nend\nexport function Badge({ label: string })\n    return <TextLabel Text={label} />\nend\n",
    )
    .unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(&dir, &config).unwrap();

    if alloy::typecheck::find_luau_lsp(&config.flux).is_none() {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    }

    let analysis = alloy::typecheck::analyze(&dir, &config, &report.checks, &report.dep_artifacts)
        .expect("the type check runs");
    let errors: Vec<String> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{}:{}", d.rel.display(), d.line))
        .collect();

    // `wrong` returns a number as a string, on line 7; nothing else fails.
    assert_eq!(errors, vec!["main.aly:7"], "{:?}", analysis.diagnostics);

    let _ = fs::remove_dir_all(&dir);
}
