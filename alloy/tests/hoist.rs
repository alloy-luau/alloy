//! A struct, an enum, or a namespace a function reads above its
//! declaration: the first line opens the table, the declaration fills
//! it, and the line count holds. A use that runs at the top level
//! before the declaration reports instead.

fn compile(src: &str) -> alloy::Output {
    alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap()
}

const DECLS: &str = "struct Point as\n    x: number\nend\n\nenum Kind as\n    A\n    B\nend\n\nnamespace Geo as\n    function two(): number\n        return 2\n    end\nend\n";

#[test]
fn a_function_body_reads_a_table_declared_below_it() {
    let src = format!(
        "function make(): Point\n    return new Point {{ x = 1 }}\nend\n\nfunction kind(): Kind\n    return Kind.A\nend\n\nlocal two = function(): number\n    return Geo.two()\nend\n\nlocal typed: Point? = nil\n\n{DECLS}print(make(), kind(), two(), typed)\n"
    );
    let out = compile(&src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    for text in [&out.ship, &out.check] {
        let first = text.lines().next().unwrap();
        assert!(
            first.contains("local Point, Kind, Geo = {}, {}, {} function make(): Point"),
            "{text}"
        );
        assert!(!text.contains("local Point = {}"), "{text}");
        assert!(!text.contains("local Kind = {}"), "{text}");
        assert!(!text.contains("local Geo = {}"), "{text}");
        assert!(text.contains("\nPoint.__index = Point"), "{text}");
        assert!(text.contains("\nKind.__index = Kind"), "{text}");
        assert_eq!(text.lines().count(), src.lines().count(), "{text}");
    }
}

#[test]
fn a_table_declared_below_a_body_that_never_reads_it_stays_local() {
    let src = format!("function one(): number\n    return 1\nend\n\n{DECLS}print(one())\n");
    let out = compile(&src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.ship.contains("local Point = {}"), "{}", out.ship);
    assert!(out.ship.contains("local Kind = {}"), "{}", out.ship);
    assert!(out.ship.contains("local Geo = {}"), "{}", out.ship);
}

/// A macro body expands at each call, so the call is where the use
/// runs: inside a function it hoists, at the top level it reports on
/// the call line, not on the macro body.
#[test]
fn a_macro_body_reads_the_table_at_each_call() {
    const MACRO: &str = "macro makeToken(n)\n    new Token { id = n }\nend\n\nstruct Token as\n    id: number\nend\n";
    let inside = format!(
        "function build(): Token\n    return $makeToken(5)\nend\n\n{MACRO}print(build())\n"
    );
    let out = compile(&inside);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains("local Token = {} function build"),
        "{}",
        out.ship
    );
    assert!(!out.ship.contains("\nlocal Token = {}"), "{}", out.ship);

    let top = format!("local t = $makeToken(5)\n\n{MACRO}print(t)\n");
    let out = compile(&top);
    let hits: Vec<(usize, String)> = out
        .diagnostics
        .iter()
        .map(|d| {
            (
                top[..d.start as usize].matches('\n').count() + 1,
                d.message.clone(),
            )
        })
        .collect();
    assert_eq!(
        hits,
        vec![(
            1,
            "`Token` is declared below this use; move the struct above it".to_string()
        )]
    );
}

#[test]
fn a_top_level_use_above_the_declaration_reports() {
    let src = format!(
        "local early = new Point {{ x = 2 }}\nlocal k = Kind.B\nlocal n = Geo.two()\n\n{DECLS}print(early, k, n)\n"
    );
    let messages: Vec<String> = compile(&src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        vec![
            "`Point` is declared below this use; move the struct above it",
            "`Kind` is declared below this use; move the enum above it",
            "`Geo` is declared below this use; move the namespace above it",
        ]
    );
}
