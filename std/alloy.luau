--!nonstrict
-- The Alloy runtime. One plain Luau module, required by emitted code, and
-- usable from vanilla Luau. Emitted code reaches it as `__alloy`.

local alloy = {}

-- ---------------------------------------------------------------------------
-- Printing. Every Alloy value has a `__tostring`, so `print(v)` and
-- `{v}` in a string show the value and not a table address. A struct's
-- own `to_string` replaces the default the compiler writes.
-- ---------------------------------------------------------------------------

--- The text of a value inside a printed container: a string in quotes,
--- anything else through `tostring`.
function alloy.show(v: any): string
	if type(v) == "string" then
		return `"{string.gsub(v, '["\\]', "\\%0")}"`
	end
	return tostring(v)
end

local function show_list(values: { any }, open: string, close: string): string
	if #values == 0 then
		return open .. close
	end
	local parts = table.create(#values)
	for i, v in ipairs(values) do
		parts[i] = alloy.show(v)
	end
	return open .. " " .. table.concat(parts, ", ") .. " " .. close
end

--- `Name { x = 1, y = 2 }`: the default printer of a struct, over its
--- declared fields in order.
function alloy.show_struct(name: string, s: any, fields: { string }): string
	if #fields == 0 then
		return name .. " {}"
	end
	local parts = table.create(#fields)
	for i, f in ipairs(fields) do
		parts[i] = f .. " = " .. alloy.show(s[f])
	end
	return name .. " { " .. table.concat(parts, ", ") .. " }"
end

--- `Msg.Move(1, 2)`: the printer of an enum variant with a payload.
function alloy.show_variant(name: string, v: any): string
	local parts = {}
	local i = 1
	while v["_" .. i] ~= nil do
		table.insert(parts, alloy.show(v["_" .. i]))
		i += 1
	end
	return `{name}.{v.tag}({table.concat(parts, ", ")})`
end

--- The keys of a map in a stable order, so a printed map reads the same
--- every time.
local function sorted_keys(t: { [any]: any }): { any }
	local keys = {}
	for k in pairs(t) do
		table.insert(keys, k)
	end
	table.sort(keys, function(a, b)
		if type(a) == type(b) and (type(a) == "number" or type(a) == "string") then
			return a < b
		end
		return tostring(a) < tostring(b)
	end)
	return keys
end

-- ---------------------------------------------------------------------------
-- Result: a tagged table. `tag` is "Ok" or "Err"; `_1` holds the payload.
-- ---------------------------------------------------------------------------

-- Two shapes under one name, so `r.tag == "Ok"` narrows `r._1` to `T`
-- in a `match` arm and in `if local Ok(v) = r`. The methods live in one
-- alias both shapes meet, so a call on the union sees one function and
-- infers its generic: two copies of `map` on the two shapes left `U`
-- as never. Every member reads only, which keeps `Result<T, E>`
-- covariant, so a `Result<Boost, E>` passes where `Result<any, E>` is
-- asked. Luau rejects an alias that names itself with other arguments,
-- so `map` yields `Result2`, the same surface one map deep, whose own
-- `map` is `any`.
--
-- The receiver is the data half of a Result, the two tagged tables
-- without the methods. `Result<T, E>` itself would close the loop
-- alias to alias, and the solver gives up on it; the data half reads
-- back as `Result<T, E>` in a hover, and it binds `T` and `E`.
export type ResultMethods<T, E> = {
	read unwrap: (self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }) -> T,
	read expect: (self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }, message: string) -> T,
	read unwrap_or: <D>(self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }, default: D) -> T | D,
	read map: <U>(self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }, f: (T) -> U) -> Result2<U, any>,
	read map_err: <F>(self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }, f: (E) -> F) -> Result2<any, F>,
	read is_ok: (self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }) -> boolean,
	read is_err: (self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }) -> boolean,
	read ok: (self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }) -> T?,
}

-- `__ok` and `__err` exist in the type alone: they carry `T` and `E` as
-- plain members, so a generic reads them off a Result it is handed
-- (`try_await_result`), where the methods' own generics stop it.
export type ResultOk<T, E> = { tag: "Ok", read _1: T, read trace: string?, read __ok: T, read __err: E } & ResultMethods<T, E>
export type ResultErr<T, E> = { tag: "Err", read _1: E, read trace: string?, read __ok: T, read __err: E } & ResultMethods<T, E>
export type Result<T, E> = ResultOk<T, E> | ResultErr<T, E>
-- `Result2` puts the two tags in a union and intersects the methods
-- over the pair. The union is what lets a mapped Result fit a declared
-- `Result<T, E>`, which `return r:map(f)` asks for: a single table is
-- the subtype of neither tag. The methods sit outside the union, so a
-- member read is one function and a callback's parameter still solves;
-- a union of two identical functions leaves it free.
--
-- `map` answers `Result2<U, any>` and `map_err` answers `Result2<any,
-- F>`, so the shape a method returns names no argument of the shape it
-- is read from. Luau compares `ResultMethods<T, E>` against another
-- copy member by member, and a return that carried `E` would make that
-- walk open `Result2` again and fail on a `T` or an `E` the two copies
-- reached by different routes. The side a map leaves alone reads `any`
-- from there on.
--
-- `unwrap_or` answers `T | D`, the way it does one rung up, so a
-- fallback of another type still names both sides. `map` and `map_err`
-- answer `any`: a return that names `Result2` again, or one that leads
-- back to `Result`, makes the solver read the next callback's parameter
-- as a free type and report `Expected type table` on code that is
-- right, which is worse than the `any`.
export type ResultMethods2<T, E> = {
	read unwrap: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> T,
	read expect: (self: { read tag: "Ok" | "Err", read _1: T | E }, message: string) -> T,
	read unwrap_or: <D>(self: { read tag: "Ok" | "Err", read _1: T | E }, default: D) -> T | D,
	read map: <U>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (T) -> U) -> any,
	read map_err: <F>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (E) -> F) -> any,
	read is_ok: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> boolean,
	read is_err: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> boolean,
	read ok: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> T?,
}

export type Result2<T, E> = ({ tag: "Ok", read _1: T, read trace: string?, read __ok: T, read __err: E } | { tag: "Err", read _1: E, read trace: string?, read __ok: T, read __err: E }) & ResultMethods2<T, E>


local Result = {}
Result.__index = Result

-- The other side is `any`, so an `Ok(1)` fits `Result<number, string>`:
-- a generic left open would be `unknown`, and Result is invariant.
local function Ok<T>(value: T): Result<T, any>
	return table.freeze(setmetatable({ tag = "Ok", _1 = value }, Result)) :: any
end

local function Err<E>(err: E, trace: string?): Result<any, E>
	return table.freeze(setmetatable({ tag = "Err", _1 = err, trace = trace }, Result)) :: any
end

function Result.unwrap(self)
	if self.tag == "Err" then
		error(if self.trace then `unwrap on Err: {self._1}\n{self.trace}` else `unwrap on Err: {self._1}`, 2)
	end
	return self._1
end

function Result.expect(self, message: string)
	if self.tag == "Err" then
		error(`{message}: {self._1}`, 2)
	end
	return self._1
end

function Result.unwrap_or(self, default)
	if self.tag == "Err" then
		return default
	end
	return self._1
end

function Result.map(self, f)
	if self.tag == "Err" then
		return self
	end
	return Ok(f(self._1))
end

function Result.map_err(self, f)
	if self.tag == "Ok" then
		return self
	end
	return Err(f(self._1), self.trace)
end

function Result.is_ok(self)
	return self.tag == "Ok"
end

function Result.is_err(self)
	return self.tag == "Err"
end

function Result.ok(self)
	if self.tag == "Ok" then
		return self._1
	end
	return nil
end

--- The one bridge to throwing APIs. The traceback is captured where the
--- error happened and stored on the Err.
function Result.pcall<T>(f: (...any) -> T, ...: any): Result<T, string>
	local packed = table.pack(xpcall(f, function(e)
		return { err = e, trace = debug.traceback(tostring(e), 2) }
	end, ...))
	if packed[1] then
		return Ok(packed[2])
	end
	local failure = packed[2]
	return Err(failure.err, failure.trace)
end

function Result.__tostring(self)
	return `{self.tag}({alloy.show(self._1)})`
end

alloy.Result = Result
alloy.Ok = Ok
alloy.Err = Err

-- `__ok` or `__err` off a Result shape. Result is a union of the two
-- tags, and each tag is a table intersected with the methods, so the
-- walk goes through both; the two tags carry the same pair, so the
-- first branch that holds the member answers. A type that is no Result
-- holds neither and gives nil.
type function __try_member(T, name)
	local want = name:value()
	local shape: any = T

	if shape:is("union") or shape:is("intersection") then
		for _, part in shape:components() do
			local found = __try_member(part, name)

			if found ~= nil then
				return found
			end
		end

		return nil
	end

	if shape:is("table") then
		for key, prop in shape:properties() do
			local k: any = key
			local p: any = prop

			if k:is("singleton") and k:value() == want then
				return p.read or p.write
			end
		end
	end

	return nil
end

-- The value type of a `try do` block, from the type its closure gives
-- back: a Result passes its payload through, and a plain value is the
-- value. A block that returns nothing leaves the type free, and `any`
-- is what fits then, since Result is invariant in both arguments.
type function __try_ok(T)
	local found = __try_member(T, types.singleton("__ok"))

	if found ~= nil then
		return found
	end

	local value: any = T

	if value:is("unknown") or value:is("never") then
		return types.any
	end

	return T
end

-- The error type of a `try do` block: the error of the Result the block
-- gives back, the type of every `fail` the block raises, or both.
-- A block that names neither can still fail, by a throw the `xpcall`
-- catches, and a throw carries any value: `any` is the answer there,
-- and it is also the one type that fits an annotation of any error,
-- since Result is invariant.
type function __try_err(T, E)
	local found = __try_member(T, types.singleton("__err"))
	local raised: any = E

	if raised:is("unknown") or raised:is("never") then
		raised = nil
	end

	if found ~= nil and raised ~= nil then
		local a: any = found
		local b: any = raised

		if a:is("any") or b:is("any") then
			return types.any
		end

		return types.unionof(found, raised)
	end

	if found ~= nil then
		return found
	end

	if raised ~= nil then
		return raised
	end

	return types.any
end

-- The escape a `try` inside a `try do` block raises. `try_block` knows
-- this table and answers with that Err; nothing else can build one, so
-- no other throw is read as one.
local TryFail = {}

local function raise(err: any, trace: string?)
	error(setmetatable({ err = err, trace = trace }, TryFail), 0)
end

--- `try do ... end`: the block's value as a Result. A plain value becomes
--- Ok, a Result passes through as is, and a throw becomes Err with the
--- traceback from the throw site.
---
--- The closure takes the block's `fail`, which a `try` inside the block
--- calls on Err. The call carries the error type, so `E` solves from
--- what the block can fail with; the call leaves the block, so the
--- block's own `return` alone decides `T`. Luau reads a closure's
--- return type off its first `return` and checks the rest against it,
--- so an Err returned there would take the place of the value.
---
--- The return type reads `...T` so a block that returns nothing still
--- solves; `T` is free then, and `__try_ok` answers `any`.
function alloy.try_block<T, E>(
	f: (fail: (E, string?) -> never) -> ...T
): Result<__try_ok<T>, __try_err<T, E>>
	local packed = table.pack(xpcall(f :: any, function(e)
		if type(e) == "table" and getmetatable(e) == TryFail then
			return e
		end

		return { err = e, trace = debug.traceback(tostring(e), 2) }
	end, raise))

	if not packed[1] then
		local failure = packed[2] :: any

		return Err(failure.err, failure.trace)
	end

	local value = packed[2]

	if type(value) == "table" and getmetatable(value) == Result then
		return value :: any
	end

	return Ok(value :: any)
end

-- ---------------------------------------------------------------------------
-- Future: a memoized task with the awaitable shape `andThen`.
-- ---------------------------------------------------------------------------

-- Every member reads only, so the type is covariant: a `Future<number>`
-- sits in a `{ Future<number | string> }`, and `race` over a mixed list
-- yields the union.
export type Future<T> = {
	-- In the type alone: a generic reads `T` off a Future it is handed.
	read __value: T,
	read andThen: (self: Future<T>, on_resolve: ((T) -> ...any)?, on_reject: ((any) -> ...any)?) -> Future<T>,
	-- The snake_case twin of `andThen`, the one camelCase name the std
	-- carries. Both name the same function.
	read and_then: (self: Future<T>, on_resolve: ((T) -> ...any)?, on_reject: ((any) -> ...any)?) -> Future<T>,
	read cancel: (self: Future<T>) -> (),
	read is_settled: (self: Future<T>) -> boolean,
}

local Future = {}
Future.__index = Future

-- The scheduler. Roblox provides `task`; the plain luau binary and the
-- test VM do not, so a coroutine fallback runs a thread at once, runs a
-- deferred call at once, and treats a delay as no wait.
local task_lib: any = task

if task_lib == nil then
	local function resume(thread: thread, ...)
		local ok, err = coroutine.resume(thread, ...)
		if not ok then
			print(err)
		end
	end
	local function spawn(f, ...)
		if type(f) == "thread" then
			resume(f, ...)
		else
			resume(coroutine.create(f), ...)
		end
	end
	task_lib = {
		spawn = spawn,
		defer = spawn,
		delay = function(_seconds: number, f, ...)
			spawn(f, ...)
		end,
		wait = function(seconds: number?)
			return seconds or 0
		end,
		cancel = function(thread: thread)
			coroutine.close(thread)
		end,
	}
end

local PENDING, RESOLVED, REJECTED, CANCELLED = 0, 1, 2, 3

local function new_future()
	return setmetatable({ state = PENDING, value = nil, trace = nil, waiters = {} }, Future)
end

local function settle(self, state, value, trace)
	if self.state ~= PENDING then
		return
	end
	self.state = state
	self.value = value
	self.trace = trace
	local waiters = self.waiters
	self.waiters = {}
	for _, w in waiters do
		task_lib.spawn(w, self)
	end
end

--- Runs `f` on a new thread and returns its Future.
-- `...T` takes a body that returns nothing as well as one that returns
-- a value, so an async function with no return type is a `Future`.
function alloy.future<T>(f: (...any) -> ...T, ...: any): Future<T>
	local self = new_future()
	local args = table.pack(...)
	task_lib.spawn(function()
		local ok, result = xpcall(function()
			return f(table.unpack(args, 1, args.n))
		end, function(e)
			return { err = e, trace = debug.traceback(tostring(e), 2) }
		end)
		if ok then
			settle(self, RESOLVED, result, nil)
		else
			settle(self, REJECTED, result.err, result.trace)
		end
	end)
	return (self :: any) :: Future<T>
end

function Future.resolve<T>(value: T): Future<T>
	local self = new_future()
	settle(self, RESOLVED, value, nil)
	return (self :: any) :: Future<T>
end

-- The payload of a rejected Future is never read, so a reject stands
-- wherever a payload is asked for. `Future<never>` would not: `andThen`
-- puts the Future in a parameter of its own, which makes the alias
-- invariant, and `never` is then no subtype of `number`.
function Future.reject(err: any): Future<any>
	local self = new_future()
	settle(self, REJECTED, err, nil)
	return (self :: any) :: Future<any>
end

function Future.delay(seconds: number): Future<nil>
	return alloy.future(function()
		task_lib.wait(seconds)
		return nil
	end)
end

-- Each receiver is annotated `Future<T>`: an inferred one would be the
-- record the runtime keeps, and `state`, `value` and `waiters` would
-- reach a hover and a completion detail.
function Future.andThen<T>(
	self: Future<T>,
	on_resolve: ((T) -> ...any)?,
	on_reject: ((any) -> ...any)?
): Future<T>
	local f = self :: any
	local function run()
		if f.state == RESOLVED and on_resolve then
			on_resolve(f.value)
		elseif f.state ~= RESOLVED and on_reject then
			on_reject(f.value)
		end
	end
	if f.state == PENDING then
		table.insert(f.waiters, run)
	else
		task_lib.spawn(run)
	end
	return self
end

--- The snake_case name for `andThen`, so a file need not carry the one
--- camelCase call the std has. It runs the same code.
function Future.and_then<T>(
	self: Future<T>,
	on_resolve: ((T) -> ...any)?,
	on_reject: ((any) -> ...any)?
): Future<T>
	return Future.andThen(self, on_resolve, on_reject)
end

function Future.cancel<T>(self: Future<T>): ()
	settle(self :: any, CANCELLED, "cancelled", nil)
end

function Future.is_settled<T>(self: Future<T>): boolean
	return (self :: any).state ~= PENDING
end

--- What `await` takes. It is `Future<T>`; the name is what the checker
--- shows for a value that is not one, and `Awaitable` says what the
--- operand has to be where the alias `Future` says only what it is.
export type Awaitable<T> = Future<T>

--- What `await` takes, read as a shape: a value with `andThen` that
--- settles with a `T`. `Future<T>` is invariant in `T`, since `andThen`
--- takes a `(T) -> ...`, so a list of `Future<number>` and
--- `Future<nil>` has no one `Future<T>` to unify with. `await` and
--- `race` bind the whole argument type and read `T` back off
--- `__value`, which is covariant, so a mixed list lands on the union
--- of the value types instead of the meet.
--- The callbacks read `any`: `alloy.await` hands `andThen` two of its
--- own and reads neither back, and a hand-written `andThen` types its
--- own callback, `(number) -> number`, which `((any) -> ...any)?` does
--- not accept.
export type Settled<T> = {
	read __value: T,
	read andThen: (self: any, on_resolve: any, on_reject: any) -> any,
}

--- `await`: a Future, or anything with `andThen`. Yields until settled,
--- returns the value, rethrows a stored error with its trace.
-- The extra values type as `any`: a `wait` on a remote settles with the
-- whole payload, and `local a, b = await f` is then no error.
function alloy.await<A>(target: A & Settled<any>): (index<A, "__value">, ...any)
	if type(target) == "table" and getmetatable(target) == Future then
		if target.state == PENDING then
			local thread = coroutine.running()
			table.insert(target.waiters, function()
				task_lib.spawn(thread)
			end)
			coroutine.yield()
		end
		if target.state == RESOLVED then
			return target.value
		end
		error(if target.trace then `{target.value}\n{target.trace}` else target.value, 2)
	end
	if type(target) == "table" and type(target.andThen) == "function" then
		local thread = coroutine.running()
		local done, ok, value = false, false, nil
		target:andThen(function(v)
			ok, value, done = true, v, true
			task_lib.spawn(thread)
		end, function(e)
			ok, value, done = false, e, true
			task_lib.spawn(thread)
		end)
		if not done then
			coroutine.yield()
		end
		if ok then
			return value
		end
		error(value, 2)
	end
	error(`cannot await a {typeof(target)}`, 2)
end

--- `try await`: a failed Future becomes an Err instead of a throw, and
--- a Result the Future settles with is the Result, not an Ok around it,
--- so `try await f()` on an async function that returns a Result yields
--- the value. The compiler calls `try_await_result` where it knows the
--- Future settles with a Result, for the type; both run the same code.
function alloy.try_await<A>(target: A & Settled<any>): Result<index<A, "__value">, any>
	local ok, value = pcall(alloy.await, target)
	if not ok then
		return Err(value) :: any
	end
	if type(value) == "table" and getmetatable(value) == Result then
		return value :: any
	end
	return Ok(value) :: any
end

--- The same call, typed for a Future that settles with a Result: the
--- Result the Future carries is the answer, not an Ok around it. The
--- arguments come off the settled value, since a generic bound to the
--- shape `{ __ok: T, __err: E }` reads `unknown` off an annotated
--- `Future<Result<T, E>>`.
function alloy.try_await_result<A>(
	target: A & Settled<any>
): Result<index<index<A, "__value">, "__ok">, index<index<A, "__value">, "__err">>
	return alloy.try_await(target) :: any
end

function Future.all<T>(futures: { Future<T> }): Future<Array<T>>
	return (alloy.future(function()
		local out = {}
		for i, f in futures do
			out[i] = alloy.await(f)
		end
		return setmetatable(out, alloy.Array)
	end) :: any) :: Future<Array<T>>
end

-- A list of one value type races to a Future of it. A mixed list needs
-- the type written on the elements, `Future<number | string>`: the
-- solver lands on the meet of the element types otherwise.
function Future.race<E>(futures: { [number]: E }): Future<index<E, "__value">>
	local winner = new_future()
	for _, f in futures do
		f:andThen(function(v)
			settle(winner, RESOLVED, v, nil)
		end, function(e)
			settle(winner, REJECTED, e, nil)
		end)
	end
	return (winner :: any) :: Future<any>
end

function Future.any<E>(futures: { [number]: E }): Future<index<E, "__value">>
	local winner = new_future()
	local failed = 0
	for _, f in futures do
		f:andThen(function(v)
			settle(winner, RESOLVED, v, nil)
		end, function(e)
			failed += 1
			if failed == #futures then
				settle(winner, REJECTED, e, nil)
			end
		end)
	end
	return (winner :: any) :: Future<any>
end

-- The element is `Result2`, the one-table Result: `Result<T, any>` here
-- is a union of two intersections under an Array under a Future, and
-- `await` on that reaches the solver's step limit. The error side is
-- `any`, since a rejection carries whatever it was rejected with.
function Future.all_settled<T>(futures: { Future<T> }): Future<Array<Result2<T, any>>>
	return (alloy.future(function()
		local out = {}
		for i, f in futures do
			out[i] = alloy.try_await(f)
		end
		return setmetatable(out, alloy.Array)
	end) :: any) :: Future<Array<Result2<T, any>>>
end

local STATE_NAMES = { [PENDING] = "pending", [RESOLVED] = "resolved", [REJECTED] = "rejected", [CANCELLED] = "cancelled" }

function Future.__tostring(self)
	if self.state == RESOLVED then
		return `Future \{ resolved = {alloy.show(self.value)} }`
	end
	return `Future \{ {STATE_NAMES[self.state] or "pending"} }`
end

alloy.Future = Future

-- ---------------------------------------------------------------------------
-- Array: a table with the shared metatable, so `xs:map(f)` works.
-- ---------------------------------------------------------------------------

export type Array<T> = {
	[number]: T,
	len: (self: { read [number]: T }) -> number,
	is_empty: (self: { read [number]: T }) -> boolean,
	push: (self: { read [number]: T }, ...T) -> Array<T>,
	pop: (self: { read [number]: T }) -> T?,
	first: (self: { read [number]: T }) -> T?,
	last: (self: { read [number]: T }) -> T?,
	map: <U>(self: { read [number]: T }, f: (T, number) -> U) -> Array2<U>,
	filter: (self: { read [number]: T }, f: (T, number) -> boolean) -> Array<T>,
	find: (self: { read [number]: T }, f: (T, number) -> boolean) -> T?,
	find_index: (self: { read [number]: T }, f: (T, number) -> boolean) -> number?,
	contains: (self: { read [number]: T }, value: T) -> boolean,
	index_of: (self: { read [number]: T }, value: T) -> number?,
	for_each: (self: { read [number]: T }, f: (T, number) -> ()) -> (),
	reduce: <U>(self: { read [number]: T }, f: (U, T, number) -> U, init: U) -> U,
	slice: (self: { read [number]: T }, from: number, to: number?) -> Array<T>,
	concat: (self: { read [number]: T }, other: Array<T>) -> Array<T>,
	reverse: (self: { read [number]: T }) -> Array<T>,
	sort_by: (self: { read [number]: T }, f: (T, T) -> boolean) -> Array<T>,
	join: (self: { read [number]: T }, sep: string?) -> string,
}

-- Luau rejects an alias that names itself with other arguments, so
-- `map` cannot return `Array<U>`. `Array2` and `Array3` are the same
-- shape one and two maps deep; a third map in a chain is `any`. Their
-- `self` is the bare table: every `self: Array2<T>` is one more
-- expansion of the alias, and the solver gives up past a few hundred.
-- A `self: any` is no good either, it turns every inferred `U` to any.
export type Array2<T> = {
	[number]: T,
	len: (self: { read [number]: T }) -> number,
	is_empty: (self: { read [number]: T }) -> boolean,
	push: (self: { read [number]: T }, ...T) -> Array2<T>,
	pop: (self: { read [number]: T }) -> T?,
	first: (self: { read [number]: T }) -> T?,
	last: (self: { read [number]: T }) -> T?,
	map: <U>(self: { read [number]: T }, f: (T, number) -> U) -> Array3<U>,
	filter: (self: { read [number]: T }, f: (T, number) -> boolean) -> Array2<T>,
	find: (self: { read [number]: T }, f: (T, number) -> boolean) -> T?,
	find_index: (self: { read [number]: T }, f: (T, number) -> boolean) -> number?,
	contains: (self: { read [number]: T }, value: T) -> boolean,
	index_of: (self: { read [number]: T }, value: T) -> number?,
	for_each: (self: { read [number]: T }, f: (T, number) -> ()) -> (),
	reduce: <U>(self: { read [number]: T }, f: (U, T, number) -> U, init: U) -> U,
	slice: (self: { read [number]: T }, from: number, to: number?) -> Array2<T>,
	concat: (self: { read [number]: T }, other: Array2<T>) -> Array2<T>,
	reverse: (self: { read [number]: T }) -> Array2<T>,
	sort_by: (self: { read [number]: T }, f: (T, T) -> boolean) -> Array2<T>,
	join: (self: { read [number]: T }, sep: string?) -> string,
}

export type Array3<T> = {
	[number]: T,
	len: (self: { read [number]: T }) -> number,
	is_empty: (self: { read [number]: T }) -> boolean,
	push: (self: { read [number]: T }, ...T) -> Array3<T>,
	pop: (self: { read [number]: T }) -> T?,
	first: (self: { read [number]: T }) -> T?,
	last: (self: { read [number]: T }) -> T?,
	map: <U>(self: { read [number]: T }, f: (T, number) -> U) -> any,
	filter: (self: { read [number]: T }, f: (T, number) -> boolean) -> Array3<T>,
	find: (self: { read [number]: T }, f: (T, number) -> boolean) -> T?,
	find_index: (self: { read [number]: T }, f: (T, number) -> boolean) -> number?,
	contains: (self: { read [number]: T }, value: T) -> boolean,
	index_of: (self: { read [number]: T }, value: T) -> number?,
	for_each: (self: { read [number]: T }, f: (T, number) -> ()) -> (),
	reduce: <U>(self: { read [number]: T }, f: (U, T, number) -> U, init: U) -> U,
	slice: (self: { read [number]: T }, from: number, to: number?) -> Array3<T>,
	concat: (self: { read [number]: T }, other: Array3<T>) -> Array3<T>,
	reverse: (self: { read [number]: T }) -> Array3<T>,
	sort_by: (self: { read [number]: T }, f: (T, T) -> boolean) -> Array3<T>,
	join: (self: { read [number]: T }, sep: string?) -> string,
}

-- `read T[]`: the Array methods that do not mutate.
export type ReadArray<T> = {
	read [number]: T,
	len: (self: { read [number]: T }) -> number,
	is_empty: (self: { read [number]: T }) -> boolean,
	first: (self: { read [number]: T }) -> T?,
	last: (self: { read [number]: T }) -> T?,
	map: <U>(self: { read [number]: T }, f: (T, number) -> U) -> Array<U>,
	filter: (self: { read [number]: T }, f: (T, number) -> boolean) -> Array<T>,
	find: (self: { read [number]: T }, f: (T, number) -> boolean) -> T?,
	find_index: (self: { read [number]: T }, f: (T, number) -> boolean) -> number?,
	contains: (self: { read [number]: T }, value: T) -> boolean,
	index_of: (self: { read [number]: T }, value: T) -> number?,
	for_each: (self: { read [number]: T }, f: (T, number) -> ()) -> (),
	reduce: <U>(self: { read [number]: T }, f: (U, T, number) -> U, init: U) -> U,
	slice: (self: { read [number]: T }, from: number, to: number?) -> Array<T>,
	concat: (self: { read [number]: T }, other: Array<T>) -> Array<T>,
	join: (self: { read [number]: T }, sep: string?) -> string,
}

-- `write T[]`: a sink. Push and index assignment only.
export type WriteArray<T> = {
	[number]: T,
	push: (self: any, ...T) -> WriteArray<T>,
}

local Array = {}
Array.__index = Array

function Array.from<T>(t: { T }): Array<T>
	return (setmetatable(t, Array) :: any) :: Array<T>
end

function Array.new<T>(): Array<T>
	return (setmetatable({}, Array) :: any) :: Array<T>
end

function Array.with_capacity<T>(n: number): Array<T>
	return (setmetatable(table.create(n), Array) :: any) :: Array<T>
end

function Array.len(xs)
	return #xs
end

function Array.is_empty(xs)
	return #xs == 0
end

function Array.push(xs, ...)
	for i = 1, select("#", ...) do
		table.insert(xs, (select(i, ...)))
	end
	return xs
end

function Array.pop(xs)
	return table.remove(xs)
end

function Array.first(xs)
	return xs[1]
end

function Array.last(xs)
	return xs[#xs]
end

function Array.map<T, U>(xs: { read [number]: T }, f: (T, number) -> U): Array<U>
	local out = table.create(#xs)
	for i, v in ipairs(xs) do
		out[i] = f(v, i)
	end
	return setmetatable(out, Array)
end

function Array.filter(xs, f)
	local out = {}
	for i, v in ipairs(xs) do
		if f(v, i) then
			table.insert(out, v)
		end
	end
	return setmetatable(out, Array)
end

function Array.find(xs, f)
	for i, v in ipairs(xs) do
		if f(v, i) then
			return v
		end
	end
	return nil
end

function Array.find_index(xs, f)
	for i, v in ipairs(xs) do
		if f(v, i) then
			return i
		end
	end
	return nil
end

function Array.contains(xs, value)
	return table.find(xs, value) ~= nil
end

function Array.index_of(xs, value)
	return table.find(xs, value)
end

function Array.for_each(xs, f)
	for i, v in ipairs(xs) do
		f(v, i)
	end
end

function Array.reduce(xs, f, init)
	local acc = init
	for i, v in ipairs(xs) do
		acc = f(acc, v, i)
	end
	return acc
end

function Array.slice<T>(xs: Array<T>, from: number, to: number?): Array<T>
	local last = to or #xs
	local out = table.move(xs, from, last, 1, {})
	return setmetatable(out, Array)
end

function Array.concat(xs, ys)
	local out = table.move(xs, 1, #xs, 1, {})
	table.move(ys, 1, #ys, #out + 1, out)
	return setmetatable(out, Array)
end

function Array.reverse(xs)
	local out = table.create(#xs)
	local n = #xs
	for i = 1, n do
		out[i] = xs[n - i + 1]
	end
	return setmetatable(out, Array)
end

function Array.sort_by(xs, f)
	table.sort(xs, f)
	return xs
end

function Array.join(xs, sep: string?)
	local parts = table.create(#xs)
	for i, v in ipairs(xs) do
		parts[i] = tostring(v)
	end
	return table.concat(parts, sep or "")
end

function Array.__tostring(self)
	return show_list(self, "[", "]")
end

alloy.Array = Array

-- ---------------------------------------------------------------------------
-- HashMap and Set: Alloy structs with methods.
-- ---------------------------------------------------------------------------

export type HashMap<K, V> = {
	get: (self: HashMap<K, V>, key: K) -> V?,
	set: (self: HashMap<K, V>, key: K, value: V) -> HashMap<K, V>,
	has: (self: HashMap<K, V>, key: K) -> boolean,
	remove: (self: HashMap<K, V>, key: K) -> V?,
	len: (self: HashMap<K, V>) -> number,
	keys: (self: HashMap<K, V>) -> Array<K>,
	values: (self: HashMap<K, V>) -> Array<V>,
	entries: (self: HashMap<K, V>) -> () -> (K?, V?),
	get_or_insert: (self: HashMap<K, V>, key: K, default: V) -> V,
	clear: (self: HashMap<K, V>) -> (),
}

local HashMap = {}
HashMap.__index = HashMap

function HashMap.new<K, V>(): HashMap<K, V>
	return (setmetatable({ _items = {}, _count = 0 }, HashMap) :: any) :: HashMap<K, V>
end

function HashMap.from<K, V>(t: { [K]: V }): HashMap<K, V>
	local self = HashMap.new()
	for k, v in pairs(t) do
		self:set(k, v)
	end
	return self
end

function HashMap.get(self, key)
	return self._items[key]
end

function HashMap.set(self, key, value)
	if self._items[key] == nil and value ~= nil then
		self._count += 1
	elseif self._items[key] ~= nil and value == nil then
		self._count -= 1
	end
	self._items[key] = value
	return self
end

function HashMap.has(self, key)
	return self._items[key] ~= nil
end

function HashMap.remove(self, key)
	local old = self._items[key]
	if old ~= nil then
		self._items[key] = nil
		self._count -= 1
	end
	return old
end

function HashMap.len(self)
	return self._count
end

function HashMap.keys(self)
	local out = {}
	for k in pairs(self._items) do
		table.insert(out, k)
	end
	return setmetatable(out, Array)
end

function HashMap.values(self)
	local out = {}
	for _, v in pairs(self._items) do
		table.insert(out, v)
	end
	return setmetatable(out, Array)
end

function HashMap.entries(self)
	local items = self._items
	local key = nil
	return function()
		local k, v = next(items, key)
		key = k
		return k, v
	end
end

function HashMap.get_or_insert(self, key, default)
	local v = self._items[key]
	if v == nil then
		self:set(key, default)
		return default
	end
	return v
end

function HashMap.clear(self)
	table.clear(self._items)
	self._count = 0
end

function HashMap.__tostring(self)
	local keys = sorted_keys(self._items)
	if #keys == 0 then
		return "HashMap {}"
	end
	local parts = table.create(#keys)
	for i, k in ipairs(keys) do
		local key = if type(k) == "string" and string.match(k, "^[%a_][%w_]*$") then k else `[{alloy.show(k)}]`
		parts[i] = key .. " = " .. alloy.show(self._items[k])
	end
	return "HashMap { " .. table.concat(parts, ", ") .. " }"
end

alloy.HashMap = HashMap

export type Set<T> = {
	add: (self: Set<T>, value: T) -> Set<T>,
	has: (self: Set<T>, value: T) -> boolean,
	remove: (self: Set<T>, value: T) -> boolean,
	len: (self: Set<T>) -> number,
	union: (self: Set<T>, other: Set<T>) -> Set<T>,
	intersection: (self: Set<T>, other: Set<T>) -> Set<T>,
	difference: (self: Set<T>, other: Set<T>) -> Set<T>,
	to_array: (self: Set<T>) -> Array<T>,
}

-- Operator traits, as the shape a bound `T: Display` names.
-- The mapped types of the language. A type function builds each shape,
-- so the analyzer sees a real table type, not an alias it cannot open.
-- The properties of a table type, or of every part of an intersection
-- merged, so a mapped type over `A & B` sees both.
type function __mapped_props(T)
	if T:is("intersection") then
		local all: { [any]: any } = {}
		for _, c in T:components() do
			for k, v in __mapped_props(c) do
				all[k] = v
			end
		end
		return all
	end
	return T:properties()
end

type function __mapped_optional(T)
	local props: { [any]: any } = {}
	for k, v in __mapped_props(T) do
		local value: any = types.unionof((v.read or v.write) :: any, types.singleton(nil))
		props[k] = { read = value, write = value }
	end
	return types.newtable(props)
end

type function __mapped_read(T)
	local props: { [any]: any } = {}
	for k, v in __mapped_props(T) do
		local value: any = (v.read or v.write) :: any
		props[k] = { read = value }
	end
	return types.newtable(props)
end

type function __mapped_write(T)
	local props: { [any]: any } = {}
	for k, v in __mapped_props(T) do
		local value: any = (v.read or v.write) :: any
		props[k] = { write = value }
	end
	return types.newtable(props)
end

--- Every field of `T`, optional: `{ [K in keyof T]: T[K]? }`.
export type Partial<T> = __mapped_optional<T>
--- Every field of `T`, read only: `{ [K in keyof T]: read T[K] }`.
export type Readonly<T> = __mapped_read<T>
--- Every field of `T`, write only: `{ [K in keyof T]: write T[K] }`.
export type Sink<T> = __mapped_write<T>

export type Display = { read to_string: (self: any) -> string }
export type Debug = { read debug: (self: any) -> string }
export type Clone<T = any> = { read clone: (self: any) -> T }
export type Eq = { read eq: (self: any, other: any) -> boolean }
export type PartialEq = Eq
export type Ord = { read lt: (self: any, other: any) -> boolean, read le: (self: any, other: any) -> boolean }
export type Add<T = any> = { read add: (self: any, other: any) -> T }
export type Sub<T = any> = { read sub: (self: any, other: any) -> T }
export type Mul<T = any> = { read mul: (self: any, other: any) -> T }
export type Div<T = any> = { read div: (self: any, other: any) -> T }
export type Serialize = { read serialize: (self: any) -> any }

local Set = {}
Set.__index = Set

function Set.new<T>(): Set<T>
	return (setmetatable({ _items = {}, _count = 0 }, Set) :: any) :: Set<T>
end

function Set.from<T>(t: { T }): Set<T>
	local self = Set.new()
	for _, v in ipairs(t) do
		self:add(v)
	end
	return self
end

function Set.add(self, value)
	if not self._items[value] then
		self._items[value] = true
		self._count += 1
	end
	return self
end

function Set.has(self, value)
	return self._items[value] == true
end

function Set.remove(self, value)
	if self._items[value] then
		self._items[value] = nil
		self._count -= 1
		return true
	end
	return false
end

function Set.len(self)
	return self._count
end

function Set.union(self, other)
	local out = Set.new()
	for v in pairs(self._items) do
		out:add(v)
	end
	for v in pairs(other._items) do
		out:add(v)
	end
	return out
end

function Set.intersection(self, other)
	local out = Set.new()
	for v in pairs(self._items) do
		if other._items[v] then
			out:add(v)
		end
	end
	return out
end

function Set.difference(self, other)
	local out = Set.new()
	for v in pairs(self._items) do
		if not other._items[v] then
			out:add(v)
		end
	end
	return out
end

function Set.to_array(self)
	local out = {}
	for v in pairs(self._items) do
		table.insert(out, v)
	end
	return setmetatable(out, Array)
end

function Set.__tostring(self)
	return show_list(sorted_keys(self._items), "Set {", "}")
end

alloy.Set = Set

-- ---------------------------------------------------------------------------
-- Symbol: a unique key that no string can collide with.
-- ---------------------------------------------------------------------------

local Symbol = {}

function Symbol.new(name: string)
	return table.freeze(setmetatable({}, {
		__tostring = function()
			return `Symbol({name})`
		end,
	}))
end

alloy.Symbol = Symbol

-- ---------------------------------------------------------------------------
-- Signal: a typed signal with the shape of RBXScriptSignal, so code that
-- takes one takes the other. Handlers run in connection order, each on a
-- reused thread, and a handler may disconnect any connection during a fire.
-- ---------------------------------------------------------------------------

export type SignalConnection = {
	Connected: boolean,
	Disconnect: (self: SignalConnection) -> (),
	-- The snake_case twin the runtime writes beside every method, for
	-- code that keeps Alloy's own naming.
	disconnect: (self: SignalConnection) -> (),
}

export type Signal<T...> = {
	Connect: (self: Signal<T...>, handler: (T...) -> ()) -> SignalConnection,
	Once: (self: Signal<T...>, handler: (T...) -> ()) -> SignalConnection,
	Wait: (self: Signal<T...>, timeout: number?) -> T...,
	Fire: (self: Signal<T...>, T...) -> (),
	FireDeferred: (self: Signal<T...>, T...) -> (),
	DisconnectAll: (self: Signal<T...>) -> (),
	Destroy: (self: Signal<T...>) -> (),
	-- The snake_case twins the runtime writes beside the Roblox names.
	connect: (self: Signal<T...>, handler: (T...) -> ()) -> SignalConnection,
	once: (self: Signal<T...>, handler: (T...) -> ()) -> SignalConnection,
	wait: (self: Signal<T...>, timeout: number?) -> T...,
	fire: (self: Signal<T...>, T...) -> (),
	fire_deferred: (self: Signal<T...>, T...) -> (),
	disconnect_all: (self: Signal<T...>) -> (),
	destroy: (self: Signal<T...>) -> (),
}

--- Anything with `Connect` or `connect`: a Signal, an RBXScriptSignal, or a
--- third party signal.
export type Signalish<T...> = RBXScriptSignal<T...> | Signal<T...> | {
	Connect: (self: any, handler: (T...) -> ()) -> any,
} | {
	connect: (self: any, handler: (T...) -> ()) -> any,
}

local Connection = {}
Connection.__index = Connection

function Connection.Disconnect(self)
	if not self.Connected then
		return
	end
	self.Connected = false
	local signal = self._signal
	if signal._head == self then
		signal._head = self._next
	else
		local prev = signal._head
		while prev ~= nil and prev._next ~= self do
			prev = prev._next
		end
		if prev ~= nil then
			prev._next = self._next
		end
	end
	if signal._tail == self then
		signal._tail = nil
		local last = signal._head
		while last ~= nil and last._next ~= nil do
			last = last._next
		end
		signal._tail = last
	end
end

Connection.disconnect = Connection.Disconnect

local Signal = {}
Signal.__index = Signal

-- One idle thread runs every handler. A handler that yields keeps the
-- thread, and the next fire makes another. `task.spawn` reports a handler
-- error to the console; without a scheduler the error goes to `warn`.
local free_thread: thread? = nil

local function run_handler(handler, ...)
	local thread = free_thread
	free_thread = nil
	handler(...)
	free_thread = thread
end

local function runner(...)
	run_handler(...)
	while true do
		run_handler(coroutine.yield())
	end
end

local function spawn_handler(handler, ...)
	local thread = free_thread
	if thread == nil then
		thread = coroutine.create(runner)
	end
	if task_lib ~= nil then
		task_lib.spawn(thread :: thread, handler, ...)
	else
		local ok, err = coroutine.resume(thread :: thread, handler, ...)
		if not ok then
			(warn or print)(err)
		end
	end
end

function Signal.new<T...>(): Signal<T...>
	return (setmetatable({ _head = nil, _tail = nil, _source = nil }, Signal) :: any) :: Signal<T...>
end

--- True for a Signal from this module, not a Roblox signal.
function Signal.is(value: any): boolean
	return type(value) == "table" and getmetatable(value) == Signal
end

function Signal.Connect(self, handler)
	local connection = setmetatable({
		Connected = true,
		_signal = self,
		_handler = handler,
		_next = nil,
	}, Connection)
	if self._tail ~= nil then
		self._tail._next = connection
	else
		self._head = connection
	end
	self._tail = connection
	return connection
end

function Signal.Once(self, handler)
	local connection
	connection = Signal.Connect(self, function(...)
		connection:Disconnect()
		handler(...)
	end)
	return connection
end

function Signal.Wait(self, timeout)
	local thread = coroutine.running()
	local settled = false
	local connection
	connection = Signal.Connect(self, function(...)
		if settled then
			return
		end
		settled = true
		connection:Disconnect()
		if task_lib ~= nil then
			task_lib.spawn(thread, ...)
		else
			coroutine.resume(thread, ...)
		end
	end)
	if timeout ~= nil and task_lib ~= nil then
		task_lib.delay(timeout, function()
			if settled then
				return
			end
			settled = true
			connection:Disconnect()
			task_lib.spawn(thread)
		end)
	end
	return coroutine.yield()
end

function Signal.Fire(self, ...)
	local item = self._head
	while item ~= nil do
		if item.Connected then
			spawn_handler(item._handler, ...)
		end
		item = item._next
	end
end

--- Fires on the next scheduler step, so every handler sees the state after
--- the caller finishes. Fires at once without a scheduler.
function Signal.FireDeferred(self, ...)
	if task_lib ~= nil then
		task_lib.defer(Signal.Fire, self, ...)
	else
		Signal.Fire(self, ...)
	end
end

function Signal.DisconnectAll(self)
	local item = self._head
	while item ~= nil do
		item.Connected = false
		item = item._next
	end
	self._head = nil
	self._tail = nil
end

function Signal.Destroy(self)
	Signal.DisconnectAll(self)
	if self._source ~= nil then
		self._source:Disconnect()
		self._source = nil
	end
end

Signal.connect = Signal.Connect
Signal.once = Signal.Once
Signal.wait = Signal.Wait
Signal.fire = Signal.Fire
Signal.fire_deferred = Signal.FireDeferred
Signal.disconnect_all = Signal.DisconnectAll
Signal.destroy = Signal.Destroy

local function connect_any(source, handler)
	local connect = source.Connect or source.connect
	if connect == nil then
		error("Signal: the source has no Connect or connect", 3)
	end
	return connect(source, handler)
end

--- A Signal that fires whenever the source fires. `Destroy` disconnects
--- from the source.
function Signal.wrap<T...>(source: Signalish<T...>): Signal<T...>
	local signal = Signal.new()
	;(signal :: any)._source = connect_any(source :: any, function(...)
		Signal.Fire(signal :: any, ...)
	end)
	return signal
end

--- An iterator over the events a source fired since the last drain, in
--- order, plus the connection that feeds it. A system reads them once per
--- step: `for id, value in Signal.collect(changed) do`. The loop stops at
--- an event whose first value is nil, as any iterator does.
---
--- The iterator takes `...any` and reads none of it. A generic `for`
--- calls its iterator with the state and the control value, so one that
--- took `()` would not fit the loop the line above writes.
function Signal.collect<T...>(source: Signalish<T...>): ((...any) -> T..., SignalConnection)
	local queue: { any } = {}
	local head = 1
	local tail = 0
	local connection = connect_any(source :: any, function(...)
		tail += 1
		queue[tail] = table.pack(...)
	end)
	local function next_event()
		if head > tail then
			return
		end
		local event = queue[head]
		queue[head] = nil
		head += 1
		return table.unpack(event, 1, event.n)
	end
	return (next_event :: any) :: (...any) -> T..., (connection :: any) :: SignalConnection
end

alloy.Signal = Signal

-- ---------------------------------------------------------------------------
-- Queue, Heap, Scope, Iter: what a game loop reaches for next.
-- ---------------------------------------------------------------------------

--- A first-in, first-out queue over a ring of indices. `push` appends,
--- `pop` takes the oldest. Iterating reads front to back without a pop.
export type Queue<T> = {
	push: (self: Queue<T>, value: T) -> (),
	pop: (self: Queue<T>) -> T?,
	peek: (self: Queue<T>) -> T?,
	len: (self: Queue<T>) -> number,
	is_empty: (self: Queue<T>) -> boolean,
	clear: (self: Queue<T>) -> (),
	to_array: (self: Queue<T>) -> Array<T>,
}

local Queue = {}
Queue.__index = Queue

function Queue.new<T>(): Queue<T>
	return (setmetatable({ _items = {}, _head = 1, _tail = 0 }, Queue) :: any) :: Queue<T>
end

function Queue.from<T>(t: { T }): Queue<T>
	local self = Queue.new()
	for _, v in ipairs(t) do
		self:push(v)
	end
	return self
end

function Queue.push(self, value)
	self._tail += 1
	self._items[self._tail] = value
end

function Queue.pop(self)
	if self._head > self._tail then
		return nil
	end
	local value = self._items[self._head]
	self._items[self._head] = nil
	self._head += 1
	-- An empty queue starts its indices over, so they never run away.
	if self._head > self._tail then
		self._head = 1
		self._tail = 0
	end
	return value
end

function Queue.peek(self)
	return self._items[self._head]
end

function Queue.len(self)
	return self._tail - self._head + 1
end

function Queue.is_empty(self)
	return self._head > self._tail
end

function Queue.clear(self)
	self._items = {}
	self._head = 1
	self._tail = 0
end

function Queue.to_array(self)
	local out = {}
	for i = self._head, self._tail do
		table.insert(out, self._items[i])
	end
	return setmetatable(out, Array)
end

function Queue.__iter(self)
	local i = self._head - 1
	return function()
		i += 1
		if i > self._tail then
			return nil
		end
		return self._items[i]
	end
end

function Queue.__tostring(self)
	return show_list(self:to_array(), "Queue [", "]")
end

alloy.Queue = Queue

--- A binary heap. `pop` yields the least value under `less`, which
--- defaults to `<`; pass one for tables: `Heap.new(function(a, b) return
--- a.cost < b.cost end)`. Iterating pops nothing; `to_array` sorts.
export type Heap<T> = {
	-- In the type alone, as `Result.__ok` is: `Queue<T>` has the same
	-- members, and without this the checker prints one name for both.
	read __less: (T, T) -> boolean,
	push: (self: Heap<T>, value: T) -> (),
	pop: (self: Heap<T>) -> T?,
	peek: (self: Heap<T>) -> T?,
	len: (self: Heap<T>) -> number,
	is_empty: (self: Heap<T>) -> boolean,
	clear: (self: Heap<T>) -> (),
	to_array: (self: Heap<T>) -> Array<T>,
}

local Heap = {}
Heap.__index = Heap

local function heap_default_less(a, b)
	return a < b
end

function Heap.new<T>(less: ((T, T) -> boolean)?): Heap<T>
	return (setmetatable({ _items = {}, _less = less or heap_default_less }, Heap) :: any) :: Heap<T>
end

function Heap.from<T>(t: { T }, less: ((T, T) -> boolean)?): Heap<T>
	local self = Heap.new(less)
	for _, v in ipairs(t) do
		self:push(v)
	end
	return self
end

function Heap.push(self, value)
	local items, less = self._items, self._less
	table.insert(items, value)
	local i = #items
	while i > 1 do
		local parent = i // 2
		if not less(items[i], items[parent]) then
			break
		end
		items[i], items[parent] = items[parent], items[i]
		i = parent
	end
end

function Heap.pop(self)
	local items, less = self._items, self._less
	local n = #items
	if n == 0 then
		return nil
	end
	local top = items[1]
	items[1] = items[n]
	items[n] = nil
	n -= 1
	local i = 1
	while true do
		local left, right = i * 2, i * 2 + 1
		local smallest = i
		if left <= n and less(items[left], items[smallest]) then
			smallest = left
		end
		if right <= n and less(items[right], items[smallest]) then
			smallest = right
		end
		if smallest == i then
			break
		end
		items[i], items[smallest] = items[smallest], items[i]
		i = smallest
	end
	return top
end

function Heap.peek(self)
	return self._items[1]
end

function Heap.len(self)
	return #self._items
end

function Heap.is_empty(self)
	return #self._items == 0
end

function Heap.clear(self)
	self._items = {}
end

function Heap.to_array(self)
	local out = table.clone(self._items)
	table.sort(out, self._less)
	return setmetatable(out, Array)
end

function Heap.__iter(self)
	local sorted = self:to_array()
	local i = 0
	return function()
		i += 1
		return sorted[i]
	end
end

function Heap.__tostring(self)
	return show_list(self:to_array(), "Heap [", "]")
end

alloy.Heap = Heap

-- What a Scope holds for an Instance, keyed weakly by the Instance:
-- `scope:add(conn, part)` records the pair. `delete part` cleans the
-- pairs before the Instance goes away, so no handler runs against an
-- Instance that is half gone.
local owned = setmetatable({}, { __mode = "k" })

local function drop_owned(owner: any)
	local items = owned[owner]
	if items == nil then
		return
	end
	owned[owner] = nil
	for i = #items, 1, -1 do
		local entry = items[i]
		-- A scope that no longer holds the item already cleaned it.
		if entry.scope:remove(entry.item) then
			alloy.delete(entry.item)
		end
	end
end

--- A cleanup bag. `add` takes anything `delete` accepts, or a function,
--- and gives it back; `clean` runs them newest first and empties the
--- bag. A Scope has `Destroy`, so `delete scope` cleans it, and a scope
--- may hold another scope. The second argument of `add` names the
--- Instance the item belongs to: `delete` on that Instance cleans the
--- item first.
export type Scope = {
	add: <T>(self: Scope, item: T, owner: Instance?) -> T,
	remove: (self: Scope, item: any) -> boolean,
	clean: (self: Scope) -> (),
	extend: (self: Scope) -> Scope,
	len: (self: Scope) -> number,
	Destroy: (self: Scope) -> (),
}

local Scope = {}
Scope.__index = Scope

function Scope.new(): Scope
	return (setmetatable({ _items = {} }, Scope) :: any) :: Scope
end

function Scope.add(self, item, owner)
	table.insert(self._items, item)
	if owner ~= nil then
		local items = owned[owner]
		if items == nil then
			items = {}
			owned[owner] = items
		end
		table.insert(items, { scope = self, item = item })
	end
	return item
end

function Scope.remove(self, item)
	local at = table.find(self._items, item)
	if at == nil then
		return false
	end
	table.remove(self._items, at)
	return true
end

function Scope.clean(self)
	local items = self._items
	self._items = {}
	for i = #items, 1, -1 do
		local item = items[i]
		if type(item) == "function" then
			item()
		else
			alloy.delete(item)
		end
	end
end

function Scope.extend(self)
	return self:add(Scope.new())
end

function Scope.len(self)
	return #self._items
end

Scope.Destroy = Scope.clean

function Scope.__tostring(self)
	local n = #self._items
	return `Scope \{ {n} {if n == 1 then "item" else "items"} }`
end

alloy.Scope = Scope

--- A lazy iterator: a `next` that yields the next value or nil, with the
--- chain methods on top. Nothing runs until `collect`, `for_each`, or a
--- `for` loop pulls. `Iter.from` takes an array, a function, or anything
--- with `__iter`; `Iter.range(a, b, step)` counts.
export type Iter<T> = {
	next: (self: Iter<T>) -> T?,
	map: <U>(self: Iter<T>, f: (T) -> U) -> Iter2<U>,
	filter: (self: Iter<T>, f: (T) -> boolean) -> Iter<T>,
	take: (self: Iter<T>, n: number) -> Iter<T>,
	skip: (self: Iter<T>, n: number) -> Iter<T>,
	take_while: (self: Iter<T>, f: (T) -> boolean) -> Iter<T>,
	chain: (self: Iter<T>, other: Iter<T>) -> Iter<T>,
	collect: (self: Iter<T>) -> Array<T>,
	for_each: (self: Iter<T>, f: (T) -> ()) -> (),
	count: (self: Iter<T>) -> number,
	any: (self: Iter<T>, f: (T) -> boolean) -> boolean,
	all: (self: Iter<T>, f: (T) -> boolean) -> boolean,
	find: (self: Iter<T>, f: (T) -> boolean) -> T?,
	reduce: <U>(self: Iter<T>, f: (U, T) -> U, init: U) -> U,
	first: (self: Iter<T>) -> T?,
	last: (self: Iter<T>) -> T?,
}

-- `Iter2` and `Iter3`: the same shape one and two maps deep, for the
-- reason `Array2` gives.
export type Iter2<T> = {
	next: (self: { next: (self: any) -> T? }) -> T?,
	map: <U>(self: { next: (self: any) -> T? }, f: (T) -> U) -> Iter3<U>,
	filter: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> Iter2<T>,
	take: (self: { next: (self: any) -> T? }, n: number) -> Iter2<T>,
	skip: (self: { next: (self: any) -> T? }, n: number) -> Iter2<T>,
	take_while: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> Iter2<T>,
	chain: (self: { next: (self: any) -> T? }, other: Iter2<T>) -> Iter2<T>,
	collect: (self: { next: (self: any) -> T? }) -> Array<T>,
	for_each: (self: { next: (self: any) -> T? }, f: (T) -> ()) -> (),
	count: (self: { next: (self: any) -> T? }) -> number,
	any: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> boolean,
	all: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> boolean,
	find: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> T?,
	reduce: <U>(self: { next: (self: any) -> T? }, f: (U, T) -> U, init: U) -> U,
	first: (self: { next: (self: any) -> T? }) -> T?,
	last: (self: { next: (self: any) -> T? }) -> T?,
}

export type Iter3<T> = {
	next: (self: { next: (self: any) -> T? }) -> T?,
	map: <U>(self: { next: (self: any) -> T? }, f: (T) -> U) -> any,
	filter: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> Iter3<T>,
	take: (self: { next: (self: any) -> T? }, n: number) -> Iter3<T>,
	skip: (self: { next: (self: any) -> T? }, n: number) -> Iter3<T>,
	take_while: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> Iter3<T>,
	chain: (self: { next: (self: any) -> T? }, other: Iter3<T>) -> Iter3<T>,
	collect: (self: { next: (self: any) -> T? }) -> Array<T>,
	for_each: (self: { next: (self: any) -> T? }, f: (T) -> ()) -> (),
	count: (self: { next: (self: any) -> T? }) -> number,
	any: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> boolean,
	all: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> boolean,
	find: (self: { next: (self: any) -> T? }, f: (T) -> boolean) -> T?,
	reduce: <U>(self: { next: (self: any) -> T? }, f: (U, T) -> U, init: U) -> U,
	first: (self: { next: (self: any) -> T? }) -> T?,
	last: (self: { next: (self: any) -> T? }) -> T?,
}

local Iter = {}
Iter.__index = Iter

local function iter_of(next)
	return setmetatable({ _next = next }, Iter)
end

function Iter.from<T>(source: { read [number]: T } | (() -> T?) | Set<T> | Queue<T> | Heap<T> | HashMap<any, T>): Iter<T>
	if type(source) == "function" then
		return (iter_of(source) :: any) :: Iter<T>
	end
	local mt = getmetatable(source)
	if mt == Iter then
		return source
	end
	if type(mt) == "table" and mt.__iter ~= nil then
		local f, s, c = mt.__iter(source)
		return (iter_of(function()
			local value = f(s, c)
			c = value
			return value
		end) :: any) :: Iter<T>
	end
	local i = 0
	return (iter_of(function()
		i += 1
		return source[i]
	end) :: any) :: Iter<T>
end

function Iter.range(from: number, to: number, step: number?): Iter<number>
	local by = step or 1
	local at = from - by
	return (iter_of(function()
		at += by
		if (by > 0 and at > to) or (by < 0 and at < to) then
			return nil
		end
		return at
	end) :: any) :: Iter<number>
end

function Iter.next(self)
	return self._next()
end

function Iter.map(self, f)
	local inner = self._next
	return iter_of(function()
		local value = inner()
		if value == nil then
			return nil
		end
		return f(value)
	end)
end

function Iter.filter(self, f)
	local inner = self._next
	return iter_of(function()
		while true do
			local value = inner()
			if value == nil or f(value) then
				return value
			end
		end
	end)
end

function Iter.take(self, n)
	local inner = self._next
	local left = n
	return iter_of(function()
		if left <= 0 then
			return nil
		end
		left -= 1
		return inner()
	end)
end

function Iter.skip(self, n)
	local inner = self._next
	local left = n
	return iter_of(function()
		while left > 0 do
			left -= 1
			if inner() == nil then
				return nil
			end
		end
		return inner()
	end)
end

function Iter.take_while(self, f)
	local inner = self._next
	local done = false
	return iter_of(function()
		if done then
			return nil
		end
		local value = inner()
		if value == nil or not f(value) then
			done = true
			return nil
		end
		return value
	end)
end

function Iter.chain(self, other)
	local first, second = self._next, other._next
	local on_second = false
	return iter_of(function()
		if not on_second then
			local value = first()
			if value ~= nil then
				return value
			end
			on_second = true
		end
		return second()
	end)
end

function Iter.collect(self)
	local out = {}
	for value in self do
		table.insert(out, value)
	end
	return setmetatable(out, Array)
end

function Iter.for_each(self, f)
	for value in self do
		f(value)
	end
end

function Iter.count(self)
	local n = 0
	for _ in self do
		n += 1
	end
	return n
end

function Iter.any(self, f)
	for value in self do
		if f(value) then
			return true
		end
	end
	return false
end

function Iter.all(self, f)
	for value in self do
		if not f(value) then
			return false
		end
	end
	return true
end

function Iter.find(self, f)
	for value in self do
		if f(value) then
			return value
		end
	end
	return nil
end

function Iter.reduce(self, f, init)
	local acc = init
	for value in self do
		acc = f(acc, value)
	end
	return acc
end

function Iter.first(self)
	return self._next()
end

function Iter.last(self)
	local last = nil
	for value in self do
		last = value
	end
	return last
end

function Iter.__iter(self)
	return self._next
end

function Iter.__tostring()
	return "Iter { lazy }"
end

alloy.Iter = Iter

-- ---------------------------------------------------------------------------
-- Operators and sugar.
-- ---------------------------------------------------------------------------

--- What `in` accepts on its right: a string read by substring, or a
--- table read by key, which covers an Array, a Set, a HashMap, a struct,
--- and a record. A number or a boolean has nothing to search.
export type Container = string | {}

--- `x in t`, dispatched on the container.
function alloy.contains(container: Container, value: any): boolean
	if type(container) == "string" then
		return string.find(container :: string, tostring(value), 1, true) ~= nil
	end
	local mt = getmetatable(container)
	if mt == Array then
		return table.find(container, value) ~= nil
	elseif mt == Set or mt == HashMap then
		return container:has(value)
	end
	return container[value] ~= nil
end

--- `{ ...a, ...b }`: a shallow merge, left to right, keeping the metatable
--- of the first table.
function alloy.spread(...)
	local first = ...
	local out = if type(first) == "table" then table.clone(first) else {}
	for i = 2, select("#", ...) do
		local t = select(i, ...)
		if t ~= nil then
			for k, v in pairs(t) do
				out[k] = v
			end
		end
	end
	return out
end

--- `obj:method` with no call: a closure that keeps `obj` as `self`.
function alloy.bind(obj, method)
	return function(...)
		return method(obj, ...)
	end
end

--- `$dbg(expr)`: prints the source and the value, returns the value.
function alloy.dbg(where: string, source: string, value)
	print(`{where}: {source} = {value}`)
	return value
end

--- `$assert_eq(a, b)`.
function alloy.assert_eq(where: string, left_source: string, left, right_source: string, right)
	if left ~= right then
		error(`{where}: assertion failed: {left_source} == {right_source}\n  left:  {left}\n  right: {right}`, 2)
	end
end

--- The object initializer: `new X(args) { ... }` assigns each field in order.
function alloy.init<T>(object: T, fields: { [string]: any }): T
	for k, v in pairs(fields) do
		(object :: any)[k] = v
	end
	return object
end

--- `new Name { fields }` on a name this file did not declare: an
--- imported Alloy struct constructs through its class call, which sets
--- the metatable and the defaults; any other class constructs through
--- its `new` and takes the fields after.
function alloy.construct(class, fields)
	if type(class) == "table" then
		local mt = getmetatable(class)
		if rawget(class, "__index") == class and type(mt) == "table" and mt.__call ~= nil then
			return class(fields)
		end
		if type(class.new) == "function" then
			return alloy.init(class.new(), fields)
		end
	end
	error(`new: {tostring(class)} has no constructor`, 2)
end

-- ---------------------------------------------------------------------------
-- Extension impls on foreign types: a registry keyed by runtime kind.
-- ---------------------------------------------------------------------------

local impls: { [string]: { [string]: any } } = {}
local class_cache: { [string]: { [string]: any }? } = {}

--- The table an `impl Foreign` fills with its methods.
function alloy.impl_for(kind: string)
	local t = impls[kind]
	if not t then
		t = {}
		impls[kind] = t
	end
	table.clear(class_cache)
	return t
end

local function impl_for_value(value)
	local kind = typeof(value)
	if kind == "Instance" then
		local class = value.ClassName
		local cached = class_cache[class]
		if cached ~= nil then
			return cached or nil
		end
		-- The most specific registered class wins.
		local found = nil
		for name, t in pairs(impls) do
			if value:IsA(name) and (found == nil or found ~= t) then
				found = t
			end
		end
		class_cache[class] = found or false
		return found
	end
	if kind == "table" then
		local mt = getmetatable(value)
		if type(mt) == "table" and mt.__name then
			return impls[mt.__name]
		end
		return impls["table"]
	end
	return impls[kind]
end

--- `x:name(...)` where `name` is an extension method: the impl for the
--- receiver's kind, else the receiver's own method.
--- The receiver is `any`: the dispatcher reads the value's own kind, and
--- the check artifact routes a name two primitives both declare here.
function alloy.call(receiver: any, name: string, ...: any): ...any
	local t = impl_for_value(receiver)
	local f = t and t[name]
	if f then
		return f(receiver, ...)
	end
	return receiver[name](receiver, ...)
end

--- `Foreign.static(...)`.
function alloy.static(kind: string, name: string)
	local t = impls[kind]
	local f = t and t[name]
	if not f then
		error(`{kind}.{name} is not defined`, 2)
	end
	return f
end

-- ---------------------------------------------------------------------------
-- Attributes: declared metadata, readable at runtime.
-- ---------------------------------------------------------------------------

local attached = setmetatable({}, { __mode = "k" })

-- The type argument is the shape of the arguments an attribute takes,
-- `{ min: number, max: number }` for `range(min, max)`; the compiler
-- casts each declaration to it, and a read comes back typed.
export type Attribute<T = any> = { name: string, targets: { string }, params: { string }, read value: T }

function alloy.attribute(name: string, targets, params)
	return table.freeze({ name = name, targets = targets, params = params })
end

--- Attributes on a struct or enum, from its declaration.
function alloy.attrs(target: any, data: any)
	target.__attrs = data
	return target
end

--- Attributes on a function value.
function alloy.attach(f, data)
	attached[f] = data
	return f
end

local Attributes = {}

-- The emit stores the arguments in declaration order; the attribute
-- value names them, so a reader sees `{ min = 0, max = 100 }`.
local function lookup(data, attr)
	if data == nil then
		return nil
	end
	local value = data[attr.name]
	if type(value) ~= "table" or type(attr.params) ~= "table" then
		return value
	end
	local named = {}
	for i, param in ipairs(attr.params) do
		named[param] = value[i]
	end
	return named
end

function Attributes.get<T>(target: any, attr: Attribute<T>): T?
	if type(target) == "function" then
		return lookup(attached[target], attr)
	end
	local a = type(target) == "table" and rawget(target, "__attrs")
	return lookup(a and a.own, attr)
end

function Attributes.field<T>(struct: any, field: string, attr: Attribute<T>): T?
	local a = type(struct) == "table" and rawget(struct, "__attrs")
	return lookup(a and a.fields and a.fields[field], attr)
end

function Attributes.variant<T>(enum: any, variant: string, attr: Attribute<T>): T?
	local a = type(enum) == "table" and rawget(enum, "__attrs")
	return lookup(a and a.variants and a.variants[variant], attr)
end

function Attributes.of(target: any): any
	if type(target) == "function" then
		return attached[target] or {}
	end
	local a = type(target) == "table" and rawget(target, "__attrs")
	return (a and a.own) or {}
end

--- The fields of a struct that carry an attribute, each with its value.
function Attributes.fields<T>(struct: any, attr: Attribute<T>): { [string]: T }
	local a = type(struct) == "table" and rawget(struct, "__attrs")
	local fields = (a and a.fields) or {}
	local out: { [string]: any } = {}
	for name, data in pairs(fields) do
		local v = lookup(data, attr)
		if v ~= nil then
			out[name] = v
		end
	end
	return out
end

alloy.Attributes = Attributes

-- ---------------------------------------------------------------------------
-- Tests: registered by the check artifact, run by `alloy test`.
-- ---------------------------------------------------------------------------

local tests = {}

function alloy.test(name: string, f)
	table.insert(tests, { name = name, run = f })
	return f
end

--- What `delete` accepts: an Instance, a connection, or any table with
--- one of the four methods.
export type Deletable =
	{ read Disconnect: (self: any) -> () }
	| { read disconnect: (self: any) -> () }
	| { read Destroy: (self: any) -> () }
	| { read destroy: (self: any) -> () }

--- What `destroy` accepts: an Instance, or a value with a destroy
--- method. A connection and a thread are not destroyed; `delete` takes
--- those.
export type Destroyable =
	Instance
	| { read destroy: (self: any) -> () }
	| { read Destroy: (self: any) -> () }

--- `delete x`. A connection disconnects, anything else destroys; the
--- Roblox spelling wins over the lowercase one, so a user class with
--- either spelling works. An Instance is asked for nothing it lacks,
--- since indexing a missing member throws on it.
function alloy.delete(target: Deletable)
	-- A handler that runs while an Instance goes away reads a parent
	-- that is already nil. What a scope holds for the value goes
	-- first, so no handler sees the half state.
	drop_owned(target)
	local kind = typeof(target)
	if kind == "Instance" then
		(target :: any):Destroy()
		return
	end
	if kind == "RBXScriptConnection" then
		(target :: any):Disconnect()
		return
	end
	-- A thread a scope holds, from `task.spawn`: cancel is its delete.
	if kind == "thread" then
		task_lib.cancel(target :: any)
		return
	end
	if kind == "function" then
		(target :: any)()
		return
	end
	if kind == "table" then
		local t = target :: any
		if t.Disconnect ~= nil then
			t:Disconnect()
		elseif t.disconnect ~= nil then
			t:disconnect()
		elseif t.Destroy ~= nil then
			t:Destroy()
		elseif t.destroy ~= nil then
			t:destroy()
		else
			error("delete: the value has no Disconnect, disconnect, Destroy, or destroy method", 2)
		end
		return
	end
	error(`delete: cannot delete a {kind}`, 2)
end

--- The destroy method a value carries, `Destroy` or `destroy`. The
--- Roblox spelling wins, so a class with both keeps its own.
local function destroy_method(target: any): string?
	if target.Destroy ~= nil then
		return "Destroy"
	end
	if target.destroy ~= nil then
		return "destroy"
	end
	return nil
end

--- `destroy x`. It calls the one method and does nothing else: no
--- disconnect, no cancel, no call of a function.
function alloy.destroy(target: Destroyable)
	drop_owned(target)
	local kind = typeof(target)
	if kind == "Instance" then
		(target :: any):Destroy()
		return
	end
	if kind == "table" then
		local t = target :: any
		local method = destroy_method(t)
		if method == nil then
			error("destroy: the value has no Destroy or destroy method", 2)
		end
		t[method](t)
		return
	end
	error(`destroy: cannot destroy a {kind}`, 2)
end

--- `destroy x after n` where the file does not say what `x` is. An
--- Instance goes to Debris, which outlives the script that scheduled
--- it; anything else waits on a timer.
function alloy.destroy_after(target: Destroyable, seconds: number)
	if typeof(target) == "Instance" then
		game:GetService("Debris"):AddItem(target :: any, seconds)
		return
	end
	task_lib.delay(seconds, function()
		alloy.destroy(target)
	end)
end

function alloy.tests()
	return tests
end

-- ---------------------------------------------------------------------------
-- Remotes: one RemoteEvent or RemoteFunction per declaration. This is the
-- plain transport; the buffered, batched one replaces it later behind the
-- same surface.
-- ---------------------------------------------------------------------------

-- The services resolve on first use, so the module loads where `game`
-- does not exist, such as the std tests under the plain luau binary.
local function run_service()
	return game:GetService("RunService")
end

local function replicated_storage()
	return game:GetService("ReplicatedStorage")
end

-- ---------------------------------------------------------------------------
-- `@cfg`: the conditions a build cannot decide, read when the code runs.
-- ---------------------------------------------------------------------------

local testing = false

--- `@cfg(server)` and the rest. Each asks RunService when it runs, so
--- the module loads where `game` does not exist.
alloy.cfg = {
	server = function(): boolean
		return run_service():IsServer()
	end,
	client = function(): boolean
		return run_service():IsClient()
	end,
	studio = function(): boolean
		return run_service():IsStudio()
	end,
	edit = function(): boolean
		return run_service():IsEdit()
	end,
	running = function(): boolean
		return run_service():IsRunning()
	end,
	test = function(): boolean
		return testing
	end,
}

--- `alloy test` marks the run, so `@cfg(test)` holds in a spec.
function alloy.set_testing(value: boolean)
	testing = value
end

local function remote_instance(name: string, class: string)
	local folder = replicated_storage():FindFirstChild("AlloyRemotes")
	if not folder then
		if run_service():IsServer() then
			folder = Instance.new("Folder")
			folder.Name = "AlloyRemotes"
			folder.Parent = replicated_storage()
		else
			folder = replicated_storage():WaitForChild("AlloyRemotes")
		end
	end
	local inst = folder:FindFirstChild(name)
	if not inst then
		if run_service():IsServer() then
			inst = Instance.new(class)
			inst.Name = name
			inst.Parent = folder
		else
			inst = folder:WaitForChild(name)
		end
	end
	return inst
end

--- The object a `remote` declaration binds. `fire` and `call` take the
--- player first on the server; `on` takes the handler; `wait` yields
--- for the next event's payload.
--- The declaration the compiler writes for a remote: what it is called,
--- which side may fire it, the parameter names, the defaults, the
--- attributes, and the wire layout when the payload packs.
export type RemoteSpec = {
	name: string,
	kind: string,
	from_client: boolean,
	from_server: boolean,
	params: { string },
	defaults: { [string]: any },
	attrs: { [string]: { any } },
	wire: { any }?,
}

export type Remote = {
	spec: RemoteSpec,
	instance: Instance?,
	fire: (...any) -> (),
	fire_all: (...any) -> (),
	fire_except: (...any) -> (),
	call: (...any) -> Future<any>,
	on: (handler: (...any) -> ...any) -> any,
	once: (handler: (...any) -> ...any) -> any,
	on_ratelimited: (handler: (...any) -> ...any) -> any,
	wait: () -> Future<any>,
}

-- ---------------------------------------------------------------------------
-- The wire: a remote packs its numbers, booleans, and strings into one
-- buffer, at the width each parameter declares. What the packer cannot
-- pack, a table or an Instance, travels beside the buffer as it is.
-- ---------------------------------------------------------------------------

local WIRE_SIZE = { u8 = 1, u16 = 2, u32 = 4, i8 = 1, i16 = 2, i32 = 4, f32 = 4, f64 = 8, bool = 1 }
local WIRE_RANGE = {
	u8 = { 0, 255 },
	u16 = { 0, 65535 },
	u32 = { 0, 4294967295 },
	i8 = { -128, 127 },
	i16 = { -32768, 32767 },
	i32 = { -2147483648, 2147483647 },
}

-- A kind is a string, `u8`, `str?`, `any`, or a table: `{ fields = {
-- { name, kind }, ... }, struct = Metatable? }` for a record, `{ item =
-- kind }` for an array, each with `optional = true` when nil is fine.
local function wire_kind(kind: any): (any, boolean)
	if type(kind) == "string" then
		if string.sub(kind, -1) == "?" then
			return string.sub(kind, 1, -2), true
		end
		return kind, false
	end
	return kind, kind.optional == true
end

local function wire_check(label: string, base: string, v: any)
	local range = WIRE_RANGE[base]
	if range ~= nil then
		if type(v) ~= "number" or v % 1 ~= 0 or v < range[1] or v > range[2] then
			error(`{label}: {tostring(v)} does not fit @{base}`, 0)
		end
	elseif base == "f32" or base == "f64" then
		if type(v) ~= "number" then
			error(`{label}: {tostring(v)} is not a number`, 0)
		end
	elseif base == "bool" then
		if type(v) ~= "boolean" then
			error(`{label}: {tostring(v)} is not a boolean`, 0)
		end
	elseif base == "str" then
		if type(v) ~= "string" then
			error(`{label}: {tostring(v)} is not a string`, 0)
		end
	elseif type(base) == "table" and type(v) ~= "table" then
		error(`{label}: {tostring(v)} is not a table`, 0)
	end
end

-- The bytes one value takes under a kind; a nil where none is allowed
-- raises with the path of the value.
local function wire_measure(kind: any, v: any, label: string): number
	local base, optional = wire_kind(kind)
	local size = 0
	if base == "any" then
		return 0
	end
	if optional then
		size += 1
		if v == nil then
			return size
		end
	elseif v == nil then
		error(`{label}: is nil`, 0)
	end
	wire_check(label, base, v)
	if type(base) == "table" then
		if base.fields ~= nil then
			for _, field in base.fields do
				size += wire_measure(field[2], v[field[1]], `{label}.{field[1]}`)
			end
		else
			size += 4
			for i = 1, #v do
				size += wire_measure(base.item, v[i], `{label}[{i}]`)
			end
		end
	elseif base == "str" then
		size += 4 + #v
	else
		size += WIRE_SIZE[base]
	end
	return size
end

local function wire_write(kind: any, v: any, buf: buffer, at: number, rest: { any }): number
	local base, optional = wire_kind(kind)
	if base == "any" then
		rest.n += 1
		rest[rest.n] = v
		return at
	end
	if optional then
		buffer.writeu8(buf, at, if v == nil then 0 else 1)
		at += 1
		if v == nil then
			return at
		end
	end
	if type(base) == "table" then
		if base.fields ~= nil then
			for _, field in base.fields do
				at = wire_write(field[2], v[field[1]], buf, at, rest)
			end
		else
			buffer.writeu32(buf, at, #v)
			at += 4
			for i = 1, #v do
				at = wire_write(base.item, v[i], buf, at, rest)
			end
		end
		return at
	end
	if base == "u8" then
		buffer.writeu8(buf, at, v)
	elseif base == "u16" then
		buffer.writeu16(buf, at, v)
	elseif base == "u32" then
		buffer.writeu32(buf, at, v)
	elseif base == "i8" then
		buffer.writei8(buf, at, v)
	elseif base == "i16" then
		buffer.writei16(buf, at, v)
	elseif base == "i32" then
		buffer.writei32(buf, at, v)
	elseif base == "f32" then
		buffer.writef32(buf, at, v)
	elseif base == "f64" then
		buffer.writef64(buf, at, v)
	elseif base == "bool" then
		buffer.writeu8(buf, at, if v then 1 else 0)
	elseif base == "str" then
		buffer.writeu32(buf, at, #v)
		buffer.writestring(buf, at + 4, v)
		return at + 4 + #v
	end
	return at + WIRE_SIZE[base]
end

local function wire_read(kind: any, buf: buffer, at: number, rest: { any }, ri: number): (any, number, number)
	local base, optional = wire_kind(kind)
	if base == "any" then
		return rest[ri], at, ri + 1
	end
	if optional then
		local present = buffer.readu8(buf, at)
		at += 1
		if present == 0 then
			return nil, at, ri
		end
	end
	if type(base) == "table" then
		local t = {}
		if base.fields ~= nil then
			for _, field in base.fields do
				t[field[1]], at, ri = wire_read(field[2], buf, at, rest, ri)
			end
			if base.struct ~= nil then
				setmetatable(t, base.struct)
			end
		else
			local count = buffer.readu32(buf, at)
			at += 4
			for i = 1, count do
				t[i], at, ri = wire_read(base.item, buf, at, rest, ri)
			end
			if base.array then
				setmetatable(t, Array)
			end
		end
		return t, at, ri
	end
	local v
	if base == "u8" then
		v = buffer.readu8(buf, at)
	elseif base == "u16" then
		v = buffer.readu16(buf, at)
	elseif base == "u32" then
		v = buffer.readu32(buf, at)
	elseif base == "i8" then
		v = buffer.readi8(buf, at)
	elseif base == "i16" then
		v = buffer.readi16(buf, at)
	elseif base == "i32" then
		v = buffer.readi32(buf, at)
	elseif base == "f32" then
		v = buffer.readf32(buf, at)
	elseif base == "f64" then
		v = buffer.readf64(buf, at)
	elseif base == "bool" then
		v = buffer.readu8(buf, at) == 1
	elseif base == "str" then
		local len = buffer.readu32(buf, at)
		return buffer.readstring(buf, at + 4, len), at + 4 + len, ri
	end
	return v, at + WIRE_SIZE[base], ri
end

local wire = {}

--- Packs the arguments of one fire by the layout: the buffer, then the
--- values the layout marks `any`, in the order the packer met them.
function wire.pack(layout: { any }, args: { any }, name: string?): (buffer, { any })
	local label = name or "remote"
	local size = 0
	for i, kind in layout do
		size += wire_measure(kind, args[i], `{label}: argument {i}`)
	end
	local buf = buffer.create(size)
	local at = 0
	local rest = { n = 0 }
	for i, kind in layout do
		at = wire_write(kind, args[i], buf, at, rest)
	end
	return buf, rest
end

--- Reads the arguments back: the buffer and the `any` values beside it,
--- as the packer laid them out.
function wire.unpack(layout: { any }, buf: buffer, rest: { any }): { any }
	local args = { n = #layout }
	local at = 0
	local ri = 1
	for i, kind in layout do
		args[i], at, ri = wire_read(kind, buf, at, rest, ri)
	end
	return args
end

alloy.wire = wire

function alloy.remote(spec: any): Remote
	local is_function = spec.kind == "function"
	local attrs = spec.attrs or {}
	-- `@unreliable` picks the event class that may drop or reorder.
	local class = if is_function then "RemoteFunction" elseif attrs.unreliable then "UnreliableRemoteEvent" else "RemoteEvent"
	local inst = remote_instance(spec.name, class)
	local remote = { spec = spec, instance = inst }
	local limiter = attrs.ratelimit
	-- `@validate(fn)`: the server runs it on the sender and the
	-- arguments; a false drops the event, or answers a call with nil.
	local validate = attrs.validate and attrs.validate[1]
	-- `@timeout(seconds)`: a call that gets no answer in time rejects.
	local timeout = attrs.timeout and attrs.timeout[1]
	-- One bucket per player, dropped when the player is collected.
	-- Both bindings open before `allowed`, which reads them: declared
	-- after it, they would be two names and `allowed` would index nil.
	local buckets = setmetatable({}, { __mode = "k" })
	local on_limited = nil

	local function allowed(player)
		if not limiter then
			return true
		end
		local count, seconds = limiter[1], limiter[2]
		local now = os.clock()
		local b = buckets[player]
		if not b or now - b.start >= seconds then
			b = { start = now, used = 0 }
			buckets[player] = b
		end
		b.used += 1
		if b.used > count then
			if on_limited then
				on_limited(player)
			end
			return false
		end
		return true
	end

	local function fill_defaults(args)
		for name, value in pairs(spec.defaults or {}) do
			for i, pname in ipairs(spec.params) do
				if pname == name and args[i] == nil then
					args[i] = value
				end
			end
		end
		return args
	end

	-- The arguments as they cross: packed by the layout, or as given.
	local layout = spec.wire

	local function outgoing(...)
		if layout == nil then
			return table.pack(...)
		end
		local buf, rest = wire.pack(layout, fill_defaults(table.pack(...)), spec.name)
		local out = { n = 1 + rest.n, buf }
		for i = 1, rest.n do
			out[i + 1] = rest[i]
		end
		return out
	end

	local function incoming(...)
		if layout == nil then
			return fill_defaults(table.pack(...))
		end
		local buf = ...
		return fill_defaults(wire.unpack(layout, buf, { select(2, ...) }))
	end

	local function accepted(player, ...)
		if not allowed(player) then
			return false
		end
		if validate ~= nil and not validate(player, ...) then
			return false
		end
		return true
	end

	local function timed(f)
		if timeout == nil then
			return alloy.future(f)
		end
		return Future.race({
			alloy.future(f),
			alloy.future(function()
				task_lib.wait(timeout)
				error(`{spec.name}: no answer in {timeout}s`)
			end),
		})
	end

	if run_service():IsServer() then
		function remote.fire(player, ...)
			local out = outgoing(...)
			inst:FireClient(player, table.unpack(out, 1, out.n))
		end
		function remote.fire_all(...)
			local out = outgoing(...)
			inst:FireAllClients(table.unpack(out, 1, out.n))
		end
		function remote.fire_except(except, ...)
			local out = outgoing(...)
			for _, p in game:GetService("Players"):GetPlayers() do
				if p ~= except then
					inst:FireClient(p, table.unpack(out, 1, out.n))
				end
			end
		end
		function remote.on(handler)
			if is_function then
				inst.OnServerInvoke = function(player, ...)
					local args = incoming(...)
					if not accepted(player, table.unpack(args, 1, args.n)) then
						return nil
					end
					local result = handler(player, table.unpack(args, 1, args.n))
					if type(result) == "table" and getmetatable(result) == Future then
						return alloy.await(result)
					end
					return result
				end
				return
			end
			return inst.OnServerEvent:Connect(function(player, ...)
				local args = incoming(...)
				if accepted(player, table.unpack(args, 1, args.n)) then
					handler(player, table.unpack(args, 1, args.n))
				end
			end)
		end
		function remote.on_ratelimited(handler)
			on_limited = handler
		end
		function remote.call(player, ...)
			local out = outgoing(...)
			return timed(function()
				return inst:InvokeClient(player, table.unpack(out, 1, out.n))
			end)
		end
	else
		function remote.fire(...)
			local out = outgoing(...)
			inst:FireServer(table.unpack(out, 1, out.n))
		end
		function remote.on(handler)
			if is_function then
				inst.OnClientInvoke = function(...)
					local args = incoming(...)
					return handler(table.unpack(args, 1, args.n))
				end
				return
			end
			return inst.OnClientEvent:Connect(function(...)
				local args = incoming(...)
				handler(table.unpack(args, 1, args.n))
			end)
		end
		function remote.call(...)
			local out = outgoing(...)
			return timed(function()
				return inst:InvokeServer(table.unpack(out, 1, out.n))
			end)
		end
	end

	function remote.once(handler)
		local conn
		conn = remote.on(function(...)
			if conn then
				conn:Disconnect()
			end
			handler(...)
		end)
		return conn
	end

	function remote.wait()
		return alloy.future(function()
			local thread = coroutine.running()
			local packed
			remote.once(function(...)
				packed = table.pack(...)
				task_lib.spawn(thread)
			end)
			coroutine.yield()
			return table.unpack(packed, 1, packed.n)
		end)
	end

	return (remote :: any) :: Remote
end

return table.freeze(alloy)
