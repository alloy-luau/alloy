//! The documentation of every Alloy construct, in one table.
//!
//! The server answers a hover and a completion from it, and `alloy doc`
//! prints it. A key is the word as written, with its sigil: `struct`,
//! `??=`, `$dbg`, `@derive`, `derive:Eq`, and `topic:strict` for the
//! articles that no token names.

/// The Alloy keywords a plain completion offers. The child lists Luau's
/// own; these are the words the emit removes.
pub const ALLOY_KEYWORDS: &[&str] = &[
    "as",
    "async",
    "attribute",
    "await",
    "band",
    "bnot",
    "bor",
    "bxor",
    "case",
    "class",
    "const",
    "declare",
    "default",
    "delete",
    "enum",
    "export",
    "extends",
    "for",
    "from",
    "impl",
    "import",
    "in",
    "interface",
    "is",
    "macro",
    "match",
    "new",
    "on",
    "private",
    "public",
    "read",
    "remote",
    "satisfies",
    "shl",
    "shr",
    "struct",
    "trait",
    "try",
    "where",
    "with",
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
        "```alloy\nc ? a : b\n```\nThe else of a ternary. Touching a name it is a method call, `obj:method()`; without the call, `obj:method` is the method bound to `obj`, a function to pass along; after a name with a space it starts a type, `x: T`.",
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
        "[",
        "```alloy\nlocal xs = [ 1, 2, 3 ]\nlocal grid = [[1, 2], [3, 4]]\nlocal [head, ...tail] = xs\n```\nAn array literal: `Array<T>` with its metatable, so `xs:map(f)` works on it. Items separate with `,` or `;`, and `[[` opens a nested one; a long string is `[=[ ... ]=]`. In a `local` or a pattern the bracket destructures, and `...rest` takes the tail. `$set[ ]` and `$map[ ]` build the other collections.",
    ),
    (
        "<<",
        "```alloy\nlocal atom = charm.atom<<number>>(0)\nlocal m = import<<Module>>(path)\nnew Stack<<string>>()\n```\nExplicit type arguments on a call, a method call, `new`, or `import`. Luau reads none at a call, so the emit casts the result instead. A pack generic, `Signal.new<T...>`, takes none.",
    ),
    (
        "<T: Bound>",
        "```alloy\nfunction largest<T: Ord>(xs: T[]): T\nfunction show<T: Display & Debug>(v: T)\n```\nA bound on a generic: the argument must have the trait's methods. `&` asks for several. A std shape, `Display`, `Eq`, `Ord`, `Add`, resolves to the runtime's type; a trait the file declares resolves to its own. The bound is the type the parameter carries in the body.",
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
        "```alloy\nx is T\n```\nType test. `T` is a type name: a primitive, a Roblox datatype, an Instance class, `Enum.Name`, or an Alloy struct or enum. `x is not T` negates.\n\nEmits `type`, `typeof`, `IsA`, or a metatable check, chosen by the name.\n\nIn an `if`, a test on a plain name narrows it in the branch. Luau refines a primitive, a class, and a datatype on its own; the check artifact adds a cast for a struct, an enum, an imported type, and `RBXScriptSignal`, and gives `table` and `function` a shape that indexes and calls. `x is not T` narrows the else branch, or the code after a guard that returns, breaks, continues, or errors.",
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
        "```alloy\nstruct Name as\n    field: T\nend\n```\nA record with fields. `impl Name` adds methods. `Name { x = 1 }` is the raw constructor and `new Name(...)` calls `Name.new`. `@derive(Eq, Debug, Clone)` on the line before generates methods. A field or a method marked `private` belongs to the impl alone.\n\nEmits a table type plus a metatable with `__index` and a `__tostring`, so `print(v)` and `` `{v}` `` show `Name { x = 1, y = 2 }`. A `to_string` in the impl, an `impl Display`, replaces the default printer.",
    ),
    (
        "impl",
        "```alloy\nimpl Name as ... end\nimpl Trait for Name as ... end\n```\nMethods for a struct, an enum, or a foreign type such as `Vector3` or `string`. A foreign impl needs `export` and works project wide.\n\nEmits functions on the class table.",
    ),
    (
        "trait",
        "```alloy\ntrait Name as\n    function m(self): T\nend\n```\nA behavior contract: method signatures, with a body as a default. `impl Trait for Name` implements it and `<T: Trait>` bounds a generic; `<T: A & B>` asks for both.\n\nAn `impl` of an operator trait writes the metamethod: `Add` (`add`, `__add`), `Sub`, `Mul`, `Div`, `Eq` (`eq`, `__eq`), `Lt` and `Le` (`__lt`, `__le`), `Display` (`to_string`, `__tostring`), `Call` (`call`, `__call`), `Len`, `Concat`, and `Drop` (`drop`, which `delete` runs as `Destroy`). A bound names a shape the std exports, `Display`, `Debug`, `Clone`, `Eq`, `PartialEq`, `Ord`, `Add`, `Sub`, `Mul`, `Div`, `Serialize`; a file's own trait of the same name wins. `alloy doc Traits` lists them.\n\nEmits a type with the method signatures.",
    ),
    (
        "interface",
        "```alloy\ninterface Name extends Base as\n    field: T\nend\n```\nA structural object shape, sugar over `type`. A known base flattens into one table type; any other base joins with `&`. A field takes no `private` or `public`: an interface is a shape other code sees whole.",
    ),
    (
        "enum",
        "```alloy\nenum Name as\n    Unit\n    Payload(T)\nend\n```\nVariants with optional payloads. A unit enum is a string union at runtime; a payload enum is a tagged table. `match` takes it apart and `impl Name` adds methods.",
    ),
    (
        "match",
        "```alloy\nmatch expr with\n    case Pat then ...\n    default ...\nend\n```\nPattern match, as a statement or an expression; `match a, b with` matches several values at once, each arm with as many patterns.\n\nThe patterns: a variant with its payload, `Ok(v)`, `Move(x, y)`; a bare name or `_` for anything (a name binds); a literal, `1`, `\"a\"`, `true`, `nil`, `-1`; a dotted path, `Color.Red`; a struct, `Vec2 { x = 0, y }`, where `y` alone binds the field; an array, `[first, ...rest]`, with `...rest` for the tail. `or` joins alternatives, `and expr` is a guard, and `if local Pat = e` and `local Pat = e else ... end` use the same patterns outside a match. An exhaustive match needs no `default`.\n\nEmits an if-chain on the tag.",
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
        "```alloy\nimport { a, b as c, type T } from \"./m\"\nimport * as M from \"./m\"\nimport Name from \"./m\"\nimport M, { a, type T } from \"./m\"\nimport type { T } from \"./m\"\n```\nBrings names from another module into this file. `{ }` picks exports by name, `as` renames one, `* as M` takes the whole module, and a bare name takes its default export. `M, { a }` takes the module and names from it in one line: `local M = require(\"./m\") local a = M.a`. `type` marks a type-only import, which costs nothing at runtime.\n\n`import(\"./m\")` is the expression form: a `require`, typed from the module when the path is a string or an instance chain. `import<<T>>(expr)` gives a dynamic path the type `T`; without it the value is `unknown`.\n\nA path that ends in `.json` or `.toml` imports a data file as a table; `alloy doc data` explains.",
    ),
    (
        "export",
        "```alloy\nexport local x = 1\nexport function f() end\nexport struct Vec2 as ... end\nexport { a, b as c }\nexport type { T }\nexport default expr\n```\nAdds a name to the table the module returns at the end of its scope. Any declaration takes it: `local`, `const`, `function`, `async function`, `struct`, `enum`, `trait`, `interface`, `remote`, `attribute`, `macro`, `impl`. `export { }` names bindings after the fact, `as` renames one on the way out, and `export type { }` exports types alone. `export default` is what `import Name from` reads.",
    ),
    (
        "from",
        "The module path of an `import` or `export`, or the side that fires a `remote`: `from client`, `from server`, or `from client or server` for one both sides fire.",
    ),
    (
        "for",
        "```alloy\nfor _, { x, y } in points do\nfor i, [ a, b ] in pairs do\nfor _, player in Players:GetPlayers() where player.Team ~= nil do\n```\nLuau\'s loop, with two additions. A name in the head may be a table or an array pattern, `{ x, y }` or `[ a, b ]`, which destructures each item on the `do` line, and `where cond` after the iterator skips the items the condition rejects, with the names in scope. `for` reads a `Queue`, a `Heap`, and an `Iter` of the std as it reads a table.",
    ),
    (
        "local",
        "```alloy\nlocal x: T = expr\nlocal { name, hp = health } = player\nlocal [first, ...rest] = xs\nlocal Build(model) = job\nlocal Ok(v) = result else\n    return\nend\n```\nLuau's binding, with three forms of its own. A table pattern takes fields by name, `= alias` renames one. An array pattern takes items by position, and `...rest` takes the tail as an array. A variant or struct pattern binds its payload: `local Build(model) = job` reads the payload of one variant, and a value of another variant raises, naming the pattern and the tag it found. With `else ... end` the block runs instead, and it must leave, with `return`, `break`, `continue`, or an error, so the names hold after it. `const` takes every form too.",
    ),
    (
        "const",
        "```alloy\nconst x = expr\nconst LIMITS = { hp = 100 }\nLIMITS.hp = 1        -- allowed: the value is not frozen\n```\nA binding that cannot be reassigned. Luau has `const` of its own, so the keyword passes through and a reassignment is a compile error there too.\n\nThe freeze is shallow: it holds the name to one value, and that value stays mutable. A field of a `const` table takes an assignment, an index takes one, and `NAMES:push(x)` grows a `const` array. The pedantic lint `const_mutation` reports each of those, so a project that reads `const` as deep can turn it on.",
    ),
    (
        "async",
        "```alloy\nasync function f() ... end\nasync do ... end\n```\nReturns a Future. The body runs on `task.spawn` under `xpcall`, and the Future memoizes the result. `async function f(): T` is `Future<T>`; without a return type, a body that returns a value infers `T`, and one that returns nothing is `Future<()>`.",
    ),
    (
        "await",
        "```alloy\nawait expr\n```\nYields until the Future settles, then returns its value or rethrows its error. Accepts a Future or any value with `andThen`; the std spells the operand's type `Awaitable<T>`, which is `Future<T>`.\n\n`try await f()` turns a rejection into an Err. When `f` is an async function declared to return a `Result`, the Result it settles with is the value, not an Ok around it.",
    ),
    (
        "try",
        "```alloy\ntry expr\ntry do ... end\n```\nReturns early with the `Err`, inside a function that returns `Result`. At the module top level it returns the `Err` from the chunk, so the module yields that `Err`. `try do` is a block whose value is a Result.",
    ),
    (
        "macro",
        "```alloy\nmacro name(params) ... end\n```\nA compile-time template with expression parameters, called as `$name(...)`. Every `local` in the body is renamed per expansion.",
    ),
    (
        "new",
        "```alloy\nnew Name(...)\nnew Name(...) { Field = value }\n```\nConstructs a value. `new Name(...)` calls the constructor a struct's impl wrote, `new` or `New`, or the `new` of a Roblox datatype, an Instance, or any class; braces after it set fields on the new value, one per line. `new Name { ... }` is a struct's fields form, the only way to construct one that writes no constructor. A struct never constructs without `new`.",
    ),
    (
        "delete",
        "```alloy\ndelete expr\n```\nDestroys the value: an Instance, a connection, a thread, a function, or a table with a `Destroy`, `Disconnect`, `destroy`, or `disconnect` method; the Roblox spelling wins when a table has both. The std names that shape `Deletable`. `delete t.field` and `delete t[key]` then set the slot to nil, so the table holds nothing destroyed.",
    ),
    (
        "attribute",
        "```alloy\nattribute name(params) on target, ...\n```\nDeclares an attribute: metadata the compiler reads and `Attributes` reads at runtime. Targets: function, struct, enum, variant, field, param, remote, interface, type, local.\n\nThe built-in ones: `@derive` and `@sealed` on a struct or an enum; `@test` and `@cfg` on a function, `@cfg` on a local too; `@rename` and `@skip` on a field; `@unreliable`, `@ratelimit`, `@timeout`, and `@validate` on a remote; `@u8` to `@f32` on a parameter or a field; and Luau's own `@native`, `@checked`, `@deprecated`, `@inline`, `@noinline`, which pass through.",
    ),
    ("on", "The targets of an `attribute` declaration."),
    (
        "remote",
        "```alloy\nremote Name(params) from client\nremote function Name(params): R from server\n```\nA typed channel between server and client. The compiler reads a layout off the parameters, and the runtime packs each fire into one `buffer`: a `number` at the width its attribute names, `@u8` to `@f32`, or 64 bits without one; a `boolean` as a byte; a `string` with its length; an optional parameter with a presence byte. A struct opens to its fields, with the widths its fields declare, and the reader restores its metatable; a record type, `{ x: number, y: number }`, opens the same way; `T[]`, `{ T }`, and `Array<T>` pack a count and each item. What none of that covers, a `Player`, an Instance, a map, crosses beside the buffer as it is, and a remote whose parameters are all such stays unpacked. The handler sees the arguments as declared, defaults filled.\n\nThe object: `fire(...)` sends (to the server from a client; to one player, the first argument, from the server), `fire_all(...)` and `fire_except(player, ...)` send from the server to every client or all but one, `call(...)` asks a remote function and yields `Future<R>`, `on(handler)` and `once(handler)` receive (the connection comes back), `wait()` yields a Future of the next event, and `on_ratelimited(handler)` hears a sender that `@ratelimit` refused. `spec` and `instance` hold the declaration and the RemoteEvent behind it.\n\nThe declaration types the object. The side that fires passes the parameters, and a default makes one optional; a handler gets them filled, and a server handler gets the sender first: `BuySaber.on(function(sender, id) ... end)` types `sender: Player` and `id: string`. `call` on a remote function yields `Future<R>`.",
    ),
    (
        "where",
        "```alloy\nfor x in xs where cond do\nfor _, { x, y } in points where x > 0 do\nif local x = f() where cond then\nwhile local job = queue:pop() where job.ready do\n```\nA filter on a loop or a conditional binding. In a loop it emits `if not (cond) then continue end` on the `do` line, and the condition sees the loop\'s names, destructured ones included; on `if local` and `while local` the binding holds only when the condition does. Bindings chain with `;`, `if local p = a; local c = p.Character then`, each seen by the next, and `if not local x = f() then return end` is the guard form, with `x` in scope after it.",
    ),
    (
        "satisfies",
        "```alloy\nexpr satisfies T\n```\nChecks the literal against `T` under contextual typing and reports a key `T` does not name. The type is `T`.",
    ),
    (
        "private",
        "```alloy\nstruct Counter as\n    read name: string\n    private count: number = 0\nend\n\nimpl Counter as\n    function bump(self): number\n        self.count += 1\n        return self.count\n    end\n\n    private function reset(self)\n        self.count = 0\n    end\nend\n```\nA field or an `impl` method that only the struct's own methods reach. The word compiles to nothing at runtime: the check artifact keeps the private members out of the struct's public type, so `c.count` and `c:reset()` in other code are type errors in the editor and under `alloy flux`, and the `private_access` lint reports them in the same file. A private field with no default has to be set in `new Counter { }`, so that one stays; a private field that carries a default draws `private_access` when a `new` outside the impl names it. A struct with type parameters keeps one view. `public` is the default and needs no word.",
    ),
    (
        "public",
        "```alloy\nimpl Counter as\n    public function peek(self): number\n        return self.count\n    end\nend\n```\nThe default visibility, written out for symmetry with `private`. A public member is part of the struct's type in every file. Both words are reserved.",
    ),
    (
        "read",
        "```alloy\nread x: T\n```\nA read-only field. `read T[]` is `ReadArray<T>`, the array without its writers.",
    ),
    (
        "write",
        "```alloy\nwrite x: T\n```\nA write-only field. `write T[]` is `WriteArray<T>`, the array with `push` alone.",
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
        "$set",
        "```alloy\n$set[1, 2, 3]\n```\nA `Set` of the values: `Set.from({ 1, 2, 3 })`. The parenthesis form, `$set(1, 2, 3)`, is the same.",
    ),
    (
        "$map",
        "```alloy\n$map[[\"a\", 1], [\"b\", 2]]\n```\nA `HashMap` of the pairs: `HashMap.from({ [\"a\"] = 1, [\"b\"] = 2 })`. Each pair is a two-item array; `$map([\"a\", 1])` is the same.",
    ),
    (
        "$matches",
        "```alloy\n$matches(e, Ok(_))\n$matches(xs, [first, ...rest])\n```\nA pattern test without a `match`: true when the value fits the pattern. Any pattern a `case` takes; a name in it binds nothing here.",
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
        "derive:PartialEq",
        "```alloy\n@derive(PartialEq)\n```\nThe same `__eq` as `Eq`, under the name Rust uses. Luau has one equality, so the two derive the same method.",
    ),
    (
        "derive:Ord",
        "```alloy\n@derive(Ord)\n```\nGenerates `__lt` and `__le`: the fields compare in declaration order, and the first that differs decides, as a tuple compares. `<`, `<=`, `>`, `>=`, and `table.sort` then work on the values.",
    ),
    (
        "derive:Debug",
        "```alloy\n@derive(Debug)\n```\nGenerates `debug` and `__tostring`: the struct's name and its fields, as text. Every struct prints that way by default; the derive adds the `debug` method, and a `to_string` in the impl replaces both.",
    ),
    (
        "derive:Clone",
        "```alloy\n@derive(Clone)\n```\nGenerates `clone`: a shallow copy with the same metatable.",
    ),
    (
        "derive:Serialize",
        "```alloy\n@derive(Serialize)\n```\nGenerates `to_table`, `from_table`, and `serialize`, the plain-table forms for storage and remotes. `serialize` calls `to_table`, so the struct meets a `T: Serialize` bound.",
    ),
    // Attributes
    (
        "@derive",
        "```alloy\n@derive(Eq, Debug, Clone)\n```\nGenerates methods from the field list: `Eq` or `PartialEq` is `__eq`, `Ord` is `__lt` and `__le` over the fields in order, `Debug` is `debug` and `__tostring`, `Clone` is `clone`, `Serialize` is `to_table`, `from_table`, and `serialize`. On an enum, `Eq`, `PartialEq`, and `Clone` derive; `Debug` is every enum's own.",
    ),
    (
        "@cfg",
        "```alloy\n@cfg(server)\nfunction save(player: Player) end\n\n@cfg(client and not studio)\nconst hud = build_hud()\n```\nCode for one side. A function keeps its type and opens with the check: called where the condition fails, it raises. A local reads its value only where the condition holds, and stays typed as the value; elsewhere it is nil.\n\nThe conditions: `server`, `client`, `studio`, `edit`, `running`, and `test` (an `alloy test` run). Join them with `not`, `and`, `or`, or `any(...)` and `all(...)`. A shared module loads on both sides, so the runtime reads RunService when the code runs.\n\n**Applies to** `function` · `local`",
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
        "```alloy\n@test\nfunction name() ... end\n```\nA test beside the code it tests. The ship artifact blanks it, line for line; the check artifact keeps it typed; `alloy test` writes it into a lest spec with everything it reaches. An `async` test is awaited.",
    ),
    (
        "@unreliable",
        "```alloy\n@unreliable\nremote ...\n```\nAn UnreliableRemoteEvent behind the remote: a fire may drop or arrive out of order, which suits state the next fire replaces. Roblox drops a payload over 900 bytes.",
    ),
    (
        "@ratelimit",
        "```alloy\n@ratelimit(count, seconds)\n```\nA per-player token bucket on the server, for client-to-server remotes.",
    ),
    (
        "@timeout",
        "```alloy\n@timeout(seconds)\n```\nOn a `remote function`: a `call` that gets no answer in the window rejects, with the remote's name and the seconds in the error.",
    ),
    (
        "@validate",
        "```alloy\n@validate(fn)\n```\nA server-side predicate, `fn(sender, ...args)`, run before the handler: a false drops the event, or answers the call with nil, and the handler never sees it. Defaults fill in after it.",
    ),
    (
        "@u8",
        "```alloy\nremote Heal(@u8 amount: number) from client\n```\nThe wire width of a `number` on a remote parameter or a struct field: 8-bit unsigned. The remote packs it into the buffer at that width, a field when its struct crosses a remote, and a value that does not fit raises at the fire with the path of the value: `Sync: argument 1.hp: 300 does not fit @u8`. Without a width a number crosses as 64 bits.",
    ),
    (
        "@u16",
        "```alloy\nremote Heal(@u16 amount: number) from client\n```\nThe wire width of a `number` on a remote parameter or a struct field: 16-bit unsigned. The remote packs it into the buffer at that width, a field when its struct crosses a remote, and a value that does not fit raises at the fire with the path of the value: `Sync: argument 1.hp: 300 does not fit @u8`. Without a width a number crosses as 64 bits.",
    ),
    (
        "@u32",
        "```alloy\nremote Heal(@u32 amount: number) from client\n```\nThe wire width of a `number` on a remote parameter or a struct field: 32-bit unsigned. The remote packs it into the buffer at that width, a field when its struct crosses a remote, and a value that does not fit raises at the fire with the path of the value: `Sync: argument 1.hp: 300 does not fit @u8`. Without a width a number crosses as 64 bits.",
    ),
    (
        "@i8",
        "```alloy\nremote Heal(@i8 amount: number) from client\n```\nThe wire width of a `number` on a remote parameter or a struct field: 8-bit signed. The remote packs it into the buffer at that width, a field when its struct crosses a remote, and a value that does not fit raises at the fire with the path of the value: `Sync: argument 1.hp: 300 does not fit @u8`. Without a width a number crosses as 64 bits.",
    ),
    (
        "@i16",
        "```alloy\nremote Heal(@i16 amount: number) from client\n```\nThe wire width of a `number` on a remote parameter or a struct field: 16-bit signed. The remote packs it into the buffer at that width, a field when its struct crosses a remote, and a value that does not fit raises at the fire with the path of the value: `Sync: argument 1.hp: 300 does not fit @u8`. Without a width a number crosses as 64 bits.",
    ),
    (
        "@i32",
        "```alloy\nremote Heal(@i32 amount: number) from client\n```\nThe wire width of a `number` on a remote parameter or a struct field: 32-bit signed. The remote packs it into the buffer at that width, a field when its struct crosses a remote, and a value that does not fit raises at the fire with the path of the value: `Sync: argument 1.hp: 300 does not fit @u8`. Without a width a number crosses as 64 bits.",
    ),
    (
        "@f32",
        "```alloy\nremote Heal(@f32 amount: number) from client\n```\nThe wire width of a `number` on a remote parameter or a struct field: 32-bit float. The remote packs it into the buffer at that width, a field when its struct crosses a remote, and a value that does not fit raises at the fire with the path of the value: `Sync: argument 1.hp: 300 does not fit @u8`. Without a width a number crosses as 64 bits.",
    ),
    (
        "class",
        "```alloy\nclass Name\n    public hp: number\n    function heal(self) end\nend\n```\nThe classes RFC, parsed ahead of Luau: `class`, `open class`, `extends`, `public` fields, and the metamethods the RFC allows. It does not compile yet: the compiler reports it and blanks the block. A `struct` with an `impl` is the form that runs today.",
    ),
    (
        "declare",
        "```alloy\ndeclare function name(params): R\ndeclare name: T\ndeclare extern type Name with ... end\ndeclare class Name extends Base ... end\n```\nA definition-file statement. Luau's own definition syntax, in a `.d.aly`, which compiles to `.d.luau` and joins the analyzer's definitions; no runtime. A class or an extern type lists members: `name: T`, `read name: T`, `write name: T`, a method `function name(self, ...): R`, and an indexer `[K]: V`; `extends` names the base.",
    ),
    // The std: ambient names, no import. The child sees `__alloy.Name`.
    (
        "HashMap",
        "```alloy\nlocal prices = $map[[\"sword\", 10], [\"pet\", 25]]\nlocal m: HashMap<string, number> = HashMap.new()\nm:set(\"gem\", 5)\nfor key, value in m:entries() do end\n```\nA map with methods, from the std. Any value that is not nil is a key. `HashMap.new()` is empty, `HashMap.from(t)` copies a table's pairs, and `$map[[k, v], ...]` is the literal.",
    ),
    (
        "Set",
        "```alloy\nlocal seen = $set[1, 2, 3]\nlocal s: Set<string> = Set.new()\nif s:add(\"a\"):has(\"a\") then end\n```\nA set with methods, from the std. Any value that is not nil is a member. `Set.new()` is empty, `Set.from(t)` takes an array's items, and `$set[a, b]` is the literal.",
    ),
    (
        "Array",
        "```alloy\nlocal xs = [ 1, 2, 3 ]\nlocal doubled = xs:map(function(x) return x * 2 end)\nlocal grid = [[1, 2], [3, 4]]\n```\nThe array type of the std: a Luau array with the std's methods on its metatable. An array literal carries it, `T[]` and `Array<T>` name the same type, and a plain table becomes one with `Array.from(t)`, in place. `[[` opens a nested array; a long string keeps Luau's leveled form, `[=[ ... ]=]`. A `read T[]` is `ReadArray<T>`, the readers alone, and a `write T[]` is `WriteArray<T>`, `push` alone.\n\nLuau rejects an alias that names itself with other arguments, so `map` returns the same shape under a second name, and the third `map` in one chain is `any`. Annotate the accumulator of `reduce` when its body uses it: `xs:reduce(function(acc: number, x) return acc + x end, 0)`. `$set[ ]` and `$map[ ]` build a `Set` and a `HashMap` the way `[ ]` builds an array.",
    ),
    (
        "ReadArray",
        "```alloy\nlocal function total(xs: read number[]): number\n    return xs:reduce(function(acc, x) return acc + x end, 0)\nend\n```\nThe array a `read T[]` names: every reader of `Array<T>` and none of its writers. An `Array<T>` passes where one is asked, so a function that only reads takes this type and says so.",
    ),
    (
        "WriteArray",
        "```alloy\nlocal function log_to(sink: write string[])\n    sink:push(\"line\")\nend\n```\nThe array a `write T[]` names: `push` and the index, nothing that reads. An `Array<T>` passes where one is asked.",
    ),
    (
        "Future",
        "```alloy\nasync function load(id: number): Profile\n    return await fetch(id)\nend\nlocal profiles = await Future.all([load(1), load(2)])\nlocal first = await Future.race([load(1), Future.delay(5)])\n```\nA memoized task, from the std: it runs once, settles once, and every `await` after that reads the same value. An `async function` returns one and its body runs on `task.spawn` under `xpcall`; `await` yields until it settles, then returns the value or rethrows; `try await` returns the `Err` from the enclosing function instead of rethrowing.\n\n`race` and `any` read the value type off each Future in the list, so a mixed list gives the union: `Future<number>` beside `Future<nil>` yields `Future<number?>`. An `async function` without a return type and without a `return` value is `Future<()>`.",
    ),
    (
        "Result",
        "```alloy\nlocal r: Result<number, string> = Ok(1)\nlocal v = r:unwrap_or(0)\nmatch r with\n    case Ok(n) then print(n)\n    case Err(e) then warn(e)\nend\n```\n`Ok(value)` or `Err(error)`, from the std. A function that can fail returns one, and the caller reads it with a `match`, an `if local Ok(v) = r`, or the methods. `try expr` inside a function that returns a Result returns the `Err` early, and at the module top level it returns the `Err` from the chunk; `try do ... end` turns a throw into an Err; `try await` does the same for a Future.\n\nAn `Err` carries `trace`, a traceback `Result.pcall` and `try` fill in and `Err(e, trace)` sets by hand. `map` and `map_err` yield a Result of the same surface whose own `map` is `any`: Luau rejects an alias that names itself with other arguments. `Result<T, E>` is covariant, so a `Result<Boost, E>` passes where `Result<any, E>` is asked.",
    ),
    (
        "Ok",
        "```alloy\nOk(value)\n```\nThe success case of a `Result`.",
    ),
    (
        "Err",
        "```alloy\nErr(error)\nErr(error, trace)\n```\nThe failure case of a `Result`. The second argument is a traceback, which `unwrap` and `expect` print under the error; `try` and `Result.pcall` fill it in.",
    ),
    (
        "Queue",
        "```alloy\nlocal jobs: Queue<Job> = Queue.new()\njobs:push(job)\nwhile local next = jobs:pop() do\n    run(next)\nend\n```\nA first-in, first-out queue over a ring of indices, from the std: `push` at the back costs the same at any size, and so does `pop` at the front. `Queue.from(t)` fills one from an array. A `for` loop reads it front to back without a pop.",
    ),
    (
        "Heap",
        "```alloy\nlocal open = Heap.new(function(a, b) return a.cost < b.cost end)\nopen:push(node)\nlocal nearest = open:pop()\n```\nA binary heap, from the std: `pop` yields the least value under `less`, which defaults to `<`, so numbers and strings need no comparator and tables take one. `Heap.new(less?)` and `Heap.from(t, less?)` both give a `Heap<T>`, and `Heap.from` builds one from an array. A `for` loop reads it least first without a pop; `to_array` returns the items sorted.",
    ),
    (
        "Scope",
        "```alloy\nlocal scope = Scope.new()\nscope:add(part.Touched:Connect(on_touch))\nscope:add(function() print(\"bye\") end)\ndelete scope\n```\nA cleanup bag, from the std. `add` takes anything `delete` accepts, an Instance, a connection, a thread, a table with `Destroy` or `Disconnect`, or a function, and gives it back, so `local conn = scope:add(signal:Connect(f))` reads as before. `clean` runs the cleanups newest first and empties the bag; `delete scope` does the same through `Destroy`. A scope may hold another scope.",
    ),
    (
        "Iter",
        "```alloy\nlocal names = Iter.from(players)\n    :filter(function(p) return p.Team == team end)\n    :map(function(p) return p.Name end)\n    :take(5)\n    :collect()\nfor i in Iter.range(1, 10, 2) do end\n```\nA lazy iterator, from the std: each step wraps the last, and nothing runs until `collect`, `for_each`, a reducer, or a `for` loop pulls. `Iter.from` takes an array, a function that returns the next value or nil, or a `Set`, `Queue`, `Heap`, or `HashMap`; it keeps the element type, so `Iter.from(number[])` is `Iter<number>` and `:collect()` is `number[]`. Another table with an `__iter` metamethod needs a cast. `Iter.range(from, to, step?)` counts, inclusive.\n\nAs with `Array`, `map` returns the same shape under a second name, and the third `map` in one chain is `any`.",
    ),
    (
        "Symbol",
        "```alloy\nlocal key = Symbol.new(\"name\")\nlocal t = { [key] = 1 }\n```\nA unique key that no string can collide with, from the std: a frozen table that prints as `Symbol(name)`. Use one for a private table slot, or a sentinel a value cannot forge.",
    ),
    (
        "Signal",
        "```alloy\nlocal damaged = Signal.new<<Player, number>>()\nlocal conn = damaged:Connect(function(player, amount) end)\ndamaged:Fire(player, 10)\nlocal who, amount = damaged:Wait(5)\n```\nA typed signal, from the std, with the shape of `RBXScriptSignal`, so code that takes one takes the other. `Signal.new<T...>()` fires `T...`; handlers run in connection order, each on a reused thread, and a handler may disconnect any connection during a fire.\n\nEach has a snake_case twin, `connect`, `once`, `wait`, `fire`, `fire_deferred`, `disconnect_all`, `destroy`. A connection has `Connected`, `Disconnect`, and `disconnect`. `Signal.is(value)` says whether a value is one.\n\n`Signal.collect(source)` turns any signal with `Connect` or `connect`, the shape `Signalish<T...>`, a Roblox one included, into an iterator that drains the queued events in order, plus the connection: `for id, value in Signal.collect(changed) do`. `Signal.wrap(source)` gives a Signal that fires with the source.",
    ),
    (
        "Traits",
        "```alloy\nfunction largest<T: Ord>(xs: T[]): T\nimpl Display for Vec2 as\n    function to_string(self): string\n        return `({self.x}, {self.y})`\n    end\nend\n```\nThe trait shapes the std exports, for a bound and for `impl X for Struct`. Each is a table type of the methods it asks for; a struct that has them passes.\n\nAn `impl` may also name `Lt`, `Le`, `Call`, `Len`, `Concat`, and `Drop`, which write `__lt`, `__le`, `__call`, `__len`, `__concat`, and `Destroy`. A trait the file declares with one of these names wins over the std's.",
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
        "```alloy\nattribute icon(asset: string) on struct, variant\n\n@icon(\"rbxassetid://1\")\nstruct Sword as ... end\n\nlocal asset = Attributes.get(Sword, icon)\n```\nDeclared metadata, readable at runtime. An `attribute` declaration binds its name to an `Attribute<T>` value, so the `attr` of each static is the name as written, not a string.\n\nAn attribute with one parameter reads as that value; with several, as a table of them in order.",
    ),
    (
        "@sealed",
        "```alloy\n@sealed\nstruct Config as\n    volume: number\nend\nlocal c = new Config { volume = 1 }\nc.volume = 2  -- fine: declared\nc.volme = 2   -- error: Config has no field volme\n```\nA struct is open: a write to a name it does not declare makes a new key, and a typo goes unnoticed. `@sealed` makes that write an error at runtime, with the struct's name and the key in the message, and the check artifact rejects it from the table type. A declared field set to nil writes through. To stop writes to a declared field, mark it `read`.\n\nEmits `__newindex` on the class table.",
    ),
    (
        "topic:strict",
        "**Strict by default**\n\nEvery `.aly` file checks in Luau strict mode unless the project says otherwise. `alloy init` writes one Luau configuration with `languageMode = \"strict\"`, and the language server gives a workspace with neither file the same setting. A `--!nonstrict` or `--!nocheck` line at the top of a file still wins for that file.\n\nOn top of the checker, the compiler holds these at compile time:\n\n  match           every variant has an arm, or a `default` (`alloy doc exhaustive`)\n  new Name { }    every field without a default is set; no unknown field\n  impl T for S    every method of the trait, with the trait's arity\n  @sealed         no write to an undeclared field, at runtime and at check time\n  remote          no function, thread, Future, or Signal in a parameter (`alloy doc wire`)\n\n`alloy lint` adds the nil discipline and, under `[lint] strict = true`, which is on unless the project turns it off, the no-implicit-any rules (`alloy doc lints`).",
    ),
    (
        "topic:exhaustive",
        "**Exhaustive match**\n\nA `match` with no `default` must cover every value. For an enum, that is every variant, each with payloads that cover; `Color.Red`, a bare unit variant, `Some(x)`, `_`, and `a or b` all count. When a variant has no arm, the compiler names it:\n\n```\nthis match is not exhaustive: `Msg` has no arm for `Leave`; add it or a `default` arm\n```\n\nA literal, a struct, or an array pattern set needs a `default`. A guarded arm proves nothing about coverage.\n\nThe other direction is a lint: a `default` under arms that already cover every variant never runs, and `unreachable_default` says so, because that default would hide the next variant added.",
    ),
    (
        "topic:wire",
        "**Wire types**\n\nA `remote` carries data across the network. A parameter typed as a function, a `thread`, a `Future`, or a `Signal` cannot be serialized, so the declaration is an error:\n\n```\nremote Ping(cb: () -> ()) from client\n-- remote `Ping`: parameter `cb` has type `() -> ()`, which is a function type; a remote carries only data\n```\n\nSend an id, a name, or a plain table, and keep the callback on the side that owns it.\n\nWhat crosses is packed: a remote with a `number`, `boolean`, or `string` parameter sends one `buffer` and unpacks it on the other side, so a fire costs its bytes and no names. A width attribute sets a number's size, `@u8`, `@u16`, `@u32`, `@i8`, `@i16`, `@i32`, `@f32`; a number without one takes 64 bits. A value that does not fit its width raises at the fire: `Heal: 300 does not fit @u8`. A struct, a record type, or an array of packable values packs field by field and item by item, down to any depth, and a struct comes back with its metatable. An Instance, a `Player`, or a map is not packed and travels as it is beside the buffer.",
    ),
    (
        "topic:lint",
        "**alloy lint**\n\nRuns the lints over the project, or over one file, and nothing else; `alloy flux` runs them with the compile and the type check. A lint is advice: the code runs, and the lint names a habit that costs bugs. The lints are Flux's, in eight groups:\n\n  correctness   code that is wrong, or cannot run\n  suspicious    code that is probably not what the author meant\n  style         a Luau habit with an Alloy form: `a and a.b` for `a?.b`, `typeof(x) == \"T\"` for `x is T`\n  complexity    a simple thing done in a hard way; the limits sit in `[flux]`\n  perf          code that runs slower than the plain form\n  roblox        a Roblox API that is deprecated or misused\n  pedantic      strict rules, on while `[lint] strict = true`\n  naming        the case of names, off until `[lint.rules] naming = \"warn\"`\n\nA lint whose rewrite keeps the program the same carries it, and `--fix` applies those; in the editor each one is a quick fix, and `source.fixAll` applies every rewrite of the file.\n\n```\nalloy lint                 the project of the nearest alloy.toml\nalloy lint src/game.aly    one file\nalloy lint --fix           apply the rewrites that keep the program the same\nalloy lint -W pedantic     a level for this run: -W warns, -A allows, -D denies\nalloy lint --deny-warnings fail on any hit\nalloy lint --list          every lint with its group and level, in the `[lint.rules]` form\n```\n\nThe `[lint]` table of alloy.toml says where every lint starts, and `[lint.rules]` sets one. Both modes are on by default, and `alloy init` writes them:\n\n```toml\n[lint]\nrecommended = true    # the level each lint declares; false starts every lint at allow\nstrict = true         # the pedantic group, at warn\n\n[lint.rules]\ncorrectness = \"deny\"\nconcat_interpolation = \"allow\"\nluau = \"allow\"\nnaming = \"warn\"\nalx.static_conditional_child = \"deny\"\n```\n\nA key is a lint name, a group name, an ingot's `<ingot>/<lint>`, or `alx.<name>` for a markup lint; a name beats its group. `recommended = false` leaves every lint at `allow`, so the rules table alone says what runs; `strict` and the table still work on top of it. `luau` is the group of the type checker's own lints, `LocalUnused` and the rest, which `alloy flux` reports.\n\nThe old form, `deny`, `warn`, and `allow` lists in `[lint]` and a `[alx.lints]` table, still reads for one release and warns with the key that replaces it.\n\n`alloy lint --list` names every lint in the `[lint.rules]` form. `alloy doc lints` lists them; `alloy doc <name>` explains one, and `alloy doc <group>` lists a group. The language server shows the same lints as warnings, with the rewrite in the message.",
    ),
    (
        "topic:flux",
        "**alloy flux**\n\nFlux is the whole analysis in one run, what clippy is to cargo. It compiles every source, runs luau-lsp over the check artifact and maps the type errors onto the Alloy lines, and runs every lint at its `[lint]` level: Flux's own eight groups, and the checker's lints under the `luau` group. It also sees what one file cannot: `circular_import` reports two files that import each other.\n\n```\nalloy flux                 the project of the nearest alloy.toml\nalloy flux src/game.aly    one file: the compile, the lints, and the type check\nalloy flux --fix           apply the rewrites that keep the program the same\nalloy flux -D correctness  deny a group for this run; -W warns, -A allows\nalloy flux --explain manual_floor_div\nalloy flux --no-typecheck  skip luau-lsp\nalloy flux --watch         run again after every change\nalloy flux --list          every lint with its group and default level\n```\n\nOne file named on the command line still compiles the whole project, since the type check needs every module the file imports, and the report then covers that file alone. A file outside `[build] in` gets the compile and the lints, and the run says the type check did not run.\n\nThe check artifact keeps the source's lines, so a type error on line 12 of the output is on line 12 of the source; the column maps through the span map. The artifacts go into a mirror of the project under the temp directory, with the root's Luau configuration and a link to every other folder, so requires resolve as they do in the editor.\n\nThe `[flux]` table:\n\n```toml\n[flux]\ntypecheck = true                  # run luau-lsp over the check artifact\ndefinitions = []                  # extra .d.luau or .d.aly files; the project's .d.aly join on their own\nroblox_types = true               # load the Roblox globals\nsecurity_level = \"PluginSecurity\" # LocalUserSecurity, RobloxScriptSecurity, None\n# luau_lsp = \"/path/to/luau-lsp\"  # unset: the PATH, then ~/.alloy/bin and ~/.ember/bin\ntoo_many_arguments = 7\ntoo_many_lines = 100\nmax_nesting = 5\ncognitive_complexity = 25\n```\n\nThe Roblox globals come from the luau-lsp extension's storage when the editor has them, and download once into `~/.alloy/types` otherwise. A `--@alloy-ignore` line silences the checker's report on that line, as it does in the editor.",
    ),
    (
        "topic:test",
        "**alloy test**\n\nA test lives beside the code it tests: a `@test` function in the module, blanked from the ship artifact. `alloy test` builds the project, then writes one lest spec per source that holds a `@test`, under `[test] out`:\n\n```\nsrc/inventory.aly        ->  tests/inventory.spec.luau\nsrc/ui/menu.aly          ->  tests/ui/menu.spec.luau\n```\n\nA spec carries the tests and every top-level statement they reach: the imports they use, the locals and functions they call, the structs and the impls those need. The rest of the module stays out, so its side effects stay out of the test VM. The slice keeps the source's lines, so a failure points at the real one. Each relative `require` in the spec points at the build output, the runtime included, and the spec ends with a `describe` that registers each test by name; an `async` test is awaited.\n\n```alloy\nlocal function clamp01(x: number): number\n    return math.clamp(x, 0, 1)\nend\n\n@test\nfunction clamp_keeps_range()\n    $assert_eq(clamp01(2), 1)\nend\n```\n\n```\nalloy test                 build, then write the specs\nalloy test --run           and run lest on the suite\nalloy test --coverage      run lest with line coverage\nalloy test --filter hit    run the tests whose name holds `hit`\nalloy test -- --reporter json   the rest goes to lest as given\nalloy test --watch         write again after every change\nalloy test --check         write nothing; fail when a spec would change\nalloy test src/game.aly    one file's spec, to stdout\n```\n\nThe `[test]` table:\n\n```toml\n[test]\nout = \"tests\"     # where the specs go\nsuite = \"alloy\"   # the suite name in lest.toml\nlest = true       # write lest.toml and the @lest alias when the root has none\n```\n\nWith `lest = true`, the first run writes a `lest.toml` with one suite over `tests/**/*.spec.luau` on the native backend, and adds `lest = \".lest/core\"` to the aliases of `.luaurc`, which is where lest puts its framework. `lest` then runs the suite; on its VM the runtime falls back to plain coroutines, so an `await` settles at once. `$assert` and `$assert_eq` raise, and lest reports the line.",
    ),
    (
        "topic:fmt",
        "**alloy fmt**\n\nAnneal formats `.aly` and `.alx` files in place. The layout comes from the tokens and the `[fmt]` options, not from how the author laid the code out: the spacing between tokens, which bracket groups break, the quotes of a string, the parentheses of a call. Statements keep their lines, at most one blank line stays between them, and a long string or a long comment keeps its text. The program is the same afterwards; the token stream changes only where an option asks for a rewrite. A file the parser cannot read is skipped and keeps its text, and the run names it: the layout would move a statement into the block the recovery chose, not the one the author wrote.\n\n```\nalloy fmt                  the sources of the project\nalloy fmt src/ui.aly dir/  the paths given\nalloy fmt --check          write nothing; fail when a file would change\n```\n\nA bracket group, the arguments of a call, a table, or an array, stays on one line when it fits in `column_width`, and breaks one element per line when it does not, or when a trailing comma in the source asks for it. An `import { }` list breaks on a trailing comma even when written on one line, and on every list with more than one name under `expand_imports`. A callback argument indents its body once: `foo(function()` opens one level, not two.\n\n`[fmt] recommended = true`, which `alloy init` writes, applies the layout below. `recommended = false` makes the formatter preserving instead: no line is reflowed, quotes and numbers keep the form the author wrote, the parentheses of a call stay as they are, a blank line at a block's edge stays, and the indent of each file comes from that file. The `[fmt]` keys the project sets then apply over that, so `recommended = false` with `quote_style = \"force-double\"` rewrites the quotes and nothing else.\n\nThe options, with their defaults:\n\n```toml\n[fmt]\nrecommended = true                    # false preserves what the file does\ncolumn_width = 100\nline_endings = \"input\"                # unix, windows; input keeps the file's own\nindent_type = \"spaces\"                # tabs\nindent_width = 4\nquote_style = \"auto-prefer-double\"    # auto-prefer-single, force-double, force-single, preserve\nleading_zero = \"add\"                  # `.5` becomes `0.5`; strip, preserve\ncall_parentheses = \"always\"           # no-single-string, no-single-table, none, input\nspace_after_function_names = \"never\"  # definitions, calls, always\ncollapse_simple_statement = \"never\"   # function-only, conditional-only, always\nblock_newline_gaps = \"never\"          # preserve keeps a blank line at a block's edge\nmagic_trailing_comma = true           # a trailing comma keeps a group expanded\ntrailing_comma = true                 # an expanded group ends its last element with a comma\nspace_inside_braces = true            # { a }\nspace_inside_parens = false           # f(a)\nspace_inside_brackets = false         # t[k]\nspace_inside_array = true             # [ 1, 2 ]; Alloy's own\nalign_struct_fields = false           # the `:` of a struct's fields line up; Alloy's own\nexpand_imports = false                # an import list with more than one name breaks one per line; Alloy's own\nexclude = []                          # paths to leave alone; `*` matches any run\n\n[fmt.call_chains]\nstyle = \"preserve\"                    # method: break before each call past the first; full: before every call\nmin_calls = 3\n\n[fmt.sort_requires]\nenabled = false                       # sort the `import` lines at the top of the file\ngrouping = \"flat\"                     # by-kind: aliases, then absolute, then relative paths\n\n[fmt.alx]\nattribute_quotes = \"double\"           # single, preserve\nbracket_same_line = false             # the `>` of a broken tag on the last attribute's line\nattribute_per_line = true             # a broken tag puts every attribute on its own line; false packs them\nself_closing_space = true             # <Frame />\ntext_wrap = \"fill\"                    # preserve keeps the author's line breaks in text\nblank_lines = true                    # a blank line between children stays\n```\n\nThe names follow larvae and stylua where the option is theirs, so a config ports over. In an `.alx` file the code formats the same way and the markup prints from its tree: a tag that fits stays on one line, a tag that does not breaks its attributes and then its children, and text flows with the holes in it.",
    ),
    (
        "topic:check",
        "**alloy check**\n\nCompiles every source of the project and writes nothing. The report carries the compiler's diagnostics and the lints at their `[lint]` levels, and the exit code is one on any diagnostic or denied lint. `alloy check <file>` does the same for one file. `alloy flux` is the same run with the type check on top.",
    ),
    (
        "topic:build",
        "**alloy build**\n\nCompiles every `.aly` and `.alx` under `[build] in` to Luau under `[build] out`, mirroring the tree, and writes the runtime as `alloy.luau` at the output root. A plain `.luau` or `.lua` beside the sources is copied as it is, so a `require(\"./other\")` finds it in the output. A file that did not change is not rewritten, so a Rojo sync stays quiet. `artifact = \"ship\"` writes the code Roblox runs; `\"check\"` writes what luau-lsp sees, with the types kept. `clean = true` removes an output whose source is gone.\n\n```\nalloy build                one project\nalloy build src/game.aly   one file, to stdout\nalloy build --check --map  the check artifact and its chunk map\n```",
    ),
    (
        "topic:config",
        "**alloy.toml**\n\nThe file `alloy init` writes:\n\n```toml\n#:schema .alloy/alloy.schema.json\n[build]\nin = \"src\"\nout = \"build\"\nexclude = []\nclean = false\nartifact = \"ship\"\n\n[emit]\n# wait_timeout = 5\n# std_require = \"@alloy\"\n# erase_type_imports = false\n\n[fmt]\nrecommended = true\ncolumn_width = 100\nindent_type = \"spaces\"\nindent_width = 4\nquote_style = \"auto-prefer-double\"\n\n[lint]\nrecommended = true\nstrict = true\n\n[lint.rules]\n# raw_require = \"allow\"\n\n[flux]\ntypecheck = true\ndefinitions = []\n\n[test]\nout = \"tests\"\nsuite = \"alloy\"\nlest = true\nshim = true\n\n[project]\nname = \"game\"\nsourcemap = true\nsource_of_truth = true\nmount_aliases = true\n\n# [mount]\n# alias = [path, mount]: the folder at path lands at mount in the DataModel\n# shared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n# server = [\"src/server\", \"@game/ServerScriptService/Server\"]\n\n# [ingots]\n# an extension that ships as an executable: a path relative to this file,\n# or a GitHub release pinned by version\n# tailwind = \"ingots/tailwind\"\n# tailwind = { repo = \"alloy-luau/tailwind-ingot\", version = \"0.1.0\" }\n```\n\nEvery key has a default, and an unknown key is an error. Both `recommended` keys are on: `[lint] recommended` applies the level each lint declares, and `[fmt] recommended` applies the layout above; either off leaves that table's own keys as the whole setting (`alloy doc lint`, `alloy doc fmt`). `[lint.rules]` gives one lint, one group, or one markup lint under `alx.` a level of `allow`, `warn`, or `deny`. `alloy doc lint`, `alloy doc flux`, `alloy doc fmt`, and `alloy doc test` explain their tables, and `alloy doc markup` the `[alx]` table, which the written file leaves out. `alloy init` writes the file, plus one Luau configuration: a `.config.luau` with strict mode and the `@alloy` alias when the folder has neither file, else the mode and the alias added to the `.config.luau` or `.luaurc` it already has (`alloy doc init`). `[project]` says which project file describes the DataModel tree, and `[mount]` describes one for a tool that reads no project file. Two keys of `[project]`, both on by default, say how far the table reaches: `source_of_truth` writes `default.project.json` and `.alloy/build.project.json` from it, and `mount_aliases` serves its names as aliases beside the ones the Luau configuration declares. `alloy doc mount` explains all of it. `[alx]` holds the markup settings, the shape `luaux.toml` has, so a project with `.alx` files needs no second file.\n\nThe editor checks the file against a JSON Schema. `alloy self schema` prints it. `alloy self code` writes it to `~/.alloy/alloy.schema.json` and points VS Code (Even Better TOML) and Zed (Tombi, taplo) at it, so every key completes with its type, default, and text, and an unknown key is marked. `alloy build` writes the project's own schema to `.alloy/alloy.schema.json`, with the options and the lint names of its ingots; the `#:schema .alloy/alloy.schema.json` line at the top of the file, which `alloy init` writes, makes the editor read that one.",
    ),
    (
        "topic:init",
        "**alloy init**\n\nWrites `alloy.toml` in the working folder, and one Luau configuration.\n\n```\nneither file       writes .config.luau: strict mode and alloy = \"./build/alloy\"\n.config.luau       adds the mode and the alias to it, only where they are absent\n.luaurc            adds them to it, and writes no .config.luau\n```\n\nThe edit keeps every other key and every comment, so a file you wrote stays as you wrote it. With both files present, `.config.luau` is the one edited, as Luau reads it first. `alloy init` fails when `alloy.toml` is already there.\n\nThe `@alloy` alias names the runtime the build writes to the output root. Emitted code requires it by that alias only when `[emit] std_require` says so; the default is a relative path, or the runtime's instance path when the DataModel tree holds the file (`alloy doc mount`).\n\nA new project needs no project file to build. Add a `default.project.json`, or a `[mount]` table, when the sources have to land somewhere in the DataModel.",
    ),
    (
        "topic:luaurc",
        "**.luaurc and .config.luau**\n\nLuau reads its settings from `.luaurc`, a JSON file, or from `.config.luau`, a Luau chunk that returns `{ luau = { ... } }`. With both, `.config.luau` wins. Alloy reads and writes both the same way: `languageMode` and `aliases` are the keys it uses.\n\n```json\n{ \"languageMode\": \"strict\", \"aliases\": { \"alloy\": \"./build/alloy\" } }\n```\n\n```luau\nreturn { luau = { languagemode = \"strict\", aliases = { alloy = \"./build/alloy\" } } }\n```\n\nThe language server copies the file into its mirror, and adds `languagemode = \"strict\"` when the file sets no mode, so the default stays strict.\n\nThe aliases of the file are the project's aliases. `@shared/economy` in a source names the folder the alias points at; the DataModel tree says where that folder lands, and the ship artifact writes the instance path (`alloy doc mount`). `alloy init` adds `alloy = \"./build/alloy\"` and strict mode when the file lacks them, and leaves every other key alone; nothing else writes to these files.",
    ),
    (
        "topic:directives",
        "**Directives**\n\nA comment that starts with `--@alloy-` steers the diagnostics of one file: the compiler's, the lints, and the checker's type errors, which the language server drops on the silenced lines before the editor sees them. The editor lists the directives after `--`, `--@`, or `--!`, and on an empty line. `alloy doc alloy-ignore` explains one.\n\n```alloy\n--@alloy-nocheck                          this file: nothing is reported\n--@alloy-ignore the solver misreads this  the next line with code is silent\nlocal x = y.z --@alloy-ignore             at the end of a line: that line\n--@alloy-expect-error a negative count    the next line must hold an error\n--@alloy-ignore-start raw_require         every line under here, up to the end\n--@alloy-ignore-end                       closes it\n--@alloy-lint raw_require=allow           this lint's level, for this file\n--@alloy-side client                      the side of every remote this file sees\n--@alloy-preserve                         `alloy flux --fix` leaves the next line\n```\n\nText after `--@alloy-ignore` and `--@alloy-expect-error` is the reason. An expectation with no reason draws the `missing_reason` lint, and the reason comes back in the error the directive draws when its line goes clean, so a stale one is easy to place.\n\nUse `ignore` for a line the new solver gets wrong. Use `expect-error` where the error is the point, a test of a lint or a contract: it silences the line, and reports when the line comes clean, so a fix that makes the directive stale shows. Luau's own `--!strict`, `--!nonstrict`, and `--!nocheck` pass through and set the checker's mode for the file.\n\n`--@alloy-lint`, `--@alloy-side`, `--@alloy-ignore-start`, and `--@alloy-ignore-end` sit on a line of their own, anywhere in the file. `--@alloy-ignore`, `--@alloy-expect-error`, and `--@alloy-preserve` sit on their own line or at the end of a line with code.\n\nA directive the compiler cannot accept is a `DirectiveError` on its own line: a name no directive has, a lint or a level `--@alloy-lint` does not know, an `--@alloy-ignore-start` with no end, an `--@alloy-ignore-end` that closes nothing, and an `--@alloy-side` that contradicts the file name.\n\nEvery diagnostic carries the book section it belongs to as its code, `Alloy(4.2)`; the number links to the section in the editor, and `alloy doc 4.2` prints it.",
    ),
    (
        "topic:alloy-ignore",
        "**--@alloy-ignore**\n\nSilences every diagnostic on one line: the compiler's, the lints, and the checker's. On a line of its own it covers the next line that holds code; at the end of a line with code it covers that line. Text after the name is the reason, and nothing reads it back, so write it for the next reader.\n\n```alloy\n--@alloy-ignore the new solver widens this union\nlocal n: number = pick()\nlocal m = t.missing --@alloy-ignore the field arrives at runtime\n```\n\nUse it for a line a tool gets wrong. Where the error is the point, `--@alloy-expect-error` is the one to reach for: it reports when the line comes clean.",
    ),
    (
        "topic:alloy-ignore-start",
        "**--@alloy-ignore-start and --@alloy-ignore-end**\n\nSilences every line between the pair. The two directive lines are outside the region, so an error on either still reports. A lint name or a checker kind after `--@alloy-ignore-start` limits the region to that one; every other diagnostic in it still reports.\n\n```alloy\n--@alloy-ignore-start raw_require\nlocal a = require(\"./a\")\nlocal b = require(\"./b\")\n--@alloy-ignore-end\n```\n\nPairs nest, and the inner one closes first. `--@alloy-ignore-end` with a name closes the innermost start with that name. A start with no end silences to the end of the file and is a `DirectiveError` on its own line; an end that closes nothing is a `DirectiveError` too.",
    ),
    (
        "topic:alloy-expect-error",
        "**--@alloy-expect-error**\n\nSilences a line the way `--@alloy-ignore` does, and is an error itself when that line comes clean. Use it where the error is the point: a test of a lint, of a contract, or of a type.\n\n```alloy\n--@alloy-expect-error the contract rejects a negative count\nlocal p = new Purchase(-1)\n```\n\nText after the name is the reason. The error the directive draws when its line goes clean quotes it, `the `--@alloy-expect-error` directive covers a line with no error: the contract rejects a negative count`, so a file with several says which one went stale. A directive with no reason draws the `missing_reason` lint, `warn` by default.",
    ),
    (
        "topic:alloy-nocheck",
        "**--@alloy-nocheck**\n\nSilences every diagnostic in the file, on any line: the compiler's, the lints, and the checker's. It works from anywhere in the file, and no other directive reports under it.\n\n```alloy\n--@alloy-nocheck\n```\n\nIt is the widest directive there is. A region, `--@alloy-ignore-start`, says the same about a part of a file, and keeps the rest under the tools.",
    ),
    (
        "topic:alloy-lint",
        "**--@alloy-lint**\n\nSets a lint's level for one file, over the `[lint]` table of alloy.toml. The file wins. The levels are `allow`, `warn`, and `deny`, and the name is a lint, a group, one of the type checker's lints, or `luau`.\n\n```alloy\n--@alloy-lint raw_require=allow, explicit_any=deny\n--@alloy-lint naming=allow\n```\n\nSeveral on one line separated by commas, or one per line: both work, from anywhere in the file. The last one in the file decides. A name that is neither a lint nor a group, and a level that is none of the three, is a `DirectiveError` on the directive's line. `alloy lint --list` has the names.",
    ),
    (
        "topic:alloy-side",
        "**--@alloy-side**\n\nSays which side of a remote the file sees: `client` or `server`. A `.client.aly` or `.server.aly` name says the same thing, and this directive says it in a file whose name does not.\n\n```alloy\n--@alloy-side server\nremote Buy from client(item: string)\n\nBuy.on(function(sender, item) end)   -- the server handles\n```\n\nA file with no side is shared and sees both halves of every remote, the way a module that branches on `RunService` does. A directive that contradicts the file's name is a `DirectiveError`. `@cfg(server)` is a different thing: it is a check the emitted code runs, not a decision the compiler makes, so the side does not reach it.",
    ),
    (
        "topic:alloy-preserve",
        "**--@alloy-preserve**\n\nKeeps `alloy flux --fix` and the editor's quick fix off one line. On a line of its own it covers the next line that holds code; at the end of a line with code it covers that line.\n\n```alloy\n--@alloy-preserve the two names read better apart\nlocal name = player and player.Name\n```\n\nThe lint still reports. Its note says the line is preserved, in place of the rewrite it would have offered. Use it where the rewrite is right in general and wrong here; `--@alloy-lint <name>=allow` is the one to reach for when the lint itself does not belong in the file.",
    ),
    (
        "topic:markup",
        "**Markup**\n\nAn `.alx` file is Alloy with markup in it: tags, attributes, fragments, and text, the way JSX sits in TypeScript. The markup lowers to calls on a factory the project names, and the Alloy around it compiles as in any `.aly`.\n\n```alloy\nlocal function Row(props: { item: Item })\n    return (\n        <TextButton Size={UDim2.new(1, 0, 0, 24)} Activated={props.on_pick}>\n            {props.item.name} x{props.item.count}\n        </TextButton>\n    )\nend\n```\n\nA tag names a Roblox class or a component in scope; a component starts with a capital and is called with its props. An attribute is `Name={expr}` or `Name=\"text\"`; an event name takes a function. `{expr}` in a body is a child, and text with `{holes}` interpolates. `<> ... </>` is a fragment, `<!-- -->` a comment. A child that depends on a condition wraps in a function, `{function() return if open then <Menu /> else nil end}`, so a reactive library re-runs it; a bare conditional child is built once, and the `static_conditional_child` lint says so.\n\nThe `[alx]` table of alloy.toml sets the lowering:\n\n```toml\n[alx.factory]\nbackend = \"table\"        # table: create(name)(props); element: create(name, props, children)\ncreate = \"create\"        # the function a tag calls; a bare name must be in scope\nchildren = \"Children\"    # the key children go under; unset, they are numeric entries\nevent = \"OnEvent\"        # wraps an event name: OnEvent(\"Activated\"), or React.Event\ncompute = \"computed\"     # wraps interpolated text; `use` names the reader inside it\nfragment = \"Fragment\"    # the component a fragment builds with; a plain table unset\ninterpolate = \"concat\"   # how text with holes joins\nmerge = \"merge\"          # how a spread group `{...props}` combines\n\n[alx.elements]\nall = \"camel\"            # a naming scheme for every class, or one alias per name\n\n[alx.properties]\nText = \"text\"            # an alias for a property, everywhere or per class\n```\n\nThe level of a markup lint sits with the other lints, under `alx.`:\n\n```toml\n[lint.rules]\nalx.static_conditional_child = \"warn\"    # allow, warn, deny\n```\n\nWith no `[alx]`, the lowering is the table form with `create` in scope. `[fmt.alx]` sets how `alloy fmt` lays a tag out. An ingot may read a markup attribute, `ClassName` for Enamel, and rewrite the tag before the lowering.\n\nIn the editor a tag and an attribute hover and complete from the Roblox API, and the semantic tokens of the lowered code stay out of the markup.",
    ),
    (
        "topic:ingots",
        "**Ingots**\n\nAn ingot is an Alloy extension. It ships as an executable beside an `ingot.toml`, and the compiler and the language server start it once and keep it alive. Over a framed pipe, one request per file, an ingot can:\n\n  transform   edit the Alloy source before the desugar; the line count holds, and every position maps back\n  output      edit the ship Luau after the desugar\n  lint        report findings under its own lint names, with rewrites\n  format      edit a file after Anneal laid it out\n  hover       answer a hover in the editor\n  complete    add completion items\n  actions     add code actions\n  colors      name the colors a file holds, for the editor's swatches and picker\n\nName one in alloy.toml. A path is relative to the root; a release is pinned by version and unpacks once into `.alloy/ingots/`:\n\n```toml\n[ingots]\ntailwind = \"ingots/tailwind\"\nlogger = { repo = \"someone/logger-ingot\", version = \"0.2.0\" }\n\n[ingot.tailwind]\n# the ingot's own options, over the defaults its manifest declares\nsort_classes = true\n```\n\nThe manifest names the ingot, the protocol revision, the hooks the host may send, the options with their defaults, and the lints with their levels:\n\n```toml\nname = \"tailwind\"\napi = 1\nhooks = [\"transform\", \"lint\", \"hover\", \"complete\"]\nrun = \"first\"          # the pass its transform runs in: first, last, or a number\nkinds = [\"alx\"]        # the file kinds it wants; unset means all\n\n[options]\nsort_classes = false\n\n[lints.unknown_class]\ndefault = \"warn\"\nsummary = \"a class no utility defines\"\n```\n\nAn ingot's lint is `<ingot>/<lint>` in `[lint]`, and the ingot's name is a group, so `allow = [\"tailwind\"]` silences all of them. An option written as `{ default = false, doc = \"...\" }` carries its text into the editor: `alloy build` writes the project's schema, `.alloy/alloy.schema.json`, with every ingot's options and lint names, and the `#:schema` line at the top of alloy.toml makes the editor complete them. `alloy lint --list` shows them under the ingot; `alloy doc tailwind/unknown_class` explains one.\n\nWrite one in Rust with the `alloy-ingot` crate: implement `Handler`, call `serve`. `alloy ingot new <name>` writes the project, `alloy ingot info <dir>` prints what a manifest declares, and `alloy ingot run <dir> <file>` pushes one file through it and reports the line count, because a transform that adds a line breaks the map. An edit is a byte span and its text; the host applies every edit of one reply at once, so no edit sees another's output. An ingot that hangs costs one request: the host kills it after a timeout and reports the loss.\n\nThe design follows larvae's native worms. Nothing is embedded: no interpreter, no wasm.",
    ),
    (
        "topic:mount",
        "**Project files and mounts**\n\nAlloy needs to know where each folder lands in the DataModel. Two things can say so, and a project writes one of them.\n\n**The project file.** A root with a Rojo or Argon project file needs nothing in alloy.toml: Alloy reads the file and takes its tree as written. It reads `default.project.json`, or the file `[project] file` names, or the one `*.project.json` at the root.\n\n```json\n{\n  \"name\": \"game\",\n  \"tree\": {\n    \"$className\": \"DataModel\",\n    \"ReplicatedStorage\": {\n      \"$className\": \"ReplicatedStorage\",\n      \"Shared\": { \"$path\": \"src/shared\" },\n      \"Packages\": { \"$path\": \"Packages\" },\n      \"Alloy\": { \"$path\": \"build/alloy.luau\" }\n    },\n    \"ServerScriptService\": {\n      \"$className\": \"ServerScriptService\",\n      \"Server\": { \"$path\": \"src/server\" }\n    }\n  }\n}\n```\n\nEvery `$path` in the tree is a folder on disk with a place in the DataModel, at any depth. `$className`, `$properties`, and `$ignoreUnknownInstances` are directives, so they never become instances. `.server.` and `.client.` in a file name pick the script class, `init` names its directory, and a container between a service and a leaf, `StarterPlayerScripts`, keeps its own class.\n\nAlloy derives four things from that tree, and writes none of them back into your file:\n\n  .alloy/build.project.json   the same tree over the output\n  .alloy/sourcemap.json       the instance tree with the source paths\n  the @alias rewrite          instance paths in the ship artifact\n  the runtime's place         where build/alloy.luau lands\n\nThe build project points every `$path` under `[build] in` at its output under `[build] out`, and leaves any other path as it is; `rojo serve` and `rojo build` read that one. The sourcemap has the shape Rojo writes, and the language server reads it. Roblox reads no `.luaurc`, so `require(\"@shared/economy\")` in the ship artifact becomes `require(\"@game/ReplicatedStorage/Shared/economy\")`.\n\n`default.project.json` is yours: Alloy never writes it when it is there. The runtime lands where the tree already mounts `build/alloy.luau`, else inside the node that mounts `[build] out`, else at `@game/ReplicatedStorage/Alloy`; `[project] runtime` names it outright.\n\n**The aliases.** They come from `.config.luau` or `.luaurc`, which is where Luau reads them, and where the editor and `alloy flux` already read them. An alias names a folder on disk; the tree says where that folder lands; the ship artifact writes the instance path.\n\n```json\n{ \"languageMode\": \"strict\", \"aliases\": { \"shared\": \"src/shared\", \"pkg\": \"Packages\" } }\n```\n\nA data file resolves the same way: `import config from \"@shared/data/config.json\"` reads `src/shared/data/config.json` and requires the module the build writes beside it. `alloy init` writes the `@alloy` alias once; nothing else writes to these files.\n\n**Watch mode.** `alloy build --watch` polls the sources, alloy.toml, the project file, and every folder the tree mounts, so a new file anywhere in the tree writes a new sourcemap and a new build project.\n\n**The mount table.** A tool that reads a Rojo or Argon project file needs no table. A sync tool with its own format has no such file, so alloy.toml describes the tree instead:\n\n```toml\n[project]\nname = \"game\"\n\n[mount]\n# alias = [path, mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nclient = [\"src/client\", \"@game/StarterPlayer/StarterPlayerScripts/Client\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n```\n\nThe table is the tree when the project writes one, over any project file at the root. Two keys of `[project]` say how far it reaches, and both are on:\n\n```toml\n[project]\n# write default.project.json and .alloy/build.project.json from [mount]\nsource_of_truth = true\n# serve the mount names as aliases, beside the Luau config ones\nmount_aliases = true\n```\n\n`source_of_truth = false` writes neither project file and derives no build project: the sync tool of the project owns the tree, and the table is left to rewrite an `@alias` require into an instance path in the ship artifact. `mount_aliases = false` leaves the aliases to `.config.luau` or `.luaurc` alone, so a mount name completes and resolves nowhere.\n\nWith both on, `alloy build` writes `default.project.json` over the sources, so `rojo serve` has a file to read, and `@shared/x` completes in the editor whether the Luau configuration names it or not. A name in the Luau configuration always wins over a mount of that name.\n\n**Neither.** A root with no project file and no table still builds. The output tree mirrors the source tree, emitted code requires the runtime by a relative path, and `@alias` requires stay as they are: no instance paths, no sourcemap, no project files.",
    ),
    (
        "topic:data",
        "**JSON and TOML data**\n\nA source imports a `.json` or `.toml` file as a table. Three forms name one:\n\n```alloy\nimport data from \"./data.json\"\nimport { coins, pets } from \"./config.toml\"\nlocal limits = import(\"./limits.toml\")\nlocal raw = require(\"./raw.json\")\n```\n\nThe emit drops the extension: `require(\"./data\")`. `alloy build` writes each data file a source names as a Luau module at the same place in the output, `src/data.json` becomes `build/data.luau`, and `clean` keeps it. A file that no source names stays as it is; `alloy.toml` and `default.project.json` never build.\n\nThe module returns the document as a table:\n\n```luau\nreturn {\n    name = \"game\",\n    [\"max-players\"] = 12,\n    pets = {\n        {\n            name = \"cat\",\n            legs = 4,\n        },\n    },\n}\n```\n\nKeys keep the document's order. A key that is not a Luau name, or is a keyword, goes in brackets. A JSON `null` is `nil`, a TOML datetime is a string, and a whole number has no `.0`.\n\nThe type check and the language server write the same module into their mirrors, so hover on `data` shows its table type, `data.` completes its keys, `import { | } from \"./config.toml\"` lists the top-level keys with their types, and go to definition on the path or on a name opens the file.\n\nThree cases are diagnostics on the import: the file is missing, the document does not parse (the message names the line), and the file's stem collides with a module beside it. `x.json` beside `x.aly` or `x.luau` would build the same `x.luau`, so one of them gets a new name. A data path is relative, `./` or `../`, and stays under `[build] in`.",
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

/// What one member of a std type is: a function on the type table, a
/// function on a value, or a plain member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemberKind {
    Static,
    Method,
    Field,
    Constant,
}

impl MemberKind {
    /// The word `alloy doc --json` writes.
    pub fn name(self) -> &'static str {
        match self {
            MemberKind::Static => "static",
            MemberKind::Method => "method",
            MemberKind::Field => "field",
            MemberKind::Constant => "constant",
        }
    }

    /// The heading a run of members of this kind sits under.
    pub fn heading(self) -> &'static str {
        match self {
            MemberKind::Static => "Statics",
            MemberKind::Method => "Methods",
            MemberKind::Field => "Fields",
            MemberKind::Constant => "Constants",
        }
    }
}

/// The order the headings print in.
pub const MEMBER_KINDS: [MemberKind; 4] = [
    MemberKind::Static,
    MemberKind::Method,
    MemberKind::Field,
    MemberKind::Constant,
];

/// One documented member of a std type. A reference page gives each one
/// a section: the signature, what it does, and an example.
pub struct Member {
    pub name: &'static str,
    pub kind: MemberKind,
    /// The signature as Alloy prints it: `HashMap:get(key: K): V?`.
    pub signature: &'static str,
    pub doc: &'static str,
    /// Alloy that `alloy check` accepts on its own. The test
    /// `every_member_example_compiles` holds that.
    pub example: &'static str,
}

/// The members of every std type, by the key of its entry in `TABLE`.
pub const MEMBERS: &[(&str, &[Member])] = &[
    (
        "HashMap",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "HashMap.new<K, V>(): HashMap<K, V>",
                doc: "An empty map. The annotation on the binding fixes `K` and `V`, since an empty map infers neither.",
                example: "local prices: HashMap<string, number> = HashMap.new()\nprices:set(\"sword\", 10)",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "HashMap.from<K, V>(t: { [K]: V }): HashMap<K, V>",
                doc: "A new map with the pairs of a plain table. The table itself is left alone.",
                example: "local prices = HashMap.from({ sword = 10, pet = 25 })\nprint(prices:len())",
            },
            Member {
                name: "get",
                kind: MemberKind::Method,
                signature: "HashMap:get(key: K): V?",
                doc: "The value under `key`, or nil when the map has no such key. The result is optional, so guard it or end the chain with `??`.",
                example: "local prices = HashMap.from({ sword = 10 })\nlocal price = prices:get(\"sword\") ?? 0\nprint(price)",
            },
            Member {
                name: "set",
                kind: MemberKind::Method,
                signature: "HashMap:set(key: K, value: V): HashMap<K, V>",
                doc: "Stores `value` under `key` and returns the map, so calls chain. A `set` to nil removes the key and drops the count, so `len` stays true.",
                example: "local prices: HashMap<string, number> = HashMap.new()\nprices:set(\"gem\", 5):set(\"sword\", 10)\nprint(prices:len())",
            },
            Member {
                name: "has",
                kind: MemberKind::Method,
                signature: "HashMap:has(key: K): boolean",
                doc: "Whether the map holds `key`. A key whose value is nil is not held.",
                example: "local prices = HashMap.from({ gem = 5 })\nif prices:has(\"gem\") then\n    print(\"in stock\")\nend",
            },
            Member {
                name: "remove",
                kind: MemberKind::Method,
                signature: "HashMap:remove(key: K): V?",
                doc: "Removes `key` and returns the value it held, or nil when there was none.",
                example: "local prices = HashMap.from({ gem = 5 })\nlocal old = prices:remove(\"gem\")\nprint(old)",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "HashMap:len(): number",
                doc: "The number of keys. The map counts as keys come and go, so this costs the same at any size.",
                example: "local prices = HashMap.from({ gem = 5, sword = 10 })\nprint(prices:len())",
            },
            Member {
                name: "keys",
                kind: MemberKind::Method,
                signature: "HashMap:keys(): K[]",
                doc: "The keys as an array, in no set order. The array carries the Array methods.",
                example: "local prices = HashMap.from({ gem = 5 })\nprint(prices:keys():join(\", \"))",
            },
            Member {
                name: "values",
                kind: MemberKind::Method,
                signature: "HashMap:values(): V[]",
                doc: "The values as an array, in the order `keys` gives.",
                example: "local prices = HashMap.from({ gem = 5 })\nprint(prices:values():len())",
            },
            Member {
                name: "entries",
                kind: MemberKind::Method,
                signature: "HashMap:entries(): () -> (K?, V?)",
                doc: "An iterator over the pairs, for a `for` loop. The order is the table's own.",
                example: "local prices = HashMap.from({ gem = 5 })\nfor key, value in prices:entries() do\n    print(key, value)\nend",
            },
            Member {
                name: "get_or_insert",
                kind: MemberKind::Method,
                signature: "HashMap:get_or_insert(key: K, default: V): V",
                doc: "The value under `key`. When there is none the map stores `default` first, so the result is never nil.",
                example: "local counts: HashMap<string, number> = HashMap.new()\nlocal n = counts:get_or_insert(\"hits\", 0)\nprint(n)",
            },
            Member {
                name: "clear",
                kind: MemberKind::Method,
                signature: "HashMap:clear()",
                doc: "Removes every key and sets the count to zero. The map itself stays, so every reference to it sees the empty map.",
                example: "local prices = HashMap.from({ gem = 5 })\nprices:clear()",
            },
        ],
    ),
    (
        "Set",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Set.new<T>(): Set<T>",
                doc: "An empty set. The annotation on the binding fixes `T`.",
                example: "local seen: Set<string> = Set.new()\nseen:add(\"a\")",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Set.from<T>(items: { T }): Set<T>",
                doc: "A new set with the items of an array. A repeated item joins once.",
                example: "local seen = Set.from({ 1, 2, 2, 3 })\nprint(seen:len())",
            },
            Member {
                name: "add",
                kind: MemberKind::Method,
                signature: "Set:add(value: T): Set<T>",
                doc: "Adds `value` and returns the set, so calls chain. Adding a member again changes nothing.",
                example: "local seen: Set<string> = Set.new()\nif seen:add(\"a\"):has(\"a\") then\n    print(\"added\")\nend",
            },
            Member {
                name: "has",
                kind: MemberKind::Method,
                signature: "Set:has(value: T): boolean",
                doc: "Whether `value` is a member.",
                example: "local seen = $set[1, 2, 3]\nprint(seen:has(2))",
            },
            Member {
                name: "remove",
                kind: MemberKind::Method,
                signature: "Set:remove(value: T): boolean",
                doc: "Removes `value` and says whether it was there.",
                example: "local seen = $set[1, 2]\nprint(seen:remove(1))",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Set:len(): number",
                doc: "The number of members. The set counts as members come and go.",
                example: "local seen = $set[1, 2, 3]\nprint(seen:len())",
            },
            Member {
                name: "union",
                kind: MemberKind::Method,
                signature: "Set:union(other: Set<T>): Set<T>",
                doc: "A new set of the members of both. Neither input changes.",
                example: "local a = $set[1, 2]\nlocal b = $set[2, 3]\nprint(a:union(b):len())",
            },
            Member {
                name: "intersection",
                kind: MemberKind::Method,
                signature: "Set:intersection(other: Set<T>): Set<T>",
                doc: "A new set of the members that both hold.",
                example: "local a = $set[1, 2]\nlocal b = $set[2, 3]\nprint(a:intersection(b):len())",
            },
            Member {
                name: "difference",
                kind: MemberKind::Method,
                signature: "Set:difference(other: Set<T>): Set<T>",
                doc: "A new set of this set's members that `other` does not hold.",
                example: "local a = $set[1, 2]\nlocal b = $set[2, 3]\nprint(a:difference(b):len())",
            },
            Member {
                name: "to_array",
                kind: MemberKind::Method,
                signature: "Set:to_array(): T[]",
                doc: "The members as an array, in no set order.",
                example: "local seen = $set[1, 2, 3]\nprint(seen:to_array():len())",
            },
        ],
    ),
    (
        "Array",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Array.new<T>(): T[]",
                doc: "An empty array with the Array metatable. `[ ]` is the literal for one.",
                example: "local xs: number[] = Array.new()\nxs:push(1)",
            },
            Member {
                name: "with_capacity",
                kind: MemberKind::Static,
                signature: "Array.with_capacity<T>(n: number): T[]",
                doc: "An empty array with room for `n` items already allocated. Use it before a loop that pushes a known count.",
                example: "local xs: number[] = Array.with_capacity(3)\nxs:push(1, 2, 3)",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Array.from<T>(t: { T }): T[]",
                doc: "The table `t` as an array: it gains the metatable and stays the same table, so no copy happens.",
                example: "local raw = { 1, 2, 3 }\nlocal xs = Array.from(raw)\nprint(xs:len())",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Array:len(): number",
                doc: "The number of items, the `#` of the table.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "Array:is_empty(): boolean",
                doc: "Whether the array holds no items.",
                example: "local xs: number[] = [ ]\nprint(xs:is_empty())",
            },
            Member {
                name: "push",
                kind: MemberKind::Method,
                signature: "Array:push(...: T): Array<T>",
                doc: "Appends the values in the order given. It writes into the array and returns it, so calls chain.",
                example: "local xs = [ 1 ]\nxs:push(2, 3):push(4)\nprint(xs:len())",
            },
            Member {
                name: "pop",
                kind: MemberKind::Method,
                signature: "Array:pop(): T?",
                doc: "Removes the last item and returns it, or nil when the array is empty.",
                example: "local xs = [ 1, 2 ]\nprint(xs:pop())",
            },
            Member {
                name: "first",
                kind: MemberKind::Method,
                signature: "Array:first(): T?",
                doc: "The first item, or nil when the array is empty.",
                example: "local xs = [ 1, 2 ]\nprint(xs:first())",
            },
            Member {
                name: "last",
                kind: MemberKind::Method,
                signature: "Array:last(): T?",
                doc: "The last item, or nil when the array is empty.",
                example: "local xs = [ 1, 2 ]\nprint(xs:last())",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "Array:map<U>(f: (T, number) -> U): U[]",
                doc: "A new array of `f(item, index)` for each item. The result names a second alias of the same shape, so the third `map` in one chain types as `any`.",
                example: "local xs = [ 1, 2, 3 ]\nlocal doubled = xs:map(function(x) return x * 2 end)\nprint(doubled:join(\", \"))",
            },
            Member {
                name: "filter",
                kind: MemberKind::Method,
                signature: "Array:filter(f: (T, number) -> boolean): T[]",
                doc: "A new array of the items where `f(item, index)` holds.",
                example: "local xs = [ 1, 2, 3, 4 ]\nlocal even = xs:filter(function(x) return x % 2 == 0 end)\nprint(even:len())",
            },
            Member {
                name: "find",
                kind: MemberKind::Method,
                signature: "Array:find(f: (T, number) -> boolean): T?",
                doc: "The first item where `f(item, index)` holds, or nil.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:find(function(x) return x > 1 end))",
            },
            Member {
                name: "find_index",
                kind: MemberKind::Method,
                signature: "Array:find_index(f: (T, number) -> boolean): number?",
                doc: "The index of the first item where `f(item, index)` holds, or nil.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:find_index(function(x) return x > 1 end))",
            },
            Member {
                name: "contains",
                kind: MemberKind::Method,
                signature: "Array:contains(value: T): boolean",
                doc: "Whether an item equals `value`. `value in xs` is the same test.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:contains(2))",
            },
            Member {
                name: "index_of",
                kind: MemberKind::Method,
                signature: "Array:index_of(value: T): number?",
                doc: "The index of the first item that equals `value`, or nil.",
                example: "local xs = [ \"a\", \"b\" ]\nprint(xs:index_of(\"b\"))",
            },
            Member {
                name: "for_each",
                kind: MemberKind::Method,
                signature: "Array:for_each(f: (T, number) -> ())",
                doc: "Calls `f(item, index)` for each item, in order, and returns nothing.",
                example: "local xs = [ 1, 2 ]\nxs:for_each(function(x, i) print(i, x) end)",
            },
            Member {
                name: "reduce",
                kind: MemberKind::Method,
                signature: "Array:reduce<U>(f: (U, T, number) -> U, init: U): U",
                doc: "Folds the array: `acc = f(acc, item, index)` from `init`, and the last `acc` is the result. Annotate the accumulator when its body needs the type.",
                example: "local xs = [ 1, 2, 3 ]\nlocal sum = xs:reduce(function(acc: number, x) return acc + x end, 0)\nprint(sum)",
            },
            Member {
                name: "slice",
                kind: MemberKind::Method,
                signature: "Array:slice(from: number, to: number?): T[]",
                doc: "A new array of the items from index `from` to index `to`, both inclusive. `to` defaults to the last index.",
                example: "local xs = [ 1, 2, 3, 4 ]\nprint(xs:slice(2, 3):join(\", \"))",
            },
            Member {
                name: "concat",
                kind: MemberKind::Method,
                signature: "Array:concat(other: T[]): T[]",
                doc: "A new array of this one's items followed by `other`'s. Neither input changes.",
                example: "local xs = [ 1, 2 ]\nprint(xs:concat([ 3 ]):len())",
            },
            Member {
                name: "reverse",
                kind: MemberKind::Method,
                signature: "Array:reverse(): T[]",
                doc: "A new array with the items in the other order. This one is left as it was.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:reverse():first())",
            },
            Member {
                name: "sort_by",
                kind: MemberKind::Method,
                signature: "Array:sort_by(less: (T, T) -> boolean): T[]",
                doc: "Sorts in place with `less(a, b)` and returns the same array, so a chain reads on. The sort is not stable.",
                example: "local xs = [ 3, 1, 2 ]\nxs:sort_by(function(a, b) return a < b end)\nprint(xs:join(\", \"))",
            },
            Member {
                name: "join",
                kind: MemberKind::Method,
                signature: "Array:join(sep: string?): string",
                doc: "Every item through `tostring`, joined by `sep`. Without `sep` the parts run together.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:join(\", \"))",
            },
        ],
    ),
    (
        "ReadArray",
        &[
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "ReadArray:len(): number",
                doc: "The number of items.",
                example: "local xs: read number[] = [ 1, 2, 3 ]\nprint(xs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "ReadArray:is_empty(): boolean",
                doc: "Whether the array holds no items.",
                example: "local xs: read number[] = [ ]\nprint(xs:is_empty())",
            },
            Member {
                name: "first",
                kind: MemberKind::Method,
                signature: "ReadArray:first(): T?",
                doc: "The first item, or nil when the array is empty.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:first())",
            },
            Member {
                name: "last",
                kind: MemberKind::Method,
                signature: "ReadArray:last(): T?",
                doc: "The last item, or nil when the array is empty.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:last())",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "ReadArray:map<U>(f: (T, number) -> U): U[]",
                doc: "A new array of `f(item, index)` for each item. The result is a writable `Array<U>`.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:map(function(x) return x * 2 end):len())",
            },
            Member {
                name: "filter",
                kind: MemberKind::Method,
                signature: "ReadArray:filter(f: (T, number) -> boolean): T[]",
                doc: "A new array of the items where `f(item, index)` holds.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:filter(function(x) return x > 1 end):len())",
            },
            Member {
                name: "find",
                kind: MemberKind::Method,
                signature: "ReadArray:find(f: (T, number) -> boolean): T?",
                doc: "The first item where `f(item, index)` holds, or nil.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:find(function(x) return x > 1 end))",
            },
            Member {
                name: "find_index",
                kind: MemberKind::Method,
                signature: "ReadArray:find_index(f: (T, number) -> boolean): number?",
                doc: "The index of the first item where `f(item, index)` holds, or nil.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:find_index(function(x) return x > 1 end))",
            },
            Member {
                name: "contains",
                kind: MemberKind::Method,
                signature: "ReadArray:contains(value: T): boolean",
                doc: "Whether an item equals `value`.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:contains(2))",
            },
            Member {
                name: "index_of",
                kind: MemberKind::Method,
                signature: "ReadArray:index_of(value: T): number?",
                doc: "The index of the first item that equals `value`, or nil.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:index_of(2))",
            },
            Member {
                name: "for_each",
                kind: MemberKind::Method,
                signature: "ReadArray:for_each(f: (T, number) -> ())",
                doc: "Calls `f(item, index)` for each item, in order.",
                example: "local xs: read number[] = [ 1, 2 ]\nxs:for_each(function(x) print(x) end)",
            },
            Member {
                name: "reduce",
                kind: MemberKind::Method,
                signature: "ReadArray:reduce<U>(f: (U, T, number) -> U, init: U): U",
                doc: "Folds the array from `init`. This is how a function that takes a `read T[]` sums it.",
                example: "local function total(xs: read number[]): number\n    return xs:reduce(function(acc: number, x) return acc + x end, 0)\nend\nprint(total([ 1, 2 ]))",
            },
            Member {
                name: "slice",
                kind: MemberKind::Method,
                signature: "ReadArray:slice(from: number, to: number?): T[]",
                doc: "A new array of the items from `from` to `to`, both inclusive. The result is writable.",
                example: "local xs: read number[] = [ 1, 2, 3 ]\nprint(xs:slice(2):len())",
            },
            Member {
                name: "concat",
                kind: MemberKind::Method,
                signature: "ReadArray:concat(other: T[]): T[]",
                doc: "A new array of this one's items followed by `other`'s.",
                example: "local xs: read number[] = [ 1 ]\nprint(xs:concat([ 2 ]):len())",
            },
            Member {
                name: "join",
                kind: MemberKind::Method,
                signature: "ReadArray:join(sep: string?): string",
                doc: "Every item through `tostring`, joined by `sep`.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:join(\", \"))",
            },
        ],
    ),
    (
        "WriteArray",
        &[Member {
            name: "push",
            kind: MemberKind::Method,
            signature: "WriteArray:push(...: T): WriteArray<T>",
            doc: "Appends the values in the order given and returns the sink, so calls chain. It is the one method a `write T[]` has: a sink takes items and never reads them back.",
            example: "local function log_to(sink: write string[])\n    sink:push(\"line\")\nend\nlog_to([ ])",
        }],
    ),
    (
        "Future",
        &[
            Member {
                name: "resolve",
                kind: MemberKind::Static,
                signature: "Future.resolve<T>(value: T): Future<T>",
                doc: "A Future that has already settled with `value`. Every `await` on it returns at once.",
                example: "local ready = Future.resolve(42)\nprint(await ready)",
            },
            Member {
                name: "reject",
                kind: MemberKind::Static,
                signature: "Future.reject(error: any): Future<never>",
                doc: "A Future that has already failed with `error`. An `await` on it rethrows, and `try await` returns that `Err` from the function.",
                example: "local bad = Future.reject(\"no disk\")\nbad:andThen(nil, function(e) print(e) end)",
            },
            Member {
                name: "delay",
                kind: MemberKind::Static,
                signature: "Future.delay(seconds: number): Future<()>",
                doc: "A Future that settles after the wait, with no value. It is the async form of `task.wait`.",
                example: "await Future.delay(0.5)\nprint(\"later\")",
            },
            Member {
                name: "all",
                kind: MemberKind::Static,
                signature: "Future.all<T>(futures: Future<T>[]): Future<T[]>",
                doc: "One Future of every value, in the order of the list. The first failure fails the whole thing, and the rest keep running. Every Future in the list carries one type here; `race` and `any` are the ones that take a mixed list.",
                example: "local both = await Future.all([Future.resolve(1), Future.resolve(2)])\nprint(both:len())",
            },
            Member {
                name: "race",
                kind: MemberKind::Static,
                signature: "Future.race<T>(futures: Future<T>[]): Future<T>",
                doc: "The first Future to settle, whether it succeeds or fails. Use it for a timeout beside real work. The futures in the list need not carry one type: the result is the union of their value types, so a value beside a `Future.delay` is optional.",
                example: "local first = await Future.race([Future.resolve(1), Future.resolve(2)])\nprint(first)",
            },
            Member {
                name: "any",
                kind: MemberKind::Static,
                signature: "Future.any<T>(futures: Future<T>[]): Future<T>",
                doc: "The first Future to succeed. It fails only when every one of them fails. As with `race`, a list whose futures carry different types gives the union of them.",
                example: "local ok = await Future.any([Future.reject(\"a\"), Future.resolve(2)])\nprint(ok)",
            },
            Member {
                name: "all_settled",
                kind: MemberKind::Static,
                signature: "Future.all_settled<T>(futures: Future<T>[]): Future<Result<T, any>[]>",
                doc: "One Result per Future, in order. No failure fails it. The error side is `any`, since a rejection carries whatever it was rejected with.",
                example: "local rs = await Future.all_settled([Future.resolve(1), Future.reject(\"no\")])\nprint(rs:len())",
            },
            Member {
                name: "andThen",
                kind: MemberKind::Method,
                signature: "Future:andThen(on_resolve: ((T) -> ...any)?, on_reject: ((any) -> ...any)?): Future<T>",
                doc: "Runs a callback when the Future settles and returns the same Future, so calls chain. What the callback returns is dropped: the Future keeps its own value.",
                example: "local f = Future.resolve(1):andThen(function(n) print(n) end)\nprint(await f)",
            },
            Member {
                name: "and_then",
                kind: MemberKind::Method,
                signature: "Future:and_then(on_resolve: ((T) -> ...any)?, on_reject: ((any) -> ...any)?): Future<T>",
                doc: "The same function under a snake_case name. `andThen` is the one camelCase name the std carries, so a file that keeps to one spelling writes this.",
                example: "local f = Future.resolve(1):and_then(function(n) print(n) end)\nprint(await f)",
            },
            Member {
                name: "cancel",
                kind: MemberKind::Method,
                signature: "Future:cancel()",
                doc: "Closes the task behind the Future. An `await` on a cancelled Future raises.",
                example: "local slow = Future.delay(10)\nslow:cancel()",
            },
            Member {
                name: "is_settled",
                kind: MemberKind::Method,
                signature: "Future:is_settled(): boolean",
                doc: "Whether the Future has a value or a failure. A cancelled Future is not settled.",
                example: "local ready = Future.resolve(1)\nprint(ready:is_settled())",
            },
        ],
    ),
    (
        "Result",
        &[
            Member {
                name: "pcall",
                kind: MemberKind::Static,
                signature: "Result.pcall<T>(f: (...any) -> T, ...: any): Result<T, string>",
                doc: "Calls `f` with the arguments after it: `Ok` of what it returns, or `Err` of what it threw. The value type follows `f`, and the `Err` carries the traceback in `trace`.",
                example: "local r = Result.pcall(string.rep, \"a\", 3)\nprint(r:unwrap_or(\"\"))",
            },
            Member {
                name: "unwrap",
                kind: MemberKind::Method,
                signature: "Result:unwrap(): T",
                doc: "The value of an `Ok`. An `Err` raises, with the error and its trace in the message. Reach for it when an `Err` is a bug, not a case.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:unwrap())",
            },
            Member {
                name: "expect",
                kind: MemberKind::Method,
                signature: "Result:expect(message: string): T",
                doc: "The value of an `Ok`. An `Err` raises with `message`, which says what the caller expected.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:expect(\"the config sets a width\"))",
            },
            Member {
                name: "unwrap_or",
                kind: MemberKind::Method,
                signature: "Result:unwrap_or<D>(default: D): T | D",
                doc: "The value of an `Ok`, or `default`. The result type is `T | D`, so `r:unwrap_or(nil)` is `T?`.",
                example: "local r: Result<number, string> = Err(\"no\")\nprint(r:unwrap_or(0))",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "Result:map<U>(f: (T) -> U): Result<U, E>",
                doc: "`Ok(f(value))`, or the same `Err` untouched. The mapped Result names a second alias, so a third `map` in one chain types as `any`.",
                example: "local r: Result<number, string> = Ok(2)\nprint(r:map(function(n) return n * 2 end):unwrap())",
            },
            Member {
                name: "map_err",
                kind: MemberKind::Method,
                signature: "Result:map_err<F>(f: (E) -> F): Result<T, F>",
                doc: "`Err(f(error))`, or the same `Ok` untouched. Use it to turn a low level error into one the caller names.",
                example: "local r: Result<number, string> = Err(\"io\")\nprint(r:map_err(function(e) return `read: {e}` end):is_err())",
            },
            Member {
                name: "is_ok",
                kind: MemberKind::Method,
                signature: "Result:is_ok(): boolean",
                doc: "Whether the Result is an `Ok`. It narrows nothing; a `match` or `if local Ok(v) = r` binds the value.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:is_ok())",
            },
            Member {
                name: "is_err",
                kind: MemberKind::Method,
                signature: "Result:is_err(): boolean",
                doc: "Whether the Result is an `Err`.",
                example: "local r: Result<number, string> = Err(\"no\")\nprint(r:is_err())",
            },
            Member {
                name: "ok",
                kind: MemberKind::Method,
                signature: "Result:ok(): T?",
                doc: "The value of an `Ok`, or nil for an `Err`. It drops the error, which is what `??` wants on the other side.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:ok() ?? 0)",
            },
            Member {
                name: "tag",
                kind: MemberKind::Field,
                signature: "Result.tag: \"Ok\" | \"Err\"",
                doc: "Which case the Result is, as a string. A `match` reads it for you; the field is there for code that stores or sends the tag.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r.tag)",
            },
            Member {
                name: "trace",
                kind: MemberKind::Field,
                signature: "Result.trace: string?",
                doc: "The traceback of an `Err`. `try` and `Result.pcall` fill it in, `Err(e, trace)` sets it by hand, and `unwrap` prints it under the error.",
                example: "local r = Result.pcall(error, \"boom\")\nprint(r.trace ?? \"no trace\")",
            },
        ],
    ),
    (
        "Queue",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Queue.new<T>(): Queue<T>",
                doc: "An empty queue. The annotation on the binding fixes `T`.",
                example: "local jobs: Queue<string> = Queue.new()\njobs:push(\"build\")",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Queue.from<T>(items: { T }): Queue<T>",
                doc: "A queue filled from an array, front to back in the array's order.",
                example: "local jobs = Queue.from({ \"build\", \"test\" })\nprint(jobs:pop())",
            },
            Member {
                name: "push",
                kind: MemberKind::Method,
                signature: "Queue:push(value: T)",
                doc: "Appends `value` at the back. The ring of indices makes this cost the same at any size.",
                example: "local jobs: Queue<string> = Queue.new()\njobs:push(\"build\")",
            },
            Member {
                name: "pop",
                kind: MemberKind::Method,
                signature: "Queue:pop(): T?",
                doc: "Removes the front item and returns it, or nil when the queue is empty. An emptied queue starts its indices over, so they never run away.",
                example: "local jobs = Queue.from({ \"build\" })\nwhile local job = jobs:pop() do\n    print(job)\nend",
            },
            Member {
                name: "peek",
                kind: MemberKind::Method,
                signature: "Queue:peek(): T?",
                doc: "The front item without removing it, or nil when the queue is empty.",
                example: "local jobs = Queue.from({ \"build\" })\nprint(jobs:peek())",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Queue:len(): number",
                doc: "The number of items waiting.",
                example: "local jobs = Queue.from({ \"build\", \"test\" })\nprint(jobs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "Queue:is_empty(): boolean",
                doc: "Whether the queue holds nothing.",
                example: "local jobs: Queue<string> = Queue.new()\nprint(jobs:is_empty())",
            },
            Member {
                name: "clear",
                kind: MemberKind::Method,
                signature: "Queue:clear()",
                doc: "Drops every item and resets the indices.",
                example: "local jobs = Queue.from({ \"build\" })\njobs:clear()",
            },
            Member {
                name: "to_array",
                kind: MemberKind::Method,
                signature: "Queue:to_array(): T[]",
                doc: "The items front to back, as a new array. The queue keeps them; a `for` loop over the queue reads the same order without a pop.",
                example: "local jobs = Queue.from({ \"build\", \"test\" })\nprint(jobs:to_array():join(\", \"))",
            },
        ],
    ),
    (
        "Heap",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Heap.new<T>(less: ((T, T) -> boolean)?): Heap<T>",
                doc: "An empty heap. `less` decides which value is least and defaults to `<`, so numbers and strings need none and tables take one.",
                example: "local open = Heap.new(function(a: number, b: number) return a < b end)\nopen:push(3)",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Heap.from<T>(items: { T }, less: ((T, T) -> boolean)?): Heap<T>",
                doc: "A heap built from an array, pushed item by item.",
                example: "local costs = Heap.from({ 3, 1, 2 })\nprint(costs:pop())",
            },
            Member {
                name: "push",
                kind: MemberKind::Method,
                signature: "Heap:push(value: T)",
                doc: "Adds a value and sifts it up. The cost grows with the log of the size.",
                example: "local costs: Heap<number> = Heap.new()\ncosts:push(3)",
            },
            Member {
                name: "pop",
                kind: MemberKind::Method,
                signature: "Heap:pop(): T?",
                doc: "Removes the least value under `less` and returns it, or nil when the heap is empty.",
                example: "local costs = Heap.from({ 3, 1, 2 })\nprint(costs:pop())",
            },
            Member {
                name: "peek",
                kind: MemberKind::Method,
                signature: "Heap:peek(): T?",
                doc: "The least value without removing it, or nil when the heap is empty.",
                example: "local costs = Heap.from({ 3, 1 })\nprint(costs:peek())",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Heap:len(): number",
                doc: "The number of values held.",
                example: "local costs = Heap.from({ 3, 1 })\nprint(costs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "Heap:is_empty(): boolean",
                doc: "Whether the heap holds nothing.",
                example: "local costs: Heap<number> = Heap.new()\nprint(costs:is_empty())",
            },
            Member {
                name: "clear",
                kind: MemberKind::Method,
                signature: "Heap:clear()",
                doc: "Drops every value.",
                example: "local costs = Heap.from({ 3, 1 })\ncosts:clear()",
            },
            Member {
                name: "to_array",
                kind: MemberKind::Method,
                signature: "Heap:to_array(): T[]",
                doc: "The values sorted under `less`, as a new array. The heap keeps its own; a `for` loop over the heap reads the same order without a pop.",
                example: "local costs = Heap.from({ 3, 1, 2 })\nprint(costs:to_array():first())",
            },
        ],
    ),
    (
        "Scope",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Scope.new(): Scope",
                doc: "An empty cleanup bag.",
                example: "local scope = Scope.new()\nprint(scope:len())",
            },
            Member {
                name: "add",
                kind: MemberKind::Method,
                signature: "Scope:add<T>(item: T): T",
                doc: "Adds a cleanup and gives `item` straight back, so a binding reads as it did before. It takes anything `delete` accepts, or a function.",
                example: "local scope = Scope.new()\nlocal stop = scope:add(function() print(\"bye\") end)\nprint(stop ~= nil)",
            },
            Member {
                name: "remove",
                kind: MemberKind::Method,
                signature: "Scope:remove(item: any): boolean",
                doc: "Takes a cleanup out without running it, and says whether it was there.",
                example: "local scope = Scope.new()\nlocal f = scope:add(function() end)\nprint(scope:remove(f))",
            },
            Member {
                name: "clean",
                kind: MemberKind::Method,
                signature: "Scope:clean()",
                doc: "Runs every cleanup, newest first, and empties the bag. A function is called; anything else goes through `delete`.",
                example: "local scope = Scope.new()\nscope:add(function() print(\"bye\") end)\nscope:clean()",
            },
            Member {
                name: "extend",
                kind: MemberKind::Method,
                signature: "Scope:extend(): Scope",
                doc: "A child scope, already added to this one, so cleaning the parent cleans the child.",
                example: "local scope = Scope.new()\nlocal child = scope:extend()\nchild:add(function() end)",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Scope:len(): number",
                doc: "The number of cleanups the bag holds.",
                example: "local scope = Scope.new()\nscope:add(function() end)\nprint(scope:len())",
            },
            Member {
                name: "Destroy",
                kind: MemberKind::Method,
                signature: "Scope:Destroy()",
                doc: "The same function as `clean`, under the name `delete` calls. `delete scope` is the form to write.",
                example: "local scope = Scope.new()\nscope:add(function() end)\ndelete scope",
            },
        ],
    ),
    (
        "Iter",
        &[
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Iter.from<T>(source: T[] | (() -> T?) | Set<T> | Queue<T> | Heap<T> | HashMap<any, T>): Iter<T>",
                doc: "A lazy iterator over an array, a function that returns the next value or nil, or a std collection. It keeps the element type, so `Iter.from(number[])` collects back to `number[]`.",
                example: "local xs = Iter.from([ 1, 2, 3 ]):collect()\nprint(xs:len())",
            },
            Member {
                name: "range",
                kind: MemberKind::Static,
                signature: "Iter.range(from: number, to: number, step: number?): Iter<number>",
                doc: "The numbers from `from` to `to`, both inclusive, by `step`. `step` defaults to 1.",
                example: "for i in Iter.range(1, 10, 2) do\n    print(i)\nend",
            },
            Member {
                name: "next",
                kind: MemberKind::Method,
                signature: "Iter:next(): T?",
                doc: "Pulls one value, or nil at the end. Every other reader is built on it.",
                example: "local it = Iter.from([ 1, 2 ])\nprint(it:next())",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "Iter:map<U>(f: (T) -> U): Iter<U>",
                doc: "Each value as `f(value)`, still lazily. As with `Array`, the third `map` in one chain types as `any`.",
                example: "local names = Iter.from([ 1, 2 ]):map(function(n) return `#{n}` end)\nprint(names:collect():join(\", \"))",
            },
            Member {
                name: "filter",
                kind: MemberKind::Method,
                signature: "Iter:filter(f: (T) -> boolean): Iter<T>",
                doc: "The values where `f(value)` holds. Nothing runs until something pulls.",
                example: "local big = Iter.from([ 1, 5 ]):filter(function(n) return n > 2 end)\nprint(big:count())",
            },
            Member {
                name: "take",
                kind: MemberKind::Method,
                signature: "Iter:take(n: number): Iter<T>",
                doc: "The first `n` values, then the end. The source is never pulled past them.",
                example: "print(Iter.range(1, 100):take(3):count())",
            },
            Member {
                name: "skip",
                kind: MemberKind::Method,
                signature: "Iter:skip(n: number): Iter<T>",
                doc: "Everything after the first `n` values.",
                example: "print(Iter.range(1, 5):skip(3):count())",
            },
            Member {
                name: "take_while",
                kind: MemberKind::Method,
                signature: "Iter:take_while(f: (T) -> boolean): Iter<T>",
                doc: "Values until `f(value)` first fails; that value and the rest are dropped.",
                example: "local it = Iter.range(1, 10):take_while(function(n) return n < 4 end)\nprint(it:count())",
            },
            Member {
                name: "chain",
                kind: MemberKind::Method,
                signature: "Iter:chain(other: Iter<T>): Iter<T>",
                doc: "This iterator, then `other`, as one run.",
                example: "local both = Iter.from([ 1 ]):chain(Iter.from([ 2 ]))\nprint(both:count())",
            },
            Member {
                name: "collect",
                kind: MemberKind::Method,
                signature: "Iter:collect(): T[]",
                doc: "Pulls every value into an array. This is what ends a chain.",
                example: "local xs = Iter.range(1, 3):collect()\nprint(xs:join(\", \"))",
            },
            Member {
                name: "for_each",
                kind: MemberKind::Method,
                signature: "Iter:for_each(f: (T) -> ())",
                doc: "Pulls every value and calls `f` on it.",
                example: "Iter.from([ 1, 2 ]):for_each(function(n) print(n) end)",
            },
            Member {
                name: "count",
                kind: MemberKind::Method,
                signature: "Iter:count(): number",
                doc: "Pulls every value and returns how many there were. It consumes the iterator.",
                example: "print(Iter.range(1, 5):count())",
            },
            Member {
                name: "any",
                kind: MemberKind::Method,
                signature: "Iter:any(f: (T) -> boolean): boolean",
                doc: "Whether `f` holds for some value. It stops at the first hit.",
                example: "print(Iter.from([ 1, 5 ]):any(function(n) return n > 3 end))",
            },
            Member {
                name: "all",
                kind: MemberKind::Method,
                signature: "Iter:all(f: (T) -> boolean): boolean",
                doc: "Whether `f` holds for every value. It stops at the first miss.",
                example: "print(Iter.from([ 1, 5 ]):all(function(n) return n > 0 end))",
            },
            Member {
                name: "find",
                kind: MemberKind::Method,
                signature: "Iter:find(f: (T) -> boolean): T?",
                doc: "The first value where `f` holds, or nil. It stops there, so the rest is never pulled.",
                example: "print(Iter.range(1, 10):find(function(n) return n % 4 == 0 end))",
            },
            Member {
                name: "reduce",
                kind: MemberKind::Method,
                signature: "Iter:reduce<U>(f: (U, T) -> U, init: U): U",
                doc: "Folds the run: `acc = f(acc, value)` from `init`, and the last `acc` is the result.",
                example: "local sum = Iter.range(1, 4):reduce(function(acc: number, n) return acc + n end, 0)\nprint(sum)",
            },
            Member {
                name: "first",
                kind: MemberKind::Method,
                signature: "Iter:first(): T?",
                doc: "The first value, or nil for an empty run. It pulls once.",
                example: "print(Iter.from([ 1, 2 ]):first())",
            },
            Member {
                name: "last",
                kind: MemberKind::Method,
                signature: "Iter:last(): T?",
                doc: "The last value, or nil for an empty run. It pulls the whole run.",
                example: "print(Iter.from([ 1, 2 ]):last())",
            },
        ],
    ),
    (
        "Symbol",
        &[Member {
            name: "new",
            kind: MemberKind::Static,
            signature: "Symbol.new(name: string)",
            doc: "A unique frozen table that no string can collide with. It prints as `Symbol(name)`, and `name` is a label alone: two symbols of one name are still two keys.",
            example: "local key = Symbol.new(\"slot\")\nlocal t = { [key] = 1 }\nprint(t[key])",
        }],
    ),
    (
        "Signal",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Signal.new<T...>(): Signal<T...>",
                doc: "A signal that fires `T...`. The pack takes no explicit type arguments at the call, so write them with `<<...>>` or on the binding.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:Fire(10)",
            },
            Member {
                name: "is",
                kind: MemberKind::Static,
                signature: "Signal.is(value: any): boolean",
                doc: "Whether `value` is one of the std's signals. An `RBXScriptSignal` is not.",
                example: "local damaged = Signal.new<<number>>()\nprint(Signal.is(damaged))",
            },
            Member {
                name: "wrap",
                kind: MemberKind::Static,
                signature: "Signal.wrap<T...>(source: Signalish<T...>): Signal<T...>",
                doc: "A std Signal that fires whenever `source` does. `source` is anything with `Connect` or `connect`, a Roblox signal included.",
                example: "local source = Signal.new<<number>>()\nlocal mine = Signal.wrap(source)\nprint(Signal.is(mine))",
            },
            Member {
                name: "collect",
                kind: MemberKind::Static,
                signature: "Signal.collect<T...>(source: Signalish<T...>): ((...any) -> T..., SignalConnection)",
                doc: "An iterator that drains the queued events of `source` in order, and the connection that feeds it. Disconnect it when the loop ends.",
                example: "local changed = Signal.new<<number>>()\nlocal events, conn = Signal.collect(changed)\nchanged:Fire(1)\nprint(events(), conn.Connected)",
            },
            Member {
                name: "Connect",
                kind: MemberKind::Method,
                signature: "Signal:Connect(handler: (T...) -> ()): SignalConnection",
                doc: "Runs `handler` on every fire and returns the connection. Handlers run in connection order, each on a reused thread.",
                example: "local damaged = Signal.new<<number>>()\nlocal conn = damaged:Connect(function(amount) print(amount) end)\nconn:Disconnect()",
            },
            Member {
                name: "Once",
                kind: MemberKind::Method,
                signature: "Signal:Once(handler: (T...) -> ()): SignalConnection",
                doc: "Runs `handler` on the next fire alone, then disconnects itself.",
                example: "local ready = Signal.new<<>>()\nready:Once(function() print(\"go\") end)",
            },
            Member {
                name: "Wait",
                kind: MemberKind::Method,
                signature: "Signal:Wait(timeout: number?): T...",
                doc: "Yields until the next fire and returns its values. With a timeout it returns nothing when the time runs out.",
                example: "local damaged = Signal.new<<number>>()\nlocal amount = damaged:Wait(5)\nprint(amount)",
            },
            Member {
                name: "Fire",
                kind: MemberKind::Method,
                signature: "Signal:Fire(...: T...)",
                doc: "Runs every handler now, in connection order. A handler may disconnect any connection during the fire.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:Fire(10)",
            },
            Member {
                name: "FireDeferred",
                kind: MemberKind::Method,
                signature: "Signal:FireDeferred(...: T...)",
                doc: "Runs the handlers at the next resumption point, through `task.defer`, so the caller returns first.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:FireDeferred(10)",
            },
            Member {
                name: "DisconnectAll",
                kind: MemberKind::Method,
                signature: "Signal:DisconnectAll()",
                doc: "Drops every connection. The signal stays usable.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:DisconnectAll()",
            },
            Member {
                name: "Destroy",
                kind: MemberKind::Method,
                signature: "Signal:Destroy()",
                doc: "Disconnects everything and marks the signal dead, which is what `delete` calls.",
                example: "local damaged = Signal.new<<number>>()\ndelete damaged",
            },
            Member {
                name: "connect",
                kind: MemberKind::Method,
                signature: "Signal:connect(handler: (T...) -> ()): SignalConnection",
                doc: "The snake_case twin of `Connect`. Both name one function, for a file that keeps to Alloy's own naming.",
                example: "local damaged = Signal.new<<number>>()\nlocal conn = damaged:connect(function(amount) print(amount) end)\nconn:disconnect()",
            },
            Member {
                name: "once",
                kind: MemberKind::Method,
                signature: "Signal:once(handler: (T...) -> ()): SignalConnection",
                doc: "The snake_case twin of `Once`.",
                example: "local ready = Signal.new<<>>()\nready:once(function() print(\"go\") end)",
            },
            Member {
                name: "wait",
                kind: MemberKind::Method,
                signature: "Signal:wait(timeout: number?): T...",
                doc: "The snake_case twin of `Wait`.",
                example: "local damaged = Signal.new<<number>>()\nprint(damaged:wait(5))",
            },
            Member {
                name: "fire",
                kind: MemberKind::Method,
                signature: "Signal:fire(...: T...)",
                doc: "The snake_case twin of `Fire`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:fire(10)",
            },
            Member {
                name: "fire_deferred",
                kind: MemberKind::Method,
                signature: "Signal:fire_deferred(...: T...)",
                doc: "The snake_case twin of `FireDeferred`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:fire_deferred(10)",
            },
            Member {
                name: "disconnect_all",
                kind: MemberKind::Method,
                signature: "Signal:disconnect_all()",
                doc: "The snake_case twin of `DisconnectAll`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:disconnect_all()",
            },
            Member {
                name: "destroy",
                kind: MemberKind::Method,
                signature: "Signal:destroy()",
                doc: "The snake_case twin of `Destroy`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:destroy()",
            },
        ],
    ),
    (
        "Attributes",
        &[
            Member {
                name: "get",
                kind: MemberKind::Static,
                signature: "Attributes.get<T>(target: any, attr: Attribute<T>): T?",
                doc: "The value an attribute carries on a function, a struct, or an enum, or nil when it carries none. `attr` is the declared name, not a string.",
                example: "attribute icon(asset: string) on struct\n@icon(\"rbxassetid://1\")\nstruct Sword as damage: number end\nprint(Attributes.get(Sword, icon))",
            },
            Member {
                name: "field",
                kind: MemberKind::Static,
                signature: "Attributes.field<T>(struct: any, field: string, attr: Attribute<T>): T?",
                doc: "The attribute's value on one field of a struct, by the field's name.",
                example: "attribute label(text: string) on field\nstruct Sword as\n    @label(\"Damage\") damage: number\nend\nprint(Attributes.field(Sword, \"damage\", label))",
            },
            Member {
                name: "variant",
                kind: MemberKind::Static,
                signature: "Attributes.variant<T>(enum: any, variant: string, attr: Attribute<T>): T?",
                doc: "The attribute's value on one variant of an enum, by the variant's name.",
                example: "attribute weight(n: number) on variant\nenum Drop as\n    @weight(3) Coin\nend\nprint(Attributes.variant(Drop, \"Coin\", weight))",
            },
            Member {
                name: "fields",
                kind: MemberKind::Static,
                signature: "Attributes.fields<T>(struct: any, attr: Attribute<T>): { [string]: T }",
                doc: "Every field of a struct that carries the attribute, as a table of field name to value.",
                example: "attribute label(text: string) on field\nstruct Sword as\n    @label(\"Damage\") damage: number\nend\nprint(Attributes.fields(Sword, label).damage)",
            },
            Member {
                name: "of",
                kind: MemberKind::Static,
                signature: "Attributes.of(target: any): any",
                doc: "Everything declared on the target, as the compiler wrote it. Use it to walk attributes no name is known for.",
                example: "attribute icon(asset: string) on struct\n@icon(\"rbxassetid://1\")\nstruct Sword as damage: number end\nprint(Attributes.of(Sword) ~= nil)",
            },
        ],
    ),
    (
        "Traits",
        &[
            Member {
                name: "Display",
                kind: MemberKind::Field,
                signature: "Display = { to_string(self): string }",
                doc: "How a value prints. An `impl Display` writes `to_string` and the emit sets `__tostring`, so `print(v)` and an interpolation use it.",
                example: "function show<T: Display>(v: T): string\n    return v:to_string()\nend",
            },
            Member {
                name: "Debug",
                kind: MemberKind::Field,
                signature: "Debug = { debug(self): string }",
                doc: "The developer view of a value, what `@derive(Debug)` writes and `$dbg` prints.",
                example: "function trace<T: Debug>(v: T): string\n    return v:debug()\nend",
            },
            Member {
                name: "Clone",
                kind: MemberKind::Field,
                signature: "Clone<T> = { clone(self): T }",
                doc: "A copy of a value, what `@derive(Clone)` writes.",
                example: "function twice<T: Clone>(v: T)\n    return v:clone()\nend",
            },
            Member {
                name: "Eq",
                kind: MemberKind::Field,
                signature: "Eq = { eq(self, other): boolean }",
                doc: "Equality. An `impl Eq` sets `__eq`, so `a == b` runs it.",
                example: "function same<T: Eq>(a: T, b: T): boolean\n    return a:eq(b)\nend",
            },
            Member {
                name: "PartialEq",
                kind: MemberKind::Field,
                signature: "PartialEq = { eq(self, other): boolean }",
                doc: "The same shape as `Eq`, under the name Rust uses for equality that is not total.",
                example: "function same<T: PartialEq>(a: T, b: T): boolean\n    return a:eq(b)\nend",
            },
            Member {
                name: "Ord",
                kind: MemberKind::Field,
                signature: "Ord = { lt(self, other): boolean, le(self, other): boolean }",
                doc: "Ordering. An `impl Ord` sets `__lt` and `__le`, so `a < b` runs it, and `@derive(Ord)` writes both.",
                example: "function smallest<T: Ord>(a: T, b: T): T\n    return a:lt(b) ? a : b\nend",
            },
            Member {
                name: "Add",
                kind: MemberKind::Field,
                signature: "Add<T> = { add(self, other): T }",
                doc: "Addition. An `impl Add` sets `__add`, so `a + b` runs it.",
                example: "function sum<T: Add>(a: T, b: T)\n    return a:add(b)\nend",
            },
            Member {
                name: "Sub",
                kind: MemberKind::Field,
                signature: "Sub<T> = { sub(self, other): T }",
                doc: "Subtraction, through `__sub`.",
                example: "function diff<T: Sub>(a: T, b: T)\n    return a:sub(b)\nend",
            },
            Member {
                name: "Mul",
                kind: MemberKind::Field,
                signature: "Mul<T> = { mul(self, other): T }",
                doc: "Multiplication, through `__mul`.",
                example: "function scale<T: Mul>(a: T, b: T)\n    return a:mul(b)\nend",
            },
            Member {
                name: "Div",
                kind: MemberKind::Field,
                signature: "Div<T> = { div(self, other): T }",
                doc: "Division, through `__div`.",
                example: "function ratio<T: Div>(a: T, b: T)\n    return a:div(b)\nend",
            },
            Member {
                name: "Serialize",
                kind: MemberKind::Field,
                signature: "Serialize = { serialize(self): any }",
                doc: "A value as plain data. The bound asks for `serialize`, which `@derive(Serialize)` writes beside `to_table` and `from_table`, so a derived struct meets the bound.",
                example: "function to_data<T: Serialize>(v: T): any\n    return v:serialize()\nend",
            },
        ],
    ),
];

/// The signature of each std type, as Alloy writes it. A reference page
/// heads the type's section with it.
pub const TYPE_SIGNATURES: &[(&str, &str)] = &[
    ("HashMap", "HashMap<K, V>"),
    ("Set", "Set<T>"),
    ("Array", "Array<T>"),
    ("ReadArray", "ReadArray<T>"),
    ("WriteArray", "WriteArray<T>"),
    ("Future", "Future<T>"),
    ("Result", "Result<T, E>"),
    ("Ok", "Ok<T>(value: T): Result<T, any>"),
    ("Err", "Err<E>(error: E, trace: string?): Result<any, E>"),
    ("Queue", "Queue<T>"),
    ("Heap", "Heap<T>"),
    ("Scope", "Scope"),
    ("Iter", "Iter<T>"),
    ("Symbol", "Symbol"),
    ("Signal", "Signal<T...>"),
    ("Partial", "Partial<T>"),
    ("Readonly", "Readonly<T>"),
    ("Sink", "Sink<T>"),
    ("Attributes", "Attributes"),
];

/// The signature of one std type, or nothing when the entry names no
/// type of its own.
pub fn type_signature(key: &str) -> Option<&'static str> {
    TYPE_SIGNATURES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, s)| *s)
}

/// The members of one entry, or nothing when the entry has none.
pub fn members(key: &str) -> &'static [Member] {
    MEMBERS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, m)| *m)
        .unwrap_or(&[])
}

/// One member of one entry, by name.
pub fn member(key: &str, name: &str) -> Option<&'static Member> {
    members(key).iter().find(|m| m.name == name)
}

/// The std type of that name, when it documents members.
pub fn member_owner(name: &str) -> Option<&'static str> {
    MEMBERS.iter().find(|(k, _)| *k == name).map(|(k, _)| *k)
}

/// `HashMap.get` or `HashMap:get` split into the type and the member.
/// None when the text names no member of a std type.
pub fn split_member(topic: &str) -> Option<(&'static str, &'static Member)> {
    let (owner, name) = topic.split_once(['.', ':'])?;
    let (key, _) = MEMBERS.iter().find(|(k, _)| *k == owner)?;

    member(key, name).map(|m| (*key, m))
}

/// The names of an entry's members, in the order they are documented.
pub fn member_names(key: &str) -> Vec<&'static str> {
    members(key).iter().map(|m| m.name).collect()
}

/// One member as Markdown: the signature, the doc, and the example.
/// This is what a hover on the member shows.
pub fn member_markdown(m: &Member) -> String {
    format!(
        "```alloy\n{}\n```\n{}\n\n```alloy\n{}\n```",
        m.signature, m.doc, m.example
    )
}

/// The member spot at `offset`: the word under it, the byte of the `.`
/// or the `:` before it, and the receiver word before that. None when
/// the byte is not a member read.
pub fn member_spot(source: &str, offset: usize) -> Option<(&str, usize, &str)> {
    let bytes = source.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    if !bytes.get(offset).is_some_and(|b| word(*b)) {
        return None;
    }

    let mut start = offset;
    let mut end = offset;

    while start > 0 && word(bytes[start - 1]) {
        start -= 1;
    }

    while end < bytes.len() && word(bytes[end]) {
        end += 1;
    }

    if start == 0 || !matches!(bytes[start - 1], b'.' | b':') {
        return None;
    }

    let sigil = start - 1;
    let head = &source[..sigil];
    let from = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);

    Some((&source[start..end], sigil, &head[from..]))
}

/// The std type an annotation names: `HashMap<string, number>` is a
/// HashMap, `read number[]` a ReadArray, `Job[]` an Array.
pub fn type_head(annotation: &str) -> Option<String> {
    let t = annotation.trim().trim_end_matches('?').trim();

    if let Some(rest) = t.strip_prefix("read ") {
        return rest.trim().ends_with("[]").then(|| "ReadArray".to_string());
    }

    if let Some(rest) = t.strip_prefix("write ") {
        return rest
            .trim()
            .ends_with("[]")
            .then(|| "WriteArray".to_string());
    }

    if t.ends_with("[]") {
        return Some("Array".to_string());
    }

    let name = t.split('<').next().unwrap_or(t).trim();

    (!name.is_empty()).then(|| name.to_string())
}

/// The std type an initializer starts from: `HashMap.new()` is a
/// HashMap, `[ 1, 2 ]` an Array, `$set[ ]` a Set, `$map[ ]` a HashMap.
pub fn value_head(init: &str) -> Option<String> {
    let v = init.trim();

    if v.starts_with('[') {
        return Some("Array".to_string());
    }

    if v.starts_with("$set[") {
        return Some("Set".to_string());
    }

    if v.starts_with("$map[") {
        return Some("HashMap".to_string());
    }

    let name = v.split(['.', ':', '(']).next()?.trim();

    (!name.is_empty()).then(|| name.to_string())
}

/// Whether a member of that kind hangs off the type itself. A static
/// does; a method hangs off a value; a plain member reads on either.
pub fn member_fits(kind: MemberKind, on_type: bool) -> bool {
    match kind {
        MemberKind::Static => on_type,
        MemberKind::Method => !on_type,
        _ => true,
    }
}

/// A member as a hover shows it under the type the checker printed:
/// the name, what it does, and an example. The signature is the line
/// the checker prints above it.
pub fn member_hover(owner: &str, m: &Member) -> String {
    let sigil = match m.kind {
        MemberKind::Method => ":",
        _ => ".",
    };

    format!(
        "**{owner}{sigil}{}**\n\n{}\n\n```alloy\n{}\n```",
        m.name, m.doc, m.example
    )
}

/// A std type's overview with a line that names its members, which is
/// what a hover on the type name shows.
pub fn type_markdown(key: &str) -> Option<String> {
    let text = lookup(key)?;
    let names = member_names(key);

    if names.is_empty() {
        return Some(text.to_string());
    }

    let list: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();

    Some(format!("{text}\n\nMembers: {}", list.join(", ")))
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
        key: Some("topic:markup"),
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
        id: "flux",
        title: "alloy flux",
        key: Some("topic:flux"),
    },
    Section {
        number: "5.5",
        id: "fmt",
        title: "alloy fmt",
        key: Some("topic:fmt"),
    },
    Section {
        number: "5.6",
        id: "test",
        title: "alloy test",
        key: Some("topic:test"),
    },
    Section {
        number: "5.7",
        id: "doc",
        title: "alloy doc",
        key: None,
    },
    Section {
        number: "5.8",
        id: "config",
        title: "alloy.toml",
        key: Some("topic:config"),
    },
    Section {
        number: "5.9",
        id: "luaurc",
        title: ".luaurc and .config.luau",
        key: Some("topic:luaurc"),
    },
    Section {
        number: "5.10",
        id: "mount",
        title: "Project files and mounts",
        key: Some("topic:mount"),
    },
    Section {
        number: "5.11",
        id: "data",
        title: "JSON and TOML data",
        key: Some("topic:data"),
    },
    Section {
        number: "5.12",
        id: "ingots",
        title: "Ingots",
        key: Some("topic:ingots"),
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

/// The kind of a compiler diagnostic, from its text: the word before
/// the colon in `ReservedWord: ...`, the way the checker names its own.
pub fn kind_for(message: &str) -> &'static str {
    let m = message.to_ascii_lowercase();
    let rules: &[(&[&str], &str)] = &[
        (&["internal:"], "InternalError"),
        (&["needs `as` before its body"], "SyntaxError"),
        (&["markup:"], "MarkupError"),
        (&["names no module"], "UnknownModule"),
        (&["ingot `"], "IngotError"),
        (&["reserved word"], "ReservedWord"),
        (&["in macro expansion"], "MacroError"),
        (&["not exhaustive", "no arm for"], "ExhaustiveMatch"),
        (&["remote"], "WireType"),
        (&["directive"], "DirectiveError"),
        (&["result"], "ResultError"),
        (&["a `const`"], "ConstError"),
        (&["@test", "test "], "TestError"),
        (&["@cfg"], "AttributeError"),
        (&["macro"], "MacroError"),
        (&["`new ", "constructor", "construct"], "ConstructorError"),
        (&["attribute", "derive"], "AttributeError"),
        (&["data file"], "DataError"),
        (&["import", "export", "require", "module"], "ImportError"),
        (
            &["does not write", "parameters in", "trait"],
            "TraitContract",
        ),
        (&["field", "sealed", "struct"], "StructError"),
        (&["variant", "enum"], "EnumError"),
        (
            &["expected", "unexpected", "unterminated", "needs a", "found"],
            "SyntaxError",
        ),
    ];

    rules
        .iter()
        .find(|(words, _)| words.iter().any(|w| m.contains(w)))
        .map(|(_, kind)| *kind)
        .unwrap_or("AlloyError")
}

/// A compiler diagnostic as the editor and the CLI show it: its kind,
/// a colon, its text.
pub fn labeled(message: &str) -> String {
    // `markup:` names the kind, and the kind prints in front of the
    // text; printing both says it twice.
    let text = message.strip_prefix("markup: ").unwrap_or(message);

    format!("{}: {text}", kind_for(message))
}

/// The book section a compiler diagnostic belongs to, from its text.
/// The diagnostics name what they are about; the first match wins, from
/// the most specific wording to the least.
pub fn code_for(message: &str) -> Option<&'static str> {
    let m = message.to_ascii_lowercase();
    let rules: &[(&[&str], &str)] = &[
        (&["names no module"], "3.2"),
        (&["not exhaustive"], "4.2"),
        (&["remote"], "4.3"),
        (&["directive"], "4.4"),
        // `try` and `Result` are one contract; 3.3 is Futures.
        (&["result"], "4.1"),
        (&["a `const`"], "6.1"),
        (&["reserved word"], "6.1"),
        (&["markup"], "3.13"),
        (&["@test", "test "], "3.14"),
        (&["@cfg"], "3.11"),
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
        (&["data file", ".json", ".toml"], "5.11"),
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
        // A syntax error is about the grammar, and the keyword page is
        // where the grammar lives. Last, so a kinded message wins.
        (
            &[
                "expected",
                "unexpected",
                "unterminated",
                "needs an",
                "needs a",
            ],
            "6.1",
        ),
    ];

    rules
        .iter()
        .find(|(words, _)| words.iter().any(|w| m.contains(w)))
        .map(|(_, code)| *code)
}

#[cfg(test)]
mod tests {
    /// The `Future` entry documents every member the std declares. The
    /// two drift apart the moment the std grows a method, and the doc
    /// is the only place a reader looks.
    #[test]
    fn the_future_topic_names_every_member_of_the_std() {
        let documented = super::member_names("Future");
        let mut names: Vec<&str> = Vec::new();

        // The members of the type: `read name: (...) -> ...`.
        let at = crate::RUNTIME
            .find("export type Future<T> = {")
            .expect("Future type");
        let body = &crate::RUNTIME[at..];
        let end = body.find("\n}").expect("end of the type");

        for line in body[..end].lines() {
            if let Some(rest) = line.trim().strip_prefix("read ")
                && let Some(name) = rest.split(':').next()
                && !name.starts_with("__")
            {
                names.push(name);
            }
        }

        // The statics: `function Future.name<T>(...)`.
        for line in crate::RUNTIME.lines() {
            if let Some(rest) = line.strip_prefix("function Future.")
                && let Some(name) = rest.split(['<', '(']).next()
                && !name.starts_with("__")
            {
                names.push(name);
            }
        }

        for name in names {
            assert!(
                documented.contains(&name),
                "the std has `Future.{name}`; the members do not"
            );
        }
    }

    /// Every member's example is Alloy the compiler accepts. A doc
    /// example that does not compile is worse than none: a reader
    /// copies it.
    #[test]
    fn every_member_example_compiles() {
        for (owner, members) in super::MEMBERS {
            for m in *members {
                let out = crate::compile(m.example)
                    .unwrap_or_else(|e| panic!("{owner}.{}: {}", m.name, e.located(m.example)));

                assert!(
                    out.diagnostics.is_empty(),
                    "{owner}.{}: {}",
                    m.name,
                    out.diagnostics
                        .iter()
                        .map(|d| d.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
        }
    }

    /// A member's signature opens with its own name: a static and a
    /// method carry the owner and the sigil the call takes, and a plain
    /// member may name itself alone.
    #[test]
    fn every_signature_names_its_owner_and_member() {
        for (owner, members) in super::MEMBERS {
            for m in *members {
                let heads = match m.kind {
                    super::MemberKind::Static => vec![format!("{owner}.{}", m.name)],
                    super::MemberKind::Method => vec![format!("{owner}:{}", m.name)],
                    _ => vec![format!("{owner}.{}", m.name), m.name.to_string()],
                };

                assert!(
                    heads.iter().any(|h| m.signature.starts_with(h)),
                    "{owner}.{} signs as `{}`, which opens with none of {heads:?}",
                    m.name,
                    m.signature
                );
                assert!(!m.doc.is_empty(), "{owner}.{} has no doc", m.name);
                assert!(!m.example.is_empty(), "{owner}.{} has no example", m.name);
            }
        }
    }

    /// Every entry with members is a Std entry, and none of the Std
    /// entries keeps a pipe table: a member is a section now.
    #[test]
    fn no_std_entry_holds_a_table() {
        for (key, _) in super::MEMBERS {
            let text = super::lookup(key).unwrap_or_else(|| panic!("no entry for `{key}`"));

            assert!(!text.contains("|---|"), "`{key}` still holds a table");
        }

        // `Traits` groups the shapes a bound names; it is the one std
        // entry that is not a type of its own.
        for (key, _) in super::TYPE_SIGNATURES {
            assert!(super::lookup(key).is_some(), "no entry for `{key}`");
        }

        for (key, _) in super::MEMBERS {
            assert!(
                *key == "Traits" || super::type_signature(key).is_some(),
                "`{key}` documents members and no signature"
            );
        }
    }

    #[test]
    fn a_dotted_topic_names_one_member() {
        assert_eq!(
            super::split_member("HashMap:get").map(|(k, m)| (k, m.name)),
            Some(("HashMap", "get"))
        );
        assert_eq!(
            super::split_member("HashMap.get").map(|(k, m)| (k, m.name)),
            Some(("HashMap", "get"))
        );
        assert_eq!(
            super::split_member("Signal.new").map(|(k, m)| (k, m.name)),
            Some(("Signal", "new"))
        );
        assert!(super::split_member("HashMap.nope").is_none());
        assert!(super::split_member("Nope.get").is_none());
        assert!(super::split_member("HashMap").is_none());
    }
}
