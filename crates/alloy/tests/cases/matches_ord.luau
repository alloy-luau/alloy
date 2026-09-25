-- `$matches` tests a pattern; `@derive(Ord)` compares field by field.
local __alloy = require("@alloy") local V = {} V.__index = V setmetatable(V, { __call = function(_, f)  return setmetatable(f, V) end }) function V.new(f) return V(f) end



 type V = typeof(setmetatable({} :: { x: number, y: number }, V)) V.__lt = function(a, b) if a.x ~= b.x then return a.x < b.x end if a.y ~= b.y then return a.y < b.y end return false end V.__le = function(a, b) if a.x ~= b.x then return a.x < b.x end if a.y ~= b.y then return a.y < b.y end return true end V.lt = V.__lt V.le = V.__le V.__eq = function(a, b) return a.x == b.x and a.y == b.y end V.eq = V.__eq V.__tostring = function(s) return __alloy.show_struct("V", s, { "x", "y" }) end

local r: __alloy.Result<number, string> = __alloy.Ok(1)
local hit = ( if type(r) == "table" and r.tag == "Ok" then true else false )
local pair = (function() local _1 = __alloy.Array.from({1, 2}) return ( if type(_1) == "table" and #_1 == 2 then true else false ) end)()
print(hit, pair, V({ x = 1, y = 2 }) < V({ x = 2, y = 0 }))
