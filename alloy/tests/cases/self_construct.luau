local __alloy = require("@alloy") local Plain = {} Plain.__index = Plain setmetatable(Plain, { __call = function(_, f)  return setmetatable(f, Plain) end }) function Plain.new(f) return Plain(f) end type Plain = typeof(setmetatable({} :: {  }, Plain)) Plain.__tostring = function(s) return __alloy.show_struct("Plain", s, {  }) end

do
	function Plain.new(): Plain
		return Plain({})
	end
end

local Marked = {} Marked.__index = Marked setmetatable(Marked, { __call = function(_, f) if f.field == nil then f.field = 0 end return setmetatable(f, Marked) end }) function Marked.new(f) return Marked(f) end

 type Marked = typeof(setmetatable({} :: { field: number }, Marked)) Marked.__tostring = function(s) return __alloy.show_struct("Marked", s, { "field" }) end

do
	function Marked.new(): Marked
		return Marked({ })
	end

	function Marked.one(): Marked
		return Marked({ field = 1 })
	end

	function Marked.copy(): Marked
		return Marked.new()
	end
end

local Held = {} Held.__index = Held setmetatable(Held, { __call = function(_, f) if f.items == nil then f.items = __alloy.Array.from({}) end return setmetatable(f, Held) end }) function Held.new(f) return Held(f) end

 type Held<T> = typeof(setmetatable({} :: { items: __alloy.Array<T> }, Held)) Held.__tostring = function(s) return __alloy.show_struct("Held", s, { "items" }) end

do
	function Held.new<T>(): Held<T>
		return Held({})
	end
end

local a = Plain.new()
local b = Marked.new()
local c = Marked.one()
local d: Held<number> = Held.new()
print(a, b, c, d)
