//! `@cfg`: a condition on a function or a local, read when the code
//! runs.

fn compile(src: &str) -> alloy::Output {
    alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap()
}

fn messages(src: &str) -> Vec<String> {
    compile(src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn a_function_opens_with_the_check() {
    let out = compile("@cfg(server)\nfunction save()\n    return 1\nend\n");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains("function save()\n    if not (__alloy.cfg.server()) then error(\"`save` is @cfg(server) and cannot run here\", 2) end return 1\nend"),
        "{}",
        out.ship
    );
    assert_eq!(out.ship.lines().count(), 4);
}

#[test]
fn a_local_keeps_the_type_of_its_value() {
    let out = compile("@cfg(client)\nconst n = 1 + 2\nprint(n)\n");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship
            .contains("(if __alloy.cfg.client() then 1 + 2 else nil) :: typeof(1 + 2)"),
        "{}",
        out.ship
    );
    assert_eq!(out.ship.lines().count(), 3);
}

#[test]
fn the_conditions_join() {
    let out = compile("@cfg(any(studio, test) and not server)\nfunction f() end\n");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains(
            "if not (((__alloy.cfg.studio() or __alloy.cfg.test()) and not __alloy.cfg.server())) then"
        ),
        "{}",
        out.ship
    );
}

#[test]
fn a_wrong_condition_is_named() {
    let m = messages("@cfg(mobile)\nfunction f() end\n");
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(
        m[0].starts_with("`@cfg(mobile)`: the conditions are server, client, studio"),
        "{m:?}"
    );

    let m = messages("@cfg(server, client)\nfunction f() end\n");
    assert!(m[0].starts_with("`@cfg` takes one condition"), "{m:?}");

    let m = messages("@cfg(server)\nlocal a, b = 1, 2\n");
    assert!(
        m[0].starts_with("`@cfg` goes on a local with one name"),
        "{m:?}"
    );
}
