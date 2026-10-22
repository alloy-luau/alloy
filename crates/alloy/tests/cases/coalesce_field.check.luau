-- The check artifact reads a field once, since the checker types a
-- second read of a map's field as `~nil` (LANG_BUGS 139).
local __alloy = require("@alloy") local function field(t: { [string]: number }): number
  local _1 = t.a const n = (if _1 == nil then 0 else _1)

  return n + 1
end

local function lazy(t: { [string]: number }, on: boolean): boolean
  return on and (__alloy.coalesce(t.a, 0)) > 1
end return { field = field, lazy = lazy }
