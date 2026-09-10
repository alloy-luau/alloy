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
local first: number? = await Future.race([ ready, timer ])

local a = Future.resolve("a")
local b = Future.resolve("b")
local same: string = await Future.race([ a, b ])
local both: string[] = await Future.all([ a, b ])

print(first, same, both)
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

fn analyze(src: &str, name: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions at {}", defs.display());

        return;
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
        std::fs::read_to_string(root.join("std/alloy.luau")).unwrap(),
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

        return;
    };

    let text =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    // An unresolved require is the alias, not the emit under test.
    let bad: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("TypeError") || l.contains("SyntaxError"))
        .filter(|l| !l.contains("Unknown require"))
        .collect();

    assert!(bad.is_empty(), "{}\n---\n{}", bad.join("\n"), out.check);
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
        std::fs::read_to_string(root.join("std/alloy.luau")).unwrap(),
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
        std::fs::read_to_string(root.join("std/alloy.luau")).unwrap(),
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

/// The globals of a project, through the analyzer. The build writes the
/// require and the binding on the first line of each file that names
/// one; `luau-lsp analyze` reads the emitted tree and must find every
/// name, every type, and no error of its own.
#[test]
fn a_project_with_globals_analyzes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions at {}", defs.display());

        return;
    }

    let dir = std::env::temp_dir().join(format!("alloy-analyze-globals-{}", std::process::id()));
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
        "--- Writes a line.\nglobal function log(msg: string)\n    print(msg)\nend\n\nglobal struct Vec2 as\n    x: number\n    y: number\nend\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/shared/ids.aly"),
        "global type Id = number\n\nglobal const MAX = 10\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.aly"),
        "local n: Id = MAX\nlog(`start {n}`)\n\nfunction origin(): Vec2\n    return new Vec2 { x = 0, y = 0 }\nend\n\nprint(origin().x)\n",
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
