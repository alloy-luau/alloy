-- The check artifact reads a field once, since the checker types a
-- second read of a map's field as `~nil` (LANG_BUGS 139).
local function field(t: { [string]: number }): number
  const n = (if t.a == nil then 0 else t.a)

  return n + 1
end

local function lazy(t: { [string]: number }, on: boolean): boolean
  return on and ((if t.a == nil then 0 else t.a)) > 1
end return { field = field, lazy = lazy }
