//! `~T`, the type negation: its emit and the reports around it.

fn compile(src: &str) -> alloy::Output {
    alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap()
}

fn ship(src: &str) -> String {
    let out = compile(src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    out.ship
}

fn messages(src: &str) -> Vec<String> {
    compile(src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

/// The first line declares the type function once, and each `~T`
/// becomes a call of it on its own line.
#[test]
fn a_negation_lowers_to_the_file_type_function() {
    let src = "local a: ~number = \"text\"\nlocal function label(v: ~nil): string\n    return tostring(v)\nend\ntype NotText = ~string\nprint(a, label(1))\n";
    let out = ship(src);
    let first = out.lines().next().unwrap_or("");
    assert!(
        first.contains("type function __neg(t) local function each(u)"),
        "{out}"
    );
    assert!(first.contains("local a: __neg<number> = \"text\""), "{out}");
    assert!(out.contains("(v: __neg<nil>): string"), "{out}");
    assert!(out.contains("type NotText = __neg<string>"), "{out}");
    assert_eq!(out.lines().count(), src.lines().count(), "{out}");
    assert_eq!(out.matches("type function __neg").count(), 1, "{out}");
}

/// `~` binds tighter than `|` and `&` and looser than a suffix; the
/// parentheses read the other grouping.
#[test]
fn a_negation_binds_tighter_than_a_union() {
    let out = ship(
        "local a: ~number | string = 1\nlocal b: ~(number | string) = true\nlocal c: ~nil & ~string = 1\nprint(a, b, c)\n",
    );
    assert!(out.contains("a: __neg<number> | string"), "{out}");
    assert!(out.contains("b: __neg<(number | string)>"), "{out}");
    assert!(out.contains("c: __neg<nil> & __neg<string>"), "{out}");
}

/// A bound takes the parameter's intersection, as a trait bound does.
#[test]
fn a_negation_bound_intersects_the_parameter() {
    let out = ship("local function g<T: ~nil>(x: T): T\n    return x\nend\nprint(g(1))\n");
    assert!(out.contains("(x: (T & __neg<nil>)): T"), "{out}");
    assert!(out.starts_with("type function __neg"), "{out}");
}

/// A file with no negation declares nothing.
/// A generic default is a type, so its negation lowers.
#[test]
fn a_negation_in_a_generic_default_lowers() {
    let out =
        ship("type D<T = ~nil, U... = ...string> = { v: T }\nlocal d: D = { v = 1 }\nprint(d)\n");
    assert!(
        out.contains("type D<T = __neg<nil>, U... = ...string> = { v: T }"),
        "{out}"
    );
}

#[test]
fn a_file_without_negation_declares_no_function() {
    assert!(!ship("local a: number = 1\nprint(a ~= 2)\n").contains("__neg"));
}

#[test]
fn the_negations_luau_cannot_build_report() {
    assert_eq!(
        messages("local n: ~~number = 1\nprint(n)\n"),
        vec!["`~~number` negates twice; write `number`"]
    );
    assert_eq!(
        messages("local n: ~~~nil = 1\nprint(n)\n"),
        vec!["`~~~nil` negates twice; write `~nil`"]
    );

    let cannot = |text: &str, kind: &str| {
        format!(
            "`~{text}` negates a {kind} type, which Luau cannot negate; negate a primitive, a singleton, a class, or a union of them"
        )
    };
    assert_eq!(
        messages("local n: ~{ x: number } = 1\nprint(n)\n"),
        vec![cannot("{ x: number }", "table")]
    );
    assert_eq!(
        messages("local n: ~number[] = 1\nprint(n)\n"),
        vec![cannot("number[]", "table")]
    );
    assert_eq!(
        messages("local n: ~() -> () = 1\nprint(n)\n"),
        vec![cannot("() -> ()", "function")]
    );
    assert_eq!(
        messages("struct P as\n    x: number\nend\nlocal n: ~P = 1\nprint(n)\n"),
        vec![cannot("P", "table")]
    );
    assert_eq!(
        messages("local function g<T: ~~nil>(x: T): T\n    return x\nend\nprint(g(1))\n"),
        vec!["`~~nil` negates twice; write `nil`"]
    );
}

/// A union member or an alias that is a table reports too, with the
/// operand the author wrote.
#[test]
fn a_table_behind_a_union_or_an_alias_reports() {
    let cannot = |text: &str, kind: &str| {
        format!(
            "`~{text}` negates a {kind} type, which Luau cannot negate; negate a primitive, a singleton, a class, or a union of them"
        )
    };
    let rec = "type Rec = { x: number }\ntype Fn = () -> ()\n";

    assert_eq!(
        messages(&format!("{rec}local n: ~(number | Rec) = 1\nprint(n)\n")),
        vec![cannot("(number | Rec)", "table")]
    );
    assert_eq!(
        messages(&format!("{rec}local n: ~Rec = 1\nprint(n)\n")),
        vec![cannot("Rec", "table")]
    );
    assert_eq!(
        messages(&format!("{rec}local n: ~(string | Fn) = 1\nprint(n)\n")),
        vec![cannot("(string | Fn)", "function")]
    );
    assert!(
        messages(&format!(
            "{rec}local n: ~(number | string) = true\nprint(n)\n"
        ))
        .is_empty()
    );
}

/// Luau reads `--!strict` only above the first token, so the header
/// goes below a plain comment too.
#[test]
fn the_header_goes_below_every_leading_comment() {
    let out = ship("-- header\n--!strict\nlocal b: ~number = \"five\"\nprint(b)\n");
    let lines: Vec<&str> = out.lines().collect();

    assert_eq!(&lines[..2], ["-- header", "--!strict"], "{out}");
    assert!(lines[2].starts_with("type function __neg"), "{out}");
}

/// A layout packs what a type names, and a negation names what a value
/// is not.
#[test]
fn a_wire_type_takes_no_negation() {
    assert_eq!(
        messages("remote Ping(v: ~nil) from client\n"),
        vec![
            "a remote packs no negation: parameter `v` has type `~nil`; name the types it carries"
        ]
    );
    assert_eq!(
        messages("@derive(Serialize)\nstruct P as\n    id: ~nil\nend\n"),
        vec![
            "a struct that derives Serialize writes each field's type: `id` has type `~nil`, a negation; name the types it holds, or mark it @skip"
        ]
    );
    // An alias that negates is a negation too.
    assert_eq!(
        messages("type NotNil = ~nil\nremote Ping(v: NotNil) from client\n"),
        vec![
            "a remote packs no negation: parameter `v` has type `NotNil`; name the types it carries"
        ]
    );
    // A field the serializer skips may hold one.
    assert!(messages("@derive(Serialize)\nstruct P as\n    @skip\n    id: ~nil\nend\n").is_empty());
}

/// The analyzer enforces the negation in strict mode, and its report
/// names the type the author wrote.
#[test]
fn the_checker_enforces_a_negation() {
    use std::fs;

    let dir = std::env::temp_dir().join(format!("alloy-negation-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "--!strict\nlocal a: ~number = \"text\"\nlocal b: ~number = 5\nlocal function g<T: ~nil>(x: T): T\n    return x\nend\nprint(a, b, g(1), g(nil))\n",
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
        .map(|d| format!("{}: {}", d.line, alloy::shapes::friendly_text(&d.message)))
        .collect();

    assert_eq!(
        errors,
        vec![
            "3: `number` does not fit: the type asks for `~number`, so it takes anything but `number`",
            "7: `nil` does not fit: the type asks for `~nil`, so it takes anything but `nil`",
        ],
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A table inside a negated union errors in the type function. Luau
/// crashes on it otherwise, and the crash drops every check of the file.
#[test]
fn a_table_in_a_negated_union_keeps_the_file_checked() {
    use std::fs;

    let dir = std::env::temp_dir().join(format!("alloy-negation-union-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "--!strict\nlocal r = { x = 1 }\nlocal a: ~(string | typeof(r)) = 1\nlocal z: number = \"not a number\"\nprint(a, z)\n",
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
    let lines: Vec<usize> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.line)
        .collect();

    assert!(lines.contains(&3), "{:?}", analysis.diagnostics);
    assert!(lines.contains(&4), "{:?}", analysis.diagnostics);

    let _ = fs::remove_dir_all(&dir);
}
