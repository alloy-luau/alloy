local __alloy = require("@alloy") -- A default sits beside named exports; they are separate names. The
-- declaration under `export default` binds here too, so the file uses
-- it like any other.
local VERSION = 2

local Theme = {} Theme.__index = Theme setmetatable(Theme, { __call = function(_, f)  return setmetatable(f, Theme) end }) function Theme.new(f) return Theme(f) end

 export type Theme = typeof(setmetatable({} :: { primary: string }, Theme)) Theme.__tostring = function(s) return __alloy.show_struct("Theme", s, { "primary" }) end

local function name(): string
    return "theme"
end

local built = Theme.new({ primary = "blue" })

print(built, VERSION, name()) return { VERSION = VERSION, default = Theme, name = name }
