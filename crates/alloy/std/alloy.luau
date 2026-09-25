--!nonstrict
-- The Alloy runtime. One plain Luau module, required by emitted code, and
-- usable from vanilla Luau. Emitted code reaches it as `__alloy`.

local alloy = {}

-- ---------------------------------------------------------------------------
-- Printing. Every Alloy value has a `__tostring`, so `print(v)` and
-- `{v}` in a string show the value and not a table address. A struct's
-- own `to_string` replaces the default the compiler writes.
-- ---------------------------------------------------------------------------

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

--- The `Text` of a lone `{expr}` in markup. Only the check artifact
--- calls it: the parameter type makes any other value a type error.
function alloy.text(v: string | number): string
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

--- The entries of a map, `a = 1, [3] = "x"`, in a stable order.
local function show_entries(t: { [any]: any }): string
	local keys = sorted_keys(t)
	local parts = table.create(#keys)
	for i, k in ipairs(keys) do
		local key = if type(k) == "string" and string.match(k, "^[%a_][%w_]*$") then k else `[{alloy.show(k)}]`
		parts[i] = key .. " = " .. alloy.show(t[k])
	end
	return table.concat(parts, ", ")
end

-- The plain tables `alloy.show` prints now. A table that holds itself
-- prints its address the second time, not a loop without end.
local showing: { [any]: boolean } = {}

--- The text of a value inside a printed container: a string in quotes,
--- anything else through `tostring`. A plain table has no `__tostring`
--- and `tostring` gives its address, so it prints the way an `Array` or
--- a `HashMap` does. `from_table` gives the one for the other, and the
--- two print alike.
function alloy.show(v: any): string
	if type(v) == "string" then
		return `"{string.gsub(v, '["\\]', "\\%0")}"`
	end
	if type(v) == "table" and getmetatable(v) == nil and not showing[v] then
		local count = 0
		for _ in next, v do
			count += 1
		end
		showing[v] = true
		local text = if count == #v then show_list(v, "[", "]") else `\{ {show_entries(v)} }`
		showing[v] = nil
		return text
	end
	return tostring(v)
end

--- A value whose declared type is an enum. A unit variant is a string
--- at runtime, and it prints under the enum's name, `Item.Junk`.
local function show_in(v: any, enum: string?): string
	if enum ~= nil and type(v) == "string" then
		return enum .. "." .. v
	end
	return alloy.show(v)
end

--- `Name { x = 1, y = 2 }`: the default printer of a struct, over its
--- declared fields in order. `enums` names the enum of each field that
--- declares one.
function alloy.show_struct(name: string, s: any, fields: { string }, enums: { [string]: string }?): string
	if #fields == 0 then
		return name .. " {}"
	end
	local parts = table.create(#fields)
	for i, f in ipairs(fields) do
		parts[i] = f .. " = " .. show_in(s[f], enums and enums[f])
	end
	return name .. " { " .. table.concat(parts, ", ") .. " }"
end

--- A new table with `f` applied to each value, for a map of structs
--- that `@derive(Serialize)` or `Deserialize` walks.
function alloy.map_values(t: any, f: (any) -> any): any
	local out = {}
	for k, v in next, t do
		out[k] = f(v)
	end
	return out
end

--- Equality by content, for `@derive(Eq)` over a field that holds a
--- table: an array, a map, a set, or a plain record. A value with an
--- `__eq` of its own, a struct or an enum, answers through `==`.
function alloy.deep_eq(a: any, b: any): boolean
	if a == b then
		return true
	end
	if type(a) ~= "table" or type(b) ~= "table" then
		return false
	end
	local meta = getmetatable(a)
	if meta ~= getmetatable(b) or (type(meta) == "table" and rawget(meta, "__eq") ~= nil) then
		return false
	end
	for k, v in next, a do
		if not alloy.deep_eq(v, rawget(b, k)) then
			return false
		end
	end
	for k in next, b do
		if rawget(a, k) == nil then
			return false
		end
	end
	return true
end

--- `Msg.Move(1, 2)`: the printer of an enum variant with a payload.
--- `enums` names the enum of each payload slot that declares one, by
--- variant: `{ Tool = { [1] = "Kind" } }`.
function alloy.show_variant(name: string, v: any, enums: { [string]: { [number]: string } }?): string
	local slots = enums and enums[v.tag]
	local parts = {}
	local i = 1
	while v["_" .. i] ~= nil do
		table.insert(parts, show_in(v["_" .. i], slots and slots[i]))
		i += 1
	end
	return `{name}.{v.tag}({table.concat(parts, ", ")})`
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
	read and_then: <R>(self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }, f: (T) -> R) -> R,
	read or_else: <R>(self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }, f: (E) -> R) -> R,
	read unwrap_or_else: <D>(self: { read tag: "Ok", read _1: T } | { read tag: "Err", read _1: E }, f: (E) -> D) -> T | D,
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
-- `and_then` and `or_else` answer the callback's own return, which is
-- the Result it builds. On the other tag they hand back `self`, a Result
-- too, so the type is right at run time and names no rung, which keeps
-- the solver off the alias loop the notes above describe.
--
-- `unwrap_or` answers `T | D`, the way it does one rung up, so a
-- fallback of another type still names both sides. `map` and `map_err`
-- climb to `Result3`, the last rung, so `r:map(f):map(g)` keeps the
-- value type.
export type ResultMethods2<T, E> = {
	read unwrap: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> T,
	read expect: (self: { read tag: "Ok" | "Err", read _1: T | E }, message: string) -> T,
	read unwrap_or: <D>(self: { read tag: "Ok" | "Err", read _1: T | E }, default: D) -> T | D,
	read map: <U>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (T) -> U) -> Result3<U, any>,
	read map_err: <F>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (E) -> F) -> Result3<any, F>,
	read is_ok: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> boolean,
	read is_err: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> boolean,
	read ok: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> T?,
	read and_then: <R>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (T) -> R) -> R,
	read or_else: <R>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (E) -> R) -> R,
	read unwrap_or_else: <D>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (E) -> D) -> T | D,
}

export type Result2<T, E> = ({ tag: "Ok", read _1: T, read trace: string?, read __ok: T, read __err: E } | { tag: "Err", read _1: E, read trace: string?, read __ok: T, read __err: E }) & ResultMethods2<T, E>

-- The third rung, which `map` on a `Result2` answers, so two maps in a
-- chain keep the value type. Its own `map` answers `any`: an alias
-- cannot name itself with other arguments, and a return that leads back
-- to `Result2` or to `Result` makes the solver read the next callback's
-- parameter as a free type and report `Expected type table` on code
-- that is right.
export type ResultMethods3<T, E> = {
	read unwrap: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> T,
	read expect: (self: { read tag: "Ok" | "Err", read _1: T | E }, message: string) -> T,
	read unwrap_or: <D>(self: { read tag: "Ok" | "Err", read _1: T | E }, default: D) -> T | D,
	read map: <U>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (T) -> U) -> any,
	read map_err: <F>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (E) -> F) -> any,
	read is_ok: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> boolean,
	read is_err: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> boolean,
	read ok: (self: { read tag: "Ok" | "Err", read _1: T | E }) -> T?,
	read and_then: <R>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (T) -> R) -> R,
	read or_else: <R>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (E) -> R) -> R,
	read unwrap_or_else: <D>(self: { read tag: "Ok" | "Err", read _1: T | E }, f: (E) -> D) -> T | D,
}

export type Result3<T, E> = ({ tag: "Ok", read _1: T, read trace: string?, read __ok: T, read __err: E } | { tag: "Err", read _1: E, read trace: string?, read __ok: T, read __err: E }) & ResultMethods3<T, E>


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

function Result.and_then(self, f)
	if self.tag == "Err" then
		return self
	end
	return f(self._1)
end

function Result.or_else(self, f)
	if self.tag == "Ok" then
		return self
	end
	return f(self._1)
end

function Result.unwrap_or_else(self, f)
	if self.tag == "Err" then
		return f(self._1)
	end
	return self._1
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

-- The first type of a pack, or `unknown` for an empty one.
type function __pack_head(pack)
	local head = pack.head

	if head == nil or #head == 0 then
		return types.unknown
	end

	return head[1]
end

-- The value the closure of a `try do` block gives back.
type function __try_ret(F)
	local fn: any = F

	if not fn:is("function") then
		return types.unknown
	end

	return __pack_head(fn:returns())
end

-- The error type the closure's `fail` takes: the first parameter of
-- its first parameter.
type function __try_fail(F)
	local fn: any = F

	if not fn:is("function") then
		return types.unknown
	end

	local fail: any = __pack_head(fn:parameters())

	if not fail:is("function") then
		return types.unknown
	end

	return __pack_head(fail:parameters())
end

--- `try do ... end`: the block's value as a Result. A plain value becomes
--- Ok, a Result passes through as is, and a throw becomes Err with the
--- traceback from the throw site.
---
--- The closure takes the block's `fail`, which a `try` inside the block
--- calls on Err. The call carries the error type, so the error solves
--- from what the block can fail with; the call leaves the block, so the
--- block's own `return` alone decides the value. Luau reads a closure's
--- return type off its first `return` and checks the rest against it,
--- so an Err returned there would take the place of the value.
---
--- `F` binds the whole closure type, and the type functions read the
--- value and the error off it. A generic in the closure's return, `(fail)
--- -> ...T`, lands on `unknown` when the return is a type function
--- that is still pending, as the result of `await` is; the value of a
--- block around an `await` then read `any`. A block that returns
--- nothing leaves the value `unknown`, and `__try_ok` answers `any`.
function alloy.try_block<F>(
	f: F
): Result<__try_ok<__try_ret<F>>, __try_err<__try_ret<F>, __try_fail<F>>>
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
--- returns the value, rethrows the error it settled with.
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
		-- The value alone, at level 0. The stored trace opens with the
		-- same message, so a rethrow of both printed the failure twice
		-- with the runtime's own frames beside it, and a level would add
		-- a second place to a message that carries its own.
		-- `future.trace` still holds the frames for a reader who wants
		-- them.
		error(target.value, 0)
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
	remove: (self: { read [number]: T }, index: number) -> T?,
	swap_remove: (self: { read [number]: T }, index: number) -> T?,
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
	to_table: (self: { read [number]: T }) -> { T },
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
	remove: (self: { read [number]: T }, index: number) -> T?,
	swap_remove: (self: { read [number]: T }, index: number) -> T?,
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
	to_table: (self: { read [number]: T }) -> { T },
}

export type Array3<T> = {
	[number]: T,
	len: (self: { read [number]: T }) -> number,
	is_empty: (self: { read [number]: T }) -> boolean,
	push: (self: { read [number]: T }, ...T) -> Array3<T>,
	pop: (self: { read [number]: T }) -> T?,
	remove: (self: { read [number]: T }, index: number) -> T?,
	swap_remove: (self: { read [number]: T }, index: number) -> T?,
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
	to_table: (self: { read [number]: T }) -> { T },
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
	-- One value is the common call, and it skips the `select` loop: 1.2x.
	if select("#", ...) == 1 then
		table.insert(xs, (...))
	else
		for i = 1, select("#", ...) do
			table.insert(xs, (select(i, ...)))
		end
	end
	return xs
end

-- Written out, not `table.remove`: the builtin costs 20% more. An empty
-- array returns no value, as `table.remove` does.
function Array.pop(xs)
	local n = #xs
	if n == 0 then
		return
	end
	local value = xs[n]
	xs[n] = nil
	return value
end

-- `table.remove`: the items after `index` move down one.
function Array.remove(xs, index)
	return table.remove(xs, index)
end

-- O(1): the last item fills the hole, so the order changes. An index
-- outside 1 to `len` removes nothing and returns nil.
function Array.swap_remove(xs, index)
	local n = #xs
	if index < 1 or index > n or index % 1 ~= 0 then
		return nil
	end
	local value = xs[index]
	xs[index] = xs[n]
	xs[n] = nil
	return value
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
	local out, n = {}, 0
	for i, v in ipairs(xs) do
		if f(v, i) then
			n += 1
			out[n] = v
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

-- `{ T }` and not `Array<T>`: a rest pattern, `local [a, ...rest] = xs`,
-- slices a plain table as well.
function Array.slice<T>(xs: { T }, from: number, to: number?): Array<T>
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

-- `to_table` hands back the table the wrapper owns, never a copy.
-- A write through the result reaches the collection. An array is
-- its own table, so this one returns `self`.
function Array.to_table(xs)
	return xs
end

function Array.__tostring(self)
	return show_list(self, "[", "]")
end

alloy.Array = Array

-- ---------------------------------------------------------------------------
-- HashMap and Set: Alloy structs with methods.
-- ---------------------------------------------------------------------------

--- `for key, value in map` walks the pairs, as `entries` does. The
--- metatable form carries `__iter`, so the loop types its variables.
export type HashMap<K, V> = typeof(setmetatable({} :: {
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
	to_table: (self: HashMap<K, V>) -> { [K]: V },
}, {} :: { __iter: (self: any) -> (({ [K]: V }, K?) -> (K, V), { [K]: V }) }))

local HashMap = {}
HashMap.__index = HashMap

function HashMap.new<K, V>(): HashMap<K, V>
	return (setmetatable({ _items = {}, _count = 0 }, HashMap) :: any) :: HashMap<K, V>
end

function HashMap.from<K, V>(t: { [K]: V }): HashMap<K, V>
	-- `table.clone` copies a plain table in one native call, 30x faster
	-- than a `set` per pair. It would also copy a metatable, so a table
	-- with one goes pair by pair.
	local plain = getmetatable(t) == nil
	local items = if plain then table.clone(t) else {}
	if not plain then
		for k, v in pairs(t) do
			items[k] = v
		end
	end
	local count = 0
	for _ in items do
		count += 1
	end
	return (setmetatable({ _items = items, _count = count }, HashMap) :: any) :: HashMap<K, V>
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
	local out, n = {}, 0
	for k in self._items do
		n += 1
		out[n] = k
	end
	return setmetatable(out, Array)
end

function HashMap.values(self)
	local out, n = {}, 0
	for _, v in self._items do
		n += 1
		out[n] = v
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

-- A loop over `next` calls it once per pair, since only a `for` that
-- names `next` or `pairs` in its own header takes the builtin walk.
-- `for k, v in map:to_table()` is the fast loop, 20x at a million pairs.
function HashMap.__iter(self)
	return next, self._items
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

-- A write through the result skips the count, so `len` goes wrong.
function HashMap.to_table(self)
	return self._items
end

function HashMap.__tostring(self)
	if next(self._items) == nil then
		return "HashMap {}"
	end
	return `HashMap \{ {show_entries(self._items)} }`
end

alloy.HashMap = HashMap

--- `for value in set` walks the members, in no set order.
export type Set<T> = typeof(setmetatable({} :: {
	add: (self: Set<T>, value: T) -> Set<T>,
	has: (self: Set<T>, value: T) -> boolean,
	remove: (self: Set<T>, value: T) -> boolean,
	len: (self: Set<T>) -> number,
	union: (self: Set<T>, other: Set<T>) -> Set<T>,
	intersection: (self: Set<T>, other: Set<T>) -> Set<T>,
	difference: (self: Set<T>, other: Set<T>) -> Set<T>,
	to_array: (self: Set<T>) -> Array<T>,
	to_table: (self: Set<T>) -> { [T]: boolean },
}, {} :: { __iter: (self: any) -> (({ [T]: boolean }, T?) -> (T, boolean), { [T]: boolean }) }))

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

-- The character a player spawns, by rig. `[roblox] rig` in alloy.toml
-- picks the one `Player.Character` reads as.
export type R15Character = Model & {
	HumanoidRootPart: Part?,
	Head: MeshPart?,

	UpperTorso: MeshPart?,
	LowerTorso: MeshPart?,

	LeftUpperArm: MeshPart?,
	LeftLowerArm: MeshPart?,
	LeftHand: MeshPart?,

	RightUpperArm: MeshPart?,
	RightLowerArm: MeshPart?,
	RightHand: MeshPart?,

	LeftUpperLeg: MeshPart?,
	LeftLowerLeg: MeshPart?,
	LeftFoot: MeshPart?,

	RightUpperLeg: MeshPart?,
	RightLowerLeg: MeshPart?,
	RightFoot: MeshPart?,

	Humanoid: (Humanoid & {
		Animator: Animator?,
		HumanoidDescription: HumanoidDescription?,
	})?,

	BodyColors: BodyColors?,

	-- Every spawned character carries these two, whatever the rig.
	Animate: LocalScript?,
	Health: Script?,
}

-- The R6 part names carry spaces, so the keys are quoted.
export type R6Character = Model & {
	HumanoidRootPart: Part?,
	Head: Part?,
	Torso: Part?,

	["Left Arm"]: Part?,
	["Right Arm"]: Part?,
	["Left Leg"]: Part?,
	["Right Leg"]: Part?,

	Humanoid: (Humanoid & {
		Animator: Animator?,
		HumanoidDescription: HumanoidDescription?,
	})?,

	BodyColors: BodyColors?,

	Animate: LocalScript?,
	Health: Script?,
}

local Set = {}
Set.__index = Set

function Set.new<T>(): Set<T>
	return (setmetatable({ _items = {}, _count = 0 }, Set) :: any) :: Set<T>
end

function Set.from<T>(t: { T }): Set<T>
	local items, count = {}, 0
	for _, v in ipairs(t) do
		if not items[v] then
			items[v] = true
			count += 1
		end
	end
	return (setmetatable({ _items = items, _count = count }, Set) :: any) :: Set<T>
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

-- The set ops write the new table directly. An `add` per member cost a
-- method call each, and `table.clone` copies the larger side at once.
local function set_of(items, count)
	return setmetatable({ _items = items, _count = count }, Set)
end

function Set.union(self, other)
	local big, small = self, other
	if other._count > self._count then
		big, small = other, self
	end
	local items, count = table.clone(big._items), big._count
	for v in small._items do
		if not items[v] then
			items[v] = true
			count += 1
		end
	end
	return set_of(items, count)
end

function Set.intersection(self, other)
	local theirs = other._items
	local items, count = {}, 0
	for v in self._items do
		if theirs[v] then
			items[v] = true
			count += 1
		end
	end
	return set_of(items, count)
end

function Set.difference(self, other)
	local theirs = other._items
	local items, count = {}, 0
	for v in self._items do
		if not theirs[v] then
			items[v] = true
			count += 1
		end
	end
	return set_of(items, count)
end

function Set.to_array(self)
	local out, n = {}, 0
	for v in self._items do
		n += 1
		out[n] = v
	end
	return setmetatable(out, Array)
end

function Set.__iter(self)
	return next, self._items
end

-- A write through the result skips the count, so `len` goes wrong.
function Set.to_table(self)
	return self._items
end

function Set.__tostring(self)
	return show_list(sorted_keys(self._items), "Set {", "}")
end

alloy.Set = Set

-- ---------------------------------------------------------------------------
-- BitSet: a set of whole numbers, one bit each, in a buffer.
-- ---------------------------------------------------------------------------

--- A set of whole numbers from 0 up, one bit per number in a buffer. A
--- `Set<number>` spends a table slot on each member; this spends a bit
--- on each number up to the largest, and `add` and `has` run about 2x
--- faster. The buffer grows to the largest member added. `for i in bits`
--- walks the members in rising order.
export type BitSet = typeof(setmetatable({} :: {
	add: (self: BitSet, index: number) -> BitSet,
	has: (self: BitSet, index: number) -> boolean,
	remove: (self: BitSet, index: number) -> boolean,
	len: (self: BitSet) -> number,
	is_empty: (self: BitSet) -> boolean,
	clear: (self: BitSet) -> (),
	union: (self: BitSet, other: BitSet) -> BitSet,
	intersection: (self: BitSet, other: BitSet) -> BitSet,
	difference: (self: BitSet, other: BitSet) -> BitSet,
	to_array: (self: BitSet) -> Array<number>,
}, {} :: { __iter: (self: any) -> () -> number? }))

-- The fields behind `BitSet`: the bits, and the count of set bits.
type BitSetData = { _buf: buffer, _count: number }

local BitSet = {}
BitSet.__index = BitSet

-- The bytes that hold `size` bits, in whole 32-bit words: the word
-- loops read 4 bytes at a time and never read past the end.
local function bitset_bytes(size: number): number
	return math.max(4, math.ceil(size / 32) * 4)
end

-- Each set bit of the 32-bit words, counted in parallel within a word.
local function bitset_popcount(buf: buffer): number
	local count = 0
	for at = 0, buffer.len(buf) - 4, 4 do
		local x = buffer.readu32(buf, at)
		if x ~= 0 then
			x -= bit32.band(bit32.rshift(x, 1), 0x55555555)
			x = bit32.band(x, 0x33333333) + bit32.band(bit32.rshift(x, 2), 0x33333333)
			x = bit32.band(x + bit32.rshift(x, 4), 0x0F0F0F0F)
			count += bit32.band(x + bit32.rshift(x, 8) + bit32.rshift(x, 16) + bit32.rshift(x, 24), 0x3F)
		end
	end
	return count
end

local function bitset_of(buf: buffer, count: number): BitSet
	return (setmetatable({ _buf = buf, _count = count }, BitSet) :: any) :: BitSet
end

--- An empty set with room for the numbers below `size`, 256 when not
--- given. A larger number grows the buffer.
function BitSet.new(size: number?): BitSet
	return bitset_of(buffer.create(bitset_bytes(size or 256)), 0)
end

function BitSet.from(indices: { number }): BitSet
	local self = BitSet.new()
	for _, index in ipairs(indices) do
		self:add(index)
	end
	return self
end

function BitSet.add(self: BitSetData, index: number): BitSet
	if type(index) ~= "number" or index < 0 or index % 1 ~= 0 then
		error(`BitSet: {tostring(index)} is not a whole number from 0 up`, 2)
	end
	local at = index // 8
	local buf = self._buf
	if at >= buffer.len(buf) then
		-- Double at the least, so a run of adds grows O(log n) times.
		local grown = buffer.create(bitset_bytes(math.max(index + 1, buffer.len(buf) * 16)))
		buffer.copy(grown, 0, buf)
		self._buf = grown
		buf = grown
	end
	local byte = buffer.readu8(buf, at)
	local bit = bit32.lshift(1, index % 8)
	if not bit32.btest(byte, bit) then
		buffer.writeu8(buf, at, bit32.bor(byte, bit))
		self._count += 1
	end
	return (self :: any) :: BitSet
end

-- A number outside the buffer, below 0, or with a fraction is no member.
function BitSet.has(self: BitSetData, index: number): boolean
	local at = index // 8
	if index % 1 ~= 0 or at < 0 or at >= buffer.len(self._buf) then
		return false
	end
	return bit32.btest(buffer.readu8(self._buf, at), bit32.lshift(1, index % 8))
end

function BitSet.remove(self: BitSetData, index: number): boolean
	local at = index // 8
	if index % 1 ~= 0 or at < 0 or at >= buffer.len(self._buf) then
		return false
	end
	local byte = buffer.readu8(self._buf, at)
	local bit = bit32.lshift(1, index % 8)
	if not bit32.btest(byte, bit) then
		return false
	end
	buffer.writeu8(self._buf, at, bit32.band(byte, bit32.bnot(bit)))
	self._count -= 1
	return true
end

function BitSet.len(self: BitSetData): number
	return self._count
end

function BitSet.is_empty(self: BitSetData): boolean
	return self._count == 0
end

function BitSet.clear(self: BitSetData)
	buffer.fill(self._buf, 0, 0)
	self._count = 0
end

-- The set ops run a 32-bit word at a time, and the result keeps the
-- size of the larger input for a union and of `self` for the others.
function BitSet.union(self: BitSetData, other: BitSetData): BitSet
	local big, small = self._buf, other._buf
	if buffer.len(small) > buffer.len(big) then
		big, small = small, big
	end
	local out = buffer.create(buffer.len(big))
	buffer.copy(out, 0, big)
	for at = 0, buffer.len(small) - 4, 4 do
		buffer.writeu32(out, at, bit32.bor(buffer.readu32(out, at), buffer.readu32(small, at)))
	end
	return bitset_of(out, bitset_popcount(out))
end

function BitSet.intersection(self: BitSetData, other: BitSetData): BitSet
	local mine, theirs = self._buf, other._buf
	local out = buffer.create(buffer.len(mine))
	for at = 0, math.min(buffer.len(mine), buffer.len(theirs)) - 4, 4 do
		buffer.writeu32(out, at, bit32.band(buffer.readu32(mine, at), buffer.readu32(theirs, at)))
	end
	return bitset_of(out, bitset_popcount(out))
end

function BitSet.difference(self: BitSetData, other: BitSetData): BitSet
	local mine, theirs = self._buf, other._buf
	local out = buffer.create(buffer.len(mine))
	buffer.copy(out, 0, mine)
	for at = 0, math.min(buffer.len(mine), buffer.len(theirs)) - 4, 4 do
		buffer.writeu32(out, at, bit32.band(buffer.readu32(mine, at), bit32.bnot(buffer.readu32(theirs, at))))
	end
	return bitset_of(out, bitset_popcount(out))
end

-- The members in rising order: each word that is not zero gives up its
-- lowest set bit until it is zero.
function BitSet.__iter(self: BitSetData): () -> number?
	local buf = self._buf
	local words = buffer.len(buf) // 4
	local w, word = -1, 0
	return function(): number?
		while word == 0 do
			w += 1
			if w >= words then
				return nil
			end
			word = buffer.readu32(buf, w * 4)
		end
		local bit = bit32.countrz(word)
		word = bit32.band(word, word - 1)
		return w * 32 + bit
	end
end

function BitSet.to_array(self: BitSetData): Array<number>
	local out, n = table.create(self._count), 0
	for index in BitSet.__iter(self) do
		n += 1
		out[n] = index
	end
	return (setmetatable(out, Array) :: any) :: Array<number>
end

function BitSet.__tostring(self: BitSetData): string
	return show_list(BitSet.to_array(self), "BitSet {", "}")
end

alloy.BitSet = BitSet

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

-- The list links both ways, so a disconnect unlinks in O(1). A walk to
-- the previous node made 10000 disconnects take 1.2 s. The node keeps
-- its `_next`, so a fire that stands on it walks on.
function Connection.Disconnect(self)
	if not self.Connected then
		return
	end
	self.Connected = false
	local signal = self._signal
	local before, after = self._prev, self._next
	if before ~= nil then
		before._next = after
	else
		signal._head = after
	end
	if after ~= nil then
		after._prev = before
	else
		signal._tail = before
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
		_prev = self._tail,
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

--- A first-in, first-out queue over a ring that doubles when full.
--- `push` appends, `pop` takes the oldest. Iterating reads front to
--- back without a pop.
--- The `__iter` in the type is for the checker: a `for` over a plain
--- table type reports, and the metatable form binds the element type.
export type Queue<T> = typeof(setmetatable({} :: {
	push: (self: Queue<T>, value: T) -> (),
	pop: (self: Queue<T>) -> T?,
	peek: (self: Queue<T>) -> T?,
	len: (self: Queue<T>) -> number,
	is_empty: (self: Queue<T>) -> boolean,
	clear: (self: Queue<T>) -> (),
	to_array: (self: Queue<T>) -> Array<T>,
	to_table: (self: Queue<T>) -> { T },
}, {} :: { __iter: (self: any) -> () -> T? }))

local Queue = {}
Queue.__index = Queue

-- The slots are 1 to `_cap`, and the front sits at `_head`. A queue
-- that slid its indices up and never emptied pushed its keys out of
-- the array part, and ran 10x slower than this ring.
function Queue.new<T>(): Queue<T>
	return (setmetatable({ _items = {}, _head = 1, _len = 0, _cap = 0 }, Queue) :: any) :: Queue<T>
end

function Queue.from<T>(t: { T }): Queue<T>
	local self = Queue.new()
	for _, v in ipairs(t) do
		self:push(v)
	end
	return self
end

-- A full ring doubles. The copy unrolls it, so the front lands at 1.
local function queue_grow(self)
	local items, head, cap = self._items, self._head, self._cap
	local size = if cap == 0 then 8 else cap * 2
	local grown = table.create(size)
	table.move(items, head, cap, 1, grown)
	table.move(items, 1, head - 1, cap - head + 2, grown)
	self._items = grown
	self._head = 1
	self._cap = size
end

function Queue.push(self, value)
	local n = self._len
	if n == self._cap then
		queue_grow(self)
	end
	local at = self._head + n
	if at > self._cap then
		at -= self._cap
	end
	self._items[at] = value
	self._len = n + 1
end

function Queue.pop(self)
	local n = self._len
	if n == 0 then
		return nil
	end
	local items, head = self._items, self._head
	local value = items[head]
	items[head] = nil
	-- An empty queue starts its indices over at 1.
	self._head = if n == 1 or head == self._cap then 1 else head + 1
	self._len = n - 1
	return value
end

function Queue.peek(self)
	return self._items[self._head]
end

function Queue.len(self)
	return self._len
end

function Queue.is_empty(self)
	return self._len == 0
end

function Queue.clear(self)
	self._items = {}
	self._head = 1
	self._len = 0
	self._cap = 0
end

function Queue.to_array(self)
	local items, head, n = self._items, self._head, self._len
	local out = table.create(n)
	-- The slots from the front to the end of the ring, then the rest from 1.
	local first = math.min(n, self._cap - head + 1)
	table.move(items, head, head + first - 1, 1, out)
	table.move(items, 1, n - first, first + 1, out)
	return setmetatable(out, Array)
end

-- The ring: the keys run from the head, which a pop moves up, and wrap
-- past the end to 1. Use `to_array` for a list that starts at 1.
function Queue.to_table(self)
	return self._items
end

function Queue.__iter(self)
	local k = 0
	return function()
		k += 1
		if k > self._len then
			return nil
		end
		local at = self._head + k - 1
		if at > self._cap then
			at -= self._cap
		end
		return self._items[at]
	end
end

function Queue.__tostring(self)
	return show_list(self:to_array(), "Queue [", "]")
end

alloy.Queue = Queue

--- A binary heap. `pop` yields the least value under `less`, which
--- defaults to `<`; pass one for tables: `Heap.new(function(a, b) return
--- a.cost < b.cost end)`. Iterating pops nothing; `to_array` sorts.
export type Heap<T> = typeof(setmetatable({} :: {
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
	to_table: (self: Heap<T>) -> { T },
}, {} :: { __iter: (self: any) -> () -> T? }))

local Heap = {}
Heap.__index = Heap

-- `_less` stays nil for the default order. The sifts then compare with
-- `<` inline, twice as fast as a call per comparison.
function Heap.new<T>(less: ((T, T) -> boolean)?): Heap<T>
	return (setmetatable({ _items = {}, _less = less }, Heap) :: any) :: Heap<T>
end

-- The sifts move a hole and write the value once, where it stops. A
-- swap at each level wrote every slot twice.
local function heap_sift_down(items, i, n, less)
	local value = items[i]
	if less == nil then
		while true do
			local child = i * 2
			if child > n then
				break
			end
			local below = items[child]
			if child < n then
				local right = items[child + 1]
				if right < below then
					child += 1
					below = right
				end
			end
			if not (below < value) then
				break
			end
			items[i] = below
			i = child
		end
	else
		while true do
			local child = i * 2
			if child > n then
				break
			end
			local below = items[child]
			if child < n then
				local right = items[child + 1]
				if less(right, below) then
					child += 1
					below = right
				end
			end
			if not less(below, value) then
				break
			end
			items[i] = below
			i = child
		end
	end
	items[i] = value
end

-- Each parent sifts down, from the last one up: O(n), where a push per
-- item costs O(n log n).
function Heap.from<T>(t: { T }, less: ((T, T) -> boolean)?): Heap<T>
	local self = Heap.new(less)
	local items = (self :: any)._items
	for i, v in ipairs(t) do
		items[i] = v
	end
	local n = #items
	for i = n // 2, 1, -1 do
		heap_sift_down(items, i, n, less)
	end
	return self
end

function Heap.push(self, value)
	-- `table.insert` of nil added nothing, and neither does this.
	if value == nil then
		return
	end
	local items, less = self._items, self._less
	local i = #items + 1
	if less == nil then
		while i > 1 do
			local parent = i // 2
			local above = items[parent]
			if not (value < above) then
				break
			end
			items[i] = above
			i = parent
		end
	else
		while i > 1 do
			local parent = i // 2
			local above = items[parent]
			if not less(value, above) then
				break
			end
			items[i] = above
			i = parent
		end
	end
	items[i] = value
end

function Heap.pop(self)
	local items = self._items
	local n = #items
	if n == 0 then
		return nil
	end
	local top = items[1]
	items[1] = items[n]
	items[n] = nil
	-- With one item left, it is the root already.
	if n > 2 then
		heap_sift_down(items, 1, n - 1, self._less)
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

-- Heap order, not sorted order. A write through the result breaks
-- the order that `pop` needs; `to_array` sorts a copy.
function Heap.to_table(self)
	return self._items
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

-- `collect` answers an `Array`, which a type function cannot name, so
-- the one function type below serves every step of a chain. It reads
-- the element off `self`.
type IterCollect = <X>(self: { next: (self: any) -> X? }) -> Array<X>

-- Luau rejects an alias that names itself with other arguments, so an
-- alias cannot give `map` the result `Iter<U>`. This type function
-- builds the type graph itself. Two steps over the generics T and U
-- point at each other, so a chain keeps its element type through any
-- number of maps.
--
-- The result is the function `<T>(T) -> Iter<T>`, and `Iter<T>` reads
-- it through a call. A type function of `T` stays unsolved inside a
-- generic function, where every method call reports a missing key.
type function IterSteps(collect)
	local function step(item)
		local maybe = types.optional(item)
		local recv = types.newtable({
			[types.singleton("next")] = types.newfunction({ head = { types.any } }, { head = { maybe } }),
		})
		local test = types.newfunction({ head = { item } }, { head = { types.boolean } })
		local pull = types.newfunction({}, { head = { maybe } })
		local meta = types.newtable({
			[types.singleton("__iter")] = types.newfunction({ head = { types.any } }, { head = { pull } }),
		})
		local iter = types.newtable(nil, nil, meta)
		local function method(name, params, result, generics)
			iter:setproperty(types.singleton(name), types.newfunction({ head = params }, { head = { result } }, generics))
		end
		method("next", { recv }, maybe)
		method("filter", { recv, test }, iter)
		method("take", { recv, types.number }, iter)
		method("skip", { recv, types.number }, iter)
		method("take_while", { recv, test }, iter)
		method("chain", { recv, iter }, iter)
		method("any", { recv, test }, types.boolean)
		method("all", { recv, test }, types.boolean)
		method("find", { recv, test }, maybe)
		method("first", { recv }, maybe)
		method("last", { recv }, maybe)
		method("count", { recv }, types.number)
		method("to_table", { recv }, types.newtable(nil, { index = types.number, readresult = item, writeresult = item }))
		iter:setproperty(types.singleton("collect"), collect)
		local each = types.newfunction({ head = { item } }, {})
		iter:setproperty(types.singleton("for_each"), types.newfunction({ head = { recv, each } }, {}))
		local acc = types.generic("R")
		local fold = types.newfunction({ head = { acc, item } }, { head = { acc } })
		method("reduce", { recv, fold, acc }, acc, { acc })
		return iter, recv
	end
	local t, u = types.generic("T"), types.generic("U")
	local on_t, recv_t = step(t)
	local on_u, recv_u = step(u)
	local function map(recv, from, to, target)
		local f = types.newfunction({ head = { from } }, { head = { to } })
		return types.newfunction({ head = { recv, f } }, { head = { target } }, { to })
	end
	on_t:setproperty(types.singleton("map"), map(recv_t, t, u, on_u))
	on_u:setproperty(types.singleton("map"), map(recv_u, u, t, on_t))
	return types.newfunction({ head = { t } }, { head = { on_t } }, { t })
end

local iter_steps: IterSteps<IterCollect> = nil :: any

--- A lazy iterator: a `next` that yields the next value or nil, with the
--- chain methods on top. Nothing runs until `collect`, `for_each`, or a
--- `for` loop pulls. `Iter.from` takes an array, a function, or anything
--- with `__iter`; `Iter.range(a, b, step)` counts.
export type Iter<T> = typeof(iter_steps((nil :: any) :: T))

local Iter = {}
Iter.__index = Iter

local function iter_of(next)
	return setmetatable({ _next = next }, Iter)
end

-- The result reads `iter_steps` in place of `Iter<T>`: luau-lsp leaves
-- the alias argument unsolved in the module that declares the alias.
-- `Set<T>` and `HashMap<K, T>` take an overload each. In one union with
-- the array form, an array argument bound `T` to `never`, so every callback down
-- the chain read `unknown` and a wrong annotation passed in silence.
type IterFrom =
	(<T>(source: { read [number]: T } | (() -> T?) | Queue<T> | Heap<T>) -> typeof(iter_steps((nil :: any) :: T)))
	& (<T>(source: Set<T>) -> typeof(iter_steps((nil :: any) :: T)))
	& (<K, T>(source: HashMap<K, T>) -> typeof(iter_steps((nil :: any) :: T)))

local function iter_from(source: any): any
	if type(source) == "function" then
		return iter_of(source) :: any
	end
	local mt = getmetatable(source)
	if mt == Iter then
		return source
	end
	-- A map's `__iter` yields the key first, and the iterator takes one
	-- value a step: the map gives its values, as the type says.
	if mt == HashMap then
		local items, key = (source :: any)._items, nil
		return (iter_of(function()
			local k, v = next(items, key)
			key = k
			return v
		end) :: any)
	end
	if type(mt) == "table" and mt.__iter ~= nil then
		local f, s, c = mt.__iter(source)
		return (iter_of(function()
			local value = f(s, c)
			c = value
			return value
		end) :: any)
	end
	local i = 0
	return (iter_of(function()
		i += 1
		return source[i]
	end) :: any)
end

Iter.from = iter_from :: IterFrom

function Iter.range(from: number, to: number, step: number?): typeof(iter_steps(0))
	local by = step or 1
	local at = from - by
	return (iter_of(function()
		at += by
		if (by > 0 and at > to) or (by < 0 and at < to) then
			return nil
		end
		return at
	end) :: any)
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

-- An Iter wraps no table of its own, so `to_table` drains it into a
-- new one: `collect` without the Array methods. The pull spends the
-- iterator, so a second call gives an empty table.
function Iter.to_table(self)
	return setmetatable(self:collect(), nil)
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

--- A namespace `local` is a local of its module. The namespace table
--- reads and writes it through these accessors, so a write on either
--- side reaches the other. A table with a metatable of its own keeps a
--- copy, as before.
function alloy.ns_local(ns: any, key: string, get: () -> any, set: (any) -> ())
	local mt = getmetatable(ns)
	if mt == nil then
		local getters, setters = {}, {}
		mt = {
			__get = getters,
			__set = setters,
			__index = function(_, k)
				local g = getters[k]
				return if g then g() else nil
			end,
			__newindex = function(t, k, v)
				local s = setters[k]
				if s then
					s(v)
				else
					rawset(t, k, v)
				end
			end,
		}
		setmetatable(ns, mt)
	end
	if type(mt) ~= "table" or mt.__get == nil then
		rawset(ns, key, get())
		return
	end
	mt.__get[key] = get
	mt.__set[key] = set
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

-- The emit stores the arguments in declaration order. One parameter
-- reads as that value, several read as a table that names them, and
-- none reads as `true`, the mark that the attribute is there.
local function lookup(data, attr)
	if data == nil then
		return nil
	end
	local value = data[attr.name]
	if type(value) ~= "table" or type(attr.params) ~= "table" then
		return value
	end
	if #attr.params == 0 then
		return true
	end
	if #attr.params == 1 then
		return value[1]
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

--- One value under test, with the matchers of the test runner on it.
--- Every matcher answers the expectation, so a chain reads left to
--- right. The names are jest's, which is what lest writes, so the same
--- source runs against either.
export type Expectation<T> = {
	toBe: (self: Expectation<T>, expected: T) -> Expectation<T>,
	toNotBe: (self: Expectation<T>, expected: T) -> Expectation<T>,
	toEqual: (self: Expectation<T>, expected: T) -> Expectation<T>,
	toNotEqual: (self: Expectation<T>, expected: T) -> Expectation<T>,
	toBeTruthy: (self: Expectation<T>) -> Expectation<T>,
	toBeFalsy: (self: Expectation<T>) -> Expectation<T>,
	toBeNil: (self: Expectation<T>) -> Expectation<T>,
	toNotBeNil: (self: Expectation<T>) -> Expectation<T>,
	toBeA: (self: Expectation<T>, type_name: string) -> Expectation<T>,
	toContain: (self: Expectation<T>, item: any) -> Expectation<T>,
	toHaveLength: (self: Expectation<T>, length: number) -> Expectation<T>,
	toBeGreaterThan: (self: Expectation<T>, expected: number) -> Expectation<T>,
	toBeGreaterThanOrEqual: (self: Expectation<T>, expected: number) -> Expectation<T>,
	toBeLessThan: (self: Expectation<T>, expected: number) -> Expectation<T>,
	toBeLessThanOrEqual: (self: Expectation<T>, expected: number) -> Expectation<T>,
	toBeCloseTo: (self: Expectation<T>, expected: number, precision: number?) -> Expectation<T>,
	toMatch: (self: Expectation<T>, pattern: string) -> Expectation<T>,
	toThrow: (self: Expectation<T>, substring: string?) -> Expectation<T>,
	toNotThrow: (self: Expectation<T>) -> Expectation<T>,
	toMatchSnapshot: (self: Expectation<T>, hint: string?) -> Expectation<T>,
}

local runner_expect: ((any) -> any)? = nil

--- The spec hands the runner's `expect` over before it registers a
--- test, so `$expect` reaches the matchers of the runner the project
--- configures and the emit names none of them.
function alloy.set_expect(f: ((any) -> any)?)
	runner_expect = f
end

--- `$expect(value)`. The runner builds the matchers as plain functions
--- over the value; each one takes `self` here, so a spec calls it with
--- a colon and the chain answers the same expectation.
function alloy.expect<T>(value: T): Expectation<T>
	local build = runner_expect

	if build == nil then
		error("$expect runs under the test runner alone; `alloy test` starts one", 2)
	end

	local matchers: any = build(value)
	local wrapped: any = {}

	for name, matcher in pairs(matchers) do
		wrapped[name] = function(_, ...)
			matcher(...)
			return wrapped
		end
	end

	return wrapped
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

--- What a remote records outside Roblox: one entry per call, with the
--- verb's name and the arguments as `table.pack` holds them.
export type RemoteCalls = { { name: string, args: { n: number, [number]: any } } }

export type Remote = {
	spec: RemoteSpec,
	instance: (RemoteEvent | UnreliableRemoteEvent | RemoteFunction)?,
	calls: RemoteCalls,
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

-- A kind in a layout is a string, `u8`, `str?`, `any`, `inst:Player`, or a
-- table: `{ fields = { { name, kind }, ... }, struct = Metatable? }` for a
-- record, `{ item = kind }` for an array, `{ enum = Enum, tags = { A = 0,
-- B = 1 }, slots = { B = { kind } } }` for an enum with each variant's
-- payload count and the kind of each payload value, each with
-- `optional = true` when nil is fine. A `struct` or an `enum` can be a key
-- of `wire.types` in place of the table. The codec reads each layout once into nodes, so a fire
-- parses no string.
type WireNode = {
	base: string,
	opt: boolean,
	fields: { any }?,
	item: any?,
	struct: any?,
	array: boolean?,
	class: string?,
	choices: { [string]: boolean }?,
	enum: any?,
	tags: { [string]: number }?,
	slots: { [string]: { WireNode } }?,
}

local function wire_node(kind: any): WireNode
	if type(kind) == "string" then
		local opt = string.sub(kind, -1) == "?"
		local base = if opt then string.sub(kind, 1, -2) else kind
		if string.sub(base, 1, 5) == "inst:" then
			return { base = "inst", opt = opt, class = string.sub(base, 6) }
		end
		if string.sub(base, 1, 4) == "val:" then
			return { base = "val", opt = opt, class = string.sub(base, 5) }
		end
		-- `one:Red,Blue`: a unit enum, a string that names a variant.
		if string.sub(base, 1, 4) == "one:" then
			local choices = {}
			for name in string.gmatch(string.sub(base, 5), "[^,]+") do
				choices[name] = true
			end
			return { base = "str", opt = opt, choices = choices }
		end
		return { base = base, opt = opt }
	end
	if kind.enum ~= nil then
		local slots = nil
		if kind.slots ~= nil then
			slots = {}
			for tag, list in kind.slots do
				local nodes = {}
				for i, k in list do
					nodes[i] = wire_node(k)
				end
				slots[tag] = nodes
			end
		end
		return { base = "enum", opt = kind.optional == true, enum = kind.enum, tags = kind.tags, slots = slots }
	end
	local node: WireNode = {
		base = if kind.fields ~= nil then "record" else "array",
		opt = kind.optional == true,
		struct = kind.struct,
		array = kind.array,
	}
	if kind.fields ~= nil then
		local fields = {}
		for i, f in kind.fields do
			fields[i] = { f[1], wire_node(f[2]) }
		end
		node.fields = fields
	else
		node.item = wire_node(kind.item)
	end
	return node
end

local wire_nodes: { [any]: { WireNode } } = setmetatable({}, { __mode = "k" }) :: any

local function wire_compile(layout: { any }): { WireNode }
	local nodes = wire_nodes[layout]
	if nodes == nil then
		nodes = {}
		for i, kind in layout do
			nodes[i] = wire_node(kind)
		end
		wire_nodes[layout] = nodes
	end
	return nodes
end

-- A length or a count takes one byte under 128, as LEB128 writes it.
local function varint_size(n: number): number
	if n < 0x80 then
		return 1
	elseif n < 0x4000 then
		return 2
	elseif n < 0x200000 then
		return 3
	elseif n < 0x10000000 then
		return 4
	end
	return 5
end

local function write_varint(buf: buffer, at: number, n: number): number
	while n >= 0x80 do
		buffer.writeu8(buf, at, n % 0x80 + 0x80)
		n = n // 0x80
		at += 1
	end
	buffer.writeu8(buf, at, n)
	return at + 1
end

local function read_varint(buf: buffer, at: number): (number, number)
	local n, scale = 0, 1
	for _ = 1, 5 do
		local b = buffer.readu8(buf, at)
		at += 1
		n += (b % 0x80) * scale
		if b < 0x80 then
			return n, at
		end
		scale *= 0x80
	end
	error("wire: a length runs past five bytes", 0)
end

-- The bytes one value packs to, or nil and why it does not fit. The why
-- starts with the path under the value, `.hp` or `[3]`, so the report
-- names the part; nothing is built for a value that fits.
local function wire_measure(node: WireNode, v: any): (number?, string?)
	local base = node.base
	if base == "any" or base == "inst" or base == "val" or base == "enum" then
		return 0
	end
	local size = 0
	if node.opt then
		size = 1
		if v == nil then
			return size
		end
	elseif v == nil then
		return nil, ": is nil"
	end
	if base == "record" then
		if type(v) ~= "table" then
			return nil, `: {tostring(v)} is not a table`
		end
		for _, f in node.fields :: { any } do
			local s, why = wire_measure(f[2], v[f[1]])
			if s == nil then
				return nil, `.{f[1]}{why}`
			end
			size += s
		end
		return size
	elseif base == "array" then
		if type(v) ~= "table" then
			return nil, `: {tostring(v)} is not a table`
		end
		local n = #v
		size += varint_size(n)
		local item = node.item :: WireNode
		for i = 1, n do
			local s, why = wire_measure(item, v[i])
			if s == nil then
				return nil, `[{i}]{why}`
			end
			size += s
		end
		return size
	elseif base == "str" then
		if type(v) ~= "string" then
			return nil, `: {tostring(v)} is not a string`
		end
		if node.choices ~= nil and not node.choices[v] then
			return nil, `: {v} is no variant of the enum`
		end
		return size + varint_size(#v) + #v
	elseif base == "bool" then
		if type(v) ~= "boolean" then
			return nil, `: {tostring(v)} is not a boolean`
		end
		return size + 1
	end
	if type(v) ~= "number" then
		return nil, `: {tostring(v)} is not a number`
	end
	local range = WIRE_RANGE[base]
	if range ~= nil and (v % 1 ~= 0 or v < range[1] or v > range[2]) then
		return nil, `: {tostring(v)} does not fit @{base}`
	end
	return size + WIRE_SIZE[base]
end

local function wire_write(node: WireNode, v: any, buf: buffer, at: number, rest: { any }): number
	local base = node.base
	if base == "any" or base == "inst" or base == "val" or base == "enum" then
		rest.n += 1
		rest[rest.n] = v
		return at
	end
	if node.opt then
		buffer.writeu8(buf, at, if v == nil then 0 else 1)
		at += 1
		if v == nil then
			return at
		end
	end
	if base == "record" then
		for _, f in node.fields :: { any } do
			at = wire_write(f[2], v[f[1]], buf, at, rest)
		end
		return at
	elseif base == "array" then
		local n = #v
		at = write_varint(buf, at, n)
		local item = node.item :: WireNode
		for i = 1, n do
			at = wire_write(item, v[i], buf, at, rest)
		end
		return at
	elseif base == "str" then
		at = write_varint(buf, at, #v)
		buffer.writestring(buf, at, v)
		return at + #v
	elseif base == "bool" then
		buffer.writeu8(buf, at, if v then 1 else 0)
		return at + 1
	elseif base == "f64" then
		buffer.writef64(buf, at, v)
	elseif base == "f32" then
		buffer.writef32(buf, at, v)
	elseif base == "u8" then
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
	end
	return at + WIRE_SIZE[base]
end

-- An Instance crosses as the engine's reference. The sender is not
-- trusted, so the reader checks the class the layout names.
local function wire_instance(node: WireNode, v: any): any
	if v == nil then
		if not node.opt then
			error(`wire: a {node.class} is missing`, 0)
		end
	elseif node.base == "val" then
		if typeof(v) ~= node.class then
			error(`wire: {typeof(v)} is not a {node.class}`, 0)
		end
	elseif typeof(v) ~= "Instance" or not (v :: Instance):IsA(node.class :: string) then
		error(`wire: {typeof(v)} is not a {node.class}`, 0)
	end
	return v
end

-- The struct and enum tables a layout names by key, `"types.aly:Stats"`.
-- A field of an imported struct can name a table the importer cannot
-- reach, so the declaring module registers it here when it loads. The
-- key comes from the source path, so it is the same on both sides.
local wire_types: { [string]: any } = {}

-- A struct comes back through its constructor, so a field the wire
-- skipped takes its default; a plain metatable takes the table as it is.
-- A key names a registered table; with none, the table stays plain.
local function wire_struct(struct: any, t: any): any
	if type(struct) == "string" then
		struct = wire_types[struct]
		if struct == nil then
			return t
		end
	end
	local meta = getmetatable(struct)
	if type(meta) == "table" and meta.__call ~= nil then
		return struct(t)
	end
	return setmetatable(t, struct)
end

-- A payload value of an enum crosses inside the variant's table, so the
-- reader of the variant restores it; the function comes below.
local wire_restore: (node: WireNode, v: any) -> any

-- An enum value crosses as Roblox copies a table, which drops the
-- metatable. A unit variant is its name, and a payload variant is a
-- table whose tag the layout lists with a payload; anything else is
-- forged. The table gets the enum's metatable back, and each payload
-- value the layout names is checked and restored by its own kind.
local function wire_variant(node: WireNode, v: any): any
	if v == nil then
		if node.opt then
			return nil
		end
		error("wire: a value is missing", 0)
	end
	local tags = node.tags :: { [string]: number }
	if type(v) == "string" then
		if tags[v] ~= 0 then
			error(`wire: {v} is no variant of the enum`, 0)
		end
		return v
	end
	if type(v) ~= "table" or type(v.tag) ~= "string" or (tags[v.tag] or 0) == 0 then
		error(`wire: {typeof(v)} is no variant of the enum`, 0)
	end
	local slots = node.slots and node.slots[v.tag]
	if slots ~= nil then
		for i, slot in slots do
			local key = `_{i}`
			v[key] = wire_restore(slot, v[key])
		end
	end
	local enum = node.enum
	if type(enum) == "string" then
		enum = wire_types[enum]
	end
	return if enum ~= nil then setmetatable(v, enum) else v
end

local function wire_read(node: WireNode, buf: buffer, at: number, rest: { any }, ri: number): (any, number, number)
	local base = node.base
	if base == "any" then
		return rest[ri], at, ri + 1
	elseif base == "enum" then
		return wire_variant(node, rest[ri]), at, ri + 1
	elseif base == "inst" or base == "val" then
		return wire_instance(node, rest[ri]), at, ri + 1
	end
	if node.opt then
		local present = buffer.readu8(buf, at)
		at += 1
		if present == 0 then
			return nil, at, ri
		end
	end
	if base == "record" then
		local t = {}
		for _, f in node.fields :: { any } do
			t[f[1]], at, ri = wire_read(f[2], buf, at, rest, ri)
		end
		if node.struct ~= nil then
			t = wire_struct(node.struct, t)
		end
		return t, at, ri
	elseif base == "array" then
		local count
		count, at = read_varint(buf, at)
		-- Every item takes a byte or a value beside the buffer, so a
		-- count past both is forged; reading it would hang the server.
		if count > buffer.len(buf) - at + rest.n - ri + 1 then
			error("wire: a count runs past the packet", 0)
		end
		local t = table.create(count)
		local item = node.item :: WireNode
		for i = 1, count do
			t[i], at, ri = wire_read(item, buf, at, rest, ri)
		end
		if node.array then
			setmetatable(t, Array)
		end
		return t, at, ri
	elseif base == "str" then
		local len
		len, at = read_varint(buf, at)
		if len > buffer.len(buf) - at then
			error("wire: a string runs past the packet", 0)
		end
		local s = buffer.readstring(buf, at, len)
		if node.choices ~= nil and not node.choices[s] then
			error(`wire: {s} is no variant of the enum`, 0)
		end
		return s, at + len, ri
	elseif base == "bool" then
		return buffer.readu8(buf, at) == 1, at + 1, ri
	end
	local v
	if base == "f64" then
		v = buffer.readf64(buf, at)
	elseif base == "f32" then
		v = buffer.readf32(buf, at)
	elseif base == "u8" then
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
	end
	return v, at + WIRE_SIZE[base], ri
end

-- A value that crossed as Roblox encodes a table, checked against its
-- node the way the reader checks a buffer. A record comes back with the
-- keys its layout names alone, and with its metatable.
function wire_restore(node: WireNode, v: any): any
	local base = node.base
	if base == "any" then
		return v
	elseif base == "enum" then
		return wire_variant(node, v)
	elseif base == "inst" or base == "val" then
		return wire_instance(node, v)
	end
	if v == nil then
		if node.opt then
			return nil
		end
		error("wire: a value is missing", 0)
	end
	if base == "record" then
		if type(v) ~= "table" then
			error(`wire: {typeof(v)} is not a table`, 0)
		end
		local t = {}
		for _, f in node.fields :: { any } do
			t[f[1]] = wire_restore(f[2], v[f[1]])
		end
		if node.struct ~= nil then
			t = wire_struct(node.struct, t)
		end
		return t
	elseif base == "array" then
		if type(v) ~= "table" then
			error(`wire: {typeof(v)} is not a table`, 0)
		end
		local n = #v
		local t = table.create(n)
		local item = node.item :: WireNode
		for i = 1, n do
			t[i] = wire_restore(item, v[i])
		end
		if node.array then
			setmetatable(t, Array)
		end
		return t
	end
	local s, why = wire_measure(node, v)
	if s == nil then
		error(`wire{why}`, 0)
	end
	return v
end

local wire = {}

--- The tables a layout names by key. A module registers a struct or
--- an enum here when a field of another struct names it.
wire.types = wire_types

--- Packs the arguments of one fire by the layout: the buffer, then the
--- values the layout marks `any`, in the order the packer met them.
function wire.pack(layout: { any }, args: { any }, name: string?): (buffer, { any })
	local nodes = wire_compile(layout)
	local size = 0
	for i, node in nodes do
		local s, why = wire_measure(node, args[i])
		if s == nil then
			error(`{name or "remote"}: argument {i}{why}`, 0)
		end
		size += s
	end
	local buf = buffer.create(size)
	local at = 0
	local rest = { n = 0 }
	for i, node in nodes do
		at = wire_write(node, args[i], buf, at, rest)
	end
	return buf, rest
end

--- Reads the arguments back: the buffer and the `any` values beside it,
--- as the packer laid them out. A packet that does not fit raises.
function wire.unpack(layout: { any }, buf: buffer, rest: { any }): { any }
	local nodes = wire_compile(layout)
	if rest.n == nil then
		rest.n = #rest
	end
	local args = { n = #nodes }
	local at = 0
	local ri = 1
	for i, node in nodes do
		args[i], at, ri = wire_read(node, buf, at, rest, ri)
	end
	return args
end

--- Checks arguments that crossed as tables against the layout and puts
--- their metatables back. A value that does not fit raises.
function wire.restore(layout: { any }, args: { any }): { any }
	local nodes = wire_compile(layout)
	local out = { n = math.max(args.n or #args, #nodes) }
	for i = 1, out.n do
		local node = nodes[i]
		out[i] = if node ~= nil then wire_restore(node, args[i]) else args[i]
	end
	return out
end

--- Checks arguments against the layout before a send, as the packer
--- does; the report names the part that does not fit.
function wire.check(layout: { any }, args: { any }, name: string?)
	for i, node in wire_compile(layout) do
		local s, why = wire_measure(node, args[i])
		if s == nil then
			error(`{name or "remote"}: argument {i}{why}`, 0)
		end
	end
end

alloy.wire = wire

function alloy.remote(spec: any): Remote
	local is_function = spec.kind == "function"
	local attrs = spec.attrs or {}
	-- `@unreliable` picks the event class that may drop or reorder.
	local class = if is_function then "RemoteFunction" elseif attrs.unreliable then "UnreliableRemoteEvent" else "RemoteEvent"
	local inst: any = nil
	local remote = { spec = spec }
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

	-- Each default by the position of its parameter, read once.
	local default_at = {}
	for name, value in pairs(spec.defaults or {}) do
		for i, pname in ipairs(spec.params) do
			if pname == name then
				default_at[i] = value
			end
		end
	end

	local function fill_defaults(args)
		for i, value in default_at do
			if args[i] == nil then
				args[i] = value
			end
		end
		return args
	end

	-- The arguments as they cross: packed into a buffer by the layout,
	-- sent as Roblox encodes them and checked on arrival (`pack =
	-- false`), or, with no layout, as given.
	local layout = spec.wire
	local packs = layout ~= nil and spec.pack ~= false

	local function outgoing(...)
		local args = fill_defaults(table.pack(...))
		if layout == nil then
			return args
		end
		if not packs then
			wire.check(layout, args, spec.name)
			return args
		end
		local buf, rest = wire.pack(layout, args, spec.name)
		local out = { n = 1 + rest.n, buf }
		for i = 1, rest.n do
			out[i + 1] = rest[i]
		end
		return out
	end

	-- The arguments of one packet, or nil when the packet does not fit
	-- the layout. The other side may be anyone, so a forged packet drops
	-- here instead of raising in the handler.
	local function incoming(...)
		if layout == nil then
			return fill_defaults(table.pack(...))
		end
		local ok, args
		if packs then
			local buf = ...
			if type(buf) ~= "buffer" then
				return nil
			end
			ok, args = pcall(wire.unpack, layout, buf, table.pack(select(2, ...)))
		else
			ok, args = pcall(wire.restore, layout, table.pack(...))
		end
		if not ok then
			return nil
		end
		return fill_defaults(args)
	end

	-- `@validate`, on arguments that decoded. The rate limit runs before
	-- the decode, so a flood costs no reading.
	local function valid(player, args)
		return validate == nil or validate(player, table.unpack(args, 1, args.n))
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

	-- Nothing the engine owns is touched until the first call. A spec
	-- requires the module before it calls `alloy.set_testing`, so a
	-- service lookup at load time broke every test in the file.
	local bound = false

	local function bind()
		if bound then
			return
		end
		bound = true

		-- Outside Roblox a remote has nothing to send on. Each call goes
		-- on `remote.calls` instead, so a test that fires one still runs.
		if testing or game == nil then
			local calls = {}
			remote.calls = calls
			local function record(name: string)
				return function(...)
					table.insert(calls, { name = name, args = table.pack(...) })
					return nil
				end
			end
			-- A send packs its arguments first, as it does under
			-- Roblox, so a value that does not fit its width raises
			-- here too. The server's send carries the player first.
			local function sent(name: string, lead: number)
				local send = record(name)
				return function(...)
					outgoing(select(lead + 1, ...))
					return send(...)
				end
			end
			local lead = if spec.from_server and not spec.from_client then 1 else 0
			remote.fire = sent("fire", lead)
			remote.fire_all = sent("fire_all", 0)
			remote.fire_except = sent("fire_except", 1)
			remote.on = record("on")
			remote.once = record("once")
			remote.on_ratelimited = record("on_ratelimited")
			local call = sent("call", lead)
			local answer = function(...)
				call(...)
				return alloy.future(function()
					return nil
				end)
			end
			remote.call = answer
			remote.wait = answer

			return
		end

		inst = remote_instance(spec.name, class)
		remote.instance = inst

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
						if not allowed(player) then
							return nil
						end
						local args = incoming(...)
						if args == nil or not valid(player, args) then
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
					if not allowed(player) then
						return
					end
					local args = incoming(...)
					if args ~= nil and valid(player, args) then
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
						if args == nil then
							return nil
						end
						return handler(table.unpack(args, 1, args.n))
					end
					return
				end
				return inst.OnClientEvent:Connect(function(...)
					local args = incoming(...)
					if args ~= nil then
						handler(table.unpack(args, 1, args.n))
					end
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
	end

	-- A read of any method binds the remote, so the first call is where
	-- the engine, or the record of the call, comes in.
	setmetatable(remote, {
		__index = function(t, key)
			bind()

			return rawget(t, key)
		end,
	})

	return (remote :: any) :: Remote
end

return table.freeze(alloy)
