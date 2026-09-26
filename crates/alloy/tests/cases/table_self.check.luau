local Provider = { }

Provider.count = 0

function Provider.helper(n: number): number
	return n
end

function Provider.Bump(self: typeof(Provider), n: number): number
	self.count += n
	return self.count
end

function Provider.Reset(self: typeof(Provider))
	self.count = 0
end

local Klass = { }
Klass.__index = Klass

function Klass.Method(self: typeof(setmetatable({} :: { [any]: any }, Klass)))
	print(self)
end

local Rebound = { }
Rebound = { other = 1 } type __self_Rebound = typeof(Rebound)

function Rebound.Method(self: __self_Rebound)
	print(self)
end

local Written = { value = 2 }

function Written.Explicit(self: typeof(Written)): number
	return self.value
end

type Self = typeof(Provider)
local held: Self = Provider
held:Reset()
print(held:Bump(2), Provider.helper(1), Klass, Rebound, Written)
