--!nonstrict
-- Engine doubles for tests on a plain Luau VM. `alloy test` requires
-- this before a spec, and it fills in what the engine provides when
-- `game` is nil: the datatypes shared code touches, `task`, `Enum`,
-- and a small Instance tree. In Studio it does nothing. The doubles
-- hold the shape the code reads, not the engine's behavior: a
-- `Vector3` adds and measures, an `Instance` parents and finds
-- children, a `task.wait` parks the thread.

-- Under the engine every name is the engine's own.
if game ~= nil then
	return {
		typeof = typeof,
		Vector3 = Vector3,
		Vector2 = Vector2,
		CFrame = CFrame,
		Color3 = Color3,
		UDim = UDim,
		UDim2 = UDim2,
		NumberRange = NumberRange,
		Random = Random,
		Enum = Enum,
		Instance = Instance,
		task = task,
		game = game,
		workspace = workspace,
		warn = warn,
		step = function()
			return 0
		end,
	}
end

local shim = {}

-- ---------------------------------------------------------------------------
-- typeof: the doubles answer with the engine's names.
-- ---------------------------------------------------------------------------

local kinds = setmetatable({}, { __mode = "k" })
local raw_typeof = typeof

local function typeof_double(value)
	local kind = kinds[value]
	if kind ~= nil then
		return kind
	end
	return raw_typeof(value)
end

typeof = typeof_double

local function mark(value, kind)
	kinds[value] = kind
	return value
end

-- ---------------------------------------------------------------------------
-- Vector3, Vector2, CFrame
-- ---------------------------------------------------------------------------

local Vector3 = {}
Vector3.__index = Vector3

local function v3(x, y, z)
	local self = setmetatable({ X = x or 0, Y = y or 0, Z = z or 0 }, Vector3)
	self.Magnitude = math.sqrt(self.X * self.X + self.Y * self.Y + self.Z * self.Z)
	return mark(table.freeze(self), "Vector3")
end

function Vector3.new(x, y, z)
	return v3(x, y, z)
end

Vector3.zero = v3(0, 0, 0)
Vector3.one = v3(1, 1, 1)
Vector3.xAxis = v3(1, 0, 0)
Vector3.yAxis = v3(0, 1, 0)
Vector3.zAxis = v3(0, 0, 1)

function Vector3.__add(a, b)
	return v3(a.X + b.X, a.Y + b.Y, a.Z + b.Z)
end

function Vector3.__sub(a, b)
	return v3(a.X - b.X, a.Y - b.Y, a.Z - b.Z)
end

function Vector3.__mul(a, b)
	if type(a) == "number" then
		return v3(a * b.X, a * b.Y, a * b.Z)
	elseif type(b) == "number" then
		return v3(a.X * b, a.Y * b, a.Z * b)
	end
	return v3(a.X * b.X, a.Y * b.Y, a.Z * b.Z)
end

function Vector3.__div(a, b)
	if type(b) == "number" then
		return v3(a.X / b, a.Y / b, a.Z / b)
	end
	return v3(a.X / b.X, a.Y / b.Y, a.Z / b.Z)
end

function Vector3.__unm(a)
	return v3(-a.X, -a.Y, -a.Z)
end

function Vector3.__eq(a, b)
	return a.X == b.X and a.Y == b.Y and a.Z == b.Z
end

function Vector3.__tostring(a)
	return `{a.X}, {a.Y}, {a.Z}`
end

function Vector3.Dot(a, b)
	return a.X * b.X + a.Y * b.Y + a.Z * b.Z
end

function Vector3.Cross(a, b)
	return v3(a.Y * b.Z - a.Z * b.Y, a.Z * b.X - a.X * b.Z, a.X * b.Y - a.Y * b.X)
end

function Vector3.Lerp(a, b, t)
	return a + (b - a) * t
end

function Vector3.FuzzyEq(a, b, epsilon)
	local e = epsilon or 1e-5
	return math.abs(a.X - b.X) <= e and math.abs(a.Y - b.Y) <= e and math.abs(a.Z - b.Z) <= e
end

local Vector2 = {}
Vector2.__index = Vector2

local function v2(x, y)
	local self = setmetatable({ X = x or 0, Y = y or 0 }, Vector2)
	self.Magnitude = math.sqrt(self.X * self.X + self.Y * self.Y)
	return mark(table.freeze(self), "Vector2")
end

function Vector2.new(x, y)
	return v2(x, y)
end

Vector2.zero = v2(0, 0)
Vector2.one = v2(1, 1)

function Vector2.__add(a, b)
	return v2(a.X + b.X, a.Y + b.Y)
end

function Vector2.__sub(a, b)
	return v2(a.X - b.X, a.Y - b.Y)
end

function Vector2.__mul(a, b)
	if type(a) == "number" then
		return v2(a * b.X, a * b.Y)
	elseif type(b) == "number" then
		return v2(a.X * b, a.Y * b)
	end
	return v2(a.X * b.X, a.Y * b.Y)
end

function Vector2.__eq(a, b)
	return a.X == b.X and a.Y == b.Y
end

function Vector2.__tostring(a)
	return `{a.X}, {a.Y}`
end

local CFrame = {}
CFrame.__index = CFrame

local function cf(position)
	local self = setmetatable({ Position = position, X = position.X, Y = position.Y, Z = position.Z }, CFrame)
	return mark(table.freeze(self), "CFrame")
end

function CFrame.new(x, y, z)
	if typeof(x) == "Vector3" then
		return cf(x)
	end
	return cf(v3(x, y, z))
end

CFrame.identity = cf(v3(0, 0, 0))

function CFrame.__add(a, b)
	return cf(a.Position + b)
end

function CFrame.__sub(a, b)
	return cf(a.Position - b)
end

function CFrame.__mul(a, b)
	if typeof(b) == "CFrame" then
		return cf(a.Position + b.Position)
	end
	return cf(a.Position + b)
end

function CFrame.__tostring(a)
	return tostring(a.Position)
end

-- ---------------------------------------------------------------------------
-- Color3, UDim, UDim2, NumberRange
-- ---------------------------------------------------------------------------

local Color3 = {}
Color3.__index = Color3

local function c3(r, g, b)
	return mark(table.freeze(setmetatable({ R = r or 0, G = g or 0, B = b or 0 }, Color3)), "Color3")
end

function Color3.new(r, g, b)
	return c3(r, g, b)
end

function Color3.fromRGB(r, g, b)
	return c3((r or 0) / 255, (g or 0) / 255, (b or 0) / 255)
end

function Color3.fromHSV(h, s, v)
	local i = math.floor(h * 6)
	local f = h * 6 - i
	local p, q, t = v * (1 - s), v * (1 - f * s), v * (1 - (1 - f) * s)
	local m = i % 6
	if m == 0 then
		return c3(v, t, p)
	elseif m == 1 then
		return c3(q, v, p)
	elseif m == 2 then
		return c3(p, v, t)
	elseif m == 3 then
		return c3(p, q, v)
	elseif m == 4 then
		return c3(t, p, v)
	end
	return c3(v, p, q)
end

function Color3.__eq(a, b)
	return a.R == b.R and a.G == b.G and a.B == b.B
end

function Color3.__tostring(a)
	return `{a.R}, {a.G}, {a.B}`
end

function Color3.Lerp(a, b, t)
	return c3(a.R + (b.R - a.R) * t, a.G + (b.G - a.G) * t, a.B + (b.B - a.B) * t)
end

local UDim = {}
UDim.__index = UDim

function UDim.new(scale, offset)
	return mark(table.freeze(setmetatable({ Scale = scale or 0, Offset = offset or 0 }, UDim)), "UDim")
end

function UDim.__add(a, b)
	return UDim.new(a.Scale + b.Scale, a.Offset + b.Offset)
end

function UDim.__eq(a, b)
	return a.Scale == b.Scale and a.Offset == b.Offset
end

local UDim2 = {}
UDim2.__index = UDim2

function UDim2.new(xs, xo, ys, yo)
	if typeof(xs) == "UDim" then
		return mark(table.freeze(setmetatable({ X = xs, Y = xo, Width = xs, Height = xo }, UDim2)), "UDim2")
	end
	local x, y = UDim.new(xs, xo), UDim.new(ys, yo)
	return mark(table.freeze(setmetatable({ X = x, Y = y, Width = x, Height = y }, UDim2)), "UDim2")
end

function UDim2.fromScale(x, y)
	return UDim2.new(x, 0, y, 0)
end

function UDim2.fromOffset(x, y)
	return UDim2.new(0, x, 0, y)
end

function UDim2.__add(a, b)
	return UDim2.new(a.X + b.X, a.Y + b.Y)
end

function UDim2.__eq(a, b)
	return a.X == b.X and a.Y == b.Y
end

local NumberRange = {}
NumberRange.__index = NumberRange

function NumberRange.new(min, max)
	return mark(table.freeze(setmetatable({ Min = min, Max = max or min }, NumberRange)), "NumberRange")
end

-- ---------------------------------------------------------------------------
-- Random: the engine's generator, seeded, so a test can pin its rolls.
-- ---------------------------------------------------------------------------

local Random = {}
Random.__index = Random

-- Park and Miller's generator: the state stays below 2^31, so every step
-- is exact in a double.
local MODULUS = 2147483647

function Random.new(seed)
	local state = math.floor(seed or os.clock() * 1000000) % MODULUS
	if state <= 0 then
		state += MODULUS - 1
	end
	return mark(setmetatable({ state = state }, Random), "Random")
end

local function step(self)
	self.state = (self.state * 16807) % MODULUS
	return self.state / MODULUS
end

function Random:NextNumber(min, max)
	local lo, hi = min or 0, max or 1
	return lo + step(self) * (hi - lo)
end

function Random:NextInteger(min, max)
	return min + math.floor(step(self) * (max - min + 1))
end

function Random:Clone()
	return mark(setmetatable({ state = self.state }, Random), "Random")
end

-- ---------------------------------------------------------------------------
-- Enum: any `Enum.Kind.Item` is an item that knows its names.
-- ---------------------------------------------------------------------------

local enum_items = {}

local function enum_item(kind, name)
	local key = kind .. "." .. name
	local item = enum_items[key]
	if item == nil then
		item = mark(table.freeze({ Name = name, EnumType = kind, Value = 0 }), "EnumItem")
		enum_items[key] = item
	end
	return item
end

local Enum = setmetatable({}, {
	__index = function(_, kind)
		return setmetatable({}, {
			__index = function(_, name)
				return enum_item(kind, name)
			end,
		})
	end,
})

-- ---------------------------------------------------------------------------
-- Signals and Instances: a tree that parents, finds, and destroys.
-- ---------------------------------------------------------------------------

local Connection = {}
Connection.__index = Connection

function Connection.Disconnect(self)
	self.Connected = false
	local handlers = self.signal.handlers
	for i, h in ipairs(handlers) do
		if h == self then
			table.remove(handlers, i)
			break
		end
	end
end

Connection.disconnect = Connection.Disconnect

local Signal = {}
Signal.__index = Signal

local function new_signal()
	return mark(setmetatable({ handlers = {} }, Signal), "RBXScriptSignal")
end

function Signal.Connect(self, f)
	local c = mark(setmetatable({ signal = self, f = f, Connected = true }, Connection), "RBXScriptConnection")
	table.insert(self.handlers, c)
	return c
end

Signal.connect = Signal.Connect

function Signal.Once(self, f)
	local c
	c = self:Connect(function(...)
		c:Disconnect()
		f(...)
	end)
	return c
end

function Signal.Fire(self, ...)
	for _, c in ipairs(table.clone(self.handlers)) do
		if c.Connected then
			c.f(...)
		end
	end
end

function Signal.Wait(self)
	local thread = coroutine.running()
	local c
	c = self:Connect(function(...)
		c:Disconnect()
		coroutine.resume(thread, ...)
	end)
	return coroutine.yield()
end

Signal.wait = Signal.Wait

local Instance = {}
local instance_meta = {}

-- Superclasses the doubles know, so `IsA("BasePart")` holds for a Part.
local parents = {
	Part = "BasePart",
	MeshPart = "BasePart",
	SpawnLocation = "BasePart",
	BasePart = "PVInstance",
	Model = "PVInstance",
	Folder = "Instance",
	PVInstance = "Instance",
	TextLabel = "GuiObject",
	TextButton = "GuiObject",
	Frame = "GuiObject",
	ScrollingFrame = "GuiObject",
	ImageLabel = "GuiObject",
	GuiObject = "GuiBase2d",
	GuiBase2d = "Instance",
	ScreenGui = "LayerCollector",
	BillboardGui = "LayerCollector",
	LayerCollector = "Instance",
	Players = "Instance",
	Workspace = "Instance",
	Player = "Instance",
	Humanoid = "Instance",
	RemoteEvent = "Instance",
	RemoteFunction = "Instance",
}

local function is_a(class, wanted)
	local c = class
	while c ~= nil do
		if c == wanted then
			return true
		end
		c = parents[c]
	end
	return wanted == "Instance"
end

local function add_child(parent, child)
	if parent == nil then
		return
	end
	table.insert(parent.__children, child)
	parent.ChildAdded:Fire(child)
end

local function remove_child(parent, child)
	if parent == nil then
		return
	end
	for i, c in ipairs(parent.__children) do
		if c == child then
			table.remove(parent.__children, i)
			break
		end
	end
	parent.ChildRemoved:Fire(child)
end

local methods = {}

function methods.IsA(self, class)
	return is_a(self.ClassName, class)
end

function methods.GetChildren(self)
	return table.clone(self.__children)
end

function methods.GetDescendants(self)
	local out = {}
	local function walk(node)
		for _, c in ipairs(node.__children) do
			table.insert(out, c)
			walk(c)
		end
	end
	walk(self)
	return out
end

function methods.FindFirstChild(self, name, recursive)
	for _, c in ipairs(self.__children) do
		if c.Name == name then
			return c
		end
	end
	if recursive then
		for _, c in ipairs(self.__children) do
			local found = c:FindFirstChild(name, true)
			if found then
				return found
			end
		end
	end
	return nil
end

function methods.FindFirstChildOfClass(self, class)
	for _, c in ipairs(self.__children) do
		if c.ClassName == class then
			return c
		end
	end
	return nil
end

function methods.FindFirstChildWhichIsA(self, class)
	for _, c in ipairs(self.__children) do
		if c:IsA(class) then
			return c
		end
	end
	return nil
end

function methods.WaitForChild(self, name)
	return self:FindFirstChild(name)
end

function methods.FindFirstAncestor(self, name)
	local p = self.Parent
	while p ~= nil do
		if p.Name == name then
			return p
		end
		p = p.Parent
	end
	return nil
end

function methods.IsDescendantOf(self, other)
	local p = self.Parent
	while p ~= nil do
		if p == other then
			return true
		end
		p = p.Parent
	end
	return false
end

function methods.GetAttribute(self, name)
	return self.__attributes[name]
end

function methods.SetAttribute(self, name, value)
	self.__attributes[name] = value
	self.AttributeChanged:Fire(name)
end

function methods.GetAttributes(self)
	return table.clone(self.__attributes)
end

function methods.GetFullName(self)
	local parts = { self.Name }
	local p = self.Parent
	while p ~= nil do
		table.insert(parts, 1, p.Name)
		p = p.Parent
	end
	return table.concat(parts, ".")
end

function methods.Clone(self)
	local copy = Instance.new(self.ClassName)
	for k, v in pairs(self.__props) do
		copy.__props[k] = v
	end
	for k, v in pairs(self.__attributes) do
		copy.__attributes[k] = v
	end
	for _, c in ipairs(self.__children) do
		c:Clone().Parent = copy
	end
	return copy
end

function methods.Destroy(self)
	remove_child(self.__parent, self)
	self.__parent = nil
	self.Destroying:Fire()
	for _, c in ipairs(table.clone(self.__children)) do
		c:Destroy()
	end
end

function methods.GetService(self, name)
	local found = self:FindFirstChild(name)
	if found then
		return found
	end
	local service = Instance.new(if parents[name] then name else "Folder")
	service.__props.Name = name
	service.__props.ClassName = name
	service.Parent = self
	return service
end

methods.FindService = methods.GetService

-- DataStoreService: every store is a table in memory, so a save in a
-- test reads back in the same run and vanishes after it.
local stores = {}

function methods.GetDataStore(self, name, scope)
	local id = `{name}/{scope or "global"}`
	local store = stores[id]
	if store then
		return store
	end
	local data = {}
	store = {}
	function store.GetAsync(_, key)
		return data[key]
	end
	function store.SetAsync(_, key, value)
		data[key] = value
	end
	function store.UpdateAsync(_, key, f)
		local value = f(data[key])
		if value ~= nil then
			data[key] = value
		end
		return value
	end
	function store.RemoveAsync(_, key)
		local old = data[key]
		data[key] = nil
		return old
	end
	stores[id] = store
	return store
end

function methods.GetPlayers(self)
	return self:GetChildren()
end

function methods.GetPivot(self)
	return self.__props.CFrame or CFrame.new(self.__props.Position or Vector3.zero)
end

function methods.PivotTo(self, cframe)
	self.__props.CFrame = cframe
	self.__props.Position = cframe.Position
end

function methods.BindToClose(self, f)
	self.__props.__close = f
end

function instance_meta.__index(self, key)
	local props = rawget(self, "__props")
	local value = props[key]
	if value ~= nil then
		return value
	end
	if key == "Parent" then
		return rawget(self, "__parent")
	end
	local method = methods[key]
	if method ~= nil then
		return method
	end
	local signals = rawget(self, "__signals")
	local signal = signals[key]
	if signal == nil and (key:match("^[A-Z]") and key:match("ed$") or key == "Destroying" or key == "Changed") then
		signal = new_signal()
		signals[key] = signal
	end
	if signal ~= nil then
		return signal
	end
	local child = self:FindFirstChild(key)
	if child ~= nil then
		return child
	end
	return nil
end

function instance_meta.__newindex(self, key, value)
	if key == "Parent" then
		local old = rawget(self, "__parent")
		if old == value then
			return
		end
		remove_child(old, self)
		rawset(self, "__parent", value)
		add_child(value, self)
		self.AncestryChanged:Fire(self, value)
		return
	end
	local props = rawget(self, "__props")
	props[key] = value
	self.Changed:Fire(key)
end

function instance_meta.__tostring(self)
	return self.Name
end

function Instance.new(class, parent)
	local self = setmetatable({
		__props = { ClassName = class, Name = class },
		__attributes = {},
		__children = {},
		__signals = {},
		__parent = nil,
	}, instance_meta)
	mark(self, "Instance")
	if parent ~= nil then
		self.Parent = parent
	end
	return self
end

-- ---------------------------------------------------------------------------
-- task: threads that park on wait and never wake, unless a test steps them.
-- ---------------------------------------------------------------------------

local task = {}
local parked = {}

function task.spawn(f, ...)
	local thread = if type(f) == "thread" then f else coroutine.create(f)
	local ok, err = coroutine.resume(thread, ...)
	if not ok then
		error(err, 0)
	end
	return thread
end

task.defer = task.spawn

-- The engine runs a delayed call later, so the double parks it beside
-- the threads a `task.wait` parked. `shim.step` fires both.
function task.delay(seconds, f, ...)
	local args = table.pack(...)
	local thread = coroutine.create(function()
		f(table.unpack(args, 1, args.n))
	end)
	table.insert(parked, thread)
	return thread
end

function task.wait(seconds)
	local thread = coroutine.running()
	if coroutine.isyieldable() then
		table.insert(parked, thread)
		coroutine.yield()
	end
	return seconds or 0
end

function task.cancel(thread)
	coroutine.close(thread)
end

-- Wakes every thread that a `task.wait` parked, in order, once.
function shim.step()
	local threads = parked
	parked = {}
	for _, thread in ipairs(threads) do
		if coroutine.status(thread) == "suspended" then
			coroutine.resume(thread)
		end
	end
	return #threads
end

-- Debris: the one service with behavior of its own. `AddItem` parks the
-- removal, so a test steps it like any other delayed call.
function methods.AddItem(self, item, seconds)
	task.delay(seconds, function()
		item:Destroy()
	end)
end

-- ---------------------------------------------------------------------------
-- The globals.
-- ---------------------------------------------------------------------------

local data_model = Instance.new("DataModel")
data_model.__props.Name = "game"
local workspace_service = data_model:GetService("Workspace")
workspace_service.__props.Name = "Workspace"

shim.Vector3 = Vector3
shim.Vector2 = Vector2
shim.CFrame = CFrame
shim.Color3 = Color3
shim.UDim = UDim
shim.UDim2 = UDim2
shim.NumberRange = NumberRange
shim.Random = Random
shim.Enum = Enum
shim.Instance = Instance
shim.task = task
shim.game = data_model
shim.workspace = workspace_service

shim.typeof = typeof_double
shim.warn = warn or function(...)
	print("[warn]", ...)
end

return shim
