-- A returned list literal takes the element type of the return type in
-- the check artifact. A test of a field refines the item, and a list is
-- invariant (LANG_BUGS 147).
local __alloy = require("@alloy") export type Entry = { name: string }

local function listed(entry: Entry): { Entry }
  return (if entry.name == '' then (__alloy.Array.from({}) :: __alloy.Array<any>) else __alloy.Array.from<<Entry>>({ entry }))
end

local function guarded(entry: Entry): __alloy.Array<Entry>
  if entry.name == '' then
    return (__alloy.Array.from({}) :: __alloy.Array<any>)
  end

  return __alloy.Array.from<<Entry>>({ entry })
end return { listed = listed, guarded = guarded }
