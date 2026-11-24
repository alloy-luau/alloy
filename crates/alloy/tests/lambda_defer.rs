//! The lambda and `defer`: what each one emits under its flag, what
//! the output prints when it runs, and what the source reports.

use std::fs;
use std::process::Command;

use alloy::EmitOptions;

const ON: &str = "AlloyExperimentalLambdaSyntax = true\nAlloyExperimentalDeferSyntax = true\n";

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

/// Runs the ship artifact with `luau` and returns what it prints.
/// `None` when `luau` is not installed.
fn run(name: &str, ship: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!("alloy-defer-{name}-{}", std::process::id()));
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

#[test]
fn a_lambda_is_a_function_of_one_return() {
    let src = "local double = |x: number| x * 2\nlocal zero = || 0\nlocal rest = |...: number| select(\"#\", ...)\nlocal add = |a, b|\n    a + b\nprint(double(2), zero(), rest(1, 2), add(1, 2))\n";
    let out = compiled(src, ON);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert!(
        out.ship
            .contains("local double = function(x: number) return x * 2 end"),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains("local zero = function() return 0 end"),
        "{}",
        out.ship
    );
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );

    if let Some(printed) = run("lambda", &out.ship) {
        assert_eq!(printed, "4\t0\t2\t3\n");
    }
}

#[test]
fn a_lambda_stands_where_an_expression_stands() {
    let src = "local xs = { 3, 1, 2 }\ntable.sort(xs, |a, b| a > b)\nlocal curry = |a: number| |b: number| a + b\nlocal pair = |a: number| { a, a }\nlocal union = |v: (number | string)| tostring(v)\nprint(xs[1], curry(1)(2), pair(4)[2], union(5), (|| 9)())\n";
    let out = compiled(src, ON);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert!(
        out.ship
            .contains("function(a: number) return function(b: number) return a + b end end"),
        "{}",
        out.ship
    );

    if let Some(printed) = run("lambda-places", &out.ship) {
        assert_eq!(printed, "3\t3\t4\t5\t9\n");
    }
}

#[test]
fn a_bar_in_a_type_stays_a_union() {
    let out = compiled(
        "type Id = number | string\nlocal id: number | string = 1\nprint(id)\n",
        "",
    );

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert!(!out.ship.contains("function"), "{}", out.ship);
}

#[test]
fn a_defer_runs_at_each_way_out_last_to_first() {
    let src = "local function work(n: number) -> number\n    defer print(\"first\")\n    defer do\n        print(\"second\")\n    end\n\n    if n > 3 then\n        return n * 2\n    end\n\n    return n\nend\n\nlocal function fails()\n    defer print(\"cleaned\")\n    error(\"boom\", 0)\nend\n\nlocal function rest(...: number) -> number\n    defer print(\"rest\")\n    return select(\"#\", ...)\nend\n\nprint(work(5))\nprint(work(1))\nprint(pcall(fails))\nprint(rest(1, 2, 3))\n";
    let out = compiled(src, ON);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("local __defers = {} return __alloy.deferred(__defers, function(): number"),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("__defers[#__defers + 1] = function() print(\"first\") end"),
        "{}",
        out.ship
    );

    if let Some(printed) = run("defer", &out.ship) {
        assert_eq!(
            printed,
            "second\nfirst\n10\nsecond\nfirst\n1\ncleaned\nfalse\tboom\nrest\n3\n"
        );
    }
}

#[test]
fn a_defer_stands_in_a_method_an_async_function_and_a_generator() {
    let src = "struct Door\n    name: string\nend\n\nimpl Door\n    function shut(self, hard: boolean = false) -> string\n        defer print(\"shut\", self.name, hard)\n        return self.name\n    end\nend\n\nasync function later(n: number) -> number\n    defer print(\"later\")\n    return n + 1\nend\n\nprint(new Door { name = \"d\" }:shut())\nprint(later(1):expect())\n";
    let flags = format!("{ON}AlloyExperimentalGeneratorSyntax = true\n");
    let out = compiled(src, &flags);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );

    let generator = compiled(
        "function* ids()\n    defer print(\"done\")\n    yield 1\nend\nfor id in ids() do print(id) end\n",
        &flags,
    );
    assert!(
        messages(&generator).is_empty(),
        "{:?}",
        messages(&generator)
    );

    if let Some(printed) = run("defer-generator", &generator.ship) {
        assert_eq!(printed, "1\ndone\n");
    }
}

#[test]
fn each_form_with_its_flag_off_names_the_flag() {
    let out = compiled(
        "local function f()\n    defer print(1)\nend\nlocal g = |x| x\nprint(f, g)\n",
        "",
    );
    let found = messages(&out);

    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].contains("AlloyExperimentalDeferSyntax"),
        "{found:?}"
    );
    assert!(
        found[1].contains("AlloyExperimentalLambdaSyntax"),
        "{found:?}"
    );
}

/// A spawner runs the literal it takes on a thread of its own, so the
/// walk gives that body another spot. The flag check holds there too.
#[test]
fn a_literal_a_spawner_takes_needs_its_flag_too() {
    for (src, flag) in [
        (
            "o:Connect(|dt| print(dt))\n",
            "AlloyExperimentalLambdaSyntax",
        ),
        ("o:Once(|dt| print(dt))\n", "AlloyExperimentalLambdaSyntax"),
        (
            "s:connect(|dt| print(dt))\n",
            "AlloyExperimentalLambdaSyntax",
        ),
        ("task.spawn(|| print(1))\n", "AlloyExperimentalLambdaSyntax"),
        (
            "task.delay(1, || print(1))\n",
            "AlloyExperimentalLambdaSyntax",
        ),
        ("o:m(|dt| print(dt))\n", "AlloyExperimentalLambdaSyntax"),
        (
            "task.spawn(function*()\n    yield 1\nend)\n",
            "AlloyExperimentalGeneratorSyntax",
        ),
        (
            "o:Connect(function*()\n    yield 1\nend)\n",
            "AlloyExperimentalGeneratorSyntax",
        ),
    ] {
        let off = compiled(src, "");
        let found = messages(&off);
        assert!(
            found.len() == 1 && found[0].contains(flag),
            "{src}: {found:?}"
        );
    }

    let on = compiled("o:Connect(|dt| print(dt))\n", ON);
    assert!(on.diagnostics.is_empty(), "{:?}", messages(&on));
}

#[test]
fn a_defer_needs_a_function_and_takes_no_defer() {
    let out = compiled("defer print(1)\n", ON);
    assert!(
        messages(&out)[0].starts_with("`defer` runs when its function ends"),
        "{:?}",
        messages(&out)
    );
    // The output still parses: the block runs in place.
    assert!(out.ship.contains("do print(1) end"), "{}", out.ship);

    let options = EmitOptions {
        fflags: toml::from_str(ON).unwrap(),
        ..EmitOptions::default()
    };
    let nested = alloy::compile_with(
        "local function f()\n    defer do\n        defer print(1)\n    end\nend\n",
        &options,
    );
    let report = match nested {
        Ok(out) => messages(&out).join("\n"),

        Err(e) => e.message,
    };
    assert!(report.contains("a `defer` inside a `defer`"), "{report}");
}

#[test]
fn a_name_called_defer_stays_a_name() {
    let src = "local defer = 1\ndefer = 2\nprint(defer)\ntask.defer(print, defer)\nlocal function f(defer)\n    defer(1)\nend\nprint(f)\n";
    let out = compiled(src, ON);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert_eq!(
        out.ship.trim_end().trim_end_matches(" return nil"),
        src.trim_end()
    );
}

#[test]
fn the_formatter_keeps_both_forms_as_written() {
    let src = "local function work(n: number) -> number\n  defer print('first')\n  defer do\n    print('second')\n  end\n\n  return n\nend\n\nconst double = |x: number| x * 2\nconst zero = || 0\nconst sorted = sort(xs, |a, b| a > b)\nprint(work, double, zero, sorted)\n";
    assert_eq!(alloy::fmt::format(src).unwrap(), src);

    let spaced =
        alloy::fmt::format("const f = | x , y:number |   x+y\nconst g = | |  1\nprint(f, g)\n")
            .unwrap();
    assert_eq!(
        spaced,
        "const f = |x, y: number| x + y\nconst g = || 1\nprint(f, g)\n"
    );
    assert_eq!(alloy::fmt::format(&spaced).unwrap(), spaced);
}
