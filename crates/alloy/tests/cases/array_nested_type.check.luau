-- A list literal inside a typed list literal takes the inner element
-- type. `Piece[][]` gives each inner literal `Piece`. Without it, an inner
-- literal took the type of its rows, and the invariant list refused it.
local __alloy = require("@alloy") type Piece = { key: string, width: number, flip: boolean? }

const SETS: __alloy.Array<__alloy.Array<Piece>> = __alloy.Array.from<<__alloy.Array<Piece>>>({
  __alloy.Array.from<<Piece>>({
    { key = 'a', width = 1, flip = false },
    { key = 'b', width = 3 },
  }),
  __alloy.Array.from<<Piece>>({
    { key = 'c', width = 2 },
  }),
})

local function rows(wide: boolean): { { Piece }? }
  return __alloy.Array.from<<{ Piece }?>>({ (if wide then __alloy.Array.from<<Piece>>({ { key = 'd', width = 4 } }) else __alloy.Array.from<<Piece>>({ { key = 'e', width = 1, flip = true } })) })
end

 return { rows = rows, default = SETS }
