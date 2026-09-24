//! A hoist runs in front of its statement. These cases run the emit and
//! hold it to the short circuits and the order the source wrote.

use std::fs;
use std::process::Command;

fn compile(src: &str) -> alloy::Output {
    alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap()
}

/// Compiles `src`, runs the ship artifact with `luau`, and returns what
/// it prints. `None` when `luau` is not installed.
fn run(name: &str, src: &str) -> Option<String> {
    let out = compile(src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        out.ship.lines().count(),
        src.lines().count(),
        "{}",
        out.ship
    );

    let dir = std::env::temp_dir().join(format!("alloy-order-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("alloy.luau"), alloy::RUNTIME).unwrap();
    fs::write(
        dir.join(".luaurc"),
        "{ \"aliases\": { \"alloy\": \"./alloy\" } }\n",
    )
    .unwrap();
    fs::write(dir.join("main.luau"), &out.ship).unwrap();
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

const GET: &str = "local calls = 0\nlocal function get(): any\n    calls += 1\n    return { a = { b = 1 } }\nend\n";

#[test]
fn a_chain_on_the_right_of_and_runs_only_when_the_left_holds() {
    let src = "type Char = { Humanoid: { Health: number }? }\ntype Player = { Character: Char? }\nlocal function health(p: Player?): number\n    return p and p.Character?.Humanoid?.Health or 0\nend\nlocal function at(list: { Player }, i: number): number?\n    return i <= #list and list[i].Character?.Humanoid?.Health or nil\nend\nprint(health(nil), health({ Character = { Humanoid = { Health = 5 } } }), at({}, 1))\n";

    // A field path reads again in place, so it takes no closure.
    let ship = compile(src).ship;
    assert!(
        ship.contains("p and (if p.Character == nil or p.Character.Humanoid == nil then nil else p.Character.Humanoid.Health) or 0"),
        "{ship}"
    );

    if let Some(out) = run("and", src) {
        assert_eq!(out.trim(), "0\t5\tnil", "{out}");
    }
}

#[test]
fn a_value_that_runs_on_some_paths_only_is_not_called_early() {
    let src = format!(
        "{GET}local cached: number? = 5\nlocal v1 = cached ?? get()?.a?.b\nlocal v2 = if cached then cached else get()?.a?.b\nlocal v3 = cached or get()?.a?.b\nlocal v4 = cached ?? match get() with\n    case nil then 0\n    default 1\nend\nlocal v5: number? = 3\nv5 ??= get()?.a?.b\nlocal none: any = nil\nnone?.m(get()?.a?.b)\nprint(v1, v2, v3, v4, v5, calls)\n"
    );

    if let Some(out) = run("lazy", &src) {
        assert_eq!(out.trim(), "5\t5\t5\t5\t3\t0", "{out}");
    }
}

#[test]
fn an_argument_runs_after_the_ones_before_it() {
    let src = "local log = {}\nlocal function a(): number\n    table.insert(log, \"a\")\n    return 1\nend\nlocal function b(): any\n    table.insert(log, \"b\")\n    return { c = { d = 2 } }\nend\nprint(a(), b()?.c?.d)\nlocal obj = { n = 0 }\nfunction obj.bump(self)\n    self.n += 1\n    return self\nend\nprint(table.concat(log, \",\"), obj.n .. (obj:bump()?.n ?? 0))\n";

    if let Some(out) = run("order", src) {
        assert_eq!(out.trim(), "1\t2\na,b\t01", "{out}");
    }
}

/// A guard runs only after its pattern matched, so its chain reads the
/// payload of that variant alone.
#[test]
fn a_guard_runs_after_its_pattern() {
    let src = "type Info = { meta: { ok: boolean }? }\nenum Ev as\n    Got(Info?)\n    Num(number)\nend\nlocal function check(e: Ev): string\n    return match e with\n        case Ev.Got(i) and i?.meta?.ok then \"ok\"\n        case Ev.Got(_) then \"got\"\n        case Ev.Num(_) then \"num\"\n    end\nend\nlocal function run(e: Ev): string\n    match e with\n        case Ev.Got(i) and i?.meta?.ok then return \"ok\"\n        default return \"other\"\n    end\nend\nprint(check(Ev.Num(5)), check(Ev.Got({ meta = { ok = true } })), run(Ev.Num(1)))\n";

    if let Some(out) = run("guard", src) {
        assert_eq!(out.trim(), "num\tok\tother", "{out}");
    }
}

/// A loop condition runs on every pass, and an `elseif` condition only
/// when the branches above it fail.
#[test]
fn a_loop_condition_reads_its_chain_on_every_pass() {
    let src = "local node: any = { next = { v = 2, next = { v = 3 } } }\nlocal n = 0\nwhile node?.next?.v do\n    node = node.next\n    n += 1\nend\nlocal k = 0\nlocal function step(): any\n    k += 1\n    return { next = if k < 3 then { v = k } else nil }\nend\nrepeat\n    local _ = 1\nuntil step()?.next?.v == nil\nlocal t: any = { a = { b = 2 } }\nif t == nil then\n    print(\"none\")\nelseif t?.a?.b == 2 then\n    print(\"two\", n, k)\nend\n";

    if let Some(out) = run("loop", src) {
        assert_eq!(out.trim(), "two\t2\t3", "{out}");
    }
}

#[test]
fn a_try_that_returns_from_an_operand_on_some_paths_reports() {
    let parse = "local function parse(s: string): Result<number, string>\n    if s == \"bad\" then\n        return Err(\"bad\")\n    end\n    return Ok(1)\nend\n";
    let src = format!(
        "{parse}local function f(flag: boolean): Result<number, string>\n    return Ok(flag and try parse(\"bad\") or 0)\nend\nprint(f(false))\n"
    );
    let messages: Vec<String> = compile(&src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        vec![
            "`try` cannot return from an operand that runs on some paths only, such as the right side of `and` or a loop condition; bind it to a local first"
        ]
    );

    // Inside a `try do` block the Err leaves by a raise, which a closure
    // passes on.
    let block = format!(
        "{parse}local flag = false\nlocal r = try do\n    local v = flag and try parse(\"bad\") or 0\n    return v\nend\nprint(r)\n"
    );

    if let Some(out) = run("try", &block) {
        assert_eq!(out.trim(), "Ok(0)", "{out}");
    }
}

/// The plain statement keeps its hoists in front, with no closure.
#[test]
fn a_chain_that_runs_first_still_hoists() {
    let src = format!("{GET}local v = get()?.a?.b\nprint(get()?.a?.b, v)\n");
    let ship = compile(&src).ship;

    assert!(ship.contains("local _1 = get() local _2 ="), "{ship}");
    assert!(ship.contains("_1 = get() _2 ="), "{ship}");
    assert!(!ship.contains("function()"), "{ship}");
}
