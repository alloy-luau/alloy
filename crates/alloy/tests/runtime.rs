//! The shipped runtime, held to what a reader cannot see by reading it.

/// `allowed` closes over the rate limiter's state. Declared after that
/// function, the two names are globals inside it: the bucket lookup
/// indexes nil on the first rate-limited call, and the handler that
/// `on_ratelimited` sets never fires.
#[test]
fn the_rate_limiter_declares_its_state_before_it_reads_it() {
    for name in ["buckets", "on_limited"] {
        let first = alloy::RUNTIME
            .find(name)
            .unwrap_or_else(|| panic!("the runtime names `{name}`"));
        let declared = alloy::RUNTIME
            .find(&format!("local {name} "))
            .unwrap_or_else(|| panic!("the runtime declares `{name}`"));

        assert_eq!(
            first,
            declared + "local ".len(),
            "`{name}` is read before its `local`"
        );
    }
}

/// `new Set(list)` fills the set, as `Set.from(list)` does. `Set.new`
/// took no argument, so the list went nowhere, the set came out empty,
/// and neither the build nor the checker said so. `HashMap`, `Array`,
/// and `Queue` had the same trap.
#[test]
fn a_new_collection_takes_the_values_it_is_given() {
    let src = "import { Array, HashMap, Queue, Set } from \"@alloy/std/collections\"\nexport function probe(): (number, number?, number, number?)\n    local s = new Set([ \"a\", \"b\", \"a\" ])\n    local m = new HashMap({ a = 1 })\n    local xs = new Array([ 1, 2 ])\n    local q = new Queue([ 3, 4 ])\n    return s:len(), m:get(\"a\"), xs:len(), q:pop()\nend\n";
    let out = alloy::compile(src).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    let lua = mlua::Lua::new();
    let runtime: mlua::Table = lua.load(alloy::RUNTIME).eval().unwrap();
    let require = lua
        .create_function(move |_, _: String| Ok(runtime.clone()))
        .unwrap();
    lua.globals().set("require", require).unwrap();
    let exports: mlua::Table = lua.load(out.ship.as_str()).eval().unwrap();
    let probe: mlua::Function = exports.get("probe").unwrap();
    let got: (i64, Option<i64>, i64, Option<i64>) = probe.call(()).unwrap();

    assert_eq!(got, (2, Some(1), 2, Some(3)), "{}", out.ship);
}
