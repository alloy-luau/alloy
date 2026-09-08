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
        "```alloy\nimpl Name ... end\nimpl Trait for Name ... end\n```\nMethods for a struct, an enum, or a foreign type such as `Vector3` or `string`. A foreign impl needs `export` and works project wide.\n\nEmits functions on the class table.",
    ),
    (
        "trait",
        "```alloy\ntrait Name\n    function m(self): T\nend\n```\nA behavior contract: method signatures, with a body as a default. `impl Trait for Name` implements it and `<T: Trait>` bounds a generic; `<T: A & B>` asks for both.\n\nAn `impl` of an operator trait writes the metamethod: `Add` (`add`, `__add`), `Sub`, `Mul`, `Div`, `Eq` (`eq`, `__eq`), `Lt` and `Le` (`__lt`, `__le`), `Display` (`to_string`, `__tostring`), `Call` (`call`, `__call`), `Len`, `Concat`, and `Drop` (`drop`, which `delete` runs as `Destroy`). A bound names a shape the std exports, `Display`, `Debug`, `Clone`, `Eq`, `PartialEq`, `Ord`, `Add`, `Sub`, `Mul`, `Div`, `Serialize`; a file's own trait of the same name wins. `alloy doc Traits` lists them.\n\nEmits a type with the method signatures.",
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
        "```alloy\nlocal x: T = expr\nlocal { name, hp = health } = player\nlocal [first, ...rest] = xs\nlocal Ok(v) = result else\n    return\nend\n```\nLuau's binding, with three forms of its own. A table pattern takes fields by name, `= alias` renames one. An array pattern takes items by position, and `...rest` takes the tail as an array. A variant or struct pattern binds its payload, and the `else` block runs when the pattern fails: it must leave, with `return`, `break`, `continue`, or an error, so the names hold after it. `const` takes every form too.",
    ),
    (
        "const",
        "```alloy\nconst x = expr\n```\nA binding that cannot be reassigned; the value stays mutable. Luau has `const` of its own, so the keyword passes through and a reassignment is a compile error there too.",
    ),
    (
        "async",
        "```alloy\nasync function f() ... end\nasync do ... end\n```\nReturns a Future. The body runs on `task.spawn` under `xpcall`, and the Future memoizes the result. `async function f(): T` is `Future<T>`; without a return type, a body that returns a value infers `T`, and one that returns nothing is `Future<()>`.",
    ),
    (
        "await",
        "```alloy\nawait expr\n```\nYields until the Future settles, then returns its value or rethrows its error. Accepts a Future or any value with `andThen`.\n\n`try await f()` turns a rejection into an Err. When `f` is an async function declared to return a `Result`, the Result it settles with is the value, not an Ok around it.",
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
        "```alloy\nstruct Counter as\n    read name: string\n    private count: number = 0\nend\n\nimpl Counter\n    function bump(self): number\n        self.count += 1\n        return self.count\n    end\n\n    private function reset(self)\n        self.count = 0\n    end\nend\n```\nA field or an `impl` method that only the struct's own methods reach. The word compiles to nothing at runtime: the check artifact keeps the private members out of the struct's public type, so `c.count` and `c:reset()` in other code are type errors in the editor and under `alloy flux`, and the `private_access` lint reports them in the same file. A private field may still be set in `new Counter { }`. A struct with type parameters keeps one view. `public` is the default and needs no word.",
    ),
    (
        "public",
        "```alloy\nimpl Counter\n    public function peek(self): number\n        return self.count\n    end\nend\n```\nThe default visibility, written out for symmetry with `private`. A public member is part of the struct's type in every file. Both words are reserved.",
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
        "```alloy\n@derive(Serialize)\n```\nGenerates `to_table` and `from_table`, the plain-table forms for storage and remotes.",
    ),
    // Attributes
    (
        "@derive",
        "```alloy\n@derive(Eq, Debug, Clone)\n```\nGenerates methods from the field list: `Eq` or `PartialEq` is `__eq`, `Ord` is `__lt` and `__le` over the fields in order, `Debug` is `debug` and `__tostring`, `Clone` is `clone`, `Serialize` is `to_table` and `from_table`. On an enum, `Eq`, `PartialEq`, and `Clone` derive; `Debug` is every enum's own.",
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
        "```alloy\nlocal prices = $map[[\"sword\", 10], [\"pet\", 25]]\nlocal m: HashMap<string, number> = HashMap.new()\nm:set(\"gem\", 5)\nfor key, value in m:entries() do end\n```\nA map with methods, from the std. Any value that is not nil is a key. `HashMap.new()` is empty, `HashMap.from(t)` copies a table's pairs, and `$map[[k, v], ...]` is the literal.\n\n| | |\n|---|---|\n| `get(key)` | The value under `key`, or nil. |\n| `set(key, value)` | Stores the value; returns the map, so calls chain. |\n| `has(key)` | Whether the key is present. |\n| `remove(key)` | Removes the key and returns its value, or nil. |\n| `len()` | The number of keys. |\n| `keys()` | The keys as an array, in no set order. |\n| `values()` | The values as an array, in the same order as `keys()`. |\n| `entries()` | An iterator of `key, value` pairs, for a `for` loop. |\n| `get_or_insert(key, default)` | The value under `key`, storing `default` first when there is none. |\n| `clear()` | Removes every key. |",
    ),
    (
        "Set",
        "```alloy\nlocal seen = $set[1, 2, 3]\nlocal s: Set<string> = Set.new()\nif s:add(\"a\"):has(\"a\") then end\n```\nA set with methods, from the std. Any value that is not nil is a member. `Set.new()` is empty, `Set.from(t)` takes an array's items, and `$set[a, b]` is the literal.\n\n| | |\n|---|---|\n| `add(value)` | Adds the value; returns the set, so calls chain. |\n| `has(value)` | Whether the value is a member. |\n| `remove(value)` | Removes it; true when it was there. |\n| `len()` | The number of members. |\n| `union(other)` | A new set of both. |\n| `intersection(other)` | A new set of the members in both. |\n| `difference(other)` | A new set of this one's members not in `other`. |\n| `to_array()` | The members as an array, in no set order. |",
    ),
    (
        "Array",
        "```alloy\nlocal xs = [ 1, 2, 3 ]\nlocal doubled = xs:map(function(x) return x * 2 end)\nlocal grid = [[1, 2], [3, 4]]\n```\nThe array type of the std: a Luau array with the methods below on its metatable. An array literal carries it, `T[]` and `Array<T>` name the same type, and a plain table becomes one with `Array.from(t)`, in place. `[[` opens a nested array; a long string keeps Luau's leveled form, `[=[ ... ]=]`. A `read T[]` is `ReadArray<T>`, the readers alone, and a `write T[]` is `WriteArray<T>`, `push` alone.\n\n**Statics**\n\n| | |\n|---|---|\n| `Array.new()` | An empty array. |\n| `Array.with_capacity(n)` | An empty array with room for `n` items. |\n| `Array.from(t)` | The table `t` as an array: it gains the metatable and stays the same table. |\n\n**Methods**\n\n| | |\n|---|---|\n| `len()` | The number of items. |\n| `is_empty()` | Whether there are none. |\n| `push(...values)` | Appends the values in order. |\n| `pop()` | Removes and returns the last item, or nil when empty. |\n| `first()`, `last()` | The first or the last item, or nil. |\n| `map(f)` | A new array of `f(item, index)` for each item. |\n| `filter(f)` | A new array of the items where `f(item, index)` holds. |\n| `find(f)` | The first item where `f(item, index)` holds, or nil. |\n| `find_index(f)` | Its index, or nil. |\n| `contains(value)` | Whether an item equals `value`. |\n| `index_of(value)` | The index of the first item that equals `value`, or nil. |\n| `for_each(f)` | Calls `f(item, index)` for each item. |\n| `reduce(f, init)` | Folds: `acc = f(acc, item, index)` from `init`, and returns the last `acc`. |\n| `slice(from, to?)` | A new array of the items from `from` to `to`, inclusive; `to` defaults to the end. |\n| `concat(other)` | A new array of this one followed by `other`. |\n| `reverse()` | A new array in the other order. |\n| `sort_by(less)` | Sorts in place with `less(a, b)`, and returns the same array. |\n| `join(sep?)` | The items as strings, joined by `sep`, a comma and a space by default. |\n\nLuau rejects an alias that names itself with other arguments, so `map` returns the same shape under a second name, and the third `map` in one chain is `any`. Annotate the accumulator of `reduce` when its body uses it: `xs:reduce(function(acc: number, x) return acc + x end, 0)`. `$set[ ]` and `$map[ ]` build a `Set` and a `HashMap` the way `[ ]` builds an array.",
    ),
    (
        "ReadArray",
        "```alloy\nlocal function total(xs: read number[]): number\n    return xs:reduce(function(acc, x) return acc + x end, 0)\nend\n```\nThe array a `read T[]` names: every reader of `Array<T>` and none of its writers. Methods: `len`, `is_empty`, `first`, `last`, `map`, `filter`, `find`, `find_index`, `contains`, `index_of`, `for_each`, `reduce`, `slice`, `concat`, `join`. An `Array<T>` passes where one is asked.",
    ),
    (
        "WriteArray",
        "```alloy\nlocal function log_to(sink: write string[])\n    sink:push(\"line\")\nend\n```\nThe array a `write T[]` names: `push` and the index, nothing that reads. An `Array<T>` passes where one is asked.",
    ),
    (
        "Future",
        "```alloy\nasync function load(id: number): Profile\n    return await fetch(id)\nend\nlocal profiles = await Future.all([load(1), load(2)])\nlocal first = await Future.race([load(1), Future.delay(5)])\n```\nA memoized task, from the std: it runs once, settles once, and every `await` after that reads the same value. An `async function` returns one and its body runs on `task.spawn` under `xpcall`; `await` yields until it settles, then returns the value or rethrows; `try await` gives a Result instead.\n\n**Statics**\n\n| | |\n|---|---|\n| `Future.resolve(value)` | A settled Future. |\n| `Future.reject(error)` | A failed one, `Future<never>`. |\n| `Future.delay(seconds)` | Settles after the wait, `Future<()>`. |\n| `Future.all(futures)` | An array of every value, in order; the first failure fails it. |\n| `Future.race(futures)` | The first to settle, value or failure. |\n| `Future.any(futures)` | The first to succeed; fails when all fail. |\n| `Future.all_settled(futures)` | An array of `Result`s, one per Future, none of which fails it. |\n\n**Methods**\n\n| | |\n|---|---|\n| `andThen(on_resolve?, on_reject?)` | Runs a callback when it settles, and returns the Future for chaining. |\n| `cancel()` | Closes the task; an `await` on it raises. |\n| `is_settled()` | Whether it has a value or a failure. |\n\nThe type is covariant: `race` and `any` over a list of `Future<number>` and `Future<string>` yield `Future<number | string>`. An `async function` without a return type and without a `return` value is `Future<()>`.",
    ),
    (
        "Result",
        "```alloy\nlocal r: Result<number, string> = Ok(1)\nlocal v = r:unwrap_or(0)\nmatch r with\n    case Ok(n) then print(n)\n    case Err(e) then warn(e)\nend\n```\n`Ok(value)` or `Err(error)`, from the std. A function that can fail returns one, and the caller reads it with a `match`, an `if local Ok(v) = r`, or the methods. `try expr` inside a function that returns a Result returns the `Err` early; `try do ... end` turns a throw into an Err; `try await` does the same for a Future.\n\n| | |\n|---|---|\n| `unwrap()` | The value; an Err raises, with its trace under the message. |\n| `expect(message)` | The value; an Err raises with `message`. |\n| `unwrap_or(default)` | The value, or `default`: the type is `T \\| D`, so `r:unwrap_or(nil)` is `T?`. |\n| `map(f)` | `Ok(f(value))`, or the same Err. |\n| `map_err(f)` | `Err(f(error))`, or the same Ok. |\n| `is_ok()`, `is_err()` | Which case it is. |\n| `ok()` | The value, or nil for an Err. |\n| `Result.pcall(f, ...)` | Calls `f`; `Ok` of what it returns, or `Err` of what it threw, with the traceback in `trace`. |\n\nAn `Err` carries `trace`, a traceback `Result.pcall` and `try` fill in and `Err(e, trace)` sets by hand. `map` and `map_err` yield a Result of the same surface whose own `map` is `any`: Luau rejects an alias that names itself with other arguments. `Result<T, E>` is covariant, so a `Result<Boost, E>` passes where `Result<any, E>` is asked.",
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
        "```alloy\nlocal jobs: Queue<Job> = Queue.new()\njobs:push(job)\nwhile local next = jobs:pop() do\n    run(next)\nend\n```\nA first-in, first-out queue over a ring of indices, from the std: `push` at the back costs the same at any size, and so does `pop` at the front. `Queue.from(t)` fills one from an array. A `for` loop reads it front to back without a pop.\n\n| | |\n|---|---|\n| `push(value)` | Appends at the back. |\n| `pop()` | Removes and returns the front item, or nil when empty. |\n| `peek()` | The front item without removing it, or nil. |\n| `len()` | The number of items. |\n| `is_empty()` | Whether there are none. |\n| `clear()` | Removes every item. |\n| `to_array()` | The items front to back, as an array. |",
    ),
    (
        "Heap",
        "```alloy\nlocal open = Heap.new(function(a, b) return a.cost < b.cost end)\nopen:push(node)\nlocal nearest = open:pop()\n```\nA binary heap, from the std: `pop` yields the least value under `less`, which defaults to `<`, so numbers and strings need no comparator and tables take one. `Heap.from(t, less)` builds one from an array. A `for` loop reads it least first without a pop; `to_array` returns the items sorted.\n\n| | |\n|---|---|\n| `push(value)` | Adds a value. |\n| `pop()` | Removes and returns the least value, or nil when empty. |\n| `peek()` | The least value without removing it, or nil. |\n| `len()` | The number of values. |\n| `is_empty()` | Whether there are none. |\n| `clear()` | Removes every value. |\n| `to_array()` | The values sorted under `less`, as an array. |",
    ),
    (
        "Scope",
        "```alloy\nlocal scope = Scope.new()\nscope:add(part.Touched:Connect(on_touch))\nscope:add(function() print(\"bye\") end)\ndelete scope\n```\nA cleanup bag, from the std. `add` takes anything `delete` accepts, an Instance, a connection, a thread, a table with `Destroy` or `Disconnect`, or a function, and gives it back, so `local conn = scope:add(signal:Connect(f))` reads as before. `clean` runs the cleanups newest first and empties the bag; `delete scope` does the same through `Destroy`. A scope may hold another scope.\n\n| | |\n|---|---|\n| `add(item)` | Adds a cleanup and returns `item`. |\n| `remove(item)` | Takes it out without running it; true when it was there. |\n| `clean()` | Runs every cleanup, newest first, and empties the bag. |\n| `extend()` | A child scope, added to this one, so cleaning the parent cleans it. |\n| `len()` | The number of cleanups held. |\n| `Destroy()` | The same as `clean`, which is what `delete` calls. |",
    ),
    (
        "Iter",
        "```alloy\nlocal names = Iter.from(players)\n    :filter(function(p) return p.Team == team end)\n    :map(function(p) return p.Name end)\n    :take(5)\n    :collect()\nfor i in Iter.range(1, 10, 2) do end\n```\nA lazy iterator, from the std: each step wraps the last, and nothing runs until `collect`, `for_each`, a reducer, or a `for` loop pulls. `Iter.from` takes an array, a function that returns the next value or nil, or anything with `__iter`; `Iter.range(from, to, step?)` counts, inclusive.\n\n| | |\n|---|---|\n| `next()` | The next value, or nil at the end. |\n| `map(f)` | Each value as `f(value)`. |\n| `filter(f)` | The values where `f(value)` holds. |\n| `take(n)` | The first `n` values. |\n| `skip(n)` | Everything after the first `n`. |\n| `take_while(f)` | Values until `f(value)` fails. |\n| `chain(other)` | This iterator, then `other`. |\n| `collect()` | The values as an array. |\n| `for_each(f)` | Calls `f(value)` for each. |\n| `count()` | The number of values. |\n| `any(f)`, `all(f)` | Whether `f` holds for some value, or for every value. |\n| `find(f)` | The first value where `f` holds, or nil. |\n| `reduce(f, init)` | Folds: `acc = f(acc, value)` from `init`. |\n| `first()`, `last()` | The first or the last value, or nil. |\n\nAs with `Array`, `map` returns the same shape under a second name, and the third `map` in one chain is `any`.",
    ),
    (
        "Symbol",
        "```alloy\nlocal key = Symbol.new(\"name\")\nlocal t = { [key] = 1 }\n```\nA unique key that no string can collide with, from the std: a frozen table that prints as `Symbol(name)`. Use one for a private table slot, or a sentinel a value cannot forge.",
    ),
    (
        "Signal",
        "```alloy\nlocal damaged = Signal.new<<Player, number>>()\nlocal conn = damaged:Connect(function(player, amount) end)\ndamaged:Fire(player, 10)\nlocal who, amount = damaged:Wait(5)\n```\nA typed signal, from the std, with the shape of `RBXScriptSignal`, so code that takes one takes the other. `Signal.new<T...>()` fires `T...`; handlers run in connection order, each on a reused thread, and a handler may disconnect any connection during a fire.\n\n| | |\n|---|---|\n| `Connect(handler)` | Runs `handler` on every fire; returns the connection. |\n| `Once(handler)` | Runs it on the next fire alone. |\n| `Wait(timeout?)` | Yields until the next fire and returns its values; with a timeout, returns nothing when the time runs out. |\n| `Fire(...)` | Runs every handler now. |\n| `FireDeferred(...)` | Runs them on the next resumption point, through `task.defer`. |\n| `DisconnectAll()` | Drops every connection. |\n| `Destroy()` | Disconnects all and marks the signal dead. |\n\nEach has a snake_case twin, `connect`, `once`, `wait`, `fire`, `fire_deferred`, `disconnect_all`, `destroy`. A connection has `Connected`, `Disconnect`, and `disconnect`. `Signal.is(value)` says whether a value is one.\n\n`Signal.collect(source)` turns any signal with `Connect` or `connect`, the shape `Signalish<T...>`, a Roblox one included, into an iterator that drains the queued events in order, plus the connection: `for id, value in Signal.collect(changed) do`. `Signal.wrap(source)` gives a Signal that fires with the source.",
    ),
    (
        "Traits",
        "```alloy\nfunction largest<T: Ord>(xs: T[]): T\nimpl Display for Vec2\n    function to_string(self): string\n        return `({self.x}, {self.y})`\n    end\nend\n```\nThe trait shapes the std exports, for a bound and for `impl X for Struct`. Each is a table type of the methods it asks for; a struct that has them passes.\n\n| | |\n|---|---|\n| `Display` | `to_string(self): string`; an impl sets `__tostring`. |\n| `Debug` | `debug(self): string`, what `@derive(Debug)` writes. |\n| `Clone` | `clone(self): T`, what `@derive(Clone)` writes. |\n| `Eq`, `PartialEq` | `eq(self, other): boolean`; an impl sets `__eq`. |\n| `Ord` | `lt(self, other)` and `le(self, other)`; `@derive(Ord)` writes `__lt` and `__le`. |\n| `Add`, `Sub`, `Mul`, `Div` | `add(self, other)` and the rest; an impl sets the metamethod. |\n| `Serialize` | `to_table(self)` and `from_table(t)`, what `@derive(Serialize)` writes. |\n\nAn `impl` may also name `Lt`, `Le`, `Call`, `Len`, `Concat`, and `Drop`, which write `__lt`, `__le`, `__call`, `__len`, `__concat`, and `Destroy`. A trait the file declares with one of these names wins over the std's.",
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
        "```alloy\nattribute icon(asset: string) on struct, variant\n\n@icon(\"rbxassetid://1\")\nstruct Sword as ... end\n\nlocal asset = Attributes.get(Sword, icon)\n```\nDeclared metadata, readable at runtime. An `attribute` declaration binds its name to an `Attribute<T>` value, so `attr` below is the name as written, not a string.\n\n| | |\n|---|---|\n| `Attributes.get(target, attr)` | The attribute's value on a function, a struct, or an enum, or nil. |\n| `Attributes.field(struct, name, attr)` | Its value on one field of a struct. |\n| `Attributes.variant(enum, name, attr)` | Its value on one variant of an enum. |\n| `Attributes.fields(struct, attr)` | The fields that carry it, as a table of field name to value. |\n| `Attributes.of(target)` | Everything declared on the target, as the compiler wrote it. |\n\nAn attribute with one parameter reads as that value; with several, as a table of them in order.",
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
        "**Wire types**\n\nA `remote` carries data across the network. A parameter typed as a function, a `thread`, a `Future`, or a `Signal` cannot be serialized, so the declaration is an error:\n\n```\nremote Ping(cb: () -> ()) from client\n-- remote `Ping`: parameter `cb` has type `() -> ()`, which is a function type; a remote carries only data\n```\n\nSend an id, a name, or a plain table, and keep the callback on the side that owns it.\n\nWhat crosses is packed: a remote with a `number`, `boolean`, or `string` parameter sends one `buffer` and unpacks it on the other side, so a fire costs its bytes and no names. A width attribute sets a number's size, `@u8`, `@u16`, `@u32`, `@i8`, `@i16`, `@i32`, `@f32`; a number without one takes 64 bits. A value that does not fit its width raises at the fire: `Heal: 300 does not fit @u8`. A struct, a record type, or an array of packable values packs field by field and item by item, down to any depth, and a struct comes back with its metatable. An Instance, a `Player`, or a map is not packed and travels as it is beside the buffer.",
    ),
    (
        "topic:lint",
        "**alloy lint**\n\nRuns the lints over the project, or over one file, and nothing else; `alloy flux` runs them with the compile and the type check. A lint is advice: the code runs, and the lint names a habit that costs bugs. The lints are Flux's, in eight groups:\n\n  correctness   code that is wrong, or cannot run\n  suspicious    code that is probably not what the author meant\n  style         a Luau habit with an Alloy form: `a and a.b` for `a?.b`, `typeof(x) == \"T\"` for `x is T`\n  complexity    a simple thing done in a hard way; the limits sit in `[flux]`\n  perf          code that runs slower than the plain form\n  roblox        a Roblox API that is deprecated or misused\n  pedantic      strict rules, off until `[lint] strict = true`\n  naming        the case of names, off until `warn = [\"naming\"]`\n\nA lint whose rewrite keeps the program the same carries it, and `--fix` applies those; in the editor each one is a quick fix, and `source.fixAll` applies every rewrite of the file.\n\n```\nalloy lint                 the project of the nearest alloy.toml\nalloy lint src/game.aly    one file\nalloy lint --fix           apply the rewrites that keep the program the same\nalloy lint -W pedantic     a level for this run: -W warns, -A allows, -D denies\nalloy lint --deny-warnings fail on any hit\nalloy lint --list          every lint with its group and default level\n```\n\nThe `[lint]` table of alloy.toml sets the levels. A list takes a lint name or a group name, and a name beats its group:\n\n```toml\n[lint]\nstrict = true\ndeny = [\"correctness\"]\nwarn = [\"pedantic\"]\nallow = [\"concat_interpolation\", \"luau\"]\n```\n\n`luau` is the group of the type checker's own lints, `LocalUnused` and the rest, which `alloy flux` reports. `alloy doc lints` lists every lint; `alloy doc <name>` explains one, and `alloy doc <group>` lists a group. The language server shows the same lints as warnings, with the rewrite in the message.",
    ),
    (
        "topic:flux",
        "**alloy flux**\n\nFlux is the whole analysis in one run, what clippy is to cargo. It compiles every source, runs luau-lsp over the check artifact and maps the type errors onto the Alloy lines, and runs every lint at its `[lint]` level: Flux's own seven groups, and the checker's lints under the `luau` group. It also sees what one file cannot: `circular_import` reports two files that import each other.\n\n```\nalloy flux                 the project of the nearest alloy.toml\nalloy flux src/game.aly    one file: the compile and the lints\nalloy flux --fix           apply the rewrites that keep the program the same\nalloy flux -D correctness  deny a group for this run; -W warns, -A allows\nalloy flux --explain manual_floor_div\nalloy flux --no-typecheck  skip luau-lsp\nalloy flux --watch         run again after every change\nalloy flux --list          every lint with its group and default level\n```\n\nThe check artifact keeps the source's lines, so a type error on line 12 of the output is on line 12 of the source; the column maps through the span map. The artifacts go into a mirror of the project under the temp directory, with the root's Luau configuration and a link to every other folder, so requires resolve as they do in the editor.\n\nThe `[flux]` table:\n\n```toml\n[flux]\ntypecheck = true                  # run luau-lsp over the check artifact\ndefinitions = []                  # extra .d.luau or .d.aly files; the project's .d.aly join on their own\nroblox_types = true               # load the Roblox globals\nsecurity_level = \"PluginSecurity\" # LocalUserSecurity, RobloxScriptSecurity, None\n# luau_lsp = \"/path/to/luau-lsp\"  # unset: the PATH, then ~/.alloy/bin and ~/.ember/bin\ntoo_many_arguments = 7\ntoo_many_lines = 100\nmax_nesting = 5\ncognitive_complexity = 25\n```\n\nThe Roblox globals come from the luau-lsp extension's storage when the editor has them, and download once into `~/.alloy/types` otherwise. A `--@alloy-ignore` line silences the checker's report on that line, as it does in the editor.",
    ),
    (
        "topic:test",
        "**alloy test**\n\nA test lives beside the code it tests: a `@test` function in the module, blanked from the ship artifact. `alloy test` builds the project, then writes one lest spec per source that holds a `@test`, under `[test] out`:\n\n```\nsrc/inventory.aly        ->  tests/inventory.spec.luau\nsrc/ui/menu.aly          ->  tests/ui/menu.spec.luau\n```\n\nA spec carries the tests and every top-level statement they reach: the imports they use, the locals and functions they call, the structs and the impls those need. The rest of the module stays out, so its side effects stay out of the test VM. The slice keeps the source's lines, so a failure points at the real one. Each relative `require` in the spec points at the build output, the runtime included, and the spec ends with a `describe` that registers each test by name; an `async` test is awaited.\n\n```alloy\nlocal function clamp01(x: number): number\n    return math.clamp(x, 0, 1)\nend\n\n@test\nfunction clamp_keeps_range()\n    $assert_eq(clamp01(2), 1)\nend\n```\n\n```\nalloy test                 build, then write the specs\nalloy test --run           and run lest on the suite\nalloy test --coverage      run lest with line coverage\nalloy test --filter hit    run the tests whose name holds `hit`\nalloy test -- --reporter json   the rest goes to lest as given\nalloy test --watch         write again after every change\nalloy test --check         write nothing; fail when a spec would change\nalloy test src/game.aly    one file's spec, to stdout\n```\n\nThe `[test]` table:\n\n```toml\n[test]\nout = \"tests\"     # where the specs go\nsuite = \"alloy\"   # the suite name in lest.toml\nlest = true       # write lest.toml and the @lest alias when the root has none\n```\n\nWith `lest = true`, the first run writes a `lest.toml` with one suite over `tests/**/*.spec.luau` on the native backend, and adds `lest = \".lest/core\"` to the aliases of `.luaurc`, which is where lest puts its framework. `lest` then runs the suite; on its VM the runtime falls back to plain coroutines, so an `await` settles at once. `$assert` and `$assert_eq` raise, and lest reports the line.",
    ),
    (
        "topic:fmt",
        "**alloy fmt**\n\nAnneal formats `.aly` and `.alx` files in place. The layout comes from the tokens and the `[fmt]` options, not from how the author laid the code out: the spacing between tokens, which bracket groups break, the quotes of a string, the parentheses of a call. Statements keep their lines, at most one blank line stays between them, and a long string or a long comment keeps its text. The program is the same afterwards; the token stream changes only where an option asks for a rewrite.\n\n```\nalloy fmt                  the sources of the project\nalloy fmt src/ui.aly dir/  the paths given\nalloy fmt --check          write nothing; fail when a file would change\n```\n\nA bracket group, the arguments of a call, a table, or an array, stays on one line when it fits in `column_width`, and breaks one element per line when it does not, or when a trailing comma in the source asks for it. An `import { }` list breaks on a trailing comma even when written on one line, and on every list with more than one name under `expand_imports`. A callback argument indents its body once: `foo(function()` opens one level, not two.\n\nThe options, with their defaults:\n\n```toml\n[fmt]\ncolumn_width = 100\nline_endings = \"unix\"                 # windows\nindent_type = \"spaces\"                # tabs\nindent_width = 4\nquote_style = \"auto-prefer-double\"    # auto-prefer-single, force-double, force-single, preserve\nleading_zero = \"add\"                  # `.5` becomes `0.5`; strip, preserve\ncall_parentheses = \"always\"           # no-single-string, no-single-table, none, input\nspace_after_function_names = \"never\"  # definitions, calls, always\ncollapse_simple_statement = \"never\"   # function-only, conditional-only, always\nblock_newline_gaps = \"never\"          # preserve keeps a blank line at a block's edge\nmagic_trailing_comma = true           # a trailing comma keeps a group expanded\ntrailing_comma = true                 # an expanded group ends its last element with a comma\nspace_inside_braces = true            # { a }\nspace_inside_parens = false           # f(a)\nspace_inside_brackets = false         # t[k]\nspace_inside_array = true             # [ 1, 2 ]; Alloy's own\nalign_struct_fields = false           # the `:` of a struct's fields line up; Alloy's own\nexpand_imports = false                # an import list with more than one name breaks one per line; Alloy's own\nexclude = []                          # paths to leave alone; `*` matches any run\n\n[fmt.call_chains]\nstyle = \"preserve\"                    # method: break before each call past the first; full: before every call\nmin_calls = 3\n\n[fmt.sort_requires]\nenabled = false                       # sort the `import` lines at the top of the file\ngrouping = \"flat\"                     # by-kind: aliases, then absolute, then relative paths\n\n[fmt.alx]\nattribute_quotes = \"double\"           # single, preserve\nbracket_same_line = false             # the `>` of a broken tag on the last attribute's line\nattribute_per_line = true             # a broken tag puts every attribute on its own line; false packs them\nself_closing_space = true             # <Frame />\ntext_wrap = \"fill\"                    # preserve keeps the author's line breaks in text\nblank_lines = true                    # a blank line between children stays\n```\n\nThe names follow larvae and stylua where the option is theirs, so a config ports over. In an `.alx` file the code formats the same way and the markup prints from its tree: a tag that fits stays on one line, a tag that does not breaks its attributes and then its children, and text flows with the holes in it.",
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
        "**alloy.toml**\n\n```toml\n[build]\nin = \"src\"\nout = \"build\"\nexclude = []\nclean = false\nartifact = \"ship\"\n\n[emit]\n# wait_timeout = 5\n# std_require = \"@alloy\"\n# erase_type_imports = false\n\n[lint]\nstrict = false\ndeny = []\nwarn = []\nallow = []\n\n[flux]\ntypecheck = true\ndefinitions = []\n\n[test]\nout = \"tests\"\nsuite = \"alloy\"\n```\n\nEvery key has a default, and an unknown key is an error. `alloy doc lint`, `alloy doc flux`, `alloy doc fmt`, and `alloy doc test` explain their tables. `alloy init` writes the file, plus `.luaurc` and `.config.luau` when the folder has neither: strict mode and the `@alloy` alias for the runtime the build writes. `[project]` and `[mount]` describe the DataModel tree; `alloy doc mount` explains them. `[alx]` holds the markup settings, the shape `luaux.toml` has, so a project with `.alx` files needs no second file.\n\nThe editor checks the file against a JSON Schema. `alloy self schema` prints it. `alloy self code` writes it to `~/.alloy/alloy.schema.json` and points VS Code (Even Better TOML) and Zed (Tombi, taplo) at it, so every key completes with its type, default, and text, and an unknown key is marked. `alloy build` writes the project's own schema to `.alloy/alloy.schema.json`, with the options and the lint names of its ingots; the `#:schema .alloy/alloy.schema.json` line at the top of the file, which `alloy init` writes, makes the editor read that one.",
    ),
    (
        "topic:luaurc",
        "**.luaurc and .config.luau**\n\nLuau reads its settings from `.luaurc`, a JSON file, or from `.config.luau`, a Luau chunk that returns `{ luau = { ... } }`. With both, `.config.luau` wins. Alloy reads and writes both the same way: `languageMode` and `aliases` are the keys it uses.\n\n```json\n{ \"languageMode\": \"strict\", \"aliases\": { \"alloy\": \"./build/alloy\" } }\n```\n\n```luau\nreturn { luau = { languagemode = \"strict\", aliases = { alloy = \"./build/alloy\" } } }\n```\n\nThe language server copies the file into its mirror, and adds `languagemode = \"strict\"` when the file sets no mode, so the default stays strict.",
    ),
    (
        "topic:directives",
        "**Directives**\n\nA comment that starts with `--@alloy-` silences diagnostics: the compiler's, the lints, and the checker's type errors, which the language server drops on the silenced lines before the editor sees them. The editor lists the directives after `--`, `--@`, or `--!`, and on an empty line.\n\n```alloy\n--@alloy-nocheck        this file: nothing is reported\n--@alloy-ignore         the next line with code is silent\nlocal x = y.z --@alloy-ignore   at the end of a line: that line\n--@alloy-expect-error   the next line must hold an error; a clean line is the error\n```\n\nUse `ignore` for a line the new solver gets wrong, and say why in the comment beside it. Use `expect-error` where the error is the point, a test of a lint or a contract: it silences the line, and reports when the line comes clean, so a fix that makes the directive stale shows. Luau's own `--!strict`, `--!nonstrict`, and `--!nocheck` pass through and set the checker's mode for the file.\n\nEvery diagnostic carries the book section it belongs to as its code, `Alloy(4.2)`; the number links to the section in the editor, and `alloy doc 4.2` prints it.",
    ),
    (
        "topic:markup",
        "**Markup**\n\nAn `.alx` file is Alloy with markup in it: tags, attributes, fragments, and text, the way JSX sits in TypeScript. The markup lowers to calls on a factory the project names, and the Alloy around it compiles as in any `.aly`.\n\n```alloy\nlocal function Row(props: { item: Item })\n    return (\n        <TextButton Size={UDim2.new(1, 0, 0, 24)} Activated={props.on_pick}>\n            {props.item.name} x{props.item.count}\n        </TextButton>\n    )\nend\n```\n\nA tag names a Roblox class or a component in scope; a component starts with a capital and is called with its props. An attribute is `Name={expr}` or `Name=\"text\"`; an event name takes a function. `{expr}` in a body is a child, and text with `{holes}` interpolates. `<> ... </>` is a fragment, `<!-- -->` a comment. A child that depends on a condition wraps in a function, `{function() return if open then <Menu /> else nil end}`, so a reactive library re-runs it; a bare conditional child is built once, and the `static_conditional_child` lint says so.\n\nThe `[alx]` table of alloy.toml sets the lowering:\n\n```toml\n[alx.factory]\nbackend = \"table\"        # table: create(name)(props); element: create(name, props, children)\ncreate = \"create\"        # the function a tag calls; a bare name must be in scope\nchildren = \"Children\"    # the key children go under; unset, they are numeric entries\nevent = \"OnEvent\"        # wraps an event name: OnEvent(\"Activated\"), or React.Event\ncompute = \"computed\"     # wraps interpolated text; `use` names the reader inside it\nfragment = \"Fragment\"    # the component a fragment builds with; a plain table unset\ninterpolate = \"concat\"   # how text with holes joins\nmerge = \"merge\"          # how a spread group `{...props}` combines\n\n[alx.elements]\nall = \"camel\"            # a naming scheme for every class, or one alias per name\n\n[alx.properties]\nText = \"text\"            # an alias for a property, everywhere or per class\n\n[alx.lints]\nstatic_conditional_child = \"warn\"\n```\n\nWith no `[alx]`, the lowering is the table form with `create` in scope. `[fmt.alx]` sets how `alloy fmt` lays a tag out. An ingot may read a markup attribute, `ClassName` for Enamel, and rewrite the tag before the lowering.\n\nIn the editor a tag and an attribute hover and complete from the Roblox API, and the semantic tokens of the lowered code stay out of the markup.",
    ),
    (
        "topic:ingots",
        "**Ingots**\n\nAn ingot is an Alloy extension. It ships as an executable beside an `ingot.toml`, and the compiler and the language server start it once and keep it alive. Over a framed pipe, one request per file, an ingot can:\n\n  transform   edit the Alloy source before the desugar; the line count holds, and every position maps back\n  output      edit the ship Luau after the desugar\n  lint        report findings under its own lint names, with rewrites\n  format      edit a file after Anneal laid it out\n  hover       answer a hover in the editor\n  complete    add completion items\n  actions     add code actions\n  colors      name the colors a file holds, for the editor's swatches and picker\n\nName one in alloy.toml. A path is relative to the root; a release is pinned by version and unpacks once into `.alloy/ingots/`:\n\n```toml\n[ingots]\ntailwind = \"ingots/tailwind\"\nlogger = { repo = \"someone/logger-ingot\", version = \"0.2.0\" }\n\n[ingot.tailwind]\n# the ingot's own options, over the defaults its manifest declares\nsort_classes = true\n```\n\nThe manifest names the ingot, the protocol revision, the hooks the host may send, the options with their defaults, and the lints with their levels:\n\n```toml\nname = \"tailwind\"\napi = 1\nhooks = [\"transform\", \"lint\", \"hover\", \"complete\"]\nrun = \"first\"          # the pass its transform runs in: first, last, or a number\nkinds = [\"alx\"]        # the file kinds it wants; unset means all\n\n[options]\nsort_classes = false\n\n[lints.unknown_class]\ndefault = \"warn\"\nsummary = \"a class no utility defines\"\n```\n\nAn ingot's lint is `<ingot>/<lint>` in `[lint]`, and the ingot's name is a group, so `allow = [\"tailwind\"]` silences all of them. An option written as `{ default = false, doc = \"...\" }` carries its text into the editor: `alloy build` writes the project's schema, `.alloy/alloy.schema.json`, with every ingot's options and lint names, and the `#:schema` line at the top of alloy.toml makes the editor complete them. `alloy lint --list` shows them under the ingot; `alloy doc tailwind/unknown_class` explains one.\n\nWrite one in Rust with the `alloy-ingot` crate: implement `Handler`, call `serve`. `alloy ingot new <name>` writes the project, `alloy ingot info <dir>` prints what a manifest declares, and `alloy ingot run <dir> <file>` pushes one file through it and reports the line count, because a transform that adds a line breaks the map. An edit is a byte span and its text; the host applies every edit of one reply at once, so no edit sees another's output. An ingot that hangs costs one request: the host kills it after a timeout and reports the loss.\n\nThe design follows larvae's native worms. Nothing is embedded: no interpreter, no wasm.",
    ),
    (
        "topic:mount",
        "**Mounts and project files**\n\nOne table in alloy.toml says where each folder lands in the DataModel, and `alloy build` writes every file that follows from it:\n\n```toml\n[project]\nname = \"game\"\nruntime = \"@game/ReplicatedStorage/Alloy\"\n\n[mount]\n# alias = [path, mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nclient = [\"src/client\", \"@game/StarterPlayer/StarterPlayerScripts/Client\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n```\n\n  default.project.json        a Rojo project over the sources\n  .alloy/build.project.json   the same tree over the compiled output, for `rojo serve` and `rojo build`\n  .alloy/sourcemap.json       the instance tree with the source paths; the language server reads it\n  .luaurc                     gains an alias per mount, so `@pkg/jecs` resolves in the editor\n\nRoblox reads no `.luaurc`, so the ship artifact rewrites `require(\"@pkg/jecs\")` into the mount's instance path, `@game/ReplicatedStorage/Packages/jecs`, and requires the runtime by its `[project] runtime` path the same way. A path under `[build] in` points at its output in the build project; any other path, such as a package folder, is mounted as it is. `.server.` and `.client.` name the script class, `init` names its directory, and a `StarterPlayerScripts` between a service and a leaf keeps its own class.",
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
        title: "Mounts and project files",
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
        (&["ingot `"], "IngotError"),
        (&["reserved word"], "ReservedWord"),
        (&["in macro expansion"], "MacroError"),
        (&["not exhaustive", "no arm for"], "ExhaustiveMatch"),
        (&["remote"], "WireType"),
        (&["directive"], "DirectiveError"),
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
    format!("{}: {message}", kind_for(message))
}

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
    ];

    rules
        .iter()
        .find(|(words, _)| words.iter().any(|w| m.contains(w)))
        .map(|(_, code)| *code)
}
