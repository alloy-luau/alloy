//! Generators, `function*` and `yield`: what each artifact holds under
//! its flags, what the ship artifact gives when it runs, and what the
//! source reports.

use std::fs;
use std::process::Command;

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

/// The 1-based line a diagnostic starts on.
fn line_of(src: &str, d: &alloy::Diagnostic) -> usize {
    src[..d.start as usize].matches('\n').count() + 1
}

/// Runs the ship artifact with `luau` and returns what it prints.
/// `None` when `luau` is not installed.
fn run(name: &str, ship: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!("alloy-generators-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("alloy.luau"), alloy::RUNTIME).unwrap();
    fs::write(
        dir.join(".luaurc"),
        "{ \"aliases\": { \"alloy\": \"./alloy\" } }\n",
    )
    .unwrap();
    fs::write(dir.join("main.luau"), ship).unwrap();
    let run = Command::new("luau")
        .arg("main.luau")
        .current_dir(&dir)
        .output();
    let _ = fs::remove_dir_all(&dir);

    let Ok(run) = run else {
        eprintln!("skipped: luau is not installed");

        return None;
    };

    Some(String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr))
}

const SYNTAX: &str = "AlloyExperimentalGeneratorSyntax = true\n";
const BOTH: &str =
    "AlloyExperimentalGeneratorSyntax = true\nAlloyExperimentalGeneratorAnalysis = true\n";

const IDS: &str = "function* ids(n: number)\n    for i = 1, n do\n        yield i\n    end\nend\nfor id in ids(3) do print(id) end\nlocal evens = ids(10):filter(function(x) return x % 2 == 0 end):to_table()\nprint(table.concat(evens, \" \"))\n";

#[test]
fn a_generator_wraps_its_body_and_keeps_its_lines() {
    let out = compiled(IDS, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(
        out.ship
            .contains("function ids(n: number) return __alloy.generator(function(__yield)\n"),
        "{}",
        out.ship
    );
    assert!(out.ship.contains("        __yield(i)\n"), "{}", out.ship);
    assert!(out.ship.contains("\nend) end\n"), "{}", out.ship);
    assert_eq!(
        out.ship.lines().count(),
        IDS.lines().count(),
        "{}",
        out.ship
    );
}

#[test]
fn a_generator_gives_its_values_to_a_loop_and_to_the_iter_methods() {
    let out = compiled(IDS, SYNTAX);

    if let Some(printed) = run("ids", &out.ship) {
        assert_eq!(printed, "1\n2\n3\n2 4 6 8 10\n", "{printed}");
    }
}

/// Every position a function stands in takes the star: a local, an
/// expression, a field of a table, a method, and a function with a
/// rest parameter. A `return` alone ends the generator, and the body
/// runs only as far as the reader asks.
#[test]
fn a_generator_stands_where_a_function_stands() {
    let src = r#"local function* words()
    yield "a"
    if true then return end
    yield "b"
end
local squares = function*(n: number)
    for i = 1, n do
        yield (i * i)
    end
end
local Bag = { items = { 5, 6 } }
function* Bag.each(scale: number, ...: number)
    for _, v in Bag.items do yield v * scale end
    for _, v in { ... } do yield -v end
end
function* Bag:own()
    for _, v in self.items do yield v end
end
local steps = 0
local function* forever()
    while true do
        steps += 1
        yield steps
    end
end
local yield = 4
print(yield, table.concat(words():to_table(), ","), table.concat(squares(3):to_table(), ","))
print(table.concat(Bag.each(2, 9):to_table(), ","), table.concat(Bag:own():to_table(), ","))
print(table.concat(forever():take(2):to_table(), ","), steps)
"#;
    let out = compiled(src, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("function(n: number) return __alloy.generator(function(__yield)"),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains(
            "function Bag.each(scale: number, ...: number) return __alloy.generator(function(__yield, ...: number)"
        ),
        "{}",
        out.ship
    );
    assert!(out.ship.contains("end, ...) end"), "{}", out.ship);

    if let Some(printed) = run("positions", &out.ship) {
        assert_eq!(printed, "4\ta\t1,4,9\n10,12,-9\t5,6\n1,2\t2\n", "{printed}");
    }
}

/// The analysis flag changes the check artifact alone. A declared
/// return type gives the `yield` parameter its item type.
#[test]
fn the_analysis_flag_types_the_check_artifact() {
    let src = "local function* ids()\n    yield 1\nend\nlocal function* words(): Iter<string>\n    yield \"a\"\nend\nprint(ids, words)\n";
    let typed = compiled(src, BOTH);
    assert_eq!(messages(&typed), [] as [&str; 0]);
    assert!(
        typed
            .check
            .contains("ids() return __alloy.generator_checked(function(__yield)\n"),
        "{}",
        typed.check
    );
    assert!(
        typed.check.contains(
            "words(): __alloy.Iter<string> return __alloy.generator_checked(function(__yield: (typeof(((nil :: any) :: __alloy.Iter<string>):to_table()[1])) -> ())\n"
        ),
        "{}",
        typed.check
    );
    assert!(!typed.ship.contains("generator_checked"), "{}", typed.ship);
    assert_eq!(typed.check.lines().count(), src.lines().count());

    let untyped = compiled(src, SYNTAX);
    assert!(
        !untyped.check.contains("generator_checked"),
        "{}",
        untyped.check
    );
    assert!(
        untyped
            .check
            .contains("__alloy.generator(function(__yield)"),
        "{}",
        untyped.check
    );
}

#[test]
fn a_generator_with_the_flag_off_names_the_flag() {
    // The analysis flag alone turns nothing on.
    for fflags in ["", "AlloyExperimentalGeneratorAnalysis = true\n"] {
        let out = compiled(IDS, fflags);
        let messages = messages(&out);

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].contains("AlloyExperimentalGeneratorSyntax = true"),
            "{messages:?}"
        );
        assert_eq!(line_of(IDS, &out.diagnostics[0]), 1);
    }
}

/// With no `function*` open, `yield` is the name Luau reads.
#[test]
fn a_yield_outside_a_generator_is_a_name() {
    let src = "local function yield(x: number) return x end\nlocal t = { yield = yield }\nyield(1)\nprint(yield(2), t.yield(3))\n";

    for fflags in ["", SYNTAX] {
        let out = compiled(src, fflags);
        assert_eq!(messages(&out), [] as [&str; 0]);
        assert!(out.ship.starts_with(src.trim_end()), "{}", out.ship);
    }

    let out = compiled("local function f(x)\n    yield x\nend\nprint(f)\n", SYNTAX);
    assert_eq!(
        messages(&out),
        ["`yield` is a statement of a generator; declare the function with `function*`"]
    );
    assert_eq!(
        line_of("local function f(x)\n    yield x\n", &out.diagnostics[0]),
        2
    );
}

/// Inside a generator, a `yield` that an assignment or an index
/// follows is still the name.
#[test]
fn a_local_named_yield_in_a_generator_stays_a_name() {
    let src = "local function* f()\n    local yield = { n = 1 }\n    yield.n = 2\n    yield = { n = 3 }\n    print(yield.n)\nend\nprint(f)\n";
    let out = compiled(src, SYNTAX);
    assert_eq!(messages(&out), [] as [&str; 0]);
    assert!(out.ship.contains("    yield.n = 2\n"), "{}", out.ship);
    assert!(!out.ship.contains("__yield("), "{}", out.ship);
}

#[test]
fn a_misplaced_yield_reports_on_its_line() {
    let src = r#"local function* a()
    local f = function()
        yield 1
    end
    yield f
end
local function* b()
    yield 1, 2
    return 5
end
local function* c()
    yield
end
print(a, b, c)
"#;
    let out = compiled(src, SYNTAX);
    let found: Vec<(usize, &str)> = out
        .diagnostics
        .iter()
        .map(|d| (line_of(src, d), d.message.as_str()))
        .collect();

    assert_eq!(
        found,
        [
            (
                3,
                "`yield` is in a plain function inside a generator; only the body of the `function*` itself yields"
            ),
            (
                8,
                "`yield` takes one value; put the values in a table to yield them together"
            ),
            (
                9,
                "a generator returns no value; `yield` gives a value, and a `return` alone ends the generator"
            ),
            (12, "`yield` takes a value on the same line"),
        ]
    );
}

#[test]
fn a_generator_takes_no_async_and_no_await() {
    let src = "async function* a()\n    yield 1\nend\nlocal function* b()\n    local v = await a()\n    yield v\nend\nprint(b)\n";
    let out = compiled(src, SYNTAX);
    let found: Vec<(usize, &str)> = out
        .diagnostics
        .iter()
        .map(|d| (line_of(src, d), d.message.as_str()))
        .collect();

    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].0, 1);
    assert!(found[0].1.contains("a generator cannot be `async`"));
    assert_eq!(found[1].0, 5);
    assert!(found[1].1.starts_with("`await` in a generator"));
}

#[test]
fn the_formatter_keeps_a_generator_as_written() {
    let src = "function* ids(n: number)\n  for i = 1, n do\n    yield i\n  end\nend\n\nconst squares = function*(n: number)\n  yield -n\n  yield (n + 1) * 2\n  yield { n }\n  yield 'n'\nend\n\nprint(ids, squares)\n";
    let once = alloy::fmt::format(src).unwrap();
    assert_eq!(once, src);

    let spaced = alloy::fmt::format(
        "function *ids()\nyield   1\nend\nconst f = function * () yield 2 end\nprint(ids, f)\n",
    )
    .unwrap();
    assert_eq!(
        spaced,
        "function* ids()\n  yield 1\nend\nconst f = function*() yield 2 end\nprint(ids, f)\n"
    );
    assert_eq!(alloy::fmt::format(&spaced).unwrap(), spaced);

    // A file with no generator keeps `yield` a name.
    let plain = "local function f(yield)\n  yield(1)\n  print(yield - 1)\nend\nprint(f)\n";
    assert_eq!(alloy::fmt::format(plain).unwrap(), plain);
}
