local __alloy = require("@alloy") local Point = {} Point.__index = Point setmetatable(Point, { __call = function(_, f) if f.cache == nil then f.cache = 0 end return setmetatable(f, Point) end }) function Point.new(f) return Point(f) end






 type Point = typeof(setmetatable({} :: { x: number, name: string, cache: number }, Point)) function Point.to_table(self) return { x = self.x, label = self.name } end function Point.from_table(t) return Point({ x = t.x, name = t.label }) end function Point.serialize(self) return Point.to_table(self) end function Point.clone(self) return setmetatable(table.clone(self), Point) end Point.__tostring = function(s) return __alloy.show_struct("Point", s, { "x", "name", "cache" }) end __alloy.attrs(Point, { own = {}, fields = { name = { rename = { "label" } }, cache = { skip = {} } } })

local function to_data<T>(v: (T & __alloy.Serialize)): any
	return v:serialize()
end

local p = Point({ x = 1, name = "a" })
print(to_data(p), p:to_table(), Point.from_table({ x = 2, label = "b" }))
