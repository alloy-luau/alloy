local __alloy = require("@alloy") local Plain = {} Plain.__index = Plain function Plain.__new(f: {  }): Plain  return ((setmetatable(f, Plain) :: any) :: Plain) end type Plain = typeof(setmetatable({} :: {  }, Plain)) Plain.__tostring = function(s: Plain) return __alloy.show_struct("Plain", (s :: any), {  }) end

do
	function Plain.new(): Plain
		return Plain.__new({})
	end
end

local Marked = {} Marked.__index = Marked function Marked.__new(f: { field: number? }): Marked if f.field == nil then local _field: number = 0 f.field = _field end return ((setmetatable(f, Marked) :: any) :: Marked) end

 type Marked = typeof(setmetatable({} :: { field: number }, Marked)) Marked.__tostring = function(s: Marked) return __alloy.show_struct("Marked", (s :: any), { "field" }) end

do
	function Marked.new(): Marked
		return Marked.__new({ })
	end

	function Marked.one(): Marked
		return Marked.__new({ field = 1 })
	end

	function Marked.copy(): Marked
		return Marked.new()
	end
end

local Held = {} Held.__index = Held function Held.__new<T>(f: { items: __alloy.Array<T>? }): Held<T> if f.items == nil then local _items: __alloy.Array<T> = (__alloy.Array.from({}) :: __alloy.Array<any>) f.items = _items end return ((setmetatable(f, Held) :: any) :: Held<T>) end

 type Held<T> = { items: __alloy.Array<T> } Held.__tostring = function(s) return __alloy.show_struct("Held", (s :: any), { "items" }) end

do
	function Held.new<T>(): Held<T>
		return Held.__new({})
	end
end

local a = Plain.new()
local b = Marked.new()
local c = Marked.one()
local d: Held<number> = Held.new()
print(a, b, c, d) return nil
