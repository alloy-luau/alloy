local __alloy = require("@alloy") -- A constructor chains: a method call, a field, and a chain of both.
local Thing = {} Thing.__index = Thing setmetatable(Thing, { __call = function(_, f)  return setmetatable(f, Thing) end }) function Thing.new(f) return Thing(f) end

 type Thing = typeof(setmetatable({} :: { n: number }, Thing)) Thing.__tostring = function(s) return __alloy.show_struct("Thing", s, { "n" }) end

do
    function Thing.new(n: number): Thing
        return Thing({ n = n })
    end

    function Thing.get(self): number
        return self.n
    end

    function Thing.bump(self): Thing
        self.n += 1
        return self
    end
end

local a = Thing.new(1):get()
local b = Thing.new(2).n
local c = Thing.new(3):bump():get()
local d = Thing.new(4):bump().n
Thing.new(5):bump()
print(a, b, c, d)
