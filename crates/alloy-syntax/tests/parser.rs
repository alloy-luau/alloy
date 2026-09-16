/*!
These tests check parser conformance. Every snippet must parse, must tile the
token stream with no holes, and must print back byte for byte. That set of
three checks is the M1a exit criterion.
*/

use alloy_syntax::{lexer, parser, printer};

/// This function parses, checks coverage, prints, and compares in one step.
#[track_caller]
fn round_trip(src: &str) {
    let lexed = match lexer::lex(src) {
        Ok(l) => l,

        Err(e) => panic!("lex error at {}: {}\nsource:\n{src}", e.offset, e.message),
    };

    let chunk = match parser::parse(src, &lexed.toks) {
        Ok(c) => c,

        Err(e) => {
            let upto = &src[..e.offset.min(src.len())];
            let line = upto.matches('\n').count() + 1;

            panic!(
                "parse error at line {line} (byte {}): {}\nsource:\n{src}",
                e.offset, e.message
            );
        }
    };

    let holes = printer::coverage_errors(&chunk);
    assert!(holes.is_empty(), "coverage holes {holes:?}\nsource:\n{src}");
    let out = printer::print_chunk(src, &lexed.toks, &chunk);
    assert_eq!(out, src, "round trip differed\nsource:\n{src}");
}

#[track_caller]
fn rejects(src: &str) {
    let Ok(lexed) = lexer::lex(src) else { return };
    assert!(
        parser::parse(src, &lexed.toks).is_err(),
        "expected a parse error for:\n{src}"
    );
}

const CORPUS: &[&str] = &[
    // --- explicit type instantiation, which the sweeps mutate and truncate ---
    "local a = charm.atom<<number>>()\n",
    "local a = charm.atom<<(number, string)>>()\n",
    "local a = obj:method<<...number>>()\n",
    "local a = f<<Map<string, number>>>()\n",
    // --- basics ---
    "",
    "\n\n",
    "-- just a comment\n",
    "--!strict\nreturn nil\n",
    "local x = 1",
    "local x, y, z = 1, 2, 3\n",
    "local x = 1;\nlocal y = 2;\n",
    ";;;",
    "x = 1",
    "x, y = y, x",
    "a.b.c = 1",
    "a[1][2] = 3",
    "local t = {}\nt.x = 1\n",
    "return",
    "return 1, 2",
    "return;",
    // --- numbers and strings ---
    "local a = 0x1F\nlocal b = 0b1010\nlocal c = 1_000_000\nlocal d = .5\nlocal e = 1e-9\nlocal f = 1.5e+10\n",
    r#"local s = "double" local t = 'single'"#,
    "local s = [[long]]\nlocal t = [==[nested ]] here]==]\n",
    "local s = `interp {value} here`",
    "local s = `nested {`inner {x}`} done`",
    r#"local s = `braces {("}")} ok`"#,
    // --- operators ---
    "local a = 1 + 2 * 3 - 4 / 5 % 6 ^ 7",
    "local a = 1 // 2",
    "local a = -x + #t + not y",
    "local a = 'x' .. 'y' .. 'z'",
    "local a = x < y and y <= z or w ~= v and u == t",
    "local a = 2 ^ 3 ^ 4",
    "x += 1\nx -= 1\nx *= 2\nx /= 2\nx %= 3\nx ^= 2\nx ..= 'a'\nx //= 2\n",
    // --- control flow ---
    "if x then end",
    "if x then y() elseif z then w() else v() end",
    "while true do break end",
    "repeat x() until done",
    "do local x = 1 end",
    "for i = 1, 10 do end",
    "for i = 10, 1, -1 do end",
    "for k, v in pairs(t) do end",
    "for i, v: string in ipairs(t) do end",
    "while true do continue end",
    "for _ = 1, 3 do if x then continue end end",
    // --- functions ---
    "function f() end",
    "function a.b.c() end",
    "function a.b:c() end",
    "local function f() end",
    "local f = function() end",
    "function f(a, b, ...) return ... end",
    "function f(a: number, b: string?): boolean return true end",
    "function f<T>(x: T): T return x end",
    "function f<T, U...>(x: T, ...: U...): (T, U...) return x, ... end",
    "local f = function(...) local a = {...} end",
    "@native function fast() end",
    "@native @deprecated function both() end",
    "@native local function localfast() end",
    // --- calls ---
    "f()",
    "f(1, 2)",
    "f'str'",
    "f[=[long]=]",
    "local m = [[1, 2], [3]]\n",
    "f{1, 2}",
    "obj:method()",
    "obj:method 'str'",
    "local x = a.b.c:d(1)(2)[3]",
    "(f or g)()",
    "local x = (a + b).c",
    // --- tables ---
    "local t = {1, 2, 3}",
    "local t = {a = 1, b = 2}",
    "local t = {['key'] = 1, [2] = 'two'}",
    "local t = {1; 2; 3;}",
    "local t = {a = 1, [f()] = 2, 3,}",
    "local t = {nested = {deep = {1}}}",
    // --- types ---
    "local x: number = 1",
    "local x: string? = nil",
    "local x: {number} = {}",
    "local x: {[string]: number} = {}",
    "local x: {name: string, age: number} = t",
    "local x: (number) -> string = f",
    "local x: (a: number, b: string) -> (boolean, number) = f",
    "local x: () -> () = f",
    "local x: number | string = 1",
    "local x: A & B = t",
    "local x: typeof(y) = y",
    "local x: Foo.Bar = t",
    "local x: Foo<Bar, Baz> = t",
    "local x: 'literal' = 'literal'",
    "local x = y :: number",
    "local x = y :: any :: string",
    "type Point = {x: number, y: number}",
    "type Maybe<T> = T?",
    "export type Handler = (Instance) -> ()",
    "type Fn = <T>(T) -> T",
    "type ReadOnly = {read x: number}",
    "type Pack = (string, ...number) -> ...string",
    "local t: {[string]: {nested: boolean}} = {}",
    // --- if expressions ---
    "local x = if c then 1 else 2",
    "local x = if a then 1 elseif b then 2 else 3",
    "return if x then y else z",
    // --- alloy: nil coalescing ---
    "local a = b ?? c",
    "local a = b ?? c ?? d",
    "local a = x == nil ?? y",
    "local a = f() ?? {}",
    "x ??= 1",
    "t[f()] ??= x",
    "a.b ??= c.d ?? e",
    // --- alloy: safe access family ---
    "local a = b?.c",
    "local a = b?.c?.d.e",
    "local a = b?[k]",
    "local a = b?:m(1, 2)",
    "local a = b?:m<<number>>()",
    "local a = f?(x)",
    "local a = workspace->Map->Spawn",
    "local a = workspace->\"Spawn Points\"",
    "local a = workspace->`Slot{i}`",
    "local a = workspace->[name]",
    "local a = gui=>Hud=>Health",
    "local a = workspace -> Map => Spawn",
    "local f: (number) -> string = g",
    "local a = model.PrimaryPart!",
    "local a = model?.PrimaryPart!.Position",
    "local a = (b?.c :: number).x",
    "b?.c = 1",
    "b?.c += 1",
    "b?:m()",
    "handler.on_done!(code)",
    // --- alloy: expression layer ---
    "local v = c ? 'a' : 'b'",
    "local v = c ? (t:m()) : t:n()",
    "local v = a ? b : c ? d : e",
    "local v = x is Part",
    "local v = x is not nil",
    "local v = k is Enum.KeyCode",
    "local v = 3 in test",
    "local v = a band b bor c bxor d shl 1 shr 2",
    "local v = bnot a + (bnot b)",
    "local v = t satisfies Config",
    "local xs: number[] = [ 1, 2, 3 ]",
    "local ys: read string[] = []",
    "local zs: Array<number> = [ [ 1 ], [ 2 ] ]",
    "local u = 'abc':upper()",
    "local u = `a{b}`:split(',')",
    "btn.Activated:Connect(self:on_click)",
    "local p = new Vector3(1, 2, 3)",
    "local p = new Instance('Part') { Name = 'x', Parent = ws }",
    "local p = new Menu { opens = 1 }",
    "local p = new Array<<number>>()",
    "local new = Instance.new\nlocal f = new('Folder')",
    "delete part",
    "local delete = table.remove\ndelete(t, 1)",
    "local function f(a: number = 1, b = {}, ...) end",
    "local function f() -> number return 1 end",
    "local function f(x) -> (string, number) end",
    "local function f({ a, b = c }: T, [ x, y ]) end",
    "local { a, b = first }: Pair = t",
    "local [ head, ...rest ] = xs",
    "for _, { x, y } in points do end",
    "for _, p in players where p.Team ~= nil do end",
    "local m = { ...base, x = 1 }",
    "local m = { ... }",
    "local m = { ... - 1 }",
    "local function f(...) return { ..., n = 1 } end",
    "local r = await f()",
    "local r = try await f()",
    "local r = try g() ?? 0",
    "local fut = async do return 1 end",
    "local res = try do return 1 end",
    "local g = async function() end",
    "async function h() end",
    "local async function i() end",
    "export async function j() end",
    "$dbg(x)",
    "local n = $stringify(a + b)",
    "local n = $M.log(x, y)",
    "local w = await\nprint(w)",
    "local n = new\nprint(n)",
    // --- alloy: modules ---
    "import * as M from './m'",
    "import M from './m'",
    "import { a, b as c, type T } from './m'",
    "import type { T, U } from './m'",
    // An attribute in the list carries the `@` it is applied with.
    "import { @tagged } from './m'",
    "import { @tagged as t, other } from './m'",
    "import {\n\t@tagged,\n\tother,\n} from './m'",
    // The whole module and names from it, the way a default takes both.
    "import * as M, { a } from './m'",
    "import * as M, { @tagged as t } from './m'",
    "export { a, b as c }",
    "export { a } from './m'",
    "export type { T } from './m'",
    "export default { x = 1 }",
    // The Roblox services: one service by path, or a list from `game`.
    "import Players from 'game:Players'",
    "import { Players, ReplicatedStorage } from 'game'",
    "import { Players as P } from 'game'",
    "local import = 1\nimport = import + 1",
    // --- alloy: enums, impls, match ---
    "enum Color as Red, Green, Blue end",
    "enum Msg as\n\tJoin(Player)\n\tChat(Player, string)\nend",
    "enum E as Foo = 1 Bar = 2 end",
    "export enum Team as Red Blue end",
    "enum Opt<T> as Some(T), Nil end",
    "export enum Either<L, R = string> as\n\tLeft(L)\n\tRight(R)\nend",
    "impl Opt<T> as function get(self) return self._1 end end",
    "impl Msg as\n\tfunction player(self) return 1 end\nend",
    "impl Shape for Circle as\n\tfunction area(self) return 0 end\nend",
    "export impl Shape for BasePart as function area(self) return 0 end end",
    "impl Box<T> as function get(self) return self.v end end",
    // An empty body: the header opens and the same line closes it.
    "impl Empty as end",
    "impl Shape for Empty as end",
    "export impl Shape for Empty as end",
    "match x with case 1 then a() case 2 then b() default c() end",
    "match m with\n\tcase Join(p) then print(p)\n\tcase Chat(p, t) and #t > 2 then print(t)\nend",
    "local v = match c with case 'a' then 1 case 'b' then 2 default 3 end",
    "match a, b with case 0, _ then x() default y() end",
    "match e with case Some(Ok({ name })) then f(name) case None then g() end",
    "match v with case Config { volume } and volume > 1 then a() case { kind = 'x', r } then b() default c() end",
    "match xs with case [] then a() case [ x ] then b() case [ x, ...rest ] then c() end",
    "match e with case Join(p) or Leave(p) then f(p) default g() end",
    "local v = match k with case Enum.KeyCode.W then 1 default 0 end",
    "local match = string.match\nlocal n = match(s, '%d+')",
    "match e as state with case Loading then a(state) default b(state) end",
    "match a as left, b as right with case 0, _ then x(left, right) default y() end",
    "match a as left, b with case 0, _ then x(left) default y() end",
    "local v = match c as st with case 'a' then st case 'b' and st ~= '' then 2 default 3 end",
    // --- alloy: conditional bindings ---
    "if local x = f() then g(x) end",
    "if const x = f() then g(x) elseif local y = h() then g(y) else i() end",
    "if not local x = f() then return end",
    "if local x = f() where x > 3 then g(x) end",
    "if local a = f(); local b = g(a) then h(a, b) end",
    "if local Build(n) = job then print(n) elseif const Move(t) = job then print(t) end",
    "while local line = next() do print(line) end",
    "while local Move(t) = queue:pop() do print(t) end",
    "local v = if local p = f() then p.Name else 'nobody'",
    "local Build(name) = job",
    "local Ok(v) = parse(s) else return nil end",
    // --- alloy: declarations ---
    "struct Vec2 as\n\tread x: number\n\tread y: number\nend",
    "struct Health as max: number = 100, current: number end",
    "@derive(Eq, Debug, Clone)\nstruct Vec2 as x: number end",
    "export struct Stack<T> as items: T[] end",
    "struct Config as\n\t@range(0, 100)\n\tvolume: number\nend",
    "trait Shape as\n\tfunction area(self): number\n\tfunction describe(self) -> string\n\t\treturn `x`\n\tend\nend",
    "trait Empty as end",
    "struct Empty as end",
    "export struct Empty as end",
    "enum Empty as end",
    "interface Empty as end",
    "interface Named as name: string end",
    "interface Entity extends Named, Positioned as\n\tid: number\nend",
    "export interface Serializable as read version: number end",
    "remote Damage(target: Player, @u16 amount: number) from client",
    "export remote Toast(message: string, seconds: number = 3) from server",
    "export remote function GetProfile(id: number) -> Profile from client",
    "@unreliable\nexport remote Position(cframe: CFrame) from client",
    "@ratelimit(20, 1)\nremote Chat(text: string) from client or server",
    "attribute range(min: number, max: number) on field",
    "export attribute server_only on function",
    "attribute icon(asset: string) on struct, enum, variant",
    "@server_only\nlocal function save(p) end",
    "@test\nfunction t() end",
    "@test\nasync function t2() end",
    "@icon(\"x\")\nenum Team as\n\t@icon(\"y\")\n\tRed\n\tBlue\nend",
    "macro square(x)\n\tlocal v = x\n\tv * v\nend",
    "macro log(level, ...) print(level, ...) end",
    "export macro retry_count(n = 3) n end",
    "export macro pick(c, a, b)\n\tif c then a else b\nend",
    "macro either(c)\n\tif c then print(\"a\") else print(\"b\") end\nend",
    "type Partial<T> = { [K in keyof T]: T[K]? }",
    "type Readonly<T> = { [K in keyof T]: read T[K] }",
    "local struct = 1\nlocal trait = 2\nlocal remote = struct + trait",
    // --- luau const ---
    "const x = 1",
    "const Signal = require('./signal')",
    // --- realistic module shapes ---
    r#"--!strict
local Players = game:GetService("Players")
local Signal = require("@pkg/signal")

local Module = {}
Module.__index = Module

export type Module = typeof(setmetatable({} :: {
    name: string,
    count: number,
}, Module))

function Module.new(name: string): Module
    local self = setmetatable({}, Module)
    self.name = name
    self.count = 0
    return self :: any
end

function Module:increment(by: number?)
    self.count += by or 1
    if self.count > 10 then
        self:reset()
    end
end

function Module:reset()
    self.count = 0
end

return Module
"#,
    r##"
local t = {}
for i = 1, 100 do
    t[#t + 1] = function(...)
        return select("#", ...) > 0 and { ... } or nil
    end
end
return t
"##,
];

/*
This is explicit type instantiation, Luau's turbofish. The argument is a type
or a type pack, so all of these forms are legal. The round trip matters as much
as the parse, because a swallowed span would drop the type arguments from the
output with no report.
*/
#[test]
fn turbofish_type_arguments() {
    round_trip("local a = charm.atom<<number>>()\n");
    round_trip("local a = charm.atom<<(number, string)>>()\n");
    round_trip("local a = charm.atom<<()>>()\n");
    round_trip("local a = charm.atom<<...number>>()\n");
    round_trip("local a = charm.atom<<{ x: number }>>()\n");
    round_trip("local a = f<<number>>()\n");

    // Nested generics. This is why the bracket count must go by depth.
    round_trip("local a = charm.atom<<Map<string, number>>>()\n");
    round_trip("local a = f<<A<B<C>>>>()\n");

    // A method call also takes type arguments.
    round_trip("local a = obj:method<<number>>()\n");
    round_trip("local a = obj:method<<(number, string)>>()\n");

    // The other two call argument forms also take them.
    round_trip("local a = f<<number>>{ 1, 2 }\n");
    round_trip("local a = f<<string>>\"lit\"\n");

    // These calls are chained, so the suffix loop continues afterward.
    round_trip("local a = f<<number>>().field\n");
    round_trip("local a = f<<number>>()<<string>>()\n");
}

/// Inside a list a type is spelled `Box<number>`; a turbofish there
/// reports once, at the inner `<<`, and names the spelling.
#[test]
fn a_turbofish_inside_a_type_argument_list_reports() {
    let src = "local i1 = identity<<Box<<number>>>>(new Box<<number>> { value = 5 })
";
    let lexed = lexer::lex(src).unwrap();
    let err = parser::parse(src, &lexed.toks).unwrap_err();

    assert_eq!(
        err.message,
        "inside a type argument list a type is written `Box<number>`"
    );
    assert_eq!(err.offset, src.find("<<number>>>>").unwrap());
    round_trip(
        "local i1 = identity<<Box<number>>>(new Box<<number>> { value = 5 })
",
    );
}

/// A single `<` is still a comparison. The parser must not read it as a
/// turbofish.
#[test]
fn comparisons_are_not_turbofish() {
    round_trip("local a = b < c\n");
    round_trip("local a = b < c and c < d\n");
    round_trip("local a = f(b < c)\n");
    round_trip("local a = t[b < c]\n");
    round_trip("if a < b then end\n");

    // This is a generic call in type position. It is not related to the
    // expression form.
    round_trip("local a: Map<string, number> = x\n");
}

/// A turbofish always precedes a call, so a bare turbofish is a real error.
#[test]
fn a_turbofish_without_a_call_is_rejected() {
    rejects("local a = f<<number>>\n");
    rejects("local a = f<<number>> + 1\n");
    rejects("local a = f<<number\n");
}

/// `??` binds above `and` and below comparison, and only when the two `?`
/// touch. `a ? ? b` is still an error, and `number?` in a type stays a type.
#[test]
fn nil_coalescing_shape() {
    round_trip("local a = b ?? c == d\n");
    round_trip("local a = b and c ?? d\n");
    round_trip("local x: number? = y ?? 0\n");
    round_trip("local x = (y :: number?) ?? 0\n");
    rejects("local a = b ? ? c\n");
    rejects("local a = b ?\n");
}

/// The `?` family fuses only with a touching token, and a `?` that touches
/// nothing it can fuse with is an error. `!=` gets its own message.
#[test]
fn safe_access_shape() {
    round_trip("local a = b?.c ?? d\n");
    round_trip("local a = b ?? c?.d\n");
    rejects("local a = b? .c\n");
    rejects("local a = b?\n");
    rejects("local a = b->\n");
    rejects("local a = b=>\n");
    rejects("local a = b->1\n");

    let src = "if a != b then end\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();
    let err = alloy_syntax::parser::parse(src, &lexed.toks).unwrap_err();
    assert!(err.message.contains("~="), "{}", err.message);
}

/// Contextual words stay names when nothing makes them an operator.
#[test]
fn expression_words_stay_names_without_an_operand() {
    round_trip("local await = 1\nawait = await + 1\n");
    round_trip("local try = f\ntry(1)\n");
    round_trip("local is = 1\nlocal satisfies = 2\nlocal band = is + satisfies\n");
    round_trip("local where = 1\nfor k in t do where = k end\n");
    rejects("local v = c ? a\n");
    rejects("local v = x is\n");
    rejects("local v = [ 1, 2\n");
}

#[test]
fn a_match_arm_stops_at_the_next_case() {
    let src = "match x with case 1 then if y then z() end case 2 then w() end\n";
    round_trip(src);
    // `return` before `default` returns nothing; `default` is not a value.
    let src = "match x with case 1 then return default y() end\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();
    let chunk = alloy_syntax::parser::parse(src, &lexed.toks).unwrap();
    let alloy_syntax::ast::Stmt::Match(m) = &chunk.block.stmts[0] else {
        panic!()
    };
    assert!(m.default.is_some(), "the default arm parses");
    rejects("match x with case 1 then a()\n");
    rejects("if local x then end\n");
}

/// `as name` in a match head binds the value for every arm. `with`
/// closes the head, so it is never the name.
#[test]
fn a_match_head_takes_an_alias() {
    let src = "match e as state with case Loading then a(state) default b(state) end\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();
    let chunk = alloy_syntax::parser::parse(src, &lexed.toks).unwrap();
    let alloy_syntax::ast::Stmt::Match(m) = &chunk.block.stmts[0] else {
        panic!()
    };
    assert_eq!(m.aliases.len(), 1);
    assert_eq!(
        m.aliases[0].map(|a| a.text(src, &lexed.toks)),
        Some("state")
    );

    rejects("match e as with case 1 then a() default b() end\n");
    rejects("local v = match e as with case 1 then 1 default 2 end\n");
    rejects("match e as, f with case 1, 2 then a() default b() end\n");

    // Each value of the head takes its own name.
    rejects("match a as x, b as x with case 1, 2 then a(x) default b() end\n");
    round_trip("match a as x, b as y with case 1, 2 then a(x, y) default b() end\n");
    round_trip("match a as x, b with case 1, 2 then a(x) default b() end\n");
}

/// A pattern that binds the alias would give one value two names on
/// one line.
#[test]
fn a_pattern_cannot_bind_the_alias() {
    let stmt = "match e as state with case Ready(state) then a(state) default b() end\n";
    let expr = "local v = match e as state with case { state } then 1 default 2 end\n";
    rejects(stmt);
    rejects(expr);
    rejects("match e as s with case [ x, ...s ] then a() default b() end\n");
    // Another arm's name, and a field the pattern reads but does not
    // bind, both stand. A name an outer scope holds is the arm's to
    // bind, alias or no alias.
    round_trip("match e as state with case Ready(n) then a(n, state) default b() end\n");
    round_trip(
        "local n = 1\nmatch e as state with case Ready(n) then a(n, state) default b(n) end\n",
    );
    round_trip("match e as state with case P { state = n } then a(n) default b() end\n");

    let lexed = alloy_syntax::lexer::lex(stmt).unwrap();
    let (_, diagnostics) = alloy_syntax::parser::parse_lenient(
        stmt,
        &lexed.toks,
        alloy_syntax::parser::ParseOptions::default(),
    );
    assert_eq!(
        diagnostics[0].message,
        "`state` is the alias of the match; a pattern cannot bind it again"
    );
}

#[test]
fn a_case_arm_after_default_is_an_error() {
    // The emit reads the arms in order, so an arm after `default` leaks
    // its source into the output.
    let stmt = "match n with case 0 then z() default a() case 1 then b() end\n";
    let expr = "local v = match n with case 0 then 0 default 1 case 1 then 1 end\n";
    rejects(stmt);
    rejects(expr);

    // A lenient parse reports it once and still reads the whole file.
    for src in [stmt, expr] {
        let lexed = alloy_syntax::lexer::lex(src).unwrap();
        let (_, diagnostics) = alloy_syntax::parser::parse_lenient(
            src,
            &lexed.toks,
            alloy_syntax::parser::ParseOptions::default(),
        );
        assert_eq!(diagnostics.len(), 1, "one report for:\n{src}");
        assert!(
            diagnostics[0].message.contains("cannot follow `default`"),
            "got {}",
            diagnostics[0].message
        );
    }
}

#[test]
fn the_default_arm_comes_once_and_after_a_case() {
    // The emit reads the arms in order. A second `default` left the first
    // one in the output, and a `match` with only `default` emitted the
    // head of a `case` that the source never wrote.
    let cases = [
        (
            "match n with case 1 then a() default b() default c() end\n",
            "takes one `default` arm",
        ),
        (
            "local v = match n with case 1 then 1 default 2 default 3 end\n",
            "takes one `default` arm",
        ),
        (
            "match n with default b() end\n",
            "needs a `case` arm before `default`",
        ),
        (
            "local v = match n with default 2 end\n",
            "needs a `case` arm before `default`",
        ),
    ];

    for (src, wanted) in cases {
        rejects(src);

        // A lenient parse reports it once and still reads the whole file.
        let lexed = alloy_syntax::lexer::lex(src).unwrap();
        let (chunk, diagnostics) = alloy_syntax::parser::parse_lenient(
            src,
            &lexed.toks,
            alloy_syntax::parser::ParseOptions::default(),
        );
        assert_eq!(diagnostics.len(), 1, "one report for:\n{src}");
        assert!(
            diagnostics[0].message.contains(wanted),
            "got {}",
            diagnostics[0].message
        );
        assert_eq!(chunk.block.stmts.len(), 1, "the match still reads:\n{src}");
    }
}

#[test]
fn corpus_round_trips() {
    for src in CORPUS {
        round_trip(src);
    }
}

#[test]
fn escaped_quote_strings() {
    round_trip("local s = \"escaped \\\" quote\"\n");
    round_trip("local s = 'it\\'s'\n");
}

#[test]
fn rejects_broken_input() {
    rejects("local = 1");
    rejects("if x then");
    rejects("function f(");
    rejects("local x = ");
    rejects("return return");
    rejects("do end end");
    rejects("local x = {");
    rejects("for i = 1 do end");
    rejects("x +");
    rejects("1 = x");
}

#[test]
fn deep_nesting_errors_instead_of_crashing() {
    // A stack overflow here is the darklua bug class that the design removed.
    let deep = format!("local x = {}1{}", "(".repeat(5000), ")".repeat(5000));
    let lexed = lexer::lex(&deep).unwrap();

    assert!(parser::parse(&deep, &lexed.toks).is_err());

    let deep_tables = format!("local t = {}{}", "{".repeat(5000), "}".repeat(5000));
    let lexed = lexer::lex(&deep_tables).unwrap();

    assert!(parser::parse(&deep_tables, &lexed.toks).is_err());
}

/*
This is a simple form of fuzzing. Byte-level mutations of the corpus must
never panic or hang. The real coverage-guided fuzzing lives in fuzz/ for
nightly runs.
*/
#[test]
fn mutations_never_panic() {
    let interesting = br#""'`[]{}()\\
-"#;
    let mut checked = 0usize;

    for src in CORPUS.iter().filter(|s| s.len() < 400) {
        let bytes = src.as_bytes();

        for pos in 0..bytes.len() {
            for &b in interesting {
                let mut m = bytes.to_vec();
                m[pos] = b;
                let Ok(text) = String::from_utf8(m) else {
                    continue;
                };

                if let Ok(lexed) = lexer::lex(&text) {
                    // The parse must stop and must not panic. Each result is
                    // acceptable.
                    let _ = parser::parse(&text, &lexed.toks);
                }

                checked += 1;
            }
        }
    }

    assert!(
        checked > 1000,
        "expected a decent mutation count, got {checked}"
    );
}

#[test]
fn truncations_never_panic() {
    for src in CORPUS {
        for cut in 0..src.len() {
            if !src.is_char_boundary(cut) {
                continue;
            }

            let text = &src[..cut];

            if let Ok(lexed) = lexer::lex(text) {
                let _ = parser::parse(text, &lexed.toks);
            }
        }
    }
}

// --- classes, export by value, and integer literals ----------------------

/// `enum Opt<T>` reads its parameter list the way a struct does: the
/// span holds the brackets, and a payload may name the parameter.
#[test]
fn a_generic_enum_keeps_its_parameter_list() {
    let src = "enum Either<L, R = string> as\n    Left(L)\n    Right(R)\nend\n";
    let lexed = lexer::lex(src).unwrap();
    let chunk = parser::parse(src, &lexed.toks).unwrap();
    let alloy_syntax::ast::Stmt::Enum(e) = &chunk.block.stmts[0] else {
        panic!("expected an enum");
    };
    let text = |s: alloy_syntax::ast::TokSpan| s.text(src, &lexed.toks);

    assert_eq!(text(e.generics.unwrap()), "<L, R = string>");
    assert_eq!(text(e.variants[0].payload[0]), "L");
    assert_eq!(e.variants.len(), 2);
}

#[test]
fn a_class_with_fields_and_methods_parses() {
    round_trip(
        "class Point\n\tpublic x: number\n\tpublic y\n\tfunction magnitude(self)\n\t\treturn math.sqrt(self.x * self.x + self.y * self.y)\n\tend\nend\n",
    );
}

#[test]
fn class_forms_all_parse() {
    round_trip("export class Empty\nend\n");
    round_trip("open class Animal\n\tpublic species: string\nend\n");
    round_trip(
        "class Cat extends Animal\n\tfunction speak(self)\n\t\treturn \"meow\"\n\tend\nend\n",
    );
    round_trip("export open class Base\nend\n");
    round_trip(
        "class M\n\tfunction __init(self)\n\tend\n\tfunction __tostring(self)\n\t\treturn \"m\"\n\tend\nend\n",
    );
}

/// `class` and `open` stay ordinary names outside a declaration.
#[test]
fn class_and_open_stay_contextual() {
    round_trip("local class = 1\nclass = class + 1\n");
    round_trip("local open = io.open\nopen(\"f\")\n");
    round_trip("class.method()\n");
    round_trip("return open\n");
}

#[test]
fn a_class_rejects_what_the_rfc_rejects() {
    rejects("class Point\n\tfunction __index(self)\n\tend\nend\n");
    rejects("class Point\n\tfunction a.b(self)\n\tend\nend\n");
    rejects("class Point\n\tpublic x: number\n");
}

#[test]
fn export_by_value_forms_parse() {
    round_trip("export local version = \"5.1\"\n");
    round_trip("export const TAU = math.pi * 2\n");
    round_trip("export local a, b, c = 1, 2, 3\n");
    round_trip("export function init()\nend\n");
    round_trip("@native\nexport function fast()\nend\n");
}

/// A variable named export keeps parsing as an expression.
#[test]
fn export_stays_contextual() {
    round_trip("local export = 1\nexport = export + 1\n");
    round_trip("export.field = 2\n");
}

// --- the contextual words -------------------------------------------------

/// The reserved-word diagnostics of one lenient parse.
#[track_caller]
fn reserved_reports(src: &str) -> Vec<String> {
    let lexed = lexer::lex(src).expect("lex");
    let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, parser::ParseOptions::default());

    diagnostics
        .into_iter()
        .map(|d| d.message)
        .filter(|m| m.contains("reserved word"))
        .collect()
}

/// The word parses as a plain name: the file round trips and nothing
/// reports it as reserved.
#[track_caller]
fn name_case(src: &str) {
    round_trip(src);
    let reports = reserved_reports(src);
    assert!(reports.is_empty(), "{src:?} reported {reports:?}");
}

/// Every contextual word is a name wherever the token after it cannot
/// follow the keyword.
#[test]
fn a_contextual_word_is_a_name() {
    for word in [
        "new",
        "export",
        "try",
        "match",
        "const",
        "async",
        "await",
        "delete",
        "destroy",
        "after",
        "enum",
        "struct",
        "interface",
        "import",
    ] {
        name_case(&format!("local {word} = 1\nprint({word})\n"));
        name_case(&format!("{word} = 1\n"));
        name_case(&format!("local t = {{}}\nprint(t.{word})\n"));
        name_case(&format!("local {word} = {{}}\nprint({word}.f)\n"));
        name_case(&format!("local {word} = {{}}\nprint({word}[1])\n"));
        name_case(&format!("local {word} = {{}}\n{word}[1] = 2\n"));
        name_case(&format!("local {word} = {{}}\nprint({word}:f())\n"));
        name_case(&format!("local f = function({word}) return {word} end\n"));
        name_case(&format!("local t = {{ {word} = 1 }}\nprint(t)\n"));
        name_case(&format!(
            "local {word}, other = 1, 2\nprint({word}, other)\n"
        ));
        name_case(&format!("local f = print\nf({word})\n"));
        name_case(&format!("local x = 1\nlocal {word} = x\nprint({word})\n"));
    }

    // A call shape: the word takes arguments the way any function does.
    name_case("local new = Instance.new\nlocal p = new(\"Part\")\nprint(p)\n");
    name_case("local try = pcall\nprint(try(print))\n");
    name_case("local match = string.match\nprint(match(\"a\", \"b\"))\n");
    name_case("local const = print\nconst(1)\n");
    name_case("local export = print\nexport(1)\n");

    // Luau's own call forms: `f \"s\"` and `f { }`.
    name_case("local new = print\nnew \"Part\"\n");
    name_case("local new = print\nnew { n = 1 }\n");
    name_case("local match = print\nmatch \"a\"\n");

    // A binary operator after the word: the word is its left operand.
    name_case("local try = \"a\"\nprint(try .. \"b\")\n");
    name_case("local new = 1\nprint(new + 1)\n");
    name_case("local const = 1\nconst += 1\n");

    // `destroy` is the Roblox cleanup method, by name and by call.
    name_case("local function destroy(self) return self end\nprint(destroy)\n");
    name_case("local t = {}\nfunction t.destroy(self) return self end\nt:destroy()\n");
    name_case("local t = { destroy = print }\nt.destroy(1)\n");

    // `after` takes every shape a number-valued local takes.
    name_case("local after = 2\nafter += 1\nprint(after)\n");
    name_case("local after = print\nafter(1)\n");
    name_case("local after = {}\nafter.f = 2\nprint(after)\n");
    name_case("local after = {}\nafter:f()\n");

    // `async`, `await`, and `delete` are plain values here.
    name_case("local async = false\nif async then print(1) end\n");
    name_case("local await = print\nawait(1)\n");
    name_case("local delete = print\ndelete(1)\n");

    // `enum`, `struct`, and `interface` name a table.
    name_case("local enum = { Idle = 1 }\nprint(enum.Idle)\n");
    name_case("local struct = {}\nstruct.x = 1\nprint(struct)\n");
    name_case("local interface = {}\nprint(interface)\n");

    // `import` is a name in every shape but the call the emit requires.
    name_case("local import = {}\nimport.cache = 1\nprint(import)\n");
    name_case("local t = { import = 1 }\nprint(t.import)\n");
}

/// Each word still opens its own syntax.
#[test]
fn a_contextual_word_is_the_keyword() {
    use alloy_syntax::ast::{Expr, Local, Stmt};

    let head = |src: &str| {
        let lexed = lexer::lex(src).expect("lex");

        parser::parse(src, &lexed.toks)
            .unwrap_or_else(|e| panic!("{src:?}: {}", e.message))
            .block
            .stmts
            .into_iter()
            .next()
            .expect("a statement")
    };

    assert!(matches!(
        head("match x with\ncase 1 then print(1)\nend\n"),
        Stmt::Match(_)
    ));
    assert!(matches!(
        head("match [a, b] with\ncase [1, 2] then print(1)\nend\n"),
        Stmt::Match(_)
    ));
    assert!(matches!(
        head("match { n = 1 } with\ncase { n = 1 } then print(1)\nend\n"),
        Stmt::Match(_)
    ));
    assert!(matches!(
        head("const LIMIT = 5\n"),
        Stmt::Local(Local { is_const: true, .. })
    ));
    assert!(matches!(
        head("const LIMIT: number = 5\n"),
        Stmt::Local(Local { is_const: true, .. })
    ));
    assert!(matches!(
        head("const function f() end\n"),
        Stmt::LocalFunction(_)
    ));
    assert!(matches!(head("const { a } = t\n"), Stmt::Local(_)));
    assert!(matches!(
        head("export type T = number\n"),
        Stmt::TypeAlias(_)
    ));
    assert!(matches!(head("export { a, b }\n"), Stmt::ExportList(_)));
    assert!(matches!(head("export local x = 1\n"), Stmt::Local(_)));
    assert!(matches!(
        head("export function f() end\n"),
        Stmt::Function(_)
    ));

    let value = |src: &str| match head(src) {
        Stmt::Local(l) => l.values.into_iter().next().expect("a value"),

        other => panic!("{src:?} parsed as {other:?}"),
    };

    assert!(matches!(value("local v = new Thing()\n"), Expr::New { .. }));
    assert!(matches!(
        value("local v = new Thing { n = 1 }\n"),
        Expr::New { .. }
    ));
    assert!(matches!(value("local v = try f()\n"), Expr::Try { .. }));
    assert!(matches!(
        value("local v = try do return 1 end\n"),
        Expr::TryBlock { .. }
    ));
    assert!(matches!(
        value("local v = match x with\ncase 1 then 2\nend\n"),
        Expr::Match { .. }
    ));
}

/// The eight words that joined the contextual set keep their own syntax.
#[test]
fn the_new_contextual_words_keep_their_syntax() {
    use alloy_syntax::ast::Stmt;

    let head = |src: &str| {
        let lexed = lexer::lex(src).expect("lex");

        parser::parse(src, &lexed.toks)
            .unwrap_or_else(|e| panic!("{src:?}: {}", e.message))
            .block
            .stmts
            .into_iter()
            .next()
            .expect("a statement")
    };

    assert!(matches!(
        head("after 2 do\n    print(1)\nend\n"),
        Stmt::After(_)
    ));
    assert!(matches!(
        head("after 3 where ready do\n    print(1)\nend\n"),
        Stmt::After(_)
    ));
    assert!(matches!(
        head("after (n + 1) do\n    print(1)\nend\n"),
        Stmt::After(_)
    ));
    assert!(matches!(head("destroy part\n"), Stmt::Destroy { .. }));
    assert!(matches!(
        head("destroy part after 2\n"),
        Stmt::Destroy { delay: Some(_), .. }
    ));
    assert!(matches!(head("delete t.x\n"), Stmt::Delete { .. }));
    assert!(matches!(
        head("async function f()\nend\n"),
        Stmt::Function(_)
    ));
    assert!(matches!(
        head("enum State as\n    Idle\nend\n"),
        Stmt::Enum(_)
    ));
    assert!(matches!(
        head("struct Vec2 as\n    x: number\nend\n"),
        Stmt::Struct(_)
    ));
    assert!(matches!(
        head("interface Named as\n    name: string\nend\n"),
        Stmt::Interface(_)
    ));
    assert!(matches!(head("import { a } from \"m\"\n"), Stmt::Import(_)));
    assert!(matches!(
        head("import * as m from \"m\"\n"),
        Stmt::Import(_)
    ));
    assert!(matches!(head("import M from \"m\"\n"), Stmt::Import(_)));
    assert!(matches!(
        head("import type { T } from \"m\"\n"),
        Stmt::Import(_)
    ));

    // `await` stays the prefix operator in expression position.
    round_trip("local v = await f()\nprint(v)\n");
    round_trip("local v = async do return 1 end\nprint(v)\n");
}

/// One file writes the word both ways. Luau does this with `export`, and
/// the five words follow it.
#[test]
fn both_readings_live_in_one_file() {
    round_trip("export type T = number\nlocal export = {}\nexport = export\nreturn export\n");
    round_trip("local match = {}\nmatch[1] = 2\nmatch x with\ncase 1 then print(1)\nend\n");
    round_trip("const LIMIT = 5\nlocal const = 1\nconst = const + LIMIT\n");
    round_trip("local new = Instance.new\nlocal v = new Thing()\nprint(new(\"Part\"), v)\n");
    round_trip("local try = pcall\nlocal v = try f()\nprint(try(print), v)\n");

    // The word a file writes both ways in the same block.
    round_trip("local after = 2\nafter after do\n    print(after)\nend\n");
    round_trip("local function destroy(self)\n    destroy self\nend\n");
    round_trip("local enum = { Idle = 1 }\nenum State as\n    Idle\nend\nprint(enum.Idle)\n");
    round_trip("local async = false\nasync function f()\nend\nprint(async, f)\n");
    round_trip("local await = pcall\nlocal v = await f()\nprint(await(print), v)\n");
    round_trip("local import = {}\nimport { a } from \"m\"\nprint(import, a)\n");
}

/// `attribute name on target as ... end`: the `requires` clauses of a
/// contract. The body is where `requires` and `each` are keywords.
#[test]
fn attribute_contracts_parse() {
    round_trip(
        "attribute service on impl as
    requires public function Start(self)
    requires private field state: number
end
",
    );
    round_trip(
        "attribute service on impl as
    requires function Start
end
",
    );
    round_trip(
        "attribute service on impl as
    requires function Stop(self): boolean
    requires function Tick(self) -> number
end
",
    );
    round_trip(
        "attribute provider(lifecycles: Lifecycle[]) on impl as
    requires private function each lifecycles(self)
end
",
    );
    round_trip(
        "attribute provider(lifecycles: Lifecycle[]) on impl as
    requires private function each lifecycles (self)
end
",
    );
    round_trip(
        "attribute s on impl as
end
",
    );
    round_trip(
        "export attribute s on impl as
    requires function Start(self)
end
",
    );
    round_trip(
        "global attribute s on impl as
    requires function Start(self)
end
",
    );
    round_trip(
        "attribute s on struct, impl, trait, enum, interface, namespace as
    requires field x: number
end
",
    );

    // The short form is untouched.
    round_trip(
        "attribute tag(name: string) on struct
",
    );
    round_trip(
        "attribute tag(name: string) on struct, enum
",
    );
}

/// `requires` and `each` read as a keyword inside a contract body and as
/// a name everywhere else.
#[test]
fn the_clause_words_are_contextual() {
    name_case("local requires = 1\nprint(requires)\n");
    name_case("requires = 1\n");
    name_case("local requires = print\nrequires(1)\n");
    name_case("local t = { requires = 1 }\nprint(t.requires)\n");
    name_case("local each = 1\nprint(each)\n");
    name_case("local each = print\neach(1)\n");
    name_case("local t = {}\nprint(t.each)\n");
    name_case("local function requires(x)\n    return x\nend\nprint(requires(1))\n");

    // Both readings in one file.
    round_trip(
        "local requires = 1\nattribute s on impl as\n    requires function Start(self)\nend\nprint(requires)\n",
    );
}

/// A clause whose head is neither `function` nor `field` reports.
#[test]
fn a_clause_needs_a_kind_word() {
    rejects("attribute s on impl as\n    requires Start(self)\nend\n");
    rejects("attribute s on impl as\n    requires public Start(self)\nend\n");
    rejects("attribute s on impl as\n    requires function\nend\n");
    rejects("attribute s on impl as\n    requires function Start(self)\n");
}

/// A word still reserved reports when a name takes it. Six are left, and
/// each opens a declaration no expression resembles.
#[test]
fn a_reserved_word_still_reports() {
    for word in ["trait", "impl", "remote", "macro", "attribute", "namespace"] {
        let src = format!("local {word} = 1\n");
        assert!(
            !reserved_reports(&src).is_empty(),
            "{src:?} reported nothing"
        );
    }
}

/// `new Thing():method()` and `new Thing().field` chain off the
/// constructor the way `(new Thing()):method()` does.
#[test]
fn a_constructor_chains() {
    round_trip("local v = new Thing():get()\n");
    round_trip("local v = new Thing().field\n");
    round_trip("local v = new Thing(1):bump():get()\n");
    round_trip("local v = new Thing { n = 1 }:get()\n");
    round_trip("local v = new Thing { n = 1 }.n\n");
    round_trip("new Thing():bump()\n");
    round_trip("local v = new Thing()!\n");

    // An index is no statement, the way `t.x` is none in Luau.
    rejects("new Thing().field\n");
}

#[test]
fn integer_literals_parse() {
    round_trip("local n = 123i\n");
    round_trip("local h = 0xABABi + 0xf_fi\n");
    round_trip("local b = 0b1000_1000i\n");
    round_trip("local big = 0xFFFF_FFFF_FFFF_FFFFi\n");
}

// --- definitions files ----------------------------------------------------

/// A `.d.aly` is a definitions file, like `.d.luau`.
#[test]
fn a_d_aly_file_selects_definitions_mode() {
    use alloy_syntax::parser::ParseOptions;
    use std::path::Path;

    assert!(ParseOptions::for_path(Path::new("types/globals.d.aly")).definitions);
    assert!(!ParseOptions::for_path(Path::new("main.aly")).definitions);
}

/// The three declare forms parse in definitions mode.
#[test]
fn declarations_parse_in_definitions_mode() {
    let src = "declare version: string\n\n\
               declare function abs(n: number): number\n\n\
               declare class Instance extends Object\n\
               \tName: string\n\
               \t[\"Special Prop\"]: boolean\n\
               \tread ClassName: string\n\
               \tfunction IsA(self, className: string): boolean\n\
               \t[string]: any\n\
               end\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();
    let opts = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };

    let chunk = alloy_syntax::parser::parse_with(src, &lexed.toks, opts).expect("parses");

    assert_eq!(chunk.block.stmts.len(), 3);
    assert_eq!(
        alloy_syntax::printer::print_chunk(src, &lexed.toks, &chunk),
        src
    );
}

/// The same source is a syntax error in an ordinary file, like Luau itself.
#[test]
fn declarations_stay_an_error_in_ordinary_source() {
    let src = "declare function abs(n: number): number\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();

    assert!(alloy_syntax::parser::parse(src, &lexed.toks).is_err());

    // The name stays a name: this is ordinary code.
    let code = "local declare = 1\ndeclare = declare + 1\n";
    let lexed = alloy_syntax::lexer::lex(code).unwrap();

    assert!(alloy_syntax::parser::parse(code, &lexed.toks).is_ok());
}

/// The name decides the mode: .d.luau asks for definitions.
#[test]
fn the_file_name_selects_the_mode() {
    use alloy_syntax::parser::ParseOptions;
    use std::path::Path;

    assert!(ParseOptions::for_path(Path::new("types/globalTypes.d.luau")).definitions);
    assert!(ParseOptions::for_path(Path::new("x.d.lua")).definitions);
    assert!(!ParseOptions::for_path(Path::new("src/main.luau")).definitions);
    assert!(!ParseOptions::for_path(Path::new("android.luau")).definitions);
}

/*
The real 837KB globalTypes.d.luau of luau-lsp must parse whole and print
back byte for byte. The file lives in the larvae-lsp crate and the nightly
refreshes it, so this test tracks real upstream output over time.
*/
#[test]
fn the_vendored_global_types_parse_and_round_trip() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/globalTypes.d.luau"
    );

    let src = std::fs::read_to_string(path).expect("the vendored types exist");
    let lexed = alloy_syntax::lexer::lex(&src).expect("lexes");
    let opts = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let chunk = match alloy_syntax::parser::parse_with(&src, &lexed.toks, opts) {
        Ok(chunk) => chunk,

        Err(e) => {
            let at = e.offset.min(src.len().saturating_sub(1));
            let line = src[..at].matches('\n').count() + 1;

            panic!(
                "line {line}: {} near {:?}",
                e.message,
                &src[at..(at + 60).min(src.len())]
            );
        }
    };

    assert_eq!(
        alloy_syntax::printer::print_chunk(&src, &lexed.toks, &chunk),
        src
    );
}

/*
The depth guard follows the option, so a consumer with deeper generated
code raises the limit and the same source parses. The deep parse runs on
a thread with a stack to match, which is the usage the option documents:
past the default, the stack budget belongs to the caller. Zero refuses
the first statement, which pins that there is no unlimited setting.
*/
#[test]
fn max_depth_follows_the_option() {
    let src = format!("return {}1{}\n", "(".repeat(400), ")".repeat(400));
    let toks = alloy_syntax::lexer::lex(&src).unwrap().toks;

    let default = alloy_syntax::parser::parse(&src, &toks);
    assert!(default.is_err(), "400 levels beat the default guard");
    assert!(
        default.unwrap_err().message.contains("nests too deeply"),
        "the refusal names the reason"
    );

    let deep = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let toks = alloy_syntax::lexer::lex(&src).unwrap().toks;
            let opts = alloy_syntax::parser::ParseOptions {
                max_depth: 2_000,
                ..Default::default()
            };

            alloy_syntax::parser::parse_with(&src, &toks, opts).is_ok()
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(deep, "the raised limit parses the same source");

    let zero = alloy_syntax::parser::ParseOptions {
        max_depth: 0,
        ..Default::default()
    };
    let toks = alloy_syntax::lexer::lex("return 1\n").unwrap().toks;
    assert!(alloy_syntax::parser::parse_with("return 1\n", &toks, zero).is_err());
}

/// `global` left the language. The declaration still parses, as an
/// `export`, and the parser records the keyword for the report.
#[test]
fn a_global_declaration_records_its_keyword() {
    for src in [
        "global function log(msg: string)\n    print(msg)\nend\n",
        "global const MAX = 10\n",
        "global local count = 0\n",
        "global struct Vec2 as\n    x: number\nend\n",
        "global type Id = number\n",
        "global namespace Math as\n    const PI = 3.14\nend\n",
        "global attribute tag(name: string) on struct\n",
        "@deprecated\nglobal function old()\nend\n",
        "export global function f()\nend\n",
        "global export function f()\nend\n",
    ] {
        round_trip(src);
        let lexed = lexer::lex(src).unwrap();
        let chunk = parser::parse(src, &lexed.toks).unwrap();
        assert_eq!(
            chunk.global_keywords.len(),
            1,
            "one `global` keyword recorded for:\n{src}"
        );
        let span = chunk.global_keywords[0];

        assert_eq!(span.text(src, &lexed.toks), "global");
    }
}

/// `global` stays contextual: a name spelled global still reads as a name.
#[test]
fn global_is_contextual() {
    round_trip("local global = 1\nprint(global)\n");
    round_trip("global = 2\n");
    round_trip("local t = { global = 1 }\nprint(t.global)\n");

    let src = "local global = 1\n";
    let lexed = lexer::lex(src).unwrap();

    assert!(
        parser::parse(src, &lexed.toks)
            .unwrap()
            .global_keywords
            .is_empty()
    );
}

/// `namespace Name as ... end`: one name over a group of declarations.
#[test]
fn namespace_declarations_parse() {
    round_trip("namespace Math as\nend\n");
    round_trip("namespace Math as\n    const PI = 3.14\nend\n");
    round_trip("namespace Math as\n    function clamp(x: number)\n    end\nend\n");
    round_trip("namespace Math as\n    async function load()\n    end\nend\n");
    round_trip("namespace Math as\n    struct Vec2 as\n        x: number\n    end\nend\n");
    round_trip("namespace Math as\n    enum State as\n        Idle\n    end\nend\n");
    round_trip(
        "namespace Math as\n    trait Show as\n        function show(self): string\n    end\nend\n",
    );
    round_trip("namespace Math as\n    interface Named as\n        name: string\n    end\nend\n");
    round_trip("namespace Math as\n    type Id = number\nend\n");
    round_trip("namespace Math as\n    remote Hit(id: number) from client\nend\n");
    round_trip("namespace Math as\n    attribute tag(name: string) on struct\nend\n");
    round_trip("namespace Math as\n    macro twice(x)\n        x + x\n    end\nend\n");
    round_trip(
        "struct Vec2 as\n    x: number\nend\nnamespace Math as\n    impl Vec2 as\n        function len(self): number\n            return self.x\n        end\n    end\nend\n",
    );
    round_trip("export namespace Math as\n    const PI = 3.14\nend\n");
}

/// A member takes `public` or `private`, and the attributes sit above.
#[test]
fn namespace_members_take_visibility_and_attributes() {
    round_trip("namespace Math as\n    private const E = 2.7\n    public const PI = 3.14\nend\n");
    round_trip("namespace Math as\n    private function helper()\n    end\nend\n");
    round_trip("namespace Math as\n    @deprecated\n    public function old()\n    end\nend\n");
    round_trip("@deprecated(\"use Geometry\")\nnamespace Math as\nend\n");
}

/// Namespaces nest, and the inner one is a member of the outer.
#[test]
fn namespaces_nest() {
    round_trip("namespace Outer as\n    namespace Inner as\n        const X = 1\n    end\nend\n");
    round_trip("namespace Outer as\n    private namespace Inner as\n    end\nend\n");
}

/// `namespace` stays contextual before anything but `Name as`, and it
/// is a reserved word for a binding.
#[test]
fn namespace_is_contextual_and_reserved() {
    round_trip("local t = { namespace = 1 }\nprint(t.namespace)\n");
    rejects("namespace Math as\n");

    // A binding by the name reports the way `struct` and `enum` do.
    let src = "local namespace = 1\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();
    let (_, diagnostics) =
        alloy_syntax::parser::parse_lenient(src, &lexed.toks, Default::default());
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0].message.contains("reserved word"),
        "{}",
        diagnostics[0].message
    );
}

/*
The two spellings an import list rejects: an `@` on the local name after
`as`, and the name list before the whole-module name. Each error names
the form that works, so the reader has the fix in the sentence.
*/
#[test]
fn an_import_list_names_the_spelling_that_works() {
    let message = |src: &str| -> String {
        let lexed = alloy_syntax::lexer::lex(src).unwrap();

        alloy_syntax::parser::parse(src, &lexed.toks)
            .unwrap_err()
            .message
    };

    assert_eq!(
        message("import { @tagged as @t } from './m'\n"),
        "an alias in an import list takes no `@`; write `@tagged as t`"
    );
    assert_eq!(
        message("import { a }, * as M from './m'\n"),
        "the name for the whole module comes first; write `import * as M, { ... } from`"
    );
}

/// `@` marks which export the list names, and the parser records it on
/// the spec. The name itself spans the word alone.
#[test]
fn the_sigil_of_an_import_spec_is_recorded() {
    use alloy_syntax::ast::{ImportKind, Stmt};

    let src = "import { @tagged as t, version } from './m'\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();
    let chunk = alloy_syntax::parser::parse(src, &lexed.toks).unwrap();
    let Some(Stmt::Import(node)) = chunk.block.stmts.first() else {
        panic!("not an import: {:?}", chunk.block.stmts.first())
    };
    let ImportKind::Named(specs) = &node.kind else {
        panic!("not a name list: {:?}", node.kind)
    };

    assert_eq!(specs.len(), 2);
    assert!(specs[0].is_attribute);
    assert!(!specs[1].is_attribute);
    let name = lexed.toks[specs[0].name.start as usize];
    assert_eq!(&src[name.start as usize..name.end as usize], "tagged");
}

/// `import * as M, { a }` keeps the alias and the list on one statement.
#[test]
fn a_namespace_import_carries_a_name_list() {
    use alloy_syntax::ast::{ImportKind, Stmt};

    let src = "import * as M, { a, @b } from './m'\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();
    let chunk = alloy_syntax::parser::parse(src, &lexed.toks).unwrap();
    let Some(Stmt::Import(node)) = chunk.block.stmts.first() else {
        panic!("not an import")
    };
    let ImportKind::Namespace(alias, specs) = &node.kind else {
        panic!("not a namespace import: {:?}", node.kind)
    };
    let tok = lexed.toks[alias.start as usize];

    assert_eq!(&src[tok.start as usize..tok.end as usize], "M");
    assert_eq!(specs.len(), 2);
    assert!(specs[1].is_attribute);
}

/*
An interpolation hole with nothing in it. The lexer reported nothing, so
the parser stood on the piece of string that follows the `}` and quoted
its whole text, closing backtick and all: ``expected a name, found `}` ``
came out with one backtick too many. The lexer names the empty hole at
the `{` that opens it, and a report that still quotes a piece of string
shows the brace alone.
*/
#[test]
fn an_empty_interpolation_hole_reports_at_its_brace() {
    let message = |src: &str| -> String { alloy_syntax::lexer::lex(src).unwrap_err().message };
    let empty = "an interpolation hole is empty; write a value inside `{ }`";

    assert_eq!(message("local s = `{}`\n"), empty);
    assert_eq!(message("local s = `empty {} hole`\n"), empty);
    assert_eq!(message("local s = `{ }`\n"), empty);

    // The `{` the report lands on, not the `}` after it.
    let src = "local s = `{}`\n";
    assert_eq!(
        alloy_syntax::lexer::lex(src).unwrap_err().offset,
        src.find('{').unwrap()
    );

    // A hole the parser cannot read still quotes the brace alone.
    let src = "local s = `{1 + }` tail\n";
    let lexed = alloy_syntax::lexer::lex(src).unwrap();

    assert_eq!(
        alloy_syntax::parser::parse(src, &lexed.toks)
            .unwrap_err()
            .message,
        "expected a name, found `}`"
    );
}
