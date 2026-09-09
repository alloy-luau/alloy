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
