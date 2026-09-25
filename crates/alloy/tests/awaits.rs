//! Where an `await` may stand.
//!
//! `await` lowers to a `coroutine.yield` on the running thread, so the
//! rule is about which thread that is. An `async function` body, an
//! `async do` block and a Roblox Script own their thread and may yield.
//! A module's top level runs inside `require`, and a plain function
//! yields into whatever called it, which for a C caller such as
//! `table.sort` is a crash on Roblox.

use alloy::EmitOptions;

/// The diagnostics of one source, compiled under `name`. The name
/// decides the file kind: `.server` and `.client` are scripts.
fn messages(name: &str, src: &str) -> Vec<String> {
    let options = EmitOptions {
        file_name: name.to_string(),
        ..EmitOptions::default()
    };

    alloy::compile_with(src, &options)
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

fn clean(name: &str, src: &str) {
    let got = messages(name, src);

    assert!(got.is_empty(), "{name}: {got:?}");
}

fn only(name: &str, src: &str) -> String {
    let got = messages(name, src);

    assert_eq!(got.len(), 1, "{name}: {got:?}");

    got.into_iter().next().unwrap()
}

const PLAIN_FIX: &str = "or wrap the body in `async do ... end`";
const MODULE_FIX: &str =
    "`await` at a module's top level yields inside `require`; wrap it in `async do ... end`";

/// An `async function` body owns its thread.
#[test]
fn an_async_function_body_takes_an_await() {
    clean(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal async function use_it()\n    print(await load())\nend\nprint(use_it)\n",
    );
}

/// An `async do` block owns its thread, at a module's top level and
/// inside a plain function alike.
#[test]
fn an_async_block_takes_an_await() {
    clean(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nasync do\n    print(await load())\nend\nprint(load)\n",
    );
    clean(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal function plain()\n    async do\n        print(await load())\n    end\nend\nprint(plain)\n",
    );
}

/// A `.server` or `.client` file is a Script: Roblox runs it on a
/// thread of its own, so its top level may yield.
#[test]
fn a_script_top_level_takes_an_await() {
    let src = "local async function load(): number\n    return 1\nend\n\nlocal v = await load()\nprint(v)\n";
    clean("main.server.aly", src);
    clean("hud.client.aly", src);
    clean("src/ui/hud.client.aly", src);
}

/// A module's top level runs inside `require`.
#[test]
fn a_module_top_level_reports() {
    let src = "local async function load(): number\n    return 1\nend\n\nlocal v = await load()\nprint(v)\n";
    assert_eq!(only("m.aly", src), MODULE_FIX);
    assert_eq!(only("src/data/store.aly", src), MODULE_FIX);
}

/// A plain function yields into its caller, and the report names the
/// function so the fix has a target.
#[test]
fn a_plain_function_reports_under_its_own_name() {
    let got = only(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal function greet()\n    print(await load())\nend\nprint(greet)\n",
    );
    assert_eq!(
        got,
        "`await` needs an async context; mark `greet` as `async`, or wrap the body in `async do ... end`"
    );

    // A method of an `impl` carries the name it was declared with.
    let got = only(
        "m.aly",
        "struct S as\n    n: number\nend\n\nlocal async function load(): number\n    return 1\nend\n\nimpl S as\n    function fill(self)\n        self.n = await load()\n    end\nend\nprint(S)\n",
    );
    assert!(got.contains("mark `fill` as `async`"), "{got}");
}

/// A function boundary resets the context: an ordinary `function`
/// inside an `async function` is not an async context. This is the
/// case that crashes on Roblox.
#[test]
fn a_plain_callback_inside_an_async_function_reports() {
    let got = only(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal async function outer()\n    local run = function()\n        print(await load())\n    end\n    run()\nend\nprint(outer)\n",
    );
    assert!(got.contains(PLAIN_FIX), "{got}");
    assert!(got.contains("mark the function `async`"), "{got}");
}

/// The crash this rule prevents: a `table.sort` comparator that awaits
/// yields across a C-call boundary, which Roblox kills. It builds with
/// no diagnostic off Roblox, because the std runs a Future to
/// completion at once when `task` is missing.
#[test]
fn a_sort_comparator_that_awaits_reports() {
    // Both awaits in the comparator report, one each.
    let got = messages(
        "m.aly",
        "local async function rank(n: number): number\n    return n\nend\n\nlocal async function order(xs: number[])\n    xs:sort(function(a, b)\n        return await rank(a) < await rank(b)\n    end)\nend\nprint(order)\n",
    );
    assert_eq!(
        got,
        vec![
            "`await` needs an async context; mark the function `async`, or wrap the body in `async do ... end`".to_string(),
            "`await` needs an async context; mark the function `async`, or wrap the body in `async do ... end`".to_string(),
        ]
    );

    // The same comparator through `table.sort`.
    let got = only(
        "m.aly",
        "local async function rank(n: number): number\n    return n\nend\n\nlocal async function order(xs: { number })\n    table.sort(xs, function(a, b)\n        return await rank(a) < b\n    end)\nend\nprint(order)\n",
    );
    assert!(got.contains("`await` needs an async context"), "{got}");
}

/// An `async function` expression as a callback is an async context.
#[test]
fn an_async_callback_takes_an_await() {
    clean(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal async function outer()\n    local run = async function(): number\n        return await load()\n    end\n    print(await run())\nend\nprint(outer)\n",
    );
}

/// `try await` follows the same rule as `await`.
#[test]
fn try_await_follows_the_same_rule() {
    let got = only(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal function fetch(): Result<number, string>\n    local v = try await load()\n    return Ok(v)\nend\nprint(fetch)\n",
    );
    assert!(got.contains("mark `fetch` as `async`"), "{got}");

    clean(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal async function fetch(): Result<number, string>\n    local v = try await load()\n    return Ok(v)\nend\nprint(fetch)\n",
    );
    clean(
        "main.server.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal r = try await load()\nprint(r)\n",
    );
}

/// An `after` block is `task.delay` over a closure, so it runs on a
/// thread of its own and may yield.
#[test]
fn an_after_block_takes_an_await() {
    clean(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nafter 1 do\n    print(await load())\nend\nprint(load)\n",
    );
}

/// A markup file follows its own name: `card.alx` is a module and
/// `hud.client.alx` is a script.
#[test]
fn an_alx_file_follows_its_name() {
    let src = "local async function load(): number\n    return 1\nend\n\nlocal v = await load()\nprint(v)\n";
    assert_eq!(only("card.alx", src), MODULE_FIX);
    clean("hud.client.alx", src);
}

/// The report carries the kind and the book section the rest of the
/// async diagnostics carry.
#[test]
fn the_report_is_an_async_error_of_section_3_3() {
    let got = only(
        "m.aly",
        "local async function load(): number\n    return 1\nend\n\nlocal v = await load()\nprint(v)\n",
    );
    assert_eq!(alloy::docs::kind_for(&got), "AsyncError");
    assert_eq!(alloy::docs::code_for(&got), Some("3.3"));
}

/// `await` of a call to a plain function takes a value, not a Future:
/// the runtime raises `cannot await a number`. The compiler names the
/// function, and a plain function that returns a `Future` still awaits.
#[test]
fn an_await_of_a_plain_function_names_it() {
    let got = only(
        "m.aly",
        "local function delayed(): number\n    return 7\nend\n\nasync function go()\n    local v = await delayed()\n    print(v)\nend\nprint(go)\n",
    );

    assert_eq!(got, "`delayed` is not async; `await` takes a Future");

    clean(
        "m.aly",
        "local function later(): Future<number>\n    return Future.resolve(7)\nend\n\nasync function go()\n    print(await later())\nend\nprint(go)\n",
    );
}

/// `await (x)` with a space is the word; `await(x)` that touches is a
/// call of a Luau function of that name.
#[test]
fn an_await_before_a_spaced_paren_is_the_word() {
    let options = EmitOptions {
        file_name: "paren.aly".to_string(),
        ..EmitOptions::default()
    };
    let src = "async function slow(): Future<number>\n    return 7\nend\nasync function main()\n    local a = await (slow())\n    print(a)\nend\n";
    let out = alloy::compile_with(src, &options).unwrap();
    assert!(out.ship.contains("__alloy.await((slow()))"), "{}", out.ship);

    let luau = "local await = function(x) return x end\nprint(await(1))\n";
    let out = alloy::compile_with(luau, &options).unwrap();
    assert!(out.ship.contains("print(await(1))"), "{}", out.ship);
}

/// A function written in a spawner's call runs on a thread the spawner
/// starts for it, so it yields safely. A function it calls does not,
/// and neither does a comparator, which C code calls.
#[test]
fn a_spawned_function_may_await() {
    let head = "local function f(): Future<number>\n    return async do return 1 end\nend\n";

    for call in [
        "game:GetService(\"Players\").PlayerAdded:Connect(function(p)\n    print(await f(), p)\nend)\n",
        "workspace.ChildAdded:Once(function()\n    print(await f())\nend)\n",
        "task.spawn(function()\n    print(await f())\nend)\n",
        "task.defer(function()\n    print(await f())\nend)\n",
        "task.delay(1, function()\n    print(await f())\nend)\n",
    ] {
        clean("m.aly", &format!("{head}{call}"));
    }

    only(
        "m.aly",
        &format!(
            "{head}table.sort({{ 2, 1 }}, function(a, b)\n    print(await f())\n    return a < b\nend)\n"
        ),
    );
    only(
        "m.aly",
        &format!(
            "{head}task.spawn(function()\n    local function inner()\n        print(await f())\n    end\n    inner()\nend)\n"
        ),
    );
}
