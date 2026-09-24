-- An `impl` inside a namespace targets the namespace's own struct, and
-- a `match` over its enum reads the variant with no prefix.
local __alloy = require("@alloy") local Shapes = {}
    local Shapes_Circle = {} Shapes_Circle.__index = Shapes_Circle setmetatable(Shapes_Circle, { __call = function(_, f)  return setmetatable(f, Shapes_Circle) end }) function Shapes_Circle.new(f) return Shapes_Circle(f) end

 type Shapes_Circle = typeof(setmetatable({} :: { r: number }, Shapes_Circle)) Shapes_Circle.__tostring = function(s) return __alloy.show_struct("Shapes.Circle", s, { "r" }) end Shapes.Circle = Shapes_Circle
    do
        function Shapes_Circle.area(self): number
            return 3.14 * self.r * self.r
        end
    end
    local Shapes_Kind = {} Shapes_Kind.__index = Shapes_Kind
        Shapes_Kind.Round = "Round"
        Shapes_Kind.Flat = "Flat"
    function Shapes_Kind.is(v) return (type(v) == "table" and getmetatable(v) == Shapes_Kind) or v == "Round" or v == "Flat" end Shapes_Kind.__tostring = function(v) return __alloy.show_variant("Shapes.Kind", v) end type Shapes_Kind = "Round" | "Flat" Shapes.Kind = Shapes_Kind
    local function Shapes_name_of(k: Shapes_Kind): string
        do local _m1 = k
            if _m1 == "Round" then return "round"
            elseif _m1 == "Flat" then return "flat"
        else error("match: no arm covers this value", 2) end end

        return ""
    end Shapes.name_of = Shapes_name_of


local c = Shapes_Circle({ r = 2 })

print(c:area(), Shapes.name_of(Shapes.Kind.Round))
