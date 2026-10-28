local __alloy = require("@alloy") local Plain = {} Plain.__index = Plain type Plain = typeof(setmetatable(({} :: any) :: {  }, Plain)) setmetatable(Plain, { __call = function(_, f: {  }): Plain  return ((setmetatable(f, Plain) :: any) :: Plain) end }) Plain.__tostring = function(s) return __alloy.show_struct("Plain", s, {  }) end

do
	function Plain.new(): Plain
		return Plain({})
	end
end

local Marked = {} Marked.__index = Marked

 type Marked = typeof(setmetatable(({} :: any) :: { field: number }, Marked)) setmetatable(Marked, { __call = function(_, f: { field: number? }): Marked if f.field == nil then local _field: number = 0 f.field = _field end return ((setmetatable(f, Marked) :: any) :: Marked) end }) Marked.__tostring = function(s) return __alloy.show_struct("Marked", s, { "field" }) end

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

local Held = {} Held.__index = Held

 type Held<T> = typeof(setmetatable(({} :: any) :: { items: __alloy.Array<T> }, Held)) setmetatable(Held, { __call = function<T>(_, f: { items: __alloy.Array<T>? }): Held<T> if f.items == nil then local _items: __alloy.Array<T> = __alloy.Array.from({}) f.items = _items end return ((setmetatable(f, Held) :: any) :: Held<T>) end }) Held.__tostring = function(s) return __alloy.show_struct("Held", s, { "items" }) end

do
	function Held.new<T>(): Held<T>
		return Held({})
	end
end

local a = Plain.new()
local b = Marked.new()
local c = Marked.one()
local d: Held<number> = Held.new()
print(a, b, c, d) return nil
