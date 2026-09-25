-- `$set` and `$map` build the std collections; after them `[[` opens the pairs.
local __alloy = require("@alloy") local tags = __alloy.Set.from({ "a", "b" })
local prices = __alloy.HashMap.from({ ["sword"] = 10, ["pet"] = 25 } :: { [string]: number })
local grid = __alloy.Array.from({ __alloy.Array.from({ 1, 2 }), __alloy.Array.from({ 3, 4 }) })
local note = [=[a long string keeps its leveled form]=]
print(tags, prices, grid, note, __alloy.Set.from({ 1, 2 }), __alloy.HashMap.from({ ["k"] = true } :: { [string]: boolean }))

local test = __alloy.Set.from({ 1, 2, 3 })
