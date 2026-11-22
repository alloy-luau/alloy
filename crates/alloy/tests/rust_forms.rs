//! The forms taken from Rust behind flags: a way out after `??`, the
//! range pattern, `using`, `@must_use`, and a struct over one type.

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

/// Runs the ship artifact with `luau` and returns what it prints.
/// `None` when `luau` is not installed.
fn run(name: &str, ship: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!("alloy-forms-{name}-{}", std::process::id()));
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

const BAIL: &str = "AlloyExperimentalBailSyntax = true\n";

#[test]
fn a_way_out_after_a_coalesce_leaves_on_nil() {
    let src = "local function find(id: number) -> string?\n    return id > 0 and \"a\" or nil\nend\n\nlocal function f(id: number) -> number\n    local name = find(id) ?? return 0\n    const typed: string = find(id) ?? return 1\n    local count = 0\n    for i = -1, 3 do\n        local each = find(i) ?? continue\n        local stop = find(2 - i) ?? break\n        count += #each + #stop\n    end\n    return #name + #typed + count\nend\nprint(f(1), f(0))\n";
    let out = compiled(src, BAIL);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );
    assert!(
        out.ship
            .contains("local name = find(id) if name == nil then return 0 end"),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains("const typed: string = _v1"),
        "{}",
        out.ship
    );

    if let Some(printed) = run("bail", &out.ship) {
        assert_eq!(printed, "4\t0\n");
    }
}

#[test]
fn a_way_out_stands_only_as_the_value_of_a_local() {
    let out = compiled(
        "local function f(a: number?)\n    print(a ?? return)\nend\nprint(f)\n",
        BAIL,
    );
    assert!(
        messages(&out)[0].starts_with("`?? return` stands as the whole value"),
        "{:?}",
        messages(&out)
    );

    let off = compiled(
        "local function f(a: number?)\n    local b = a ?? return\n    print(b)\nend\nprint(f)\n",
        "",
    );
    assert!(
        messages(&off)[0].contains("AlloyExperimentalBailSyntax"),
        "{:?}",
        messages(&off)
    );
}

const RANGE: &str = "AlloyExperimentalRangePatternSyntax = true\n";

#[test]
fn a_range_pattern_takes_the_numbers_between() {
    let src = "local function grade(score: any) -> string\n    match score with\n        case 90..=100 then return \"A\"\n        case 80..90 or 70..80 then return \"B\"\n        case -5..0 then return \"neg\"\n        default return \"F\"\n    end\nend\nprint(grade(100), grade(90), grade(89.5), grade(70), grade(-5), grade(0), grade(\"x\"))\n";
    let out = compiled(src, RANGE);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );

    if let Some(printed) = run("range", &out.ship) {
        assert_eq!(printed, "A\tA\tB\tB\tneg\tF\tF\n");
    }

    let off = compiled(src, "");
    let found = messages(&off);
    assert_eq!(found.len(), 4, "{found:?}");
    assert!(
        found[0].contains("AlloyExperimentalRangePatternSyntax"),
        "{found:?}"
    );
}

#[test]
fn a_range_pattern_takes_a_number_on_each_side() {
    let options = EmitOptions {
        fflags: toml::from_str(RANGE).unwrap(),
        ..EmitOptions::default()
    };
    let wrong = alloy::compile_with(
        "match x with\n    case \"a\"..\"z\" then print(1)\n    default print(2)\nend\n",
        &options,
    );
    let report = match wrong {
        Ok(out) => messages(&out).join("\n"),

        Err(e) => e.message,
    };

    assert!(
        report.contains("a range pattern takes a number on each side"),
        "{report}"
    );
}

const USING: &str = "AlloyExperimentalUsingSyntax = true\nAlloyExperimentalDeferSyntax = true\n";

#[test]
fn a_using_drops_its_value_when_the_function_ends() {
    let src = "local function open(name: string)\n    return { name = name, close = function(self) print(\"close\", self.name) end }\nend\n\nlocal function work(n: number) -> number\n    using a = open(\"a\")\n    defer print(\"deferred\")\n    using b: { name: string, close: (any) -> () } = open(\"b\")\n    if n > 1 then\n        return n\n    end\n    return #a.name + #b.name\nend\nlocal using = 1\nprint(work(5), work(0), using)\n";
    let out = compiled(src, USING);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains(
            "    local a = open(\"a\") __defers[#__defers + 1] = function() __alloy.drop(a) end"
        ),
        "{}",
        out.ship
    );

    if let Some(printed) = run("using", &out.ship) {
        assert_eq!(
            printed,
            "close\tb\ndeferred\nclose\ta\nclose\tb\ndeferred\nclose\ta\n5\t2\t1\n"
        );
    }
}

#[test]
fn a_using_needs_its_flag_and_a_function() {
    let off = compiled(
        "local function f()\n    using a = open()\nend\nprint(f)\n",
        "",
    );
    assert!(
        messages(&off)[0].contains("AlloyExperimentalUsingSyntax"),
        "{:?}",
        messages(&off)
    );

    let top = compiled("using a = open()\nprint(a)\n", USING);
    assert!(
        messages(&top)[0].starts_with("`using` drops its value when its function ends"),
        "{:?}",
        messages(&top)
    );
    assert!(top.ship.starts_with("local a = open()"), "{}", top.ship);
}

const MUST_USE: &str = "AlloyExperimentalMustUse = true\n";

#[test]
fn a_must_use_function_leaves_its_attribute_out_of_the_output() {
    let src = "@must_use(\"it says if the save went through\")\nlocal function save(key: string) -> boolean\n    return key ~= \"\"\nend\nsave(\"a\")\n";
    let out = compiled(src, MUST_USE);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert!(!out.ship.contains("must_use"), "{}", out.ship);

    let off = compiled(src, "");
    assert!(
        messages(&off)[0].contains("AlloyExperimentalMustUse"),
        "{:?}",
        messages(&off)
    );
}

#[test]
fn a_dropped_result_of_a_must_use_function_is_a_lint() {
    let src = "@must_use(\"it says if the save went through\")\nlocal function save(key: string) -> boolean\n    return key ~= \"\"\nend\n\nstruct Box\n    n: number\nend\n\nimpl Box\n    @must_use\n    function take(self) -> number\n        return self.n\n    end\nend\n\nlocal b = new Box { n = 1 }\nsave(\"a\")\nb:take()\nif save(\"b\") then b:take() end\nlocal ok = save(\"c\")\nlocal _ = save(\"d\")\nlocal n =\n    b:take()\nprint(save(\"e\"), b:take(), ok, n)\n";
    let out = compiled(src, MUST_USE);
    let found: Vec<(usize, String)> = out
        .lints
        .iter()
        .filter(|l| l.name == "unused_must_use")
        .map(|l| {
            (
                src[..l.start as usize].matches('\n').count() + 1,
                l.message.clone(),
            )
        })
        .collect();
    let lines: Vec<usize> = found.iter().map(|(line, _)| *line).collect();

    assert_eq!(lines, [18, 19, 20], "{found:?}");
    assert!(
        found[0]
            .1
            .contains("the result of `save` is dropped; it says if the save went through"),
        "{found:?}"
    );
}

const NEWTYPE: &str = "AlloyExperimentalNewtypeSyntax = true\n";

#[test]
fn a_struct_over_one_type_is_its_value_at_run_time() {
    let src = "struct UserId(number)\nexport struct Meters(number)\n\nlocal function ban(id: UserId) -> number\n    return UserId.raw(id)\nend\n\nconst id = new UserId(5)\nconst names: { [UserId]: string } = { [id] = \"a\" }\nprint(ban(id), names[id], id == id, `{id}`, Meters.raw(new Meters(2)) * 2)\n";
    let out = compiled(src, NEWTYPE);

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );
    assert!(
        out.ship.contains(
            "type UserId = { read __newtype_UserId: number } local UserId = setmetatable("
        ),
        "{}",
        out.ship
    );
    assert!(out.ship.contains("export type Meters = "), "{}", out.ship);

    if let Some(printed) = run("newtype", &out.ship) {
        assert_eq!(printed, "5\ta\ttrue\t5\t4\n");
    }

    let off = compiled(src, "");
    let found = messages(&off);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].contains("AlloyExperimentalNewtypeSyntax"),
        "{found:?}"
    );
}

#[test]
fn the_formatter_keeps_each_form_as_written() {
    let src = "struct UserId(number)\nexport struct PlaceId(number)\n\nlocal function f(id: number?) -> number\n  const n = id ?? return 0\n  using lock = take(n)\n\n  match n with\n    case 1..=9 then return 1\n    case 10..100 then return 2\n    default return 3\n  end\nend\n\nprint(f, UserId, lock)\n";

    assert_eq!(alloy::fmt::format(src).unwrap(), src);
}
