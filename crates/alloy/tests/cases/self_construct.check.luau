local __alloy = require("@alloy") local Plain = {} Plain.__index = Plain function Plain.__new(f: {  }): Plain  return (setmetatable(f, Plain) :: any) end type Plain = typeof(setmetatable({} :: {  }, Plain)) Plain.__tostring = function(s: Plain) return __alloy.show_struct("Plain", s, {  }) end

do
	function Plain.new(): Plain
		return Plain.__new({})
	end
end

local Marked = {} Marked.__index = Marked function Marked.__new(f: { field: number? }): Marked if f.field == nil then f.field = 0 end return (setmetatable(f, Marked) :: any) end

 type Marked = typeof(setmetatable({} :: { field: number }, Marked)) Marked.__tostring = function(s: Marked) return __alloy.show_struct("Marked", s, { "field" }) end

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

local Held = {} Held.__index = Held function Held.__new<T>(f: { items: __alloy.Array<T>? }): Held<T> if f.items == nil then f.items = (__alloy.Array.from({}) :: any) end return (setmetatable(f, Held) :: any) end

 type Held<T> = { items: __alloy.Array<T> } Held.__tostring = function(s) return __alloy.show_struct("Held", s, { "items" }) end

do
	function Held.new<T>(): Held<T>
		return Held.__new({})
	end
end

local a = Plain.new()
local b = Marked.new()
local c = Marked.one()
local d: Held<number> = Held.new()
print(a, b, c, d)
