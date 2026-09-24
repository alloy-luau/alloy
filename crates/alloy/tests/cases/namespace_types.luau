-- A namespace over types. Luau has no `Math.Vec2` type path, so the
-- struct declares `Math_Vec2` and every type slot reads that name.
local __alloy = require("@alloy") local Math = {}
    local Math_Vec2 = {} Math_Vec2.__index = Math_Vec2 setmetatable(Math_Vec2, { __call = function(_, f)  return setmetatable(f, Math_Vec2) end }) function Math_Vec2.new(f) return Math_Vec2(f) end


 type Math_Vec2 = typeof(setmetatable({} :: { x: number, y: number }, Math_Vec2)) Math_Vec2.__tostring = function(s) return __alloy.show_struct("Math.Vec2", s, { "x", "y" }) end Math.Vec2 = Math_Vec2
    type Math_Scale = number


local v: Math_Vec2 = { x = 1, y = 2 }
local s: Math_Scale = 2

print(v.x * s)
