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
        out.contains("local id, rest: { [string]: unknown } = _p1.id, (function()"),
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
