-- A remote packs its parameters by width, down into a struct, a record,
-- and an array; a Player crosses as it is.
local __alloy = require("@alloy") local Stats = {} Stats.__index = Stats setmetatable(Stats, { __call = function(_, f)  return setmetatable(f, Stats) end }) function Stats.new(f) return Stats(f) end



 type Stats = typeof(setmetatable({} :: { level: number, hp: number, title: string? }, Stats)) Stats.__tostring = function(s) return __alloy.show_struct("Stats", s, { "level", "hp", "title" }) end __alloy.attrs(Stats, { own = {}, fields = { level = { u8 = {} } } })

local Damage = __alloy.remote({ name = "Damage", kind = "event", from_client = true, from_server = false, params = { "target", "amount", "crit" }, defaults = {  }, attrs = {}, wire = { "any", "u16", "bool" } })
local Toast = __alloy.remote({ name = "Toast", kind = "event", from_client = false, from_server = true, params = { "message", "seconds" }, defaults = { seconds = 3 }, attrs = {}, wire = { "str", "f64?" } })
local Sync = __alloy.remote({ name = "Sync", kind = "event", from_client = false, from_server = true, params = { "stats", "tags", "pos" }, defaults = {  }, attrs = {}, wire = { { fields = { { "level", "u8" }, { "hp", "f64" }, { "title", "str?" } }, struct = Stats }, { item = "str", array = true }, { fields = { { "x", "f64" }, { "y", "f64" } } } } })
print(Damage, Toast, Sync)
