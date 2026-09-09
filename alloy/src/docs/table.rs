//! The documentation table: every operator, declaration, intrinsic,
//! derive name, attribute, ambient std name, and `topic:` article,
//! keyed by the word as written.

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
        "The fallback arm of a `match`. Required when the scrutinee is a literal, a struct, or a table.\n\nAfter `export`, the one value a module sends out under no name: `export default expr` or `export default <declaration>`. `import X from \"./m\"` reads it, under any name.",
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
        "```alloy\nimport { a, b as c, type T } from \"./m\"\nimport * as M from \"./m\"\nimport Name from \"./m\"\nimport M, { a, type T } from \"./m\"\nimport type { T } from \"./m\"\n```\nBrings names from another module into this file. `{ }` picks exports by name, `as` renames one, and `* as M` takes the whole module, where the default sits under `M.default`.\n\nA bare name takes the module's `export default`, whatever the name here is: `import Name from \"./m\"` is `local Name = require(\"./m\").default`. A module with no `export default` reports on that line and names the export you probably meant. The default and the named exports are separate, so `import M, { a }` takes both in one line. A plain Luau module and a data file have no export table: a bare name takes the value they return.\n\n`type` marks a type-only import, which costs nothing at runtime.\n\n`import(\"./m\")` is the expression form: a `require`, typed from the module when the path is a string or an instance chain. `import<<T>>(expr)` gives a dynamic path the type `T`; without it the value is `unknown`.\n\nA path that ends in `.json` or `.toml` imports a data file as a table; `alloy doc data` explains.\n\nA `global` needs no import: the name is in scope in every file of the project, and an `import` of one reports on the name. `alloy doc global` has the rules.\n\nA namespace comes in as one name: `import { Math } from \"./math\"` binds the group and every type it carries, so `Math.Vec2` reads in a type slot here too.\n\nA Roblox service is an import too. `import Players from \"game:Players\"` names one service, and `import { Players, ReplicatedStorage } from \"game\"` names any number, with `as` to rename one. Both lower to `local Players = game:GetService(\"Players\")` on the import's own line, so the binding carries the service class. `\"game\"` takes the braces and `\"game:X\"` takes a bare name; the other way round reports the form to write, and a name that is no service names the nearest one.",
    ),
    (
        "export",
        "```alloy\nexport local x = 1\nexport function f() end\nexport struct Vec2 as ... end\nexport { a, b as c }\nexport type { T }\nexport default expr\nexport default function make() end\n```\nAdds a name to the table the module returns at the end of its scope. Any declaration takes it: `local`, `const`, `function`, `async function`, `struct`, `enum`, `trait`, `interface`, `remote`, `attribute`, `macro`, `impl`. `export { }` names bindings after the fact, `as` renames one on the way out, and `export type { }` exports types alone.\n\n`export default` puts one value in the table under `default`, which is what `import Name from` reads. It takes an expression or a declaration; a declaration also binds its name in the module. A default sits beside any number of named exports, and a module has at most one. A type is not a value: send one out with `export type`.\n\nA Roblox service comes in as an import, `import Players from \"game:Players\"` or `import { Players } from \"game\"`; `alloy doc import` has the forms. A module sends one on the way every other binding goes: `export { Players }` after the import line.\n\n`global` is the wider form: it exports the name and puts it in scope in every file of the project, with no import. `export global` is an error, since `global` already reaches everywhere; `alloy doc global` has the rules.\n\n`export namespace Name as ... end` sends a whole group under one name, and the types the group holds travel with it; `alloy doc namespace` has the shape.",
    ),
    (
        "global",
        "```alloy\nglobal function log(msg: string) end\nglobal const MAX = 10\nglobal struct Vec2 as ... end\nglobal enum State as ... end\nglobal type Id = number\nglobal impl BasePart as ... end\n```\nPuts a name in scope in every file of the project, with no import. `global` sits where `export` sits, in front of the declaration, and every declaration takes it: `local`, `const`, `function`, `async function`, `struct`, `enum`, `trait`, `interface`, `type`, `remote`, `attribute`, `macro`, `impl`, `namespace`. `global namespace Math as ... end` puts the whole group in scope, and a type of it reads as `Math.Vec2` in every file.\n\nThe build resolves it. Each file that names a global gets the `require` of the declaring module and the binding on its first line, the way the runtime require is written, so the line count holds and the type checker reads the same module the run does. Nothing goes through `_G` or `shared`.\n\nA global needs a project: outside a folder with alloy.toml there is no set of files to reach. The declaring module still exports the name, so a build that reads the output by hand finds it, but an `import` of a global in Alloy source is an error: the name is already here. `export global` is an error too.\n\nTwo files that declare one global name report, and so does a global whose module leads back to a file that uses it, since the injected require would close a cycle. A global by a Luau name, `print` or `type`, is an error; a global by a std name such as `Signal` fires the pedantic lint `shadowed_global`, and the project\'s name wins.\n\nA `.d.aly` declares ambient names with no module behind them: `global` in one is an error, and a global by a name a `.d.aly` already declares is an error naming both files.\n\nA script, `main.server.aly` or `hud.client.aly`, cannot be required, so the build moves its globals into `main.server.globals.luau` beside it and every file requires that. A global in a script may name only imports, other globals, and literals. A global declared on one side is out of scope on the other, and out of scope in a shared file too, since a shared file runs on either side. The side of a file, strongest word first: the name suffix, `--@alloy-file-side`, the `[contexts]` table of alloy.toml, then the DataModel service the file lands under, where `ServerScriptService` and `ServerStorage` are the server and `StarterPlayer`, `StarterGui`, and `StarterPack` are the client. A `--@alloy-side` right above one global beats all of that for that global alone.\n\nA `global impl` on a foreign type such as `BasePart` is the project-wide extension. `export impl` still parses and says the same thing; `[lint.rules] export_impl = \"warn\"` turns on the lint that asks for `global impl`, and `alloy flux --fix` rewrites the word.",
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
        "namespace",
        "```alloy\nnamespace Name as ... end\n```\nOne name over a group of declarations. A member is a `function`, a `const`, a `local`, a `struct`, an `enum`, a `trait`, an `interface`, a `type`, an `impl`, a `remote`, an `attribute`, a `macro`, or another `namespace`. Outside, a member reads as `Name.member`; inside, a sibling reads by its own name. A member takes `public`, the default, or `private`, which keeps it inside the namespace. `export namespace` sends the group, and `global namespace` reaches every file.\n\nEmits a table: `local Name = {}`, one name per member, and `Name.member` on it. A type of the namespace emits as `Name_Type`, since Luau has no `Name.Type` type path.",
    ),
    (
        "new",
        "```alloy\nnew Name(...)\nnew Name(...) { Field = value }\n```\nConstructs a value. `new Name(...)` calls the constructor a struct's impl wrote, `new` or `New`, or the `new` of a Roblox datatype, an Instance, or any class; braces after it set fields on the new value, one per line. `new Name { ... }` is a struct's fields form, the only way to construct one that writes no constructor. A struct never constructs without `new`.",
    ),
    (
        "delete",
        "```alloy\ndelete expr\n```\nCleans the value up, whatever it is: an Instance is destroyed, an `RBXScriptConnection` and a `SignalConnection` disconnect, a thread is cancelled, a function is called, a `Scope` closes, a `Signal` is destroyed, and any other table takes its `Destroy`, `Disconnect`, `destroy`, or `disconnect` method; the Roblox spelling wins when a table has both. The std names that shape `Deletable`. `delete t.field` and `delete t[key]` then set the slot to nil, so the table holds nothing destroyed.\n\nWhat a scope holds for an Instance goes first: `scope:add(connection, part)` names the Instance an item belongs to, and `delete part` disconnects the item before the Destroy, so no handler runs against an Instance that is half gone.\n\n`delete` takes no timer. `destroy x after n` is the one that waits.",
    ),
    (
        "destroy",
        "```alloy\ndestroy expr\ndestroy expr after seconds\n```\nCalls the value's destroy method and nothing else. The operand is an Instance or a value whose type has a `destroy` or a `Destroy`; the std names that shape `Destroyable`, and any other type is a compile error naming it. Use `delete` for a connection, a thread, or a scope.\n\n`destroy x after n` waits `n` seconds first. An Instance goes to `Debris:AddItem(x, n)`, which outlives the script that scheduled it; a value with a method goes on a `task.delay`. The emit picks by the type the file shows, and asks `typeof(x)` at run time when the file shows none.",
    ),
    (
        "after",
        "```alloy\nafter 3 do\n    part.Transparency = 1\nend\n\nafter 3 where alive do\n    respawn()\nend\n```\nRuns a block later: `task.delay(seconds, function() ... end)`. The seconds are a number. `return` leaves the block, not the function around it, since the block is a function of its own.\n\n`where` puts a condition on it, and the condition is read when the timer fires, not when the block is scheduled. A local the code changes in between is read at its new value.\n\n`after` is also the word in `destroy x after n`. It is reserved either way, so no name may be `after`.",
    ),
    (
        "attribute",
        "```alloy\nattribute name(params) on target, ...\n```\nDeclares an attribute: metadata the compiler reads and `Attributes` reads at runtime. Targets: function, struct, enum, variant, field, param, remote, interface, type, local, namespace.\n\nThe built-in ones: `@derive` and `@sealed` on a struct or an enum; `@test` and `@cfg` on a function, `@cfg` on a local or a namespace too; `@deprecated` on a namespace as well; `@rename` and `@skip` on a field; `@unreliable`, `@ratelimit`, `@timeout`, and `@validate` on a remote; `@u8` to `@f32` on a parameter or a field; and Luau's own `@native`, `@checked`, `@deprecated`, `@inline`, `@noinline`, which pass through.",
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
        "@immediate",
        "```alloy\n@immediate\nremote ...\n```\nA fire goes out at once instead of joining the per-frame batch: for a remote whose latency matters more than its bandwidth.\n\n**Applies to** `remote`",
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
        "**alloy.toml**\n\nThe file `alloy init` writes:\n\n```toml\n#:schema .alloy/alloy.schema.json\n[build]\nin = \"src\"\nout = \"build\"\nexclude = []\nclean = false\nartifact = \"ship\"\n\n[emit]\n# wait_timeout = 5\n# std_require = \"@alloy\"\n# erase_type_imports = false\n\n[fmt]\nrecommended = true\ncolumn_width = 100\nindent_type = \"spaces\"\nindent_width = 4\nquote_style = \"auto-prefer-double\"\n\n[lint]\nrecommended = true\nstrict = true\n\n[lint.rules]\n# raw_require = \"allow\"\n\n[flux]\ntypecheck = true\ndefinitions = []\n\n[test]\nout = \"tests\"\nsuite = \"alloy\"\nlest = true\nshim = true\n\n[project]\nname = \"game\"\nsourcemap = true\nsource_of_truth = true\nmount_aliases = true\n\n# [mount]\n# alias = [path, mount]: the folder at path lands at mount in the DataModel\n# shared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n# server = [\"src/server\", \"@game/ServerScriptService/Server\"]\n\n# [contexts]\n# which folders hold which side's code; a folder name matches any segment\n# of a file's path, and an entry with a \"/\" is a path from this file\n# client = [\"client\", \"ui\"]\n# server = [\"server\"]\n# shared = [\"shared\"]\n\n# [ingots]\n# an extension that ships as an executable: a path relative to this file,\n# or a GitHub release pinned by version\n# tailwind = \"ingots/tailwind\"\n# tailwind = { repo = \"alloy-luau/tailwind-ingot\", version = \"0.1.0\" }\n```\n\nEvery key has a default, and an unknown key is an error. Both `recommended` keys are on: `[lint] recommended` applies the level each lint declares, and `[fmt] recommended` applies the layout above; either off leaves that table's own keys as the whole setting (`alloy doc lint`, `alloy doc fmt`). `[lint.rules]` gives one lint, one group, or one markup lint under `alx.` a level of `allow`, `warn`, or `deny`. `alloy doc lint`, `alloy doc flux`, `alloy doc fmt`, and `alloy doc test` explain their tables, and `alloy doc markup` the `[alx]` table, which the written file leaves out. `alloy init` writes the file, plus one Luau configuration: a `.config.luau` with strict mode and the `@alloy` alias when the folder has neither file, else the mode and the alias added to the `.config.luau` or `.luaurc` it already has (`alloy doc init`). `[project]` says which project file describes the DataModel tree, and `[mount]` describes one for a tool that reads no project file. Two keys of `[project]`, both on by default, say how far the table reaches: `source_of_truth` writes `default.project.json` and `.alloy/build.project.json` from it, and `mount_aliases` serves its names as aliases beside the ones the Luau configuration declares. `alloy doc mount` explains all of it. `[alx]` holds the markup settings, the shape `luaux.toml` has, so a project with `.alx` files needs no second file.\n\nThe editor checks the file against a JSON Schema. `alloy self schema` prints it. `alloy self code` writes it to `~/.alloy/alloy.schema.json` and points VS Code (Even Better TOML) and Zed (Tombi, taplo) at it, so every key completes with its type, default, and text, and an unknown key is marked. `alloy build` writes the project's own schema to `.alloy/alloy.schema.json`, with the options and the lint names of its ingots; the `#:schema .alloy/alloy.schema.json` line at the top of the file, which `alloy init` writes, makes the editor read that one.",
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
        "**Directives**\n\nA comment that starts with `--@alloy-` steers the diagnostics of one file: the compiler's, the lints, and the checker's type errors, which the language server drops on the silenced lines before the editor sees them. The editor lists the directives after `--`, `--@`, or `--!`, and on an empty line. `alloy doc alloy-ignore` explains one.\n\n```alloy\n--@alloy-nocheck                          this file: nothing is reported\n--@alloy-ignore the solver misreads this  the next line with code is silent\nlocal x = y.z --@alloy-ignore             at the end of a line: that line\n--@alloy-expect-error a negative count    the next line must hold an error\n--@alloy-ignore-start raw_require         every line under here, up to the end\n--@alloy-ignore-end                       closes it\n--@alloy-lint raw_require=allow           this lint's level, for this file\n--@alloy-file-side client                 the side this file sits on\n--@alloy-side client                      the side of the global under it\n--@alloy-preserve                         `alloy flux --fix` leaves the next line\n```\n\nText after `--@alloy-ignore` and `--@alloy-expect-error` is the reason. An expectation with no reason draws the `missing_reason` lint, and the reason comes back in the error the directive draws when its line goes clean, so a stale one is easy to place.\n\nUse `ignore` for a line the new solver gets wrong. Use `expect-error` where the error is the point, a test of a lint or a contract: it silences the line, and reports when the line comes clean, so a fix that makes the directive stale shows. Luau's own `--!strict`, `--!nonstrict`, and `--!nocheck` pass through and set the checker's mode for the file.\n\n`--@alloy-lint`, `--@alloy-file-side`, `--@alloy-side`, `--@alloy-ignore-start`, and `--@alloy-ignore-end` sit on a line of their own. `--@alloy-side` sits right above the global it names. `--@alloy-ignore`, `--@alloy-expect-error`, and `--@alloy-preserve` sit on their own line or at the end of a line with code.\n\nA directive the compiler cannot accept is a `DirectiveError` on its own line: a name no directive has, a lint or a level `--@alloy-lint` does not know, an `--@alloy-ignore-start` with no end, an `--@alloy-ignore-end` that closes nothing, an `--@alloy-file-side` that contradicts the file name, and an `--@alloy-side` that sits over anything but a global.\n\nEvery diagnostic carries the book section it belongs to as its code, `Alloy(4.2)`; the number links to the section in the editor, and `alloy doc 4.2` prints it.",
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
        "topic:alloy-file-side",
        "**--@alloy-file-side**\n\nSays which side the whole file sits on: `client`, `server`, or `shared`. A `.client.aly` or `.server.aly` name says the same thing, and this directive says it in a file whose name does not.\n\n```alloy\n--@alloy-file-side server\nremote Buy from client(item: string)\n\nBuy.on(function(sender, item) end)   -- the server handles\n```\n\nThe side decides two things: which half of every `remote` the file sees, and which `global` names it reaches. A shared file sees both halves of a remote, the way a module that branches on `RunService` does, and reaches shared globals alone.\n\nThe side of a file, from the strongest word to the weakest: the name suffix, this directive, the `[contexts]` table of alloy.toml, then the DataModel service the file lands under. `ServerScriptService` and `ServerStorage` are the server; `StarterPlayer`, `StarterGui`, and `StarterPack` are the client; every other service replicates, `ReplicatedFirst` included, so a file there is shared.\n\nA directive that contradicts the file\'s name is a `DirectiveError`. `@cfg(server)` is a different thing: it is a check the emitted code runs, not a decision the compiler makes, so the side does not reach it.",
    ),
    (
        "topic:alloy-side",
        "**--@alloy-side**\n\nSays which side one `global` sits on: `client`, `server`, or `shared`. It covers the declaration under it, past blank and comment lines, and nothing else.\n\n```alloy\n--@alloy-side client\nglobal const THEME = \"dark\"\n```\n\nA global of one side is in scope in the files of that side alone. The directive beats every rule the file\'s own side follows, so a shared module may hold a client global and a server one side by side.\n\nOver anything but a global the directive says nothing, and reports. `--@alloy-file-side` is the one that names the side of a whole file.",
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
        "**Ingots**\n\nAn ingot is an Alloy extension. It ships as an executable beside an `ingot.toml`, and the compiler and the language server start it once and keep it alive. Over a framed pipe, one request per file, an ingot can:\n\n  transform   edit the Alloy source before the desugar; the line count holds, and every position maps back\n  output      edit the ship Luau after the desugar\n  lint        report findings under its own lint names, with rewrites\n  format      edit a file after Anneal laid it out\n  hover       answer a hover in the editor\n  complete    add completion items\n  actions     add code actions\n  colors      name the colors a file holds, for the editor's swatches and picker\n\nName one in alloy.toml. A path is relative to the root; a release is pinned by version and unpacks once into `.alloy/ingots/`:\n\n```toml\n[ingots]\ntailwind = \"ingots/tailwind\"\nlogger = { repo = \"someone/logger-ingot\", version = \"0.2.0\" }\n\n[ingot.tailwind]\n# the ingot's own options, over the defaults its manifest declares\nsort_classes = true\n```\n\nThe manifest names the ingot, the protocol revision, the hooks the host may send, the options with their defaults, and the lints with their levels:\n\n```toml\nname = \"tailwind\"\napi = 1\nhooks = [\"transform\", \"lint\", \"hover\", \"complete\"]\nrun = \"first\"          # the pass its transform runs in: first, last, or a number\nkinds = [\"alx\"]        # the file kinds it wants; unset means all\n\n[options]\nsort_classes = false\n\n[lints.unknown_class]\ndefault = \"warn\"\nsummary = \"a class no utility defines\"\n\n[props]\nClassName = { doc = \"the utility list\", insert = \"ClassName=\\\"$1\\\"\" }\n```\n\n`[props]` names the props the ingot reads on a markup tag; the editor completes them on any tag of a file kind the ingot wants, and a bare string is the prop's doc.\n\nAn ingot's lint is `<ingot>/<lint>` in `[lint]`, and the ingot's name is a group, so `allow = [\"tailwind\"]` silences all of them. An option written as `{ default = false, doc = \"...\" }` carries its text into the editor: `alloy build` writes the project's schema, `.alloy/alloy.schema.json`, with every ingot's options and lint names, and the `#:schema` line at the top of alloy.toml makes the editor complete them. `alloy lint --list` shows them under the ingot; `alloy doc tailwind/unknown_class` explains one.\n\nWrite one in Rust with the `alloy-ingot` crate: implement `Handler`, call `serve`. `alloy ingot new <name>` writes the project, `alloy ingot info <dir>` prints what a manifest declares, and `alloy ingot run <dir> <file>` pushes one file through it and reports the line count, because a transform that adds a line breaks the map. An edit is a byte span and its text; the host applies every edit of one reply at once, so no edit sees another's output. An ingot that hangs costs one request: the host kills it after a timeout and reports the loss.\n\nThe design follows larvae's native worms. Nothing is embedded: no interpreter, no wasm.",
    ),
    (
        "topic:mount",
        "**Project files and mounts**\n\nAlloy needs to know where each folder lands in the DataModel. Two things can say so, and a project writes one of them.\n\n**The project file.** A root with a Rojo or Argon project file needs nothing in alloy.toml: Alloy reads the file and takes its tree as written. It reads `default.project.json`, or the file `[project] file` names, or the one `*.project.json` at the root.\n\n```json\n{\n  \"name\": \"game\",\n  \"tree\": {\n    \"$className\": \"DataModel\",\n    \"ReplicatedStorage\": {\n      \"$className\": \"ReplicatedStorage\",\n      \"Shared\": { \"$path\": \"src/shared\" },\n      \"Packages\": { \"$path\": \"Packages\" },\n      \"Alloy\": { \"$path\": \"build/alloy.luau\" }\n    },\n    \"ServerScriptService\": {\n      \"$className\": \"ServerScriptService\",\n      \"Server\": { \"$path\": \"src/server\" }\n    }\n  }\n}\n```\n\nEvery `$path` in the tree is a folder on disk with a place in the DataModel, at any depth. `$className`, `$properties`, and `$ignoreUnknownInstances` are directives, so they never become instances. `.server.` and `.client.` in a file name pick the script class, `init` names its directory, and a container between a service and a leaf, `StarterPlayerScripts`, keeps its own class.\n\nAlloy derives four things from that tree, and writes none of them back into your file:\n\n  .alloy/build.project.json   the same tree over the output\n  sourcemap.json              the instance tree with the source paths\n  the @alias rewrite          instance paths in the ship artifact\n  the runtime's place         where build/alloy.luau lands\n\nThe build project points every `$path` under `[build] in` at its output under `[build] out`, and leaves any other path as it is; `rojo serve` and `rojo build` read that one. The sourcemap sits at the root under the name Rojo writes and luau-lsp reads, so `rojo sourcemap` and the language server both find this one; `[project] sourcemap = true`, the default, writes it on every build, over a file another tool wrote there, and `false` writes none. Roblox reads no `.luaurc`, so `require(\"@shared/economy\")` in the ship artifact becomes `require(\"@game/ReplicatedStorage/Shared/economy\")`.\n\n`default.project.json` is yours: Alloy never writes it when it is there. The runtime lands where the tree already mounts `build/alloy.luau`, else inside the node that mounts `[build] out`, else at `@game/ReplicatedStorage/Alloy`; `[project] runtime` names it outright.\n\n**The aliases.** They come from `.config.luau` or `.luaurc`, which is where Luau reads them, and where the editor and `alloy flux` already read them. An alias names a folder on disk; the tree says where that folder lands; the ship artifact writes the instance path.\n\n```json\n{ \"languageMode\": \"strict\", \"aliases\": { \"shared\": \"src/shared\", \"pkg\": \"Packages\" } }\n```\n\nA data file resolves the same way: `import config from \"@shared/data/config.json\"` reads `src/shared/data/config.json` and requires the module the build writes beside it. `alloy init` writes the `@alloy` alias once; nothing else writes to these files.\n\n**Watch mode.** `alloy build --watch` polls the sources, alloy.toml, the project file, and every folder the tree mounts, so a new file anywhere in the tree writes a new sourcemap and a new build project.\n\n**The mount table.** A tool that reads a Rojo or Argon project file needs no table. A sync tool with its own format has no such file, so alloy.toml describes the tree instead:\n\n```toml\n[project]\nname = \"game\"\n\n[mount]\n# alias = [path, mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nclient = [\"src/client\", \"@game/StarterPlayer/StarterPlayerScripts/Client\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n```\n\nThe table is the tree when the project writes one, over any project file at the root. Two keys of `[project]` say how far it reaches, and both are on:\n\n```toml\n[project]\n# write default.project.json and .alloy/build.project.json from [mount]\nsource_of_truth = true\n# serve the mount names as aliases, beside the Luau config ones\nmount_aliases = true\n```\n\n`source_of_truth = false` writes neither project file and derives no build project: the sync tool of the project owns the tree, and the table is left to rewrite an `@alias` require into an instance path in the ship artifact. `mount_aliases = false` leaves the aliases to `.config.luau` or `.luaurc` alone, so a mount name completes and resolves nowhere.\n\nWith both on, `alloy build` writes `default.project.json` over the sources, so `rojo serve` has a file to read, and `@shared/x` completes in the editor whether the Luau configuration names it or not. A name in the Luau configuration always wins over a mount of that name.\n\n**The side of a file.** The tree also says which side a file sits on, which decides the half of a `remote` it sees and the `global` names it reaches. A `.client` or `.server` name wins, then `--@alloy-file-side`, then the `[contexts]` table, then the service the file lands under: `ServerScriptService` and `ServerStorage` are the server, `StarterPlayer`, `StarterGui`, and `StarterPack` are the client, and every other service replicates, `ReplicatedFirst` included, so a file there is shared.\n\n```toml\n[contexts]\n# a folder name matches any segment of a file's path under [build] in;\n# an entry with a \"/\" is a path from this file\nclient = [\"client\", \"ui\"]\nserver = [\"server\"]\nshared = [\"shared\"]\n```\n\nThe table is for a project that keeps a `client` folder under `ReplicatedStorage`, where the service alone would call it shared.\n\n**Neither.** A root with no project file and no table still builds. The output tree mirrors the source tree, emitted code requires the runtime by a relative path, and `@alias` requires stay as they are: no instance paths, no sourcemap, no project files.",
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
