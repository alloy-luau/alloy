local __alloy = require("@alloy") local Slotted = {} Slotted.__index = Slotted setmetatable(Slotted, { __call = function(_, f) if f.count == nil then f.count = 1 end return setmetatable(f, Slotted) end }) function Slotted.new(f) return Slotted(f) end


 export type Slotted<T> = typeof(setmetatable({} :: { value: T, count: number }, Slotted)) Slotted.__tostring = function(s) return __alloy.show_struct("Slotted", s, { "value", "count" }) end

do
	function Slotted.new<T>(value: T): Slotted<T>
		return Slotted({ value = value })
	end

	function Slotted.get<T>(self): T
		return self.value
	end

	function Slotted.bump<T>(self, n: number): number
		self.count += n
		return self.count
	end
end

local Pair = {} Pair.__index = Pair setmetatable(Pair, { __call = function(_, f)  return setmetatable(f, Pair) end }) function Pair.new(f) return Pair(f) end


 type Pair<A, B> = typeof(setmetatable({} :: { left: A, right: B }, Pair)) Pair.__tostring = function(s) return __alloy.show_struct("Pair", s, { "left", "right" }) end

do
	function Pair.swap<A, B>(self): B
		return self.right
	end
end

local Swap = {} Swap.__index = Swap setmetatable(Swap, { __call = function(_, f)  return setmetatable(f, Swap) end }) function Swap.new(f) return Swap(f) end


 type Swap<A, B> = typeof(setmetatable({} :: { left: A, right: B }, Swap)) Swap.__tostring = function(s) return __alloy.show_struct("Swap", s, { "left", "right" }) end

do
	function Swap.flip<A, B>(self): Swap<B, A>
		return Swap({ left = self.right, right = self.left })
	end
end

local Loud = {} type Loud = { read shout: (self: any) -> string } Loud.shout = (nil :: any) :: (self: any) -> string



local Voiced = {} Voiced.__index = Voiced setmetatable(Voiced, { __call = function(_, f)  return setmetatable(f, Voiced) end }) function Voiced.new(f) return Voiced(f) end

 type Voiced<T> = typeof(setmetatable({} :: { value: T }, Voiced)) Voiced.__tostring = function(s) return __alloy.show_struct("Voiced", s, { "value" }) end

do
	function Voiced.shout<T>(self): string
		return "hi"
	end
end

local held = Slotted.new(5)
local sw = Swap({ left = 1, right = "a" })
local p = Pair({ left = 1, right = "a" })
local v = Voiced({ value = 1 })
print(held:get(), p:swap(), v:shout(), sw:flip()) return { Slotted = Slotted }
