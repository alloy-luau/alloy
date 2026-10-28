local __alloy = require("@alloy") local Slotted = {} Slotted.__index = Slotted


 export type Slotted<T> = typeof(setmetatable(({} :: any) :: { value: T, count: number }, Slotted)) setmetatable(Slotted, { __call = function<T>(_, f: { value: T, count: number? }): Slotted<T> if f.count == nil then local _count: number = 1 f.count = _count end return ((setmetatable(f, Slotted) :: any) :: Slotted<T>) end }) Slotted.__tostring = function(s) return __alloy.show_struct("Slotted", s, { "value", "count" }) end

do
	function Slotted.new<T>(value: T): Slotted<T>
		return Slotted({ value = value })
	end

	function Slotted.get<T>(self: Slotted<T>): T
		return self.value
	end

	function Slotted.bump<T>(self: Slotted<T>, n: number): number
		self.count += n
		return self.count
	end
end

local Pair = {} Pair.__index = Pair


 type Pair<A, B> = typeof(setmetatable(({} :: any) :: { left: A, right: B }, Pair)) setmetatable(Pair, { __call = function<A, B>(_, f: { left: A, right: B }): Pair<A, B>  return ((setmetatable(f, Pair) :: any) :: Pair<A, B>) end }) function Pair.new<A, B>(f: { left: A, right: B }): Pair<A, B> return Pair(f) end Pair.__tostring = function(s) return __alloy.show_struct("Pair", s, { "left", "right" }) end

do
	function Pair.swap<A, B>(self: Pair<A, B>): B
		return self.right
	end
end

local Swap = {} Swap.__index = Swap


 type Swap<A, B> = typeof(setmetatable(({} :: any) :: { left: A, right: B }, Swap)) setmetatable(Swap, { __call = function<A, B>(_, f: { left: A, right: B }): Swap<A, B>  return ((setmetatable(f, Swap) :: any) :: Swap<A, B>) end }) function Swap.new<A, B>(f: { left: A, right: B }): Swap<A, B> return Swap(f) end Swap.__tostring = function(s) return __alloy.show_struct("Swap", s, { "left", "right" }) end

do
	function Swap.flip<A, B>(self: Swap<A, B>): Swap<B, A>
		return Swap({ left = self.right, right = self.left })
	end
end

local Loud = {} type Loud = { read shout: (self: any) -> string } Loud.shout = (nil :: any) :: (self: any) -> string



local Voiced = {} Voiced.__index = Voiced

 type Voiced<T> = typeof(setmetatable(({} :: any) :: { value: T }, Voiced)) setmetatable(Voiced, { __call = function<T>(_, f: { value: T }): Voiced<T>  return ((setmetatable(f, Voiced) :: any) :: Voiced<T>) end }) function Voiced.new<T>(f: { value: T }): Voiced<T> return Voiced(f) end Voiced.__tostring = function(s) return __alloy.show_struct("Voiced", s, { "value" }) end

do
	function Voiced.shout<T>(self: Voiced<T>): string
		return "hi"
	end
end

local held = Slotted.new(5)
local sw = Swap({ left = 1, right = "a" })
local p = Pair({ left = 1, right = "a" })
local v = Voiced({ value = 1 })
print(held:get(), p:swap(), v:shout(), sw:flip()) return { Slotted = Slotted }
