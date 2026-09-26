//! `@cfg`: a condition on a function, a local, or a statement, read
//! when the code runs.

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
fn an_exported_local_keeps_its_guard() {
    let out =
        compile("@cfg(server)\nexport const a = os.clock()\n@cfg(client)\nexport local b = 1\n");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains(
            "const a = (if __alloy.cfg.server() then os.clock() else nil) :: typeof(os.clock())"
        ),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("local b = (if __alloy.cfg.client() then 1 else nil) :: typeof(1)"),
        "{}",
        out.ship
    );
    assert_eq!(out.ship.lines().count(), 4);
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

#[test]
fn a_function_with_nothing_to_give_returns_on_the_other_side() {
    // A shared module calls both halves of a bootstrap: each one runs
    // on its own side and does nothing on the other.
    let out = compile("@cfg(server)\nfunction start()\n    print(1)\nend\n");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship
            .contains("if not (__alloy.cfg.server()) then return end print(1)"),
        "{}",
        out.ship
    );

    // A declared `()` says the same thing as no return type.
    let out = compile("@cfg(server)\nfunction start(): ()\n    print(1)\nend\n");
    assert!(
        out.ship
            .contains("if not (__alloy.cfg.server()) then return end"),
        "{}",
        out.ship
    );

    // An `async` function with no declared type resolves its future
    // with nil, so it skips too.
    let out = compile("@cfg(server)\nlocal async function start()\n    print(1)\nend\n");
    assert!(
        out.ship
            .contains("if not (__alloy.cfg.server()) then return end"),
        "{}",
        out.ship
    );
}

#[test]
fn a_function_that_owes_a_value_still_raises() {
    // The declared type says the caller gets a value.
    let out = compile("@cfg(server)\nfunction key(): string\n    return \"k\"\nend\n");
    assert!(
        out.ship
            .contains("error(\"`key` is @cfg(server) and cannot run here\", 2)"),
        "{}",
        out.ship
    );

    // With no declared type the body decides: a `return` with a value
    // owes the caller one.
    let out = compile("@cfg(server)\nfunction key()\n    return 1\nend\n");
    assert!(out.ship.contains("error("), "{}", out.ship);

    // An optional is a value too: nil where the wrong side calls would
    // read as an honest answer.
    let out = compile("@cfg(server)\nfunction key(): string?\n    return nil\nend\n");
    assert!(out.ship.contains("error("), "{}", out.ship);

    // A `return` inside a nested function is the nested function's.
    let out = compile(
        "@cfg(server)\nfunction start()\n    local f = function()\n        return 1\n    end\n    print(f)\nend\n",
    );
    assert!(
        out.ship
            .contains("if not (__alloy.cfg.server()) then return end"),
        "{}",
        out.ship
    );
}

#[test]
fn a_statement_under_the_attribute_is_skipped() {
    let out = compile("@cfg(server)\nstart()\n");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship
            .contains("if __alloy.cfg.server() then start() end"),
        "{}",
        out.ship
    );
    assert_eq!(out.ship.lines().count(), 2);

    // The guard sits on the statement's own lines, whatever the
    // statement is.
    let cases = [
        (
            "@cfg(server)\ntotal = total + 1\n",
            "if __alloy.cfg.server() then total = total + 1 end",
        ),
        (
            "@cfg(server)\nreturn 1\n",
            "if __alloy.cfg.server() then return 1 end",
        ),
        (
            "@cfg(server)\ndo\n    go()\nend\n",
            "if __alloy.cfg.server() then do",
        ),
        (
            "@cfg(server)\nif ready then\n    go()\nend\n",
            "if __alloy.cfg.server() then if ready then",
        ),
        (
            "@cfg(server)\nwhile ready do\n    go()\nend\n",
            "if __alloy.cfg.server() then while ready do",
        ),
        (
            "@cfg(server)\nfor i = 1, 3 do\n    go(i)\nend\n",
            "if __alloy.cfg.server() then for i = 1, 3 do",
        ),
    ];

    for (src, want) in cases {
        let out = compile(src);
        assert!(out.diagnostics.is_empty(), "{src}: {:?}", out.diagnostics);
        assert!(out.ship.contains(want), "{src}\n{}", out.ship);
        assert_eq!(out.ship.lines().count(), src.lines().count(), "{src}");
    }
}

#[test]
fn two_attributes_on_one_statement_join() {
    let out = compile("@cfg(server)\n@cfg(not studio)\ngo()\n");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship
            .contains("if __alloy.cfg.server() and not __alloy.cfg.studio() then go() end"),
        "{}",
        out.ship
    );

    // Two statements in a row each take their own guard.
    let out = compile("@cfg(server)\ngo()\n@cfg(client)\nstop()\n");
    assert!(
        out.ship.contains("if __alloy.cfg.server() then go() end"),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains("if __alloy.cfg.client() then stop() end"),
        "{}",
        out.ship
    );
}

#[test]
fn another_attribute_on_a_statement_reports() {
    let m = messages("@derive(Clone)\ngo()\n");
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(
        m[0],
        "`@derive` goes on a declaration; `@cfg` and `@allow` are the attributes a statement takes"
    );

    // An unknown condition word reads the same way it reads on a
    // declaration.
    let m = messages("@cfg(mobile)\ngo()\n");
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(
        m[0].starts_with("`@cfg(mobile)`: the conditions are server, client, studio"),
        "{m:?}"
    );
}
