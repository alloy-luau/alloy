local __alloy = require("@alloy") local Sized = {} type Sized = { read size: (self: any) -> number } Sized.size = (nil :: any) :: (self: any) -> number



local Bag = {} Bag.__index = Bag

 type Bag = typeof(setmetatable(({} :: any) :: { items: __alloy.Array<string> }, Bag)) setmetatable(Bag, { __call = function(_, f: { items: __alloy.Array<string> }): Bag  return ((setmetatable(f, Bag) :: any) :: Bag) end }) function Bag.new(f: { items: __alloy.Array<string> }): Bag return Bag(f) end Bag.__tostring = function(s) return __alloy.show_struct("Bag", s, { "items" }) end

do
	function Bag.size(self: Bag): number
		return self.items:len()
	end
end

local function total<T>(xs: __alloy.Array<T>): number
	local sum = 0
	for _, x in xs do
		sum += x:size()
	end
	return sum
end

local function first<T>(xs: __alloy.Array<T>): number
	return xs[1]:size()
end

local function grow<T>(xs: __alloy.Array<T>, t: (T & Sized)): number
	xs:push(t)
	return xs:len()
end

local function pairs_of<T>(xs: __alloy.Array<T>): number
	local n = 0
	for i, x in xs do
		n += i + x:size()
	end
	return n
end

local bag = Bag({ items = __alloy.Array.from({ "a" }) })
print(total(__alloy.Array.from({ bag })), first(__alloy.Array.from({ bag })), grow(__alloy.Array.from({ bag }), bag), pairs_of(__alloy.Array.from({ bag }))) return nil
