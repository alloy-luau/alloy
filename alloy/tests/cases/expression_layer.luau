--!strict
local __alloy = require("@alloy") local v = (if c then "a" else "b")
local w = (typeof(x) == "Instance" and x:IsA("Part"))
local u = (typeof(k) == "EnumItem" and k.EnumType == Enum.KeyCode)
local s = (n ~= nil)
local t = __alloy.contains(xs, 3)
local b = bit32.bor(bit32.band(a, m), bit32.lshift(1, 2))
local cfg = ({ volume = 1 } :: Config)
local xs: __alloy.Array<number> = __alloy.Array.from({ 1, 2, 3 })
local ys: __alloy.ReadArray<string> = __alloy.Array.from({})
local up = ("abc"):upper()
btn.Activated:Connect(__alloy.bind(self, self.on_click))
local p = Vector3.new(1, 2, 3)
local q = Instance.new("Part")
	q.Name = "x" 
	q.Anchored = true 
	q.Parent = ws 

__alloy.delete(q)
local function f(a: number?, _p1: P, ...): number local a: number = if a == nil then 1 else a local x, y = _p1.x, _p1.y
	return a + x + y
end
local function load(id: number): __alloy.Future<Profile> return __alloy.future(function()
	local raw = __alloy.await(fetch(id))
	return raw
end) end
local function g(): __alloy.Result<number, string>
	local _1 = parse("1") if _1.tag == "Err" then return _1 end local n = _1._1
	_1 = __alloy.try_await(load(2)) if _1.tag == "Err" then return _1 end local m = _1._1
	return __alloy.Ok(n + m)
end
local fut = __alloy.future(function() return 1 end)
local res = __alloy.try_block(function() return 2 end)
local merged = __alloy.spread( base, { x = 1 }, other )
local _1 = t local a, first = _1.a, _1.b
_1 = xs local head, rest = _1[1], __alloy.Array.slice(_1, 2)
for _, _p1 in points do local x, y = _p1.x, _p1.y if not (x > 0) then continue end print(x, y) end
__alloy.dbg("expression_layer.aly:38", "v", v)
local name = "c"
local text = "1 + 2"
assert(a > b, "assertion failed: a > b")
__alloy.assert_eq("expression_layer.aly:42", "a", a, "b", b)
local bn = bit32.bnot(a)
local sym = __alloy.Symbol.new("k")
local arr = __alloy.Array.from({ 1 })
