local __alloy = require("@alloy") -- An `impl` inside a namespace targets the namespace's own struct, and
-- a `match` over its enum reads the variant with no prefix.
local Shapes = {}
    local Shapes_Circle = {} Shapes_Circle.__index = Shapes_Circle function Shapes_Circle.__new(f: { r: number }): Shapes_Circle  return (setmetatable(f, Shapes_Circle) :: any) end function Shapes_Circle.new(f: { r: number }): Shapes_Circle return Shapes_Circle.__new(f) end

 type Shapes_Circle = typeof(setmetatable({} :: { r: number }, Shapes_Circle)) Shapes_Circle.__tostring = function(s: Shapes_Circle) return __alloy.show_struct("Shapes.Circle", s, { "r" }) end Shapes.Circle = Shapes_Circle
    do
        function Shapes_Circle.area(self: Shapes_Circle): number
            return 3.14 * self.r * self.r
        end
    end
    local Shapes_Kind = {} Shapes_Kind.__index = Shapes_Kind
        Shapes_Kind.Round = ("Round" :: Shapes_Kind)
        Shapes_Kind.Flat = ("Flat" :: Shapes_Kind)
    function Shapes_Kind.is(v) return (type(v) == "table" and getmetatable(v) == Shapes_Kind) or v == "Round" or v == "Flat" end Shapes_Kind.__tostring = function(v) return __alloy.show_variant("Shapes.Kind", v) end type Shapes_Kind = "Round" | "Flat" Shapes.Kind = Shapes_Kind
    local function Shapes_name_of(k: Shapes_Kind): string
        do local _m1 = (k) :: Shapes_Kind
            if _m1 == "Round" then return "round"
            elseif _m1 == "Flat" then return "flat"
        else error("match: no arm covers this value", 2) end end

        return ""
    end Shapes.name_of = Shapes_name_of


local c = Shapes_Circle.__new({ r = 2 })

print(c:area(), Shapes.name_of(Shapes.Kind.Round))
