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

/// A derived method takes the struct's public type, so a caller outside
/// the impl passes the value it holds. The fields form of a generic
/// struct takes the arguments `<<T>>` or the annotation names.
const DERIVED_PRIVATE: &str = r#"@derive(Clone, Debug, Serialize, Eq)
struct V as
    x: number
    private y: number = 0
end

local a = new V { x = 1 }
local b = a:clone()
print(a:debug(), b.x, a:serialize(), a == b)

struct Stack<T> as
    items: { T } = {}
end

local s = new Stack<<number>> {}
local s2: Stack<number> = new Stack { items = {} }
print(#s.items, #s2.items)
"#;

/// An operand that keeps its temps: a path of fields read again in
/// place, a closure called in place, and a guard past its pattern.
const IN_PLACE: &str = r#"type Char = { Humanoid: { Health: number }? }
type Player = { Character: Char? }

local function health(p: Player?): number
    return p and p.Character?.Humanoid?.Health or 0
end

local function get(): { a: { b: number }? }?
    return nil
end

type Info = { meta: { ok: boolean }? }
enum Ev as
    Got(Info?)
    Num(number)
end

local function check(e: Ev): string
    return match e with
        case Ev.Got(i) and i?.meta?.ok then "ok"
        case Ev.Got(_) then "got"
        case Ev.Num(_) then "num"
    end
end

local cached: number? = 5
local v: number? = cached ?? get()?.a?.b
print(health(nil), check(Ev.Num(1)), v)
"#;

/// A rest pattern over a plain table: the slice takes `{ T }`, and the
/// fields a table rest keeps read as `any`.
const RESTS: &str = r#"local xs: { number } = { 1, 2, 3 }
local [head, ...rest] = xs

local function f(ys: { number }): number
    return match ys with
        case [first, ...more] then first + #more
        default 0
    end
end

type Point = { x: number, y: number }
local pt: Point = { x = 1, y = 2 }
local { x, ...others } = pt
print(head, #rest, f(xs), x, others.y)
"#;

/// The analyzer's `TypeError` and `SyntaxError` lines for one source,
/// or `None` when luau-lsp or the Roblox definitions are missing.
fn reports(src: &str, name: &str) -> Option<Vec<String>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("../tools/types/globalTypes.d.luau");

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

    let dir = std::env::temp_dir().join(format!("alloy-analyze-{name}-{}", std::process::id()));
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

/// A name that a nested variant pattern binds read as `never`, so a
/// wrong use of it went through. It takes the payload type, `number`.
#[test]
fn a_nested_variant_binding_takes_its_payload_type() {
    let src = "enum Item as\n    Sword(number)\n    Nothing\nend\nenum Purchase as\n    Bought(Item)\n    Denied(string)\nend\nlocal function show(r: Purchase): string\n    return match r with\n        case Purchase.Bought(Item.Sword(d)) then d:upper()\n        default \"x\"\n    end\nend\nlocal function label(r: Purchase): string\n    match r with\n        case Purchase.Bought(Item.Sword(d)) then\n            local s: string = d\n            return s\n        default\n            return \"x\"\n    end\nend\nprint(show(Purchase.Bought(Item.Sword(4))), label)\n";
    let Some(reported) = reports(src, "nested-variant") else {
        return;
    };
    assert_eq!(reported.len(), 2, "{reported:?}");
    assert!(
        reported[0].contains("'number' does not have key 'upper'"),
        "{reported:?}"
    );
    assert!(
        reported[1].contains("Expected this to be 'string', but got 'number'"),
        "{reported:?}"
    );
}

/// `@derive(Debug)` on an enum wrote no `debug`, so a call of it
/// reported a missing key. It writes the one a derived struct has.
#[test]
fn a_derived_debug_on_an_enum_writes_debug() {
    let good = "@derive(Debug)\nenum Item as\n    Sword(number)\n    Nothing\nend\n@derive(Debug)\nenum Opt<T> as\n    Some(T)\n    None\nend\nlocal s = Item.Sword(4)\nlocal a: string = Item.debug(s)\nlocal b: string = Item.debug(Item.Nothing)\nlocal c: string = s:debug()\nlocal d: string = Opt.Some(1):debug()\nprint(a, b, c, d)\n";
    analyze(good, "enum-debug");

    let bad = "@derive(Debug)\nenum Item as\n    Sword(number)\n    Nothing\nend\nlocal n: number = Item.debug(Item.Nothing)\nprint(n)\n";
    let Some(reported) = reports(bad, "enum-debug-bad") else {
        return;
    };
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(
        reported[0].contains("Expected this to be 'number', but got 'string'"),
        "{reported:?}"
    );
}

/// A test on a nested path narrowed the root to `never`, so every name
/// the arm bound went through unchecked: a generic variant under an
/// enum, and a name one level down beside a nested pattern. Each takes
/// its own type now.
#[test]
fn a_nested_generic_variant_and_its_sibling_take_their_types() {
    let src = "enum Opt<T> as\n    Some(T)\n    Nil\nend\nenum Wrap as\n    W(Opt<string>)\n    Empty\nend\nenum Pair as\n    Both(Wrap, number)\n    Neither\nend\nlocal function a(p: Pair): boolean\n    return match p with\n        case Pair.Both(Wrap.W(Opt.Some(s)), _) then s\n        case Pair.Both(Wrap.Empty, n) then n\n        default true\n    end\nend\nlocal function b(o: Opt<Wrap>): boolean\n    match o with\n        case Opt.Some(Wrap.W(Opt.Some(t))) then\n            return t\n        default\n            return true\n    end\nend\nprint(a, b)\n";
    let Some(reported) = reports(src, "nested-generic") else {
        return;
    };
    assert_eq!(reported.len(), 3, "{reported:?}");

    for (line, ty) in [(15, "string"), (16, "number"), (23, "string")] {
        assert!(
            reported.iter().any(|r| r.contains(&format!("({line},"))
                && r.contains(&format!("Expected this to be 'boolean', but got '{ty}'"))),
            "{line}: {reported:?}"
        );
    }

    let good = "enum Opt<T> as\n    Some(T)\n    Nil\nend\nenum Wrap as\n    W(Opt<string>)\n    Empty\nend\nenum Pair as\n    Both(Wrap, number)\n    Neither\nend\nlocal function a(p: Pair): string\n    return match p with\n        case Pair.Both(Wrap.W(Opt.Some(s)), n) then s:upper() .. tostring(n + 1)\n        default \"x\"\n    end\nend\nprint(a)\n";
    analyze(good, "nested-generic-good");
}

/// A struct or an array pattern under a variant bound `never`, or read
/// a union no test narrowed. Each name takes its field or element type.
#[test]
fn a_struct_and_an_array_under_a_variant_take_their_types() {
    let src = "struct Point\n    x: number\n    y: number\nend\ntype Rec = { name: string }\nenum Box as\n    Full(Rec)\n    List({ number })\n    Pt(Point)\n    Empty\nend\nenum Order as\n    B(Box)\n    Nothing\nend\ntype Holder = { item: Box }\nlocal function a(o: Order): boolean\n    return match o with\n        case Order.B(Box.Full({ name = m })) then m\n        case Order.B(Box.List([f, ...rest])) then rest\n        case Order.B(Box.Pt(Point { x = qx })) then qx\n        default true\n    end\nend\nlocal function b(h: Holder): boolean\n    return match h with\n        case { item = Box.Full({ name = k }) } then k\n        default true\n    end\nend\nprint(a, b)\n";
    let Some(reported) = reports(src, "nested-struct") else {
        return;
    };
    assert_eq!(reported.len(), 4, "{reported:?}");

    for (line, ty) in [
        (19, "string"),
        (20, "Array<number>"),
        (21, "number"),
        (27, "string"),
    ] {
        assert!(
            reported.iter().any(|r| r.contains(&format!("({line},"))
                && r.contains(&format!("Expected this to be 'boolean', but got '{ty}'"))),
            "{line}: {reported:?}"
        );
    }

    // A literal item reads its slot with no test in front of it.
    let good = "enum Box as\n    List({ number })\n    Empty\nend\nenum Order as\n    B(Box)\n    Nothing\nend\nlocal function a(o: Order): number\n    return match o with\n        case Order.B(Box.List([1, g])) then g * 2\n        default 0\n    end\nend\nprint(a)\n";
    analyze(good, "nested-struct-good");
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

/// `for k, v in map:entries()` binds the value as `V`, the way `for k, v
/// in map` does. The iterator typed it `V?`, so `v + 1` reported
/// "number? and number" on the docs' own example.
#[test]
fn a_loop_over_entries_binds_the_value_type() {
    let good = "local prices: HashMap<string, number> = HashMap.new()\nprices:set(\"gem\", 5)\nfor key, value in prices:entries() do\n    print(key, value + 1)\nend\n";
    analyze(good, "entries-good");

    let bad = "local prices: HashMap<string, number> = HashMap.new()\nfor _, value in prices:entries() do\n    local s: string = value\n    print(s)\nend\n";
    let Some(reported) = reports(bad, "entries-bad") else {
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

/// `Iter.map` answered a second alias, then a third, and the third
/// `map` in a chain gave `any`. Every callback now reads its parameter
/// type from the step before, however long the chain.
#[test]
fn an_iter_chain_keeps_its_element_type_at_any_length() {
    let src = "local xs = Iter.from([ 1, 2, 3 ])\nlocal last: nil = xs\n    :map(function(n) return tostring(n) end)\n    :map(function(s) return #s > 0 end)\n    :map(function(b) return if b then 1 else 0 end)\n    :map(function(n) return n + 1 end)\n    :map(function(n) return `{n}` end)\n    :next()\nprint(last)\n";
    let Some(reported) = reports(src, "iter-chain") else {
        return;
    };
    // `any` fits `nil`, so a collapsed chain reports nothing at all.
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(reported[0].contains("'string?'"), "{reported:?}");

    // A generic function reads the methods of an `Iter<T>`, and a
    // mapped iterator fits a declared one.
    analyze(
        "function lengths<T>(it: Iter<T>): number[]\n    return it:map(function(x) return #tostring(x) end):collect()\nend\n\nlocal names: Iter<string> = Iter.from([ 1 ]):map(function(n) return tostring(n) end)\nprint(lengths(names), names:filter(function(s) return #s > 0 end):count())\n",
        "iter-generic",
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

/// A bound reaches every type the body writes with the parameter: a
/// local, an optional local, a loop variable, a cast, and a closure's
/// parameter and return. A closure with a `T` of its own keeps it bare.
#[test]
fn a_bound_reaches_the_types_of_the_body() {
    analyze(
        r#"trait Shape
    function area(self): number
end

struct Sq
    side: number
end

impl Shape for Sq
    function area(self): number
        return self.side * self.side
    end
end

local function biggest<T: Shape>(xs: { T }): T?
    local best: T? = nil
    for _, x: T in xs do
        local cur: T = x
        if best == nil or cur:area() > best:area() then
            best = cur
        end
    end
    local pick = function(a: T?): T?
        return a
    end
    local keep = function<T>(a: T): T
        return a
    end
    local first = (xs[1] :: T)
    print(first:area(), keep(1))
    return pick(best)
end

print(biggest({ new Sq { side = 2 }, new Sq { side = 3 } }))
"#,
        "bound-locals",
    );
}

/// Luau checks a `return` in a function written in a for-in header
/// against the function around the loop, so `filter` in a loop header
/// reported `Expected '()', got 'boolean'`. The check artifact casts
/// those values; a `return` of the loop body still checks.
#[test]
fn a_function_in_a_loop_header_returns_its_own_type() {
    analyze(
        r#"local xs = [ 1, 2, 3, 4 ]
for x in xs:filter(function(v) return v > 2 end) do
    local n: number = x
    print(n)
end
local function f(): number
    for x in xs:filter(function(v) return v > 2 end):map(function(v) return v * 2 end) do
        return x
    end
    return 0
end
print(f())
"#,
        "loop-header",
    );

    let Some(bad) = reports(
        "local xs = [ 1 ]\nlocal function f(): number\n    for x in xs:filter(function(v) return v > 2 end) do\n        return \"no\"\n    end\n    return 0\nend\nprint(f(), xs)\n",
        "loop-body",
    ) else {
        return;
    };
    assert_eq!(bad.len(), 1, "{bad:?}");
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

/// `await` gives every value a Future settles with. `local a, b = await
/// pair()` read `b` as `any` and got nil at run time. Now `b` takes the
/// second value's type, and a second name on a one-value Future reports.
#[test]
fn an_await_types_every_value_the_future_carries() {
    let src = "async function pair()
    return 1, \"x\"
end

async function typed(): (number, string)
    return 1, \"x\"
end

async function one(): number
    return 1
end

async function main()
    local a, b = await pair()
    local wrong: number = b
    local c, d = await typed()
    local right: string = d
    local e, f = await one()
    print(a, wrong, c, right, e, f)
end
main()
";
    let Some(bad) = reports(src, "await-pack") else {
        return;
    };

    assert_eq!(bad.len(), 2, "{}", bad.join("\n"));
    assert!(
        bad[0].contains("(15,") && bad[0].contains("'number', but got 'string'"),
        "{}",
        bad.join("\n")
    );
    assert!(
        bad[1].contains("(18,") && bad[1].contains("only returns 1 value"),
        "{}",
        bad.join("\n")
    );
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
    // Each report is the annotation's, not a type function that failed
    // on the pack a bare `await` returns.
    assert!(
        bad.iter().all(|l| l.contains("Expected this to be")),
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
    let defs = root.join("../tools/types/globalTypes.d.luau");

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
    let defs = root.join("../tools/types/globalTypes.d.luau");

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
    let defs = root.join("../tools/types/globalTypes.d.luau");

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

/// A field of an imported struct whose type names a type of its module,
/// `HashMap<string, Entry>`, constructs with a bare `HashMap.new()`. The
/// call took no type arguments there, and the analyzer reported that
/// the type arguments differ. The check artifact now casts it to the
/// field's own type, `index<Ballot, "votes">`, which keeps the field
/// typed: a read of it as a string reports.
#[test]
fn a_field_of_an_imported_struct_takes_its_own_type() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let defs = root.join("../tools/types/globalTypes.d.luau");

    if !defs.is_file() {
        eprintln!("skipped: no definitions at {}", defs.display());

        return;
    }

    let dir = std::env::temp_dir().join(format!("alloy-analyze-fields-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/shared")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nout = \"out\"\nartifact = \"check\"\n",
    )
    .unwrap();
    std::fs::write(dir.join(".luaurc"), "{ \"languageMode\": \"strict\" }\n").unwrap();
    std::fs::write(
        dir.join("src/shared/book.aly"),
        "import { HashMap, Set } from \"@alloy/std/collections\"\n\nexport type Entry = { n: number }\n\nexport struct Ballot as\n    votes: HashMap<string, Entry>\n    seen: Set<Entry>\n    plain: HashMap<string, string>\nend\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.aly"),
        "import { HashMap, Set } from \"@alloy/std/collections\"\nimport { Ballot } from \"./shared/book\"\nlocal b = new Ballot { votes = HashMap.new(), seen = Set.new(), plain = HashMap.new() }\nlocal wrong: string = b.votes\nprint(wrong)\n",
    )
    .unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let main = dir.join("out/main.luau");
    let emitted = std::fs::read_to_string(&main).unwrap();
    assert!(
        emitted.contains("votes = ((__alloy.HashMap.new() :: any) :: index<Ballot, \"votes\">)")
            && emitted.contains("plain = __alloy.HashMap.new<<string, string>>()"),
        "{emitted}"
    );

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
    assert_eq!(bad.len(), 1, "{}\n---\n{emitted}", bad.join("\n"));
    assert!(
        bad[0].contains("(4,") && bad[0].contains("Expected this to be 'string'"),
        "{}",
        bad[0]
    );

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

    if !root.join("../tools/types/globalTypes.d.luau").is_file() {
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
    let defs = root.join("../tools/types/globalTypes.d.luau");

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

/// `[flux] definitions` passed a `.d.aly` to the checker as it was, and
/// the checker cannot read Alloy. It compiles now, and a report on it
/// names the file. The editor reads the same list.
#[test]
fn a_listed_declaration_file_outside_in_compiles_and_reports() {
    let dir = std::env::temp_dir().join(format!("alloy-listed-defs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("types")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[flux]\nroblox_types = false\ndefinitions = [\"types/host.d.aly\", \"types/broken.d.aly\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/g.d.aly"),
        "interface Save as\n    coins: number\nend\nenum Mode as Fast, Slow end\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("types/host.d.aly"),
        "declare function host_fn(): number\n",
    )
    .unwrap();
    std::fs::write(dir.join("types/broken.d.aly"), "declare x: Nope\n").unwrap();
    std::fs::write(
        dir.join("src/main.aly"),
        "local s: Save = { coins = host_fn() }\nlocal m: Mode = \"Fast\"\nprint(s, m)\n",
    )
    .unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let listed: Vec<String> = alloy::build::definition_files(&dir, &config)
        .iter()
        .map(|p| p.strip_prefix(&dir).unwrap().display().to_string())
        .collect();
    assert_eq!(
        listed,
        vec!["src/g.d.aly", "types/host.d.aly", "types/broken.d.aly"]
    );

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
        .map(|d| format!("{} {}", d.rel.display(), d.message))
        .collect();

    assert_eq!(
        errors,
        vec![format!(
            "{} Unknown type 'Nope'",
            dir.join("types/broken.d.aly").display()
        )]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_derived_method_and_a_generic_fields_form_analyze() {
    analyze(DERIVED_PRIVATE, "derived_private");
}

#[test]
fn an_operand_that_keeps_its_temps_analyzes() {
    analyze(IN_PLACE, "in_place");
}

#[test]
fn a_rest_pattern_over_a_plain_table_analyzes() {
    analyze(RESTS, "rests");
}

/// `Future.all_settled` reached the checker's limit at every call, an
/// annotated one too, so its own doc example failed. The elements read
/// as `any`, and an annotation types them.
#[test]
fn all_settled_analyzes_and_an_annotation_types_it() {
    let body = "local a = Future.resolve(1)\nlocal f: Future<Result<number, any>[]> = Future.all_settled([a])\nlocal g = Future.all_settled([a])\nasync do\n    local rs = await Future.all_settled([Future.resolve(1), Future.reject(\"no\")])\n    local typed = await f\n    local n: TYPE = typed[1]:unwrap_or(0)\n    print(rs:len(), g, n)\nend\n";
    analyze(&body.replace("TYPE", "number"), "all-settled-good");

    let Some(reported) = reports(&body.replace("TYPE", "string"), "all-settled-bad") else {
        return;
    };
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(
        reported[0].contains("Expected this to be 'string'"),
        "{reported:?}"
    );
}

/// An `is` test on a struct or an enum narrowed the name in an `if`
/// statement alone. The right side of `and` and the branch of an `if`
/// expression read it as `unknown`: "Type 'unknown' does not have key
/// 'x'". Each read there casts now, and a wrong use still reports.
#[test]
fn an_is_test_narrows_the_expression_it_guards() {
    let head = "struct P as\n    x: number\nend\n\nenum Species as\n    Cat\n    Dog\nend\n\n";
    let good = format!(
        "{head}local function g(v: unknown, name: string): Species\n    local a = v is P and v.x > 0\n    local b = if v is P then v.x else 0\n    local c = v is not P or v.x > 0\n    local d = if v is not P then 0 else v.x\n    local e = v is P ? v.x : 0\n    print(a, b, c, d, e)\n    return if name is Species then name else Species.Cat\nend\nprint(g(1, \"Cat\"))\n"
    );
    let out = alloy::compile(&good).unwrap();
    assert!(
        out.check
            .contains("(getmetatable((v :: any)) == P) and ((v :: any) :: P).x > 0"),
        "{}",
        out.check
    );
    assert!(!out.ship.contains(":: P)"), "{}", out.ship);
    analyze(&good, "is-in-expression-good");

    let bad = format!(
        "{head}local function g(v: unknown): string\n    return if v is P then v.x else \"\"\nend\nprint(g(1))\n"
    );
    let Some(reported) = reports(&bad, "is-in-expression-bad") else {
        return;
    };
    assert!(
        reported.iter().any(|l| l.contains("got 'number'")),
        "{reported:?}"
    );
}

/// `f is function` cast `f` to two function types in an intersection,
/// an overload, so `f()` and `f(1)` were ambiguous. One function type
/// takes every call, and passes where a callback is asked.
#[test]
fn a_value_narrowed_to_function_calls() {
    let src = "local function take(cb: () -> ()) cb() end\nlocal function run(f: unknown, g: string | (number) -> ())\n    if f is function then\n        f()\n        f(1)\n        f(\"x\")\n        take(f)\n    end\n    if g is function then\n        g(1)\n    end\nend\nrun(print, \"x\")\n";
    analyze(src, "is-function");
}

/// A `.d.aly` that names a type of another `.d.aly` could load first,
/// and the type was unknown then, which dropped the whole file with no
/// report. An array there typed as a bare `Array` did the same.
#[test]
fn a_declaration_file_names_the_types_of_another() {
    let dir = std::env::temp_dir().join(format!("alloy-defs-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[flux]\nroblox_types = false\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/a.d.aly"),
        "declare function make(): Save\ndeclare items: number[]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/z.d.aly"),
        "interface Save as\n    coins: number\nend\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.aly"),
        "local s: Save = make()\nlocal n: number = items[1]\nprint(s.coins, n)\n",
    )
    .unwrap();

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
        .map(|d| format!("{} {}", d.rel.display(), d.message))
        .collect();

    assert!(errors.is_empty(), "{errors:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A match with no arm for a variant ends in a nil fallthrough, and the
/// checker reported that nil at the last arm beside `ExhaustiveMatch`.
/// The editor showed one report; `flux` now shows the same one.
#[test]
fn a_match_that_is_not_exhaustive_reports_once() {
    let dir = std::env::temp_dir().join(format!("alloy-exhaustive-flux-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    let src = "enum Kind\n    Pickaxe\n    Axe\n    Sword\nend\n\nlocal function h(k: Kind): number\n    return match k with\n        case Pickaxe then 1\n        case Axe then 2\n    end\nend\nprint(h(Kind.Axe))\n";
    std::fs::write(dir.join("src/a.aly"), src).unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(&dir, &config).unwrap();

    // Every line of the match holds the report, not only its first.
    assert_eq!(report.checks[0].error_lines, vec![8, 9, 10, 11]);

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

    assert!(errors.is_empty(), "{errors:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// React takes a binding where a Roblox property wants a value. The
/// check took any binding there, so a `Binding<string>` passed as a
/// `Size`. The value the binding holds is checked now, in either solver,
/// and a lone `{expr}` that sets `Text` takes a binding as `Text=` does.
#[test]
fn a_binding_on_a_roblox_tag_checks_its_value_in_either_solver() {
    let src = "type Binding<T> = { getValue: (self: Binding<T>) -> T }\nlocal React = {}\nfunction React.createElement(kind: any, props: any, ...: any): any\n    return props\nend\nlocal function View(label: Binding<string>, flag: Binding<boolean>)\n    return (\n        <Frame>\n            <TextLabel Text={label} Visible={flag} />\n            <TextLabel>{label}</TextLabel>\n            <TextLabel Size={label} Text={flag} />\n            <TextLabel>{flag}</TextLabel>\n        </Frame>\n    )\nend\nprint(View)\n";

    for new_solver in [true, false] {
        let dir =
            std::env::temp_dir().join(format!("alloy-binding-{new_solver}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("alloy.toml"),
            format!(
                "[build]\nin = \"src\"\nout = \"build\"\n\n[flux]\nnew_solver = {new_solver}\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/view.alx"), src).unwrap();

        let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
        let report = alloy::build::flux_project(&dir, &config).unwrap();
        let Ok(analysis) = alloy::typecheck::analyze(&dir, &config, &report.checks, &[]) else {
            eprintln!("skipped: luau-lsp is not installed");

            return;
        };
        let errors: Vec<(usize, String)> = analysis
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| (d.line, d.message.clone()))
            .collect();
        let lines: Vec<usize> = errors.iter().map(|e| e.0).collect();

        assert_eq!(lines, vec![11, 11, 12], "{new_solver}: {errors:?}");
        assert!(errors[0].1.contains("got 'Binding<string>'"), "{errors:?}");
        assert!(errors[2].1.contains("got 'Binding<boolean>'"), "{errors:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
