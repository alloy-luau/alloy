//! The documentation of every Alloy construct, in one table.
//!
//! The server answers a hover and a completion from it, and `alloy doc`
//! prints it. A key is the word as written, with its sigil: `struct`,
//! `??=`, `$dbg`, `@derive`, `derive:Eq`, and `topic:strict` for the
//! articles that no token names.

/// The Alloy keywords a plain completion offers. The child lists Luau's
/// own; these are the words the emit removes.
pub const ALLOY_KEYWORDS: &[&str] = &[
    "struct",
    "enum",
    "trait",
    "impl",
    "interface",
    "remote",
    "macro",
    "attribute",
    "match",
    "case",
    "default",
    "const",
    "async",
    "await",
    "try",
    "new",
    "delete",
    "import",
    "export",
    "where",
    "extends",
    "read",
    "write",
];

pub const TABLE: &[(&str, &str)] = &[
    // Operators
    (
        "?",
        "```alloy\nlocal label = n > 2 ? \"big\" : \"small\"\n```\nThe ternary: `c ? a : b` is `if c then a else b`. A `:` that touches its receiver, `x:upper()`, stays a method call inside the branches; the else marker has a space before it.",
    ),
    (
        ":",
        "```alloy\nc ? a : b\n```\nThe else of a ternary. Touching a name it is a method call, `obj:method()`; after a name with a space it starts a type, `x: T`.",
    ),
    (
        "??=",
        "```alloy\na ??= b\n```\nAssigns `b` to `a` when `a` is nil. An index on the left runs once.\n\nEmits `if a == nil then a = b end`.",
    ),
    (
        "??",
        "```alloy\na ?? b\n```\nNil coalescing: `a` when it is not nil, else `b`. Unlike `or`, `false` passes through.",
    ),
    (
        "?.",
        "```alloy\na?.b\n```\nSafe index: nil when `a` is nil, else `a.b`. The `?` guards the rest of the chain, and `!` ends the guard.\n\nEmits `a.b` under a nil check on a temp.",
    ),
    (
        "?[",
        "```alloy\na?[k]\n```\nSafe index by key: nil when `a` is nil, else `a[k]`.",
    ),
    (
        "?:",
        "```alloy\na?:m(x)\n```\nSafe method call: nil when `a` is nil, and `x` does not evaluate. Else `a:m(x)`.",
    ),
    (
        "?(",
        "```alloy\nf?(x)\n```\nSafe call: nil when `f` is nil, and `x` does not evaluate.",
    ),
    (
        "->",
        "```alloy\na->b\n```\nChild lookup: `a:FindFirstChild(\"b\")`, nil when the child is missing. The right side is a Name, a string, an interpolated string, or `[expr]`.\n\nAfter a parameter list, `-> T` is the return type.",
    ),
    (
        "=>",
        "```alloy\na=>b\n```\nBlocking child lookup: `a:WaitForChild(\"b\")`. With `wait_timeout` under `[emit]` in alloy.toml the call takes that timeout and can return nil.",
    ),
    (
        "!",
        "```alloy\na!\n```\nNon-nil assert. Throws when `a` is nil, with the source text in the message. The type loses its `?`.\n\nEmits `if a == nil then error(...) else a` on a temp.",
    ),
    (
        "...",
        "```alloy\n{ ...base, x = 1 }\n```\nTable spread. Emits `spread(base, { x = 1 })` from the std. A positional entry after a spread is an error.",
    ),
    (
        "is",
        "```alloy\nx is T\n```\nType test. `T` is a type name: a primitive, a Roblox datatype, an Instance class, `Enum.Name`, or an Alloy struct or enum. `x is not T` negates.\n\nEmits `type`, `typeof`, `IsA`, or a metatable check, chosen by the name.",
    ),
    (
        "in",
        "```alloy\nx in t\n```\nMembership. Searches an Array by value, a Set or HashMap through `:has`, a string by substring, and a raw table by key.\n\nEmits `contains(t, x)` from the std.",
    ),
    (
        "bor",
        "```alloy\na bor b\n```\nBitwise or. Emits `bit32.bor(a, b)`. Operands are 32-bit unsigned.",
    ),
    (
        "band",
        "```alloy\na band b\n```\nBitwise and. Emits `bit32.band(a, b)`.",
    ),
    (
        "bxor",
        "```alloy\na bxor b\n```\nBitwise exclusive or. Emits `bit32.bxor(a, b)`.",
    ),
    (
        "bnot",
        "```alloy\nbnot a\n```\nBitwise not. Emits `bit32.bnot(a)`. `$bnot(x)` takes a parenthesized operand.",
    ),
    (
        "shl",
        "```alloy\na shl n\n```\nShift left. Emits `bit32.lshift(a, n)`.",
    ),
    (
        "shr",
        "```alloy\na shr n\n```\nShift right. Emits `bit32.rshift(a, n)`.",
    ),
    // Declarations
    (
        "struct",
        "```alloy\nstruct Name as\n    field: T\nend\n```\nA record with fields. `impl Name` adds methods. `Name { x = 1 }` is the raw constructor and `new Name(...)` calls `Name.new`. `@derive(Eq, Debug, Clone)` on the line before generates methods.\n\nEmits a table type plus a metatable with `__index`.",
    ),
    (
        "impl",
        "```alloy\nimpl Name ... end\nimpl Trait for Name ... end\n```\nMethods for a struct, an enum, or a foreign type such as `Vector3` or `string`. A foreign impl needs `export` and works project wide.\n\nEmits functions on the class table.",
    ),
    (
        "trait",
        "```alloy\ntrait Name\n    function m(self): T\nend\n```\nA behavior contract: method signatures, with a body as a default. `impl Trait for Name` implements it and `<T: Trait>` bounds a generic. Operator traits map to metamethods: `Add`, `Eq`, `Ord`, `Call`, `Display`.\n\nEmits a type with the method signatures.",
    ),
    (
        "interface",
        "```alloy\ninterface Name extends Base as\n    field: T\nend\n```\nA structural object shape, sugar over `type`. A known base flattens into one table type; any other base joins with `&`.",
    ),
    (
        "enum",
        "```alloy\nenum Name as\n    Unit\n    Payload(T)\nend\n```\nVariants with optional payloads. A unit enum is a string union at runtime; a payload enum is a tagged table. `match` takes it apart and `impl Name` adds methods.",
    ),
    (
        "match",
        "```alloy\nmatch expr with\n    case Pat then ...\n    default ...\nend\n```\nPattern match, as a statement or an expression. A pattern binds payloads, `_` matches anything, `or` joins alternatives, and `and expr` is a guard. An exhaustive match needs no `default`.\n\nEmits an if-chain on the tag.",
    ),
    ("with", "Opens the arms of a `match`."),
    (
        "case",
        "```alloy\ncase Pat then ...\n```\nOne arm of a `match`. The arm runs to the next `case`, `default`, or `end`.",
    ),
    (
        "default",
        "The fallback arm of a `match`. Required when the scrutinee is a literal, a struct, or a table.",
    ),
    (
        "as",
        "Marks where a declaration's name, or its `extends` list, ends and the members begin: `struct Vec2 as x: number end`.\n\nIn `import { }` and `export { }`, renames a name.",
    ),
    (
        "extends",
        "```alloy\ninterface Name extends Base, Other as\n```\nThe base shapes of an interface.",
    ),
    (
        "import",
        "```alloy\nimport { a, b as c, type T } from \"./m\"\nimport * as M from \"./m\"\nimport Name from \"./m\"\nimport type { T } from \"./m\"\n```\nBrings names from another module into this file. `{ }` picks exports by name, `as` renames one, `* as M` takes the whole module, and a bare name takes its default export. `type` marks a type-only import, which costs nothing at runtime.\n\n`import(\"./m\")` is the expression form: a `require`, typed from the module when the path is a string or an instance chain. `import<<T>>(expr)` gives a dynamic path the type `T`; without it the value is `unknown`.",
    ),
    (
        "export",
        "```alloy\nexport local x = 1\nexport function f() end\nexport { a, b as c }\nexport default expr\n```\nAdds a name to the table the module returns at the end of its scope.",
    ),
    (
        "from",
        "The module path of an `import` or `export`, or the side that fires a `remote`: `from client`, `from server`.",
    ),
    (
        "const",
        "```alloy\nconst x = expr\n```\nA binding that cannot be reassigned; the value stays mutable. Luau has `const` of its own, so the keyword passes through and a reassignment is a compile error there too.",
    ),
    (
        "async",
        "```alloy\nasync function f() ... end\nasync do ... end\n```\nReturns a Future. The body runs on `task.spawn` under `xpcall`, and the Future memoizes the result.",
    ),
    (
        "await",
        "```alloy\nawait expr\n```\nYields until the Future settles, then returns its value or rethrows its error. Accepts a Future or any value with `andThen`.",
    ),
    (
        "try",
        "```alloy\ntry expr\ntry do ... end\n```\nReturns early with the `Err`, inside a function that returns `Result`. `try do` is a block whose value is a Result.",
    ),
    (
        "macro",
        "```alloy\nmacro name(params) ... end\n```\nA compile-time template with expression parameters, called as `$name(...)`. Every `local` in the body is renamed per expansion.",
    ),
    (
        "new",
        "```alloy\nnew Name(...)\nnew Name(...) { Field = value }\n```\nConstructs a value. `new Name(...)` calls the constructor a struct's impl wrote, `new` or `New`, or the `new` of a Roblox datatype, an Instance, or any class; braces after it set fields on the new value, one per line. `new Name { ... }` is a struct's fields form, the only way to construct one that writes no constructor. A struct never constructs without `new`.",
    ),
    ("delete", "```alloy\ndelete expr\n```\n`expr:Destroy()`."),
    (
        "attribute",
        "```alloy\nattribute name(params) on target, ...\n```\nDeclares an attribute: metadata the compiler reads and `Attributes` reads at runtime. Targets: function, struct, enum, variant, field, param, remote, interface, type.",
    ),
    ("on", "The targets of an `attribute` declaration."),
    (
        "remote",
        "```alloy\nremote Name(params) from client\nremote function Name(params): R from server\n```\nA typed channel between server and client. The compiler generates the serializer. Use `.fire`, `.fire_all`, `.on`, and `.call`.",
    ),
    (
        "where",
        "```alloy\nfor x in xs where cond do\nif local x = f() where cond then\n```\nA filter. In a loop it emits `if not (cond) then continue end` on the `do` line.",
    ),
    (
        "satisfies",
        "```alloy\nexpr satisfies T\n```\nChecks the literal against `T` under contextual typing and reports a key `T` does not name. The type is `T`.",
    ),
    (
        "read",
        "```alloy\nread x: T\n```\nA read-only field. `read T[]` is `ReadArray<T>`.",
    ),
    (
        "write",
        "```alloy\nwrite x: T\n```\nA write-only field. `write T[]` is `WriteArray<T>`.",
    ),
    // Intrinsics
    (
        "$dbg",
        "```alloy\n$dbg(expr)\n```\nPrints `file:line: <source text> = <value>` and returns the value.",
    ),
    (
        "$todo",
        "```alloy\n$todo(\"why\")\n```\n`error(\"todo at file:line: why\")`.",
    ),
    (
        "$unreachable",
        "```alloy\n$unreachable()\n```\n`error(\"unreachable at file:line\")`.",
    ),
    (
        "$assert",
        "```alloy\n$assert(cond)\n```\n`assert(cond, \"assertion failed: <source text>\")`.",
    ),
    (
        "$assert_eq",
        "```alloy\n$assert_eq(a, b)\n```\nAn `error` that names both source texts and both values when they differ.",
    ),
    (
        "$nameof",
        "```alloy\n$nameof(a.b.c)\n```\nThe string `\"c\"`.",
    ),
    (
        "$stringify",
        "```alloy\n$stringify(expr)\n```\nThe source text of `expr` as a string.",
    ),
    (
        "$matches",
        "```alloy\n$matches(e, Pat(_))\n```\nA pattern test without a `match`: `e.tag == \"Pat\"`.",
    ),
    (
        "$bnot",
        "```alloy\n$bnot(x)\n```\n`bit32.bnot(x)`, for a parenthesized operand.",
    ),
    // Derive names, keyed for a hover inside `@derive( )`.
    (
        "derive:Eq",
        "```alloy\n@derive(Eq)\n```\nGenerates `__eq`: two values are equal when every field is.",
    ),
    (
        "derive:Debug",
        "```alloy\n@derive(Debug)\n```\nGenerates `debug` and `__tostring`: the struct's name and its fields, as text.",
    ),
    (
        "derive:Clone",
        "```alloy\n@derive(Clone)\n```\nGenerates `clone`: a shallow copy with the same metatable.",
    ),
    (
        "derive:Serialize",
        "```alloy\n@derive(Serialize)\n```\nGenerates `to_table` and `from_table`, the plain-table forms for storage and remotes.",
    ),
    // Attributes
    (
        "@derive",
        "```alloy\n@derive(Eq, Debug, Clone)\n```\nGenerates methods from the field list: `Eq` is `__eq`, `Ord` is `__lt` and `__le`, `Debug` is `debug` and `__tostring`, `Clone` is `clone`, `Serialize` is `to_table` and `from_table`. Any trait whose methods all have defaults derives too.",
    ),
    (
        "@native",
        "```alloy\n@native\nfunction hot() end\n```\nLuau's own: compiles the function natively. Passes through to the emit.",
    ),
    (
        "@checked",
        "```alloy\n@checked\nfunction f(x: number) end\n```\nLuau's own: the runtime checks the argument types of a native-typed function. Passes through to the emit.",
    ),
    (
        "@deprecated",
        "```alloy\n@deprecated\nfunction old() end\n```\nLuau's own: a call to the function is a lint. Passes through to the emit.",
    ),
    (
        "@inline",
        "```alloy\n@inline\nfunction small() end\n```\nLuau's own: asks the compiler to inline the function. Passes through to the emit.",
    ),
    (
        "@noinline",
        "```alloy\n@noinline\nfunction big() end\n```\nLuau's own: keeps the function out of line. Passes through to the emit.",
    ),
    (
        "@rename",
        "```alloy\n@rename(\"regen_per_second\")\nregen: number\n```\nThe key a field takes in the tables `@derive(Serialize)` writes and reads.\n\n**Applies to** `field`",
    ),
    (
        "@skip",
        "```alloy\n@skip\nconnection: RBXScriptConnection?\n```\nLeaves a field out of the tables `@derive(Serialize)` writes and reads: a handle, a cache, anything that is not data.\n\n**Applies to** `field`",
    ),
    (
        "@test",
        "```alloy\n@test\nfunction name() ... end\n```\nA test that `alloy test` runs. The ship artifact blanks it; the check artifact keeps it typed.",
    ),
    (
        "@unreliable",
        "```alloy\n@unreliable\nremote ...\n```\nAn UnreliableRemoteEvent. An error when the packed size can pass 900 bytes.",
    ),
    (
        "@ratelimit",
        "```alloy\n@ratelimit(count, seconds)\n```\nA per-player token bucket on the server, for client-to-server remotes.",
    ),
    (
        "@timeout",
        "```alloy\n@timeout(seconds)\n```\nRejects the caller's Future after the window, on a `remote function`.",
    ),
    (
        "@immediate",
        "```alloy\n@immediate\n```\nSends the remote at once, outside the per-frame batch.",
    ),
    (
        "@validate",
        "```alloy\n@validate(fn)\n```\nA server-side predicate on the decoded arguments, after the shape check.",
    ),
    (
        "@u8",
        "The wire width of a `number` parameter on a `remote`: 8-bit unsigned.",
    ),
    (
        "@u16",
        "The wire width of a `number` parameter on a `remote`: 16-bit unsigned.",
    ),
    (
        "@u32",
        "The wire width of a `number` parameter on a `remote`: 32-bit unsigned.",
    ),
    (
        "@i8",
        "The wire width of a `number` parameter on a `remote`: 8-bit signed.",
    ),
    (
        "@i16",
        "The wire width of a `number` parameter on a `remote`: 16-bit signed.",
    ),
    (
        "@i32",
        "The wire width of a `number` parameter on a `remote`: 32-bit signed.",
    ),
    (
        "@f32",
        "The wire width of a `number` parameter on a `remote`: 32-bit float. The default is f64.",
    ),
    (
        "declare",
        "```alloy\ndeclare function name(params): R\ndeclare extern type Name with ... end\ndeclare name: T\n```\nA definition-file statement. Luau's own definition syntax, in a `.d.aly`, which compiles to `.d.luau` and joins the analyzer's definitions; no runtime.",
    ),
    // The std: ambient names, no import. The child sees `__alloy.Name`.
    (
        "HashMap",
        "```alloy\nlocal m = HashMap.new()\nm:set(key, value)\nlocal v = m:get(key)\n```\nA map with methods, from the std. `HashMap.from(t)` wraps a table.\n\nMethods: `get`, `set`, `has`, `remove`, `len`, `keys`, `values`, `entries`, `get_or_insert`, `clear`.",
    ),
    (
        "Set",
        "```alloy\nlocal s = Set.new()\ns:add(value)\n```\nA set with methods, from the std. `Set.from(t)` wraps an array.\n\nMethods: `add`, `has`, `remove`, `len`, `union`, `intersection`, `difference`, `to_array`.",
    ),
    (
        "Array",
        "```alloy\nlocal xs = [ 1, 2, 3 ]\nlocal ys: Array<number> = xs:map(f)\n```\nThe array type of the std. An array literal carries its metatable, so methods work on it. `T[]` and `Array<T>` name the same type.\n\nMethods: `len`, `is_empty`, `push`, `pop`, `first`, `last`, `map`, `filter`, `find`, `find_index`, `contains`, `index_of`, `for_each`, `reduce`, `slice`, `concat`, `reverse`, `sort_by`, `join`.",
    ),
    (
        "Future",
        "```alloy\nlocal f = Future.delay(1)\nlocal v = await f\n```\nA memoized task, from the std. `async function` returns one; `await` yields until it settles.\n\nStatics: `resolve`, `reject`, `delay`, `all`, `race`, `any`, `all_settled`. Methods: `cancel`, `is_settled`, `andThen`.",
    ),
    (
        "Result",
        "```alloy\nlocal r: Result<number, string> = Ok(1)\nlocal v = r:unwrap_or(0)\n```\n`Ok(value)` or `Err(error)`, from the std. `try` turns a throw into an Err; `?` returns an Err early.\n\nMethods: `unwrap`, `expect`, `unwrap_or`, `map`, `map_err`, `is_ok`, `is_err`, `ok`. `Result.pcall(f, ...)` wraps a throwing call.",
    ),
    (
        "Ok",
        "```alloy\nOk(value)\n```\nThe success case of a `Result`.",
    ),
    (
        "Err",
        "```alloy\nErr(error)\n```\nThe failure case of a `Result`.",
    ),
    (
        "Symbol",
        "```alloy\nlocal key = Symbol.new(\"name\")\n```\nA unique key that no string can collide with, from the std.",
    ),
    (
        "Signal",
        "```alloy\nlocal changed = Signal.new()\nlocal conn = changed:Connect(function(id, value) end)\nchanged:Fire(1, 2)\n```\nA typed signal, from the std. `Signal.new<T...>()` fires `T...`; handlers run on reused threads, in order, and may disconnect during a fire.\n\nMethods: `Connect`, `Once`, `Wait`, `Fire`, `FireDeferred`, `DisconnectAll`, `Destroy`. A connection has `Connected` and `Disconnect`.\n\n`Signal.collect(source)` turns any signal with `Connect` or `connect`, a Roblox one included, into an iterator that drains the queued events in order, plus the connection: `for id, value in Signal.collect(changed) do`. `Signal.wrap(source)` gives a Signal that fires with the source.",
    ),
    (
        "Partial",
        "```alloy\ntype Partial<T> = { [K in keyof T]: T[K]? }\nlocal changes: Partial<Entity> = { name = \"b\" }\n```\nEvery field of `T`, optional. A language-level type; the std builds it with a type function. A `type Partial` in a file replaces it there.",
    ),
    (
        "Readonly",
        "```alloy\ntype Readonly<T> = { [K in keyof T]: read T[K] }\n```\nEvery field of `T`, read only. A language-level type; the std builds it with a type function. A `type Readonly` in a file replaces it there.",
    ),
    (
        "Sink",
        "```alloy\ntype Sink<T> = { [K in keyof T]: write T[K] }\n```\nEvery field of `T`, write only. A language-level type; the std builds it with a type function. A `type Sink` in a file replaces it there.",
    ),
    (
        "Attributes",
        "```alloy\nlocal value = Attributes.get(Target, attr)\n```\nDeclared metadata, readable at runtime. `get`, `field`, `variant`, `of`, and `fields` read what `@attr` declared.",
    ),
    (
        "@sealed",
        "```alloy\n@sealed\nstruct Config as\n    volume: number\nend\nlocal c = new Config { volume = 1 }\nc.volume = 2  -- fine: declared\nc.volme = 2   -- error: Config has no field volme\n```\nA struct is open: a write to a name it does not declare makes a new key, and a typo goes unnoticed. `@sealed` makes that write an error at runtime, with the struct's name and the key in the message, and the check artifact rejects it from the table type. A declared field set to nil writes through. To stop writes to a declared field, mark it `read`.\n\nEmits `__newindex` on the class table.",
    ),
    (
        "topic:strict",
        "**Strict by default**\n\nEvery `.aly` file checks in Luau strict mode unless the project says otherwise. `alloy init` writes `.luaurc` and `.config.luau` with `languageMode = \"strict\"`, and the language server gives a workspace with neither file the same setting. A `--!nonstrict` or `--!nocheck` line at the top of a file still wins for that file.\n\nOn top of the checker, the compiler holds these at compile time:\n\n  match           every variant has an arm, or a `default` (`alloy doc exhaustive`)\n  new Name { }    every field without a default is set; no unknown field\n  impl T for S    every method of the trait, with the trait's arity\n  @sealed         no write to an undeclared field, at runtime and at check time\n  remote          no function, thread, Future, or Signal in a parameter (`alloy doc wire`)\n\n`alloy lint` adds the nil discipline and, with `[lint] strict = true`, the no-implicit-any rules (`alloy doc lints`).",
    ),
    (
        "topic:exhaustive",
        "**Exhaustive match**\n\nA `match` with no `default` must cover every value. For an enum, that is every variant, each with payloads that cover; `Color.Red`, a bare unit variant, `Some(x)`, `_`, and `a or b` all count. When a variant has no arm, the compiler names it:\n\n```\nthis match is not exhaustive: `Msg` has no arm for `Leave`; add it or a `default` arm\n```\n\nA literal, a struct, or an array pattern set needs a `default`. A guarded arm proves nothing about coverage.\n\nThe other direction is a lint: a `default` under arms that already cover every variant never runs, and `unreachable_default` says so, because that default would hide the next variant added.",
    ),
    (
        "topic:wire",
        "**Wire types**\n\nA `remote` carries data across the network. A parameter typed as a function, a `thread`, a `Future`, or a `Signal` cannot be serialized, so the declaration is an error:\n\n```\nremote Ping(cb: () -> ()) from client\n-- remote `Ping`: parameter `cb` has type `() -> ()`, which is a function type; a remote carries only data\n```\n\nSend an id, a name, or a plain table, and keep the callback on the side that owns it. `@u8`, `@u16`, `@i32`, `@f32` and the other width attributes set how a number packs.",
    ),
    (
        "topic:lint",
        "**alloy lint**\n\nRuns the lints over the project, or over one file. A lint is advice: the code runs, and the lint names a habit that costs bugs. Flux is the set that knows the Alloy form of a Luau habit: `a and a.b` for `a?.b`, `if f then f() end` for `f?()`, `typeof(x) == \"T\"` for `x is T`, `\"a\" .. b` for `` `a{b}` ``. Each of those carries its rewrite, and `--fix` applies the ones that keep the program the same.\n\n```\nalloy lint                 the project of the nearest alloy.toml\nalloy lint src/game.aly    one file\nalloy lint --fix           apply the rewrites that keep the program the same\nalloy lint --strict        the strict-only lints too\nalloy lint --deny-warnings fail on any hit\nalloy lint --list          every lint with its default level\n```\n\nThe `[lint]` table of alloy.toml sets the levels:\n\n```toml\n[lint]\nstrict = true\ndeny = [\"optional_access\"]\nallow = [\"concat_interpolation\"]\n```\n\n`alloy doc lints` lists them; `alloy doc <name>` explains one. The language server shows the same lints as warnings, with the rewrite in the message.",
    ),
    (
        "topic:fmt",
        "**alloy fmt**\n\nAnneal formats `.aly` and `.alx` files in place. The layout comes from the tokens and the `[fmt]` options, not from how the author laid the code out: the spacing between tokens, which bracket groups break, the quotes of a string, the parentheses of a call. Statements keep their lines, at most one blank line stays between them, and a long string or a long comment keeps its text. The program is the same afterwards; the token stream changes only where an option asks for a rewrite.\n\n```\nalloy fmt                  the sources of the project\nalloy fmt src/ui.aly dir/  the paths given\nalloy fmt --check          write nothing; fail when a file would change\n```\n\nA bracket group, the arguments of a call, a table, or an array, stays on one line when it fits in `column_width`, and breaks one element per line when it does not, or when a trailing comma in the source asks for it. A callback argument indents its body once: `foo(function()` opens one level, not two.\n\nThe options, with their defaults:\n\n```toml\n[fmt]\ncolumn_width = 100\nline_endings = \"unix\"                 # windows\nindent_type = \"spaces\"                # tabs\nindent_width = 4\nquote_style = \"auto-prefer-double\"    # auto-prefer-single, force-double, force-single, preserve\nleading_zero = \"add\"                  # `.5` becomes `0.5`; strip, preserve\ncall_parentheses = \"always\"           # no-single-string, no-single-table, none, input\nspace_after_function_names = \"never\"  # definitions, calls, always\ncollapse_simple_statement = \"never\"   # function-only, conditional-only, always\nblock_newline_gaps = \"never\"          # preserve keeps a blank line at a block's edge\nmagic_trailing_comma = true           # a trailing comma keeps a group expanded\ntrailing_comma = true                 # an expanded group ends its last element with a comma\nspace_inside_braces = true            # { a }\nspace_inside_parens = false           # f(a)\nspace_inside_brackets = false         # t[k]\nspace_inside_array = true             # [ 1, 2 ]; Alloy's own\nalign_struct_fields = false           # the `:` of a struct's fields line up; Alloy's own\nexclude = []                          # paths to leave alone; `*` matches any run\n\n[fmt.call_chains]\nstyle = \"preserve\"                    # method: break before each call past the first; full: before every call\nmin_calls = 3\n\n[fmt.sort_requires]\nenabled = false                       # sort the `import` lines at the top of the file\ngrouping = \"flat\"                     # by-kind: aliases, then absolute, then relative paths\n\n[fmt.alx]\nattribute_quotes = \"double\"           # single, preserve\nbracket_same_line = false             # the `>` of a broken tag on the last attribute's line\nattribute_per_line = false            # every attribute of a broken tag on its own line\nself_closing_space = true             # <Frame />\ntext_wrap = \"fill\"                    # preserve keeps the author's line breaks in text\nblank_lines = true                    # a blank line between children stays\n```\n\nThe names follow larvae and stylua where the option is theirs, so a config ports over. In an `.alx` file the code formats the same way and the markup prints from its tree: a tag that fits stays on one line, a tag that does not breaks its attributes and then its children, and text flows with the holes in it.",
    ),
    (
        "topic:check",
        "**alloy check**\n\nCompiles every source of the project and writes nothing. The report carries the compiler's diagnostics and the lints at their `[lint]` levels, and the exit code is one on any diagnostic or denied lint. `alloy check <file>` does the same for one file.",
    ),
    (
        "topic:build",
        "**alloy build**\n\nCompiles every `.aly` and `.alx` under `[build] in` to Luau under `[build] out`, mirroring the tree, and writes the runtime as `alloy.luau` at the output root. A file that did not change is not rewritten, so a Rojo sync stays quiet. `artifact = \"ship\"` writes the code Roblox runs; `\"check\"` writes what luau-lsp sees, with the types kept. `clean = true` removes an output whose source is gone.\n\n```\nalloy build                one project\nalloy build src/game.aly   one file, to stdout\nalloy build --check --map  the check artifact and its chunk map\n```",
    ),
    (
        "topic:config",
        "**alloy.toml**\n\n```toml\n[build]\nin = \"src\"\nout = \"build\"\nexclude = []\nclean = false\nartifact = \"ship\"\n\n[emit]\n# wait_timeout = 5\n# std_require = \"@alloy\"\n# erase_type_imports = false\n\n[lint]\nstrict = false\ndeny = []\nwarn = []\nallow = []\n```\n\nEvery key has a default, and an unknown key is an error. `alloy init` writes the file, plus `.luaurc` and `.config.luau` when the folder has neither: strict mode and the `@alloy` alias for the runtime the build writes. `[project]` and `[mount]` describe the DataModel tree; `alloy doc mount` explains them.",
    ),
    (
        "topic:luaurc",
        "**.luaurc and .config.luau**\n\nLuau reads its settings from `.luaurc`, a JSON file, or from `.config.luau`, a Luau chunk that returns `{ luau = { ... } }`. With both, `.config.luau` wins. Alloy reads and writes both the same way: `languageMode` and `aliases` are the keys it uses.\n\n```json\n{ \"languageMode\": \"strict\", \"aliases\": { \"alloy\": \"./build/alloy\" } }\n```\n\n```luau\nreturn { luau = { languagemode = \"strict\", aliases = { alloy = \"./build/alloy\" } } }\n```\n\nThe language server copies the file into its mirror, and adds `languagemode = \"strict\"` when the file sets no mode, so the default stays strict.",
    ),
    (
        "topic:directives",
        "**Directives**\n\nA comment that starts with `--@alloy-` silences diagnostics: the compiler's, the lints, and the checker's type errors, which the language server drops on the silenced lines before the editor sees them.\n\n```alloy\n--@alloy-nocheck        this file: nothing is reported\n--@alloy-ignore         the next line with code is silent\nlocal x = y.z --@alloy-ignore   at the end of a line: that line\n```\n\nUse one for a line the new solver gets wrong, and say why in the comment beside it. Luau's own `--!strict`, `--!nonstrict`, and `--!nocheck` pass through and set the checker's mode for the file.\n\nEvery diagnostic carries the book section it belongs to as its code, `Alloy(4.2)`; the number links to the section in the editor, and `alloy doc 4.2` prints it.",
    ),
    (
        "topic:mount",
        "**Mounts and project files**\n\nOne table in alloy.toml says where each folder lands in the DataModel, and `alloy build` writes every file that follows from it:\n\n```toml\n[project]\nname = \"game\"\nruntime = \"@game/ReplicatedStorage/Alloy\"\n\n[mount]\n# alias = [path, mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nclient = [\"src/client\", \"@game/StarterPlayer/StarterPlayerScripts/Client\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n```\n\n  default.project.json        a Rojo project over the sources\n  .alloy/build.project.json   the same tree over the compiled output, for `rojo serve` and `rojo build`\n  .alloy/sourcemap.json       the instance tree with the source paths; the language server reads it\n  .luaurc                     gains an alias per mount, so `@pkg/jecs` resolves in the editor\n\nRoblox reads no `.luaurc`, so the ship artifact rewrites `require(\"@pkg/jecs\")` into the relative instance path from the file's mount, `../../ReplicatedStorage/Packages/jecs` from a server script, and requires the runtime the same way. A path under `[build] in` points at its output in the build project; any other path, such as a package folder, is mounted as it is. `.server.` and `.client.` name the script class, `init` names its directory, and a `StarterPlayerScripts` between a service and a leaf keeps its own class.",
    ),
];

/// The Markdown for a key.
pub fn lookup(key: &str) -> Option<&'static str> {
    TABLE.iter().find(|(k, _)| *k == key).map(|(_, text)| *text)
}

/// Every documented key with the prefix: `@` for the attributes, `$`
/// for the intrinsics, `derive:` for the derive names, `topic:` for
/// the articles.
pub fn keys_with_prefix(prefix: &str) -> Vec<&'static str> {
    TABLE
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| k.starts_with(prefix))
        .collect()
}

/// The website of the book, where a diagnostic's code links.
pub const SITE: &str = "https://alloy-luau.github.io";

/// One numbered section of the book, as the website lays it out. A
/// diagnostic carries a section's number as its code, `Alloy(4.2)`, and
/// the number links to the section; `alloy doc 4.2` prints it.
pub struct Section {
    pub number: &'static str,
    /// The anchor on the book page.
    pub id: &'static str,
    pub title: &'static str,
    /// The doc entry that explains the section, when one does.
    pub key: Option<&'static str>,
}

pub const BOOK: &[Section] = &[
    Section {
        number: "1",
        id: "intro",
        title: "Introduction",
        key: None,
    },
    Section {
        number: "2",
        id: "getting-started",
        title: "Getting started",
        key: None,
    },
    Section {
        number: "2.1",
        id: "install",
        title: "Install",
        key: None,
    },
    Section {
        number: "2.2",
        id: "first-project",
        title: "A first project",
        key: Some("topic:build"),
    },
    Section {
        number: "2.3",
        id: "editor",
        title: "The editor",
        key: None,
    },
    Section {
        number: "3",
        id: "language",
        title: "The language",
        key: None,
    },
    Section {
        number: "3.1",
        id: "safe",
        title: "Safe access",
        key: Some("?."),
    },
    Section {
        number: "3.2",
        id: "modules",
        title: "Modules",
        key: Some("import"),
    },
    Section {
        number: "3.3",
        id: "async",
        title: "Async and Futures",
        key: Some("async"),
    },
    Section {
        number: "3.4",
        id: "enums",
        title: "Enums and match",
        key: Some("enum"),
    },
    Section {
        number: "3.5",
        id: "bindings",
        title: "Conditional bindings",
        key: Some("match"),
    },
    Section {
        number: "3.6",
        id: "structs",
        title: "Structs and traits",
        key: Some("struct"),
    },
    Section {
        number: "3.7",
        id: "interfaces",
        title: "Interfaces and types",
        key: Some("interface"),
    },
    Section {
        number: "3.8",
        id: "sugar",
        title: "Sugar",
        key: Some("?"),
    },
    Section {
        number: "3.9",
        id: "extensions",
        title: "Extensions",
        key: Some("impl"),
    },
    Section {
        number: "3.10",
        id: "macros",
        title: "Macros",
        key: Some("macro"),
    },
    Section {
        number: "3.11",
        id: "attributes",
        title: "Attributes",
        key: Some("attribute"),
    },
    Section {
        number: "3.12",
        id: "remotes",
        title: "Remotes",
        key: Some("remote"),
    },
    Section {
        number: "3.13",
        id: "markup",
        title: "Markup",
        key: None,
    },
    Section {
        number: "3.14",
        id: "tests",
        title: "Tests",
        key: Some("@test"),
    },
    Section {
        number: "4",
        id: "strict",
        title: "Strict by default",
        key: Some("topic:strict"),
    },
    Section {
        number: "4.1",
        id: "contracts",
        title: "The contracts",
        key: Some("topic:strict"),
    },
    Section {
        number: "4.2",
        id: "exhaustive",
        title: "Exhaustive match",
        key: Some("topic:exhaustive"),
    },
    Section {
        number: "4.3",
        id: "wire",
        title: "Wire types",
        key: Some("topic:wire"),
    },
    Section {
        number: "4.4",
        id: "directives",
        title: "Directives",
        key: Some("topic:directives"),
    },
    Section {
        number: "5",
        id: "tooling",
        title: "Tooling",
        key: None,
    },
    Section {
        number: "5.1",
        id: "build",
        title: "alloy build",
        key: Some("topic:build"),
    },
    Section {
        number: "5.2",
        id: "check",
        title: "alloy check",
        key: Some("topic:check"),
    },
    Section {
        number: "5.3",
        id: "lint",
        title: "alloy lint",
        key: Some("topic:lint"),
    },
    Section {
        number: "5.4",
        id: "fmt",
        title: "alloy fmt",
        key: Some("topic:fmt"),
    },
    Section {
        number: "5.5",
        id: "doc",
        title: "alloy doc",
        key: None,
    },
    Section {
        number: "5.6",
        id: "config",
        title: "alloy.toml",
        key: Some("topic:config"),
    },
    Section {
        number: "5.7",
        id: "luaurc",
        title: ".luaurc and .config.luau",
        key: Some("topic:luaurc"),
    },
    Section {
        number: "5.8",
        id: "mount",
        title: "Mounts and project files",
        key: Some("topic:mount"),
    },
    Section {
        number: "6",
        id: "reference",
        title: "Reference",
        key: None,
    },
    Section {
        number: "6.1",
        id: "ref-keywords",
        title: "Keywords",
        key: None,
    },
    Section {
        number: "6.2",
        id: "ref-operators",
        title: "Operators",
        key: None,
    },
    Section {
        number: "6.3",
        id: "ref-intrinsics",
        title: "Intrinsics",
        key: None,
    },
    Section {
        number: "6.4",
        id: "ref-attributes",
        title: "Attributes",
        key: None,
    },
    Section {
        number: "6.5",
        id: "ref-derives",
        title: "Derives",
        key: None,
    },
    Section {
        number: "6.6",
        id: "ref-std",
        title: "Standard library",
        key: None,
    },
    Section {
        number: "6.7",
        id: "lints",
        title: "Lints",
        key: Some("lints"),
    },
];

/// The section a number names.
pub fn section(number: &str) -> Option<&'static Section> {
    BOOK.iter().find(|s| s.number == number)
}

/// The link for a section number.
pub fn book_url(number: &str) -> Option<String> {
    section(number).map(|s| format!("{SITE}/docs/#{}", s.id))
}

/// The lints' section number: every lint's code.
pub const LINT_CODE: &str = "6.7";

/// The book section a compiler diagnostic belongs to, from its text.
/// The diagnostics name what they are about; the first match wins, from
/// the most specific wording to the least.
pub fn code_for(message: &str) -> Option<&'static str> {
    let m = message.to_ascii_lowercase();
    let rules: &[(&[&str], &str)] = &[
        (&["not exhaustive"], "4.2"),
        (&["remote"], "4.3"),
        (&["directive"], "4.4"),
        (&["reserved word"], "6.1"),
        (&["markup"], "3.13"),
        (&["@test", "test "], "3.14"),
        (&["macro"], "3.10"),
        (&["attribute", "derive"], "3.11"),
        (
            &[
                "`or` pattern",
                "pattern",
                "destructur",
                "let-else",
                "binding",
            ],
            "3.5",
        ),
        (&["variant", "enum"], "3.4"),
        (&["async", "await", "try", "future"], "3.3"),
        (&["import", "export", "require", "module"], "3.2"),
        (&["extension", "foreign", "primitive"], "3.9"),
        (
            &[
                "trait",
                "impl",
                "struct",
                "field",
                "`new ",
                "constructor",
                "sealed",
                "parameters in",
            ],
            "3.6",
        ),
        (&["interface", "type "], "3.7"),
        (&["?.", "?:", "??", "->", "=>", "safe", "non-nil"], "3.1"),
        (&["ternary", "spread", "where", "in operator"], "3.8"),
    ];

    rules
        .iter()
        .find(|(words, _)| words.iter().any(|w| m.contains(w)))
        .map(|(_, code)| *code)
}
