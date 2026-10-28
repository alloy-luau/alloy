-- Every type-argument list, written or inferred, lowers the same way:
-- `T[]` becomes `Array<T>` and a std name takes the runtime prefix.
local __alloy = require("@alloy") local Pair = {} Pair.__index = Pair


 type Pair<A, B> = typeof(setmetatable(({} :: any) :: { first: A, second: B }, Pair)) setmetatable(Pair, { __call = function<A, B>(_, f: { first: A, second: B }): Pair<A, B>  return ((setmetatable(f, Pair) :: any) :: Pair<A, B>) end }) function Pair.new<A, B>(f: { first: A, second: B }): Pair<A, B> return Pair(f) end Pair.__tostring = function(s) return __alloy.show_struct("Pair", s, { "first", "second" }) end

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

print(written, nested, inferred, make()) return nil
