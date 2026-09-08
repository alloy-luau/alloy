local __alloy = require("@alloy") local Slotted = {} Slotted.__index = Slotted function Slotted.__new<T>(f: { value: T, count: number? }): Slotted<T> if f.count == nil then f.count = 1 end return (setmetatable(f, Slotted) :: any) end


 export type Slotted<T> = { value: T, count: number, read get: (self: Slotted<T>) -> T, read bump: (self: Slotted<T>, n: number) -> number } Slotted.__tostring = function(s) return __alloy.show_struct("Slotted", s, { "value", "count" }) end

do
	function Slotted.new<T>(value: T): Slotted<T>
		return Slotted.__new({ value = value })
	end

	function Slotted.get<T>(self: Slotted<T>): T
		return self.value
	end

	function Slotted.bump<T>(self: Slotted<T>, n: number): number
		self.count += n
		return self.count
	end
end

local Pair = {} Pair.__index = Pair function Pair.__new<A, B>(f: { left: A, right: B }): Pair<A, B>  return (setmetatable(f, Pair) :: any) end function Pair.new<A, B>(f: { left: A, right: B }): Pair<A, B> return Pair.__new(f) end


 type Pair<A, B> = { left: A, right: B, read swap: (self: Pair<A, B>) -> B } Pair.__tostring = function(s) return __alloy.show_struct("Pair", s, { "left", "right" }) end

do
	function Pair.swap<A, B>(self: Pair<A, B>): B
		return self.right
	end
end

local Swap = {} Swap.__index = Swap function Swap.__new<A, B>(f: { left: A, right: B }): Swap<A, B>  return (setmetatable(f, Swap) :: any) end function Swap.new<A, B>(f: { left: A, right: B }): Swap<A, B> return Swap.__new(f) end


 type Swap<A, B> = typeof(setmetatable({} :: { left: A, right: B }, Swap)) Swap.__tostring = function(s) return __alloy.show_struct("Swap", s, { "left", "right" }) end

do
	function Swap.flip<A, B>(self: Swap<A, B>): Swap<B, A>
		return Swap.__new({ left = self.right, right = self.left })
	end
end

local Loud = {} type Loud = { read shout: (self: any) -> string } Loud.shout = (nil :: any) :: (self: any) -> string



local Voiced = {} Voiced.__index = Voiced function Voiced.__new<T>(f: { value: T }): Voiced<T>  return (setmetatable(f, Voiced) :: any) end function Voiced.new<T>(f: { value: T }): Voiced<T> return Voiced.__new(f) end

 type Voiced<T> = typeof(setmetatable({} :: { value: T }, Voiced)) Voiced.__tostring = function(s) return __alloy.show_struct("Voiced", s, { "value" }) end

do
	function Voiced.shout<T>(self: Voiced<T>): string
		return "hi"
	end
end

local held = Slotted.new(5)
local sw = Swap.__new({ left = 1, right = "a" })
local p = Pair.__new({ left = 1, right = "a" })
local v = Voiced.__new({ value = 1 })
print(held:get(), p:swap(), v:shout(), sw:flip()) return { Slotted = Slotted }
