--!nonstrict
-- Engine doubles for tests on a plain Luau VM. `alloy test` requires
-- this before a spec, and it fills in what the engine provides when
-- `game` is nil: the datatypes shared code touches, `task`, `Enum`,
-- and a small Instance tree. In Studio it does nothing. The doubles
-- hold the shape the code reads, not the engine's behavior: a
-- `Vector3` adds and measures, a `CFrame` turns and moves points, an
-- `Instance` parents and finds children, a `task.wait` parks the
-- thread.

local shim = {}

-- A scoped import, an `import` in a function body, requires its module
-- at the first call in the game. lest's VM resolves a `require` only
-- while a spec loads, so a module of the test build gives the require
-- to `later` (LANG_BUGS 120). The temp of the import holds a stand-in
-- that loads the module when it is read. The spec calls `preload` at
-- the end of its load, and `set` puts the module itself in the temp.
local pending = {}

-- The path from the modules folder to what `path` names from the
-- module at `owner`. A module writes a path from its own folder, and
-- `@self/` in an `init` module names that folder too.
local function rebase(owner, path)
	local parts = string.split(owner, "/")
	table.remove(parts)

	for _, part in string.split((string.gsub(path, "^@self/", "")), "/") do
		if part == ".." and #parts > 0 and parts[#parts] ~= ".." then
			table.remove(parts)
		elseif part ~= "." then
			table.insert(parts, part)
		end
	end

	return table.concat(parts, "/")
end

-- A module gives its loader the require to call. In `preload` that is
-- the spec's own, with the path from the spec: lest reads a path from
-- the module that loads at the time, Lune from the chunk that calls,
-- and here both are the spec. A module that reads the import while it
-- loads gives the loader the plain `require`, and both read the path
-- from that module.
local function load_once(entry, root, spec_require)
	local load = entry.load

	if load ~= nil then
		entry.load = nil
		local from_spec = spec_require
			and function(path)
				if string.sub(path, 1, 1) == "@" and string.sub(path, 1, 6) ~= "@self/" then
					return spec_require(path)
				end

				return spec_require(root .. rebase(entry.owner, path))
			end
		entry.ok, entry.value = pcall(load, from_spec or require)

		if entry.ok then
			entry.set(entry.value)
		end
	end
end

-- A module that fails to load fails the read, as the call fails in the
-- game, and not the load of the whole spec.
local function loaded(entry)
	load_once(entry, nil, nil)

	if not entry.ok then
		error(entry.value, 0)
	end

	return entry.value
end

function shim.later(owner, load, set)
	local entry = { owner = owner, load = load, set = set }
	table.insert(pending, entry)

	return setmetatable({}, {
		__index = function(_, key)
			return loaded(entry)[key]
		end,
		__call = function(_, ...)
			return loaded(entry)(...)
		end,
	})
end

-- Loads each module that a scoped import names, and the modules that
-- their own scoped imports name. `root` is the spec's path to the
-- modules folder, and `spec_require` calls `require` in the spec.
function shim.preload(root, spec_require)
	while #pending > 0 do
		load_once(table.remove(pending, 1), root, spec_require)
	end
end

-- Under the engine every name is the engine's own.
if game ~= nil then
	shim.step = function()
		return 0
	end
	shim.settle = function(future)
		return future
	end

	return shim
end

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

local typeof = typeof_double

local function mark(value, kind)
	kinds[value] = kind
	return value
end

-- ---------------------------------------------------------------------------
-- Vector3, Vector2, CFrame
-- ---------------------------------------------------------------------------

local Vector3 = {}

-- `Unit` is a vector too, so it comes on read: a field would build a
-- unit vector of each unit vector without end.
function Vector3.__index(v, key)
	if key == "Unit" then
		return v / v.Magnitude
	end
	return Vector3[key]
end

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
	if type(a) == "number" then
		return v3(a / b.X, a / b.Y, a / b.Z)
	elseif type(b) == "number" then
		return v3(a.X / b, a.Y / b, a.Z / b)
	end
	return v3(a.X / b.X, a.Y / b.Y, a.Z / b.Z)
end

function Vector3.__idiv(a, b)
	return (a / b):Floor()
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

-- `Unit` comes on read, as it does for a Vector3.
function Vector2.__index(v, key)
	if key == "Unit" then
		return v / v.Magnitude
	end
	return Vector2[key]
end

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
Vector2.xAxis = v2(1, 0)
Vector2.yAxis = v2(0, 1)

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

function Vector2.__div(a, b)
	if type(a) == "number" then
		return v2(a / b.X, a / b.Y)
	elseif type(b) == "number" then
		return v2(a.X / b, a.Y / b)
	end
	return v2(a.X / b.X, a.Y / b.Y)
end

function Vector2.__idiv(a, b)
	return (a / b):Floor()
end

function Vector2.__unm(a)
	return v2(-a.X, -a.Y)
end

function Vector2.__eq(a, b)
	return a.X == b.X and a.Y == b.Y
end

function Vector2.__tostring(a)
	return `{a.X}, {a.Y}`
end

function Vector2.Dot(a, b)
	return a.X * b.X + a.Y * b.Y
end

-- The engine's Vector2 cross product is a number: the Z of the 3D one.
function Vector2.Cross(a, b)
	return a.X * b.Y - a.Y * b.X
end

-- Counterclockwise is positive when the angle is signed.
function Vector2.Angle(a, b, signed)
	local angle = math.atan2(a:Cross(b), a:Dot(b))
	return if signed then angle else math.abs(angle)
end

function Vector2.Lerp(a, b, t)
	return a + (b - a) * t
end

function Vector2.FuzzyEq(a, b, epsilon)
	local e = epsilon or 1e-5
	return math.abs(a.X - b.X) <= e and math.abs(a.Y - b.Y) <= e
end

-- The axis gives the angle a sign: negative when the turn from `a` to
-- `b` goes clockwise about it.
function Vector3.Angle(a, b, axis)
	local cross = a:Cross(b)
	local angle = math.atan2(cross.Magnitude, a:Dot(b))
	return if axis ~= nil and cross:Dot(axis) < 0 then -angle else angle
end

-- The methods that act on each component.
for name, f in { Abs = math.abs, Ceil = math.ceil, Floor = math.floor, Sign = math.sign } do
	Vector3[name] = function(a)
		return v3(f(a.X), f(a.Y), f(a.Z))
	end
	Vector2[name] = function(a)
		return v2(f(a.X), f(a.Y))
	end
end

for name, f in { Max = math.max, Min = math.min } do
	Vector3[name] = function(a, ...)
		local x, y, z = a.X, a.Y, a.Z
		for _, b in { ... } do
			x, y, z = f(x, b.X), f(y, b.Y), f(z, b.Z)
		end
		return v3(x, y, z)
	end
	Vector2[name] = function(a, ...)
		local x, y = a.X, a.Y
		for _, b in { ... } do
			x, y = f(x, b.X), f(y, b.Y)
		end
		return v2(x, y)
	end
end

-- A CFrame holds its rotation as a row-major 3x3 matrix in the slots 1
-- to 9, as `GetComponents` gives it. The columns are the RightVector, the
-- UpVector, and the back vector, the way the engine lays them out.
local CFrame = {}

local IDENTITY = { 1, 0, 0, 0, 1, 0, 0, 0, 1 }

-- The matrix `r`, or a CFrame, applied to the vector (x, y, z).
local function rotate(r, x, y, z)
	return v3(r[1] * x + r[2] * y + r[3] * z, r[4] * x + r[5] * y + r[6] * z, r[7] * x + r[8] * y + r[9] * z)
end

-- The product of two matrices, either of which can be a CFrame.
local function mul3(a, b)
	local out = {}
	for row = 0, 2 do
		for col = 1, 3 do
			out[row * 3 + col] = a[row * 3 + 1] * b[col] + a[row * 3 + 2] * b[col + 3] + a[row * 3 + 3] * b[col + 6]
		end
	end
	return out
end

local function transpose(r)
	return { r[1], r[4], r[7], r[2], r[5], r[8], r[3], r[6], r[9] }
end

local function cf(position, r)
	r = r or IDENTITY
	local self = setmetatable({
		Position = position,
		X = position.X,
		Y = position.Y,
		Z = position.Z,
		r[1],
		r[2],
		r[3],
		r[4],
		r[5],
		r[6],
		r[7],
		r[8],
		r[9],
	}, CFrame)
	return mark(table.freeze(self), "CFrame")
end

local function from_columns(position, x, y, z)
	return cf(position, { x.X, y.X, z.X, x.Y, y.Y, z.Y, x.Z, y.Z, z.Z })
end

local function rot_x(a)
	local c, s = math.cos(a or 0), math.sin(a or 0)
	return { 1, 0, 0, 0, c, -s, 0, s, c }
end

local function rot_y(a)
	local c, s = math.cos(a or 0), math.sin(a or 0)
	return { c, 0, s, 0, 1, 0, -s, 0, c }
end

local function rot_z(a)
	local c, s = math.cos(a or 0), math.sin(a or 0)
	return { c, -s, 0, s, c, 0, 0, 0, 1 }
end

local function from_quaternion(w, x, y, z)
	local n = math.sqrt(w * w + x * x + y * y + z * z)
	w, x, y, z = w / n, x / n, y / n, z / n
	return {
		1 - 2 * (y * y + z * z),
		2 * (x * y - z * w),
		2 * (x * z + y * w),
		2 * (x * y + z * w),
		1 - 2 * (x * x + z * z),
		2 * (y * z - x * w),
		2 * (x * z - y * w),
		2 * (y * z + x * w),
		1 - 2 * (x * x + y * y),
	}
end

-- The quaternion w, x, y, z of a rotation, by the largest diagonal term
-- so that the square root never takes a value near zero.
local function to_quaternion(r)
	local trace = r[1] + r[5] + r[9]
	if trace > 0 then
		local s = math.sqrt(1 + trace) * 2
		return s / 4, (r[8] - r[6]) / s, (r[3] - r[7]) / s, (r[4] - r[2]) / s
	elseif r[1] > r[5] and r[1] > r[9] then
		local s = math.sqrt(1 + r[1] - r[5] - r[9]) * 2
		return (r[8] - r[6]) / s, s / 4, (r[2] + r[4]) / s, (r[3] + r[7]) / s
	elseif r[5] > r[9] then
		local s = math.sqrt(1 + r[5] - r[1] - r[9]) * 2
		return (r[3] - r[7]) / s, (r[2] + r[4]) / s, s / 4, (r[6] + r[8]) / s
	end
	local s = math.sqrt(1 + r[9] - r[1] - r[5]) * 2
	return (r[4] - r[2]) / s, (r[3] + r[7]) / s, (r[6] + r[8]) / s, s / 4
end

-- The direction vectors and `Rotation` come on read: each one is a new
-- value, and `Rotation` is a CFrame of its own.
local props = {}

function props.RightVector(c)
	return v3(c[1], c[4], c[7])
end

function props.UpVector(c)
	return v3(c[2], c[5], c[8])
end

function props.LookVector(c)
	return v3(-c[3], -c[6], -c[9])
end

function props.ZVector(c)
	return v3(c[3], c[6], c[9])
end

props.XVector = props.RightVector
props.YVector = props.UpVector

function props.Rotation(c)
	return cf(Vector3.zero, c)
end

function CFrame.__index(c, key)
	local prop = props[key]
	if prop ~= nil then
		return prop(c)
	end
	return CFrame[key]
end

-- `new()`, `new(x, y, z)`, `new(pos)`, `new(pos, target)`, the twelve
-- components `GetComponents` gives, or a position and a quaternion.
function CFrame.new(...)
	local n = select("#", ...)
	local x, y, z = ...
	if n == 0 then
		return CFrame.identity
	elseif typeof(x) == "Vector3" then
		if y ~= nil then
			return CFrame.lookAt(x, y)
		end
		return cf(x)
	elseif n == 12 then
		return cf(v3(x, y, z), { select(4, ...) })
	elseif n == 7 then
		local qx, qy, qz, qw = select(4, ...)
		return cf(v3(x, y, z), from_quaternion(qw, qx, qy, qz))
	end
	return cf(v3(x, y, z))
end

-- The engine's `lookAt`: the LookVector points from `at` to `target`, and
-- the UpVector leans to `up`. A look along `up` leaves no RightVector, so
-- the X axis stands in for `up` there. A target at `at` turns nothing.
function CFrame.lookAt(at, target, up)
	local look = target - at
	if look.Magnitude == 0 then
		return cf(at)
	end
	local back = -look.Unit
	local right = (up or Vector3.yAxis):Cross(back)
	if right.Magnitude < 1e-6 then
		right = Vector3.xAxis:Cross(back)
	end
	right = right.Unit
	return from_columns(at, right, back:Cross(right), back)
end

-- The engine turns about Z, then Y, then X.
function CFrame.Angles(rx, ry, rz)
	return cf(Vector3.zero, mul3(rot_x(rx), mul3(rot_y(ry), rot_z(rz))))
end

CFrame.fromEulerAnglesXYZ = CFrame.Angles

-- The engine turns about Z, then X, then Y.
function CFrame.fromEulerAnglesYXZ(rx, ry, rz)
	return cf(Vector3.zero, mul3(rot_y(ry), mul3(rot_x(rx), rot_z(rz))))
end

CFrame.fromOrientation = CFrame.fromEulerAnglesYXZ

function CFrame.fromAxisAngle(axis, angle)
	local k = axis.Unit
	local x, y, z = k.X, k.Y, k.Z
	local c, s = math.cos(angle), math.sin(angle)
	local t = 1 - c
	return cf(Vector3.zero, {
		t * x * x + c,
		t * x * y - s * z,
		t * x * z + s * y,
		t * x * y + s * z,
		t * y * y + c,
		t * y * z - s * x,
		t * x * z - s * y,
		t * y * z + s * x,
		t * z * z + c,
	})
end

function CFrame.fromMatrix(position, x, y, z)
	return from_columns(position, x, y, z or x:Cross(y).Unit)
end

CFrame.identity = cf(v3(0, 0, 0))

function CFrame.__add(a, b)
	return cf(a.Position + b, a)
end

function CFrame.__sub(a, b)
	return cf(a.Position - b, a)
end

-- A CFrame times a CFrame composes the two; times a Vector3 it moves the
-- point into world space.
function CFrame.__mul(a, b)
	if typeof(b) == "CFrame" then
		return cf(a.Position + rotate(a, b.X, b.Y, b.Z), mul3(a, b))
	end
	return a.Position + rotate(a, b.X, b.Y, b.Z)
end

function CFrame.__eq(a, b)
	for i = 1, 9 do
		if a[i] ~= b[i] then
			return false
		end
	end
	return a.Position == b.Position
end

function CFrame.__tostring(a)
	return table.concat({ a:GetComponents() }, ", ")
end

function CFrame.GetComponents(a)
	return a.X, a.Y, a.Z, a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9]
end

function CFrame.Inverse(a)
	local r = transpose(a)
	return cf(-rotate(r, a.X, a.Y, a.Z), r)
end

function CFrame.ToWorldSpace(a, b)
	return a * b
end

function CFrame.ToObjectSpace(a, b)
	return a:Inverse() * b
end

function CFrame.PointToWorldSpace(a, v)
	return a * v
end

function CFrame.PointToObjectSpace(a, v)
	return a:Inverse() * v
end

function CFrame.VectorToWorldSpace(a, v)
	return rotate(a, v.X, v.Y, v.Z)
end

function CFrame.VectorToObjectSpace(a, v)
	return rotate(transpose(a), v.X, v.Y, v.Z)
end

-- The position moves in a line and the rotation turns on the shortest
-- arc, as the engine's `Lerp` does.
function CFrame.Lerp(a, b, t)
	local aw, ax, ay, az = to_quaternion(a)
	local bw, bx, by, bz = to_quaternion(b)
	local dot = aw * bw + ax * bx + ay * by + az * bz
	if dot < 0 then
		bw, bx, by, bz, dot = -bw, -bx, -by, -bz, -dot
	end
	local wa, wb = 1 - t, t
	-- Two rotations this close divide by a sine near zero, so they mix
	-- in a line, and `from_quaternion` scales the result back to one.
	if dot < 0.9995 then
		local theta = math.acos(dot)
		wa = math.sin((1 - t) * theta) / math.sin(theta)
		wb = math.sin(t * theta) / math.sin(theta)
	end
	local r = from_quaternion(wa * aw + wb * bw, wa * ax + wb * bx, wa * ay + wb * by, wa * az + wb * bz)
	return cf(a.Position:Lerp(b.Position, t), r)
end

-- The angles that `Angles` takes back to this rotation.
function CFrame.ToEulerAnglesXYZ(a)
	local ry = math.asin(math.clamp(a[3], -1, 1))
	return math.atan2(-a[6], a[9]), ry, math.atan2(-a[2], a[1])
end

-- The angles that `fromEulerAnglesYXZ` takes back to this rotation.
function CFrame.ToEulerAnglesYXZ(a)
	local rx = math.asin(math.clamp(-a[6], -1, 1))
	return rx, math.atan2(a[3], a[9]), math.atan2(a[4], a[5])
end

CFrame.ToOrientation = CFrame.ToEulerAnglesYXZ

function CFrame.FuzzyEq(a, b, epsilon)
	local e = epsilon or 1e-5
	for i = 1, 9 do
		if math.abs(a[i] - b[i]) > e then
			return false
		end
	end
	return a.Position:FuzzyEq(b.Position, e)
end

-- ---------------------------------------------------------------------------
-- Color3, UDim, UDim2, NumberRange, Rect
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

-- A Rect holds its corners as they come: the engine does not sort them,
-- so a Width can be negative.
local Rect = {}
Rect.__index = Rect

function Rect.new(a, b, c, d)
	local min, max
	if typeof(a) == "Vector2" then
		min, max = a, b
	else
		min, max = v2(a, b), v2(c, d)
	end
	local self = { Min = min, Max = max, Width = max.X - min.X, Height = max.Y - min.Y }
	return mark(table.freeze(setmetatable(self, Rect)), "Rect")
end

function Rect.__eq(a, b)
	return a.Min == b.Min and a.Max == b.Max
end

function Rect.__tostring(a)
	return `{a.Min}, {a.Max}`
end

-- ---------------------------------------------------------------------------
-- ColorSequence, NumberSequence: keypoints along a time from 0 to 1.
-- ---------------------------------------------------------------------------

-- `Keypoints` gives a new table on each read, as the engine does, so a
-- test that changes it changes no sequence.
local function sequence_index(class)
	return function(self, key)
		if key == "Keypoints" then
			return table.clone(rawget(self, "points"))
		end
		return class[key]
	end
end

-- `new(value)`, `new(first, last)`, or a table of keypoints, which the
-- engine checks the way this does.
local function sequence(class, kind, keypoint, ...)
	local first, last = ...
	local points
	-- An Alloy array carries a metatable, and a list it is all the same.
	if typeof(first) == "table" then
		points = table.move(first, 1, #first, 1, {})
		if #points < 2 then
			error(`{kind}: requires at least 2 keypoints`, 3)
		end
		for i, point in points do
			if i > 1 and point.Time < points[i - 1].Time then
				error(`{kind}: all keypoints must be ordered by time`, 3)
			end
		end
		if points[1].Time ~= 0 then
			error(`{kind} must start at time=0.0`, 3)
		end
		if points[#points].Time ~= 1 then
			error(`{kind} must end at time=1.0`, 3)
		end
	else
		points = { keypoint.new(0, first), keypoint.new(1, if last == nil then first else last) }
	end
	return mark(table.freeze(setmetatable({ points = table.freeze(points) }, class)), kind)
end

local function same_points(a, b)
	local x, y = rawget(a, "points"), rawget(b, "points")
	if #x ~= #y then
		return false
	end
	for i, point in x do
		if point ~= y[i] then
			return false
		end
	end
	return true
end

local function points_text(a)
	local parts = {}
	for _, point in rawget(a, "points") do
		table.insert(parts, tostring(point))
	end
	return table.concat(parts)
end

local NumberSequenceKeypoint = {}
NumberSequenceKeypoint.__index = NumberSequenceKeypoint

function NumberSequenceKeypoint.new(time, value, envelope)
	local self = { Time = time, Value = value, Envelope = envelope or 0 }
	return mark(table.freeze(setmetatable(self, NumberSequenceKeypoint)), "NumberSequenceKeypoint")
end

function NumberSequenceKeypoint.__eq(a, b)
	return a.Time == b.Time and a.Value == b.Value and a.Envelope == b.Envelope
end

function NumberSequenceKeypoint.__tostring(a)
	return `{a.Time} {a.Value} {a.Envelope} `
end

local NumberSequence = {}
NumberSequence.__index = sequence_index(NumberSequence)
NumberSequence.__eq = same_points
NumberSequence.__tostring = points_text

function NumberSequence.new(...)
	return sequence(NumberSequence, "NumberSequence", NumberSequenceKeypoint, ...)
end

local ColorSequenceKeypoint = {}
ColorSequenceKeypoint.__index = ColorSequenceKeypoint

function ColorSequenceKeypoint.new(time, color)
	local self = { Time = time, Value = color }
	return mark(table.freeze(setmetatable(self, ColorSequenceKeypoint)), "ColorSequenceKeypoint")
end

function ColorSequenceKeypoint.__eq(a, b)
	return a.Time == b.Time and a.Value == b.Value
end

function ColorSequenceKeypoint.__tostring(a)
	return `{a.Time} {a.Value.R} {a.Value.G} {a.Value.B} 0 `
end

local ColorSequence = {}
ColorSequence.__index = sequence_index(ColorSequence)
ColorSequence.__eq = same_points
ColorSequence.__tostring = points_text

function ColorSequence.new(...)
	return sequence(ColorSequence, "ColorSequence", ColorSequenceKeypoint, ...)
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

-- A BindableEvent: `Fire` runs the handlers of its `Event` at once, as
-- the engine does in the serial phase. The reply of a message rides one.
function methods.Fire(self, ...)
	self.Event:Fire(...)
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
	if
		signal == nil
		and (key:match("^[A-Z]") and key:match("ed$") or key == "Destroying" or key == "Changed" or key == "Event")
	then
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

-- The runner calls an async test on its own thread, which cannot yield,
-- so the spec hands the test's Future here first. The parked threads
-- step until it settles. The cap stops a loop that parks forever.
function shim.settle(future)
	local steps = 0
	while not future:is_settled() do
		steps += 1
		if steps > 10000 or shim.step() == 0 then
			error("the test awaits a Future that no parked thread settles", 0)
		end
	end
	return future
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

-- Roblox's `debug` adds the MicroProfiler labels to Luau's own. Here
-- they do nothing.
local debug_double = table.clone(debug)

function debug_double.profilebegin(_label) end

function debug_double.profileend() end

local doubles = {
	typeof = typeof_double,
	Vector3 = Vector3,
	Vector2 = Vector2,
	CFrame = CFrame,
	Color3 = Color3,
	ColorSequence = ColorSequence,
	ColorSequenceKeypoint = ColorSequenceKeypoint,
	NumberSequence = NumberSequence,
	NumberSequenceKeypoint = NumberSequenceKeypoint,
	UDim = UDim,
	UDim2 = UDim2,
	NumberRange = NumberRange,
	Rect = Rect,
	Random = Random,
	Enum = Enum,
	Instance = Instance,
	task = task,
	game = data_model,
	workspace = workspace_service,
	debug = debug_double,
	warn = warn or function(...)
		print("[warn]", ...)
	end,
}

-- The doubles are globals, as the engine's names are. Every chunk of a
-- spec reads them from one environment: an Alloy module, the runtime,
-- and a plain Luau package that reads `game` as it loads (LANG_BUGS
-- 125). A module takes no locals for them, so it keeps the 200 locals
-- of Luau for its own names (LANG_BUGS 127). On Lune that environment
-- is not `_G`, so the shim writes to its own environment.
local env = getfenv(1)

for name, value in doubles do
	shim[name] = value
	env[name] = value
end

return shim
