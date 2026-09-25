local __alloy = require("@alloy") local Point = {} Point.__index = Point function Point.__new(f: { x: number, name: string, cache: number? }): Point if f.cache == nil then f.cache = 0 end return (setmetatable(f, Point) :: any) end function Point.new(f: { x: number, name: string, cache: number? }): Point return Point.__new(f) end






 type Point = typeof(setmetatable({} :: { x: number, name: string, cache: number }, Point)) function Point.to_table(self: Point) return { x = self.x, label = self.name } end function Point.serialize(self: Point): any return Point.to_table(self) end function Point.from_table(t: any): Point return Point.__new({ x = t.x, name = t.label }) end function Point.clone(self: Point): Point return (setmetatable(table.clone(self), Point) :: any) end Point.__tostring = function(s: Point) return __alloy.show_struct("Point", s, { "x", "name", "cache" }) end __alloy.attrs(Point, { own = {}, fields = { name = { rename = { "label" } }, cache = { skip = {} } } })

local function to_data<T>(v: (T & __alloy.Serialize)): any
	return v:serialize()
end

local p = Point.__new({ x = 1, name = "a" })
print(to_data(p), p:to_table(), Point.from_table({ x = 2, label = "b" }))
