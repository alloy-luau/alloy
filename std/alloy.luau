--!nonstrict
-- The Alloy runtime. One plain Luau module, required by emitted code, and
-- usable from vanilla Luau. Emitted code reaches it as `__alloy`.

local alloy = {}

-- ---------------------------------------------------------------------------
-- Result: a tagged table. `tag` is "Ok" or "Err"; `_1` holds the payload.
-- ---------------------------------------------------------------------------

export type Result<T, E> = {
	tag: "Ok" | "Err",
	_1: T | E,
	unwrap: (self: any) -> T,
	expect: (self: any, message: string) -> T,
	unwrap_or: (self: any, default: T) -> T,
	map: (self: any, f: (T) -> any) -> any,
	map_err: (self: any, f: (E) -> any) -> any,
	is_ok: (self: any) -> boolean,
	is_err: (self: any) -> boolean,
	ok: (self: any) -> T?,
	trace: string?,
}

local Result = {}
Result.__index = Result

local function Ok<T, E>(value: T): Result<T, E>
	return table.freeze(setmetatable({ tag = "Ok", _1 = value }, Result)) :: any
end

local function Err<T, E>(err: E, trace: string?): Result<T, E>
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
function Result.pcall(f, ...)
	local packed = table.pack(xpcall(f, function(e)
		return { err = e, trace = debug.traceback(tostring(e), 2) }
	end, ...))
	if packed[1] then
		return Ok(packed[2])
	end
	local failure = packed[2]
	return Err(failure.err, failure.trace)
end

alloy.Result = Result
alloy.Ok = Ok
alloy.Err = Err

--- `try do ... end`: a block that returns Ok on a plain return and passes a
--- Result through as is.
function alloy.try_block(f: () -> any)
	local value = f()
	if type(value) == "table" and getmetatable(value) == Result then
		return value
	end
	return Ok(value)
end

-- ---------------------------------------------------------------------------
-- Future: a memoized task with the awaitable shape `andThen`.
-- ---------------------------------------------------------------------------

export type Future<T> = {
	andThen: (self: any, on_resolve: ((T) -> ())?, on_reject: ((any) -> ())?) -> any,
	cancel: (self: any) -> (),
	is_settled: (self: any) -> boolean,
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
function alloy.future<T>(f: (...any) -> T, ...: any): Future<T>
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

function Future.reject(err: any): Future<any>
	local self = new_future()
	settle(self, REJECTED, err, nil)
	return (self :: any) :: Future<any>
end

function Future.delay(seconds: number)
	return alloy.future(function()
		task_lib.wait(seconds)
		return nil
	end)
end

function Future.andThen(self, on_resolve, on_reject)
	local function run()
		if self.state == RESOLVED and on_resolve then
			on_resolve(self.value)
		elseif self.state ~= RESOLVED and on_reject then
			on_reject(self.value)
		end
	end
	if self.state == PENDING then
		table.insert(self.waiters, run)
	else
		task_lib.spawn(run)
	end
	return self
end

function Future.cancel(self)
	settle(self, CANCELLED, "cancelled", nil)
end

function Future.is_settled(self)
	return self.state ~= PENDING
end

--- `await`: a Future, or anything with `andThen`. Yields until settled,
--- returns the value, rethrows a stored error with its trace.
function alloy.await<T>(target: Future<T>): T
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

--- `try await`: a failed Future becomes an Err instead of a throw.
function alloy.try_await(target)
	local ok, value = pcall(alloy.await, target)
	if ok then
		return Ok(value)
	end
	return Err(value)
end

function Future.all<T>(futures: { Future<T> }): Future<{ T }>
	return (alloy.future(function()
		local out = {}
		for i, f in futures do
			out[i] = alloy.await(f)
		end
		return out
	end) :: any) :: Future<{ T }>
end

function Future.race<T>(futures: { Future<T> }): Future<T>
	local winner = new_future()
	for _, f in futures do
		f:andThen(function(v)
			settle(winner, RESOLVED, v, nil)
		end, function(e)
			settle(winner, REJECTED, e, nil)
		end)
	end
	return (winner :: any) :: Future<T>
end

function Future.any<T>(futures: { Future<T> }): Future<T>
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
	return (winner :: any) :: Future<T>
end

function Future.all_settled<T>(futures: { Future<T> }): Future<{ Result<T, any> }>
	return (alloy.future(function()
		local out = {}
		for i, f in futures do
			out[i] = alloy.try_await(f)
		end
		return out
	end) :: any) :: Future<{ Result<T, any> }>
end

alloy.Future = Future

-- ---------------------------------------------------------------------------
-- Array: a table with the shared metatable, so `xs:map(f)` works.
-- ---------------------------------------------------------------------------

export type Array<T> = {
	[number]: T,
	len: (self: Array<T>) -> number,
	is_empty: (self: Array<T>) -> boolean,
	push: (self: Array<T>, ...T) -> (),
	pop: (self: Array<T>) -> T?,
	first: (self: Array<T>) -> T?,
	last: (self: Array<T>) -> T?,
	map: <U>(self: Array<T>, f: (T, number) -> U) -> Array<U>,
	filter: (self: Array<T>, f: (T, number) -> boolean) -> Array<T>,
	find: (self: Array<T>, f: (T, number) -> boolean) -> T?,
	find_index: (self: Array<T>, f: (T, number) -> boolean) -> number?,
	contains: (self: Array<T>, value: T) -> boolean,
	index_of: (self: Array<T>, value: T) -> number?,
	for_each: (self: Array<T>, f: (T, number) -> ()) -> (),
	reduce: <U>(self: Array<T>, f: (U, T, number) -> U, init: U) -> U,
	slice: (self: Array<T>, from: number, to: number?) -> Array<T>,
	concat: (self: Array<T>, other: Array<T>) -> Array<T>,
	reverse: (self: Array<T>) -> Array<T>,
	sort_by: (self: Array<T>, f: (T, T) -> boolean) -> Array<T>,
	join: (self: Array<T>, sep: string?) -> string,
}

-- `read T[]`: the Array methods that do not mutate.
export type ReadArray<T> = {
	read [number]: T,
	len: (self: any) -> number,
	is_empty: (self: any) -> boolean,
	first: (self: any) -> T?,
	last: (self: any) -> T?,
	map: <U>(self: any, f: (T, number) -> U) -> Array<U>,
	filter: (self: any, f: (T, number) -> boolean) -> Array<T>,
	find: (self: any, f: (T, number) -> boolean) -> T?,
	find_index: (self: any, f: (T, number) -> boolean) -> number?,
	contains: (self: any, value: T) -> boolean,
	index_of: (self: any, value: T) -> number?,
	for_each: (self: any, f: (T, number) -> ()) -> (),
	reduce: <U>(self: any, f: (U, T, number) -> U, init: U) -> U,
	slice: (self: any, from: number, to: number?) -> Array<T>,
	concat: (self: any, other: Array<T>) -> Array<T>,
	join: (self: any, sep: string?) -> string,
}

-- `write T[]`: a sink. Push and index assignment only.
export type WriteArray<T> = {
	[number]: T,
	push: (self: any, ...T) -> (),
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

function Array.map(xs, f)
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

alloy.Array = Array

-- ---------------------------------------------------------------------------
-- HashMap and Set: Alloy structs with methods.
-- ---------------------------------------------------------------------------

export type HashMap<K, V> = {
	get: (self: HashMap<K, V>, key: K) -> V?,
	set: (self: HashMap<K, V>, key: K, value: V) -> (),
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
	return pairs(self._items)
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

alloy.HashMap = HashMap

export type Set<T> = {
	add: (self: Set<T>, value: T) -> boolean,
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
type function __mapped_optional(T)
	local props: { [any]: any } = {}
	for k, v in T:properties() do
		local value: any = types.unionof((v.read or v.write) :: any, types.singleton(nil))
		props[k] = { read = value, write = value }
	end
	return types.newtable(props)
end

type function __mapped_read(T)
	local props: { [any]: any } = {}
	for k, v in T:properties() do
		local value: any = (v.read or v.write) :: any
		props[k] = { read = value }
	end
	return types.newtable(props)
end

type function __mapped_write(T)
	local props: { [any]: any } = {}
	for k, v in T:properties() do
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

export type Display = { to_string: (self: any) -> string }
export type Debug = { debug: (self: any) -> string }
export type Clone<T = any> = { clone: (self: any) -> T }
export type Eq = { eq: (self: any, other: any) -> boolean }
export type Ord = { lt: (self: any, other: any) -> boolean, le: (self: any, other: any) -> boolean }
export type Add<T = any> = { add: (self: any, other: any) -> T }
export type Sub<T = any> = { sub: (self: any, other: any) -> T }
export type Mul<T = any> = { mul: (self: any, other: any) -> T }
export type Div<T = any> = { div: (self: any, other: any) -> T }
export type Serialize = { serialize: (self: any) -> any }

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
}

export type Signal<T...> = {
	Connect: (self: Signal<T...>, handler: (T...) -> ()) -> SignalConnection,
	Once: (self: Signal<T...>, handler: (T...) -> ()) -> SignalConnection,
	Wait: (self: Signal<T...>, timeout: number?) -> T...,
	Fire: (self: Signal<T...>, T...) -> (),
	FireDeferred: (self: Signal<T...>, T...) -> (),
	DisconnectAll: (self: Signal<T...>) -> (),
	Destroy: (self: Signal<T...>) -> (),
}

--- Anything with `Connect` or `connect`: a Signal, an RBXScriptSignal, or a
--- third party signal.
export type Signalish<T...> = {
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
	if timeout ~= nil and task ~= nil then
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
function Signal.collect<T...>(source: Signalish<T...>): (() -> T..., SignalConnection)
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
	return (next_event :: any) :: () -> T..., (connection :: any) :: SignalConnection
end

alloy.Signal = Signal

-- ---------------------------------------------------------------------------
-- Operators and sugar.
-- ---------------------------------------------------------------------------

--- `x in t`, dispatched on the container.
function alloy.contains(container, value)
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
function alloy.init(object, fields)
	for k, v in pairs(fields) do
		object[k] = v
	end
	return object
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
function alloy.call(receiver, name: string, ...)
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

function alloy.attribute(name: string, targets, params)
	return table.freeze({ name = name, targets = targets, params = params })
end

--- Attributes on a struct or enum, from its declaration.
function alloy.attrs(target, data)
	target.__attrs = data
	return target
end

--- Attributes on a function value.
function alloy.attach(f, data)
	attached[f] = data
	return f
end

local Attributes = {}

local function lookup(data, attr)
	if data == nil then
		return nil
	end
	return data[attr.name]
end

function Attributes.get(target, attr)
	if type(target) == "function" then
		return lookup(attached[target], attr)
	end
	local a = type(target) == "table" and rawget(target, "__attrs")
	return lookup(a and a.own, attr)
end

function Attributes.field(struct, field: string, attr)
	local a = type(struct) == "table" and rawget(struct, "__attrs")
	return lookup(a and a.fields and a.fields[field], attr)
end

function Attributes.variant(enum, variant: string, attr)
	local a = type(enum) == "table" and rawget(enum, "__attrs")
	return lookup(a and a.variants and a.variants[variant], attr)
end

function Attributes.of(target)
	if type(target) == "function" then
		return attached[target] or {}
	end
	local a = type(target) == "table" and rawget(target, "__attrs")
	return (a and a.own) or {}
end

--- Iterates the fields of a struct that carry an attribute.
function Attributes.fields(struct, attr)
	local a = type(struct) == "table" and rawget(struct, "__attrs")
	local fields = (a and a.fields) or {}
	local out = {}
	for name, data in pairs(fields) do
		local v = data[attr.name]
		if v ~= nil then
			out[name] = v
		end
	end
	return pairs(out)
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
	{ Disconnect: (self: any) -> () }
	| { disconnect: (self: any) -> () }
	| { Destroy: (self: any) -> () }
	| { destroy: (self: any) -> () }

--- `delete x`. A connection disconnects, anything else destroys; the
--- Roblox spelling wins over the lowercase one, so a user class with
--- either spelling works. An Instance is asked for nothing it lacks,
--- since indexing a missing member throws on it.
function alloy.delete(target: Deletable)
	local kind = typeof(target)
	if kind == "Instance" then
		(target :: any):Destroy()
		return
	end
	if kind == "RBXScriptConnection" then
		(target :: any):Disconnect()
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

function alloy.remote(spec)
	local is_function = spec.kind == "function"
	local inst = remote_instance(spec.name, if is_function then "RemoteFunction" else "RemoteEvent")
	local remote = { spec = spec, instance = inst }
	local limiter = spec.attrs and spec.attrs.ratelimit
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

	if run_service():IsServer() then
		function remote.fire(player, ...)
			inst:FireClient(player, ...)
		end
		function remote.fire_all(...)
			inst:FireAllClients(...)
		end
		function remote.fire_except(except, ...)
			for _, p in game:GetService("Players"):GetPlayers() do
				if p ~= except then
					inst:FireClient(p, ...)
				end
			end
		end
		function remote.on(handler)
			if is_function then
				inst.OnServerInvoke = function(player, ...)
					if not allowed(player) then
						return nil
					end
					local result = handler(player, table.unpack(fill_defaults(table.pack(...))))
					if type(result) == "table" and getmetatable(result) == Future then
						return alloy.await(result)
					end
					return result
				end
				return
			end
			return inst.OnServerEvent:Connect(function(player, ...)
				if allowed(player) then
					handler(player, table.unpack(fill_defaults(table.pack(...))))
				end
			end)
		end
		function remote.on_ratelimited(handler)
			on_limited = handler
		end
		function remote.call(player, ...)
			local args = table.pack(...)
			return alloy.future(function()
				return inst:InvokeClient(player, table.unpack(args, 1, args.n))
			end)
		end
	else
		function remote.fire(...)
			inst:FireServer(...)
		end
		function remote.on(handler)
			if is_function then
				inst.OnClientInvoke = function(...)
					return handler(...)
				end
				return
			end
			return inst.OnClientEvent:Connect(function(...)
				handler(table.unpack(fill_defaults(table.pack(...))))
			end)
		end
		function remote.call(...)
			local args = table.pack(...)
			return alloy.future(function()
				return inst:InvokeServer(table.unpack(args, 1, args.n))
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

	return remote
end

return table.freeze(alloy)
