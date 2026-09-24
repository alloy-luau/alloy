-- Every type-argument list, written or inferred, lowers the same way:
-- `T[]` becomes `Array<T>` and a std name takes the runtime prefix.
local __alloy = require("@alloy") local Pair = {} Pair.__index = Pair setmetatable(Pair, { __call = function(_, f)  return setmetatable(f, Pair) end }) function Pair.new(f) return Pair(f) end


 type Pair<A, B> = typeof(setmetatable({} :: { first: A, second: B }, Pair)) Pair.__tostring = function(s) return __alloy.show_struct("Pair", s, { "first", "second" }) end

-- 1. a written list that holds an Alloy spelling
local written = __alloy.HashMap.new()

-- 2. a written list with a nested list inside it
local nested = __alloy.HashMap.new()

-- 3. a list the annotation infers
local inferred: __alloy.HashMap<string, __alloy.Array<number>> = __alloy.HashMap.new()

-- 4. a list the return type infers
local function make(): __alloy.HashMap<string, __alloy.Array<number>>
    return __alloy.HashMap.new()
end

print(written, nested, inferred, make())
