-- A namespace inside a namespace: the inner one is a field of the
-- outer table, and its members take both names.
local __alloy = require("@alloy") local Outer = {}
    const Outer_A = 1 Outer.A = Outer_A
    Outer.Inner = {}
        const Outer_Inner_B = 2 Outer.Inner.B = Outer_Inner_B
        local Outer_Inner_Point = {} Outer_Inner_Point.__index = Outer_Inner_Point setmetatable(Outer_Inner_Point, { __call = function(_, f)  return setmetatable(f, Outer_Inner_Point) end }) function Outer_Inner_Point.new(f) return Outer_Inner_Point(f) end

 type Outer_Inner_Point = typeof(setmetatable({} :: { x: number }, Outer_Inner_Point)) Outer_Inner_Point.__tostring = function(s) return __alloy.show_struct("Outer.Inner.Point", s, { "x" }) end Outer.Inner.Point = Outer_Inner_Point
    


local p: Outer_Inner_Point = { x = Outer.A + Outer.Inner.B }

print(p)
