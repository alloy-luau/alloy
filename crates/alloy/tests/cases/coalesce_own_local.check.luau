-- In the check artifact each `??` reads into a local of its own, since
-- a reused temp keeps the type of its first value (LANG_BUGS 144).
local function both(counts: { [string]: { number } }, names: { [string]: { string } }, key: string): number
  local _1 = counts[key] const some: { number } = ((if _1 == nil then {} else _1) :: typeof(assert(_1)))
  local _1 = names[key] const words: { string } = ((if _1 == nil then {} else _1) :: typeof(assert(_1)))

  return #some + #words
end return { both = both }
