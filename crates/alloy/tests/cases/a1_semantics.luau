-- Golden case for the compiler-semantics sweep: a payload enum with an
-- impl, a generic struct, an exhaustive match statement, and a spread.
local __alloy = require("@alloy") local Shape = {} Shape.__index = Shape
    function Shape.Circle(_1) return setmetatable({ tag = "Circle", _1 = _1 }, Shape) end
    function Shape.Rect(_1, _2) return setmetatable({ tag = "Rect", _1 = _1, _2 = _2 }, Shape) end
function Shape.is(v) return (type(v) == "table" and getmetatable(v) == Shape) end Shape.__tostring = function(v) return __alloy.show_variant("Shape", v) end type Shape = typeof(setmetatable({} :: { tag: "Circle", _1: number }, Shape)) | typeof(setmetatable({} :: { tag: "Rect", _1: number, _2: number }, Shape))

do
    function Shape.area(self): number
        return (
            if type(self) == "table" and self.tag == "Circle" then self._1 * self._1
            else self._1 * self._2
        )
    end
end

local Box = {} Box.__index = Box setmetatable(Box, { __call = function(_, f) if f.count == nil then f.count = 1 end return setmetatable(f, Box) end }) function Box.new(f) return Box(f) end


 type Box<T> = typeof(setmetatable({} :: { value: T, count: number }, Box)) Box.__tostring = function(s) return __alloy.show_struct("Box", s, { "value", "count" }) end

do
    function Box.get<T>(self): T
        return self.value
    end
end

local function describe(s: Shape)
    do local _m1 = s
        if type(_m1) == "table" and _m1.tag == "Circle" then local r = _m1._1
            print("circle", r)
        elseif type(_m1) == "table" and _m1.tag == "Rect" then local w, h = _m1._1, _m1._2
            print("rect", w, h)
    else error("match: no arm covers this value", 2) end end
end

local base = { x = 1, y = 2 }
local merged = __alloy.spread( base, { z = 3 } )
local b = Box({ value = 5 })
print(Shape.Rect(2, 3):area(), describe, merged, b:get())
