//! The check artifact of a type-argument list is Luau the analyzer reads.
//!
//! `luau-lsp analyze` is the last word on the emit: a list that keeps an
//! Alloy spelling, such as `number[]`, is a syntax error there. The test
//! skips when the tool or the Roblox definitions are missing.

use std::path::Path;
use std::process::Command;

use alloy::EmitOptions;

const SRC: &str = r#"struct Pair<A, B> as
    first: A
    second: B
end

local written = HashMap.new<<string, number[]>>()
local nested = HashMap.new<<string, Pair<number, string[]>[]>>()
local inferred: HashMap<string, number[]> = HashMap.new()

function make(): HashMap<string, number[]>
    return HashMap.new()
end

print(written, nested, inferred, make())
"#;

/// `set`, `add`, and `push` give back the container they wrote into.
const CHAINS: &str = r#"local prices: HashMap<string, number> = HashMap.new()
local counted = prices:set("gem", 5):set("sword", 10):len()

local seen: Set<string> = Set.new()
local members = seen:add("a"):add("b"):to_array()

local xs = [ 1 ]
local total = xs:push(2, 3):push(4):len()

function log_to(sink: write string[])
    sink:push("line"):push("more")
end

print(counted, members, total)
"#;

/// A `@derive(Serialize)` struct passed to a `T: Serialize` bound.
const DERIVED: &str = r#"@derive(Serialize)
struct Point as
    x: number
    y: number
end

function to_data<T: Serialize>(v: T): any
    return v:serialize()
end

print(to_data(new Point { x = 1, y = 2 }))
"#;

/// `Future.race` over a list whose futures carry different values.
/// The union of the value types is the answer; a list of one value
/// type keeps that type.
const RACED: &str = r#"local ready = Future.resolve(42)
local timer = Future.delay(1)
local a = Future.resolve("a")
local b = Future.resolve("b")

async do
    local first: number? = await Future.race([ ready, timer ])
    local same: string = await Future.race([ a, b ])
    local both: string[] = await Future.all([ a, b ])

    print(first, same, both)
end
"#;

/// A Roblox service as an import lowers to `game:GetService`, so the
/// analyzer types the binding as the service class.
const SERVICES: &str = r#"import Players from "game:Players"
import { ReplicatedStorage, RunService as Run } from "game"

local remotes: Instance = ReplicatedStorage:WaitForChild("Remotes")
local count: number = #Players:GetPlayers()
local delta: number = Run.Heartbeat:Wait()

print(remotes, count, delta)
"#;

/// Compiles `src`, writes it beside a copy of the std, and runs
/// `luau-lsp analyze` over it. The test skips when the tool or the
/// Roblox definitions are missing.
/// A namespace: values, a struct, an enum, an impl, and a nested
/// namespace, each read back through the table and through the type
/// name the emit gives it.
const NAMESPACED: &str = r#"namespace Geom as
    const ORIGIN = 0

    struct Point as
        x: number
        y: number
    end

    impl Point as
        function sum(self): number
            return self.x + self.y
        end
    end

    enum Kind as
        Round
        Flat
    end

    function name_of(k: Kind): string
        match k with
            case Round then return "round"
            case Flat then return "flat"
        end

        return ""
    end

    namespace Deep as
        type Id = number
    end
end

local p: Geom.Point = new Geom.Point { x = Geom.ORIGIN, y = 1 }
local id: Geom.Deep.Id = 7

print(p:sum(), Geom.name_of(Geom.Kind.Round), id)
"#;

/// Every form of `destroy` and `after`: the plain call, Debris for an
/// Instance, a timer for a value with the method, the runtime helper,
/// and a block with and without a `where`.
const TIMED: &str = r#"struct Timer as
    left: number
end

impl Timer as
    function destroy(self)
        self.left = 0
    end
end

local part: Part = new Instance("Part")
local timer = new Timer { left = 5 }

destroy part
destroy timer
destroy part after 3
destroy timer after 1.5

function drop(x: Instance)
    destroy x after 4
end

local ready = false

after 2 do
    print("late")
end

after 0 where ready do
    print("gated")
end

print(drop, part, timer, ready)
"#;

/// `Future.reject` under an annotation, `Future.any` where every future
/// fails, and an `await` of a table with an `andThen` of its own.
const REJECTED: &str = r#"local one: Future<number> = Future.reject("no disk")
local two: Future<number> = Future.reject("no net")

local async function pick(): Result<number, string>
    local v = try await Future.any([ one, two ])

    return Ok(v)
end

local async function hand_written(): number
    local obj = {
        andThen = function(self, on_resolve: (number) -> number): number
            return on_resolve(42)
        end,
    }

    return await obj
end

print(pick, hand_written)
"#;

/// `try await` over every shape of Future the compiler can read: one
/// that settles with a Result, one annotated as such, one from an async
/// function declared to return a Result, and one that settles with a
/// plain value.
const TRIED: &str = r#"local async function loaded(): Result<number, string>
    return Ok(1)
end

local async function from_resolve(): Result<number, string>
    local v = try await Future.resolve(Ok(1))

    return Ok(v)
end

local async function from_annotation(): Result<number, string>
    local f: Future<Result<number, string>> = Future.resolve(Ok(1))
    local v = try await f

    return Ok(v)
end

local async function from_async(): Result<number, string>
    local v = try await loaded()

    return Ok(v)
end

local async function from_plain(): Result<number, string>
    local v = try await Future.resolve(1)

    return Ok(v)
end

print(from_resolve, from_annotation, from_async, from_plain)
"#;

/// `new Self()` inside a struct's own constructor. The value the
/// constructor gives back must be the struct, so the annotated locals
/// take it with no cast.
const CONSTRUCTED: &str = r#"struct Test as end

impl Test as
    function new()
        return new Test()
    end
end

struct Marked as
    field: number = 0
end

impl Marked as
    function new()
        return new Marked { }
    end

    function one(): Marked
        return new Marked { field = 1 }
    end
end

local made: Test = Test.new()
local blank: Marked = Marked.new()
local one: Marked = Marked.one()

print(made, blank, one)
"#;

/// A colon method on a plain table. The check artifact writes the
/// `self` parameter out as `typeof(Provider)`, which names the table
/// while the table still holds the method: the alias must not recurse
/// past the solver.
const TABLE_SELF: &str = r#"local Provider = { }

Provider.count = 0

function Provider:Bump(n: number): number
    self.count += n
    return self:Peek()
end

function Provider:Peek(): number
    return self.count
end

function Provider:Reset()
    self.count = 0
    self:Bump(1)
end

type Self = typeof(Provider)
local held: Self = Provider
held:Reset()
print(held:Bump(2), held:Peek())
"#;

/// The analyzer's `TypeError` and `SyntaxError` lines for one source,
/// or `None` when luau-lsp or the Roblox definitions are missing.
fn reports(src: &str, name: &str) -> Option<Vec<String>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions at {}", defs.display());

        return None;
    }

    let options = EmitOptions {
        check: true,
        file_name: format!("{name}.aly"),
        // The runtime lands beside the artifact, so the require is a path.
        std_require: "./alloy".to_string(),
        ..EmitOptions::default()
    };
    let out = alloy::compile_with(src, &options).unwrap();
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join(format!("alloy-analyze-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.luau"));
    std::fs::write(&file, &out.check).unwrap();
    // The runtime sits beside the artifact, which requires it by alias.
    std::fs::write(
        dir.join("alloy.luau"),
        std::fs::read_to_string(root.join("alloy/std/alloy.luau")).unwrap(),
    )
    .unwrap();
    // Without this the analyzer runs nonstrict, and the require gives
    // `any`: every std type in the artifact would go unchecked.
    std::fs::write(dir.join(".luaurc"), "{ \"languageMode\": \"strict\" }\n").unwrap();

    let run = Command::new("luau-lsp")
        .arg("analyze")
        .arg("--flag:LuauSolverV2=true")
        .arg(format!("--definitions={}", defs.display()))
        .arg(&file)
        .output();

    let Ok(run) = run else {
        eprintln!("skipped: luau-lsp is not installed");

        return None;
    };

    let text =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    // An unresolved require is the alias, not the emit under test.
    Some(
        text.lines()
            .filter(|l| l.contains("TypeError") || l.contains("SyntaxError"))
            .filter(|l| !l.contains("Unknown require"))
            .map(str::to_string)
            .collect(),
    )
}

#[track_caller]
fn analyze(src: &str, name: &str) {
    let Some(bad) = reports(src, name) else {
        return;
    };

    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `in` on a value that is no container compiled clean and threw inside
/// the std at run time.
#[test]
fn in_on_a_value_that_is_no_container_reports() {
    let bad = "local n = 5\nlocal found = 1 in n\nprint(found)\n";
    let Some(reported) = reports(bad, "in-bad") else {
        return;
    };
    assert!(
        reported.iter().any(|l| l.contains("but got 'number'")),
        "{reported:?}"
    );

    // Every container the std dispatches on still passes.
    let good = "struct Rec as\n    x: number\nend\n\nlocal arr = [ 1, 2 ]\nlocal st: Set<string> = Set.new()\nlocal hm: HashMap<string, number> = HashMap.new()\nlocal rec = new Rec { x = 1 }\nlocal raw = { a = 1 }\nprint(1 in arr, \"x\" in st, \"k\" in hm, \"h\" in \"hello\", \"x\" in rec, \"a\" in raw)\n";
    analyze(good, "in-good");
}

/// A `for` over an `Iter`, a `Queue`, or a `Heap` reported "Cannot
/// iterate over a table without indexer": the checker reads an
/// `__iter` from the type's metatable, and the std types had none.
/// Each loop binds the element type, so a wrong annotation reports.
#[test]
fn a_for_loop_over_a_std_collection_binds_the_element() {
    let good = "local seen = 0\nfor i in Iter.range(1, 10, 2) do\n    seen += i\nend\nlocal jobs = Queue.from({ \"a\", \"b\" })\nfor j in jobs do\n    seen += #j\nend\nlocal open = Heap.from({ 3, 1, 2 })\nfor h in open do\n    seen += h\nend\nprint(seen)\n";
    analyze(good, "for-std-good");

    let bad = "for i in Iter.range(1, 3) do\n    local s: string = i\n    print(s)\nend\n";
    let Some(reported) = reports(bad, "for-std-bad") else {
        return;
    };
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(
        reported[0].contains("Expected this to be 'string', but got 'number'"),
        "{reported:?}"
    );
}

/// `satisfies` emitted a `::`, which casts either way, so a literal that
/// leaves a key of `T` out went through.
#[test]
fn satisfies_reports_a_key_the_literal_leaves_out() {
    let missing = "local shape = { x = 0 } satisfies { x: number, y: number }\nprint(shape)\n";
    let Some(reported) = reports(missing, "satisfies-missing") else {
        return;
    };
    assert!(
        reported.iter().any(|l| l.contains("missing field 'y'")),
        "{reported:?}"
    );

    // A literal that covers `T` passes, and an Alloy spelling in `T`
    // lowers the way a declaration's does.
    analyze(
        "local shape = { x = 0, y = 1 } satisfies { x: number, y: number }\nlocal list = { xs = [ 1 ] } satisfies { xs: number[] }\nprint(shape, list)\n",
        "satisfies-good",
    );
}

/// `map` twice collapsed the value side to `any`: the second `map` read
/// the rung whose own return was `any`.
#[test]
fn two_maps_in_a_chain_keep_the_value_type() {
    let src = "local function parse(text: string): Result<number, string>\n    if text == \"\" then\n        return Err(\"bad\")\n    end\n    return Ok(1)\nend\n\nlocal twice: nil = parse(\"3\"):map(tostring):map(function(s) return #s end)\nprint(twice)\n";
    let Some(reported) = reports(src, "map-twice") else {
        return;
    };
    // `any` fits `nil`, so a collapsed chain reports nothing at all.
    assert!(
        reported.iter().any(|l| l.contains("number")),
        "{reported:?}"
    );

    // The chain still fits a declared Result, and one map is unchanged.
    analyze(
        "local function parse(text: string): Result<number, string>\n    if text == \"\" then\n        return Err(\"bad\")\n    end\n    return Ok(1)\nend\n\nlocal function lengths(text: string): Result<number, string>\n    return parse(text):map(tostring):map(function(s) return #s end)\nend\n\nprint(lengths)\n",
        "map-twice-fits",
    );
}

/// `unwrap_or` on a mapped Result answered `any`, so a fallback of
/// another type named neither side.
#[test]
fn unwrap_or_after_a_map_names_both_sides() {
    let src = "local function parse(text: string): Result<number, string>\n    if text == \"\" then\n        return Err(\"bad\")\n    end\n    return Ok(1)\nend\n\nlocal both: nil = parse(\"x\"):map(tostring):unwrap_or(0)\nprint(both)\n";
    let Some(bad) = reports(src, "unwrap-or-mapped") else {
        return;
    };
    assert!(
        bad.iter()
            .any(|l| l.contains("string") && l.contains("number")),
        "{bad:?}"
    );
}

/// The check artifact of every `destroy` and `after` form is Luau the
/// analyzer reads with no report of its own.
#[test]
fn destroy_and_after_analyze_as_luau() {
    analyze(TIMED, "timed");
}

#[test]
fn a_type_argument_list_analyzes_as_luau() {
    analyze(SRC, "type-args");
}

/// The mutating methods that return the value they changed keep that
/// return in the std's type, so a chain of them checks.
#[test]
fn a_mutating_method_chains() {
    analyze(CHAINS, "chains");
}

/// `@derive(Serialize)` writes `serialize`, so the struct meets a
/// `T: Serialize` bound with no method of its own.
#[test]
fn a_derived_struct_meets_the_serialize_bound() {
    analyze(DERIVED, "derived-serialize");
}

/// A mixed list of Futures races to the union of the value types, and
/// a list of one type keeps it.
#[test]
fn a_mixed_race_lands_on_the_union() {
    analyze(RACED, "raced");
}

/// A rejected Future never settles with a payload, so it stands where a
/// `Future<T>` is asked for. `Future<never>` did not: `andThen` puts the
/// Future in a parameter of its own, which makes the alias invariant.
/// `await` also takes any value with an `andThen`, as `alloy doc await`
/// says, whatever that method types its own callback as.
#[test]
fn a_reject_and_a_hand_written_awaitable_analyze() {
    analyze(REJECTED, "rejected");
}

/// A `try do` block around an `await` carries the awaited type: the
/// two blocks read `Result<number, any>`, so both annotations report.
/// The result of `await` is an `index<A, "__value">` type function,
/// and a generic bound off the closure's return landed on `unknown`
/// while it was pending, so the block read `Result<any, any>`.
#[test]
fn a_try_block_around_an_await_keeps_the_payload_type() {
    let src = "async function slow(): number
    return 9
end

async function main()
    local b: Result<string, any> = try do
        local r = await (slow())
        r
    end
    local c: Result<string, any> = try do
        await (slow())
    end
    print(b, c)
end
main()
";
    let Some(bad) = reports(src, "try-await-payload") else {
        return;
    };

    // An `any` payload fits the annotation, so a report on each
    // binding line is the proof. The `but got` half sits on a second
    // line, which the harness drops.
    assert_eq!(bad.len(), 2, "{}", bad.join("\n"));
    assert!(
        bad[0].contains("(6,") && bad[1].contains("(10,"),
        "{}",
        bad.join("\n")
    );
}

/// A `try await` of a Future that settles with a Result yields that
/// Result, not an Ok around it. The typed form said otherwise, and the
/// nested print named the emit's own keys: `_1`, `__ok`, `__err`.
#[test]
fn a_tried_await_yields_the_result_the_future_settles_with() {
    analyze(TRIED, "tried-await");
}

/// `import Players from "game:Players"` binds the service class, so a
/// member of it checks and the binding is no `Instance`.
#[test]
fn a_service_import_types_as_its_class() {
    analyze(SERVICES, "services");
}

/// A module of types alone and a module of types and values both give
/// the analyzer one value to require. Before the emit returned a table,
/// `require` of a types-only module read `Module does not return exactly
/// 1 value`.
#[test]
fn an_exporting_module_returns_one_value_to_require() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions at {}", defs.display());

        return;
    }

    let dir = std::env::temp_dir().join("alloy-analyze-module-return");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".luaurc"), "{ \"languageMode\": \"strict\" }\n").unwrap();
    std::fs::write(
        dir.join("alloy.luau"),
        std::fs::read_to_string(root.join("alloy/std/alloy.luau")).unwrap(),
    )
    .unwrap();

    let cases = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut files = Vec::new();

    for name in ["module_types_only", "module_mixed_exports"] {
        let src = std::fs::read_to_string(cases.join(format!("{name}.aly"))).unwrap();
        let options = EmitOptions {
            check: true,
            file_name: format!("{name}.aly"),
            std_require: "./alloy".to_string(),
            ..EmitOptions::default()
        };
        let out = alloy::compile_with(&src, &options).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        std::fs::write(dir.join(format!("{name}.luau")), &out.check).unwrap();
        files.push(name);
    }

    let main = "local types = require(\"./module_types_only\")\nlocal mixed = require(\"./module_mixed_exports\")\ntype Id = mixed.Id\nlocal n: Id = mixed.next(mixed.MAX)\nprint(types, n)\n";
    let main_file = dir.join("main.luau");
    std::fs::write(&main_file, main).unwrap();

    let run = Command::new("luau-lsp")
        .arg("analyze")
        .arg("--flag:LuauSolverV2=true")
        .arg(format!("--definitions={}", defs.display()))
        .arg(&main_file)
        .output();

    let Ok(run) = run else {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    };

    let text =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    let bad: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("TypeError") || l.contains("SyntaxError"))
        .collect();
    assert!(bad.is_empty(), "{}\n---\n{files:?}", bad.join("\n"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A module that ends in `return <expr>` has no export table, so the
/// analyzer must read the returned value where an import binds it: a
/// bare name, `* as`, and a name in braces all reach the same table.
#[test]
fn a_returning_module_types_through_its_value() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions at {}", defs.display());

        return;
    }

    let dir = std::env::temp_dir().join("alloy-analyze-returning");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".luaurc"), "{ \"languageMode\": \"strict\" }\n").unwrap();
    std::fs::write(
        dir.join("alloy.luau"),
        std::fs::read_to_string(root.join("alloy/std/alloy.luau")).unwrap(),
    )
    .unwrap();

    let module = "local Palette = { dark = \"#111111\" }\n\nfunction Palette.tint(hex: string): string\n    return hex\nend\n\nreturn Palette\n";
    std::fs::write(dir.join("palette.aly"), module).unwrap();

    let main = "import Palette from \"./palette\"\nimport * as All from \"./palette\"\nimport { tint } from \"./palette\"\n\nlocal a: string = Palette.tint(All.dark)\nlocal b: string = tint(\"#222222\")\nprint(a, b)\n";

    for (name, src) in [("palette", module), ("main", main)] {
        let options = EmitOptions {
            check: true,
            file_name: format!("{name}.aly"),
            std_require: "./alloy".to_string(),
            plain_modules: alloy::modules::plain_modules(src, &dir.join("main.aly"), &[]),
            ..EmitOptions::default()
        };
        let out = alloy::compile_with(src, &options).unwrap();
        assert!(out.diagnostics.is_empty(), "{name}: {:?}", out.diagnostics);
        std::fs::write(dir.join(format!("{name}.luau")), &out.check).unwrap();
    }

    let run = Command::new("luau-lsp")
        .arg("analyze")
        .arg("--flag:LuauSolverV2=true")
        .arg(format!("--definitions={}", defs.display()))
        .arg(dir.join("main.luau"))
        .output();

    let Ok(run) = run else {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    };

    let text =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    let bad: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("TypeError") || l.contains("SyntaxError"))
        .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The shared names of a project, through the analyzer. Each file
/// imports what it reads; `luau-lsp analyze` reads the emitted tree and
/// must find every name, every type, and no error of its own.
#[test]
fn a_project_with_imports_analyzes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions at {}", defs.display());

        return;
    }

    let dir = std::env::temp_dir().join(format!("alloy-analyze-imports-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/shared")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nout = \"out\"\nartifact = \"check\"\n",
    )
    .unwrap();
    std::fs::write(dir.join(".luaurc"), "{ \"languageMode\": \"strict\" }\n").unwrap();
    std::fs::write(
        dir.join("src/shared/log.aly"),
        "--- Writes a line.\nexport function log(msg: string)\n    print(msg)\nend\n\nexport struct Vec2 as\n    x: number\n    y: number\nend\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/shared/ids.aly"),
        "export type Id = number\n\nexport const MAX = 10\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.aly"),
        "import { log, Vec2 } from \"./shared/log\"\nimport { MAX } from \"./shared/ids\"\nimport type { Id } from \"./shared/ids\"\n\nlocal n: Id = MAX\nlog(`start {n}`)\n\nfunction origin(): Vec2\n    return new Vec2 { x = 0, y = 0 }\nend\n\nprint(origin().x)\n",
    )
    .unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let main = dir.join("out/main.luau");
    let run = Command::new("luau-lsp")
        .arg("analyze")
        .arg("--flag:LuauSolverV2=true")
        .arg(format!("--definitions={}", defs.display()))
        .arg(&main)
        .output();

    let Ok(run) = run else {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    };

    let text =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    let bad: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("TypeError") || l.contains("SyntaxError"))
        .collect();
    let emitted = std::fs::read_to_string(&main).unwrap();
    assert!(bad.is_empty(), "{}\n---\n{emitted}", bad.join("\n"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A namespace's artifact is Luau the analyzer reads: the table, the
/// type names, and the impl on a member struct.
#[test]
fn a_namespace_analyzes_clean() {
    analyze(NAMESPACED, "namespaced");
}

/// A constructor that writes `new Self()` gives back the struct, the
/// same as `new Self { }`. The paren form used to call the constructor
/// again, which left the return type unknown.
#[test]
fn a_self_construct_returns_the_struct() {
    analyze(CONSTRUCTED, "constructed");
}

/// `typeof(Provider)` on the `self` of a colon method reads back as the
/// table, and the analyzer settles it: the table names the methods and
/// each method names the table.
#[test]
fn a_table_method_self_type_settles() {
    analyze(TABLE_SELF, "table-self");
}

/// `alloy doc private` promises a type error on a private member read
/// from outside the impl. The check artifact keeps the member out of
/// the struct's public type, and the report stands beside the
/// `private_access` lint, which says the same thing without a checker.
#[test]
fn a_private_member_read_from_outside_is_an_error_and_a_lint() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");

    if !root.join("tools/types/globalTypes.d.luau").is_file() {
        eprintln!("skipped: no definitions");

        return;
    }

    let dir = std::env::temp_dir().join(format!("alloy-private-flux-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    // The doc's own example, plus a read and a call from outside.
    let src = "struct Counter as\n    read name: string,\n    private count: number = 0,\nend\n\nimpl Counter as\n    function bump(self): number\n        self.count += 1\n        return self.count\n    end\n\n    private function reset(self)\n        self.count = 0\n    end\nend\n\nfunction useIt()\n    local c = new Counter { name = \"hits\" }\n    c:reset()\n    print(c.count)\nend\n";
    std::fs::write(dir.join("src/c.aly"), src).unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(&dir, &config).unwrap();

    assert!(report.is_clean(), "{:?}", report.diagnostics);

    // The private field and the private method stay out of the public
    // type; `Counter__all` holds them for the impl.
    let check = &report.checks[0].check;

    assert!(
        check.contains("type Counter = typeof(setmetatable({} :: { read name: string }, Counter))"),
        "{check}"
    );
    assert!(
        check.contains("type Counter__all = Counter & { count: number }"),
        "{check}"
    );

    let lints: Vec<&str> = report
        .lints
        .iter()
        .map(|(_, l)| l.name)
        .filter(|n| *n == "private_access")
        .collect();

    assert_eq!(lints, vec!["private_access", "private_access"]);

    let Ok(analysis) = alloy::typecheck::analyze(&dir, &config, &report.checks, &[]) else {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    };
    let errors: Vec<String> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{}:{} {}", d.line, d.col, d.message))
        .collect();

    assert_eq!(
        errors,
        vec![
            "19:7 `reset` is private to `Counter`; only its impl reaches it".to_string(),
            "20:13 `count` is private to `Counter`; only its impl reaches it".to_string(),
        ]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `[roblox] rig` types `Player.Character`: `HumanoidRootPart` is a
/// `Part?` on either rig, and `Torso` is an R6 part alone.
#[test]
fn the_rig_types_the_character_of_a_player() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions");

        return;
    }

    let src = "function spawned(player: Player)\n    if local character = player.Character then\n        local root: string = character.HumanoidRootPart\n        local torso: string = character.Torso\n    end\nend\n";

    for rig in ["R15", "R6"] {
        let dir = std::env::temp_dir().join(format!("alloy-rig-flux-{rig}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("alloy.toml"),
            format!(
                "[build]\nin = \"src\"\nout = \"build\"\n\n[flux]\nroblox_types = false\ndefinitions = [\"{}\"]\n\n[roblox]\nrig = \"{rig}\"\n",
                defs.display()
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/c.aly"), src).unwrap();

        let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
        let report = alloy::build::flux_project(&dir, &config).unwrap();
        assert!(report.is_clean(), "{:?}", report.diagnostics);

        let Ok(analysis) = alloy::typecheck::analyze(&dir, &config, &report.checks, &[]) else {
            eprintln!("skipped: luau-lsp is not installed");

            return;
        };
        let errors: Vec<String> = analysis
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| format!("{} {}", d.line, d.message))
            .collect();

        let root_line = errors
            .iter()
            .find(|e| e.starts_with("3 "))
            .unwrap_or_else(|| panic!("{rig}: no report on the root part: {errors:?}"));
        assert!(root_line.contains("Part?"), "{rig}: {errors:?}");

        let torso_line = errors
            .iter()
            .find(|e| e.starts_with("4 "))
            .unwrap_or_else(|| panic!("{rig}: no report on the torso: {errors:?}"));

        match rig {
            "R15" => assert!(torso_line.contains("Torso"), "{rig}: {errors:?}"),

            _ => assert!(torso_line.contains("Part?"), "{rig}: {errors:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `if not local v: number = f()` keeps the name as the binding, so the
/// annotation checks the `number?` the value has. The declaration
/// writes `number?`; the guard's return narrows the name to `number`
/// after it, and a wrong annotation still reports.
#[test]
fn an_annotated_negated_if_local_types_the_nilable_value() {
    let head = "function f2(n: number): number?\n    if n > 0 then\n        return n + 1\n    end\n    return nil\nend\n\n";
    let good = format!(
        "{head}local function g(): number\n    if not local v3: number = f2(4) then\n        return 0\n    end\n    return v3\nend\nprint(g())\n"
    );
    let out = alloy::compile(&good).unwrap();
    assert!(
        out.check
            .contains("local v3: number? = f2(4) if not (v3) then"),
        "{}",
        out.check
    );
    analyze(&good, "if-not-local-annotated-good");

    let bad = format!(
        "{head}local function g(): number\n    if not local v3: string = f2(4) then\n        return 0\n    end\n    return 1\nend\nprint(g())\n"
    );
    let Some(reported) = reports(&bad, "if-not-local-annotated-bad") else {
        return;
    };
    assert!(
        reported
            .iter()
            .any(|l| l.contains("Expected this to be 'string?', but got 'number?'")),
        "{reported:?}"
    );
}

/// `if local v: number = f()` wrote the annotation on the temp, which
/// holds the `number?` the value has, so the one annotation a reader
/// writes reported. The annotation now goes on the name the branch
/// declares, where the test has narrowed the temp; a wrong one still
/// reports there.
#[test]
fn an_annotated_if_local_types_the_narrowed_name() {
    let head = "function f2(n: number): number?\n    if n > 0 then\n        return n + 1\n    end\n    return nil\nend\n\n";
    let good = format!("{head}if local v3: number = f2(4) then\n    print(v3)\nend\n");
    let out = alloy::compile(&good).unwrap();
    assert!(
        out.check
            .contains("do local _c1 = f2(4) if _c1 then local v3: number = _c1"),
        "{}",
        out.check
    );
    analyze(&good, "if-local-annotated-good");

    let bad = format!("{head}if local v3: string = f2(4) then\n    print(v3)\nend\n");
    let Some(reported) = reports(&bad, "if-local-annotated-bad") else {
        return;
    };
    assert!(
        reported
            .iter()
            .any(|l| l.contains("Expected this to be 'string', but got 'number'")),
        "{reported:?}"
    );
}
